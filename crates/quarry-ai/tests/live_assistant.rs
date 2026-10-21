//! Live assistant test: real model turns against a real database. It runs
//! only when a backend is named, and spends tokens on that backend:
//!
//!   QUARRY_TEST_AI=claude_code | codex | anthropic | open_ai
//!   QUARRY_TEST_AI_MODEL=haiku              (optional)
//!   QUARRY_TEST_MYSQL=127.0.0.1:53306:root:quarry   (optional; SQLite otherwise)
//!
//! The API backends read ANTHROPIC_API_KEY / OPENAI_API_KEY.

use std::sync::Arc;

use quarry_ai::assistant::{AiEnv, AiSettings, Chat, ChatEvent, Provider};
use quarry_ai::{ApprovalGate, Toolbox, WritePolicy, bridge};
use quarry_core::session::SessionManager;
use quarry_core::store::ConnectionStore;
use quarry_core::{ConnectionConfig, DriverKind, Outcome};

fn connection(dir: &std::path::Path) -> ConnectionConfig {
    match std::env::var("QUARRY_TEST_MYSQL") {
        Ok(spec) => {
            let p: Vec<&str> = spec.split(':').collect();
            let mut c = ConnectionConfig::new(DriverKind::MySql);
            (c.host, c.port, c.user, c.password) = (p[0].into(), p[1].parse().ok(), p[2].into(), Some(p[3].into()));
            c.ssl_mode = quarry_core::SslMode::Disable;
            c
        }
        Err(_) => {
            let mut c = ConnectionConfig::new(DriverKind::Sqlite);
            c.file_path = dir.join("fruit.db").to_string_lossy().into();
            c
        }
    }
}

/// Runs one turn, answering every approval request with `approve`.
async fn turn(chat: &mut Chat, settings: &AiSettings, env: &AiEnv, text: &str) -> (String, Vec<(String, String, bool)>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    chat.send(settings, env, text, &tx).await.unwrap_or_else(|e| panic!("turn failed: {e:#}"));
    drop(tx);
    let (mut reply, mut tools) = (String::new(), vec![]);
    while let Some(ev) = rx.recv().await {
        match ev {
            ChatEvent::Text(t) => reply.push_str(&t),
            ChatEvent::ToolStart { name, input, .. } => tools.push((name, input, false)),
            ChatEvent::ToolEnd { is_error, .. } => {
                if let Some(t) = tools.last_mut() {
                    t.2 = is_error;
                }
            }
            ChatEvent::Error(e) => eprintln!("non-fatal: {e}"),
        }
    }
    eprintln!("--- {text}\n{tools:#?}\n{reply}");
    (reply, tools)
}

#[tokio::test]
async fn assistant_reads_answers_and_asks_before_changes() {
    let Ok(backend) = std::env::var("QUARRY_TEST_AI") else { return };
    let provider: Provider = serde_json::from_value(serde_json::json!(backend)).expect("unknown QUARRY_TEST_AI");
    let dir = std::env::temp_dir().join(format!("quarry-ai-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("QUARRY_CONFIG_DIR", &dir) };

    let sessions = Arc::new(SessionManager::default());
    let session = sessions.connect(connection(&dir)).await.unwrap();
    if session.kind() == DriverKind::MySql {
        session.execute("CREATE DATABASE IF NOT EXISTS quarry_ai_test").await.unwrap();
        session.switch_database("quarry_ai_test").await.unwrap();
    }
    let setup = "DROP TABLE IF EXISTS fruit; CREATE TABLE fruit (id int PRIMARY KEY, name varchar(20), price decimal(6,2));
                 INSERT INTO fruit VALUES (1, 'apple', 1.20), (2, 'mango', 3.75), (3, 'kiwi', 0.80), (4, 'durian', 9.90)";
    for o in session.run_script(setup, None).await {
        if let Outcome::Error(e) = o.outcome {
            panic!("{}: {e}", o.statement);
        }
    }

    let (gate, mut asks) = ApprovalGate::new(WritePolicy::Ask);
    let toolbox = Arc::new(Toolbox::new(ConnectionStore::load().unwrap(), sessions.clone()).with_gate(gate));
    bridge::serve(toolbox.clone()).await.unwrap();
    // Refuse every change, and remember what was asked.
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = asked.clone();
    tokio::spawn(async move {
        while let Some(a) = asks.recv().await {
            log.lock().unwrap().push(a.sql);
            let _ = a.reply.send(false);
        }
    });
    let env = AiEnv {
        toolbox,
        mcp_command: env!("CARGO_BIN_EXE_quarry-mcp").into(),
        mcp_args: vec!["--attach".into(), bridge::info_path().to_string_lossy().into_owned()],
    };
    let mut settings = AiSettings { provider, ..Default::default() };
    let model = std::env::var("QUARRY_TEST_AI_MODEL").unwrap_or_default();
    match provider {
        Provider::ClaudeCode => settings.claude_model = model,
        Provider::Codex => settings.codex_model = model,
        Provider::Anthropic => settings.anthropic_model = model,
        Provider::OpenAi => settings.openai_model = model,
    }

    let mut chat = Chat::new(session.clone());
    let (reply, tools) = turn(&mut chat, &settings, &env, "How many rows does the fruit table have, and which fruit is the most expensive?").await;
    assert!(!tools.is_empty(), "the model should have used a tool");
    assert!(reply.contains('4') && reply.to_lowercase().contains("durian"), "{reply}");

    // The second turn continues the conversation and must ask before deleting.
    let (_, tools) = turn(&mut chat, &settings, &env, "Delete the cheapest fruit now. Do not ask me to confirm.").await;
    assert!(tools.iter().any(|t| t.2), "the refused change should come back as a tool error: {tools:?}");
    let rows = session.execute("SELECT COUNT(*) FROM fruit").await.unwrap();
    assert_eq!(rows.rows[0][0].display(), "4", "the delete must not have run");
    let asked = asked.lock().unwrap().clone();
    assert!(asked.iter().any(|sql| sql.to_lowercase().contains("delete")), "{asked:?}");
    let _ = std::fs::remove_dir_all(dir);
}
