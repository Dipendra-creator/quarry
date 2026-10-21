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
}

impl DriverKind {
    pub const ALL: [DriverKind; 4] = [
        DriverKind::Postgres,
        DriverKind::MySql,
        DriverKind::MariaDb,
        DriverKind::Sqlite,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DriverKind::Postgres => "PostgreSQL",
            DriverKind::MySql => "MySQL",
            DriverKind::MariaDb => "MariaDB",
            DriverKind::Sqlite => "SQLite",
        }
    }

    /// Two-letter badge shown where TablePlus shows the vendor logo.
    pub fn badge(self) -> &'static str {
        match self {
            DriverKind::Postgres => "Pg",
            DriverKind::MySql => "My",
            DriverKind::MariaDb => "Ma",
            DriverKind::Sqlite => "Sl",
        }
    }

    pub fn default_port(self) -> Option<u16> {
        match self {
            DriverKind::Postgres => Some(5432),
            DriverKind::MySql | DriverKind::MariaDb => Some(3306),
            DriverKind::Sqlite => None,
        }
    }

    pub fn default_user(self) -> &'static str {
        match self {
            DriverKind::Postgres => "postgres",
            DriverKind::MySql | DriverKind::MariaDb => "root",
            DriverKind::Sqlite => "",
        }
    }

    pub fn is_file_based(self) -> bool {
        matches!(self, DriverKind::Sqlite)
    }

    /// Whether the server has schemas inside a database (PostgreSQL) or the
    /// database itself is the namespace (MySQL, SQLite).
    pub fn has_schemas(self) -> bool {
        matches!(self, DriverKind::Postgres)
    }

    pub fn parse(s: &str) -> Option<DriverKind> {
        let s = s.to_ascii_lowercase();
        DriverKind::ALL
            .into_iter()
            .find(|d| d.label().to_ascii_lowercase() == s || format!("{d:?}").to_ascii_lowercase() == s)
            .or(match s.as_str() {
                "pg" | "postgresql" => Some(DriverKind::Postgres),
                "sqlite3" => Some(DriverKind::Sqlite),
                _ => None,
            })
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

    /// What the UI must do before running `sql`.
    pub fn check(self, sql: &str) -> Confirmation {
        let read = crate::sql::is_read_only(sql);
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
