use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow, bail};
use async_trait::async_trait;
use rusqlite::Connection;
use rusqlite::types::ValueRef;

use super::Driver;
use crate::model::{ConnectionConfig, DriverKind};
use crate::query::{QueryResult, ResultColumn};
use crate::schema::{ColumnInfo, ForeignKeyInfo, IndexInfo, Item, ItemKind, TableRef, TableStructure};
use crate::sql::Dialect;
use crate::value::Value;

pub struct SqliteDriver {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteDriver {
    pub fn open(cfg: &ConnectionConfig) -> Result<Self> {
        if cfg.file_path.trim().is_empty() {
            bail!("no SQLite file selected");
        }
        let conn = Connection::open(&cfg.file_path)
            .with_context(|| format!("could not open {}", cfg.file_path))?;
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Runs `f` on the connection off the async threads.
    async fn with<T: Send + 'static>(&self, f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static) -> Result<T> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().map_err(|_| anyhow!("SQLite connection poisoned"))?;
            f(&mut guard)
        })
        .await?
    }
}

fn cell(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Int(i),
        ValueRef::Real(f) => Value::Float(f),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Bytes(b.to_vec()),
    }
}

fn run(conn: &Connection, sql: &str) -> Result<QueryResult> {
    let start = Instant::now();
    let mut stmt = conn.prepare(sql)?;
    let mut result = QueryResult { statement: sql.to_string(), ..Default::default() };
    if stmt.column_count() == 0 {
        result.rows_affected = Some(stmt.execute([])? as u64);
    } else {
        result.columns = stmt
            .columns()
            .iter()
            .map(|c| ResultColumn { name: c.name().to_string(), type_name: c.decl_type().unwrap_or_default().to_string() })
            .collect();
        let n = result.columns.len();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            result.rows.push((0..n).map(|i| row.get_ref(i).map(cell).unwrap_or(Value::Null)).collect());
        }
    }
    result.duration_ms = start.elapsed().as_millis() as u64;
    Ok(result)
}

fn strings(conn: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    Ok(run(conn, sql)?.rows)
}

fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        v => v.display(),
    }
}

#[async_trait]
impl Driver for SqliteDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::Sqlite
    }

    async fn server_version(&self) -> Result<String> {
        Ok(rusqlite::version().to_string())
    }

    async fn current_database(&self) -> Result<String> {
        Ok("main".into())
    }

    async fn list_databases(&self) -> Result<Vec<String>> {
        self.with(|c| Ok(strings(c, "PRAGMA database_list")?.iter().map(|r| text(&r[1])).collect()))
            .await
    }

    async fn use_database(&self, _name: &str) -> Result<()> {
        Ok(())
    }

    async fn list_schemas(&self, _include_system: bool) -> Result<Vec<String>> {
        Ok(vec![])
    }

    async fn list_items(&self, _schema: Option<&str>) -> Result<Vec<Item>> {
        self.with(|c| {
            Ok(strings(
                c,
                "SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') \
                 AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
            )?
            .iter()
            .map(|r| Item {
                table: TableRef::new(None, &text(&r[0])),
                kind: if text(&r[1]) == "view" { ItemKind::View } else { ItemKind::Table },
                comment: None,
            })
            .collect())
        })
        .await
    }

    async fn structure(&self, t: &TableRef) -> Result<TableStructure> {
        let t = t.clone();
        self.with(move |c| {
            let d = Dialect(DriverKind::Sqlite);
            let q = d.string_literal(&t.name);
            let columns = strings(c, &format!("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info({q})"))?
                .iter()
                .map(|r| ColumnInfo {
                    name: text(&r[0]),
                    data_type: text(&r[1]),
                    nullable: r[2] == Value::Int(0),
                    default: (!r[3].is_null()).then(|| text(&r[3])),
                    primary_key: r[4] != Value::Int(0),
                    ..Default::default()
                })
                .collect();
            let mut indexes = Vec::new();
            for r in strings(c, &format!("SELECT name, \"unique\", origin FROM pragma_index_list({q})"))? {
                let name = text(&r[0]);
                let cols = strings(c, &format!("SELECT name FROM pragma_index_info({}) ORDER BY seqno", d.string_literal(&name)))?;
                indexes.push(IndexInfo {
                    unique: r[1] == Value::Int(1),
                    primary: text(&r[2]) == "pk",
                    columns: cols.iter().map(|r| text(&r[0])).collect(),
                    method: None,
                    definition: None,
                    name,
                });
            }
            let mut foreign_keys: Vec<ForeignKeyInfo> = Vec::new();
            for r in strings(
                c,
                &format!("SELECT id, \"table\", \"from\", \"to\", on_update, on_delete FROM pragma_foreign_key_list({q}) ORDER BY id, seq"),
            )? {
                let name = format!("fk_{}_{}", t.name, text(&r[0]));
                match foreign_keys.iter_mut().find(|f| f.name == name) {
                    Some(fk) => {
                        fk.columns.push(text(&r[2]));
                        fk.ref_columns.push(text(&r[3]));
                    }
                    None => foreign_keys.push(ForeignKeyInfo {
                        name,
                        columns: vec![text(&r[2])],
                        ref_schema: None,
                        ref_table: text(&r[1]),
                        ref_columns: vec![text(&r[3])],
                        on_update: text(&r[4]),
                        on_delete: text(&r[5]),
                    }),
                }
            }
            let mut s = TableStructure { table: t, columns, indexes, foreign_keys };
            s.link_foreign_keys();
            Ok(s)
        })
        .await
    }

    async fn ddl(&self, item: &Item) -> Result<String> {
        let name = item.table.name.clone();
        self.with(move |c| {
            let q = Dialect(DriverKind::Sqlite).string_literal(&name);
            let rows = strings(
                c,
                &format!("SELECT sql FROM sqlite_master WHERE (name = {q} OR tbl_name = {q}) AND sql IS NOT NULL ORDER BY type DESC"),
            )?;
            Ok(rows.iter().map(|r| format!("{};", text(&r[0]))).collect::<Vec<_>>().join("\n"))
        })
        .await
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        let sql = sql.to_string();
        self.with(move |c| run(c, &sql)).await
    }

    async fn execute_batch(&self, statements: &[String]) -> Result<()> {
        let statements = statements.to_vec();
        self.with(move |c| {
            let tx = c.transaction()?;
            for s in &statements {
                tx.execute_batch(s).with_context(|| s.clone())?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn introspects_and_edits() {
        let dir = std::env::temp_dir().join(format!("quarry-test-{}.db", uuid::Uuid::new_v4()));
        let cfg = ConnectionConfig { driver: DriverKind::Sqlite, file_path: dir.to_string_lossy().into(), ..Default::default() };
        let d = SqliteDriver::open(&cfg).unwrap();
        d.execute_batch(&[
            "CREATE TABLE a (id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT 'x')".into(),
            "CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id) ON DELETE CASCADE, data BLOB)".into(),
            "CREATE INDEX b_a ON b(a_id)".into(),
            "CREATE VIEW v AS SELECT * FROM a".into(),
            "INSERT INTO a (name) VALUES ('one'), ('two')".into(),
        ])
        .await
        .unwrap();

        let items = d.list_items(None).await.unwrap();
        assert_eq!(items.iter().map(|i| (i.name(), i.kind)).collect::<Vec<_>>(), vec![
            ("a", ItemKind::Table),
            ("b", ItemKind::Table),
            ("v", ItemKind::View)
        ]);

        let s = d.structure(&TableRef::new(None, "b")).await.unwrap();
        assert_eq!(s.primary_key(), vec!["id"]);
        assert_eq!(s.column("a_id").unwrap().references.as_deref(), Some("a(id)"));
        assert_eq!(s.foreign_keys[0].on_delete, "CASCADE");
        assert_eq!(s.indexes[0].columns, vec!["a_id"]);

        let r = d.execute("SELECT id, name FROM a ORDER BY id").await.unwrap();
        assert_eq!(r.rows, vec![vec![Value::Int(1), "one".into()], vec![Value::Int(2), "two".into()]]);
        let r = d.execute("UPDATE a SET name = 'z'").await.unwrap();
        assert_eq!(r.rows_affected, Some(2));

        // A failing batch leaves nothing behind.
        assert!(d.execute_batch(&["INSERT INTO a (name) VALUES ('three')".into(), "INSERT INTO nope VALUES (1)".into()]).await.is_err());
        assert_eq!(d.execute("SELECT COUNT(*) FROM a").await.unwrap().rows[0][0], Value::Int(2));

        assert!(d.ddl(&items[1]).await.unwrap().contains("CREATE INDEX b_a"));
        drop(d);
        let _ = std::fs::remove_file(dir);
    }
}
