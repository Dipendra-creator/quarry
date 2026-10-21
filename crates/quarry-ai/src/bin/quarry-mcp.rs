//! MCP server (stdio, newline-delimited JSON-RPC 2.0) exposing Quarry's tools
//! to any MCP client, e.g. `claude mcp add quarry -- quarry-mcp`.

use std::sync::Arc;

use quarry_ai::Toolbox;
use quarry_core::session::SessionManager;
use quarry_core::store::ConnectionStore;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROTOCOL_VERSION: &str = "2025-06-18";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let toolbox = Toolbox::new(ConnectionStore::load()?, Arc::new(SessionManager::default()));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&toolbox, msg).await,
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
async fn handle(toolbox: &Toolbox, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "quarry", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Quarry database tools. Start with list_connections, then schema_overview to learn a database before querying it."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": Toolbox::tools().iter().map(|t| json!({
            "name": t.name, "description": t.description, "inputSchema": t.input_schema
        })).collect::<Vec<_>>()})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            Ok(match toolbox.call(name, args).await {
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
