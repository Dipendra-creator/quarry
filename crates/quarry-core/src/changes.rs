//! Staged row edits. The data view collects these until the user commits
//! (Ctrl+S), previews (Ctrl+Shift+P) or discards them; AI tools build them
//! directly. Every change becomes one SQL statement.

use serde::{Deserialize, Serialize};

use crate::schema::TableRef;
use crate::sql::Dialect;
use crate::value::Value;

pub type ColumnValues = Vec<(String, Value)>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RowChange {
    Insert { values: ColumnValues },
    /// `key` identifies the row: primary-key columns, or every original
    /// column value when the table has no primary key.
    Update { key: ColumnValues, values: ColumnValues },
    Delete { key: ColumnValues },
}

/// How a row is identified, which decides whether MySQL needs `LIMIT 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    PrimaryKey,
    AllColumns,
}

impl Dialect {
    fn where_key(self, key: &ColumnValues) -> String {
        key.iter()
            .map(|(c, v)| match v {
                Value::Null => format!("{} IS NULL", self.quote_ident(c)),
                v => format!("{} = {}", self.quote_ident(c), self.literal(v)),
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    fn limit_one(self, key_kind: KeyKind) -> &'static str {
        match (self.0, key_kind) {
            (crate::model::DriverKind::MySql | crate::model::DriverKind::MariaDb, KeyKind::AllColumns) => " LIMIT 1",
            _ => "",
        }
    }

    pub fn change_sql(self, table: &TableRef, change: &RowChange, key_kind: KeyKind) -> String {
        let t = self.table(table);
        match change {
            RowChange::Insert { values } if values.is_empty() => match self.0 {
                crate::model::DriverKind::MySql | crate::model::DriverKind::MariaDb => {
                    format!("INSERT INTO {t} () VALUES ();")
                }
                _ => format!("INSERT INTO {t} DEFAULT VALUES;"),
            },
            RowChange::Insert { values } => format!(
                "INSERT INTO {t} ({}) VALUES ({});",
                values.iter().map(|(c, _)| self.quote_ident(c)).collect::<Vec<_>>().join(", "),
                values.iter().map(|(_, v)| self.literal(v)).collect::<Vec<_>>().join(", ")
            ),
            RowChange::Update { key, values } => format!(
                "UPDATE {t} SET {} WHERE {}{};",
                values
                    .iter()
                    .map(|(c, v)| format!("{} = {}", self.quote_ident(c), self.literal(v)))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.where_key(key),
                self.limit_one(key_kind)
            ),
            RowChange::Delete { key } => {
                format!("DELETE FROM {t} WHERE {}{};", self.where_key(key), self.limit_one(key_kind))
            }
        }
    }

    /// Statements for a batch, deletes last so updates still find their rows.
    pub fn changes_sql(self, table: &TableRef, changes: &[RowChange], key_kind: KeyKind) -> Vec<String> {
        let rank = |c: &RowChange| match c {
            RowChange::Update { .. } => 0,
            RowChange::Insert { .. } => 1,
            RowChange::Delete { .. } => 2,
        };
        let mut ordered: Vec<&RowChange> = changes.iter().collect();
        ordered.sort_by_key(|c| rank(c));
        ordered.into_iter().map(|c| self.change_sql(table, c, key_kind)).collect()
    }
}

/// Builds the identifying key of a row from its current values.
pub fn row_key(columns: &[String], row: &[Value], primary_key: &[String]) -> (ColumnValues, KeyKind) {
    let pick = |names: &mut dyn Iterator<Item = &String>| -> ColumnValues {
        names
            .filter_map(|n| columns.iter().position(|c| c == n).map(|i| (n.clone(), row[i].clone())))
            .collect()
    };
    if !primary_key.is_empty() && primary_key.iter().all(|k| columns.contains(k)) {
        (pick(&mut primary_key.iter()), KeyKind::PrimaryKey)
    } else {
        (pick(&mut columns.iter()), KeyKind::AllColumns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DriverKind;

    fn t() -> TableRef {
        TableRef::new(Some("public"), "users")
    }

    #[test]
    fn statements() {
        let pg = Dialect(DriverKind::Postgres);
        let upd = RowChange::Update {
            key: vec![("id".into(), Value::Int(1))],
            values: vec![("name".into(), Value::Text("O'Neil".into())), ("note".into(), Value::Null)],
        };
        assert_eq!(
            pg.change_sql(&t(), &upd, KeyKind::PrimaryKey),
            r#"UPDATE "public"."users" SET "name" = 'O''Neil', "note" = NULL WHERE "id" = 1;"#
        );
        let del = RowChange::Delete { key: vec![("a".into(), Value::Null), ("b".into(), "x".into())] };
        let my = Dialect(DriverKind::MySql);
        assert_eq!(
            my.change_sql(&TableRef::new(None, "t"), &del, KeyKind::AllColumns),
            "DELETE FROM `t` WHERE `a` IS NULL AND `b` = 'x' LIMIT 1;"
        );
        assert_eq!(
            pg.change_sql(&t(), &RowChange::Insert { values: vec![] }, KeyKind::PrimaryKey),
            r#"INSERT INTO "public"."users" DEFAULT VALUES;"#
        );
        let ins = RowChange::Insert { values: vec![("id".into(), Value::Int(2)), ("name".into(), "b".into())] };
        let all = pg.changes_sql(&t(), &[del.clone(), ins, upd], KeyKind::PrimaryKey);
        assert!(all[0].starts_with("UPDATE") && all[1].starts_with("INSERT") && all[2].starts_with("DELETE"));
    }

    #[test]
    fn keys() {
        let cols = vec!["id".to_string(), "name".to_string()];
        let row = vec![Value::Int(7), Value::Text("x".into())];
        assert_eq!(row_key(&cols, &row, &["id".into()]), (vec![("id".into(), Value::Int(7))], KeyKind::PrimaryKey));
        assert_eq!(row_key(&cols, &row, &[]).1, KeyKind::AllColumns);
        // PK column not selected: fall back to every column.
        assert_eq!(row_key(&cols, &row, &["uuid".into()]).1, KeyKind::AllColumns);
    }
}
