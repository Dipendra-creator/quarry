//! Connection configuration: what the connection list, the connection form and
//! the AI tools all read and write.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DriverKind {
    Postgres,
    MySql,
    MariaDb,
    Sqlite,
    /// Microsoft SQL Server (and Azure SQL), over TDS.
    SqlServer,
    /// Distributed SQL that speaks PostgreSQL's protocol and most of its SQL.
    CockroachDb,
    /// YugabyteDB's YSQL API, built on PostgreSQL's own code.
    YugabyteDb,
    /// PingCAP's distributed SQL that speaks MySQL's protocol.
    TiDb,
    /// Redis and servers speaking its protocol (Valkey, KeyDB, Dragonfly).
    Redis,
    /// MongoDB and services speaking its protocol (Atlas, DocumentDB).
    MongoDb,
}

impl DriverKind {
    pub const ALL: [DriverKind; 10] = [
        DriverKind::Postgres,
        DriverKind::MySql,
        DriverKind::MariaDb,
        DriverKind::SqlServer,
        DriverKind::Sqlite,
        DriverKind::CockroachDb,
        DriverKind::YugabyteDb,
        DriverKind::TiDb,
        DriverKind::Redis,
        DriverKind::MongoDb,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DriverKind::Postgres => "PostgreSQL",
            DriverKind::MySql => "MySQL",
            DriverKind::MariaDb => "MariaDB",
            DriverKind::Sqlite => "SQLite",
            DriverKind::SqlServer => "SQL Server",
            DriverKind::CockroachDb => "CockroachDB",
            DriverKind::YugabyteDb => "YugabyteDB",
            DriverKind::TiDb => "TiDB",
            DriverKind::Redis => "Redis",
            DriverKind::MongoDb => "MongoDB",
        }
    }

    /// Other names people search for in the driver picker.
    pub fn keywords(self) -> &'static [&'static str] {
        match self {
            DriverKind::Postgres => &["postgres", "pg"],
            DriverKind::MySql => &["mysql"],
            DriverKind::MariaDb => &["mariadb"],
            DriverKind::Sqlite => &["sqlite"],
            DriverKind::SqlServer => &["mssql", "microsoft", "ms sql", "azure", "tsql", "t-sql"],
            DriverKind::CockroachDb => &["cockroach", "crdb", "postgres compatible", "distributed"],
            DriverKind::YugabyteDb => &["yugabyte", "ysql", "postgres compatible", "distributed"],
            DriverKind::TiDb => &["tidb", "pingcap", "mysql compatible", "distributed"],
            DriverKind::Redis => &["redis", "valkey", "keydb", "dragonfly", "cache", "key value", "nosql"],
            DriverKind::MongoDb => &["mongo", "mongodb", "atlas", "documentdb", "document", "nosql", "json"],
        }
    }

    /// Two-letter badge shown where TablePlus shows the vendor logo.
    pub fn badge(self) -> &'static str {
        match self {
            DriverKind::Postgres => "Pg",
            DriverKind::MySql => "My",
            DriverKind::MariaDb => "Ma",
            DriverKind::Sqlite => "Sl",
            DriverKind::SqlServer => "Ms",
            DriverKind::CockroachDb => "Cr",
            DriverKind::YugabyteDb => "Yb",
            DriverKind::TiDb => "Ti",
            DriverKind::Redis => "Rd",
            DriverKind::MongoDb => "Mg",
        }
    }

    /// The engine whose protocol, driver and SQL dialect this one uses.
    /// CockroachDB and YugabyteDB speak PostgreSQL; TiDB speaks MySQL.
    pub fn base(self) -> DriverKind {
        match self {
            DriverKind::CockroachDb | DriverKind::YugabyteDb => DriverKind::Postgres,
            DriverKind::TiDb => DriverKind::MySql,
            other => other,
        }
    }

    /// Whether its language is SQL. Redis has its own commands, so SQL
    /// features (structure editing, raw SQL filters, plans) do not apply.
    pub fn is_sql(self) -> bool {
        !matches!(self, DriverKind::Redis | DriverKind::MongoDb)
    }

    /// PostgreSQL or a database that speaks its protocol and dialect.
    pub fn is_postgres(self) -> bool {
        self.base() == DriverKind::Postgres
    }

    pub fn default_port(self) -> Option<u16> {
        match self {
            DriverKind::Postgres => Some(5432),
            DriverKind::MySql | DriverKind::MariaDb => Some(3306),
            DriverKind::SqlServer => Some(1433),
            DriverKind::Sqlite => None,
            DriverKind::CockroachDb => Some(26257),
            DriverKind::YugabyteDb => Some(5433),
            DriverKind::TiDb => Some(4000),
            DriverKind::Redis => Some(6379),
            DriverKind::MongoDb => Some(27017),
        }
    }

    pub fn default_user(self) -> &'static str {
        match self {
            DriverKind::Postgres => "postgres",
            DriverKind::MySql | DriverKind::MariaDb | DriverKind::TiDb | DriverKind::CockroachDb => "root",
            DriverKind::SqlServer => "sa",
            DriverKind::Sqlite | DriverKind::Redis | DriverKind::MongoDb => "",
            DriverKind::YugabyteDb => "yugabyte",
        }
    }

    /// Database new connections open when none is set: the one each server
    /// creates for you.
    pub fn default_database(self) -> &'static str {
        match self {
            DriverKind::CockroachDb => "defaultdb",
            DriverKind::YugabyteDb => "yugabyte",
            _ => "",
        }
    }

    /// Whether a failed batch of schema changes is undone as a whole. MySQL
    /// and its relatives commit each one at once; CockroachDB and YugabyteDB
    /// keep the ones that ran before an error.
    pub fn transactional_ddl(self) -> bool {
        matches!(self, DriverKind::Postgres | DriverKind::Sqlite | DriverKind::SqlServer)
    }

    pub fn is_file_based(self) -> bool {
        matches!(self, DriverKind::Sqlite)
    }

    /// Whether the server has schemas inside a database (PostgreSQL, SQL
    /// Server) or the database itself is the namespace (MySQL, SQLite).
    pub fn has_schemas(self) -> bool {
        matches!(self.base(), DriverKind::Postgres | DriverKind::SqlServer)
    }

    /// MySQL, MariaDB and TiDB share a driver and a dialect.
    pub fn is_mysql(self) -> bool {
        matches!(self.base(), DriverKind::MySql | DriverKind::MariaDb)
    }

    /// Schema new connections start in when the server does not say.
    pub fn default_schema(self) -> Option<&'static str> {
        match self.base() {
            DriverKind::Postgres => Some("public"),
            DriverKind::SqlServer => Some("dbo"),
            _ => None,
        }
    }

    pub fn parse(s: &str) -> Option<DriverKind> {
        let s = s.to_ascii_lowercase();
        DriverKind::ALL
            .into_iter()
            .find(|d| d.label().to_ascii_lowercase() == s || format!("{d:?}").to_ascii_lowercase() == s)
            .or(match s.as_str() {
                "pg" | "postgresql" => Some(DriverKind::Postgres),
                "sqlite3" => Some(DriverKind::Sqlite),
                "mssql" | "sql_server" | "sql-server" | "microsoft sql server" | "azure sql" => Some(DriverKind::SqlServer),
                "cockroach" | "crdb" => Some(DriverKind::CockroachDb),
                "yugabyte" | "ysql" => Some(DriverKind::YugabyteDb),
                "valkey" | "keydb" | "dragonfly" => Some(DriverKind::Redis),
                "mongo" => Some(DriverKind::MongoDb),
                _ => None,
            })
    }
}

/// A hosted service or extension of a database, with the connection
/// defaults it needs: Supabase is PostgreSQL that wants SSL, and so on.
/// Connections made from one are plain connections of `driver`.
pub struct Preset {
    pub label: &'static str,
    pub driver: DriverKind,
    pub badge: &'static str,
    /// Badge color, 0xRRGGBB.
    pub color: u32,
    pub keywords: &'static [&'static str],
    pub port: Option<u16>,
    pub user: &'static str,
    pub database: &'static str,
    pub ssl_mode: SslMode,
    /// What to type in Host.
    pub host_hint: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        label: "Supabase",
        driver: DriverKind::Postgres,
        badge: "Sb",
        color: 0x3ECF8E,
        keywords: &["supabase", "postgres", "hosted", "cloud"],
        port: Some(5432),
        user: "postgres",
        database: "postgres",
        ssl_mode: SslMode::Require,
        host_hint: "db.<project>.supabase.co, or the pooler host",
    },
    Preset {
        label: "Neon",
        driver: DriverKind::Postgres,
        badge: "Ne",
        color: 0x00B386,
        keywords: &["neon", "postgres", "serverless", "hosted", "cloud"],
        port: Some(5432),
        user: "",
        database: "neondb",
        ssl_mode: SslMode::Require,
        host_hint: "ep-….neon.tech",
    },
    Preset {
        label: "TimescaleDB",
        driver: DriverKind::Postgres,
        badge: "Ts",
        color: 0xF5A623,
        keywords: &["timescale", "tiger", "time series", "postgres"],
        port: Some(5432),
        user: "postgres",
        database: "",
        ssl_mode: SslMode::Prefer,
        host_hint: "127.0.0.1, or ….tsdb.cloud.timescale.com",
    },
    Preset {
        label: "Amazon RDS / Aurora",
        driver: DriverKind::Postgres,
        badge: "Aw",
        color: 0xFF9900,
        keywords: &["aws", "amazon", "rds", "aurora", "postgres", "cloud"],
        port: Some(5432),
        user: "postgres",
        database: "postgres",
        ssl_mode: SslMode::Require,
        host_hint: "….rds.amazonaws.com",
    },
    Preset {
        label: "PlanetScale",
        driver: DriverKind::MySql,
        badge: "Ps",
        color: 0x111111,
        keywords: &["planetscale", "vitess", "mysql", "hosted", "cloud"],
        port: Some(3306),
        user: "",
        database: "",
        ssl_mode: SslMode::VerifyFull,
        host_hint: "aws.connect.psdb.cloud",
    },
];

impl Preset {
    /// A new connection with this preset's defaults.
    pub fn config(&self) -> ConnectionConfig {
        ConnectionConfig {
            // Empty, so the form shows where to find the host.
            host: String::new(),
            port: self.port.or(self.driver.default_port()),
            user: self.user.into(),
            database: self.database.into(),
            ssl_mode: self.ssl_mode,
            ..ConnectionConfig::new(self.driver)
        }
    }
}

/// The safe-mode levels of the original app. Every level except `Silent`
/// asks the user before sending queries; AI tools treat them as read-only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeMode {
    #[default]
    Silent,
    /// Warn before sending any query.
    Alert,
    /// Warn before sending queries except SELECT/EXPLAIN/SHOW.
    AlertExceptRead,
    /// Require typing the connection name before sending any query.
    Safe,
    /// Require typing the connection name except for SELECT/EXPLAIN/SHOW.
    SafeExceptRead,
}

impl SafeMode {
    pub const ALL: [SafeMode; 5] = [
        SafeMode::Silent,
        SafeMode::Alert,
        SafeMode::AlertExceptRead,
        SafeMode::Safe,
        SafeMode::SafeExceptRead,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SafeMode::Silent => "Silent Mode - Send queries to the server without any warnings",
            SafeMode::Alert => "Alert Mode 1 - Warn before sending queries to the server",
            SafeMode::AlertExceptRead => {
                "Alert Mode 2 - Warn before sending queries to the server except SELECT/EXPLAIN/SHOW queries"
            }
            SafeMode::Safe => "Safe Mode 1 - Confirm with the connection name before sending queries to the server",
            SafeMode::SafeExceptRead => {
                "Safe Mode 2 - Confirm with the connection name before sending queries except SELECT/EXPLAIN/SHOW queries"
            }
        }
    }

    /// What the UI must do before running `sql`, written in `kind`'s language.
    pub fn check(self, kind: DriverKind, sql: &str) -> Confirmation {
        let read = crate::sql::is_read_only_for(kind, sql);
        match self {
            SafeMode::Silent => Confirmation::None,
            SafeMode::AlertExceptRead if read => Confirmation::None,
            SafeMode::SafeExceptRead if read => Confirmation::None,
            SafeMode::Alert | SafeMode::AlertExceptRead => Confirmation::Warn,
            SafeMode::Safe | SafeMode::SafeExceptRead => Confirmation::TypeName,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    None,
    Warn,
    TypeName,
}

/// Environment tag shown next to the connection name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnTag {
    #[default]
    None,
    Local,
    Development,
    Testing,
    Staging,
    Production,
}

impl ConnTag {
    pub const ALL: [ConnTag; 6] = [
        ConnTag::None,
        ConnTag::Local,
        ConnTag::Development,
        ConnTag::Testing,
        ConnTag::Staging,
        ConnTag::Production,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ConnTag::None => "none",
            ConnTag::Local => "local",
            ConnTag::Development => "development",
            ConnTag::Testing => "testing",
            ConnTag::Staging => "staging",
            ConnTag::Production => "production",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl SslMode {
    pub const ALL: [SslMode; 5] = [
        SslMode::Disable,
        SslMode::Prefer,
        SslMode::Require,
        SslMode::VerifyCa,
        SslMode::VerifyFull,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SslMode::Disable => "DISABLE",
            SslMode::Prefer => "PREFERRED",
            SslMode::Require => "REQUIRED",
            SslMode::VerifyCa => "VERIFY-CA",
            SslMode::VerifyFull => "VERIFY-IDENTITY",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionConfig {
    pub id: String,
    pub name: String,
    pub driver: DriverKind,
    pub group_id: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    /// Kept in the OS keychain, never written to the connections file.
    #[serde(skip_serializing)]
    pub password: Option<String>,
    pub save_password: bool,
    pub database: String,
    /// SQLite database file.
    pub file_path: String,
    pub ssl_mode: SslMode,
    /// 0 = none, 1..=5 = the status colors of the title bar.
    pub status_color: u8,
    pub tag: ConnTag,
    pub safe_mode: SafeMode,
    /// SQL run right after connecting ("Bootstrap commands").
    pub startup_commands: String,
    pub show_system_schemas: bool,
    /// Rows per page in the data view.
    pub page_size: u32,
    /// "Over SSH": reach the database through this SSH server.
    pub ssh: Option<crate::ssh::SshConfig>,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            driver: DriverKind::Postgres,
            group_id: None,
            host: "127.0.0.1".into(),
            port: None,
            user: String::new(),
            password: None,
            save_password: true,
            database: String::new(),
            file_path: String::new(),
            ssl_mode: SslMode::default(),
            status_color: 0,
            tag: ConnTag::None,
            safe_mode: SafeMode::Silent,
            startup_commands: String::new(),
            show_system_schemas: false,
            page_size: 300,
            ssh: None,
        }
    }
}

impl ConnectionConfig {
    pub fn new(driver: DriverKind) -> Self {
        Self {
            driver,
            user: driver.default_user().into(),
            port: driver.default_port(),
            database: driver.default_database().into(),
            ..Self::default()
        }
    }

    pub fn port_or_default(&self) -> u16 {
        self.port.or(self.driver.default_port()).unwrap_or(0)
    }

    /// Second line under the connection name in the connection list.
    pub fn detail(&self) -> String {
        if self.driver.is_file_based() {
            return self.file_path.clone();
        }
        let mut s = format!("{}:{}", self.host, self.port_or_default());
        if !self.database.is_empty() {
            s.push_str(" / ");
            s.push_str(&self.database);
        }
        s
    }

    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        if self.driver.is_file_based() {
            return std::path::Path::new(&self.file_path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into());
        }
        self.host.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectionGroup {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub expanded: bool,
}

fn yes() -> bool {
    true
}
