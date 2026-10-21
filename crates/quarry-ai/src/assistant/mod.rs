//! The in-app assistant: a chat about one open connection, answered by a
//! model that works through the [`Toolbox`]. Four backends: the Claude Code
//! and Codex CLIs, which reach the tools through `quarry-mcp --attach`, and
//! the Anthropic and OpenAI HTTP APIs, which call the toolbox directly.

mod api;
mod cli;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use quarry_core::Session;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::{Toolbox, WritePolicy};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    ClaudeCode,
    Codex,
    Anthropic,
    OpenAi,
}

impl Provider {
    pub const ALL: [Provider; 4] = [Provider::ClaudeCode, Provider::Codex, Provider::Anthropic, Provider::OpenAi];

    pub fn label(self) -> &'static str {
        match self {
            Provider::ClaudeCode => "Claude Code",
            Provider::Codex => "Codex",
            Provider::Anthropic => "Anthropic API",
            Provider::OpenAi => "OpenAI API",
        }
    }

    pub fn is_cli(self) -> bool {
        matches!(self, Provider::ClaudeCode | Provider::Codex)
    }

    /// Keychain entry of the API key (API providers).
    pub fn key_id(self) -> &'static str {
        match self {
            Provider::OpenAi => "ai:openai",
            _ => "ai:anthropic",
        }
    }

    /// The model used when the settings name none; empty = the CLI's own default.
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Anthropic => "claude-opus-5-5",
            Provider::OpenAi => "gpt-6-astra",
            _ => "",
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Provider::Anthropic => "https://api.anthropic.com",
            Provider::OpenAi => "https://api.openai.com/v1",
            _ => "",
        }
    }
}

/// The AI section of the app's settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub provider: Provider,
    /// CLI executables; empty = found on PATH.
    pub claude_path: String,
    pub codex_path: String,
    /// Models; empty = [`Provider::default_model`].
    pub claude_model: String,
    pub codex_model: String,
    pub anthropic_model: String,
    pub openai_model: String,
    /// API endpoints; empty = [`Provider::default_url`]. The OpenAI one may
    /// point at any server with an OpenAI-compatible chat completions API.
    pub anthropic_url: String,
    pub openai_url: String,
    pub writes: WritePolicy,
}

impl AiSettings {
    pub fn model(&self, p: Provider) -> String {
        let m = match p {
            Provider::ClaudeCode => &self.claude_model,
            Provider::Codex => &self.codex_model,
            Provider::Anthropic => &self.anthropic_model,
            Provider::OpenAi => &self.openai_model,
        };
        if m.trim().is_empty() { p.default_model().to_string() } else { m.trim().to_string() }
    }

    pub fn url(&self, p: Provider) -> String {
        let u = if p == Provider::OpenAi { &self.openai_url } else { &self.anthropic_url };
        let u = if u.trim().is_empty() { p.default_url() } else { u.trim() };
        u.trim_end_matches('/').to_string()
    }

    /// API key from the keychain, else from `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`.
    pub fn api_key(&self, p: Provider) -> Option<String> {
        let env = if p == Provider::OpenAi { "OPENAI_API_KEY" } else { "ANTHROPIC_API_KEY" };
        quarry_core::store::load_password(p.key_id()).or_else(|| std::env::var(env).ok()).filter(|k| !k.trim().is_empty())
    }

    /// "Claude Code · opus", "Anthropic API · claude-opus-5-5".
    pub fn summary(&self) -> String {
        match self.model(self.provider) {
            m if m.is_empty() => self.provider.label().to_string(),
            m => format!("{} · {m}", self.provider.label()),
        }
    }
}

/// What a turn needs from the app.
#[derive(Clone)]
pub struct AiEnv {
    pub toolbox: Arc<Toolbox>,
    /// How CLI agents start the MCP server attached to this app:
    /// `quarry-mcp --attach <bridge.json>`.
    pub mcp_command: PathBuf,
    pub mcp_args: Vec<String>,
}

/// Progress of a turn, in order.
#[derive(Clone, Debug)]
pub enum ChatEvent {
    /// More of the assistant's reply (Markdown).
    Text(String),
    ToolStart { id: String, name: String, input: String },
    ToolEnd { id: String, output: String, is_error: bool },
    /// A problem that did not end the turn.
    Error(String),
}

/// One conversation about one connection.
pub struct Chat {
    pub session: Arc<Session>,
    /// The backend `history` and `cli_session` belong to.
    provider: Option<Provider>,
    /// API backends: the messages so far, in that API's format.
    history: Vec<serde_json::Value>,
    /// CLI backends: the CLI's session id, for resuming.
    cli_session: Option<String>,
}

impl Chat {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session, provider: None, history: vec![], cli_session: None }
    }

    /// Sends a user message and runs the turn to its end, reporting progress
    /// on `events`. After an error or cancellation the conversation continues
    /// from the last complete step.
    pub async fn send(&mut self, settings: &AiSettings, env: &AiEnv, text: &str, events: &UnboundedSender<ChatEvent>) -> Result<()> {
        // A different backend cannot read the other one's history.
        if self.provider != Some(settings.provider) {
            self.provider = Some(settings.provider);
            self.history.clear();
            self.cli_session = None;
        }
        match settings.provider {
            Provider::ClaudeCode => cli::claude(self, settings, env, text, events).await,
            Provider::Codex => cli::codex(self, settings, env, text, events).await,
            Provider::Anthropic | Provider::OpenAi => api::turn(self, settings, env, text, events).await,
        }
    }

    fn system_prompt(&self, cli: bool) -> String {
        let s = &self.session;
        let schema = s.schema().map(|x| format!(", schema \"{x}\"")).unwrap_or_default();
        let only_tools = if cli { "\nUse only the quarry tools to reach the database. Do not run shell commands or read files.\n" } else { "" };
        format!(
            r#"You are the AI assistant built into Quarry, a desktop database client. The user is working on the connection "{name}" (id "{id}"): {driver} {version}, database "{db}"{schema}.

Use the quarry tools to work with this database, and pass connection "{id}" to every tool. Never guess table or column names: look them up first with schema_overview, list_tables, describe_table or get_ddl. list_tables also lists views, functions and procedures, and get_ddl shows their code.
{only_tools}
You can:
- answer questions about the data by running queries (run_sql, select_rows, count_rows, search_data);
- write SQL for the user, including complex reports with joins, aggregates, CTEs and window functions; run a query you write to check that it works before you show it;
- explain a query the user pastes, and find its mistakes;
- optimize queries: read the plan with explain_query and the indexes with describe_table, then suggest a faster query or CREATE INDEX statements and say why they help;
- change data or structure (run_sql, insert_row, update_row, delete_rows, alter_table, import_csv). The user may have to approve each change. If a change is refused, do not retry it.

Write SQL for {driver} only. Put every query you show in a ```sql code block: the user can open it in the SQL editor with one click. Keep answers short and concrete. Show small results as Markdown tables of at most 20 rows, and say how many rows a query returned."#,
            name = s.config.display_name(),
            id = s.config.id,
            driver = s.kind().label(),
            version = s.server_version,
            db = s.database(),
        )
    }
}

/// Checks that the selected backend can run: the CLI starts, or the API
/// accepts the key. Returns what to show the user.
pub async fn test(settings: &AiSettings) -> Result<String> {
    match settings.provider {
        Provider::ClaudeCode | Provider::Codex => cli::version(settings).await,
        Provider::Anthropic | Provider::OpenAi => api::check(settings).await,
    }
}

/// Where the CLI backends run and keep their files.
fn work_dir() -> Result<PathBuf> {
    let dir = quarry_core::store::config_dir().join("assistant");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// At most `max` characters, so one huge tool result cannot fill the context.
fn clip(text: String, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}\n… (cut: the full result was {} characters)", &text[..at], text.chars().count()),
        None => text,
    }
}
