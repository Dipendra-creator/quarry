//! Backup and restore. Like the original (which ships dump/restore
//! binaries), server engines use the vendor tools — found on PATH here — and
//! SQLite is handled in-process.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use gpui_kit::assets::IconName;
use gpui_kit::*;
use quarry_core::{ConnectionConfig, DriverKind, Session};

use crate::picker::{self, PickerItem};
use crate::prompt;
use crate::state::{AppState, db};

fn pick_connection(title: &'static str, window: &mut Window, cx: &mut App, then: impl Fn(ConnectionConfig, &mut Window, &mut App) + 'static) {
    let conns: Vec<ConnectionConfig> = AppState::get(cx).store.list.connections.clone();
    if conns.is_empty() {
        prompt::info(title, "Create a connection first.", window, cx);
        return;
    }
    let items = conns
        .iter()
        .map(|c| PickerItem { label: c.display_name(), detail: c.driver.label().into(), icon: IconName::Database })
        .collect();
    picker::open(title, "Search for connection...", items, window, cx, move |ix, window, cx| {
        if let Some(c) = AppState::get(cx).store.resolve(&conns[ix].id) {
            then(c, window, cx);
        }
    });
}

pub fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(Into::into).unwrap_or_else(|| ".".into())
}

pub fn open_backup(window: &mut Window, cx: &mut App) {
    pick_connection("Backup database", window, cx, |config, window, cx| {
        let name = if config.driver.is_file_based() { "backup.db".to_string() } else { format!("{}.sql", config.database) };
        let rx = cx.prompt_for_new_path(&home(), Some(&name));
        window
            .spawn(cx, async move |cx| {
                let Ok(Ok(Some(path))) = rx.await else { return };
                let r = db(async move { tokio_blocking(move || backup(&config, &path)).await }).await;
                let _ = cx.update(|window, cx| match r {
                    Ok(msg) => prompt::info("Backup finished", &msg, window, cx),
                    Err(e) => prompt::error("Backup failed", &format!("{e:#}"), window, cx),
                });
            })
            .detach();
    });
}

pub fn open_restore(window: &mut Window, cx: &mut App) {
    pick_connection("Restore database", window, cx, |config, window, cx| {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        window
            .spawn(cx, async move |cx| {
                let Ok(Ok(Some(paths))) = rx.await else { return };
                let Some(path) = paths.into_iter().next() else { return };
                let r = db(async move { restore(config, path).await }).await;
                let _ = cx.update(|window, cx| match r {
                    Ok(msg) => prompt::info("Restore finished", &msg, window, cx),
                    Err(e) => prompt::error("Restore failed", &format!("{e:#}"), window, cx),
                });
            })
            .detach();
    });
}

async fn tokio_blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await?
}

/// A database client tool on the user's PATH (the login shell's, which
/// includes Homebrew and the like even when Quarry was opened from Finder).
fn tool(name: &str) -> Result<PathBuf> {
    quarry_core::shell_env::find_program(name)
        .with_context(|| format!("{name} was not found on PATH. Install the database client tools to use backup and restore."))
}

/// A mongodump/mongorestore config file holding the connection string, so
/// the password stays out of the process list. Readable by the user only.
fn mongo_tool_config(c: &ConnectionConfig) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("quarry-mongo-{}.yaml", uuid_like()));
    let uri = quarry_core::driver::mongo_connection_string(c);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    use std::io::Write as _;
    options.open(&path)?.write_all(format!("uri: {}\n", serde_json::to_string(&uri)?).as_bytes())?;
    Ok(path)
}

fn uuid_like() -> String {
    format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0))
}

fn run(mut cmd: Command) -> Result<()> {
    let out = cmd.env("PATH", quarry_core::shell_env::user_path()).output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn backup(c: &ConnectionConfig, path: &std::path::Path) -> Result<String> {
    let port = c.port_or_default().to_string();
    let pw = c.password.clone().unwrap_or_default();
    match c.driver {
        DriverKind::Sqlite => {
            std::fs::copy(&c.file_path, path)?;
        }
        DriverKind::CockroachDb => bail!(
            "CockroachDB backs up with its own BACKUP statement, to cloud storage or a node's disk. Run, for example, \
             BACKUP DATABASE {} INTO 'nodelocal://1/backups' in a SQL editor.",
            if c.database.is_empty() { "defaultdb" } else { &c.database }
        ),
        DriverKind::Postgres | DriverKind::YugabyteDb => {
            if c.database.is_empty() {
                bail!("set a database in the connection to back it up");
            }
            // YugabyteDB ships its own pg_dump as ysql_dump.
            let dump = match c.driver {
                DriverKind::YugabyteDb => tool("ysql_dump").or_else(|_| tool("pg_dump"))?,
                _ => tool("pg_dump")?,
            };
            let mut cmd = Command::new(dump);
            cmd.args(["-h", &c.host, "-p", &port, "-U", &c.user, "-d", &c.database, "-f"]).arg(path).env("PGPASSWORD", pw);
            run(cmd)?;
        }
        DriverKind::MySql | DriverKind::MariaDb | DriverKind::TiDb => {
            if c.database.is_empty() {
                bail!("set a database in the connection to back it up");
            }
            let mut cmd = Command::new(tool("mysqldump")?);
            cmd.args(["-h", &c.host, "-P", &port, "-u", &c.user]);
            if c.driver == DriverKind::TiDb {
                // TiDB has no LOCK TABLES for dumps; a snapshot read is consistent.
                cmd.args(["--single-transaction", "--skip-lock-tables", "--column-statistics=0"]);
            }
            cmd.arg("--result-file").arg(path).arg(&c.database).env("MYSQL_PWD", pw);
            run(cmd)?;
        }
        DriverKind::Redis => {
            // An RDB snapshot of the whole server, downloaded over the connection.
            let mut cmd = Command::new(tool("redis-cli")?);
            cmd.args(["-h", &c.host, "-p", &port]);
            if !c.user.trim().is_empty() {
                cmd.args(["--user", c.user.trim()]);
            }
            if matches!(c.ssl_mode, quarry_core::SslMode::Require | quarry_core::SslMode::VerifyCa | quarry_core::SslMode::VerifyFull) {
                cmd.arg("--tls");
                if c.ssl_mode == quarry_core::SslMode::Require {
                    cmd.arg("--insecure");
                }
            }
            cmd.arg("--rdb").arg(path).env("REDISCLI_AUTH", pw);
            run(cmd)?;
        }
        DriverKind::MongoDb => {
            let db = if c.database.trim().is_empty() { None } else { Some(c.database.trim().to_string()) };
            let mut cmd = Command::new(tool("mongodump")?);
            let config = mongo_tool_config(c)?;
            cmd.arg(format!("--config={}", config.display())).arg(format!("--archive={}", path.display())).arg("--gzip");
            if let Some(db) = &db {
                cmd.arg(format!("--db={db}"));
            }
            let r = run(cmd);
            let _ = std::fs::remove_file(&config);
            r?;
        }
        DriverKind::SqlServer => bail!(
            "SQL Server writes backups on the server itself. Run BACKUP DATABASE [name] TO DISK = N'/path/on/server/name.bak' \
             in a SQL editor, or use SQL Server Management Studio or Azure Data Studio."
        ),
    }
    Ok(format!("Saved to {}", path.display()))
}

async fn restore(c: ConnectionConfig, path: PathBuf) -> Result<String> {
    let port = c.port_or_default().to_string();
    let pw = c.password.clone().unwrap_or_default();
    match c.driver {
        DriverKind::Sqlite | DriverKind::Redis => {
            let what = if c.driver == DriverKind::Redis { "Redis restore expects a text file of commands, one per line" } else { "SQLite restore expects a .sql script" };
            let script = std::fs::read_to_string(&path).context(what)?;
            let session = Session::connect(c).await?;
            // Sessions open read-only; restoring is the write the user asked for.
            session.set_read_only(false).await?;
            let out = session.run_script(&script, None).await;
            if let Some(quarry_core::Outcome::Error(e)) = out.last().map(|o| &o.outcome) {
                bail!("{e}");
            }
            Ok(format!("Ran {} statements", out.len()))
        }
        DriverKind::Postgres | DriverKind::CockroachDb | DriverKind::YugabyteDb => tokio_blocking(move || {
            let mut cmd = Command::new(tool("psql")?);
            cmd.args(["-h", &c.host, "-p", &port, "-U", &c.user, "-d", &c.database, "-v", "ON_ERROR_STOP=1", "-f"]).arg(&path).env("PGPASSWORD", pw);
            run(cmd)?;
            Ok(format!("Restored {}", path.display()))
        })
        .await,
        DriverKind::MySql | DriverKind::MariaDb | DriverKind::TiDb => tokio_blocking(move || {
            let mut cmd = Command::new(tool("mysql")?);
            cmd.args(["-h", &c.host, "-P", &port, "-u", &c.user, &c.database]).env("MYSQL_PWD", pw).stdin(std::fs::File::open(&path)?);
            run(cmd)?;
            Ok(format!("Restored {}", path.display()))
        })
        .await,
        // A mongodump archive (gzipped, as Quarry writes them).
        DriverKind::MongoDb => tokio_blocking(move || {
            let mut cmd = Command::new(tool("mongorestore")?);
            let config = mongo_tool_config(&c)?;
            cmd.arg(format!("--config={}", config.display())).arg(format!("--archive={}", path.display())).arg("--gzip");
            let r = run(cmd);
            let _ = std::fs::remove_file(&config);
            r?;
            Ok(format!("Restored {}", path.display()))
        })
        .await,
        // A .sql script (with GO batches) through sqlcmd; .bak files are restored by the server.
        DriverKind::SqlServer => tokio_blocking(move || {
            let mut cmd = Command::new(tool("sqlcmd")?);
            cmd.args(["-S", &format!("{},{port}", c.host), "-U", &c.user, "-b", "-C", "-i"]).arg(&path).env("SQLCMDPASSWORD", pw);
            if !c.database.is_empty() {
                cmd.args(["-d", &c.database]);
            }
            run(cmd)?;
            Ok(format!("Restored {}", path.display()))
        })
        .await,
    }
}
