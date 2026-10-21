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

    /// ALTER/CREATE/DROP statements for structure changes, in a safe order.
    pub fn structure_sql(self, table: &TableRef, changes: &[StructureChange]) -> Result<Vec<String>> {
        let t = self.table(table);
        let mut out = Vec::new();
        for ch in changes {
            match (self.0, ch) {
                (_, StructureChange::DropIndex { name }) => out.push(match self.0 {
                    DriverKind::MySql | DriverKind::MariaDb => format!("DROP INDEX {} ON {t};", self.quote_ident(name)),
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
                (_, StructureChange::DropColumn { name }) => {
                    out.push(format!("ALTER TABLE {t} DROP COLUMN {};", self.quote_ident(name)))
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
