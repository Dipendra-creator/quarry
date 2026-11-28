//! Query results, filters, sorting and paging for the data view.

use serde::{Deserialize, Serialize};

use crate::value::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResultColumn {
    pub name: String,
    /// Server type name when the driver knows it (`int4`, `VARCHAR`, …).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub type_name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub statement: String,
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_affected: Option<u64>,
    pub duration_ms: u64,
}

impl QueryResult {
    pub fn has_rows(&self) -> bool {
        !self.columns.is_empty()
    }

    /// Footer / message-tab summary, e.g. "32 rows" or "3 rows affected".
    pub fn summary(&self) -> String {
        if self.has_rows() {
            let n = self.rows.len();
            format!("{n} {}", if n == 1 { "row" } else { "rows" })
        } else {
            match self.rows_affected {
                Some(n) => format!("{n} {} affected", if n == 1 { "row" } else { "rows" }),
                None => "Query executed successfully".into(),
            }
        }
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    /// Rows as JSON objects keyed by column name, for AI consumers.
    pub fn rows_as_objects(&self, max_rows: usize) -> Vec<serde_json::Map<String, serde_json::Value>> {
        self.rows
            .iter()
            .take(max_rows)
            .map(|row| {
                self.columns
                    .iter()
                    .zip(row)
                    .map(|(c, v)| (c.name.clone(), serde_json::to_value(v).unwrap_or_default()))
                    .collect()
            })
            .collect()
    }
}

/// Filter operators of the data view's filter bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterOp {
    #[serde(rename = "=")]
    Eq,
    #[serde(rename = "<>")]
    NotEq,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = "<=")]
    LtEq,
    #[serde(rename = ">=")]
    GtEq,
    #[serde(rename = "IN")]
    In,
    #[serde(rename = "NOT IN")]
    NotIn,
    #[serde(rename = "IS NULL")]
    IsNull,
    #[serde(rename = "IS NOT NULL")]
    IsNotNull,
    #[serde(rename = "BETWEEN")]
    Between,
    #[serde(rename = "NOT BETWEEN")]
    NotBetween,
    #[serde(rename = "LIKE")]
    Like,
    #[serde(rename = "ILIKE")]
    ILike,
    #[serde(rename = "Contains")]
    Contains,
    #[serde(rename = "Not contains")]
    NotContains,
    #[serde(rename = "Contains - Case insensitive")]
    ContainsCi,
    #[serde(rename = "Not contains - Case insensitive")]
    NotContainsCi,
    #[serde(rename = "Has prefix")]
    HasPrefix,
    #[serde(rename = "Has suffix")]
    HasSuffix,
    #[serde(rename = "Has prefix - Case insensitive")]
    HasPrefixCi,
    #[serde(rename = "Has suffix - Case insensitive")]
    HasSuffixCi,
}

impl FilterOp {
    pub fn label(self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::NotEq => "<>",
            FilterOp::Lt => "<",
            FilterOp::Gt => ">",
            FilterOp::LtEq => "<=",
            FilterOp::GtEq => ">=",
            FilterOp::In => "IN",
            FilterOp::NotIn => "NOT IN",
            FilterOp::IsNull => "IS NULL",
            FilterOp::IsNotNull => "IS NOT NULL",
            FilterOp::Between => "BETWEEN",
            FilterOp::NotBetween => "NOT BETWEEN",
            FilterOp::Like => "LIKE",
            FilterOp::ILike => "ILIKE",
            FilterOp::Contains => "Contains",
            FilterOp::NotContains => "Not contains",
            FilterOp::ContainsCi => "Contains - Case insensitive",
            FilterOp::NotContainsCi => "Not contains - Case insensitive",
            FilterOp::HasPrefix => "Has prefix",
            FilterOp::HasSuffix => "Has suffix",
            FilterOp::HasPrefixCi => "Has prefix - Case insensitive",
            FilterOp::HasSuffixCi => "Has suffix - Case insensitive",
        }
    }

    pub fn takes_value(self) -> bool {
        !matches!(self, FilterOp::IsNull | FilterOp::IsNotNull)
    }

    /// Placeholder of the value field, as in the original.
    pub fn placeholder(self) -> &'static str {
        match self {
            FilterOp::In | FilterOp::NotIn => "1, 2, 3",
            FilterOp::IsNull | FilterOp::IsNotNull => "",
            FilterOp::Between | FilterOp::NotBetween => "1 AND 100",
            FilterOp::Contains
            | FilterOp::NotContains
            | FilterOp::ContainsCi
            | FilterOp::NotContainsCi
            | FilterOp::HasPrefix
            | FilterOp::HasSuffix
            | FilterOp::HasPrefixCi
            | FilterOp::HasSuffixCi => "Pattern",
            _ => "EMPTY",
        }
    }

    pub fn parse(s: &str) -> Option<FilterOp> {
        serde_json::from_value(serde_json::Value::String(s.into()))
            .ok()
            .or_else(|| {
                let l = s.to_ascii_lowercase();
                ALL_OPS.iter().copied().find(|op| op.label().to_ascii_lowercase() == l)
            })
    }
}

const ALL_OPS: [FilterOp; 22] = [
    FilterOp::Eq,
    FilterOp::NotEq,
    FilterOp::Lt,
    FilterOp::Gt,
    FilterOp::LtEq,
    FilterOp::GtEq,
    FilterOp::In,
    FilterOp::NotIn,
    FilterOp::IsNull,
    FilterOp::IsNotNull,
    FilterOp::Between,
    FilterOp::NotBetween,
    FilterOp::Like,
    FilterOp::ILike,
    FilterOp::Contains,
    FilterOp::NotContains,
    FilterOp::ContainsCi,
    FilterOp::NotContainsCi,
    FilterOp::HasPrefix,
    FilterOp::HasSuffix,
    FilterOp::HasPrefixCi,
    FilterOp::HasSuffixCi,
];

/// Column choice of a filter row: a real column, any column, or raw SQL.
pub const ANY_COLUMN: &str = "Any column";
pub const RAW_SQL: &str = "Raw SQL";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    /// Column name, `"Any column"` or `"Raw SQL"`.
    pub column: String,
    #[serde(rename = "op")]
    pub op: FilterOp,
    #[serde(default)]
    pub value: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sort {
    pub column: String,
    #[serde(default)]
    pub descending: bool,
}

/// One page of the data view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowseRequest {
    pub table: crate::schema::TableRef,
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub sort: Vec<Sort>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u64,
    /// Restrict to these columns; empty = all.
    #[serde(default)]
    pub columns: Vec<String>,
}

fn default_limit() -> u32 {
    300
}

/// What the document view asks for: MongoDB documents matching a query, or
/// a Redis key's value. `filter` and `sort` are shell literals (`{age: 1}`).
#[derive(Clone, Debug)]
pub struct DocumentRequest {
    pub table: crate::schema::TableRef,
    pub filter: String,
    pub sort: String,
    pub skip: u64,
    pub limit: u64,
}

/// One document as the document view shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentEntry {
    /// What edits and deletes address: a MongoDB `_id` literal, a Redis key.
    pub id: Option<String>,
    /// Pretty-printed JSON or shell literal.
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct DocumentPage {
    pub documents: Vec<DocumentEntry>,
    /// Documents matching the filter (not just this page).
    pub total: u64,
    /// The call that read them, for "open in editor".
    pub statement: String,
    /// Facts shown above them: a Redis key's type, TTL, size.
    pub info: Vec<(String, String)>,
}

/// Filters, sorts and pages rows in memory, with the filter bar's meaning,
/// for engines that cannot do it on the server (a Redis hash's fields).
/// "Raw SQL" filters do not apply here and are ignored.
pub fn filter_sort_page(columns: &[String], mut rows: Vec<Vec<Value>>, req: &BrowseRequest) -> Vec<Vec<Value>> {
    rows.retain(|row| req.filters.iter().filter(|f| f.enabled).all(|f| row_matches(columns, row, f)));
    for sort in req.sort.iter().rev() {
        if let Some(ix) = columns.iter().position(|c| *c == sort.column) {
            rows.sort_by(|a, b| {
                let o = compare_values(&a[ix], &b[ix]);
                if sort.descending { o.reverse() } else { o }
            });
        }
    }
    rows.into_iter().skip(req.offset as usize).take(req.limit as usize).collect()
}

/// Number of rows the filters keep.
pub fn count_matching(columns: &[String], rows: &[Vec<Value>], filters: &[Filter]) -> u64 {
    rows.iter().filter(|row| filters.iter().filter(|f| f.enabled).all(|f| row_matches(columns, row, f))).count() as u64
}

fn row_matches(columns: &[String], row: &[Value], f: &Filter) -> bool {
    if f.column == RAW_SQL {
        return true;
    }
    if f.column == ANY_COLUMN {
        return row.iter().any(|v| value_matches(v, f.op, &f.value));
    }
    match columns.iter().position(|c| *c == f.column) {
        Some(ix) => value_matches(&row[ix], f.op, &f.value),
        None => true,
    }
}

fn value_matches(v: &Value, op: FilterOp, want: &str) -> bool {
    if matches!(op, FilterOp::IsNull) {
        return matches!(v, Value::Null);
    }
    if matches!(op, FilterOp::IsNotNull) {
        return !matches!(v, Value::Null);
    }
    if matches!(v, Value::Null) {
        return false;
    }
    let text = v.display().to_string();
    let (lower, want_lower) = (text.to_lowercase(), want.to_lowercase());
    let cmp = |w: &str| compare_values(v, &Value::Text(w.trim().to_string()));
    use std::cmp::Ordering::*;
    match op {
        FilterOp::Eq => cmp(want) == Equal,
        FilterOp::NotEq => cmp(want) != Equal,
        FilterOp::Lt => cmp(want) == Less,
        FilterOp::Gt => cmp(want) == Greater,
        FilterOp::LtEq => cmp(want) != Greater,
        FilterOp::GtEq => cmp(want) != Less,
        FilterOp::In | FilterOp::NotIn => {
            let hit = want.split(',').map(str::trim).filter(|s| !s.is_empty()).any(|w| cmp(w) == Equal);
            hit == (op == FilterOp::In)
        }
        FilterOp::Between | FilterOp::NotBetween => {
            let upper = want.to_ascii_uppercase();
            let Some(at) = upper.find(" AND ") else { return true };
            let inside = cmp(&want[..at]) != Less && cmp(&want[at + 5..]) != Greater;
            inside == (op == FilterOp::Between)
        }
        FilterOp::Like => like(&text, want),
        FilterOp::ILike => like(&lower, &want_lower),
        FilterOp::Contains => text.contains(want),
        FilterOp::NotContains => !text.contains(want),
        FilterOp::ContainsCi => lower.contains(&want_lower),
        FilterOp::NotContainsCi => !lower.contains(&want_lower),
        FilterOp::HasPrefix => text.starts_with(want),
        FilterOp::HasSuffix => text.ends_with(want),
        FilterOp::HasPrefixCi => lower.starts_with(&want_lower),
        FilterOp::HasSuffixCi => lower.ends_with(&want_lower),
        FilterOp::IsNull | FilterOp::IsNotNull => unreachable!(),
    }
}

/// Numbers compare as numbers, everything else as text.
fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    let num = |v: &Value| match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Text(t) => t.trim().parse::<f64>().ok(),
        _ => None,
    };
    match (a, b) {
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
            _ => a.display().cmp(&b.display()),
        },
    }
}

/// SQL LIKE: `%` any run, `_` one character.
fn like(text: &str, pattern: &str) -> bool {
    fn go(t: &[char], p: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('%') => (0..=t.len()).any(|i| go(&t[i..], &p[1..])),
            Some('_') => !t.is_empty() && go(&t[1..], &p[1..]),
            Some(c) => t.first() == Some(c) && go(&t[1..], &p[1..]),
        }
    }
    let (t, p): (Vec<char>, Vec<char>) = (text.chars().collect(), pattern.chars().collect());
    go(&t, &p)
}

#[cfg(test)]
mod memory_tests {
    use super::*;

    #[test]
    fn filters_sorts_and_pages_in_memory() {
        let cols = vec!["member".to_string(), "score".to_string()];
        let rows = vec![
            vec![Value::Text("ann".into()), Value::Float(10.0)],
            vec![Value::Text("Bob".into()), Value::Float(2.5)],
            vec![Value::Text("cy".into()), Value::Float(30.0)],
        ];
        let mut req = BrowseRequest {
            table: crate::schema::TableRef::new(None, "z"),
            filters: vec![Filter { column: "score".into(), op: FilterOp::Gt, value: "5".into(), enabled: true }],
            sort: vec![Sort { column: "score".into(), descending: true }],
            limit: 10,
            offset: 0,
            columns: vec![],
        };
        let out = filter_sort_page(&cols, rows.clone(), &req);
        assert_eq!(out.iter().map(|r| r[0].display().to_string()).collect::<Vec<_>>(), vec!["cy", "ann"]);
        req.filters = vec![Filter { column: ANY_COLUMN.into(), op: FilterOp::ContainsCi, value: "BO".into(), enabled: true }];
        assert_eq!(count_matching(&cols, &rows, &req.filters), 1);
        req.filters = vec![Filter { column: "member".into(), op: FilterOp::Like, value: "_n%".into(), enabled: true }];
        assert_eq!(count_matching(&cols, &rows, &req.filters), 1);
        req.filters.clear();
        req.sort = vec![Sort { column: "score".into(), descending: false }];
        req.offset = 1;
        req.limit = 1;
        assert_eq!(filter_sort_page(&cols, rows, &req)[0][0], Value::Text("ann".into()), "numbers sort as numbers");
    }
}
