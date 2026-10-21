//! Quarry core: everything a database client does, without any UI.
//!
//! The GPUI app and the AI tool layer (`quarry-ai`) are both thin clients of
//! [`session::Session`], so every feature is reachable programmatically.

pub mod alter;
pub mod changes;
pub mod driver;
pub mod export;
pub mod import;
pub mod model;
pub mod query;
pub mod schema;
pub mod session;
pub mod sql;
pub mod ssh;
pub mod store;
pub mod value;

pub use changes::{KeyKind, RowChange};
pub use model::{ConnTag, ConnectionConfig, ConnectionGroup, DriverKind, SafeMode, SslMode};
pub use query::{BrowseRequest, Filter, FilterOp, QueryResult, ResultColumn, Sort};
pub use schema::{ColumnInfo, ForeignKeyInfo, IndexInfo, Item, ItemKind, TableRef, TableStructure};
pub use session::{Outcome, Session, SessionManager, StatementOutcome};
pub use value::Value;

/// Shared Tokio runtime for database I/O, for hosts (like the GPUI app)
/// that do not run on Tokio themselves.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("quarry-db")
            .build()
            .expect("failed to start the database runtime")
    })
}
