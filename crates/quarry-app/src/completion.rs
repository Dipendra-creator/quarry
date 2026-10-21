//! SQL autocomplete for the query editor: keywords, tables, functions and
//! columns, with `alias.` resolved through the statement's FROM/JOIN.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::Result;
use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt as _};
use gpui_kit::{App, Task, Window};
use lsp_types::{CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit, TextEdit};

const KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "AS", "ON", "JOIN", "LEFT JOIN", "RIGHT JOIN",
    "INNER JOIN", "FULL JOIN", "CROSS JOIN", "GROUP BY", "ORDER BY", "HAVING", "LIMIT", "OFFSET", "UNION", "UNION ALL",
    "DISTINCT", "INSERT INTO", "VALUES", "UPDATE", "SET", "DELETE FROM", "CREATE TABLE", "CREATE VIEW", "CREATE INDEX",
    "ALTER TABLE", "DROP TABLE", "TRUNCATE", "ASC", "DESC", "CASE", "WHEN", "THEN", "ELSE", "END", "BETWEEN", "LIKE",
    "ILIKE", "EXISTS", "WITH", "RETURNING", "DEFAULT", "PRIMARY KEY", "FOREIGN KEY", "REFERENCES", "INDEX", "UNIQUE",
    "COUNT", "SUM", "AVG", "MIN", "MAX", "COALESCE", "CAST", "NOW()", "CURRENT_TIMESTAMP", "EXPLAIN", "BEGIN", "COMMIT",
    "ROLLBACK", "TRUE", "FALSE",
];

#[derive(Default)]
pub struct Catalog {
    pub tables: Vec<String>,
    pub functions: Vec<String>,
    /// Lower-cased table name → (column, type).
    pub columns: HashMap<String, Vec<(String, String)>>,
}

impl Catalog {
    pub fn set(&mut self, items: &[quarry_core::Item], columns: Vec<(String, String, String)>) {
        self.tables = items.iter().filter(|i| i.kind.has_rows()).map(|i| i.name().to_string()).collect();
        self.functions = items
            .iter()
            .filter(|i| matches!(i.kind, quarry_core::ItemKind::Function | quarry_core::ItemKind::Procedure))
            .map(|i| i.name().to_string())
            .collect();
        self.columns.clear();
        for (t, c, ty) in columns {
            self.columns.entry(t.to_lowercase()).or_default().push((c, ty));
        }
    }
}

pub struct SqlCompletion {
    pub catalog: Rc<RefCell<Catalog>>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// alias (or table name) → table, from `FROM t [AS] a` / `JOIN t [AS] a`.
fn aliases(sql: &str) -> HashMap<String, String> {
    let words: Vec<&str> = sql
        .split(|c: char| !(is_word(c) || c == '.' || c == '"' || c == '`'))
        .filter(|w| !w.is_empty())
        .collect();
    let mut map = HashMap::new();
    for (i, w) in words.iter().enumerate() {
        if !(w.eq_ignore_ascii_case("from") || w.eq_ignore_ascii_case("join") || w.eq_ignore_ascii_case("update") || w.eq_ignore_ascii_case("into")) {
            continue;
        }
        let Some(table) = words.get(i + 1) else { continue };
        let table = table.rsplit('.').next().unwrap_or(table).trim_matches(['"', '`']).to_string();
        map.insert(table.to_lowercase(), table.clone());
        let mut j = i + 2;
        if words.get(j).is_some_and(|w| w.eq_ignore_ascii_case("as")) {
            j += 1;
        }
        if let Some(alias) = words.get(j) {
            let upper = alias.to_ascii_uppercase();
            if !KEYWORDS.iter().any(|k| k.split(' ').next() == Some(upper.as_str())) && !matches!(upper.as_str(), "LEFT" | "RIGHT" | "INNER" | "FULL" | "CROSS" | "USING") {
                map.insert(alias.to_lowercase(), table);
            }
        }
    }
    map
}

/// Suggestions for the text before the cursor: `(label, kind, detail)`.
pub fn suggest(catalog: &Catalog, statement: &str, prefix: &str) -> Vec<(String, CompletionItemKind, String)> {
    let mut out = Vec::new();
    let starts = |s: &str, p: &str| s.to_lowercase().starts_with(&p.to_lowercase());
    if let Some((qualifier, partial)) = prefix.rsplit_once('.') {
        let qualifier = qualifier.trim_matches(['"', '`']).to_lowercase();
        let table = aliases(statement).get(&qualifier).cloned().unwrap_or(qualifier);
        if let Some(cols) = catalog.columns.get(&table.to_lowercase()) {
            for (c, ty) in cols.iter().filter(|(c, _)| starts(c, partial)) {
                out.push((c.clone(), CompletionItemKind::FIELD, ty.clone()));
            }
        }
        return out;
    }
    if prefix.is_empty() {
        return out;
    }
    let mut seen = std::collections::HashSet::new();
    for table in aliases(statement).values() {
        if let Some(cols) = catalog.columns.get(&table.to_lowercase()) {
            for (c, ty) in cols.iter().filter(|(c, _)| starts(c, prefix)) {
                if seen.insert(c.clone()) {
                    out.push((c.clone(), CompletionItemKind::FIELD, format!("{ty} — {table}")));
                }
            }
        }
    }
    for t in catalog.tables.iter().filter(|t| starts(t, prefix)) {
        out.push((t.clone(), CompletionItemKind::CLASS, "table".into()));
    }
    for f in catalog.functions.iter().filter(|f| starts(f, prefix)) {
        out.push((f.clone(), CompletionItemKind::FUNCTION, "function".into()));
    }
    for k in KEYWORDS.iter().filter(|k| starts(k, prefix)) {
        out.push((k.to_string(), CompletionItemKind::KEYWORD, "keyword".into()));
    }
    out.truncate(200);
    out
}

impl CompletionProvider for SqlCompletion {
    fn completions(&self, rope: &Rope, offset: usize, _: CompletionContext, _: &mut Window, _: &mut App) -> Task<Result<CompletionResponse>> {
        let text = rope.to_string();
        let offset = offset.min(text.len());
        let before = &text[..offset];
        let word_start = before
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word(*c) || *c == '.' || *c == '"' || *c == '`')
            .last()
            .map_or(offset, |(i, _)| i);
        let prefix = &text[word_start..offset];
        let stmt = quarry_core::sql::statement_at(&text, offset).map(|s| s.text).unwrap_or_default();
        let items = suggest(&self.catalog.borrow(), &stmt, prefix);
        let partial_start = prefix.rfind('.').map_or(word_start, |p| word_start + p + 1);
        let range = lsp_types::Range { start: rope.offset_to_position(partial_start), end: rope.offset_to_position(offset) };
        let items = items
            .into_iter()
            .map(|(label, kind, detail)| CompletionItem {
                label: label.clone(),
                kind: Some(kind),
                detail: Some(detail),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit { range, new_text: label })),
                ..Default::default()
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        !new_text.is_empty() && new_text.chars().all(|c| is_word(c) || c == '.')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        let mut c = Catalog::default();
        c.tables = vec!["users".into(), "orders".into()];
        c.columns.insert("users".into(), vec![("id".into(), "int".into()), ("email".into(), "text".into())]);
        c.columns.insert("orders".into(), vec![("id".into(), "int".into()), ("user_id".into(), "int".into())]);
        c
    }

    #[::core::prelude::v1::test]
    fn completes_columns_through_aliases() {
        let c = catalog();
        let labels = |v: Vec<(String, CompletionItemKind, String)>| v.into_iter().map(|x| x.0).collect::<Vec<_>>();
        assert_eq!(labels(suggest(&c, "select u. from users u", "u.")), vec!["id", "email"]);
        assert_eq!(labels(suggest(&c, "select o.us from orders as o join users u on 1=1", "o.us")), vec!["user_id"]);
        assert_eq!(labels(suggest(&c, "select * from public.orders o where o.", "o.")), vec!["id", "user_id"]);
        assert_eq!(labels(suggest(&c, "select em from users", "em")), vec!["email"]);
        let top = labels(suggest(&c, "select * from u", "u"));
        assert!(top.contains(&"users".to_string()) && top.contains(&"UPDATE".to_string()) && top.contains(&"UNION".to_string()));
        assert!(suggest(&c, "select", "").is_empty());
    }
}
