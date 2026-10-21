//! SQL text utilities: per-dialect quoting, filter → WHERE translation,
//! statement splitting, read-only detection, auto LIMIT and formatting.

use std::ops::Range;

use crate::model::DriverKind;
use crate::query::{ANY_COLUMN, BrowseRequest, Filter, FilterOp, RAW_SQL};
use crate::schema::TableRef;
use crate::value::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dialect(pub DriverKind);

impl Dialect {
    fn is_mysql(self) -> bool {
        matches!(self.0, DriverKind::MySql | DriverKind::MariaDb)
    }

    pub fn quote_ident(self, ident: &str) -> String {
        if self.is_mysql() {
            format!("`{}`", ident.replace('`', "``"))
        } else {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
    }

    pub fn table(self, t: &TableRef) -> String {
        match &t.schema {
            Some(s) => format!("{}.{}", self.quote_ident(s), self.quote_ident(&t.name)),
            None => self.quote_ident(&t.name),
        }
    }

    pub fn string_literal(self, s: &str) -> String {
        let mut escaped = s.replace('\'', "''");
        if self.is_mysql() {
            escaped = escaped.replace('\\', "\\\\");
        }
        format!("'{escaped}'")
    }

    pub fn literal(self, v: &Value) -> String {
        match v {
            Value::Null => "NULL".into(),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) if f.is_finite() => f.to_string(),
            Value::Float(f) => self.string_literal(&f.to_string()),
            Value::Text(t) => self.string_literal(t),
            Value::Bytes(b) => {
                let hex = crate::value::hex(b);
                match self.0 {
                    DriverKind::Postgres => format!("'\\x{}'", &hex[2..]),
                    _ => format!("X'{}'", &hex[2..]),
                }
            }
        }
    }

    /// `expr` cast to text where the server needs it for LIKE on any type.
    fn as_text(self, column: &str) -> String {
        match self.0 {
            DriverKind::Postgres => format!("{}::TEXT", self.quote_ident(column)),
            _ => self.quote_ident(column),
        }
    }

    fn like(self, case_insensitive: bool) -> &'static str {
        if case_insensitive && self.0 == DriverKind::Postgres {
            "ILIKE"
        } else {
            "LIKE"
        }
    }

    /// One filter row as a SQL condition. `columns` is used for "Any column".
    pub fn filter_condition(self, f: &Filter, columns: &[String]) -> Option<String> {
        if !f.enabled {
            return None;
        }
        if f.column == RAW_SQL {
            let raw = f.value.trim();
            return (!raw.is_empty()).then(|| format!("({raw})"));
        }
        if f.column == ANY_COLUMN {
            let parts: Vec<String> = columns
                .iter()
                .filter_map(|c| self.column_condition(c, f.op, &f.value, true))
                .collect();
            return (!parts.is_empty()).then(|| format!("({})", parts.join(" OR ")));
        }
        if f.column.is_empty() {
            return None;
        }
        self.column_condition(&f.column, f.op, &f.value, false)
    }

    fn column_condition(self, column: &str, op: FilterOp, value: &str, any: bool) -> Option<String> {
        let col = if any { self.as_text(column) } else { self.quote_ident(column) };
        let lit = |s: &str| self.string_literal(s.trim());
        let pattern = |pre: &str, post: &str| self.string_literal(&format!("{pre}{value}{post}"));
        Some(match op {
            FilterOp::Eq | FilterOp::NotEq | FilterOp::Lt | FilterOp::Gt | FilterOp::LtEq | FilterOp::GtEq => {
                format!("{col} {} {}", op.label(), lit(value))
            }
            FilterOp::In | FilterOp::NotIn => {
                let items: Vec<String> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(lit)
                    .collect();
                if items.is_empty() {
                    return None;
                }
                format!("{col} {} ({})", op.label(), items.join(", "))
            }
            FilterOp::IsNull | FilterOp::IsNotNull => format!("{} {}", self.quote_ident(column), op.label()),
            FilterOp::Between | FilterOp::NotBetween => {
                let upper = value.to_ascii_uppercase();
                let at = upper.find(" AND ")?;
                format!(
                    "{col} {} {} AND {}",
                    op.label(),
                    lit(&value[..at]),
                    lit(&value[at + 5..])
                )
            }
            FilterOp::Like => format!("{col} LIKE {}", lit(value)),
            FilterOp::ILike => format!("{col} {} {}", self.like(true), lit(value)),
            FilterOp::Contains | FilterOp::ContainsCi => {
                format!("{} {} {}", self.as_text(column), self.like(op == FilterOp::ContainsCi), pattern("%", "%"))
            }
            FilterOp::NotContains | FilterOp::NotContainsCi => format!(
                "{} NOT {} {}",
                self.as_text(column),
                self.like(op == FilterOp::NotContainsCi),
                pattern("%", "%")
            ),
            FilterOp::HasPrefix | FilterOp::HasPrefixCi => {
                format!("{} {} {}", self.as_text(column), self.like(op == FilterOp::HasPrefixCi), pattern("", "%"))
            }
            FilterOp::HasSuffix | FilterOp::HasSuffixCi => {
                format!("{} {} {}", self.as_text(column), self.like(op == FilterOp::HasSuffixCi), pattern("%", ""))
            }
        })
    }

    pub fn where_clause(self, filters: &[Filter], columns: &[String]) -> String {
        let conds: Vec<String> = filters
            .iter()
            .filter_map(|f| self.filter_condition(f, columns))
            .collect();
        if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        }
    }

    /// The data view's SELECT for one page.
    pub fn select_query(self, req: &BrowseRequest, all_columns: &[String]) -> String {
        let cols = if req.columns.is_empty() {
            "*".to_string()
        } else {
            req.columns
                .iter()
                .map(|c| self.quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut sql = format!(
            "SELECT {cols} FROM {}{}",
            self.table(&req.table),
            self.where_clause(&req.filters, all_columns)
        );
        if !req.sort.is_empty() {
            let order: Vec<String> = req
                .sort
                .iter()
                .map(|s| format!("{} {}", self.quote_ident(&s.column), if s.descending { "DESC" } else { "ASC" }))
                .collect();
            sql.push_str(&format!(" ORDER BY {}", order.join(", ")));
        }
        sql.push_str(&format!(" LIMIT {} OFFSET {}", req.limit, req.offset));
        sql
    }

    pub fn count_query(self, table: &TableRef, filters: &[Filter], columns: &[String]) -> String {
        format!(
            "SELECT COUNT(*) FROM {}{}",
            self.table(table),
            self.where_clause(filters, columns)
        )
    }
}

/// A statement inside a script, with its byte range in the script.
#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    pub range: Range<usize>,
    pub text: String,
}

/// Splits a script on top-level `;`, skipping quotes, comments and
/// PostgreSQL dollar-quoted bodies. Empty statements are dropped.
pub fn split_statements(script: &str) -> Vec<Statement> {
    let bytes = script.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let push = |out: &mut Vec<Statement>, from: usize, to: usize| {
        let raw = &script[from..to];
        let trimmed = raw.trim();
        if !trimmed.is_empty() && !is_only_comments(trimmed) {
            let lead = raw.len() - raw.trim_start().len();
            out.push(Statement {
                range: from + lead..from + lead + trimmed.len(),
                text: trimmed.to_string(),
            });
        }
    };
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(bytes, i),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_line(bytes, i),
            b'#' => i = skip_line(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            b'$' => i = skip_dollar(script, i),
            b';' => {
                push(&mut out, start, i);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    push(&mut out, start, bytes.len());
    out
}

fn skip_quoted(bytes: &[u8], at: usize) -> usize {
    let q = bytes[at];
    let mut i = at + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && q == b'\'' {
            i += 2;
            continue;
        }
        if bytes[i] == q {
            if bytes.get(i + 1) == Some(&q) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_line(bytes: &[u8], at: usize) -> usize {
    bytes[at..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |p| at + p + 1)
}

fn skip_block_comment(bytes: &[u8], at: usize) -> usize {
    let mut i = at + 2;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return i + 2;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_dollar(script: &str, at: usize) -> usize {
    let rest = &script[at + 1..];
    let tag_len = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    if !rest[tag_len..].starts_with('$') || rest.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return at + 1;
    }
    let tag = &script[at..at + tag_len + 2];
    let body = at + tag.len();
    match script[body..].find(tag) {
        Some(p) => body + p + tag.len(),
        None => script.len(),
    }
}

fn is_only_comments(s: &str) -> bool {
    strip_leading_comments(s).trim().is_empty()
}

fn strip_leading_comments(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if s.starts_with("--") || s.starts_with('#') {
            s = s.find('\n').map_or("", |p| &s[p + 1..]);
        } else if s.starts_with("/*") {
            s = s.find("*/").map_or("", |p| &s[p + 2..]);
        } else {
            return s;
        }
    }
}

/// Statement under the cursor ("Run current"). Falls back to the closest
/// statement before the cursor.
pub fn statement_at(script: &str, cursor: usize) -> Option<Statement> {
    let stmts = split_statements(script);
    stmts
        .iter()
        .find(|s| cursor >= s.range.start && cursor <= s.range.end + 1)
        .or_else(|| stmts.iter().rev().find(|s| s.range.end <= cursor))
        .or(stmts.first())
        .cloned()
}

fn first_keyword(sql: &str) -> String {
    strip_leading_comments(sql)
        .trim_start_matches('(')
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .find(|w| !w.is_empty())
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// True when every statement only reads (SELECT/SHOW/EXPLAIN/…).
pub fn is_read_only(sql: &str) -> bool {
    let stmts = split_statements(sql);
    !stmts.is_empty()
        && stmts.iter().all(|s| {
            let kw = first_keyword(&s.text);
            match kw.as_str() {
                "SELECT" | "SHOW" | "EXPLAIN" | "DESCRIBE" | "DESC" | "VALUES" | "TABLE" => {
                    !top_level_words(&s.text).iter().any(|w| w == "INTO")
                }
                "WITH" => !words(&s.text, false)
                    .iter()
                    .any(|w| matches!(w.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "INTO")),
                "PRAGMA" => !s.text.contains('='),
                _ => false,
            }
        })
}

fn top_level_words(sql: &str) -> Vec<String> {
    words(sql, true)
}

/// Upper-cased words outside quotes and comments; with `top_only`, also
/// outside parentheses.
fn words(sql: &str, top_only: bool) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut words = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(bytes, i),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_line(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            b'$' => i = skip_dollar(sql, i),
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                if depth == 0 || !top_only {
                    words.push(sql[start..i].to_ascii_uppercase());
                }
            }
            _ => i += 1,
        }
    }
    words
}

/// Appends `LIMIT n` to a top-level SELECT that has no LIMIT/FETCH of its
/// own, like the editor's row-limit setting in the original app.
pub fn apply_auto_limit(stmt: &str, limit: Option<u32>) -> String {
    let Some(limit) = limit else {
        return stmt.to_string();
    };
    if first_keyword(stmt) != "SELECT" {
        return stmt.to_string();
    }
    let words = top_level_words(stmt);
    if words.iter().any(|w| matches!(w.as_str(), "LIMIT" | "FETCH" | "INTO" | "FOR")) {
        return stmt.to_string();
    }
    format!("{} LIMIT {limit}", stmt.trim_end().trim_end_matches(';'))
}

const CLAUSES: [&str; 22] = [
    "SELECT", "FROM", "WHERE", "GROUP BY", "ORDER BY", "HAVING", "LIMIT", "OFFSET", "UNION ALL", "UNION",
    "LEFT JOIN", "RIGHT JOIN", "INNER JOIN", "FULL JOIN", "CROSS JOIN", "JOIN", "VALUES", "SET", "INSERT INTO",
    "UPDATE", "DELETE FROM", "RETURNING",
];

const KEYWORDS: [&str; 40] = [
    "select", "from", "where", "and", "or", "not", "null", "is", "in", "as", "on", "join", "left", "right",
    "inner", "outer", "full", "cross", "group", "by", "order", "having", "limit", "offset", "union", "all",
    "distinct", "insert", "into", "values", "update", "set", "delete", "asc", "desc", "case", "when", "then",
    "else", "end",
];

/// "Beautify": upper-cases keywords and starts each major clause on its own line.
pub fn format_sql(sql: &str) -> String {
    split_statements(sql)
        .iter()
        .map(|s| format!("{};", format_statement(&s.text)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_statement(sql: &str) -> String {
    let tokens = tokenize(&minify_statement(sql));
    let mut out = String::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        let upper = tok.to_ascii_uppercase();
        let two = tokens
            .get(i + 2)
            .map(|t| format!("{upper} {}", t.to_ascii_uppercase()))
            .filter(|_| tokens.get(i + 1).is_some_and(|t| t == " "));
        let clause = two
            .as_deref()
            .filter(|t| CLAUSES.contains(t))
            .map(|t| (t.to_string(), 3))
            .or_else(|| CLAUSES.contains(&upper.as_str()).then(|| (upper.clone(), 1)));
        match clause {
            Some((kw, len)) if depth == 0 => {
                let trimmed = out.trim_end().len();
                out.truncate(trimmed);
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&kw);
                out.push_str("\n  ");
                i += len;
                if tokens.get(i).is_some_and(|t| t == " ") {
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        match tok.as_str() {
            "(" => depth += 1,
            ")" => depth -= 1,
            "," if depth == 0 => {
                out.push_str(",\n  ");
                i += 1;
                if tokens.get(i).is_some_and(|t| t == " ") {
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        if KEYWORDS.contains(&tok.to_ascii_lowercase().as_str()) {
            out.push_str(&upper);
        } else {
            out.push_str(tok);
        }
        i += 1;
    }
    out.trim().to_string()
}

/// "Uglify": collapses whitespace and comments onto one line.
pub fn minify_sql(sql: &str) -> String {
    split_statements(sql)
        .iter()
        .map(|s| format!("{};", minify_statement(&s.text)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn minify_statement(sql: &str) -> String {
    let mut out = String::new();
    for tok in tokenize(sql) {
        if tok.starts_with("--") || tok.starts_with("/*") {
            continue;
        }
        if tok.trim().is_empty() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push_str(&tok);
        }
    }
    out.trim().to_string()
}

/// Words, quoted strings, comments, whitespace runs and single punctuation.
fn tokenize(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        i = match bytes[i] {
            b'\'' | b'"' | b'`' => skip_quoted(bytes, i),
            b'-' if bytes.get(i + 1) == Some(&b'-') => skip_line(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => skip_block_comment(bytes, i),
            b'$' => skip_dollar(sql, i).max(i + 1),
            b if b.is_ascii_whitespace() => {
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                j
            }
            b if b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 => {
                let mut j = i;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] >= 0x80 || bytes[j] == b'.') {
                    j += 1;
                }
                j
            }
            _ => i + 1,
        };
        let tok = &sql[start..i];
        out.push(if tok.trim().is_empty() { " ".to_string() } else { tok.to_string() });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Sort;

    #[test]
    fn splits_respecting_quotes_comments_and_dollars() {
        let script = "select ';' as a; -- c;\n/* x; */ select 2;\nCREATE FUNCTION f() RETURNS int AS $$ select 1; $$ LANGUAGE sql;  ";
        let s: Vec<String> = split_statements(script).into_iter().map(|s| s.text).collect();
        assert_eq!(
            s,
            vec![
                "select ';' as a",
                "-- c;\n/* x; */ select 2",
                "CREATE FUNCTION f() RETURNS int AS $$ select 1; $$ LANGUAGE sql"
            ]
        );
        assert!(split_statements("-- only a comment").is_empty());
    }

    #[test]
    fn statement_under_cursor() {
        let script = "select 1;\nselect 2;";
        assert_eq!(statement_at(script, 12).unwrap().text, "select 2");
        assert_eq!(statement_at(script, 3).unwrap().text, "select 1");
        assert_eq!(statement_at(script, 9).unwrap().text, "select 1");
    }

    #[test]
    fn read_only_detection() {
        assert!(is_read_only("SELECT * FROM t"));
        assert!(is_read_only("/* hi */ show tables; explain select 1"));
        assert!(is_read_only("with a as (select 1) select * from a"));
        assert!(!is_read_only("with a as (delete from t returning *) select * from a"));
        assert!(!is_read_only("select 1; delete from t"));
        assert!(!is_read_only("select * into x from t"));
        assert!(!is_read_only("PRAGMA user_version = 3"));
        assert!(!is_read_only(""));
    }

    #[test]
    fn auto_limit() {
        assert_eq!(apply_auto_limit("select * from t;", Some(300)), "select * from t LIMIT 300");
        assert_eq!(apply_auto_limit("select * from t limit 5", Some(300)), "select * from t limit 5");
        assert_eq!(
            apply_auto_limit("select * from (select 1 limit 1) x", Some(10)),
            "select * from (select 1 limit 1) x LIMIT 10"
        );
        assert_eq!(apply_auto_limit("update t set a=1", Some(10)), "update t set a=1");
        assert_eq!(apply_auto_limit("select 1", None), "select 1");
    }

    #[test]
    fn filters_to_sql() {
        let pg = Dialect(DriverKind::Postgres);
        let my = Dialect(DriverKind::MySql);
        let f = |column: &str, op, value: &str| Filter {
            column: column.into(),
            op,
            value: value.into(),
            enabled: true,
        };
        assert_eq!(pg.filter_condition(&f("id", FilterOp::Eq, "5"), &[]).unwrap(), r#""id" = '5'"#);
        assert_eq!(
            pg.filter_condition(&f("id", FilterOp::In, "1, 2,3"), &[]).unwrap(),
            r#""id" IN ('1', '2', '3')"#
        );
        assert_eq!(
            pg.filter_condition(&f("n", FilterOp::Between, "1 and 9"), &[]).unwrap(),
            r#""n" BETWEEN '1' AND '9'"#
        );
        assert_eq!(
            pg.filter_condition(&f("name", FilterOp::ContainsCi, "o'k"), &[]).unwrap(),
            r#""name"::TEXT ILIKE '%o''k%'"#
        );
        assert_eq!(
            my.filter_condition(&f("name", FilterOp::HasPrefix, r"a\b"), &[]).unwrap(),
            r"`name` LIKE 'a\\b%'"
        );
        assert_eq!(
            my.filter_condition(&f(ANY_COLUMN, FilterOp::Contains, "x"), &["a".into(), "b".into()]).unwrap(),
            "(`a` LIKE '%x%' OR `b` LIKE '%x%')"
        );
        assert_eq!(pg.filter_condition(&f(RAW_SQL, FilterOp::Eq, "a > 1"), &[]).unwrap(), "(a > 1)");
        assert_eq!(pg.filter_condition(&f("a", FilterOp::IsNull, ""), &[]).unwrap(), r#""a" IS NULL"#);
        let mut off = f("a", FilterOp::Eq, "1");
        off.enabled = false;
        assert_eq!(pg.filter_condition(&off, &[]), None);
    }

    #[test]
    fn select_page() {
        let req = BrowseRequest {
            table: TableRef::new(Some("public"), "users"),
            filters: vec![Filter { column: "age".into(), op: FilterOp::Gt, value: "3".into(), enabled: true }],
            sort: vec![Sort { column: "id".into(), descending: true }],
            limit: 300,
            offset: 600,
            columns: vec![],
        };
        assert_eq!(
            Dialect(DriverKind::Postgres).select_query(&req, &[]),
            r#"SELECT * FROM "public"."users" WHERE "age" > '3' ORDER BY "id" DESC LIMIT 300 OFFSET 600"#
        );
    }

    #[test]
    fn literals() {
        let pg = Dialect(DriverKind::Postgres);
        assert_eq!(pg.literal(&Value::Bytes(vec![1, 255])), r"'\x01FF'");
        assert_eq!(Dialect(DriverKind::Sqlite).literal(&Value::Bytes(vec![1])), "X'01'");
        assert_eq!(pg.quote_ident(r#"we"ird"#), r#""we""ird""#);
    }

    #[test]
    fn beautify_and_uglify() {
        assert_eq!(
            format_sql("select a, b from t where a = 1 order by b"),
            "SELECT\n  a,\n  b\nFROM\n  t\nWHERE\n  a = 1\nORDER BY\n  b;"
        );
        assert_eq!(minify_sql("select  a,\n  b -- c\nfrom t;"), "select a, b from t;");
    }
}
