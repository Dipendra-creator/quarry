//! One trait per database engine. Everything above this layer (sessions, the
//! UI, AI tools) is engine-agnostic.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::changes::{KeyKind, RowChange};
use crate::model::{ConnectionConfig, DriverKind};
use crate::query::{BrowseRequest, DocumentPage, DocumentRequest, Filter, QueryResult};
use crate::schema::{Item, TableRef, TableStructure};

mod mongo;
pub use mongo::connection_string as mongo_connection_string;
mod mssql;
mod mysql;
mod postgres;
mod redis;
mod sqlite;

#[async_trait]
pub trait Driver: Send + Sync {
    fn kind(&self) -> DriverKind;
    async fn server_version(&self) -> Result<String>;
    async fn current_database(&self) -> Result<String>;
    async fn list_databases(&self) -> Result<Vec<String>>;
    /// Switches database on the existing connection. Engines that cannot
    /// (PostgreSQL) return an error and the session reconnects instead.
    async fn use_database(&self, name: &str) -> Result<()>;
    async fn list_schemas(&self, include_system: bool) -> Result<Vec<String>>;
    /// The schema unqualified names resolve to right now, on engines with schemas.
    async fn default_schema(&self) -> Result<Option<String>> {
        Ok(None)
    }
    /// Makes unqualified names in SQL resolve to `schema` (PostgreSQL's
    /// `search_path`). Engines without a per-session setting keep their default.
    async fn use_schema(&self, _schema: &str) -> Result<()> {
        Ok(())
    }
    async fn list_items(&self, schema: Option<&str>) -> Result<Vec<Item>>;
    async fn structure(&self, table: &TableRef) -> Result<TableStructure>;
    /// CREATE statement of a table, view, function, … ("DDL" button).
    async fn ddl(&self, item: &Item) -> Result<String>;
    /// `(table, column, type)` for every table and view of a schema, in one
    /// round trip where the engine allows (autocomplete, AI context).
    async fn all_columns(&self, schema: Option<&str>) -> Result<Vec<(String, String, String)>> {
        let mut out = Vec::new();
        for item in self.list_items(schema).await? {
            if item.kind.has_rows() {
                for c in self.structure(&item.table).await?.columns {
                    out.push((item.table.name.clone(), c.name, c.data_type));
                }
            }
        }
        Ok(out)
    }
    /// Runs one statement.
    async fn execute(&self, sql: &str) -> Result<QueryResult>;
    /// Runs statements atomically: all succeed or none are applied.
    async fn execute_batch(&self, statements: &[String]) -> Result<()>;
    /// Makes the connection itself refuse changes (or allow them again),
    /// where the engine has a session setting for it. Quarry also refuses
    /// statements that are not reads before they get here; this catches what
    /// a read can hide, such as a function call that writes.
    async fn set_read_only(&self, _on: bool) -> Result<()> {
        Ok(())
    }
    /// The plan of `sql`, for engines whose plans need session settings
    /// rather than an EXPLAIN prefix. `None`: the session uses EXPLAIN.
    async fn explain(&self, _sql: &str, _analyze: bool) -> Result<Option<QueryResult>> {
        Ok(None)
    }

    // Engines that do not speak SQL (Redis) answer the data view themselves;
    // SQL engines leave these to the SQL generator.

    /// One page of a table-like item, if this engine browses on its own.
    async fn browse(&self, _req: &BrowseRequest) -> Option<Result<QueryResult>> {
        None
    }

    /// Rows the filters keep, if this engine counts on its own.
    async fn count(&self, _table: &TableRef, _filters: &[Filter]) -> Option<Result<u64>> {
        None
    }

    /// Documents (MongoDB) or a key's value (Redis) for the document view.
    async fn documents(&self, _req: &DocumentRequest) -> Option<Result<DocumentPage>> {
        None
    }

    /// Statements that save a document edited as text: `id` is the
    /// document's (from [`DocumentPage`]), `None` for a new one.
    async fn document_save(&self, _table: &TableRef, _id: Option<&str>, _text: &str) -> Option<Result<Vec<String>>> {
        None
    }

    /// Statements that delete a document of the document view.
    fn document_delete(&self, _table: &TableRef, _id: &str) -> Option<Result<Vec<String>>> {
        None
    }

    /// The statement a browse runs, for the filter bar's "open in editor".
    fn browse_text(&self, _req: &BrowseRequest) -> Option<Result<String>> {
        None
    }

    /// Statements in this engine's own language for staged grid edits.
    fn changes_text(&self, _table: &TableRef, _changes: &[RowChange], _key: KeyKind) -> Option<Result<Vec<String>>> {
        None
    }
}

/// Opens a connection. `database` overrides the configured default database.
pub async fn connect(config: &ConnectionConfig, database: Option<&str>) -> Result<Arc<dyn Driver>> {
    Ok(match config.driver {
        DriverKind::Postgres | DriverKind::CockroachDb | DriverKind::YugabyteDb => {
            Arc::new(postgres::PgDriver::connect(config, database).await?)
        }
        DriverKind::MySql | DriverKind::MariaDb | DriverKind::TiDb => Arc::new(mysql::MyDriver::connect(config, database).await?),
        DriverKind::Sqlite => Arc::new(sqlite::SqliteDriver::open(config)?),
        DriverKind::SqlServer => Arc::new(mssql::MsDriver::connect(config, database).await?),
        DriverKind::Redis => Arc::new(redis::RedisDriver::connect(config, database).await?),
        DriverKind::MongoDb => Arc::new(mongo::MongoDriver::connect(config, database).await?),
    })
}
