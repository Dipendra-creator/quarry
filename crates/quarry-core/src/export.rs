//! Result export: CSV, JSON and SQL INSERT statements.

use serde::{Deserialize, Serialize};

use crate::query::QueryResult;
use crate::schema::TableRef;
use crate::sql::Dialect;
use crate::value::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Csv,
    Json,
    Sql,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 3] = [ExportFormat::Csv, ExportFormat::Json, ExportFormat::Sql];

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
            ExportFormat::Sql => "sql",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Csv => "CSV",
            ExportFormat::Json => "JSON",
            ExportFormat::Sql => "SQL",
        }
    }
}

/// `table` names the INSERT target for SQL exports.
pub fn export(result: &QueryResult, format: ExportFormat, dialect: Dialect, table: &TableRef) -> String {
    match format {
        ExportFormat::Csv => to_csv(result),
        ExportFormat::Json => serde_json::to_string_pretty(&result.rows_as_objects(usize::MAX)).unwrap_or_default(),
        ExportFormat::Sql => to_inserts(result, dialect, table),
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn to_csv(result: &QueryResult) -> String {
    let mut out = result.columns.iter().map(|c| csv_field(&c.name)).collect::<Vec<_>>().join(",");
    out.push('\n');
    for row in &result.rows {
        let line: Vec<String> = row
            .iter()
            .map(|v| match v {
                Value::Null => String::new(),
                v => csv_field(&v.display()),
            })
            .collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }
    out
}

pub fn to_inserts(result: &QueryResult, dialect: Dialect, table: &TableRef) -> String {
    let cols = result.columns.iter().map(|c| dialect.quote_ident(&c.name)).collect::<Vec<_>>().join(", ");
    result
        .rows
        .iter()
        .map(|row| {
            let vals = row.iter().map(|v| dialect.literal(v)).collect::<Vec<_>>().join(", ");
            format!("INSERT INTO {} ({cols}) VALUES ({vals});\n", dialect.table(table))
        })
        .collect()
}

/// Tab-separated rows for the clipboard ("Copy" in the grid).
pub fn to_tsv(columns: Option<&[String]>, rows: &[Vec<Value>]) -> String {
    let mut out = String::new();
    if let Some(cols) = columns {
        out.push_str(&cols.join("\t"));
        out.push('\n');
    }
    for row in rows {
        out.push_str(&row.iter().map(|v| v.display().replace(['\t', '\n'], " ")).collect::<Vec<_>>().join("\t"));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DriverKind;
    use crate::query::ResultColumn;

    #[test]
    fn formats() {
        let r = QueryResult {
            columns: vec![ResultColumn { name: "a".into(), ..Default::default() }, ResultColumn { name: "b".into(), ..Default::default() }],
            rows: vec![vec![Value::Int(1), Value::Text("x, \"y\"".into())], vec![Value::Null, Value::Text("z".into())]],
            ..Default::default()
        };
        assert_eq!(to_csv(&r), "a,b\n1,\"x, \"\"y\"\"\"\n,z\n");
        assert_eq!(
            to_inserts(&r, Dialect(DriverKind::MySql), &TableRef::new(None, "t")),
            "INSERT INTO `t` (`a`, `b`) VALUES (1, 'x, \"y\"');\nINSERT INTO `t` (`a`, `b`) VALUES (NULL, 'z');\n"
        );
        assert!(export(&r, ExportFormat::Json, Dialect(DriverKind::MySql), &TableRef::new(None, "t")).contains("\"a\": 1"));
    }
}
