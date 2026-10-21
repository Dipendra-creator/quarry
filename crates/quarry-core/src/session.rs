//! An open connection plus its current database/schema. This is the API the
//! UI and the AI tools share: every user-visible database feature is a method
//! here, so anything the UI can do an agent can do too.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::changes::{KeyKind, RowChange};
use crate::driver::{self, Driver};
use crate::model::{ConnectionConfig, DriverKind, SafeMode};
use crate::query::{ANY_COLUMN, BrowseRequest, Filter, FilterOp, QueryResult};
use crate::schema::{Item, ItemKind, TableRef, TableStructure};
use crate::sql::{self, Dialect};
use crate::store::History;
use crate::value::Value;

struct State {
    database: String,
    schema: Option<String>,
    safe_mode: SafeMode,
}

pub struct Session {
    pub config: ConnectionConfig,
    /// What drivers actually connect to: `config`, or the local end of the SSH tunnel.
    connect_config: ConnectionConfig,
    _tunnel: Option<crate::ssh::Tunnel>,
    pub server_version: String,
    driver: RwLock<Arc<dyn Driver>>,
    state: Mutex<State>,
    history: History,
}

/// Result of one statement of a script.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatementOutcome {
    pub statement: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Result(QueryResult),
    Error(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHit {
    pub table: TableRef,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl Session {
    pub async fn connect(config: ConnectionConfig) -> Result<Arc<Session>> {
        let (connect_config, tunnel) = match &config.ssh {
            Some(ssh) if !config.driver.is_file_based() => {
                let tunnel = crate::ssh::open(ssh, &config.host, config.port_or_default()).await?;
                let mut c = config.clone();
                c.host = "127.0.0.1".into();
                c.port = Some(tunnel.local_port);
                (c, Some(tunnel))
            }
            _ => (config.clone(), None),
        };
        let driver = driver::connect(&connect_config, None).await?;
        let server_version = driver.server_version().await.unwrap_or_default();
        let database = driver.current_database().await.unwrap_or_default();
        let session = Arc::new(Session {
            state: Mutex::new(State { database, schema: None, safe_mode: config.safe_mode }),
            config,
            connect_config,
            _tunnel: tunnel,
            server_version,
            driver: RwLock::new(driver),
            history: History::open(),
        });
        session.reset_schema().await?;
        session.run_startup_commands().await?;
        Ok(session)
    }

    async fn run_startup_commands(&self) -> Result<()> {
        for stmt in sql::split_statements(&self.config.startup_commands) {
            self.driver()
                .execute(&stmt.text)
                .await
                .map_err(|e| anyhow!("bootstrap command failed: {}\n{e}", stmt.text))?;
        }
        Ok(())
    }

    /// Picks `public` (or the first schema) on engines with schemas.
    async fn reset_schema(&self) -> Result<()> {
        let schema = if self.kind().has_schemas() {
            let schemas = self.list_schemas().await?;
            schemas
                .iter()
                .find(|s| *s == "public")
                .or(schemas.first())
                .cloned()
        } else {
            None
        };
        self.state.lock().unwrap().schema = schema;
        Ok(())
    }

    pub fn driver(&self) -> Arc<dyn Driver> {
        self.driver.read().unwrap().clone()
    }

    pub fn kind(&self) -> DriverKind {
        self.config.driver
    }

    pub fn dialect(&self) -> Dialect {
        Dialect(self.kind())
    }

    pub fn database(&self) -> String {
        self.state.lock().unwrap().database.clone()
    }

    pub fn schema(&self) -> Option<String> {
        self.state.lock().unwrap().schema.clone()
    }

    pub fn set_schema(&self, schema: Option<String>) {
        self.state.lock().unwrap().schema = schema;
    }

    /// Current safe mode; the UI can change it while connected.
    pub fn safe_mode(&self) -> SafeMode {
        self.state.lock().unwrap().safe_mode
    }

    pub fn set_safe_mode(&self, mode: SafeMode) {
        self.state.lock().unwrap().safe_mode = mode;
    }

    /// Recent statements run on this connection, newest first.
    pub fn history(&self, limit: usize) -> Vec<crate::store::HistoryEntry> {
        self.history.entries(&self.config.id, limit)
    }

    /// Title bar text: "PostgreSQL 16.1 : mydb : public".
    pub fn title(&self) -> String {
        let mut parts = vec![format!("{} {}", self.kind().label(), self.server_version)];
        parts.push(self.database());
        if let Some(s) = self.schema() {
            parts.push(s);
        }
        parts.join(" : ")
    }

    pub async fn switch_database(&self, name: &str) -> Result<()> {
        if self.driver().use_database(name).await.is_err() {
            let fresh = driver::connect(&self.connect_config, Some(name)).await?;
            *self.driver.write().unwrap() = fresh;
        }
        self.state.lock().unwrap().database = name.to_string();
        self.reset_schema().await
    }

    /// Opens a fresh connection to the current database.
    pub async fn reconnect(&self) -> Result<()> {
        let db = self.database();
        let fresh = driver::connect(&self.connect_config, (!db.is_empty()).then_some(db.as_str())).await?;
        *self.driver.write().unwrap() = fresh;
        Ok(())
    }

    pub async fn list_databases(&self) -> Result<Vec<String>> {
        self.driver().list_databases().await
    }

    pub async fn list_schemas(&self) -> Result<Vec<String>> {
        if !self.kind().has_schemas() {
            return Ok(vec![]);
        }
        self.driver().list_schemas(self.config.show_system_schemas).await
    }

    /// Items of `schema`, or of the current schema.
    pub async fn list_items(&self, schema: Option<&str>) -> Result<Vec<Item>> {
        let current = self.schema();
        self.driver().list_items(schema.or(current.as_deref())).await
    }

    /// Qualifies a table name with the current schema where needed.
    pub fn table_ref(&self, schema: Option<&str>, name: &str) -> TableRef {
        let schema = schema.map(str::to_string).or_else(|| self.schema());
        TableRef::new(schema.as_deref(), name)
    }

    pub async fn structure(&self, table: &TableRef) -> Result<TableStructure> {
        self.driver().structure(table).await
    }

    /// `(table, column, type)` for a whole schema in one round trip.
    pub async fn all_columns(&self, schema: Option<&str>) -> Result<Vec<(String, String, String)>> {
        let current = self.schema();
        self.driver().all_columns(schema.or(current.as_deref())).await
    }

    pub async fn ddl(&self, item: &Item) -> Result<String> {
        self.driver().ddl(item).await
    }

    async fn column_names(&self, table: &TableRef, filters: &[Filter]) -> Result<Vec<String>> {
        if filters.iter().any(|f| f.enabled && f.column == ANY_COLUMN) {
            Ok(self.structure(table).await?.columns.into_iter().map(|c| c.name).collect())
        } else {
            Ok(vec![])
        }
    }

    /// One page of a table, as in the data view. Without an explicit sort,
    /// rows are ordered by primary key so pages stay stable.
    pub async fn browse(&self, req: &BrowseRequest) -> Result<QueryResult> {
        let columns = self.column_names(&req.table, &req.filters).await?;
        let mut req = req.clone();
        if req.sort.is_empty() {
            if let Ok(s) = self.structure(&req.table).await {
                req.sort = s.primary_key().into_iter().map(|column| crate::query::Sort { column, descending: false }).collect();
            }
        }
        let sql = self.dialect().select_query(&req, &columns);
        self.driver().execute(&sql).await
    }

    pub async fn count(&self, table: &TableRef, filters: &[Filter]) -> Result<u64> {
        let columns = self.column_names(table, filters).await?;
        let sql = self.dialect().count_query(table, filters, &columns);
        let r = self.driver().execute(&sql).await?;
        let v = r.rows.first().and_then(|row| row.first()).cloned().unwrap_or(Value::Int(0));
        Ok(v.display().parse().unwrap_or_default())
    }

    /// Runs one statement exactly as given and records it in history.
    pub async fn execute(&self, statement: &str) -> Result<QueryResult> {
        let _ = self.history.append(&self.config.id, &self.database(), statement);
        self.driver().execute(statement).await
    }

    /// Runs every statement of a script, stopping at the first error.
    /// `limit` appends a LIMIT to SELECTs that have none.
    pub async fn run_script(&self, script: &str, limit: Option<u32>) -> Vec<StatementOutcome> {
        let mut out = Vec::new();
        for stmt in sql::split_statements(script) {
            let text = sql::apply_auto_limit(&stmt.text, limit);
            let outcome = match self.execute(&text).await {
                Ok(r) => Outcome::Result(r),
                Err(e) => Outcome::Error(format!("{e:#}")),
            };
            let failed = matches!(outcome, Outcome::Error(_));
            out.push(StatementOutcome { statement: text, outcome });
            if failed {
                break;
            }
        }
        out
    }

    pub fn changes_sql(&self, table: &TableRef, changes: &[RowChange], key: KeyKind) -> Vec<String> {
        self.dialect().changes_sql(table, changes, key)
    }

    /// Commits staged edits in one transaction; returns the statements run.
    pub async fn apply_changes(&self, table: &TableRef, changes: &[RowChange], key: KeyKind) -> Result<Vec<String>> {
        let statements = self.changes_sql(table, changes, key);
        if statements.is_empty() {
            return Ok(statements);
        }
        for s in &statements {
            let _ = self.history.append(&self.config.id, &self.database(), s);
        }
        self.driver().execute_batch(&statements).await?;
        Ok(statements)
    }

    /// Applies structure edits (columns, indexes). PostgreSQL and SQLite run
    /// them in one transaction; MySQL commits each DDL statement on its own.
    pub async fn apply_structure(&self, table: &TableRef, changes: &[crate::alter::StructureChange]) -> Result<Vec<String>> {
        let statements = self.dialect().structure_sql(table, changes)?;
        if statements.is_empty() {
            return Ok(statements);
        }
        for s in &statements {
            let _ = self.history.append(&self.config.id, &self.database(), s);
        }
        self.driver().execute_batch(&statements).await?;
        Ok(statements)
    }

    /// Imports CSV text whose header row names columns of `table`, in one
    /// transaction. Returns the number of rows inserted.
    pub async fn import_csv(&self, table: &TableRef, csv: &str, delimiter: char, empty_as_null: bool) -> Result<usize> {
        let structure = self.structure(table).await?;
        let rows = crate::import::parse_csv(csv, delimiter);
        let statements = crate::import::insert_statements(self.dialect(), table, &structure, &rows, empty_as_null, 500)?;
        self.driver().execute_batch(&statements).await?;
        let _ = self.history.append(&self.config.id, &self.database(), &format!("-- imported {} rows into {}", rows.len().saturating_sub(1), table.qualified()));
        Ok(rows.len().saturating_sub(1))
    }

    /// Server sessions and their running statements (Ctrl+.). The first
    /// column is the id `kill` takes.
    pub async fn processes(&self) -> Result<QueryResult> {
        let sql = match self.kind() {
            DriverKind::Postgres => {
                "SELECT pid, usename AS user, datname AS database, client_addr::text AS client, state,                  now() - query_start AS duration, query FROM pg_stat_activity                  WHERE pid <> pg_backend_pid() AND backend_type = 'client backend' ORDER BY query_start NULLS LAST"
            }
            DriverKind::MySql | DriverKind::MariaDb => "SHOW FULL PROCESSLIST",
            DriverKind::Sqlite => anyhow::bail!("SQLite has no server processes"),
        };
        self.driver().execute(sql).await
    }

    /// Stops a server session's running statement (`cancel`) or the whole session.
    pub async fn kill(&self, id: i64, cancel_only: bool) -> Result<()> {
        let sql = match (self.kind(), cancel_only) {
            (DriverKind::Postgres, true) => format!("SELECT pg_cancel_backend({id})"),
            (DriverKind::Postgres, false) => format!("SELECT pg_terminate_backend({id})"),
            (DriverKind::MySql | DriverKind::MariaDb, true) => format!("KILL QUERY {id}"),
            (DriverKind::MySql | DriverKind::MariaDb, false) => format!("KILL {id}"),
            (DriverKind::Sqlite, _) => anyhow::bail!("SQLite has no server processes"),
        };
        self.execute(&sql).await.map(|_| ())
    }

    /// Tables and views of a schema with their columns: the context an
    /// assistant needs to write correct SQL.
    pub async fn overview(&self, schema: Option<&str>) -> Result<Vec<TableStructure>> {
        let mut out = Vec::new();
        for item in self.list_items(schema).await? {
            if item.kind.has_rows() {
                out.push(self.structure(&item.table).await?);
            }
        }
        Ok(out)
    }

    /// Finds `term` in any column of any table of a schema.
    pub async fn search(&self, term: &str, schema: Option<&str>, max_tables: usize, rows_per_table: u32) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();
        let items = self.list_items(schema).await?;
        for item in items.iter().filter(|i| i.kind == ItemKind::Table).take(max_tables) {
            let req = BrowseRequest {
                table: item.table.clone(),
                filters: vec![Filter { column: ANY_COLUMN.into(), op: FilterOp::ContainsCi, value: term.into(), enabled: true }],
                sort: vec![],
                limit: rows_per_table,
                offset: 0,
                columns: vec![],
            };
            match self.browse(&req).await {
                Ok(r) if !r.rows.is_empty() => hits.push(SearchHit {
                    table: item.table.clone(),
                    columns: r.columns.into_iter().map(|c| c.name).collect(),
                    rows: r.rows,
                }),
                _ => {}
            }
        }
        Ok(hits)
    }
}

/// Open sessions by connection id, shared by the UI and AI tools so both
/// see the same live connections.
#[derive(Default)]
pub struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

impl SessionManager {
    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    pub async fn connect(&self, config: ConnectionConfig) -> Result<Arc<Session>> {
        if let Some(s) = self.get(&config.id) {
            return Ok(s);
        }
        let s = Session::connect(config).await?;
        self.sessions.lock().unwrap().insert(s.config.id.clone(), s.clone());
        Ok(s)
    }

    pub fn disconnect(&self, id: &str) {
        self.sessions.lock().unwrap().remove(id);
    }

    pub fn all(&self) -> Vec<Arc<Session>> {
        self.sessions.lock().unwrap().values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::row_key;
    use crate::query::Sort;

    #[tokio::test]
    async fn sqlite_end_to_end() {
        let dir = std::env::temp_dir().join(format!("quarry-session-{}.db", uuid::Uuid::new_v4()));
        unsafe { std::env::set_var("QUARRY_CONFIG_DIR", std::env::temp_dir().join("quarry-test-config")) };
        let cfg = ConnectionConfig {
            driver: DriverKind::Sqlite,
            file_path: dir.to_string_lossy().into(),
            startup_commands: "CREATE TABLE IF NOT EXISTS people (id INTEGER PRIMARY KEY, name TEXT, city TEXT);".into(),
            ..Default::default()
        };
        let s = Session::connect(cfg).await.unwrap();
        let out = s
            .run_script("INSERT INTO people (name, city) VALUES ('Ada', 'London'), ('Linus', 'Helsinki'); select * from people", Some(1))
            .await;
        assert!(matches!(&out[0].outcome, Outcome::Result(r) if r.rows_affected == Some(2)));
        assert!(matches!(&out[1].outcome, Outcome::Result(r) if r.rows.len() == 1));
        assert_eq!(out[1].statement, "select * from people LIMIT 1");

        let t = s.table_ref(None, "people");
        let page = s
            .browse(&BrowseRequest {
                table: t.clone(),
                filters: vec![],
                sort: vec![Sort { column: "name".into(), descending: true }],
                limit: 10,
                offset: 0,
                columns: vec![],
            })
            .await
            .unwrap();
        assert_eq!(page.rows[0][1], Value::Text("Linus".into()));

        let structure = s.structure(&t).await.unwrap();
        let cols: Vec<String> = page.columns.iter().map(|c| c.name.clone()).collect();
        let (key, kind) = row_key(&cols, &page.rows[0], &structure.primary_key());
        s.apply_changes(&t, &[RowChange::Update { key, values: vec![("city".into(), "Portland".into())] }], kind)
            .await
            .unwrap();

        let hits = s.search("portland", None, 10, 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rows[0][1], Value::Text("Linus".into()));
        assert_eq!(s.count(&t, &[]).await.unwrap(), 2);

        let bad = s.run_script("select 1; select nope from nothing; select 2", None).await;
        assert_eq!(bad.len(), 2);
        assert!(matches!(bad[1].outcome, Outcome::Error(_)));
        let _ = std::fs::remove_file(dir);
    }
}
