//! The Anthropic Messages API and OpenAI-compatible chat completions, both
//! streamed, with Quarry's tools run in-process between model steps.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value as Json, json};
use tokio::sync::mpsc::UnboundedSender;

use super::{AiEnv, AiSettings, Chat, ChatEvent, Provider, clip};
use crate::{APP_HIDDEN, Toolbox};

const MAX_STEPS: usize = 40;
const MAX_TOOL_OUTPUT: usize = 40_000;
/// Models that accept server-side refusal fallbacks on Anthropic's own API.
const FALLBACK_MODELS: &[&str] = &["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().connect_timeout(Duration::from_secs(20)).build().expect("HTTP client"))
}

struct ToolCall {
    id: String,
    name: String,
    /// `Err` holds input that was not valid JSON.
    input: Result<Json, String>,
}

pub(super) async fn turn(chat: &mut Chat, settings: &AiSettings, env: &AiEnv, text: &str, events: &UnboundedSender<ChatEvent>) -> Result<()> {
    let p = settings.provider;
    let key = settings.api_key(p).ok_or_else(|| anyhow!("No {} key. Add one in Preferences → AI.", p.label()))?;
    let system = chat.system_prompt(false);
    let mut msgs = chat.history.clone();
    msgs.push(json!({"role": "user", "content": text}));
    for _ in 0..MAX_STEPS {
        let (reply, calls) = match p {
            Provider::OpenAi => openai_step(settings, &key, &system, &msgs, events).await?,
            _ => anthropic_step(settings, &key, &system, &msgs, events).await?,
        };
        // An empty reply cannot be sent back: the APIs reject empty content.
        if reply["content"].as_array().is_some_and(|c| !c.is_empty()) || reply["content"].is_string() || !calls.is_empty() {
            msgs.push(reply);
        }
        if calls.is_empty() {
            chat.history = msgs;
            return Ok(());
        }
        let mut results = Vec::new();
        for call in calls {
            let input = match &call.input {
                Ok(v) => v.to_string(),
                Err(raw) => raw.clone(),
            };
            let _ = events.send(ChatEvent::ToolStart { id: call.id.clone(), name: call.name.clone(), input });
            let (output, is_error) = run_tool(&env.toolbox, &call).await;
            let _ = events.send(ChatEvent::ToolEnd { id: call.id.clone(), output: output.clone(), is_error });
            results.push((call.id, output, is_error));
        }
        match p {
            Provider::OpenAi => msgs.extend(
                results.into_iter().map(|(id, output, _)| json!({"role": "tool", "tool_call_id": id, "content": output})),
            ),
            _ => msgs.push(json!({"role": "user", "content": results.into_iter().map(|(id, output, is_error)| json!({
                "type": "tool_result", "tool_use_id": id, "content": output, "is_error": is_error
            })).collect::<Vec<_>>()})),
        }
        // A complete step: a cancel from here on keeps it.
        chat.history = msgs.clone();
    }
    bail!("Stopped after {MAX_STEPS} steps. Ask me to continue if you need more.")
}

async fn run_tool(toolbox: &Toolbox, call: &ToolCall) -> (String, bool) {
    let input = match &call.input {
        Ok(v) => v.clone(),
        Err(raw) => return (json!({"INVALID_JSON": raw}).to_string(), true),
    };
    if APP_HIDDEN.contains(&call.name.as_str()) {
        return (format!("{} is not available in the app", call.name), true);
    }
    match toolbox.call(&call.name, input).await {
        Ok(v) => (clip(v.to_string(), MAX_TOOL_OUTPUT), false),
        Err(e) => (format!("{e:#}"), true),
    }
}

fn tools(openai: bool) -> Vec<Json> {
    Toolbox::tools()
        .into_iter()
        .filter(|t| !APP_HIDDEN.contains(&t.name))
        .map(|t| match openai {
            true => json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}}),
            false => json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}),
        })
        .collect()
}

async fn anthropic_step(
    settings: &AiSettings,
    key: &str,
    system: &str,
    msgs: &[Json],
    events: &UnboundedSender<ChatEvent>,
) -> Result<(Json, Vec<ToolCall>)> {
    let url = settings.url(Provider::Anthropic);
    let model = settings.model(Provider::Anthropic);
    let mut body = json!({
        "model": model, "max_tokens": 32000, "stream": true,
        "system": system, "tools": tools(false), "messages": msgs,
        // Caches the tools, the system prompt and the conversation so far.
        "cache_control": {"type": "ephemeral"},
    });
    let mut req = client()
        .post(format!("{url}/v1/messages"))
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01");
    // On Anthropic's own API, a request the model declines is retried on another model.
    if url == Provider::Anthropic.default_url() && FALLBACK_MODELS.contains(&model.as_str()) {
        body["fallbacks"] = json!("default");
        req = req.header("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    let mut stream = Sse(send(with_json(req, &body)).await?, Vec::new());
    let mut blocks: Vec<Json> = Vec::new();
    let mut inputs: Vec<String> = Vec::new();
    let mut stop = String::new();
    while let Some(data) = stream.next().await? {
        let ev: Json = serde_json::from_str(&data)?;
        let i = ev["index"].as_u64().unwrap_or(0) as usize;
        match ev["type"].as_str().unwrap_or_default() {
            "content_block_start" => {
                if blocks.len() <= i {
                    blocks.resize(i + 1, Json::Null);
                    inputs.resize(i + 1, String::new());
                }
                blocks[i] = ev["content_block"].clone();
            }
            "content_block_delta" => {
                let d = &ev["delta"];
                let Some(block) = blocks.get_mut(i) else { continue };
                // Blocks go back to the API unchanged, thinking and signatures included.
                match d["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or_default();
                        append(block, "text", t);
                        let _ = events.send(ChatEvent::Text(t.to_string()));
                    }
                    "input_json_delta" => inputs[i].push_str(d["partial_json"].as_str().unwrap_or_default()),
                    "thinking_delta" => append(block, "thinking", d["thinking"].as_str().unwrap_or_default()),
                    "signature_delta" => append(block, "signature", d["signature"].as_str().unwrap_or_default()),
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(s) = ev["delta"]["stop_reason"].as_str() {
                    stop = s.to_string();
                }
            }
            "error" => bail!("{}", ev["error"]["message"].as_str().unwrap_or("the API reported an error")),
            _ => {}
        }
    }
    let mut calls = Vec::new();
    for (block, raw) in blocks.iter_mut().zip(&inputs) {
        if block["type"] == "tool_use" {
            let input = match raw.trim() {
                "" => Ok(json!({})),
                raw => serde_json::from_str::<Json>(raw).map_err(|_| raw.to_string()),
            };
            block["input"] = input.clone().unwrap_or_else(|_| json!({}));
            let id = block["id"].as_str().unwrap_or_default().to_string();
            calls.push(ToolCall { id, name: block["name"].as_str().unwrap_or_default().to_string(), input });
        }
    }
    blocks.retain(|b| !b.is_null());
    match stop.as_str() {
        "refusal" => bail!("The model declined this request."),
        "max_tokens" if !calls.is_empty() => bail!("The reply was cut off before its tool call was complete."),
        _ => {}
    }
    Ok((json!({"role": "assistant", "content": blocks}), calls))
}

async fn openai_step(
    settings: &AiSettings,
    key: &str,
    system: &str,
    msgs: &[Json],
    events: &UnboundedSender<ChatEvent>,
) -> Result<(Json, Vec<ToolCall>)> {
    let url = settings.url(Provider::OpenAi);
    let mut messages = vec![json!({"role": "system", "content": system})];
    messages.extend(msgs.iter().cloned());
    let body = json!({"model": settings.model(Provider::OpenAi), "stream": true, "messages": messages, "tools": tools(true)});
    let req = client().post(format!("{url}/chat/completions")).bearer_auth(key);
    let mut stream = Sse(send(with_json(req, &body)).await?, Vec::new());
    let mut text = String::new();
    // (id, name, arguments), assembled from fragments by index.
    let mut calls: Vec<(String, String, String)> = Vec::new();
    let mut finish = String::new();
    while let Some(data) = stream.next().await? {
        if data == "[DONE]" {
            break;
        }
        let ev: Json = serde_json::from_str(&data)?;
        if let Some(e) = ev.get("error") {
            bail!("{}", e["message"].as_str().unwrap_or("the API reported an error"));
        }
        let choice = &ev["choices"][0];
        if let Some(t) = choice["delta"]["content"].as_str() {
            text.push_str(t);
            let _ = events.send(ChatEvent::Text(t.to_string()));
        }
        for tc in choice["delta"]["tool_calls"].as_array().into_iter().flatten() {
            let i = tc["index"].as_u64().unwrap_or(0) as usize;
            if calls.len() <= i {
                calls.resize(i + 1, Default::default());
            }
            if let Some(id) = tc["id"].as_str() {
                calls[i].0 = id.to_string();
            }
            calls[i].1.push_str(tc["function"]["name"].as_str().unwrap_or_default());
            calls[i].2.push_str(tc["function"]["arguments"].as_str().unwrap_or_default());
        }
        if let Some(f) = choice["finish_reason"].as_str() {
            finish = f.to_string();
        }
    }
    if finish == "length" && !calls.is_empty() {
        bail!("The reply was cut off before its tool call was complete.");
    }
    let mut reply = json!({"role": "assistant", "content": if text.is_empty() { Json::Null } else { json!(text) }});
    if !calls.is_empty() {
        reply["tool_calls"] = calls
            .iter()
            .map(|(id, name, args)| json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}}))
            .collect();
    }
    let calls = calls
        .into_iter()
        .map(|(id, name, args)| {
            let input = match args.trim() {
                "" => Ok(json!({})),
                raw => serde_json::from_str(raw).map_err(|_| raw.to_string()),
            };
            ToolCall { id, name, input }
        })
        .collect();
    Ok((reply, calls))
}

/// Checks the key and endpoint by listing models.
pub(super) async fn check(settings: &AiSettings) -> Result<String> {
    let p = settings.provider;
    let key = settings.api_key(p).ok_or_else(|| anyhow!("No API key. Enter one above."))?;
    let url = settings.url(p);
    let req = match p {
        Provider::OpenAi => client().get(format!("{url}/models")).bearer_auth(&key),
        _ => client().get(format!("{url}/v1/models")).header("x-api-key", &key).header("anthropic-version", "2023-06-01"),
    };
    send(req).await?;
    Ok(format!("Connected to {url}. Model: {}", settings.model(p)))
}

fn with_json(req: reqwest::RequestBuilder, body: &Json) -> reqwest::RequestBuilder {
    req.header("content-type", "application/json").body(body.to_string())
}

/// Sends, retrying rate limits and overloads twice.
async fn send(req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    let mut wait = Duration::from_secs(2);
    let mut attempt = 0;
    loop {
        let resp = req.try_clone().expect("buffered body").send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        if attempt == 2 || !matches!(status.as_u16(), 429 | 500 | 502 | 503 | 529) {
            let message = serde_json::from_str::<Json>(&body)
                .ok()
                .and_then(|j| j["error"]["message"].as_str().map(str::to_string))
                .unwrap_or(body);
            bail!("{status}: {}", clip(message, 500));
        }
        tokio::time::sleep(wait).await;
        wait *= 3;
        attempt += 1;
    }
}

fn append(block: &mut Json, key: &str, more: &str) {
    let mut s = block[key].as_str().unwrap_or_default().to_string();
    s.push_str(more);
    block[key] = Json::String(s);
}

/// A server-sent event stream; yields each `data:` payload.
struct Sse(reqwest::Response, Vec<u8>);

impl Sse {
    async fn next(&mut self) -> Result<Option<String>> {
        loop {
            if let Some(end) = self.1.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.1.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line);
                if let Some(data) = line.trim_end().strip_prefix("data:") {
                    return Ok(Some(data.trim_start().to_string()));
                }
                continue;
            }
            match self.0.chunk().await? {
                Some(bytes) => self.1.extend_from_slice(&bytes),
                None => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use quarry_core::session::SessionManager;
    use quarry_core::store::ConnectionStore;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// Serves one canned event stream per request, in order; yields the raw requests.
    async fn mock(replies: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = vec![];
            for reply in replies {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                loop {
                    let mut chunk = [0u8; 8192];
                    let n = s.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    let Some(end) = text.find("\r\n\r\n") else { continue };
                    let len = text[..end]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
                        .unwrap_or(0);
                    if n == 0 || buf.len() >= end + 4 + len {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&buf).into_owned());
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
                s.write_all(format!("{head}{reply}").as_bytes()).await.unwrap();
                let _ = s.shutdown().await;
            }
            requests
        });
        (url, server)
    }

    fn body(request: &str) -> Json {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    async fn fixture() -> (Chat, AiEnv) {
        let dir = std::env::temp_dir().join(format!("quarry-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = quarry_core::ConnectionConfig::new(quarry_core::DriverKind::Sqlite);
        c.file_path = dir.join("t.db").to_string_lossy().into();
        c.startup_commands = "CREATE TABLE fruit (id integer primary key, name text); INSERT INTO fruit (name) VALUES ('apple'), ('kiwi');".into();
        let sessions = Arc::new(SessionManager::default());
        let session = sessions.connect(c).await.unwrap();
        let store = ConnectionStore::load_from(dir.join("connections.json")).unwrap();
        let env = AiEnv { toolbox: Arc::new(Toolbox::new(store, sessions)), mcp_command: "".into(), mcp_args: vec![] };
        (Chat::new(session), env)
    }

    async fn run(chat: &mut Chat, env: &AiEnv, settings: &AiSettings) -> Vec<ChatEvent> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        chat.send(settings, env, "How many fruits?", &tx).await.unwrap();
        drop(tx);
        let mut events = vec![];
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
        events
    }

    fn check_events(events: &[ChatEvent]) {
        assert!(matches!(&events[0], ChatEvent::Text(t) if t == "Counting."), "{events:?}");
        assert!(matches!(&events[1], ChatEvent::ToolStart { name, .. } if name == "count_rows"), "{events:?}");
        assert!(matches!(&events[2], ChatEvent::ToolEnd { output, is_error: false, .. } if output.contains("\"count\":2")), "{events:?}");
        assert!(matches!(&events[3], ChatEvent::Text(t) if t == "There are 2."), "{events:?}");
    }

    #[tokio::test]
    async fn anthropic_streams_and_runs_tools() {
        let (mut chat, env) = fixture().await;
        let input = json!({"connection": chat.session.config.id, "table": "fruit"}).to_string();
        let (a, b) = input.split_at(12);
        let sse = |events: Vec<Json>| events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())).collect::<String>();
        let first = sse(vec![
            json!({"type": "message_start", "message": {}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Counting."}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "count_rows", "input": {}}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": a}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": b}}),
            json!({"type": "content_block_stop", "index": 2}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
            json!({"type": "message_stop"}),
        ]);
        let second = sse(vec![
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "There are 2."}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
        ]);
        let (url, server) = mock(vec![first, second]).await;
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "test-key") };
        let settings = AiSettings { provider: Provider::Anthropic, anthropic_url: url, ..Default::default() };
        check_events(&run(&mut chat, &env, &settings).await);

        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /v1/messages") && requests[0].contains("anthropic-version: 2023-06-01"));
        let second = body(&requests[1]);
        assert_eq!(second["model"], "claude-opus-5-5");
        // The assistant turn goes back unchanged, thinking signature included.
        assert_eq!(second["messages"][1]["content"][0]["signature"], "sig");
        assert_eq!(second["messages"][1]["content"][2]["input"]["table"], "fruit");
        assert_eq!(second["messages"][2]["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(chat.history.len(), 4);
    }

    #[tokio::test]
    async fn openai_streams_and_runs_tools() {
        let (mut chat, env) = fixture().await;
        let input = json!({"connection": chat.session.config.id, "table": "fruit"}).to_string();
        let (a, b) = input.split_at(12);
        let chunk = |delta: Json, finish: Json| format!("data: {}\n\n", json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}));
        let first = [
            chunk(json!({"role": "assistant", "content": "Counting."}), Json::Null),
            chunk(json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "count_rows", "arguments": ""}}]}), Json::Null),
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": a}}]}), Json::Null),
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": b}}]}), Json::Null),
            chunk(json!({}), json!("tool_calls")),
            "data: [DONE]\n\n".to_string(),
        ]
        .concat();
        let second = [chunk(json!({"content": "There are 2."}), Json::Null), chunk(json!({}), json!("stop")), "data: [DONE]\n\n".to_string()].concat();
        let (url, server) = mock(vec![first, second]).await;
        unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };
        let settings = AiSettings { provider: Provider::OpenAi, openai_url: url, ..Default::default() };
        check_events(&run(&mut chat, &env, &settings).await);

        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /chat/completions"));
        let second = body(&requests[1]);
        assert_eq!(second["messages"][0]["role"], "system");
        assert_eq!(second["messages"][2]["tool_calls"][0]["function"]["arguments"], input);
        assert_eq!(second["messages"][3]["role"], "tool");
        assert_eq!(second["messages"][3]["tool_call_id"], "call_1");
        assert_eq!(chat.history.len(), 4);
    }
}
