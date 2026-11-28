//! The part of mongosh's language Quarry's editor runs:
//! `db.<collection>.<method>(…)` calls with chained cursor methods
//! (`.sort()`, `.limit()`, …), database methods (`db.runCommand(…)`),
//! `show dbs` / `show collections`, and JavaScript literals with the shell's
//! helpers: `ObjectId()`, `ISODate()`, `NumberLong()`, regex literals and
//! extended JSON (`{"$oid": …}`).

use std::str::FromStr as _;

use mongodb::bson::{self, Bson, Document, oid::ObjectId};

use crate::sql::Statement;

/// What a statement calls methods on.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Database,
    Collection(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub name: String,
    pub args: Vec<Bson>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// `show dbs`, `show collections`.
    Show(String),
    /// `use name`.
    Use(String),
    /// The method, then chained cursor methods.
    Calls { target: Target, calls: Vec<Call> },
}

impl Command {
    pub fn method(&self) -> &str {
        match self {
            Command::Calls { calls, .. } => calls.first().map(|c| c.name.as_str()).unwrap_or_default(),
            _ => "",
        }
    }

    /// A chained call, such as `limit` of `find().limit(10)`.
    pub fn chained(&self, name: &str) -> Option<&Call> {
        match self {
            Command::Calls { calls, .. } => calls.iter().skip(1).find(|c| c.name == name),
            _ => None,
        }
    }
}

pub fn parse(statement: &str) -> Result<Command, String> {
    let mut p = Parser { s: statement.trim().trim_end_matches(';').trim().chars().collect(), i: 0 };
    let cmd = p.command()?;
    p.ws();
    if p.i < p.s.len() {
        return Err(format!("unexpected {:?} after the command", p.rest(20)));
    }
    Ok(cmd)
}

/// One value in shell syntax: `{a: 1}`, `[1, 2]`, `ObjectId("…")`, `"text"`.
pub fn parse_value(text: &str) -> Result<Bson, String> {
    let mut p = Parser { s: text.trim().chars().collect(), i: 0 };
    let v = p.value()?;
    p.ws();
    if p.i < p.s.len() {
        return Err(format!("unexpected {:?} after the value", p.rest(20)));
    }
    Ok(v)
}

struct Parser {
    s: Vec<char>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn rest(&self, n: usize) -> String {
        self.s[self.i..].iter().take(n).collect()
    }

    /// Skips spaces and comments.
    fn ws(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.i += 1;
            }
            if self.rest(2) == "//" {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
            } else if self.rest(2) == "/*" {
                self.i += 2;
                while self.i < self.s.len() && self.rest(2) != "*/" {
                    self.i += 1;
                }
                self.i = (self.i + 2).min(self.s.len());
            } else {
                return;
            }
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.ws();
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.eat(c) { Ok(()) } else { Err(format!("expected {c:?} at {:?}", self.rest(20))) }
    }

    fn ident(&mut self) -> Option<String> {
        self.ws();
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$') {
            self.i += 1;
        }
        (self.i > start).then(|| self.s[start..self.i].iter().collect())
    }

    fn command(&mut self) -> Result<Command, String> {
        let first = self.ident().ok_or("expected db.<collection>.<method>(…), show or use")?;
        match first.as_str() {
            "show" => return Ok(Command::Show(self.ident().ok_or("show what? (dbs or collections)")?.to_lowercase())),
            "use" => return Ok(Command::Use(self.ident().ok_or("use which database?")?)),
            "db" => {}
            other => return Err(format!("statements start with db., show or use, not {other:?}")),
        }
        let mut target = Target::Database;
        let mut calls = Vec::new();
        // db["name"] names a collection with any characters.
        if self.eat('[') {
            let Bson::String(name) = self.value()? else { return Err("db[…] takes a collection name".into()) };
            self.expect(']')?;
            target = Target::Collection(name);
        }
        while self.eat('.') {
            let name = self.ident().ok_or("expected a name after '.'")?;
            if self.eat('(') {
                let args = self.list(')')?;
                if target == Target::Database && calls.is_empty() && matches!(name.as_str(), "getCollection" | "getSiblingDB") {
                    if name == "getSiblingDB" {
                        return Err("switch databases with the database list instead of getSiblingDB".into());
                    }
                    let Some(Bson::String(c)) = args.into_iter().next() else { return Err("getCollection takes a name".into()) };
                    target = Target::Collection(c);
                    continue;
                }
                calls.push(Call { name, args });
            } else if target == Target::Database && calls.is_empty() {
                target = Target::Collection(name);
            } else if let Target::Collection(c) = &mut target {
                // db.logs.app.find(): collection names may have dots.
                if calls.is_empty() {
                    c.push('.');
                    c.push_str(&name);
                } else {
                    return Err(format!("{name} is not a method call"));
                }
            }
        }
        if calls.is_empty() {
            return Err("expected a method call, such as db.users.find()".into());
        }
        Ok(Command::Calls { target, calls })
    }

    /// Comma-separated values up to `close` (trailing comma allowed).
    fn list(&mut self, close: char) -> Result<Vec<Bson>, String> {
        let mut out = Vec::new();
        loop {
            if self.eat(close) {
                return Ok(out);
            }
            out.push(self.value()?);
            if !self.eat(',') {
                self.expect(close)?;
                return Ok(out);
            }
        }
    }

    fn value(&mut self) -> Result<Bson, String> {
        self.ws();
        match self.peek() {
            None => Err("expected a value".into()),
            Some('{') => {
                self.i += 1;
                let mut doc = Document::new();
                loop {
                    if self.eat('}') {
                        break;
                    }
                    self.ws();
                    let key = match self.peek() {
                        Some('"' | '\'') => self.string()?,
                        _ => self.ident().or_else(|| self.number_text()).ok_or_else(|| format!("expected a key at {:?}", self.rest(20)))?,
                    };
                    self.expect(':')?;
                    let v = self.value()?;
                    doc.insert(key, v);
                    if !self.eat(',') {
                        self.expect('}')?;
                        break;
                    }
                }
                Ok(from_extended(doc))
            }
            Some('[') => {
                self.i += 1;
                Ok(Bson::Array(self.list(']')?))
            }
            Some('"' | '\'') => Ok(Bson::String(self.string()?)),
            Some('/') => self.regex(),
            Some(c) if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() => {
                let text = self.number_text().ok_or("expected a number")?;
                number(&text)
            }
            Some(_) => {
                let word = self.ident().ok_or_else(|| format!("unexpected {:?}", self.rest(20)))?;
                self.word(&word)
            }
        }
    }

    fn number_text(&mut self) -> Option<String> {
        self.ws();
        let start = self.i;
        if matches!(self.peek(), Some('-' | '+')) {
            self.i += 1;
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || ((c == '-' || c == '+') && matches!(self.s.get(self.i - 1), Some('e' | 'E')))) {
            self.i += 1;
        }
        (self.i > start).then(|| self.s[start..self.i].iter().collect())
    }

    fn string(&mut self) -> Result<String, String> {
        let quote = self.peek().ok_or("expected a string")?;
        self.i += 1;
        let mut out = String::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            if c == quote {
                return Ok(out);
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let Some(e) = self.peek() else { break };
            self.i += 1;
            match e {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                '0' => out.push('\0'),
                'u' => {
                    let hex: String = self.s[self.i..].iter().take(4).collect();
                    self.i += 4;
                    let code = u32::from_str_radix(&hex, 16).map_err(|_| format!("bad \\u escape {hex:?}"))?;
                    // Surrogate pairs, as JSON writes characters outside the BMP.
                    if (0xD800..0xDC00).contains(&code) && self.rest(2) == "\\u" {
                        let low: String = self.s[self.i + 2..].iter().take(4).collect();
                        if let Ok(low) = u32::from_str_radix(&low, 16) {
                            self.i += 6;
                            out.extend(char::from_u32(0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)));
                            continue;
                        }
                    }
                    out.extend(char::from_u32(code));
                }
                other => out.push(other),
            }
        }
        Err("unterminated string".into())
    }

    fn regex(&mut self) -> Result<Bson, String> {
        self.i += 1;
        let mut pattern = String::new();
        let mut class = false;
        loop {
            let c = self.peek().ok_or("unterminated regular expression")?;
            self.i += 1;
            match c {
                '\\' => {
                    pattern.push(c);
                    if let Some(n) = self.peek() {
                        pattern.push(n);
                        self.i += 1;
                    }
                }
                '[' => {
                    class = true;
                    pattern.push(c);
                }
                ']' => {
                    class = false;
                    pattern.push(c);
                }
                '/' if !class => break,
                c => pattern.push(c),
            }
        }
        let mut options = String::new();
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            options.push(self.peek().unwrap());
            self.i += 1;
        }
        let mut sorted: Vec<char> = options.chars().collect();
        sorted.sort();
        Ok(Bson::RegularExpression(bson::Regex { pattern, options: sorted.into_iter().collect() }))
    }

    /// Keywords and shell helpers.
    fn word(&mut self, word: &str) -> Result<Bson, String> {
        match word {
            "true" => return Ok(Bson::Boolean(true)),
            "false" => return Ok(Bson::Boolean(false)),
            "null" | "undefined" => return Ok(Bson::Null),
            "NaN" => return Ok(Bson::Double(f64::NAN)),
            "Infinity" => return Ok(Bson::Double(f64::INFINITY)),
            "new" => {
                let next = self.ident().ok_or("expected a name after new")?;
                return self.word(&next);
            }
            _ => {}
        }
        self.expect('(')?;
        let args = self.list(')')?;
        let text = |i: usize| -> Option<String> {
            match args.get(i) {
                Some(Bson::String(s)) => Some(s.clone()),
                Some(Bson::Int32(n)) => Some(n.to_string()),
                Some(Bson::Int64(n)) => Some(n.to_string()),
                Some(Bson::Double(n)) => Some(n.to_string()),
                _ => None,
            }
        };
        match word {
            "ObjectId" => match text(0) {
                Some(hex) => ObjectId::parse_str(&hex).map(Bson::ObjectId).map_err(|e| format!("ObjectId({hex:?}): {e}")),
                None => Ok(Bson::ObjectId(ObjectId::new())),
            },
            "ISODate" | "Date" => match (args.first(), text(0)) {
                (None, _) => Ok(Bson::DateTime(bson::DateTime::now())),
                (Some(Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)), Some(ms)) => Ok(Bson::DateTime(bson::DateTime::from_millis(ms.parse::<f64>().unwrap_or(0.0) as i64))),
                (_, Some(s)) => parse_date(&s).map(Bson::DateTime),
                _ => Err(format!("{word} takes a date string")),
            },
            "NumberLong" | "Long" => text(0).and_then(|s| s.parse::<i64>().ok()).map(Bson::Int64).ok_or_else(|| "NumberLong takes an integer".into()),
            "NumberInt" | "Int32" => text(0).and_then(|s| s.parse::<i32>().ok()).map(Bson::Int32).ok_or_else(|| "NumberInt takes an integer".into()),
            "Double" | "Number" => text(0).and_then(|s| s.parse::<f64>().ok()).map(Bson::Double).ok_or_else(|| format!("{word} takes a number")),
            "NumberDecimal" | "Decimal128" => text(0)
                .and_then(|s| bson::Decimal128::from_str(&s).ok())
                .map(Bson::Decimal128)
                .ok_or_else(|| "NumberDecimal takes a decimal string".into()),
            "UUID" => match text(0) {
                Some(s) => {
                    let hex: String = s.chars().filter(|c| *c != '-').collect();
                    let bytes = (0..hex.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("zz"), 16))
                        .collect::<Result<Vec<u8>, _>>()
                        .ok()
                        .filter(|b| b.len() == 16)
                        .ok_or_else(|| format!("UUID({s:?}) is not a UUID"))?;
                    Ok(Bson::Binary(bson::Binary { subtype: bson::spec::BinarySubtype::Uuid, bytes }))
                }
                None => Ok(Bson::Binary(bson::Binary { subtype: bson::spec::BinarySubtype::Uuid, bytes: uuid::Uuid::new_v4().as_bytes().to_vec() })),
            },
            "BinData" => {
                let subtype = text(0).and_then(|s| s.parse::<u8>().ok()).unwrap_or(0);
                let data = text(1).ok_or("BinData takes a subtype and base64 data")?;
                let ext = serde_json::json!({"$binary": {"base64": data, "subType": format!("{subtype:02x}")}});
                Bson::try_from(ext).map_err(|e| format!("BinData: {e}"))
            }
            "Timestamp" => {
                let t = text(0).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                let i = text(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                Ok(Bson::Timestamp(bson::Timestamp { time: t, increment: i }))
            }
            "MinKey" => Ok(Bson::MinKey),
            "MaxKey" => Ok(Bson::MaxKey),
            "RegExp" => Ok(Bson::RegularExpression(bson::Regex { pattern: text(0).unwrap_or_default(), options: text(1).unwrap_or_default() })),
            other => Err(format!("unknown function {other}()")),
        }
    }
}

fn number(text: &str) -> Result<Bson, String> {
    let t = text.trim_start_matches('+');
    if let Some(hex) = t.strip_prefix("0x") {
        return i64::from_str_radix(hex, 16).map(Bson::Int64).map_err(|e| e.to_string());
    }
    let whole = !t.contains(['.', 'e', 'E']);
    if whole {
        if let Ok(n) = t.parse::<i32>() {
            return Ok(Bson::Int32(n));
        }
        if let Ok(n) = t.parse::<i64>() {
            return Ok(Bson::Int64(n));
        }
    }
    t.parse::<f64>().map(Bson::Double).map_err(|_| format!("{text:?} is not a number"))
}

/// "2024-05-06", "2024-05-06T07:08:09Z" or with an offset.
pub fn parse_date(s: &str) -> Result<bson::DateTime, String> {
    let s = s.trim();
    let full = if s.len() == 10 { format!("{s}T00:00:00Z") } else if !s.ends_with('Z') && !s[10..].contains(['+', '-']) { format!("{s}Z") } else { s.to_string() };
    bson::DateTime::parse_rfc3339_str(&full).map_err(|e| format!("{s:?} is not a date: {e}"))
}

/// `{"$oid": "…"}` and the other extended JSON wrappers become their types.
fn from_extended(doc: Document) -> Bson {
    const WRAPPERS: [&str; 14] = [
        "$oid", "$date", "$numberLong", "$numberInt", "$numberDouble", "$numberDecimal", "$binary", "$uuid", "$timestamp",
        "$regularExpression", "$minKey", "$maxKey", "$symbol", "$code",
    ];
    if doc.len() == 1 && doc.keys().next().is_some_and(|k| WRAPPERS.contains(&k.as_str())) {
        let json = Bson::Document(doc.clone()).into_relaxed_extjson();
        if let Ok(b) = Bson::try_from(json) {
            if !matches!(b, Bson::Document(_)) {
                return b;
            }
        }
    }
    Bson::Document(doc)
}

/// A value as mongosh would print it, which [`parse`] reads back.
pub fn to_shell(b: &Bson) -> String {
    let json = |s: &str| serde_json::to_string(s).unwrap_or_default();
    match b {
        Bson::Document(d) => {
            if d.is_empty() {
                return "{}".into();
            }
            let fields: Vec<String> = d.iter().map(|(k, v)| format!("{}: {}", json(k), to_shell(v))).collect();
            format!("{{{}}}", fields.join(", "))
        }
        Bson::Array(a) => format!("[{}]", a.iter().map(to_shell).collect::<Vec<_>>().join(", ")),
        Bson::String(s) => json(s),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => format!("NumberLong({n})"),
        Bson::Double(f) if f.is_nan() => "NaN".into(),
        Bson::Double(f) if f.is_infinite() => if *f > 0.0 { "Infinity" } else { "-Infinity" }.into(),
        // A decimal point keeps it a double when read back.
        Bson::Double(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{f:.1}"),
        Bson::Double(f) => f.to_string(),
        Bson::Boolean(v) => v.to_string(),
        Bson::Null | Bson::Undefined => "null".into(),
        Bson::ObjectId(o) => format!("ObjectId({})", json(&o.to_hex())),
        Bson::DateTime(d) => format!("ISODate({})", json(&d.try_to_rfc3339_string().unwrap_or_default())),
        Bson::Decimal128(d) => format!("NumberDecimal({})", json(&d.to_string())),
        Bson::Binary(bin) if bin.subtype == bson::spec::BinarySubtype::Uuid && bin.bytes.len() == 16 => {
            format!("UUID({})", json(&uuid::Uuid::from_slice(&bin.bytes).map(|u| u.to_string()).unwrap_or_default()))
        }
        Bson::RegularExpression(r) => format!("/{}/{}", r.pattern.replace('/', "\\/"), r.options),
        Bson::Timestamp(t) => format!("Timestamp({}, {})", t.time, t.increment),
        Bson::MinKey => "MinKey()".into(),
        Bson::MaxKey => "MaxKey()".into(),
        other => other.clone().into_relaxed_extjson().to_string(),
    }
}

/// [`to_shell`] over several lines, two spaces per level, as the document
/// view shows it. Short arrays of plain values stay on one line.
pub fn to_shell_pretty(b: &Bson) -> String {
    fn go(b: &Bson, level: usize, out: &mut String) {
        let pad = |n: usize| "  ".repeat(n);
        match b {
            Bson::Document(d) if !d.is_empty() => {
                out.push_str("{\n");
                for (i, (k, v)) in d.iter().enumerate() {
                    out.push_str(&pad(level + 1));
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push_str(": ");
                    go(v, level + 1, out);
                    if i + 1 < d.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad(level));
                out.push('}');
            }
            Bson::Array(a) if !a.is_empty() => {
                let inline = to_shell(b);
                if inline.len() <= 72 && !a.iter().any(|v| matches!(v, Bson::Document(_) | Bson::Array(_))) {
                    out.push_str(&inline);
                    return;
                }
                out.push_str("[\n");
                for (i, v) in a.iter().enumerate() {
                    out.push_str(&pad(level + 1));
                    go(v, level + 1, out);
                    if i + 1 < a.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad(level));
                out.push(']');
            }
            other => out.push_str(&to_shell(other)),
        }
    }
    let mut out = String::new();
    go(b, 0, &mut out);
    out
}

/// Splits a script at top-level `;` and at line breaks that start a new
/// `db.`, `show` or `use` statement. Chained calls on their own lines
/// (`.sort(…)`) stay with their statement.
pub fn statements(script: &str) -> Vec<Statement> {
    let chars: Vec<(usize, char)> = script.char_indices().collect();
    let mut out = Vec::new();
    let (mut depth, mut start, mut i) = (0i32, 0usize, 0usize);
    let push = |out: &mut Vec<Statement>, from: usize, to: usize| {
        let text = &script[from..to];
        let trimmed = strip_comments(text);
        if !trimmed.trim().is_empty() {
            let lead = text.len() - text.trim_start().len();
            let body = text.trim();
            out.push(Statement { range: from + lead..from + lead + body.len(), text: body.to_string() });
        }
    };
    while i < chars.len() {
        let (at, c) = chars[i];
        let next = chars.get(i + 1).map(|(_, c)| *c);
        match c {
            '"' | '\'' | '`' => {
                i += 1;
                while i < chars.len() && chars[i].1 != c {
                    if chars[i].1 == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            '/' if next == Some('/') => {
                while i < chars.len() && chars[i].1 != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if next == Some('*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i].1 == '*' && chars[i + 1].1 == '/') {
                    i += 1;
                }
                i += 1;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ';' if depth <= 0 => {
                push(&mut out, start, at);
                start = at + 1;
                depth = 0;
            }
            '\n' if depth <= 0 => {
                // Comment lines belong to the statement below them.
                let mut rest = script[at + 1..].trim_start();
                while rest.starts_with("//") {
                    rest = rest.split_once('\n').map_or("", |(_, r)| r).trim_start();
                }
                let starts_new = ["db.", "db[", "show ", "use "].iter().any(|p| rest.starts_with(p));
                if starts_new && !strip_comments(&script[start..at]).trim().is_empty() {
                    push(&mut out, start, at);
                    start = at + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    push(&mut out, start, script.len());
    out
}

fn strip_comments(text: &str) -> String {
    text.lines().map(|l| if l.trim_start().starts_with("//") { "" } else { l }).collect::<Vec<_>>().join("\n")
}

/// Database commands (`db.runCommand({…})`) that only read.
const READ_COMMANDS: &[&str] = &[
    "ping", "hello", "ismaster", "isMaster", "buildInfo", "buildinfo", "serverStatus", "dbStats", "dbstats", "collStats", "listCollections",
    "listDatabases", "listIndexes", "find", "count", "distinct", "explain", "connectionStatus", "hostInfo", "getLog", "currentOp",
    "getParameter", "getCmdLineOpts", "replSetGetStatus", "top", "validate", "dataSize", "usersInfo", "rolesInfo",
];

/// Whether a statement only reads.
pub fn is_read(statement: &str) -> bool {
    let Ok(cmd) = parse(statement) else { return false };
    let Command::Calls { target, calls } = &cmd else { return true };
    if calls.iter().skip(1).any(|c| c.name == "explain") {
        return true;
    }
    let first = &calls[0];
    let name = first.name.as_str();
    match target {
        Target::Collection(_) => match name {
            "find" | "findOne" | "countDocuments" | "estimatedDocumentCount" | "count" | "distinct" | "getIndexes" | "listIndexes"
            | "stats" | "dataSize" | "storageSize" | "totalSize" | "totalIndexSize" | "explain" | "latencyStats" | "getName"
            | "exists" | "isCapped" => true,
            // $out and $merge write the results into a collection.
            "aggregate" => !matches!(first.args.first(), Some(Bson::Array(stages)) if stages.iter().any(|s| matches!(s, Bson::Document(d) if d.contains_key("$out") || d.contains_key("$merge")))),
            _ => false,
        },
        Target::Database => match name {
            "getCollectionNames" | "getCollectionInfos" | "stats" | "getName" | "version" | "hello" | "isMaster" | "serverStatus"
            | "hostInfo" | "currentOp" | "listCommands" | "serverBuildInfo" | "getProfilingStatus" => true,
            "runCommand" | "adminCommand" => match first.args.first() {
                Some(Bson::Document(d)) => d.keys().next().is_some_and(|k| READ_COMMANDS.contains(&k.as_str())),
                Some(Bson::String(s)) => READ_COMMANDS.contains(&s.as_str()),
                _ => false,
            },
            _ => false,
        },
    }
}

/// `find(…)` with `.limit(n)` added when it has none, like the SQL editor's
/// row limit.
pub fn with_limit(statement: &str, limit: Option<u32>) -> String {
    let Some(n) = limit else { return statement.to_string() };
    match parse(statement) {
        Ok(cmd) if cmd.method() == "find" && cmd.chained("limit").is_none() => {
            format!("{}.limit({n})", statement.trim().trim_end_matches(';').trim_end())
        }
        _ => statement.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    fn calls(s: &str) -> (Target, Vec<Call>) {
        match parse(s).unwrap() {
            Command::Calls { target, calls } => (target, calls),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_shell_calls() {
        let (t, c) = calls("db.users.find({ age: { $gt: 30 }, 'name': \"Ann\" }, {name: 1, _id: 0}).sort({age: -1}).limit(5);");
        assert_eq!(t, Target::Collection("users".into()));
        assert_eq!(c[0].name, "find");
        assert_eq!(c[0].args[0], Bson::Document(doc! {"age": {"$gt": 30}, "name": "Ann"}));
        assert_eq!(c[1], Call { name: "sort".into(), args: vec![Bson::Document(doc! {"age": -1})] });
        assert_eq!(c[2].args, vec![Bson::Int32(5)]);

        let (t, _) = calls("db.getCollection('order-items').countDocuments()");
        assert_eq!(t, Target::Collection("order-items".into()));
        let (t, _) = calls("db[\"weird name\"].find()");
        assert_eq!(t, Target::Collection("weird name".into()));
        let (t, _) = calls("db.logs.app.find()");
        assert_eq!(t, Target::Collection("logs.app".into()));
        let (t, c) = calls("db.runCommand({ping: 1})");
        assert_eq!((t, c[0].name.as_str()), (Target::Database, "runCommand"));
        assert_eq!(parse("show dbs").unwrap(), Command::Show("dbs".into()));
        assert!(parse("SELECT 1").is_err());
        assert!(parse("db.users").is_err());
    }

    #[test]
    fn parses_shell_literals() {
        let (_, c) = calls(
            r#"db.t.insertOne({
                _id: ObjectId("65a1b2c3d4e5f60718293a4b"),
                at: ISODate("2024-05-06T07:08:09Z"),
                day: new Date("2024-05-06"),
                big: NumberLong("9007199254740993"),
                n: 42, f: 1.5, e: 1e3, neg: -7,
                price: NumberDecimal("9.99"),
                re: /^ann/i,
                tags: ['a', "b",],
                ext: {"$oid": "65a1b2c3d4e5f60718293a4c"},
                // a comment
                nothing: null, yes: true,
            })"#,
        );
        let Bson::Document(d) = &c[0].args[0] else { panic!() };
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
        assert!(matches!(d.get("at"), Some(Bson::DateTime(_))));
        assert_eq!(d.get("day"), Some(&Bson::DateTime(parse_date("2024-05-06").unwrap())));
        assert_eq!(d.get("big"), Some(&Bson::Int64(9007199254740993)));
        assert_eq!((d.get("n"), d.get("f"), d.get("e"), d.get("neg")), (Some(&Bson::Int32(42)), Some(&Bson::Double(1.5)), Some(&Bson::Double(1000.0)), Some(&Bson::Int32(-7))));
        assert!(matches!(d.get("price"), Some(Bson::Decimal128(_))));
        assert_eq!(d.get("re"), Some(&Bson::RegularExpression(bson::Regex { pattern: "^ann".into(), options: "i".into() })));
        assert_eq!(d.get("tags"), Some(&Bson::Array(vec!["a".into(), "b".into()])));
        assert!(matches!(d.get("ext"), Some(Bson::ObjectId(_))), "extended JSON becomes its type");
        assert_eq!(d.get("nothing"), Some(&Bson::Null));
    }

    #[test]
    fn printing_round_trips() {
        let value = Bson::Document(doc! {
            "_id": ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap(),
            "at": bson::DateTime::from_millis(1_714_979_289_000),
            "big": 9_007_199_254_740_993i64,
            "whole": 3.0,
            "s": "quote \" and 'apostrophe' and 文字",
            "nested": {"a": [1, 2.5, null, true]},
        });
        let text = format!("db.t.insertOne({})", to_shell(&value));
        let (_, c) = calls(&text);
        assert_eq!(c[0].args[0], value, "{text}");
    }

    #[test]
    fn splits_statements() {
        let script = "db.a.find({x: 1})\n  .sort({x: -1})\ndb.b.countDocuments(); db.c.drop()\n// note\nshow collections";
        let s: Vec<String> = statements(script).into_iter().map(|s| s.text).collect();
        assert_eq!(s, vec!["db.a.find({x: 1})\n  .sort({x: -1})", "db.b.countDocuments()", "db.c.drop()", "// note\nshow collections"]);
        let s = statements("db.a.insertOne({\n  text: 'has; semicolon\\nand db.lines'\n})");
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn reads_writes_and_limits() {
        for r in ["db.a.find()", "db.a.aggregate([{$match: {}}])", "db.a.countDocuments({})", "db.runCommand({dbStats: 1})", "show dbs", "db.a.find().explain()", "db.getCollectionNames()"] {
            assert!(is_read(r), "{r}");
        }
        for w in ["db.a.insertOne({})", "db.a.aggregate([{$out: 'b'}])", "db.a.drop()", "db.runCommand({drop: 'a'})", "db.dropDatabase()", "nonsense"] {
            assert!(!is_read(w), "{w}");
        }
        assert_eq!(with_limit("db.a.find({x: 1});", Some(100)), "db.a.find({x: 1}).limit(100)");
        assert_eq!(with_limit("db.a.find().limit(5)", Some(100)), "db.a.find().limit(5)");
        assert_eq!(with_limit("db.a.countDocuments()", Some(100)), "db.a.countDocuments()");
    }
}
