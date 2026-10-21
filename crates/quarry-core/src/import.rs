//! CSV import: parse, map header names to table columns, insert in batches
//! inside one transaction.

use anyhow::{Result, bail};

use crate::schema::{TableRef, TableStructure};
use crate::sql::Dialect;
use crate::value::Value;

/// RFC 4180 CSV: quoted fields may hold the delimiter, quotes ("") and newlines.
pub fn parse_csv(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            c if c == delimiter => row.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r| !(r.len() == 1 && r[0].is_empty()));
    rows
}

/// INSERT statements for CSV rows whose header names table columns.
pub fn insert_statements(
    dialect: Dialect,
    table: &TableRef,
    structure: &TableStructure,
    rows: &[Vec<String>],
    empty_as_null: bool,
    batch: usize,
) -> Result<Vec<String>> {
    let Some((header, data)) = rows.split_first() else { return Ok(vec![]) };
    let columns: Vec<String> = header
        .iter()
        .map(|h| {
            let h = h.trim();
            structure
                .columns
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(h))
                .map(|c| c.name.clone())
                .ok_or_else(|| anyhow::anyhow!("CSV column \"{h}\" is not a column of {}", table.qualified()))
        })
        .collect::<Result<_>>()?;
    if let Some((i, r)) = data.iter().enumerate().find(|(_, r)| r.len() != columns.len()) {
        bail!("CSV line {} has {} fields, expected {}", i + 2, r.len(), columns.len());
    }
    let cols = columns.iter().map(|c| dialect.quote_ident(c)).collect::<Vec<_>>().join(", ");
    Ok(data
        .chunks(batch.max(1))
        .map(|chunk| {
            let values = chunk
                .iter()
                .map(|r| {
                    let vals: Vec<String> = r
                        .iter()
                        .map(|f| match f.as_str() {
                            "" if empty_as_null => "NULL".into(),
                            f => dialect.literal(&Value::Text(f.into())),
                        })
                        .collect();
                    format!("({})", vals.join(", "))
                })
                .collect::<Vec<_>>()
                .join(",\n");
            format!("INSERT INTO {} ({cols}) VALUES\n{values};", dialect.table(table))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DriverKind;
    use crate::schema::ColumnInfo;

    #[test]
    fn csv() {
        let rows = parse_csv("\u{feff}id,name\r\n1,\"Smith, \"\"Jo\"\"\"\n2,\"multi\nline\"\n3,\n", ',');
        assert_eq!(rows, vec![
            vec!["id", "name"],
            vec!["1", "Smith, \"Jo\""],
            vec!["2", "multi\nline"],
            vec!["3", ""],
        ]);
        assert_eq!(parse_csv("a;b\n1;2", ';'), vec![vec!["a", "b"], vec!["1", "2"]]);
    }

    #[test]
    fn inserts() {
        let s = TableStructure {
            table: TableRef::new(None, "t"),
            columns: vec![
                ColumnInfo { name: "id".into(), ..Default::default() },
                ColumnInfo { name: "Name".into(), ..Default::default() },
            ],
            indexes: vec![],
            foreign_keys: vec![],
        };
        let rows = parse_csv("ID,name\n1,a\n2,\n3,c", ',');
        let d = Dialect(DriverKind::Sqlite);
        let sql = insert_statements(d, &s.table, &s, &rows, true, 2).unwrap();
        assert_eq!(sql, vec![
            "INSERT INTO \"t\" (\"id\", \"Name\") VALUES\n('1', 'a'),\n('2', NULL);",
            "INSERT INTO \"t\" (\"id\", \"Name\") VALUES\n('3', 'c');",
        ]);
        assert!(insert_statements(d, &s.table, &s, &parse_csv("nope\n1", ','), true, 10).is_err());
        assert!(insert_statements(d, &s.table, &s, &parse_csv("id,name\n1", ','), true, 10).is_err());
    }
}
