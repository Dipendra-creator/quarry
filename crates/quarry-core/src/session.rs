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
    /// The schema the server resolved unqualified names to when this
    /// database was opened (the login's default schema on SQL Server).
    home_schema: Option<String>,
    /// The user picked `schema` (rather than starting in the server's), so
    /// a reconnect must apply it again.
    schema_chosen: bool,
    safe_mode: SafeMode,
    /// Read-only mode: only reads run. Every connection starts in it.
    read_only: bool,
}

/// Why a change was refused in read-only mode, for people and for AI tools.
pub const READ_ONLY_REFUSAL: &str =
    "is in read-only mode, so nothing was changed. Switch the title bar's toggle to Full access to change data or structure";

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
            state: Mutex::new(State {
                database,
                schema: None,
                home_schema: None,
                schema_chosen: false,
                safe_mode: config.safe_mode,
                read_only: true,
            }),
            config,
            connect_config,
            _tunnel: tunnel,
            server_version,
            driver: RwLock::new(driver),
            history: History::open(),
        });
        // Bootstrap commands first: a `SET search_path` there picks the starting schema.
        session.run_startup_commands(&*session.driver()).await?;
        session.reset_schema().await?;
        // After the bootstrap commands, which may need to write.
        session.driver().set_read_only(true).await?;
        Ok(session)
    }

    async fn run_startup_commands(&self, driver: &dyn Driver) -> Result<()> {
        for stmt in sql::split_for(self.kind(), &self.config.startup_commands) {
            driver
                .execute(&stmt.text)
                .await
                .map_err(|e| anyhow!("bootstrap command failed: {}\n{e}", stmt.text))?;
        }
        Ok(())
    }

    /// Starts in the schema the server resolves names to (`search_path`, the
    /// user's default schema), else `public`/`dbo`, else the first schema.
    async fn reset_schema(&self) -> Result<()> {
        let (schema, home) = if self.kind().has_schemas() {
            let schemas = self.list_schemas().await?;
            let current = self.driver().default_schema().await.ok().flatten();
            let schema = current
                .clone()
                .filter(|c| schemas.contains(c))
                .or_else(|| self.kind().default_schema().map(str::to_string).filter(|d| schemas.contains(d)))
                .or_else(|| schemas.first().cloned());
            (schema, current)
        } else {
            (None, None)
        };
        let mut state = self.state.lock().unwrap();
        state.schema = schema;
        state.home_schema = home;
        Ok(())
    }

    /// The schema unqualified names in SQL resolve to: the current schema on
    /// PostgreSQL, the login's default schema on SQL Server.
    pub fn unqualified_schema(&self) -> Option<String> {
        match self.kind().base() {
            DriverKind::SqlServer => self.state.lock().unwrap().home_schema.clone(),
            _ => self.schema(),
        }
    }

    /// A fresh connection with the bootstrap commands and the schema the user
    /// chose applied, so reconnecting changes nothing the user can see. A
    /// schema the user never picked is left to the bootstrap commands.
    async fn fresh_driver(&self, database: Option<&str>) -> Result<Arc<dyn Driver>> {
        let fresh = driver::connect(&self.connect_config, database).await?;
        self.run_startup_commands(&*fresh).await?;
        let chosen = {
            let state = self.state.lock().unwrap();
            state.schema.clone().filter(|_| state.schema_chosen)
        };
        if let Some(schema) = chosen {
            fresh.use_schema(&schema).await?;
        }
        fresh.set_read_only(self.read_only()).await?;
        Ok(fresh)
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

    /// Switches the current schema: the sidebar, autocomplete and AI context
    /// follow it, and on PostgreSQL unqualified names in SQL resolve to it
    /// (`search_path`). SQL Server has no per-session default schema, so
    /// there unqualified names keep resolving to the login's default schema.
    pub async fn use_schema(&self, schema: &str) -> Result<()> {
        self.driver().use_schema(schema).await?;
        let mut state = self.state.lock().unwrap();
        state.schema = Some(schema.to_string());
        state.schema_chosen = true;
        Ok(())
    }

    /// Whether only reads may run (the title bar's Read only / Full access toggle).
    pub fn read_only(&self) -> bool {
        self.state.lock().unwrap().read_only
    }

    /// Switches between read-only mode and full access.
    pub async fn set_read_only(&self, on: bool) -> Result<()> {
        self.driver().set_read_only(on).await?;
        self.state.lock().unwrap().read_only = on;
        Ok(())
    }

    /// An error unless changes are allowed (full access).
    pub fn ensure_writable(&self) -> Result<()> {
        if self.read_only() {
            anyhow::bail!("{} {READ_ONLY_REFUSAL}.", self.config.display_name());
        }
        Ok(())
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
        // The new database has its own schemas; start from the server's default.
        let previous = {
            let mut state = self.state.lock().unwrap();
            state.schema_chosen = false;
            state.schema.take()
        };
        if self.driver().use_database(name).await.is_err() {
            match self.fresh_driver(Some(name)).await {
                Ok(fresh) => *self.driver.write().unwrap() = fresh,
                Err(e) => {
                    self.state.lock().unwrap().schema = previous;
                    return Err(e);
                }
            }
        }
        self.state.lock().unwrap().database = name.to_string();
        self.reset_schema().await
    }

    /// Opens a fresh connection to the current database and schema.
    pub async fn reconnect(&self) -> Result<()> {
        let db = self.database();
        let fresh = self.fresh_driver((!db.is_empty()).then_some(db.as_str())).await?;
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
        if let Some(r) = self.driver().browse(req).await {
            return r;
        }
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

    /// The statement a browse runs, written for the editor: SQL, or a
    /// MongoDB `find()`.
    pub fn browse_text(&self, req: &BrowseRequest, columns: &[String]) -> Result<String> {
        match self.driver().browse_text(req) {
            Some(r) => r,
            None if self.kind().is_sql() => Ok(format!("{};", self.dialect().select_query(req, columns))),
            None => anyhow::bail!("{} has no query language for this", self.kind().label()),
        }
    }

    pub async fn count(&self, table: &TableRef, filters: &[Filter]) -> Result<u64> {
        if let Some(r) = self.driver().count(table, filters).await {
            return r;
        }
        let columns = self.column_names(table, filters).await?;
        let sql = self.dialect().count_query(table, filters, &columns);
        let r = self.driver().execute(&sql).await?;
        let v = r.rows.first().and_then(|row| row.first()).cloned().unwrap_or(Value::Int(0));
        Ok(v.display().parse().unwrap_or_default())
    }

    /// Runs one statement exactly as given and records it in history. In
    /// read-only mode, only statements that just read are run.
    pub async fn execute(&self, statement: &str) -> Result<QueryResult> {
        if !sql::is_read_only_for(self.kind(), statement) {
            self.ensure_writable()?;
        }
        let _ = self.history.append(&self.config.id, &self.database(), statement);
        self.driver().execute(statement).await
    }

    /// Runs every statement of a script, stopping at the first error.
    /// `limit` appends a LIMIT to SELECTs that have none.
    pub async fn run_script(&self, script: &str, limit: Option<u32>) -> Vec<StatementOutcome> {
        let mut out = Vec::new();
        for stmt in sql::split_for(self.kind(), script) {
            let text = sql::apply_auto_limit_for(self.kind(), &stmt.text, limit);
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

    /// Statements that apply staged grid edits, for the preview. On Redis
    /// they are commands; an edit Redis cannot make shows why instead.
    pub fn changes_sql(&self, table: &TableRef, changes: &[RowChange], key: KeyKind) -> Vec<String> {
        match self.driver().changes_text(table, changes, key) {
            Some(Ok(commands)) => commands,
            Some(Err(e)) => vec![format!("# {e:#}")],
            None => self.dialect().changes_sql(table, changes, key),
        }
    }

    /// Commits staged edits in one transaction; returns the statements run.
    pub async fn apply_changes(&self, table: &TableRef, changes: &[RowChange], key: KeyKind) -> Result<Vec<String>> {
        self.ensure_writable()?;
        let statements = match self.driver().changes_text(table, changes, key) {
            Some(r) => r?,
            None => self.dialect().changes_sql(table, changes, key),
        };
        if statements.is_empty() {
            return Ok(statements);
        }
        for s in &statements {
            let _ = self.history.append(&self.config.id, &self.database(), s);
        }
        self.driver().execute_batch(&statements).await?;
        Ok(statements)
    }

    /// Applies structure edits (columns, indexes). PostgreSQL, SQL Server and
    /// SQLite run them in one transaction. Elsewhere each change commits on
    /// its own, so they run one by one and an error says which were made.
    pub async fn apply_structure(&self, table: &TableRef, changes: &[crate::alter::StructureChange]) -> Result<Vec<String>> {
        self.ensure_writable()?;
        let statements = self.dialect().structure_sql(table, changes)?;
        if statements.is_empty() {
            return Ok(statements);
        }
        for s in &statements {
            let _ = self.history.append(&self.config.id, &self.database(), s);
        }
        if self.kind().transactional_ddl() {
            self.driver().execute_batch(&statements).await?;
            return Ok(statements);
        }
        // Each change commits on its own here, so say which ones did.
        for (i, statement) in statements.iter().enumerate() {
            if let Err(e) = self.driver().execute(statement).await {
                if i == 0 {
                    return Err(e);
                }
                return Err(e.context(format!(
                    "{i} of {} changes were applied before this error, and {} cannot undo schema changes, so they stay. \
                     Reload the structure to see them.",
                    statements.len(),
                    self.kind().label()
                )));
            }
        }
        Ok(statements)
    }

    /// Imports CSV text whose header row names columns of `table`, in one
    /// transaction. Returns the number of rows inserted.
    pub async fn import_csv(&self, table: &TableRef, csv: &str, delimiter: char, empty_as_null: bool) -> Result<usize> {
        self.ensure_writable()?;
        let structure = self.structure(table).await?;
        let rows = crate::import::parse_csv(csv, delimiter);
        let statements = crate::import::insert_statements(self.dialect(), table, &structure, &rows, empty_as_null, 500)?;
        self.driver().execute_batch(&statements).await?;
        let _ = self.history.append(&self.config.id, &self.database(), &format!("-- imported {} rows into {}", rows.len().saturating_sub(1), table.qualified()));
        Ok(rows.len().saturating_sub(1))
    }

    /// The server's plan for `statement`. `analyze` runs it to report real
    /// timings, so it is refused for statements that change data.
    pub async fn explain(&self, statement: &str, analyze: bool) -> Result<QueryResult> {
        let stmt = statement.trim().trim_end_matches(';');
        if analyze && !sql::is_read_only_for(self.kind(), stmt) {
            anyhow::bail!("EXPLAIN ANALYZE runs the statement, so only read-only statements can be analyzed");
        }
        if let Some(plan) = self.driver().explain(stmt, analyze).await? {
            let _ = self.history.append(&self.config.id, &self.database(), &format!("-- plan of: {stmt}"));
            return Ok(plan);
        }
        let sql = match (self.kind(), analyze) {
            (DriverKind::Postgres | DriverKind::YugabyteDb | DriverKind::CockroachDb | DriverKind::TiDb, false) => {
                format!("EXPLAIN {stmt}")
            }
            (DriverKind::Postgres | DriverKind::YugabyteDb, true) => format!("EXPLAIN (ANALYZE, BUFFERS) {stmt}"),
            // CockroachDB has no BUFFERS; TiDB has no FORMAT=TREE.
            (DriverKind::CockroachDb | DriverKind::TiDb, true) => format!("EXPLAIN ANALYZE {stmt}"),
            (DriverKind::MySql, false) => format!("EXPLAIN FORMAT=TREE {stmt}"),
            (DriverKind::MySql, true) => format!("EXPLAIN ANALYZE {stmt}"),
            (DriverKind::MariaDb, false) => format!("EXPLAIN {stmt}"),
            (DriverKind::MariaDb, true) => format!("ANALYZE {stmt}"),
            (DriverKind::Sqlite, _) => format!("EXPLAIN QUERY PLAN {stmt}"),
            (DriverKind::SqlServer, _) => anyhow::bail!("the SQL Server driver did not return a plan"),
            (DriverKind::Redis, _) => anyhow::bail!("Redis commands have no query plans"),
            (DriverKind::MongoDb, _) => anyhow::bail!("add .explain() to a find() or aggregate() in the editor to see its plan"),
        };
        // Straight to the driver: the statement was checked above, and MariaDB's
        // ANALYZE does not look like a read to the read-only check.
        let _ = self.history.append(&self.config.id, &self.database(), &sql);
        match self.driver().execute(&sql).await {
            // FORMAT=TREE needs MySQL 8.0.16; older servers get the table form.
            Err(_) if self.kind() == DriverKind::MySql && !analyze => self.driver().execute(&format!("EXPLAIN {stmt}")).await,
            r => r,
        }
    }

    /// Server sessions and their running statements (Ctrl+.). The first
    /// column is the id `kill` takes.
    pub async fn processes(&self) -> Result<QueryResult> {
        let sql = match self.kind() {
            DriverKind::CockroachDb => {
                "SELECT session_id AS id, user_name AS user, client_address AS client, application_name, status, \
                 now() - active_query_start AS duration, active_queries AS query FROM [SHOW CLUSTER SESSIONS] \
                 WHERE session_id <> (SELECT session_id FROM [SHOW session_id]) ORDER BY active_query_start NULLS LAST"
            }
            DriverKind::Postgres | DriverKind::YugabyteDb => {
                "SELECT pid, usename AS user, datname AS database, client_addr::text AS client, state,                  now() - query_start AS duration, query FROM pg_stat_activity                  WHERE pid <> pg_backend_pid() AND backend_type = 'client backend' ORDER BY query_start NULLS LAST"
            }
            DriverKind::MySql | DriverKind::MariaDb | DriverKind::TiDb => "SHOW FULL PROCESSLIST",
            DriverKind::SqlServer => {
                "SELECT s.session_id AS id, s.login_name AS [user], DB_NAME(s.database_id) AS [database], \
                 s.host_name AS client, s.status, r.total_elapsed_time AS duration_ms, t.text AS query \
                 FROM sys.dm_exec_sessions s LEFT JOIN sys.dm_exec_requests r ON r.session_id = s.session_id \
                 OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) t \
                 WHERE s.is_user_process = 1 AND s.session_id <> @@SPID ORDER BY s.session_id"
            }
            DriverKind::Sqlite => anyhow::bail!("SQLite has no server processes"),
            DriverKind::Redis => "CLIENT LIST",
            DriverKind::MongoDb => "db.currentOp()",
        };
        self.driver().execute(sql).await
    }

    /// Stops a server session's running statement (`cancel`) or the whole
    /// session. `id` is the first column of [`Self::processes`]: a number, or
    /// on CockroachDB a hex session id.
    pub async fn kill(&self, id: &str, cancel_only: bool) -> Result<()> {
        self.ensure_writable()?;
        let id = id.trim();
        if self.kind() == DriverKind::MongoDb {
            // Operation ids are numbers, or "shard:number" on a sharded cluster.
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '-' || c == '_') {
                anyhow::bail!("{id:?} is not a MongoDB operation id");
            }
            let op = if id.chars().all(|c| c.is_ascii_digit()) { id.to_string() } else { serde_json::to_string(id)? };
            // MongoDB stops operations, not connections, so both buttons do the same.
            return self.execute(&format!("db.adminCommand({{killOp: 1, op: {op}}})")).await.map(|_| ());
        }
        if self.kind() == DriverKind::CockroachDb {
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
                anyhow::bail!("{id:?} is not a CockroachDB session id");
            }
            let sql = if cancel_only {
                format!("CANCEL QUERIES IF EXISTS (SELECT query_id FROM [SHOW CLUSTER QUERIES] WHERE session_id = '{id}')")
            } else {
                format!("CANCEL SESSION '{id}'")
            };
            return self.execute(&sql).await.map(|_| ());
        }
        let id: i64 = id.parse().map_err(|_| anyhow!("{id:?} is not a process id"))?;
        let sql = match (self.kind().base(), cancel_only) {
            (DriverKind::Postgres, true) => format!("SELECT pg_cancel_backend({id})"),
            (DriverKind::Postgres, false) => format!("SELECT pg_terminate_backend({id})"),
            (DriverKind::MySql | DriverKind::MariaDb, true) => format!("KILL QUERY {id}"),
            (DriverKind::MySql | DriverKind::MariaDb, false) => format!("KILL {id}"),
            (DriverKind::SqlServer, true) => {
                anyhow::bail!("SQL Server cannot cancel another session's query on its own; use Kill Process to end the session")
            }
            (DriverKind::SqlServer, false) => format!("KILL {id}"),
            (DriverKind::Sqlite, _) => anyhow::bail!("SQLite has no server processes"),
            (DriverKind::Redis, true) => anyhow::bail!("Redis cannot stop a client's command on its own; use Kill Process to close the connection"),
            (DriverKind::Redis, false) => format!("CLIENT KILL ID {id}"),
            (DriverKind::CockroachDb | DriverKind::YugabyteDb | DriverKind::TiDb | DriverKind::MongoDb, _) => unreachable!("handled above, or base() is an engine"),
        };
        self.execute(&sql).await.map(|_| ())
    }

    /// Tables and views of a schema with their columns: the context an
    /// assistant needs to write correct SQL.
    pub async fn overview(&self, schema: Option<&str>) -> Result<Vec<TableStructure>> {
        let mut out = Vec::new();
        // A Redis database can hold millions of keys; a sample describes it.
        let max = if self.kind() == DriverKind::Redis { 100 } else { usize::MAX };
        for item in self.list_items(schema).await?.into_iter().filter(|i| i.kind.has_rows()).take(max) {
            out.push(self.structure(&item.table).await?);
        }
        Ok(out)
    }

    /// Finds `term` in any column of any table of a schema.
    pub async fn search(&self, term: &str, schema: Option<&str>, max_tables: usize, rows_per_table: u32) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();
        let items = self.list_items(schema).await?;
        for item in items.iter().filter(|i| matches!(i.kind, ItemKind::Table | ItemKind::Collection)).take(max_tables) {
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

        // Connections start read-only: writes are refused by Quarry and by SQLite itself.
        assert!(s.read_only());
        let blocked = s.run_script("select 1; INSERT INTO people (name) VALUES ('x')", None).await;
        assert!(matches!(&blocked[1].outcome, Outcome::Error(e) if e.contains("read-only mode")), "{blocked:?}");
        assert!(s.driver().execute("INSERT INTO people (name) VALUES ('x')").await.is_err(), "query_only refuses writes");
        let t = s.table_ref(None, "people");
        assert!(s.apply_changes(&t, &[RowChange::Insert { values: vec![] }], KeyKind::PrimaryKey).await.is_err());
        assert!(s.import_csv(&t, "name\nx", ',', true).await.is_err());
        assert_eq!(s.count(&t, &[]).await.unwrap(), 0, "reads still work");
        s.set_read_only(false).await.unwrap();

        let out = s
            .run_script("INSERT INTO people (name, city) VALUES ('Ada', 'London'), ('Linus', 'Helsinki'); select * from people", Some(1))
            .await;
        assert!(matches!(&out[0].outcome, Outcome::Result(r) if r.rows_affected == Some(2)));
        assert!(matches!(&out[1].outcome, Outcome::Result(r) if r.rows.len() == 1));
        assert_eq!(out[1].statement, "select * from people LIMIT 1");

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
        assert!(!s.explain("select * from people where id = 1", false).await.unwrap().rows.is_empty());
        assert!(s.explain("delete from people", true).await.is_err());

        let bad = s.run_script("select 1; select nope from nothing; select 2", None).await;
        assert_eq!(bad.len(), 2);
        assert!(matches!(bad[1].outcome, Outcome::Error(_)));
        let _ = std::fs::remove_file(dir);
    }
}
