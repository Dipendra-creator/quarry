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

fn tool(name: &str) -> Result<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
        .with_context(|| format!("{name} was not found on PATH. Install the database client tools to use backup and restore."))
}

fn run(mut cmd: Command) -> Result<()> {
    let out = cmd.output()?;
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
        DriverKind::Postgres => {
            if c.database.is_empty() {
                bail!("set a database in the connection to back it up");
            }
            let mut cmd = Command::new(tool("pg_dump")?);
            cmd.args(["-h", &c.host, "-p", &port, "-U", &c.user, "-d", &c.database, "-f"]).arg(path).env("PGPASSWORD", pw);
            run(cmd)?;
        }
        DriverKind::MySql | DriverKind::MariaDb => {
            if c.database.is_empty() {
                bail!("set a database in the connection to back it up");
            }
            let mut cmd = Command::new(tool("mysqldump")?);
            cmd.args(["-h", &c.host, "-P", &port, "-u", &c.user, "--result-file"]).arg(path).arg(&c.database).env("MYSQL_PWD", pw);
            run(cmd)?;
        }
    }
    Ok(format!("Saved to {}", path.display()))
}

async fn restore(c: ConnectionConfig, path: PathBuf) -> Result<String> {
    let port = c.port_or_default().to_string();
    let pw = c.password.clone().unwrap_or_default();
    match c.driver {
        DriverKind::Sqlite => {
            let script = std::fs::read_to_string(&path).context("SQLite restore expects a .sql script")?;
            let session = Session::connect(c).await?;
            let out = session.run_script(&script, None).await;
            if let Some(quarry_core::Outcome::Error(e)) = out.last().map(|o| &o.outcome) {
                bail!("{e}");
            }
            Ok(format!("Ran {} statements", out.len()))
        }
        DriverKind::Postgres => tokio_blocking(move || {
            let mut cmd = Command::new(tool("psql")?);
            cmd.args(["-h", &c.host, "-p", &port, "-U", &c.user, "-d", &c.database, "-v", "ON_ERROR_STOP=1", "-f"]).arg(&path).env("PGPASSWORD", pw);
            run(cmd)?;
            Ok(format!("Restored {}", path.display()))
        })
        .await,
        DriverKind::MySql | DriverKind::MariaDb => tokio_blocking(move || {
            let mut cmd = Command::new(tool("mysql")?);
            cmd.args(["-h", &c.host, "-P", &port, "-u", &c.user, &c.database]).env("MYSQL_PWD", pw).stdin(std::fs::File::open(&path)?);
            run(cmd)?;
            Ok(format!("Restored {}", path.display()))
        })
        .await,
    }
}
