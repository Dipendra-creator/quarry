//! App-wide state shared by every window, and the bridge from GPUI tasks to
//! the Tokio runtime the database drivers run on.

use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use gpui_kit::{App, Global};
use quarry_ai::assistant::{AiEnv, AiSettings};
use quarry_ai::{Approval, ApprovalGate, Toolbox, bridge};
use quarry_core::session::SessionManager;
use quarry_core::store::{ConnectionStore, SavedQueries, config_dir};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedReceiver;

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
    pub ai: AiSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self { appearance: Appearance::Light, query_limit: Some(300), show_row_detail: true, ai: AiSettings::default() }
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
    /// AI tools on the open sessions, used by the assistant and, through the
    /// bridge, by `quarry-mcp`.
    pub toolbox: Arc<Toolbox>,
    pub gate: Arc<ApprovalGate>,
}

impl Global for AppState {}

impl AppState {
    /// Returns the changes AI tools ask the user to approve.
    pub fn init(cx: &mut App) -> Result<UnboundedReceiver<Approval>> {
        let settings = Settings::load();
        let sessions = Arc::new(SessionManager::default());
        let (gate, approvals) = ApprovalGate::new(settings.ai.writes);
        let toolbox = Arc::new(Toolbox::new(ConnectionStore::load()?, sessions.clone()).with_gate(gate.clone()));
        // Without the bridge only the CLI backends and external agents lose access.
        if let Err(e) = quarry_core::runtime().block_on(bridge::serve(toolbox.clone())) {
            eprintln!("AI bridge not started: {e:#}");
        }
        cx.set_global(AppState { store: ConnectionStore::load()?, saved: SavedQueries::load()?, sessions, settings, toolbox, gate });
        Ok(approvals)
    }

    pub fn get(cx: &App) -> &AppState {
        cx.global::<AppState>()
    }

    pub fn ai_env(&self) -> AiEnv {
        AiEnv { toolbox: self.toolbox.clone(), mcp_command: mcp_exe(), mcp_args: vec!["--attach".into(), bridge::info_path().to_string_lossy().into_owned()] }
    }
}

/// `quarry-mcp`, installed next to the app.
pub fn mcp_exe() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.with_file_name(format!("quarry-mcp{}", std::env::consts::EXE_SUFFIX))
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
