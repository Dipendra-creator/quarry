//! Lets agents outside the app use the app's live connections. The app
//! serves its [`Toolbox`] on a localhost port; `quarry-mcp` forwards tool
//! calls to it. The port and a random token are in `bridge.json` in the
//! config dir, which only the user can read.
//!
//! Protocol: one JSON object per line each way.
//! Request `{"token", "tool", "arguments"}`, reply `{"ok": …}` or `{"error": "…"}`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use quarry_core::store::config_dir;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};

use crate::{APP_HIDDEN, Toolbox};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

pub fn info_path() -> PathBuf {
    config_dir().join("bridge.json")
}

/// Starts serving `toolbox` and writes `bridge.json`. Call inside the Tokio
/// runtime; the server runs until the process exits.
pub async fn serve(toolbox: Arc<Toolbox>) -> Result<BridgeInfo> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let info = BridgeInfo {
        port: listener.local_addr()?.port(),
        token: uuid::Uuid::new_v4().to_string(),
        pid: std::process::id(),
    };
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(info_path(), serde_json::to_string_pretty(&info)?)?;
    let token = info.token.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (toolbox, token) = (toolbox.clone(), token.clone());
            tokio::spawn(async move {
                let _ = handle(stream, toolbox, token).await;
            });
        }
    });
    Ok(info)
}

async fn handle(stream: TcpStream, toolbox: Arc<Toolbox>, token: String) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let request: Json = serde_json::from_str(&line)?;
        if request["token"].as_str() != Some(token.as_str()) {
            bail!("wrong token");
        }
        let name = request["tool"].as_str().unwrap_or_default();
        let reply = if APP_HIDDEN.contains(&name) {
            json!({"error": format!("{name} is not available while the Quarry app manages the connections")})
        } else {
            match toolbox.call(name, request["arguments"].clone()).await {
                Ok(v) => json!({"ok": v}),
                Err(e) => json!({"error": format!("{e:#}")}),
            }
        };
        write.write_all(format!("{reply}\n").as_bytes()).await?;
    }
    Ok(())
}

/// The `quarry-mcp` side of the bridge.
pub struct BridgeClient {
    lines: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
    token: String,
}

impl BridgeClient {
    /// Connects to the app described by `info_file` (normally [`info_path`]).
    pub async fn connect(info_file: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(info_file).with_context(|| format!("could not read {}", info_file.display()))?;
        let info: BridgeInfo = serde_json::from_str(&text)?;
        let stream = TcpStream::connect(("127.0.0.1", info.port)).await.context("the Quarry app is not running")?;
        let (read, write) = stream.into_split();
        Ok(Self { lines: BufReader::new(read).lines(), write, token: info.token })
    }

    pub async fn call(&mut self, tool: &str, arguments: Json) -> Result<Json> {
        let request = json!({"token": self.token, "tool": tool, "arguments": arguments});
        self.write.write_all(format!("{request}\n").as_bytes()).await?;
        let line = self.lines.next_line().await?.ok_or_else(|| anyhow!("the Quarry app closed the connection"))?;
        let mut reply: Json = serde_json::from_str(&line)?;
        match reply["error"].as_str() {
            Some(e) => bail!("{e}"),
            None => Ok(reply["ok"].take()),
        }
    }
}
