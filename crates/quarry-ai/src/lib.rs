//! Every Quarry database feature as a JSON-in/JSON-out tool.
//!
//! [`Toolbox::tools`] describes the tools (name, description, JSON schema);
//! [`Toolbox::call`] runs one. The MCP server binary exposes them to any
//! agent, and the in-app assistant ([`assistant`]) uses the same `Toolbox`
//! with the app's live [`SessionManager`]. [`bridge`] lets an MCP server
//! started outside the app reach those live connections.
//!
//! Write policy: every statement that changes a database passes a
//! [`WriteGate`]. The standalone MCP server's gate treats a connection whose
//! safe mode is anything but `Silent` as read-only; the app's gate
//! ([`ApprovalGate`]) asks the user.

pub mod assistant;
pub mod bridge;

use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use quarry_core::changes::{ColumnValues, KeyKind, RowChange};
use quarry_core::query::{BrowseRequest, Filter, Sort};
use quarry_core::session::{Outcome, Session, SessionManager};
use quarry_core::store::ConnectionStore;
use quarry_core::{ConnectionConfig, DriverKind, Item, ItemKind, SafeMode, TableRef, Value, sql};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};
use tokio::sync::{mpsc, oneshot};

const MAX_ROWS: usize = 1000;
const DEFAULT_ROWS: usize = 100;

/// Tools agents may not use while the app owns the connections: they would
/// close or repoint a connection under the user's open tabs.
pub const APP_HIDDEN: &[&str] = &["disconnect", "save_connection", "switch_database"];

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Json,
}

/// Decides whether a tool may run statements that change a database.
#[async_trait::async_trait]
pub trait WriteGate: Send + Sync {
    /// `sql` is what would run. An error refuses, with a message for the model.
    async fn allow(&self, session: &Session, sql: &str) -> Result<()>;
}

/// Changes only on connections in Silent safe mode: the standalone MCP
/// server's policy, since nobody is there to ask.
pub struct SafeModeGate;

#[async_trait::async_trait]
impl WriteGate for SafeModeGate {
    async fn allow(&self, s: &Session, _: &str) -> Result<()> {
        if s.safe_mode() != SafeMode::Silent {
            bail!(
                "connection {:?} is in {:?} safe mode, so tools may only read. Ask the user to switch it to Silent mode to allow changes.",
                s.config.display_name(),
                s.safe_mode()
            );
        }
        Ok(())
    }
}

/// What the app's AI settings let agents change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePolicy {
    /// Every change waits for the user's approval.
    #[default]
    Ask,
    /// Changes run without asking, except on connections in a safe mode.
    Allow,
    /// Agents may only read.
    ReadOnly,
}

/// A change waiting for the user's decision; send `true` to allow it.
/// Dropping `reply` refuses.
pub struct Approval {
    pub connection: String,
    pub sql: String,
    pub reply: oneshot::Sender<bool>,
}

/// The app's gate: applies the [`WritePolicy`] and sends the changes that
/// need approval to the UI.
pub struct ApprovalGate {
    policy: Mutex<WritePolicy>,
    requests: mpsc::UnboundedSender<Approval>,
}

impl ApprovalGate {
    pub fn new(policy: WritePolicy) -> (Arc<Self>, mpsc::UnboundedReceiver<Approval>) {
        let (requests, rx) = mpsc::unbounded_channel();
        (Arc::new(Self { policy: Mutex::new(policy), requests }), rx)
    }

    pub fn set_policy(&self, policy: WritePolicy) {
        *self.policy.lock().unwrap() = policy;
    }
}

#[async_trait::async_trait]
impl WriteGate for ApprovalGate {
    async fn allow(&self, s: &Session, sql: &str) -> Result<()> {
        let policy = *self.policy.lock().unwrap();
        match policy {
            WritePolicy::ReadOnly => bail!("Quarry's AI settings allow reading only, so the change was not made."),
            WritePolicy::Allow if s.safe_mode() == SafeMode::Silent => Ok(()),
            _ => {
                let (reply, answer) = oneshot::channel();
                let request = Approval { connection: s.config.display_name(), sql: sql.to_string(), reply };
                self.requests.send(request).map_err(|_| anyhow!("Quarry could not ask the user to approve the change"))?;
                match answer.await {
                    Ok(true) => Ok(()),
                    _ => bail!("The user declined this change. Do not retry it unless they ask."),
                }
            }
        }
    }
}

pub struct Toolbox {
    store: Mutex<ConnectionStore>,
    sessions: Arc<SessionManager>,
    gate: Arc<dyn WriteGate>,
}

fn conn_arg() -> Json {
    json!({"type": "string", "description": "Connection id or name (see list_connections)."})
}

fn table_args(extra: Json) -> Json {
    let mut props = json!({
        "connection": conn_arg(),
        "table": {"type": "string", "description": "Table or view name."},
        "schema": {"type": "string", "description": "Schema (PostgreSQL). Defaults to the current schema."}
    });
    props.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    props
}

fn object(props: Json, required: &[&str]) -> Json {
    json!({"type": "object", "properties": props, "required": required})
}

fn filters_schema() -> Json {
    json!({
        "type": "array",
        "description": "AND-ed conditions. column may also be \"Any column\" or \"Raw SQL\" (value is then a SQL condition).",
        "items": {"type": "object", "properties": {
            "column": {"type": "string"},
            "op": {"type": "string", "enum": ["=", "<>", "<", ">", "<=", ">=", "IN", "NOT IN", "IS NULL", "IS NOT NULL",
                "BETWEEN", "NOT BETWEEN", "LIKE", "ILIKE", "Contains", "Not contains", "Contains - Case insensitive",
                "Not contains - Case insensitive", "Has prefix", "Has suffix", "Has prefix - Case insensitive",
                "Has suffix - Case insensitive"]},
            "value": {"type": "string", "description": "IN: \"1, 2, 3\"; BETWEEN: \"1 AND 100\"."}
        }, "required": ["column", "op"]}
    })
}

impl Toolbox {
    pub fn new(store: ConnectionStore, sessions: Arc<SessionManager>) -> Self {
        Self { store: Mutex::new(store), sessions, gate: Arc::new(SafeModeGate) }
    }

    pub fn with_gate(mut self, gate: Arc<dyn WriteGate>) -> Self {
        self.gate = gate;
        self
    }

    pub fn tools() -> Vec<Tool> {
        let row_obj = json!({"type": "object", "description": "Column name → value.", "additionalProperties": true});
        vec![
            Tool {
                name: "list_connections",
                description: "List saved database connections (id, name, driver, host, database, tag, safe mode, whether connected).",
                input_schema: object(json!({}), &[]),
            },
            Tool {
                name: "connect",
                description: "Open a saved connection. Returns server version, current database and schema.",
                input_schema: object(json!({"connection": conn_arg()}), &["connection"]),
            },
            Tool {
                name: "disconnect",
                description: "Close an open connection.",
                input_schema: object(json!({"connection": conn_arg()}), &["connection"]),
            },
            Tool {
                name: "save_connection",
                description: "Create or update a saved connection. The password goes to the OS keychain.",
                input_schema: object(
                    json!({
                        "id": {"type": "string", "description": "Existing id to update; omit to create."},
                        "name": {"type": "string"},
                        "driver": {"type": "string", "enum": ["postgres", "mysql", "mariadb", "sqlite"]},
                        "host": {"type": "string"}, "port": {"type": "integer"}, "user": {"type": "string"},
                        "password": {"type": "string"}, "database": {"type": "string"},
                        "file_path": {"type": "string", "description": "SQLite database file."},
                        "safe_mode": {"type": "string", "enum": ["silent", "alert", "alert_except_read", "safe", "safe_except_read"]}
                    }),
                    &["name", "driver"],
                ),
            },
            Tool {
                name: "list_databases",
                description: "List databases on the server and the current one.",
                input_schema: object(json!({"connection": conn_arg()}), &["connection"]),
            },
            Tool {
                name: "switch_database",
                description: "Make another database current for this connection.",
                input_schema: object(json!({"connection": conn_arg(), "database": {"type": "string"}}), &["connection", "database"]),
            },
            Tool {
                name: "list_schemas",
                description: "List schemas of the current database (PostgreSQL; empty for MySQL/SQLite).",
                input_schema: object(json!({"connection": conn_arg()}), &["connection"]),
            },
            Tool {
                name: "list_tables",
                description: "List tables, views, materialized views, functions, procedures and sequences of a schema.",
                input_schema: object(json!({"connection": conn_arg(), "schema": {"type": "string"}}), &["connection"]),
            },
            Tool {
                name: "describe_table",
                description: "Columns (type, nullable, default, primary key, references), indexes and foreign keys of a table.",
                input_schema: object(table_args(json!({})), &["connection", "table"]),
            },
            Tool {
                name: "get_ddl",
                description: "CREATE statement of a table, view, function, procedure or sequence.",
                input_schema: object(
                    table_args(json!({"kind": {"type": "string", "enum": ["table", "view", "materialized_view", "function", "procedure", "sequence"]}})),
                    &["connection", "table"],
                ),
            },
            Tool {
                name: "schema_overview",
                description: "Every table and view of a schema with its columns and keys. Call this first to learn a database.",
                input_schema: object(json!({"connection": conn_arg(), "schema": {"type": "string"}}), &["connection"]),
            },
            Tool {
                name: "select_rows",
                description: "Read rows of a table with filters, sorting and paging, like the data view.",
                input_schema: object(
                    table_args(json!({
                        "filters": filters_schema(),
                        "sort": {"type": "array", "items": {"type": "object", "properties": {
                            "column": {"type": "string"}, "descending": {"type": "boolean"}}, "required": ["column"]}},
                        "columns": {"type": "array", "items": {"type": "string"}},
                        "limit": {"type": "integer", "description": "Default 100, max 1000."},
                        "offset": {"type": "integer"}
                    })),
                    &["connection", "table"],
                ),
            },
            Tool {
                name: "count_rows",
                description: "Count rows of a table, optionally filtered.",
                input_schema: object(table_args(json!({"filters": filters_schema()})), &["connection", "table"]),
            },
            Tool {
                name: "search_data",
                description: "Search a text in every column of every table of a schema. Returns matching rows per table.",
                input_schema: object(
                    json!({
                        "connection": conn_arg(), "term": {"type": "string"}, "schema": {"type": "string"},
                        "max_tables": {"type": "integer", "description": "Default 50."},
                        "rows_per_table": {"type": "integer", "description": "Default 10."}
                    }),
                    &["connection", "term"],
                ),
            },
            Tool {
                name: "explain_query",
                description: "The database's execution plan for one query (EXPLAIN). Use it to find full scans and missing indexes before suggesting optimizations. analyze=true runs the query to report real row counts and timings; it works for read-only queries only.",
                input_schema: object(
                    json!({"connection": conn_arg(), "sql": {"type": "string"}, "analyze": {"type": "boolean"}}),
                    &["connection", "sql"],
                ),
            },
            Tool {
                name: "run_sql",
                description: "Run one or more SQL statements. Statements that change data or structure need permission: they can be refused.",
                input_schema: object(
                    json!({
                        "connection": conn_arg(), "sql": {"type": "string"},
                        "limit": {"type": "integer", "description": "LIMIT added to SELECTs without one. Default 100."}
                    }),
                    &["connection", "sql"],
                ),
            },
            Tool {
                name: "insert_row",
                description: "Insert one row. Set dry_run to get the SQL without running it.",
                input_schema: object(
                    table_args(json!({"values": row_obj, "dry_run": {"type": "boolean"}})),
                    &["connection", "table", "values"],
                ),
            },
            Tool {
                name: "update_row",
                description: "Update one row identified by its primary key. key must contain every primary-key column.",
                input_schema: object(
                    table_args(json!({"key": row_obj, "values": row_obj, "dry_run": {"type": "boolean"}})),
                    &["connection", "table", "key", "values"],
                ),
            },
            Tool {
                name: "alter_table",
                description: "Change a table's structure: add/drop/modify columns, add/drop indexes. Set dry_run to get the ALTER statements without running them.",
                input_schema: object(
                    table_args(json!({
                        "changes": {"type": "array", "items": {"type": "object", "description":
                            "One of: {action:\"add_column\", column:{name,data_type,nullable?,default?,comment?}},                              {action:\"drop_column\", name}, {action:\"modify_column\", old:{...current definition}, new:{...}},                              {action:\"add_index\", name, columns:[..], unique?}, {action:\"drop_index\", name}.                              default is a SQL expression used verbatim, e.g. \"'text'\" or \"now()\"."}},
                        "dry_run": {"type": "boolean"}
                    })),
                    &["connection", "table", "changes"],
                ),
            },
            Tool {
                name: "import_csv",
                description: "Insert CSV rows into a table in one transaction. The header row must name table columns; empty fields become NULL.",
                input_schema: object(
                    table_args(json!({
                        "csv": {"type": "string", "description": "CSV text including the header row."},
                        "path": {"type": "string", "description": "Or a CSV file on this machine."},
                        "delimiter": {"type": "string", "description": "Default \",\"."}
                    })),
                    &["connection", "table"],
                ),
            },
            Tool {
                name: "query_history",
                description: "Recent statements run on a connection, newest first.",
                input_schema: object(json!({"connection": conn_arg(), "limit": {"type": "integer", "description": "Default 50."}}), &["connection"]),
            },
            Tool {
                name: "list_saved_queries",
                description: "Saved queries of a connection (the Queries tab).",
                input_schema: object(json!({"connection": conn_arg()}), &["connection"]),
            },
            Tool {
                name: "save_query",
                description: "Save a named query to a connection's Queries tab.",
                input_schema: object(json!({"connection": conn_arg(), "name": {"type": "string"}, "sql": {"type": "string"}}), &["connection", "name", "sql"]),
            },
            Tool {
                name: "delete_rows",
                description: "Delete rows identified by their primary keys.",
                input_schema: object(
                    table_args(json!({"keys": {"type": "array", "items": row_obj}, "dry_run": {"type": "boolean"}})),
                    &["connection", "table", "keys"],
                ),
            },
        ]
    }

    pub async fn call(&self, name: &str, args: Json) -> Result<Json> {
        let a = Args(args);
        match name {
            "list_connections" => Ok(self.list_connections()),
            "connect" => {
                let s = self.session(&a).await?;
                Ok(json!({"id": s.config.id, "server": s.title(), "database": s.database(), "schema": s.schema()}))
            }
            "disconnect" => {
                let c = self.config(a.str("connection")?)?;
                self.sessions.disconnect(&c.id);
                Ok(json!({"disconnected": c.id}))
            }
            "save_connection" => self.save_connection(&a),
            "list_databases" => {
                let s = self.session(&a).await?;
                Ok(json!({"current": s.database(), "databases": s.list_databases().await?}))
            }
            "switch_database" => {
                let s = self.session(&a).await?;
                s.switch_database(a.str("database")?).await?;
                Ok(json!({"database": s.database(), "schema": s.schema()}))
            }
            "list_schemas" => {
                let s = self.session(&a).await?;
                Ok(json!({"current": s.schema(), "schemas": s.list_schemas().await?}))
            }
            "list_tables" => {
                let s = self.session(&a).await?;
                Ok(serde_json::to_value(s.list_items(a.opt_str("schema")).await?)?)
            }
            "describe_table" => {
                let s = self.session(&a).await?;
                Ok(serde_json::to_value(s.structure(&self.table(&s, &a)?).await?)?)
            }
            "get_ddl" => {
                let s = self.session(&a).await?;
                let kind = match a.opt_str("kind") {
                    Some(k) => serde_json::from_value(json!(k)).context("unknown kind")?,
                    None => ItemKind::Table,
                };
                let item = Item { table: self.table(&s, &a)?, kind, comment: None };
                Ok(json!({"ddl": s.ddl(&item).await?}))
            }
            "schema_overview" => {
                let s = self.session(&a).await?;
                Ok(serde_json::to_value(s.overview(a.opt_str("schema")).await?)?)
            }
            "select_rows" => self.select_rows(&a).await,
            "count_rows" => {
                let s = self.session(&a).await?;
                let filters = a.parse::<Vec<Filter>>("filters")?.unwrap_or_default();
                Ok(json!({"count": s.count(&self.table(&s, &a)?, &filters).await?}))
            }
            "search_data" => {
                let s = self.session(&a).await?;
                let hits = s
                    .search(
                        a.str("term")?,
                        a.opt_str("schema"),
                        a.opt_u64("max_tables").unwrap_or(50) as usize,
                        a.opt_u64("rows_per_table").unwrap_or(10) as u32,
                    )
                    .await?;
                Ok(serde_json::to_value(hits)?)
            }
            "explain_query" => {
                let s = self.session(&a).await?;
                let analyze = a.0.get("analyze").and_then(Json::as_bool).unwrap_or(false);
                let r = s.explain(a.str("sql")?, analyze).await?;
                // PostgreSQL and MySQL return the plan as lines of one column.
                if r.columns.len() == 1 {
                    let lines: Vec<String> = r.rows.iter().map(|row| row[0].display()).collect();
                    return Ok(json!({"plan": lines.join("\n")}));
                }
                Ok(json!({"plan": r.rows_as_objects(MAX_ROWS)}))
            }
            "run_sql" => self.run_sql(&a).await,
            "insert_row" => {
                let values = a.row("values")?;
                self.write(&a, |_| Ok((vec![RowChange::Insert { values }], KeyKind::PrimaryKey))).await
            }
            "update_row" => {
                let (key, values) = (a.row("key")?, a.row("values")?);
                self.write(&a, |pk| {
                    check_key(&key, pk)?;
                    Ok((vec![RowChange::Update { key, values }], KeyKind::PrimaryKey))
                })
                .await
            }
            "delete_rows" => {
                let keys = a
                    .0
                    .get("keys")
                    .and_then(Json::as_array)
                    .ok_or_else(|| anyhow!("keys must be an array of objects"))?
                    .iter()
                    .map(to_row)
                    .collect::<Result<Vec<_>>>()?;
                self.write(&a, |pk| {
                    keys.iter().try_for_each(|k| check_key(k, pk))?;
                    Ok((keys.into_iter().map(|key| RowChange::Delete { key }).collect(), KeyKind::PrimaryKey))
                })
                .await
            }
            "alter_table" => {
                let s = self.session(&a).await?;
                let table = self.table(&s, &a)?;
                let changes: Vec<quarry_core::alter::StructureChange> =
                    a.parse("changes")?.ok_or_else(|| anyhow!("missing array argument \"changes\""))?;
                let statements = s.dialect().structure_sql(&table, &changes)?;
                if a.0.get("dry_run").and_then(Json::as_bool).unwrap_or(false) {
                    return Ok(json!({"sql": statements}));
                }
                self.gate.allow(&s, &statements.join("\n")).await?;
                Ok(json!({"executed": s.apply_structure(&table, &changes).await?}))
            }
            "import_csv" => {
                let s = self.session(&a).await?;
                let table = self.table(&s, &a)?;
                let csv = match (a.opt_str("csv"), a.opt_str("path")) {
                    (Some(text), _) => text.to_string(),
                    (None, Some(path)) => std::fs::read_to_string(path).with_context(|| format!("could not read {path}"))?,
                    (None, None) => bail!("give either csv or path"),
                };
                let rows = csv.lines().count().saturating_sub(1);
                self.gate.allow(&s, &format!("-- insert {rows} CSV rows into {}", table.qualified())).await?;
                let delimiter = a.opt_str("delimiter").and_then(|d| d.chars().next()).unwrap_or(',');
                Ok(json!({"imported": s.import_csv(&table, &csv, delimiter, true).await?}))
            }
            "query_history" => {
                let s = self.session(&a).await?;
                Ok(serde_json::to_value(s.history(a.opt_u64("limit").unwrap_or(50) as usize))?)
            }
            "list_saved_queries" => {
                let c = self.config(a.str("connection")?)?;
                let saved = quarry_core::store::SavedQueries::load()?;
                Ok(serde_json::to_value(saved.for_connection(&c.id))?)
            }
            "save_query" => {
                let c = self.config(a.str("connection")?)?;
                let mut saved = quarry_core::store::SavedQueries::load()?;
                let q = quarry_core::store::SavedQuery {
                    id: ConnectionConfig::default().id,
                    connection_id: c.id,
                    name: a.str("name")?.to_string(),
                    sql: a.str("sql")?.to_string(),
                };
                let id = q.id.clone();
                saved.upsert(q)?;
                Ok(json!({"id": id}))
            }
            other => bail!("unknown tool: {other}"),
        }
    }

    fn list_connections(&self) -> Json {
        let mut store = self.store.lock().unwrap();
        let _ = store.reload();
        let live = self.sessions.all();
        // Open connections first, including ones never saved.
        let unsaved = live.iter().filter(|s| store.find(&s.config.id).is_none()).map(|s| &s.config);
        Json::Array(
            unsaved
                .chain(store.list.connections.iter())
                .map(|c| {
                    let session = live.iter().find(|s| s.config.id == c.id);
                    json!({
                        "id": c.id, "name": c.display_name(), "driver": c.driver, "host": c.host,
                        "database": match session {
                            Some(s) => s.database(),
                            None if c.driver.is_file_based() => c.file_path.clone(),
                            None => c.database.clone(),
                        },
                        "tag": c.tag, "safe_mode": session.map_or(c.safe_mode, |s| s.safe_mode()),
                        "connected": session.is_some()
                    })
                })
                .collect(),
        )
    }

    fn save_connection(&self, a: &Args) -> Result<Json> {
        let mut store = self.store.lock().unwrap();
        let driver = DriverKind::parse(a.str("driver")?).ok_or_else(|| anyhow!("unknown driver"))?;
        let mut c = match a.opt_str("id") {
            Some(id) => store.resolve(id).ok_or_else(|| anyhow!("no connection {id}"))?,
            None => ConnectionConfig::new(driver),
        };
        c.driver = driver;
        c.name = a.str("name")?.to_string();
        let set = |field: &mut String, key: &str| {
            if let Some(v) = a.opt_str(key) {
                *field = v.to_string();
            }
        };
        set(&mut c.host, "host");
        set(&mut c.user, "user");
        set(&mut c.database, "database");
        set(&mut c.file_path, "file_path");
        if let Some(p) = a.opt_u64("port") {
            c.port = Some(u16::try_from(p).context("port out of range")?);
        }
        if let Some(pw) = a.opt_str("password") {
            c.password = Some(pw.to_string());
        }
        if let Some(m) = a.opt_str("safe_mode") {
            c.safe_mode = serde_json::from_value(json!(m)).context("unknown safe_mode")?;
        }
        let id = c.id.clone();
        store.upsert(c)?;
        Ok(json!({"id": id}))
    }

    fn config(&self, id_or_name: &str) -> Result<ConnectionConfig> {
        if let Some(s) = self.live(id_or_name) {
            return Ok(s.config.clone());
        }
        let mut store = self.store.lock().unwrap();
        let _ = store.reload();
        store
            .resolve(id_or_name)
            .ok_or_else(|| anyhow!("no saved connection named or with id {id_or_name:?}; see list_connections"))
    }

    /// An open connection, by id or name.
    fn live(&self, id_or_name: &str) -> Option<Arc<Session>> {
        self.sessions.get(id_or_name).or_else(|| {
            self.sessions.all().into_iter().find(|s| s.config.display_name().eq_ignore_ascii_case(id_or_name))
        })
    }

    async fn session(&self, a: &Args) -> Result<Arc<Session>> {
        let key = a.str("connection")?;
        if let Some(s) = self.live(key) {
            return Ok(s);
        }
        let config = self.config(key)?;
        self.sessions.connect(config).await
    }

    fn table(&self, s: &Session, a: &Args) -> Result<TableRef> {
        Ok(s.table_ref(a.opt_str("schema"), a.str("table")?))
    }

    async fn select_rows(&self, a: &Args) -> Result<Json> {
        let s = self.session(a).await?;
        let limit = a.opt_u64("limit").map_or(DEFAULT_ROWS, |l| l as usize).min(MAX_ROWS);
        let req = BrowseRequest {
            table: self.table(&s, a)?,
            filters: a.parse("filters")?.unwrap_or_default(),
            sort: a.parse::<Vec<Sort>>("sort")?.unwrap_or_default(),
            limit: limit as u32,
            offset: a.opt_u64("offset").unwrap_or(0),
            columns: a.parse("columns")?.unwrap_or_default(),
        };
        let r = s.browse(&req).await?;
        Ok(json!({"sql": r.statement, "row_count": r.rows.len(), "rows": r.rows_as_objects(limit)}))
    }

    async fn run_sql(&self, a: &Args) -> Result<Json> {
        let s = self.session(a).await?;
        let script = a.str("sql")?;
        if !sql::is_read_only(script) {
            self.gate.allow(&s, script).await?;
        }
        let limit = a.opt_u64("limit").map_or(DEFAULT_ROWS, |l| l as usize).min(MAX_ROWS);
        let outcomes = s.run_script(script, Some(limit as u32)).await;
        Ok(Json::Array(
            outcomes
                .into_iter()
                .map(|o| match o.outcome {
                    Outcome::Result(r) if r.has_rows() => json!({
                        "statement": o.statement, "duration_ms": r.duration_ms,
                        "row_count": r.rows.len(), "rows": r.rows_as_objects(limit)
                    }),
                    Outcome::Result(r) => json!({
                        "statement": o.statement, "duration_ms": r.duration_ms, "rows_affected": r.rows_affected
                    }),
                    Outcome::Error(e) => json!({"statement": o.statement, "error": e}),
                })
                .collect(),
        ))
    }

    /// Shared path of the row-editing tools: checks the write policy and the
    /// primary key, then previews (`dry_run`) or commits in one transaction.
    async fn write(
        &self,
        a: &Args,
        build: impl FnOnce(&[String]) -> Result<(Vec<RowChange>, KeyKind)>,
    ) -> Result<Json> {
        let s = self.session(a).await?;
        let table = self.table(&s, a)?;
        let pk = s.structure(&table).await?.primary_key();
        let (changes, key_kind) = build(&pk)?;
        let statements = s.changes_sql(&table, &changes, key_kind);
        if a.0.get("dry_run").and_then(Json::as_bool).unwrap_or(false) {
            return Ok(json!({"sql": statements}));
        }
        self.gate.allow(&s, &statements.join("\n")).await?;
        Ok(json!({"executed": s.apply_changes(&table, &changes, key_kind).await?}))
    }
}

fn check_key(key: &ColumnValues, pk: &[String]) -> Result<()> {
    if pk.is_empty() {
        bail!("the table has no primary key; use run_sql with an explicit WHERE clause");
    }
    if let Some(missing) = pk.iter().find(|c| !key.iter().any(|(k, _)| k == *c)) {
        bail!("key is missing primary-key column {missing:?}");
    }
    Ok(())
}

fn to_row(v: &Json) -> Result<ColumnValues> {
    let obj: &Map<String, Json> = v.as_object().ok_or_else(|| anyhow!("expected an object of column → value"))?;
    Ok(obj.iter().map(|(k, v)| (k.clone(), Value::from_json(v))).collect())
}

/// Typed access to tool arguments with clear errors for the model.
struct Args(Json);

impl Args {
    fn str(&self, key: &str) -> Result<&str> {
        self.opt_str(key).ok_or_else(|| anyhow!("missing string argument {key:?}"))
    }

    fn opt_str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Json::as_str).filter(|s| !s.is_empty())
    }

    fn opt_u64(&self, key: &str) -> Option<u64> {
        self.0.get(key).and_then(Json::as_u64)
    }

    fn row(&self, key: &str) -> Result<ColumnValues> {
        to_row(self.0.get(key).ok_or_else(|| anyhow!("missing object argument {key:?}"))?)
    }

    fn parse<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.0.get(key) {
            None | Some(Json::Null) => Ok(None),
            Some(v) => serde_json::from_value(v.clone()).map(Some).with_context(|| format!("invalid {key}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tools_drive_a_database() {
        let dir = std::env::temp_dir().join(format!("quarry-ai-{}", uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        // Keep query history out of the real config dir.
        unsafe { std::env::set_var("QUARRY_CONFIG_DIR", &dir) };
        let db = dir.join("shop.db");
        let store = ConnectionStore::load_from(dir.join("connections.json")).unwrap();
        let tb = Toolbox::new(store, Arc::new(SessionManager::default()));

        let id = tb
            .call("save_connection", json!({"name": "Shop", "driver": "sqlite", "file_path": db.to_string_lossy()}))
            .await
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let r = tb
            .call("run_sql", json!({"connection": "shop", "sql":
                "create table items (id integer primary key, name text, price real); \
                 insert into items (name, price) values ('pen', 1.5), ('ink', 3)"}))
            .await
            .unwrap();
        assert_eq!(r[1]["rows_affected"], 2);

        let overview = tb.call("schema_overview", json!({"connection": id})).await.unwrap();
        assert_eq!(overview[0]["columns"][0]["name"], "id");
        assert_eq!(overview[0]["columns"][0]["primary_key"], true);

        let rows = tb
            .call("select_rows", json!({"connection": "Shop", "table": "items",
                "filters": [{"column": "price", "op": ">", "value": "2"}]}))
            .await
            .unwrap();
        assert_eq!(rows["rows"][0]["name"], "ink");

        let preview = tb
            .call("update_row", json!({"connection": "Shop", "table": "items", "key": {"id": 1}, "values": {"price": 2}, "dry_run": true}))
            .await
            .unwrap();
        assert_eq!(preview["sql"][0], "UPDATE \"items\" SET \"price\" = 2 WHERE \"id\" = 1;");
        tb.call("update_row", json!({"connection": "Shop", "table": "items", "key": {"id": 1}, "values": {"price": 2}}))
            .await
            .unwrap();
        assert!(tb.call("update_row", json!({"connection": "Shop", "table": "items", "key": {"name": "pen"}, "values": {}})).await.is_err());

        let alter = json!({"connection": "Shop", "table": "items", "changes": [
            {"action": "add_column", "column": {"name": "sku", "data_type": "text"}},
            {"action": "add_index", "name": "items_sku", "columns": ["sku"], "unique": true}
        ]});
        let mut dry = alter.clone();
        dry["dry_run"] = json!(true);
        assert_eq!(tb.call("alter_table", dry).await.unwrap()["sql"][0], "ALTER TABLE \"items\" ADD COLUMN \"sku\" text NULL;");
        tb.call("alter_table", alter).await.unwrap();
        let d = tb.call("describe_table", json!({"connection": "Shop", "table": "items"})).await.unwrap();
        assert_eq!(d["columns"][3]["name"], "sku");
        assert_eq!(d["indexes"][0]["name"], "items_sku");
        tb.call("save_query", json!({"connection": "Shop", "name": "cheap", "sql": "select * from items where price < 2"})).await.unwrap();
        assert_eq!(tb.call("list_saved_queries", json!({"connection": "Shop"})).await.unwrap()[0]["name"], "cheap");
        assert!(tb.call("query_history", json!({"connection": "Shop"})).await.unwrap().as_array().unwrap().len() > 2);

        let n = tb.call("import_csv", json!({"connection": "Shop", "table": "items", "csv": "name,price
book,12
\"glue, strong\",4"})).await.unwrap();
        assert_eq!(n["imported"], 2);
        assert_eq!(tb.call("count_rows", json!({"connection": "Shop", "table": "items"})).await.unwrap()["count"], 4);

        let hits = tb.call("search_data", json!({"connection": "Shop", "term": "PEN"})).await.unwrap();
        assert_eq!(hits[0]["rows"][0][2], 2.0);

        // Safe mode makes the connection read-only for tools.
        tb.call("save_connection", json!({"id": id, "name": "Shop", "driver": "sqlite", "safe_mode": "alert"})).await.unwrap();
        tb.call("disconnect", json!({"connection": "Shop"})).await.unwrap();
        assert!(tb.call("run_sql", json!({"connection": "Shop", "sql": "delete from items"})).await.is_err());
        assert!(tb.call("run_sql", json!({"connection": "Shop", "sql": "select * from items"})).await.is_ok());
        assert!(tb.call("delete_rows", json!({"connection": "Shop", "table": "items", "keys": [{"id": 1}]})).await.is_err());

        // The app serves its toolbox over the bridge, and its gate asks the user.
        let (gate, mut asks) = ApprovalGate::new(WritePolicy::Ask);
        let store = ConnectionStore::load_from(dir.join("connections.json")).unwrap();
        let app = Arc::new(Toolbox::new(store, Arc::new(SessionManager::default())).with_gate(gate.clone()));
        bridge::serve(app).await.unwrap();
        let mut client = bridge::BridgeClient::connect(&bridge::info_path()).await.unwrap();
        let answers = tokio::spawn(async move {
            let mut asked = vec![];
            for answer in [false, true] {
                let a = asks.recv().await.unwrap();
                asked.push(a.sql.clone());
                a.reply.send(answer).unwrap();
            }
            asked
        });
        let delete = json!({"connection": "Shop", "sql": "delete from items where id = 2"});
        let refused = client.call("run_sql", delete.clone()).await.unwrap_err();
        assert!(refused.to_string().contains("declined"), "{refused}");
        assert_eq!(client.call("run_sql", delete.clone()).await.unwrap()[0]["rows_affected"], 1);
        assert_eq!(answers.await.unwrap(), ["delete from items where id = 2"; 2]);
        gate.set_policy(WritePolicy::ReadOnly);
        assert!(client.call("run_sql", delete).await.is_err());
        assert!(client.call("disconnect", json!({"connection": "Shop"})).await.is_err());
        let plan = client.call("explain_query", json!({"connection": "Shop", "sql": "select * from items where price > 1"})).await.unwrap();
        assert!(plan["plan"].as_array().is_some_and(|p| !p.is_empty()), "{plan}");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn uuid() -> String {
        format!("{:?}", std::time::SystemTime::now()).replace(|c: char| !c.is_ascii_alphanumeric(), "")
    }
}
