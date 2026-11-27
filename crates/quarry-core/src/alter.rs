//! Structure edits (columns and indexes) as ALTER statements, staged and
//! committed like row edits in the Structure tab.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::model::DriverKind;
use crate::schema::TableRef;
use crate::sql::Dialect;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: String,
    #[serde(default = "yes")]
    pub nullable: bool,
    /// SQL expression, used verbatim (`'text'`, `0`, `now()`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum StructureChange {
    AddColumn { column: ColumnDef },
    DropColumn { name: String },
    /// `old` is the column as it is now; `new` what it should become.
    ModifyColumn { old: ColumnDef, new: ColumnDef },
    AddIndex { name: String, columns: Vec<String>, #[serde(default)] unique: bool },
    DropIndex { name: String },
}

impl Dialect {
    fn column_spec(self, c: &ColumnDef) -> String {
        let mut s = format!("{} {}", self.quote_ident(&c.name), c.data_type);
        s.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
        if let Some(d) = c.default.as_deref().filter(|d| !d.is_empty()) {
            s.push_str(&format!(" DEFAULT {d}"));
        }
        if matches!(self.0, DriverKind::MySql | DriverKind::MariaDb) {
            if let Some(cm) = c.comment.as_deref().filter(|c| !c.is_empty()) {
                s.push_str(&format!(" COMMENT {}", self.string_literal(cm)));
            }
        }
        s
    }

    fn comment_sql(self, table: &TableRef, column: &str, comment: &Option<String>) -> String {
        let value = match comment.as_deref().filter(|c| !c.is_empty()) {
            Some(c) => self.string_literal(c),
            None => "NULL".into(),
        };
        format!("COMMENT ON COLUMN {}.{} IS {value};", self.table(table), self.quote_ident(column))
    }

    /// T-SQL that drops the default constraint of a column, if it has one:
    /// SQL Server names defaults and refuses to drop or retype a column that has one.
    fn mssql_drop_default(self, table: &TableRef, column: &str) -> String {
        let t = self.table(table);
        format!(
            "DECLARE @df sysname = (SELECT d.name FROM sys.default_constraints d JOIN sys.columns c \
             ON c.object_id = d.parent_object_id AND c.column_id = d.parent_column_id \
             WHERE d.parent_object_id = OBJECT_ID({obj}) AND c.name = {col}); \
             IF @df IS NOT NULL BEGIN DECLARE @sql nvarchar(max) = {stmt} + QUOTENAME(@df); EXEC sp_executesql @sql; END;",
            obj = self.string_literal(&t),
            col = self.string_literal(column),
            stmt = self.string_literal(&format!("ALTER TABLE {t} DROP CONSTRAINT ")),
        )
    }

    /// SQL Server keeps column comments as `MS_Description` extended properties.
    fn mssql_comment(self, table: &TableRef, column: &str, comment: &Option<String>, replace: bool) -> Option<String> {
        let schema = table.schema.as_deref().unwrap_or("dbo");
        let levels = format!(
            "@level0type = N'SCHEMA', @level0name = {}, @level1type = N'TABLE', @level1name = {}, @level2type = N'COLUMN', @level2name = {}",
            self.string_literal(schema),
            self.string_literal(&table.name),
            self.string_literal(column)
        );
        let mut sql = String::new();
        if replace {
            let obj = self.string_literal(&self.table(table));
            sql.push_str(&format!(
                "IF EXISTS (SELECT 1 FROM sys.extended_properties WHERE class = 1 AND name = N'MS_Description' \
                 AND major_id = OBJECT_ID({obj}) AND minor_id = COLUMNPROPERTY(OBJECT_ID({obj}), {col}, 'ColumnId')) \
                 EXEC sys.sp_dropextendedproperty @name = N'MS_Description', {levels};",
                col = self.string_literal(column)
            ));
        }
        if let Some(c) = comment.as_deref().filter(|c| !c.is_empty()) {
            if !sql.is_empty() {
                sql.push(' ');
            }
            sql.push_str(&format!(
                "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = {}, {levels};",
                self.string_literal(c)
            ));
        }
        (!sql.is_empty()).then_some(sql)
    }

    /// ALTER/CREATE/DROP statements for structure changes, in a safe order.
    pub fn structure_sql(self, table: &TableRef, changes: &[StructureChange]) -> Result<Vec<String>> {
        let t = self.table(table);
        let mut out = Vec::new();
        for ch in changes {
            match (self.0, ch) {
                (_, StructureChange::DropIndex { name }) => out.push(match self.0 {
                    DriverKind::MySql | DriverKind::MariaDb | DriverKind::SqlServer => {
                        format!("DROP INDEX {} ON {t};", self.quote_ident(name))
                    }
                    _ => format!("DROP INDEX {};", self.table(&TableRef::new(table.schema.as_deref(), name))),
                }),
                (_, StructureChange::AddIndex { name, columns, unique }) => {
                    let cols = columns.iter().map(|c| self.quote_ident(c)).collect::<Vec<_>>().join(", ");
                    out.push(format!(
                        "CREATE {}INDEX {} ON {t} ({cols});",
                        if *unique { "UNIQUE " } else { "" },
                        self.quote_ident(name)
                    ));
                }
                (DriverKind::SqlServer, StructureChange::DropColumn { name }) => {
                    out.push(self.mssql_drop_default(table, name));
                    out.push(format!("ALTER TABLE {t} DROP COLUMN {};", self.quote_ident(name)));
                }
                (_, StructureChange::DropColumn { name }) => {
                    out.push(format!("ALTER TABLE {t} DROP COLUMN {};", self.quote_ident(name)))
                }
                (DriverKind::SqlServer, StructureChange::AddColumn { column }) => {
                    out.push(format!("ALTER TABLE {t} ADD {};", self.column_spec(column)));
                    out.extend(self.mssql_comment(table, &column.name, &column.comment, false));
                }
                (DriverKind::SqlServer, StructureChange::ModifyColumn { old, new }) => {
                    if old.name != new.name {
                        out.push(format!(
                            "EXEC sp_rename {}, {}, N'COLUMN';",
                            self.string_literal(&format!("{t}.{}", self.quote_ident(&old.name))),
                            self.string_literal(&new.name)
                        ));
                    }
                    let col = self.quote_ident(&new.name);
                    let retype = old.data_type != new.data_type || old.nullable != new.nullable;
                    if old.default != new.default || (retype && old.default.is_some()) {
                        out.push(self.mssql_drop_default(table, &new.name));
                    }
                    if retype {
                        out.push(format!(
                            "ALTER TABLE {t} ALTER COLUMN {col} {} {};",
                            new.data_type,
                            if new.nullable { "NULL" } else { "NOT NULL" }
                        ));
                    }
                    if old.default != new.default || (retype && old.default.is_some()) {
                        if let Some(d) = new.default.as_deref().filter(|d| !d.is_empty()) {
                            out.push(format!("ALTER TABLE {t} ADD DEFAULT {d} FOR {col};"));
                        }
                    }
                    if old.comment != new.comment {
                        out.extend(self.mssql_comment(table, &new.name, &new.comment, true));
                    }
                }
                (DriverKind::Sqlite, StructureChange::AddColumn { column }) => {
                    if !column.nullable && column.default.as_deref().is_none_or(str::is_empty) {
                        bail!("SQLite cannot add a NOT NULL column without a default value ({})", column.name);
                    }
                    out.push(format!("ALTER TABLE {t} ADD COLUMN {};", self.column_spec(column)));
                }
                (_, StructureChange::AddColumn { column }) => {
                    out.push(format!("ALTER TABLE {t} ADD COLUMN {};", self.column_spec(column)));
                    if self.0 == DriverKind::Postgres && column.comment.as_deref().is_some_and(|c| !c.is_empty()) {
                        out.push(self.comment_sql(table, &column.name, &column.comment));
                    }
                }
                (DriverKind::MySql | DriverKind::MariaDb, StructureChange::ModifyColumn { old, new }) => out.push(format!(
                    "ALTER TABLE {t} CHANGE COLUMN {} {};",
                    self.quote_ident(&old.name),
                    self.column_spec(new)
                )),
                (DriverKind::Postgres, StructureChange::ModifyColumn { old, new }) => {
                    let col = self.quote_ident(&new.name);
                    if old.name != new.name {
                        out.push(format!("ALTER TABLE {t} RENAME COLUMN {} TO {col};", self.quote_ident(&old.name)));
                    }
                    if old.data_type != new.data_type {
                        out.push(format!("ALTER TABLE {t} ALTER COLUMN {col} TYPE {ty} USING {col}::{ty};", ty = new.data_type));
                    }
                    if old.nullable != new.nullable {
                        out.push(format!("ALTER TABLE {t} ALTER COLUMN {col} {} NOT NULL;", if new.nullable { "DROP" } else { "SET" }));
                    }
                    if old.default != new.default {
                        out.push(match new.default.as_deref().filter(|d| !d.is_empty()) {
                            Some(d) => format!("ALTER TABLE {t} ALTER COLUMN {col} SET DEFAULT {d};"),
                            None => format!("ALTER TABLE {t} ALTER COLUMN {col} DROP DEFAULT;"),
                        });
                    }
                    if old.comment != new.comment {
                        out.push(self.comment_sql(table, &new.name, &new.comment));
                    }
                }
                (DriverKind::Sqlite, StructureChange::ModifyColumn { old, new }) => {
                    if (&old.data_type, old.nullable, &old.default) != (&new.data_type, new.nullable, &new.default) {
                        bail!("SQLite can only rename columns; changing type, nullability or default of {} needs a table rebuild", old.name);
                    }
                    if old.name != new.name {
                        out.push(format!(
                            "ALTER TABLE {t} RENAME COLUMN {} TO {};",
                            self.quote_ident(&old.name),
                            self.quote_ident(&new.name)
                        ));
                    }
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str) -> ColumnDef {
        ColumnDef { name: name.into(), data_type: ty.into(), nullable: true, default: None, comment: None }
    }

    #[test]
    fn postgres() {
        let pg = Dialect(DriverKind::Postgres);
        let t = TableRef::new(Some("public"), "users");
        let mut new = col("full_name", "varchar(200)");
        new.nullable = false;
        new.default = Some("''".into());
        new.comment = Some("shown name".into());
        let sql = pg
            .structure_sql(&t, &[
                StructureChange::ModifyColumn { old: col("name", "text"), new },
                StructureChange::AddColumn { column: ColumnDef { comment: Some("x".into()), ..col("age", "int") } },
                StructureChange::DropColumn { name: "legacy".into() },
                StructureChange::AddIndex { name: "users_age".into(), columns: vec!["age".into()], unique: false },
                StructureChange::DropIndex { name: "old_idx".into() },
            ])
            .unwrap();
        assert_eq!(sql, vec![
            r#"ALTER TABLE "public"."users" RENAME COLUMN "name" TO "full_name";"#,
            r#"ALTER TABLE "public"."users" ALTER COLUMN "full_name" TYPE varchar(200) USING "full_name"::varchar(200);"#,
            r#"ALTER TABLE "public"."users" ALTER COLUMN "full_name" SET NOT NULL;"#,
            r#"ALTER TABLE "public"."users" ALTER COLUMN "full_name" SET DEFAULT '';"#,
            r#"COMMENT ON COLUMN "public"."users"."full_name" IS 'shown name';"#,
            r#"ALTER TABLE "public"."users" ADD COLUMN "age" int NULL;"#,
            r#"COMMENT ON COLUMN "public"."users"."age" IS 'x';"#,
            r#"ALTER TABLE "public"."users" DROP COLUMN "legacy";"#,
            r#"CREATE INDEX "users_age" ON "public"."users" ("age");"#,
            r#"DROP INDEX "public"."old_idx";"#,
        ]);
    }

    #[test]
    fn sql_server() {
        let ms = Dialect(DriverKind::SqlServer);
        let t = TableRef::new(Some("dbo"), "users");
        let sql = ms
            .structure_sql(&t, &[
                StructureChange::AddColumn { column: ColumnDef { default: Some("0".into()), comment: Some("age".into()), ..col("age", "int") } },
                StructureChange::DropIndex { name: "ix".into() },
            ])
            .unwrap();
        assert_eq!(sql[0], "ALTER TABLE [dbo].[users] ADD [age] int NULL DEFAULT 0;");
        assert!(sql[1].starts_with("EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'age'"), "{}", sql[1]);
        assert_eq!(sql[2], "DROP INDEX [ix] ON [dbo].[users];");

        let mut new = col("full_name", "nvarchar(200)");
        new.nullable = false;
        let sql = ms.structure_sql(&t, &[StructureChange::ModifyColumn { old: col("name", "nvarchar(50)"), new }]).unwrap();
        assert_eq!(sql, vec![
            "EXEC sp_rename N'[dbo].[users].[name]', N'full_name', N'COLUMN';",
            "ALTER TABLE [dbo].[users] ALTER COLUMN [full_name] nvarchar(200) NOT NULL;",
        ]);

        let sql = ms.structure_sql(&t, &[StructureChange::DropColumn { name: "legacy".into() }]).unwrap();
        assert!(sql[0].contains("sys.default_constraints") && sql[0].contains("N'legacy'"), "{}", sql[0]);
        assert_eq!(sql[1], "ALTER TABLE [dbo].[users] DROP COLUMN [legacy];");
    }

    #[test]
    fn mysql_and_sqlite() {
        let my = Dialect(DriverKind::MySql);
        let t = TableRef::new(None, "users");
        let new = ColumnDef { comment: Some("it's".into()), nullable: false, ..col("nick", "varchar(20)") };
        assert_eq!(
            my.structure_sql(&t, &[StructureChange::ModifyColumn { old: col("name", "text"), new }, StructureChange::DropIndex { name: "i".into() }]).unwrap(),
            vec!["ALTER TABLE `users` CHANGE COLUMN `name` `nick` varchar(20) NOT NULL COMMENT 'it''s';", "DROP INDEX `i` ON `users`;"]
        );
        let sl = Dialect(DriverKind::Sqlite);
        assert!(sl.structure_sql(&t, &[StructureChange::ModifyColumn { old: col("a", "text"), new: col("a", "int") }]).is_err());
        assert!(sl.structure_sql(&t, &[StructureChange::AddColumn { column: ColumnDef { nullable: false, ..col("b", "int") } }]).is_err());
        assert_eq!(
            sl.structure_sql(&t, &[StructureChange::ModifyColumn { old: col("a", "text"), new: col("b", "text") }]).unwrap(),
            vec![r#"ALTER TABLE "users" RENAME COLUMN "a" TO "b";"#]
        );
    }
}
