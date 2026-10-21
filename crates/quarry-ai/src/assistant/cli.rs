//! The Claude Code and Codex CLIs as backends. Each turn runs the CLI in
//! headless JSON mode, resuming the chat's CLI session, with Quarry's MCP
//! server (attached to this app) as its only tools.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context as _, Result, bail};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use super::{AiEnv, AiSettings, Chat, ChatEvent, Provider, work_dir};

pub(super) async fn claude(chat: &mut Chat, settings: &AiSettings, env: &AiEnv, text: &str, events: &UnboundedSender<ChatEvent>) -> Result<()> {
    let dir = work_dir()?;
    // Files rather than arguments: JSON and multi-line text survive no quoting.
    let mcp = dir.join("claude-mcp.json");
    std::fs::write(&mcp, json!({"mcpServers": {"quarry": {"command": env.mcp_command, "args": env.mcp_args}}}).to_string())?;
    let prompt = dir.join(format!("prompt-{}.md", chat.session.config.id));
    std::fs::write(&prompt, chat.system_prompt(true))?;
    let mut cmd = command(program(&settings.claude_path, "claude"));
    cmd.current_dir(&dir).args(["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages"]);
    // Only Quarry's tools: no built-in tools, no other MCP servers, no user hooks or plugins.
    cmd.args(["--tools", "", "--strict-mcp-config", "--setting-sources", "", "--allowedTools", "mcp__quarry"]);
    cmd.arg("--mcp-config").arg(&mcp).arg("--system-prompt-file").arg(&prompt);
    let model = settings.model(Provider::ClaudeCode);
    if !model.is_empty() {
        cmd.args(["--model", &model]);
    }
    if let Some(id) = &chat.cli_session {
        cmd.args(["--resume", id]);
    }
    let session = &mut chat.cli_session;
    run(cmd, text, |ev| {
        match ev["type"].as_str().unwrap_or_default() {
            "system" if ev["subtype"] == "init" => {
                *session = ev["session_id"].as_str().map(str::to_string);
                let quarry = ev["mcp_servers"].as_array().and_then(|s| s.iter().find(|s| s["name"] == "quarry"));
                if quarry.is_some_and(|s| s["status"] == "failed") {
                    bail!("Claude Code could not start Quarry's MCP server ({}).", env.mcp_command.display());
                }
            }
            "stream_event" => {
                let e = &ev["event"];
                if e["type"] == "content_block_delta" && e["delta"]["type"] == "text_delta" {
                    let _ = events.send(ChatEvent::Text(e["delta"]["text"].as_str().unwrap_or_default().to_string()));
                }
            }
            "assistant" => {
                for b in blocks(&ev).filter(|b| b["type"] == "tool_use") {
                    let name = b["name"].as_str().unwrap_or_default();
                    let _ = events.send(ChatEvent::ToolStart {
                        id: b["id"].as_str().unwrap_or_default().to_string(),
                        name: name.strip_prefix("mcp__quarry__").unwrap_or(name).to_string(),
                        input: b["input"].to_string(),
                    });
                }
            }
            "user" => {
                for b in blocks(&ev).filter(|b| b["type"] == "tool_result") {
                    let _ = events.send(ChatEvent::ToolEnd {
                        id: b["tool_use_id"].as_str().unwrap_or_default().to_string(),
                        output: content_text(&b["content"]),
                        is_error: b["is_error"].as_bool().unwrap_or(false),
                    });
                }
            }
            "result" if ev["is_error"].as_bool().unwrap_or(false) => {
                let message = ev["result"].as_str().or(ev["subtype"].as_str()).unwrap_or("Claude Code failed");
                bail!("{message}");
            }
            _ => {}
        }
        Ok(())
    })
    .await
}

pub(super) async fn codex(chat: &mut Chat, settings: &AiSettings, env: &AiEnv, text: &str, events: &UnboundedSender<ChatEvent>) -> Result<()> {
    let dir = work_dir()?;
    let mut cmd = command(program(&settings.codex_path, "codex"));
    cmd.current_dir(&dir).args(["exec", "--json", "--skip-git-repo-check", "--sandbox", "read-only"]);
    // `-c` values are TOML; JSON strings and arrays are valid TOML.
    cmd.arg("-C").arg(&dir);
    cmd.arg("-c").arg(format!("mcp_servers.quarry.command={}", json!(env.mcp_command)));
    cmd.arg("-c").arg(format!("mcp_servers.quarry.args={}", json!(env.mcp_args)));
    // Quarry asks the user itself before any change, so Codex need not.
    cmd.arg("-c").arg("mcp_servers.quarry.default_tools_approval_mode=\"approve\"");
    cmd.arg("-c").arg(format!("developer_instructions={}", json!(chat.system_prompt(true))));
    let model = settings.model(Provider::Codex);
    if !model.is_empty() {
        cmd.args(["-m", &model]);
    }
    if let Some(id) = &chat.cli_session {
        cmd.args(["resume", id]);
    }
    cmd.arg("-");
    let session = &mut chat.cli_session;
    run(cmd, text, |ev| {
        let item = &ev["item"];
        let id = item["id"].as_str().unwrap_or_default().to_string();
        match (ev["type"].as_str().unwrap_or_default(), item["type"].as_str().unwrap_or_default()) {
            ("thread.started", _) => *session = ev["thread_id"].as_str().map(str::to_string),
            ("item.started", "mcp_tool_call") => {
                let name = item["tool"].as_str().unwrap_or_default().to_string();
                let _ = events.send(ChatEvent::ToolStart { id, name, input: item["arguments"].to_string() });
            }
            ("item.started", "command_execution") => {
                let input = item["command"].as_str().unwrap_or_default().to_string();
                let _ = events.send(ChatEvent::ToolStart { id, name: "shell".into(), input });
            }
            ("item.completed", "agent_message") => {
                let _ = events.send(ChatEvent::Text(format!("{}\n\n", item["text"].as_str().unwrap_or_default())));
            }
            ("item.completed", "mcp_tool_call") => {
                let failed = item["error"].is_object() || item["status"] == "failed";
                let output = match item["error"]["message"].as_str() {
                    Some(e) => e.to_string(),
                    None => content_text(&item["result"]["content"]),
                };
                let _ = events.send(ChatEvent::ToolEnd { id, output, is_error: failed });
            }
            ("item.completed", "command_execution") => {
                let output = item["aggregated_output"].as_str().unwrap_or_default().to_string();
                let is_error = item["exit_code"].as_i64().is_some_and(|c| c != 0);
                let _ = events.send(ChatEvent::ToolEnd { id, output, is_error });
            }
            ("item.completed", "error") => {
                let _ = events.send(ChatEvent::Error(item["message"].as_str().unwrap_or_default().to_string()));
            }
            ("turn.failed", _) => bail!("{}", ev["error"]["message"].as_str().unwrap_or("Codex failed")),
            ("error", _) => bail!("{}", ev["message"].as_str().unwrap_or("Codex failed")),
            _ => {}
        }
        Ok(())
    })
    .await
}

/// `<cli> --version`, to check the path in the settings.
pub(super) async fn version(settings: &AiSettings) -> Result<String> {
    let exe = match settings.provider {
        Provider::Codex => program(&settings.codex_path, "codex"),
        _ => program(&settings.claude_path, "claude"),
    };
    let out = command(exe.clone()).arg("--version").stdin(Stdio::null()).output().await.with_context(|| format!("could not start {}", exe.display()))?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(format!("{} ({})", String::from_utf8_lossy(&out.stdout).trim(), exe.display()))
}

/// The executable as configured, else the first match on PATH.
fn program(configured: &str, name: &str) -> PathBuf {
    if !configured.trim().is_empty() {
        return PathBuf::from(configured.trim());
    }
    let exts: &[&str] = if cfg!(windows) { &["exe", "cmd", "bat"] } else { &[""] };
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .flat_map(|dir| exts.iter().map(move |e| dir.join(name).with_extension(e)))
        .find(|p| p.is_file())
        .unwrap_or_else(|| name.into())
}

fn command(program: PathBuf) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    // No console window flashing up from the GUI app.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd
}

/// Runs `cmd` with `prompt` on stdin and hands each JSON line of its output
/// to `on_line`. An error from `on_line` stops the CLI.
async fn run(mut cmd: Command, prompt: &str, mut on_line: impl FnMut(Json) -> Result<()>) -> Result<()> {
    let program = cmd.as_std().get_program().to_string_lossy().into_owned();
    let mut child = cmd.spawn().with_context(|| format!("could not start {program}; check its path in Preferences → AI"))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    stdin.write_all(prompt.as_bytes()).await?;
    drop(stdin);
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stderr = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    while let Some(line) = lines.next_line().await? {
        if let Ok(v) = serde_json::from_str::<Json>(&line) {
            on_line(v)?;
        }
    }
    let status = child.wait().await?;
    if !status.success() {
        let err = stderr.await.unwrap_or_default();
        let tail: Vec<&str> = err.trim().lines().rev().take(6).collect();
        bail!("{program} exited with {status}\n{}", tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    Ok(())
}

fn blocks(ev: &Json) -> impl Iterator<Item = &Json> {
    ev["message"]["content"].as_array().into_iter().flatten()
}

/// Text of an MCP or tool-result content: a string or a list of text parts.
fn content_text(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}
