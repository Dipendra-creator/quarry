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
