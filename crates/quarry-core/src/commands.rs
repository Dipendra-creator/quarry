//! Redis's command language as Quarry's editor speaks it: one redis-cli
//! style command per line, with redis-cli quoting.

use crate::sql::Statement;

/// Splits a redis-cli line into arguments. Spaces separate them; "double
/// quotes" take `\n`, `\r`, `\t`, `\"`, `\\` and `\xHH` escapes; 'single
/// quotes' take only `\'`.
pub fn split_args(line: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else { break };
        let mut arg = String::new();
        if first == '"' || first == '\'' {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == first {
                    closed = true;
                    break;
                }
                if c == '\\' {
                    let Some(e) = chars.next() else { break };
                    if first == '\'' {
                        if e != '\'' {
                            arg.push('\\');
                        }
                        arg.push(e);
                        continue;
                    }
                    match e {
                        'n' => arg.push('\n'),
                        'r' => arg.push('\r'),
                        't' => arg.push('\t'),
                        'b' => arg.push('\u{8}'),
                        'a' => arg.push('\u{7}'),
                        'x' => {
                            let hex: String = chars.by_ref().take(2).collect();
                            match u8::from_str_radix(&hex, 16) {
                                Ok(b) => arg.push(b as char),
                                Err(_) => return Err(format!("bad \\x escape \"\\x{hex}\"")),
                            }
                        }
                        other => arg.push(other),
                    }
                } else {
                    arg.push(c);
                }
            }
            if !closed {
                return Err(format!("unbalanced quotes in: {line}"));
            }
            if chars.peek().is_some_and(|c| !c.is_whitespace()) {
                return Err(format!("closing quote must be followed by a space in: {line}"));
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                arg.push(c);
                chars.next();
            }
        }
        args.push(arg);
    }
    Ok(args)
}

/// An argument as redis-cli would need it typed: bare when it is plain,
/// else double-quoted with escapes. [`split_args`] reads it back.
pub fn quote_arg(s: &str) -> String {
    let plain = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_:.@/+*?[]{}=,$%&!~^".contains(c));
    if plain {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A whole command line from its parts.
pub fn command_line<S: AsRef<str>>(parts: &[S]) -> String {
    parts.iter().map(|p| quote_arg(p.as_ref())).collect::<Vec<_>>().join(" ")
}

/// One statement per line. Blank lines and `#` or `//` comment lines are
/// skipped.
pub fn redis_statements(script: &str) -> Vec<Statement> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in script.split_inclusive('\n') {
        let end = start + line.len();
        let text = line.trim();
        if !text.is_empty() && !text.starts_with('#') && !text.starts_with("//") {
            let lead = line.len() - line.trim_start().len();
            out.push(Statement { range: start + lead..start + lead + text.len(), text: text.to_string() });
        }
        start = end;
    }
    out
}

/// Commands offered by the editor's autocomplete.
pub const COMMON_COMMANDS: &[&str] = &[
    "APPEND", "BITCOUNT", "CLIENT LIST", "CONFIG GET", "COPY", "DBSIZE", "DECR", "DECRBY", "DEL", "EXISTS", "EXPIRE", "EXPIREAT",
    "FLUSHDB", "GET", "GETDEL", "GETEX", "GETRANGE", "GETSET", "HDEL", "HEXISTS", "HGET", "HGETALL", "HINCRBY", "HKEYS", "HLEN",
    "HMGET", "HSCAN", "HSET", "HSETNX", "HVALS", "INCR", "INCRBY", "INCRBYFLOAT", "INFO", "JSON.GET", "JSON.SET", "KEYS",
    "LINDEX", "LINSERT", "LLEN", "LMOVE", "LPOP", "LPOS", "LPUSH", "LRANGE", "LREM", "LSET", "LTRIM", "MEMORY USAGE", "MGET",
    "MSET", "OBJECT ENCODING", "PERSIST", "PEXPIRE", "PFADD", "PFCOUNT", "PING", "PTTL", "PUBLISH", "RENAME", "RENAMENX",
    "RPOP", "RPUSH", "SADD", "SCAN", "SCARD", "SDIFF", "SET", "SETEX", "SETNX", "SINTER", "SISMEMBER", "SLOWLOG GET",
    "SMEMBERS", "SMOVE", "SPOP", "SRANDMEMBER", "SREM", "SSCAN", "STRLEN", "SUNION", "TIME", "TTL", "TYPE", "UNLINK", "XACK",
    "XADD", "XDEL", "XGROUP", "XINFO", "XLEN", "XRANGE", "XREAD", "XREVRANGE", "XTRIM", "ZADD", "ZCARD", "ZCOUNT", "ZINCRBY",
    "ZRANGE", "ZRANGEBYSCORE", "ZRANK", "ZREM", "ZREVRANGE", "ZSCAN", "ZSCORE",
];

/// Commands that only read. Anything else may change data.
const READ_COMMANDS: &[&str] = &[
    "GET", "MGET", "GETRANGE", "SUBSTR", "STRLEN", "LCS", "EXISTS", "TYPE", "TTL", "PTTL", "EXPIRETIME", "PEXPIRETIME", "KEYS",
    "SCAN", "RANDOMKEY", "DBSIZE", "DUMP", "TOUCH", "HGET", "HMGET", "HGETALL", "HKEYS", "HVALS", "HLEN", "HEXISTS", "HSCAN",
    "HSTRLEN", "HRANDFIELD", "HTTL", "HPTTL", "LRANGE", "LLEN", "LINDEX", "LPOS", "SMEMBERS", "SISMEMBER", "SMISMEMBER",
    "SCARD", "SSCAN", "SRANDMEMBER", "SINTER", "SUNION", "SDIFF", "SINTERCARD", "ZRANGE", "ZRANGEBYSCORE", "ZRANGEBYLEX",
    "ZREVRANGE", "ZREVRANGEBYSCORE", "ZREVRANGEBYLEX", "ZSCORE", "ZMSCORE", "ZCARD", "ZCOUNT", "ZLEXCOUNT", "ZRANK",
    "ZREVRANK", "ZSCAN", "ZRANDMEMBER", "ZINTER", "ZUNION", "ZDIFF", "ZINTERCARD", "XRANGE", "XREVRANGE", "XLEN", "XREAD",
    "XINFO", "XPENDING", "GEOPOS", "GEODIST", "GEOHASH", "GEOSEARCH", "GEORADIUS_RO", "GEORADIUSBYMEMBER_RO", "PFCOUNT",
    "BITCOUNT", "BITPOS", "GETBIT", "BITFIELD_RO", "SORT_RO", "EVAL_RO", "EVALSHA_RO", "FCALL_RO", "JSON.GET", "JSON.MGET",
    "JSON.TYPE", "JSON.STRLEN", "JSON.ARRLEN", "JSON.ARRINDEX", "JSON.OBJKEYS", "JSON.OBJLEN", "JSON.RESP", "FT.SEARCH",
    "FT.AGGREGATE", "FT.INFO", "FT._LIST", "FT.EXPLAIN", "TS.GET", "TS.MGET", "TS.RANGE", "TS.REVRANGE", "TS.MRANGE",
    "TS.MREVRANGE", "TS.INFO", "TS.QUERYINDEX", "BF.EXISTS", "BF.MEXISTS", "BF.INFO", "CF.EXISTS", "CF.INFO", "PING", "ECHO",
    "INFO", "TIME", "LASTSAVE", "ROLE", "LOLWUT", "COMMAND", "OBJECT",
];

/// Commands whose first argument is a subcommand that decides.
const READ_SUBCOMMANDS: &[(&str, &[&str])] = &[
    ("CONFIG", &["GET", "HELP"]),
    ("CLIENT", &["LIST", "INFO", "GETNAME", "ID", "HELP", "TRACKINGINFO", "GETREDIR"]),
    ("MEMORY", &["USAGE", "STATS", "DOCTOR", "MALLOC-STATS", "HELP"]),
    ("SLOWLOG", &["GET", "LEN", "HELP"]),
    ("LATENCY", &["LATEST", "HISTORY", "DOCTOR", "GRAPH", "HISTOGRAM", "HELP"]),
    ("CLUSTER", &["INFO", "NODES", "SLOTS", "SHARDS", "MYID", "KEYSLOT", "COUNTKEYSINSLOT", "HELP"]),
    ("FUNCTION", &["LIST", "STATS", "DUMP", "HELP"]),
    ("SCRIPT", &["EXISTS", "HELP"]),
    ("ACL", &["WHOAMI", "LIST", "USERS", "GETUSER", "CAT", "LOG", "HELP"]),
    ("PUBSUB", &["CHANNELS", "NUMSUB", "NUMPAT", "SHARDCHANNELS", "SHARDNUMSUB", "HELP"]),
];

/// Whether a Redis command line only reads.
pub fn redis_is_read(line: &str) -> bool {
    let Ok(args) = split_args(line) else { return false };
    let Some(cmd) = args.first().map(|c| c.to_ascii_uppercase()) else { return true };
    if cmd == "SORT" {
        return !args.iter().any(|a| a.eq_ignore_ascii_case("STORE"));
    }
    if READ_COMMANDS.contains(&cmd.as_str()) {
        return true;
    }
    let sub = args.get(1).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    READ_SUBCOMMANDS.iter().any(|(c, subs)| *c == cmd && subs.contains(&sub.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_split_like_redis_cli() {
        assert_eq!(split_args(r#"SET greeting "hello world""#).unwrap(), vec!["SET", "greeting", "hello world"]);
        assert_eq!(split_args(r#"SET k "a\"b\\c\n\x41""#).unwrap(), vec!["SET", "k", "a\"b\\c\nA"]);
        assert_eq!(split_args(r"SET k 'it\'s \n'").unwrap(), vec!["SET", "k", "it's \\n"]);
        assert_eq!(split_args("  HGETALL   user:1  ").unwrap(), vec!["HGETALL", "user:1"]);
        assert!(split_args(r#"SET k "open"#).is_err());
        assert_eq!(split_args(r#"SET k """#).unwrap(), vec!["SET", "k", ""]);
    }

    #[test]
    fn quoting_round_trips() {
        for s in ["plain:key", "two words", "", "quote\"back\\slash", "line\nbreak\ttab", "ünïcode 文字", "\u{1}"] {
            let line = command_line(&["SET", s, "v"]);
            assert_eq!(split_args(&line).unwrap(), vec!["SET", s, "v"], "{line}");
        }
        assert_eq!(quote_arg("user:1"), "user:1");
        assert_eq!(quote_arg("a b"), "\"a b\"");
    }

    #[test]
    fn statements_are_lines() {
        let s = redis_statements("# setup\nSET a 1\n\n  GET a  \n// done\nDEL a");
        let texts: Vec<&str> = s.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["SET a 1", "GET a", "DEL a"]);
        let script = "SET a 1\n  GET a  \n";
        let s = redis_statements(script);
        assert_eq!(&script[s[1].range.clone()], "GET a");
    }

    #[test]
    fn reads_and_writes() {
        for r in ["GET a", "hgetall user:1", "SCAN 0 MATCH user:*", "CONFIG GET maxmemory", "CLIENT LIST", "SORT list", "INFO"] {
            assert!(redis_is_read(r), "{r}");
        }
        for w in ["SET a 1", "DEL a", "FLUSHALL", "CONFIG SET maxmemory 1", "SORT list STORE out", "HSET h f v", "EVAL x 0", "bogus"] {
            assert!(!redis_is_read(w), "{w}");
        }
    }
}
