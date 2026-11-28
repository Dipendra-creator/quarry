//! Redis (and Valkey, KeyDB, Dragonfly) over RESP.
//!
//! Keys are the sidebar's items, grouped by type. Opening one shows its
//! value as rows: a hash's fields, a list's items, a set's members, a sorted
//! set's members with scores, a stream's entries. Grid edits become Redis
//! commands, applied in one MULTI/EXEC. The SQL editor runs redis-cli style
//! command lines.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use async_trait::async_trait;
use redis::aio::MultiplexedConnection;
use redis::{AsyncConnectionConfig, ConnectionAddr, IntoConnectionInfo, RedisConnectionInfo, Value as Rv};

use super::Driver;
use crate::changes::{ColumnValues, KeyKind, RowChange};
use crate::commands::{command_line, split_args};
use crate::model::{ConnectionConfig, DriverKind, SslMode};
use crate::query::{BrowseRequest, Filter, QueryResult, ResultColumn, count_matching, filter_sort_page};
use crate::schema::{ColumnInfo, Item, ItemKind, TableRef, TableStructure};
use crate::value::Value;

/// Keys listed in the sidebar; larger keyspaces show the first ones.
const MAX_KEYS: usize = 10_000;
/// Elements of one key read for the grid.
const MAX_ELEMENTS: usize = 100_000;
/// Elements written out by "Show DDL".
const DDL_ELEMENTS: usize = 500;

pub struct RedisDriver {
    conn: MultiplexedConnection,
    db: i64,
    /// Types of keys seen so far, so staged edits know which commands to write.
    types: Mutex<HashMap<String, ItemKind>>,
}

/// "db3", "3" or "" (0).
fn db_index(name: &str) -> i64 {
    name.trim().trim_start_matches(|c: char| c.is_ascii_alphabetic()).parse().unwrap_or(0)
}

fn kind_of(redis_type: &str) -> ItemKind {
    match redis_type {
        "string" => ItemKind::StringKey,
        "hash" => ItemKind::HashKey,
        "list" => ItemKind::ListKey,
        "set" => ItemKind::SetKey,
        "zset" => ItemKind::ZSetKey,
        "stream" => ItemKind::StreamKey,
        "ReJSON-RL" | "json" => ItemKind::JsonKey,
        _ => ItemKind::OtherKey,
    }
}

impl RedisDriver {
    pub async fn connect(cfg: &ConnectionConfig, database: Option<&str>) -> Result<Self> {
        let db = db_index(database.unwrap_or(&cfg.database));
        let verify = matches!(cfg.ssl_mode, SslMode::VerifyCa | SslMode::VerifyFull);
        let tls = ConnectionAddr::TcpTls { host: cfg.host.clone(), port: cfg.port_or_default(), insecure: !verify, tls_params: None };
        let plain = ConnectionAddr::Tcp(cfg.host.clone(), cfg.port_or_default());
        let conn = match cfg.ssl_mode {
            // A plain server never answers a TLS handshake, so trying TLS
            // first would stall every local connection until the timeout:
            // "Preferred" connects without it. Cloud services need "Required".
            SslMode::Disable | SslMode::Prefer => Self::open(plain, cfg, db).await,
            _ => Self::open(tls, cfg, db).await,
        }
        .context("could not connect to Redis")?;
        Ok(Self { conn, db, types: Mutex::new(HashMap::new()) })
    }

    async fn open(addr: ConnectionAddr, cfg: &ConnectionConfig, db: i64) -> Result<MultiplexedConnection> {
        let mut settings = RedisConnectionInfo::default().set_db(db);
        if !cfg.user.trim().is_empty() {
            settings = settings.set_username(cfg.user.trim());
        }
        if let Some(pw) = cfg.password.as_deref().filter(|p| !p.is_empty()) {
            settings = settings.set_password(pw);
        }
        let client = redis::Client::open(addr.into_connection_info()?.set_redis_settings(settings))?;
        let config = AsyncConnectionConfig::new()
            .set_connection_timeout(Some(Duration::from_secs(15)))
            .set_response_timeout(Some(Duration::from_secs(60)));
        let mut conn = client.get_multiplexed_async_connection_with_config(&config).await?;
        redis::cmd("PING").query_async::<Rv>(&mut conn).await?;
        Ok(conn)
    }

    async fn run<S: AsRef<str>>(&self, args: &[S]) -> Result<Rv> {
        let (name, rest) = args.split_first().ok_or_else(|| anyhow!("empty command"))?;
        let mut cmd = redis::cmd(name.as_ref());
        for a in rest {
            cmd.arg(a.as_ref());
        }
        let v: Rv = cmd.query_async(&mut self.conn.clone()).await?;
        if let Rv::ServerError(e) = v {
            bail!("{e:?}");
        }
        Ok(v)
    }

    async fn key_type(&self, key: &str) -> Result<ItemKind> {
        let t = text(&self.run(&["TYPE", key]).await?).unwrap_or_default();
        if t == "none" {
            bail!("the key {key:?} does not exist (any more)");
        }
        let kind = kind_of(&t);
        self.types.lock().unwrap().insert(key.to_string(), kind);
        Ok(kind)
    }

    /// `SCAN`-style iteration: `args` with `{cursor}` for the cursor, until
    /// the cursor returns to 0 or `max` items are read.
    async fn scan_all(&self, args: &[&str], max: usize) -> Result<Vec<Rv>> {
        let mut cursor = "0".to_string();
        let mut out = Vec::new();
        loop {
            let line: Vec<&str> = args.iter().map(|a| if *a == "{cursor}" { cursor.as_str() } else { a }).collect();
            let reply = self.run(&line).await?;
            let Rv::Array(mut parts) = reply else { bail!("unexpected reply to {}", args[0]) };
            if parts.len() != 2 {
                bail!("unexpected reply to {}", args[0]);
            }
            let items = parts.pop().unwrap();
            cursor = text(&parts[0]).unwrap_or_else(|| "0".into());
            if let Rv::Array(items) = items {
                out.extend(items);
            }
            if cursor == "0" || out.len() >= max {
                break;
            }
        }
        Ok(out)
    }

    /// Column names and rows of a key's value.
    async fn elements(&self, key: &str, kind: ItemKind) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        let max = MAX_ELEMENTS.to_string();
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Ok(match kind {
            ItemKind::StringKey => (names(&["value"]), vec![vec![to_value(&self.run(&["GET", key]).await?)]]),
            ItemKind::HashKey => {
                let flat = self.scan_all(&["HSCAN", key, "{cursor}", "COUNT", "1000"], MAX_ELEMENTS * 2).await?;
                let mut rows: Vec<Vec<Value>> = flat.chunks(2).map(|p| vec![to_value(&p[0]), p.get(1).map(to_value).unwrap_or(Value::Null)]).collect();
                rows.sort_by(|a, b| a[0].display().cmp(&b[0].display()));
                rows.dedup_by(|a, b| a[0] == b[0]);
                (names(&["field", "value"]), rows)
            }
            ItemKind::ListKey => {
                let items = arr(self.run(&["LRANGE", key, "0", &(MAX_ELEMENTS - 1).to_string()]).await?);
                let rows = items.iter().enumerate().map(|(i, v)| vec![Value::Int(i as i64), to_value(v)]).collect();
                (names(&["index", "value"]), rows)
            }
            ItemKind::SetKey => {
                let mut rows: Vec<Vec<Value>> = self.scan_all(&["SSCAN", key, "{cursor}", "COUNT", "1000"], MAX_ELEMENTS).await?.iter().map(|v| vec![to_value(v)]).collect();
                rows.sort_by(|a, b| a[0].display().cmp(&b[0].display()));
                rows.dedup();
                (names(&["member"]), rows)
            }
            ItemKind::ZSetKey => {
                let flat = arr(self.run(&["ZRANGE", key, "0", &(MAX_ELEMENTS - 1).to_string(), "WITHSCORES"]).await?);
                let rows = flat
                    .chunks(2)
                    .map(|p| {
                        let score = p.get(1).and_then(text).and_then(|s| s.parse::<f64>().ok()).map(Value::Float).unwrap_or(Value::Null);
                        vec![to_value(&p[0]), score]
                    })
                    .collect();
                (names(&["member", "score"]), rows)
            }
            ItemKind::StreamKey => {
                let entries = arr(self.run(&["XRANGE", key, "-", "+", "COUNT", &max]).await?);
                stream_rows(&entries)
            }
            ItemKind::JsonKey => (names(&["value"]), vec![vec![to_value(&self.run(&["JSON.GET", key]).await?)]]),
            _ => bail!("Quarry cannot show this type of key yet; use the command editor"),
        })
    }

    fn cached_kind(&self, key: &str) -> Result<ItemKind> {
        self.types.lock().unwrap().get(key).copied().ok_or_else(|| anyhow!("reload {key:?}: Quarry does not know its type yet"))
    }
}

/// A stream's entries as rows: the id, then one column per field.
fn stream_rows(entries: &[Rv]) -> (Vec<String>, Vec<Vec<Value>>) {
    let mut fields: Vec<String> = Vec::new();
    let mut parsed: Vec<(Value, Vec<(String, Value)>)> = Vec::new();
    for e in entries {
        let Rv::Array(parts) = e else { continue };
        let id = parts.first().map(to_value).unwrap_or(Value::Null);
        let pairs: Vec<(String, Value)> = match parts.get(1) {
            Some(Rv::Array(kv)) => kv.chunks(2).map(|p| (text(&p[0]).unwrap_or_default(), p.get(1).map(to_value).unwrap_or(Value::Null))).collect(),
            _ => vec![],
        };
        for (f, _) in &pairs {
            if !fields.contains(f) {
                fields.push(f.clone());
            }
        }
        parsed.push((id, pairs));
    }
    let rows = parsed
        .into_iter()
        .map(|(id, pairs)| {
            let mut row = vec![id];
            row.extend(fields.iter().map(|f| pairs.iter().find(|(k, _)| k == f).map(|(_, v)| v.clone()).unwrap_or(Value::Null)));
            row
        })
        .collect();
    let mut columns = vec!["id".to_string()];
    columns.extend(fields);
    (columns, rows)
}

fn arr(v: Rv) -> Vec<Rv> {
    match v {
        Rv::Array(a) | Rv::Set(a) => a,
        Rv::Map(m) => m.into_iter().flat_map(|(k, v)| [k, v]).collect(),
        Rv::Nil => vec![],
        other => vec![other],
    }
}

/// A scalar reply as text; `None` for nil and aggregates.
fn text(v: &Rv) -> Option<String> {
    Some(match v {
        Rv::BulkString(b) => String::from_utf8_lossy(b).into_owned(),
        Rv::SimpleString(s) => s.clone(),
        Rv::Okay => "OK".into(),
        Rv::Int(i) => i.to_string(),
        Rv::Double(d) => d.to_string(),
        Rv::Boolean(b) => b.to_string(),
        Rv::VerbatimString { text, .. } => text.clone(),
        Rv::BigNumber(n) => format!("{n:?}"),
        _ => return None,
    })
}

/// A reply as a grid cell; aggregates become JSON text.
fn to_value(v: &Rv) -> Value {
    match v {
        Rv::Nil => Value::Null,
        Rv::Int(i) => Value::Int(*i),
        Rv::Double(d) => Value::Float(*d),
        Rv::Boolean(b) => Value::Bool(*b),
        Rv::BulkString(b) => match std::str::from_utf8(b) {
            Ok(s) => Value::Text(s.to_string()),
            Err(_) => Value::Bytes(b.clone()),
        },
        Rv::Array(_) | Rv::Set(_) | Rv::Map(_) | Rv::Push { .. } | Rv::Attribute { .. } => Value::Text(to_json(v).to_string()),
        other => text(other).map(Value::Text).unwrap_or(Value::Null),
    }
}

fn to_json(v: &Rv) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Rv::Nil => J::Null,
        Rv::Int(i) => J::from(*i),
        Rv::Double(d) => J::from(*d),
        Rv::Boolean(b) => J::Bool(*b),
        Rv::Array(a) | Rv::Set(a) => J::Array(a.iter().map(to_json).collect()),
        Rv::Map(m) => J::Object(m.iter().map(|(k, v)| (text(k).unwrap_or_default(), to_json(v))).collect()),
        Rv::Push { data, .. } => J::Array(data.iter().map(to_json).collect()),
        Rv::Attribute { data, .. } => to_json(data),
        other => text(other).map(J::String).unwrap_or(J::Null),
    }
}

/// A command's reply as a result grid, shaped for what people run most:
/// field/value pairs for HGETALL and CONFIG GET, members and scores for
/// WITHSCORES, one row per item for lists, sections for INFO.
fn reply_result(args: &[String], reply: Rv) -> QueryResult {
    let cmd = args.first().map(|c| c.to_ascii_uppercase()).unwrap_or_default();
    let sub = args.get(1).map(|c| c.to_ascii_uppercase()).unwrap_or_default();
    let has = |w: &str| args.iter().any(|a| a.eq_ignore_ascii_case(w));
    let cols = |names: &[&str]| names.iter().map(|n| ResultColumn { name: n.to_string(), type_name: String::new() }).collect::<Vec<_>>();
    let pairs = |items: &[Rv]| items.chunks(2).map(|p| vec![to_value(&p[0]), p.get(1).map(to_value).unwrap_or(Value::Null)]).collect::<Vec<_>>();
    let mut r = QueryResult { statement: command_line(args), ..Default::default() };
    match reply {
        Rv::Map(m) => {
            r.columns = cols(&["field", "value"]);
            r.rows = m.iter().map(|(k, v)| vec![to_value(k), to_value(v)]).collect();
        }
        Rv::Array(items) | Rv::Set(items) => {
            let paired = matches!(cmd.as_str(), "HGETALL" | "HRANDFIELD" if cmd == "HGETALL" || has("WITHVALUES"))
                || (cmd == "CONFIG" && sub == "GET")
                || has("WITHSCORES");
            if matches!(cmd.as_str(), "SCAN" | "HSCAN" | "SSCAN" | "ZSCAN") && items.len() == 2 {
                let next = text(&items[0]).unwrap_or_default();
                let inner = arr(items[1].clone());
                (r.columns, r.rows) = match cmd.as_str() {
                    "HSCAN" => (cols(&["field", "value"]), pairs(&inner)),
                    "ZSCAN" => (cols(&["member", "score"]), pairs(&inner)),
                    "SSCAN" => (cols(&["member"]), inner.iter().map(|v| vec![to_value(v)]).collect()),
                    _ => (cols(&["key"]), inner.iter().map(|v| vec![to_value(v)]).collect()),
                };
                if next != "0" {
                    let mut more = vec![Value::Text(format!("… more: run again with cursor {next}"))];
                    more.resize(r.columns.len(), Value::Null);
                    r.rows.push(more);
                }
            } else if paired {
                let names: &[&str] = if has("WITHSCORES") { &["member", "score"] } else { &["field", "value"] };
                r.columns = cols(names);
                r.rows = pairs(&items);
            } else if matches!(cmd.as_str(), "XRANGE" | "XREVRANGE") {
                let (names, rows) = stream_rows(&items);
                r.columns = names.iter().map(|n| ResultColumn { name: n.clone(), type_name: String::new() }).collect();
                r.rows = rows;
            } else {
                r.columns = cols(&["#", "value"]);
                r.rows = items.iter().enumerate().map(|(i, v)| vec![Value::Int(i as i64 + 1), to_value(v)]).collect();
            }
        }
        reply => {
            let body = text(&reply);
            match (cmd.as_str(), body) {
                ("INFO", Some(body)) => {
                    r.columns = cols(&["section", "name", "value"]);
                    let mut section = String::new();
                    for line in body.lines().map(str::trim).filter(|l| !l.is_empty()) {
                        if let Some(s) = line.strip_prefix("# ") {
                            section = s.to_string();
                        } else if let Some((k, v)) = line.split_once(':') {
                            r.rows.push(vec![Value::Text(section.clone()), Value::Text(k.into()), Value::Text(v.into())]);
                        }
                    }
                }
                ("CLIENT", Some(body)) if sub == "LIST" => (r.columns, r.rows) = client_list(&body),
                _ => {
                    r.columns = cols(&["result"]);
                    r.rows = vec![vec![to_value(&reply)]];
                }
            }
        }
    }
    r
}

/// `CLIENT LIST` lines ("id=3 addr=… cmd=client|list") as rows.
fn client_list(body: &str) -> (Vec<ResultColumn>, Vec<Vec<Value>>) {
    const SHOWN: [&str; 8] = ["id", "addr", "name", "user", "db", "cmd", "age", "idle"];
    let columns = SHOWN.iter().map(|n| ResultColumn { name: n.to_string(), type_name: String::new() }).collect();
    let rows = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: HashMap<&str, &str> = line.split_whitespace().filter_map(|kv| kv.split_once('=')).collect();
            SHOWN
                .iter()
                .map(|k| match fields.get(k) {
                    Some(v) if matches!(*k, "id" | "db" | "age" | "idle") => v.parse().map(Value::Int).unwrap_or_else(|_| Value::Text(v.to_string())),
                    Some(v) => Value::Text(v.to_string()),
                    None => Value::Null,
                })
                .collect()
        })
        .collect();
    (columns, rows)
}

fn get<'a>(values: &'a ColumnValues, column: &str) -> Option<&'a Value> {
    values.iter().find(|(c, _)| c == column).map(|(_, v)| v)
}

/// A cell as a Redis argument. Redis has no NULL.
fn arg(v: Option<&Value>, what: &str) -> Result<String> {
    match v {
        None | Some(Value::Null) => bail!("{what} cannot be NULL in Redis"),
        Some(Value::Bytes(b)) => Ok(String::from_utf8_lossy(b).into_owned()),
        Some(v) => Ok(v.display()),
    }
}

/// Commands that make staged grid edits of `key`, a `kind` key.
fn edit_commands(key: &str, kind: ItemKind, changes: &[RowChange]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let line = |parts: &[&str]| command_line(parts);
    // List deletions mark items first and remove the marks at the end, so
    // positions of other staged changes stay valid.
    let tombstone = format!("__quarry_deleted_{}__", uuid::Uuid::new_v4().simple());
    let mut list_deletes = false;
    let mut later = Vec::new();
    for change in changes {
        match (kind, change) {
            (ItemKind::StringKey, RowChange::Update { values, .. } | RowChange::Insert { values }) => {
                out.push(line(&["SET", key, &arg(get(values, "value"), "value")?, "KEEPTTL"]))
            }
            (ItemKind::StringKey, RowChange::Delete { .. }) => out.push(line(&["DEL", key])),

            (ItemKind::HashKey, RowChange::Insert { values }) => {
                let value = get(values, "value").map(Value::display).unwrap_or_default();
                out.push(line(&["HSET", key, &arg(get(values, "field"), "field")?, &value]));
            }
            (ItemKind::HashKey, RowChange::Update { key: old, values }) => {
                let field = arg(get(old, "field"), "field")?;
                let new_field = get(values, "field").map(|v| arg(Some(v), "field")).transpose()?.unwrap_or_else(|| field.clone());
                let value = arg(get(values, "value").or(get(old, "value")), "value")?;
                if new_field != field {
                    out.push(line(&["HDEL", key, &field]));
                }
                out.push(line(&["HSET", key, &new_field, &value]));
            }
            (ItemKind::HashKey, RowChange::Delete { key: old }) => out.push(line(&["HDEL", key, &arg(get(old, "field"), "field")?])),

            (ItemKind::ListKey, RowChange::Insert { values }) => {
                later.push(line(&["RPUSH", key, &get(values, "value").map(Value::display).unwrap_or_default()]))
            }
            (ItemKind::ListKey, RowChange::Update { key: old, values }) => {
                if get(values, "index").is_some() {
                    bail!("list positions cannot be edited; change the value, or delete the item and add a new one");
                }
                out.push(line(&["LSET", key, &arg(get(old, "index"), "index")?, &arg(get(values, "value"), "value")?]));
            }
            (ItemKind::ListKey, RowChange::Delete { key: old }) => {
                out.push(line(&["LSET", key, &arg(get(old, "index"), "index")?, &tombstone]));
                list_deletes = true;
            }

            (ItemKind::SetKey, RowChange::Insert { values }) => out.push(line(&["SADD", key, &arg(get(values, "member"), "member")?])),
            (ItemKind::SetKey, RowChange::Update { key: old, values }) => {
                out.push(line(&["SREM", key, &arg(get(old, "member"), "member")?]));
                out.push(line(&["SADD", key, &arg(get(values, "member"), "member")?]));
            }
            (ItemKind::SetKey, RowChange::Delete { key: old }) => out.push(line(&["SREM", key, &arg(get(old, "member"), "member")?])),

            (ItemKind::ZSetKey, RowChange::Insert { values }) => {
                let score = get(values, "score").map(Value::display).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "0".into());
                out.push(line(&["ZADD", key, &score, &arg(get(values, "member"), "member")?]));
            }
            (ItemKind::ZSetKey, RowChange::Update { key: old, values }) => {
                let member = arg(get(old, "member"), "member")?;
                let new_member = get(values, "member").map(|v| arg(Some(v), "member")).transpose()?.unwrap_or_else(|| member.clone());
                let score = arg(get(values, "score").or(get(old, "score")), "score")?;
                if new_member != member {
                    out.push(line(&["ZREM", key, &member]));
                }
                out.push(line(&["ZADD", key, &score, &new_member]));
            }
            (ItemKind::ZSetKey, RowChange::Delete { key: old }) => out.push(line(&["ZREM", key, &arg(get(old, "member"), "member")?])),

            (ItemKind::StreamKey, RowChange::Insert { values }) => {
                let id = get(values, "id").map(Value::display).filter(|s| !s.trim().is_empty() && s != "NULL").unwrap_or_else(|| "*".into());
                let mut parts = vec!["XADD".to_string(), key.to_string(), id];
                for (field, v) in values.iter().filter(|(c, v)| c != "id" && !matches!(v, Value::Null)) {
                    parts.push(field.clone());
                    parts.push(v.display());
                }
                if parts.len() < 5 {
                    bail!("a stream entry needs at least one field");
                }
                out.push(command_line(&parts));
            }
            (ItemKind::StreamKey, RowChange::Delete { key: old }) => out.push(line(&["XDEL", key, &arg(get(old, "id"), "id")?])),
            (ItemKind::StreamKey, RowChange::Update { .. }) => bail!("stream entries cannot be changed; delete the entry and add a new one"),

            _ => bail!("Quarry cannot edit this type of key; use the command editor"),
        }
    }
    out.extend(later);
    if list_deletes {
        out.push(line(&["LREM", key, "0", &tombstone]));
    }
    Ok(out)
}

/// Commands that recreate a key, for "Show DDL" and copying a key elsewhere.
fn recreate_commands(key: &str, kind: ItemKind, columns: &[String], rows: &[Vec<Value>]) -> Vec<String> {
    let s = |v: &Value| v.display();
    let chunks = rows.chunks(50);
    let mut out = Vec::new();
    match kind {
        ItemKind::StringKey => out.extend(rows.first().map(|r| command_line(&["SET".to_string(), key.into(), s(&r[0])]))),
        ItemKind::JsonKey => out.extend(rows.first().map(|r| command_line(&["JSON.SET".to_string(), key.into(), "$".into(), s(&r[0])]))),
        ItemKind::HashKey | ItemKind::ListKey | ItemKind::SetKey | ItemKind::ZSetKey => {
            for chunk in chunks {
                let mut parts = vec![
                    match kind {
                        ItemKind::HashKey => "HSET",
                        ItemKind::ListKey => "RPUSH",
                        ItemKind::SetKey => "SADD",
                        _ => "ZADD",
                    }
                    .to_string(),
                    key.to_string(),
                ];
                for r in chunk {
                    match kind {
                        ItemKind::HashKey => parts.extend([s(&r[0]), s(&r[1])]),
                        ItemKind::ListKey => parts.push(s(&r[1])),
                        ItemKind::SetKey => parts.push(s(&r[0])),
                        _ => parts.extend([s(&r[1]), s(&r[0])]),
                    }
                }
                out.push(command_line(&parts));
            }
        }
        ItemKind::StreamKey => {
            for r in rows {
                let mut parts = vec!["XADD".to_string(), key.to_string(), s(&r[0])];
                for (c, v) in columns.iter().zip(r).skip(1).filter(|(_, v)| !matches!(v, Value::Null)) {
                    parts.extend([c.clone(), s(v)]);
                }
                out.push(command_line(&parts));
            }
        }
        _ => {}
    }
    out
}

#[async_trait]
impl Driver for RedisDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::Redis
    }

    async fn server_version(&self) -> Result<String> {
        let info = text(&self.run(&["INFO", "server"]).await?).unwrap_or_default();
        let field = |name: &str| info.lines().find_map(|l| l.strip_prefix(name).and_then(|v| v.strip_prefix(':')).map(|v| v.trim().to_string()));
        // Valkey also reports a redis_version for compatibility.
        Ok(match (field("valkey_version"), field("redis_version")) {
            (Some(v), _) => format!("{v} (Valkey)"),
            (None, Some(v)) => v,
            _ => String::new(),
        })
    }

    async fn current_database(&self) -> Result<String> {
        Ok(format!("db{}", self.db))
    }

    async fn list_databases(&self) -> Result<Vec<String>> {
        // Managed services may forbid CONFIG; 16 is Redis's default.
        let n = match self.run(&["CONFIG", "GET", "databases"]).await.map(arr) {
            Ok(v) => v.get(1).and_then(text).and_then(|s| s.parse::<i64>().ok()).unwrap_or(16),
            Err(_) => 16,
        };
        Ok((0..n.max(1)).map(|i| format!("db{i}")).collect())
    }

    async fn use_database(&self, _name: &str) -> Result<()> {
        // A fresh connection keeps the database number in its settings.
        bail!("Redis switches database on a new connection")
    }

    async fn list_schemas(&self, _include_system: bool) -> Result<Vec<String>> {
        Ok(vec![])
    }

    async fn list_items(&self, _schema: Option<&str>) -> Result<Vec<Item>> {
        let mut keys: Vec<String> = self.scan_all(&["SCAN", "{cursor}", "COUNT", "1000"], MAX_KEYS).await?.iter().filter_map(text).collect();
        keys.sort();
        keys.dedup();
        keys.truncate(MAX_KEYS);
        let mut types = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(500) {
            let mut pipe = redis::pipe();
            for k in chunk {
                pipe.cmd("TYPE").arg(k);
            }
            let replies: Vec<String> = pipe.query_async(&mut self.conn.clone()).await?;
            types.extend(replies);
        }
        let mut cache = self.types.lock().unwrap();
        Ok(keys
            .into_iter()
            .zip(types)
            .filter(|(_, t)| t != "none")
            .map(|(key, t)| {
                let kind = kind_of(&t);
                cache.insert(key.clone(), kind);
                Item { table: TableRef::new(None, &key), kind, comment: None }
            })
            .collect())
    }

    async fn structure(&self, table: &TableRef) -> Result<TableStructure> {
        let key = &table.name;
        let kind = self.key_type(key).await?;
        let col = |name: &str, ty: &str| ColumnInfo {
            name: name.into(),
            data_type: ty.into(),
            nullable: false,
            default: None,
            // No primary key: staged edits then carry the whole original
            // row, which renaming a field or member needs.
            primary_key: false,
            extra: None,
            comment: None,
            references: None,
        };
        let columns = match kind {
            ItemKind::StringKey => vec![col("value", "string")],
            ItemKind::HashKey => vec![col("field", "string"), col("value", "string")],
            ItemKind::ListKey => vec![col("index", "integer"), col("value", "string")],
            ItemKind::SetKey => vec![col("member", "string")],
            ItemKind::ZSetKey => vec![col("member", "string"), col("score", "double")],
            ItemKind::StreamKey => {
                let sample = arr(self.run(&["XRANGE", key, "-", "+", "COUNT", "100"]).await?);
                let (names, _) = stream_rows(&sample);
                names.iter().map(|n| col(n, if n == "id" { "stream id" } else { "string" })).collect()
            }
            ItemKind::JsonKey => vec![col("value", "json")],
            _ => vec![col("value", "unknown")],
        };
        Ok(TableStructure { table: table.clone(), columns, indexes: vec![], foreign_keys: vec![] })
    }

    async fn all_columns(&self, _schema: Option<&str>) -> Result<Vec<(String, String, String)>> {
        // Keys are not tables: their "columns" help no autocomplete, and
        // reading every key's type would take a round trip each.
        Ok(vec![])
    }

    async fn ddl(&self, item: &Item) -> Result<String> {
        let key = &item.table.name;
        let kind = self.key_type(key).await?;
        let (columns, rows) = self.elements(key, kind).await?;
        let ttl = text(&self.run(&["TTL", key]).await?).and_then(|s| s.parse::<i64>().ok()).unwrap_or(-1);
        let encoding = self.run(&["OBJECT", "ENCODING", key]).await.ok().and_then(|v| text(&v)).unwrap_or_default();
        let bytes = self.run(&["MEMORY", "USAGE", key]).await.ok().and_then(|v| text(&v));
        let mut out = vec![format!(
            "# {} \"{}\": {} {}, encoding {encoding}{}, {}",
            kind.section().trim_end_matches('s').to_lowercase(),
            key,
            rows.len(),
            if rows.len() == 1 { "element" } else { "elements" },
            bytes.map(|b| format!(", {b} bytes")).unwrap_or_default(),
            if ttl >= 0 { format!("expires in {ttl} s") } else { "no expiry".into() }
        )];
        out.push(command_line(&["DEL", key]));
        let shown = &rows[..rows.len().min(DDL_ELEMENTS)];
        out.extend(recreate_commands(key, kind, &columns, shown));
        if rows.len() > shown.len() {
            out.push(format!("# … {} more elements not shown", rows.len() - shown.len()));
        }
        if ttl >= 0 {
            out.push(command_line(&["EXPIRE", key, &ttl.to_string()]));
        }
        Ok(out.join("\n"))
    }

    async fn execute(&self, statement: &str) -> Result<QueryResult> {
        let start = Instant::now();
        let args = split_args(statement).map_err(|e| anyhow!(e))?;
        let Some(cmd) = args.first().map(|c| c.to_ascii_uppercase()) else { bail!("empty command") };
        match cmd.as_str() {
            "SELECT" => bail!("switch databases with the database list (the database name in the title bar, or Ctrl+K)"),
            "MULTI" | "EXEC" | "DISCARD" | "WATCH" | "UNWATCH" => {
                bail!("transactions need one connection per editor; Quarry applies grid edits in MULTI/EXEC itself")
            }
            "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" | "MONITOR" | "SYNC" | "PSYNC" => bail!("{cmd} streams forever; use redis-cli for it"),
            _ => {}
        }
        let reply = self.run(&args).await?;
        let mut r = reply_result(&args, reply);
        r.duration_ms = start.elapsed().as_millis() as u64;
        Ok(r)
    }

    async fn execute_batch(&self, statements: &[String]) -> Result<()> {
        let mut pipe = redis::pipe();
        pipe.atomic();
        for s in statements {
            let args = split_args(s).map_err(|e| anyhow!(e))?;
            let Some((name, rest)) = args.split_first() else { continue };
            let cmd = pipe.cmd(name);
            for a in rest {
                cmd.arg(a);
            }
        }
        let replies: Rv = pipe.query_async(&mut self.conn.clone()).await?;
        // EXEC runs every command; one that failed does not undo the others.
        for (s, reply) in statements.iter().zip(arr(replies)) {
            if let Rv::ServerError(e) = reply {
                bail!("{s}: {e:?}");
            }
        }
        Ok(())
    }

    async fn browse(&self, req: &BrowseRequest) -> Option<Result<QueryResult>> {
        Some(
            async {
                let start = Instant::now();
                let key = &req.table.name;
                let kind = self.key_type(key).await?;
                let (columns, rows) = self.elements(key, kind).await?;
                let rows = filter_sort_page(&columns, rows, req);
                Ok(QueryResult {
                    statement: format!("# {} {key}", kind.section()),
                    columns: columns.iter().map(|c| ResultColumn { name: c.clone(), type_name: String::new() }).collect(),
                    rows,
                    rows_affected: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            .await,
        )
    }

    async fn count(&self, table: &TableRef, filters: &[Filter]) -> Option<Result<u64>> {
        Some(
            async {
                let key = &table.name;
                let kind = self.key_type(key).await?;
                if filters.iter().all(|f| !f.enabled) {
                    let len = match kind {
                        ItemKind::HashKey => "HLEN",
                        ItemKind::ListKey => "LLEN",
                        ItemKind::SetKey => "SCARD",
                        ItemKind::ZSetKey => "ZCARD",
                        ItemKind::StreamKey => "XLEN",
                        _ => return Ok(1),
                    };
                    return Ok(text(&self.run(&[len, key]).await?).and_then(|s| s.parse().ok()).unwrap_or(0));
                }
                let (columns, rows) = self.elements(key, kind).await?;
                Ok(count_matching(&columns, &rows, filters))
            }
            .await,
        )
    }

    fn changes_text(&self, table: &TableRef, changes: &[RowChange], _key: KeyKind) -> Option<Result<Vec<String>>> {
        Some(self.cached_kind(&table.name).and_then(|kind| edit_commands(&table.name, kind, changes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(pairs: &[(&str, &str)]) -> ColumnValues {
        pairs.iter().map(|(c, v)| (c.to_string(), Value::Text(v.to_string()))).collect()
    }

    #[test]
    fn grid_edits_become_commands() {
        let hash = edit_commands(
            "user:1",
            ItemKind::HashKey,
            &[
                RowChange::Update { key: kv(&[("field", "name"), ("value", "Ann")]), values: kv(&[("value", "Ann Lee")]) },
                RowChange::Update { key: kv(&[("field", "mail"), ("value", "a@x")]), values: kv(&[("field", "email")]) },
                RowChange::Insert { values: kv(&[("field", "age"), ("value", "42")]) },
                RowChange::Delete { key: kv(&[("field", "old"), ("value", "x")]) },
            ],
        )
        .unwrap();
        assert_eq!(hash, vec!["HSET user:1 name \"Ann Lee\"", "HDEL user:1 mail", "HSET user:1 email a@x", "HSET user:1 age 42", "HDEL user:1 old"]);

        let list = edit_commands(
            "queue",
            ItemKind::ListKey,
            &[
                RowChange::Delete { key: vec![("index".into(), Value::Int(0)), ("value".into(), "a".into())] },
                RowChange::Insert { values: kv(&[("value", "z")]) },
                RowChange::Delete { key: vec![("index".into(), Value::Int(2)), ("value".into(), "c".into())] },
            ],
        )
        .unwrap();
        assert!(list[0].starts_with("LSET queue 0 __quarry_deleted_") && list[1].starts_with("LSET queue 2 __quarry_deleted_"), "{list:?}");
        assert_eq!(list[2], "RPUSH queue z");
        assert!(list[3].starts_with("LREM queue 0 __quarry_deleted_"), "marks removed last: {list:?}");

        let z = edit_commands("board", ItemKind::ZSetKey, &[RowChange::Update { key: vec![("member".into(), "ann".into()), ("score".into(), Value::Float(1.5))], values: kv(&[("member", "anne")]) }]).unwrap();
        assert_eq!(z, vec!["ZREM board ann", "ZADD board 1.5 anne"]);
        assert!(edit_commands("s", ItemKind::StreamKey, &[RowChange::Update { key: kv(&[("id", "1-0")]), values: kv(&[("a", "b")]) }]).is_err());
        assert!(edit_commands("h", ItemKind::HashKey, &[RowChange::Insert { values: vec![("field".into(), Value::Null)] }]).is_err(), "no NULL in Redis");
    }

    #[test]
    fn replies_become_grids() {
        let args: Vec<String> = ["HGETALL", "h"].iter().map(|s| s.to_string()).collect();
        let r = reply_result(&args, Rv::Array(vec![Rv::BulkString(b"a".to_vec()), Rv::BulkString(b"1".to_vec())]));
        assert_eq!(r.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["field", "value"]);
        assert_eq!(r.rows, vec![vec![Value::Text("a".into()), Value::Text("1".into())]]);
        let args: Vec<String> = ["INFO", "server"].iter().map(|s| s.to_string()).collect();
        let r = reply_result(&args, Rv::BulkString(b"# Server\r\nredis_version:8.0.0\r\nos:Linux\r\n".to_vec()));
        assert_eq!(r.rows[0], vec![Value::Text("Server".into()), Value::Text("redis_version".into()), Value::Text("8.0.0".into())]);
        let args: Vec<String> = ["SCAN", "0"].iter().map(|s| s.to_string()).collect();
        let r = reply_result(&args, Rv::Array(vec![Rv::BulkString(b"17".to_vec()), Rv::Array(vec![Rv::BulkString(b"k".to_vec())])]));
        assert_eq!(r.rows.len(), 2, "keys plus the next cursor");
        let args: Vec<String> = ["GET", "k"].iter().map(|s| s.to_string()).collect();
        assert_eq!(reply_result(&args, Rv::Nil).rows, vec![vec![Value::Null]]);
    }

    #[test]
    fn database_names() {
        assert_eq!(db_index("db3"), 3);
        assert_eq!(db_index("3"), 3);
        assert_eq!(db_index(""), 0);
    }
}
