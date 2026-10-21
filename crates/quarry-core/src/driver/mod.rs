//! One trait per database engine. Everything above this layer (sessions, the
//! UI, AI tools) is engine-agnostic.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::model::{ConnectionConfig, DriverKind};
use crate::query::QueryResult;
use crate::schema::{Item, TableRef, TableStructure};

mod mysql;
mod postgres;
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
}

/// Opens a connection. `database` overrides the configured default database.
pub async fn connect(config: &ConnectionConfig, database: Option<&str>) -> Result<Arc<dyn Driver>> {
    Ok(match config.driver {
        DriverKind::Postgres => Arc::new(postgres::PgDriver::connect(config, database).await?),
        DriverKind::MySql | DriverKind::MariaDb => Arc::new(mysql::MyDriver::connect(config, database).await?),
        DriverKind::Sqlite => Arc::new(sqlite::SqliteDriver::open(config)?),
    })
}
