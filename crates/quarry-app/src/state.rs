//! App-wide state shared by every window, and the bridge from GPUI tasks to
//! the Tokio runtime the database drivers run on.

use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use gpui_kit::{App, Global};
use quarry_core::session::SessionManager;
use quarry_core::store::{ConnectionStore, SavedQueries, config_dir};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    Light,
    Dark,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    /// Row limit appended to SELECTs in the SQL editor; `None` = no limit.
    pub query_limit: Option<u32>,
    pub show_row_detail: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { appearance: Appearance::Light, query_limit: Some(300), show_row_detail: true }
    }
}

impl Settings {
    fn path() -> std::path::PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(Self::path(), serde_json::to_string_pretty(self).unwrap_or_default());
    }
}

pub struct AppState {
    pub store: ConnectionStore,
    pub saved: SavedQueries,
    pub sessions: Arc<SessionManager>,
    pub settings: Settings,
}

impl Global for AppState {}

impl AppState {
    pub fn init(cx: &mut App) -> Result<()> {
        cx.set_global(AppState {
            store: ConnectionStore::load()?,
            saved: SavedQueries::load()?,
            sessions: Arc::new(SessionManager::default()),
            settings: Settings::load(),
        });
        Ok(())
    }

    pub fn get(cx: &App) -> &AppState {
        cx.global::<AppState>()
    }
}

/// Runs a database future on the Tokio runtime; await it from a GPUI task.
pub fn db<T, F>(fut: F) -> impl Future<Output = Result<T>>
where
    T: Send + 'static,
    F: Future<Output = Result<T>> + Send + 'static,
{
    let handle = quarry_core::runtime().spawn(fut);
    async move { handle.await? }
}
