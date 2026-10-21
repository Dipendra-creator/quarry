//! MCP server (stdio, newline-delimited JSON-RPC 2.0) exposing Quarry's tools
//! to any MCP client, e.g. `claude mcp add quarry -- quarry-mcp`.
//!
//! While the Quarry app is open, tool calls go to the app and work on its
//! open connections, with its approval prompts for changes. Otherwise the
//! server opens saved connections itself.
//!
//!   quarry-mcp                       attach to the app if it is running
//!   quarry-mcp --attach [bridge.json]  require the app (the app's own assistant uses this)
//!   quarry-mcp --standalone          never attach

use std::path::PathBuf;
use std::sync::Arc;

use quarry_ai::bridge::{self, BridgeClient};
use quarry_ai::{APP_HIDDEN, Toolbox};
use quarry_core::session::SessionManager;
use quarry_core::store::ConnectionStore;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

const PROTOCOL_VERSION: &str = "2025-06-18";

enum Backend {
    Local(Toolbox),
    App(Mutex<BridgeClient>),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let attach = args.iter().position(|a| a == "--attach");
    let info = attach.and_then(|i| args.get(i + 1)).map(PathBuf::from).unwrap_or_else(bridge::info_path);
    let backend = if args.iter().any(|a| a == "--standalone") {
        None
    } else {
        match BridgeClient::connect(&info).await {
            Ok(client) => Some(Backend::App(Mutex::new(client))),
            Err(e) if attach.is_some() => return Err(e.context("--attach: cannot reach the Quarry app")),
            Err(_) => None,
        }
    };
    let backend = match backend {
        Some(b) => b,
        None => Backend::Local(Toolbox::new(ConnectionStore::load()?, Arc::new(SessionManager::default()))),
    };

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&backend, msg).await,
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})),
        };
        if let Some(r) = response {
            stdout.write_all(format!("{r}\n").as_bytes()).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

/// `None` for notifications, which get no reply.
async fn handle(backend: &Backend, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let in_app = matches!(backend, Backend::App(_));
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "quarry", "version": env!("CARGO_PKG_VERSION")},
            "instructions": if in_app {
                "Quarry database tools, working on the connections open in the Quarry app. Start with list_connections, then schema_overview to learn a database before querying it. Changes may need the user's approval in the app."
            } else {
                "Quarry database tools. Start with list_connections, then schema_overview to learn a database before querying it."
            }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": Toolbox::tools().iter().filter(|t| !in_app || !APP_HIDDEN.contains(&t.name)).map(|t| json!({
            "name": t.name, "description": t.description, "inputSchema": t.input_schema
        })).collect::<Vec<_>>()})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let outcome = match backend {
                Backend::Local(toolbox) => toolbox.call(name, args).await,
                Backend::App(client) => client.lock().await.call(name, args).await,
            };
            Ok(match outcome {
                Ok(v) => json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}]}),
                Err(e) => json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
            })
        }
        other => Err(json!({"code": -32601, "message": format!("method not found: {other}")})),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}
