use std::time::Instant;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use mysql_async::consts::ColumnType;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, OptsBuilder, Row, SslOpts};
use tokio::sync::Mutex;

use super::Driver;
use crate::model::{ConnectionConfig, DriverKind, SslMode};
use crate::query::{QueryResult, ResultColumn};
use crate::schema::{ColumnInfo, ForeignKeyInfo, IndexInfo, Item, ItemKind, TableRef, TableStructure};
use crate::sql::Dialect;
use crate::value::Value;

pub struct MyDriver {
    kind: DriverKind,
    conn: Mutex<Conn>,
}

const SYSTEM_DATABASES: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

impl MyDriver {
    pub async fn connect(cfg: &ConnectionConfig, database: Option<&str>) -> Result<Self> {
        let db = database.unwrap_or(&cfg.database);
        let base = OptsBuilder::default()
            .ip_or_hostname(cfg.host.clone())
            .tcp_port(cfg.port_or_default())
            .user(Some(cfg.user.clone()))
            .pass(cfg.password.clone().filter(|p| !p.is_empty()))
            .db_name((!db.is_empty()).then(|| db.to_string()))
            .prefer_socket(false);
        let verify = matches!(cfg.ssl_mode, SslMode::VerifyCa | SslMode::VerifyFull);
        let ssl = SslOpts::default()
            .with_danger_accept_invalid_certs(!verify)
            .with_danger_skip_domain_validation(cfg.ssl_mode != SslMode::VerifyFull);
        let conn = match cfg.ssl_mode {
            SslMode::Disable => Conn::new(base).await,
            // "Preferred": try TLS first, then plain.
            SslMode::Prefer => match Conn::new(base.clone().ssl_opts(Some(ssl))).await {
                Ok(c) => Ok(c),
                Err(_) => Conn::new(base).await,
            },
            _ => Conn::new(base.ssl_opts(Some(ssl))).await,
        }
        .context("could not connect to MySQL")?;
        Ok(Self { kind: cfg.driver, conn: Mutex::new(conn) })
    }

    async fn rows(&self, sql: &str, params: Vec<mysql_async::Value>) -> Result<Vec<Vec<Option<String>>>> {
        let mut conn = self.conn.lock().await;
        let rows: Vec<Row> = if params.is_empty() {
            conn.query(sql).await?
        } else {
            conn.exec(sql, params).await?
        };
        Ok(rows
            .iter()
            .map(|r| (0..r.len()).map(|i| r.as_ref(i).and_then(text)).collect())
            .collect())
    }

    async fn database_or_current(&self, schema: Option<&str>) -> Result<String> {
        match schema {
            Some(s) if !s.is_empty() => Ok(s.to_string()),
            _ => self.current_database().await,
        }
    }
}

/// Any MySQL value as text; `None` for NULL.
fn text(v: &mysql_async::Value) -> Option<String> {
    use mysql_async::Value as V;
    Some(match v {
        V::NULL => return None,
        V::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        V::Int(i) => i.to_string(),
        V::UInt(u) => u.to_string(),
        V::Float(f) => f.to_string(),
        V::Double(d) => d.to_string(),
        V::Date(y, mo, d, h, mi, s, us) => {
            if *h == 0 && *mi == 0 && *s == 0 && *us == 0 {
                format!("{y:04}-{mo:02}-{d:02}")
            } else {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
            }
        }
        V::Time(neg, days, h, mi, s, _) => {
            format!("{}{:02}:{mi:02}:{s:02}", if *neg { "-" } else { "" }, *days * 24 + u32::from(*h))
        }
    })
}

fn cell(v: Option<&mysql_async::Value>, col: &mysql_async::Column) -> Value {
    let Some(v) = v else {
        return Value::Null;
    };
    let Some(t) = text(v) else {
        return Value::Null;
    };
    use ColumnType::*;
    match col.column_type() {
        MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG
        | MYSQL_TYPE_YEAR => t.parse().map(Value::Int).unwrap_or(Value::Text(t)),
        MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => t.parse().map(Value::Float).unwrap_or(Value::Text(t)),
        // Charset 63 marks binary data (BLOB, BINARY, BIT), even when it happens to be valid UTF-8.
        MYSQL_TYPE_BLOB | MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_STRING
        | MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_BIT | MYSQL_TYPE_GEOMETRY
            if col.character_set() == 63 =>
        {
            match v {
                mysql_async::Value::Bytes(b) => Value::Bytes(b.clone()),
                _ => Value::Text(t),
            }
        }
        _ => Value::Text(t),
    }
}

fn type_name(t: ColumnType) -> &'static str {
    use ColumnType::*;
    match t {
        MYSQL_TYPE_TINY => "TINYINT",
        MYSQL_TYPE_SHORT => "SMALLINT",
        MYSQL_TYPE_INT24 => "MEDIUMINT",
        MYSQL_TYPE_LONG => "INT",
        MYSQL_TYPE_LONGLONG => "BIGINT",
        MYSQL_TYPE_FLOAT => "FLOAT",
        MYSQL_TYPE_DOUBLE => "DOUBLE",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "DECIMAL",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "DATE",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "DATETIME",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "TIMESTAMP",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "TIME",
        MYSQL_TYPE_YEAR => "YEAR",
        MYSQL_TYPE_JSON => "JSON",
        MYSQL_TYPE_BIT => "BIT",
        MYSQL_TYPE_ENUM => "ENUM",
        MYSQL_TYPE_SET => "SET",
        MYSQL_TYPE_GEOMETRY => "GEOMETRY",
        MYSQL_TYPE_BLOB | MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB => "BLOB",
        _ => "VARCHAR",
    }
}

#[async_trait]
impl Driver for MyDriver {
    fn kind(&self) -> DriverKind {
        self.kind
    }

    async fn server_version(&self) -> Result<String> {
        let rows = self.rows("SELECT VERSION()", vec![]).await?;
        Ok(rows.into_iter().next().and_then(|r| r.into_iter().next().flatten()).unwrap_or_default())
    }

    async fn current_database(&self) -> Result<String> {
        let rows = self.rows("SELECT DATABASE()", vec![]).await?;
        Ok(rows.into_iter().next().and_then(|r| r.into_iter().next().flatten()).unwrap_or_default())
    }

    async fn list_databases(&self) -> Result<Vec<String>> {
        Ok(self
            .rows("SHOW DATABASES", vec![])
            .await?
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
            .collect())
    }

    async fn use_database(&self, name: &str) -> Result<()> {
        let sql = format!("USE {}", Dialect(self.kind).quote_ident(name));
        self.conn.lock().await.query_drop(sql).await?;
        Ok(())
    }

    async fn list_schemas(&self, include_system: bool) -> Result<Vec<String>> {
        // MySQL databases are its schemas; the sidebar uses databases instead.
        let dbs = self.list_databases().await?;
        Ok(dbs
            .into_iter()
            .filter(|d| include_system || !SYSTEM_DATABASES.contains(&d.as_str()))
            .collect())
    }

    async fn list_items(&self, schema: Option<&str>) -> Result<Vec<Item>> {
        let db = self.database_or_current(schema).await?;
        let mut items: Vec<Item> = self
            .rows(
                "SELECT TABLE_NAME, TABLE_TYPE, TABLE_COMMENT FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
                vec![db.clone().into()],
            )
            .await?
            .into_iter()
            .map(|r| Item {
                table: TableRef::new(None, r[0].as_deref().unwrap_or_default()),
                kind: if r[1].as_deref() == Some("VIEW") { ItemKind::View } else { ItemKind::Table },
                comment: r[2].clone().filter(|c| !c.is_empty()),
            })
            .collect();
        let routines = self
            .rows(
                "SELECT ROUTINE_NAME, ROUTINE_TYPE FROM information_schema.ROUTINES \
                 WHERE ROUTINE_SCHEMA = ? ORDER BY ROUTINE_NAME",
                vec![db.into()],
            )
            .await?;
        items.extend(routines.into_iter().map(|r| Item {
            table: TableRef::new(None, r[0].as_deref().unwrap_or_default()),
            kind: if r[1].as_deref() == Some("PROCEDURE") { ItemKind::Procedure } else { ItemKind::Function },
            comment: None,
        }));
        Ok(items)
    }

    async fn structure(&self, t: &TableRef) -> Result<TableStructure> {
        let db = self.database_or_current(t.schema.as_deref()).await?;
        let params = || vec![db.clone().into(), t.name.clone().into()];
        let columns = self
            .rows(
                "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, COLUMN_KEY, EXTRA, COLUMN_COMMENT \
                 FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
                params(),
            )
            .await?
            .into_iter()
            .map(|r| ColumnInfo {
                name: r[0].clone().unwrap_or_default(),
                data_type: r[1].clone().unwrap_or_default(),
                nullable: r[2].as_deref() == Some("YES"),
                default: r[3].clone(),
                primary_key: r[4].as_deref() == Some("PRI"),
                extra: r[5].clone().filter(|e| !e.is_empty()),
                comment: r[6].clone().filter(|c| !c.is_empty()),
                references: None,
            })
            .collect();
        let mut indexes: Vec<IndexInfo> = Vec::new();
        for r in self
            .rows(
                "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME, INDEX_TYPE FROM information_schema.STATISTICS \
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY INDEX_NAME, SEQ_IN_INDEX",
                params(),
            )
            .await?
        {
            let name = r[0].clone().unwrap_or_default();
            let column = r[2].clone().unwrap_or_default();
            match indexes.iter_mut().find(|i| i.name == name) {
                Some(ix) => ix.columns.push(column),
                None => indexes.push(IndexInfo {
                    primary: name == "PRIMARY",
                    unique: r[1].as_deref() == Some("0"),
                    method: r[3].clone(),
                    columns: vec![column],
                    definition: None,
                    name,
                }),
            }
        }
        let mut foreign_keys: Vec<ForeignKeyInfo> = Vec::new();
        for r in self
            .rows(
                "SELECT k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME, \
                 k.REFERENCED_COLUMN_NAME, c.UPDATE_RULE, c.DELETE_RULE \
                 FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS c \
                 ON c.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND c.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
                 WHERE k.TABLE_SCHEMA = ? AND k.TABLE_NAME = ? ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
                params(),
            )
            .await?
        {
            let name = r[0].clone().unwrap_or_default();
            let (col, ref_col) = (r[1].clone().unwrap_or_default(), r[4].clone().unwrap_or_default());
            match foreign_keys.iter_mut().find(|f| f.name == name) {
                Some(fk) => {
                    fk.columns.push(col);
                    fk.ref_columns.push(ref_col);
                }
                None => foreign_keys.push(ForeignKeyInfo {
                    name,
                    columns: vec![col],
                    ref_schema: r[2].clone().filter(|s| *s != db),
                    ref_table: r[3].clone().unwrap_or_default(),
                    ref_columns: vec![ref_col],
                    on_update: r[5].clone().unwrap_or_default(),
                    on_delete: r[6].clone().unwrap_or_default(),
                }),
            }
        }
        let mut s = TableStructure { table: t.clone(), columns, indexes, foreign_keys };
        s.link_foreign_keys();
        Ok(s)
    }

    async fn ddl(&self, item: &Item) -> Result<String> {
        let d = Dialect(self.kind);
        let (kw, col) = match item.kind {
            ItemKind::View => ("VIEW", 1),
            ItemKind::Function => ("FUNCTION", 2),
            ItemKind::Procedure => ("PROCEDURE", 2),
            _ => ("TABLE", 1),
        };
        let rows = self.rows(&format!("SHOW CREATE {kw} {}", d.table(&item.table)), vec![]).await?;
        Ok(rows
            .into_iter()
            .next()
            .and_then(|r| r.get(col).cloned().flatten())
            .map(|s| format!("{s};"))
            .unwrap_or_default())
    }

    async fn all_columns(&self, schema: Option<&str>) -> Result<Vec<(String, String, String)>> {
        let db = self.database_or_current(schema).await?;
        Ok(self
            .rows(
                "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE FROM information_schema.COLUMNS                  WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, ORDINAL_POSITION",
                vec![db.into()],
            )
            .await?
            .into_iter()
            .map(|r| (r[0].clone().unwrap_or_default(), r[1].clone().unwrap_or_default(), r[2].clone().unwrap_or_default()))
            .collect())
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        let start = Instant::now();
        let mut conn = self.conn.lock().await;
        let mut res = conn.query_iter(sql).await?;
        let cols: Vec<mysql_async::Column> = res.columns().map(|c| c.to_vec()).unwrap_or_default();
        let rows: Vec<Row> = res.collect().await?;
        let affected = res.affected_rows();
        drop(res);
        let mut result = QueryResult {
            statement: sql.to_string(),
            columns: cols
                .iter()
                .map(|c| ResultColumn { name: c.name_str().into_owned(), type_name: type_name(c.column_type()).into() })
                .collect(),
            rows: rows
                .iter()
                .map(|r| cols.iter().enumerate().map(|(i, c)| cell(r.as_ref(i), c)).collect())
                .collect(),
            rows_affected: None,
            duration_ms: 0,
        };
        if cols.is_empty() {
            result.rows_affected = Some(affected);
        }
        result.duration_ms = start.elapsed().as_millis() as u64;
        Ok(result)
    }

    async fn execute_batch(&self, statements: &[String]) -> Result<()> {
        let mut conn = self.conn.lock().await;
        conn.query_drop("START TRANSACTION").await?;
        for s in statements {
            if let Err(e) = conn.query_drop(s.as_str()).await {
                let _ = conn.query_drop("ROLLBACK").await;
                return Err(anyhow::Error::from(e).context(s.clone()));
            }
        }
        conn.query_drop("COMMIT").await?;
        Ok(())
    }
}
