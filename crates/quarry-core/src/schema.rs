//! Database objects as the sidebar, the structure view and the AI see them.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Table,
    View,
    MaterializedView,
    Function,
    Procedure,
    Sequence,
}

impl ItemKind {
    /// Sidebar section title.
    pub fn section(self) -> &'static str {
        match self {
            ItemKind::Table => "Tables",
            ItemKind::View => "Views",
            ItemKind::MaterializedView => "Materialized Views",
            ItemKind::Function => "Functions",
            ItemKind::Procedure => "Procedures",
            ItemKind::Sequence => "Sequences",
        }
    }

    pub fn has_rows(self) -> bool {
        matches!(self, ItemKind::Table | ItemKind::View | ItemKind::MaterializedView)
    }
}

/// Fully qualified name of a table-like object.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TableRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
}

impl TableRef {
    pub fn new(schema: Option<&str>, name: &str) -> Self {
        Self {
            schema: schema.filter(|s| !s.is_empty()).map(Into::into),
            name: name.into(),
        }
    }

    pub fn qualified(&self) -> String {
        match &self.schema {
            Some(s) => format!("{s}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    #[serde(flatten)]
    pub table: TableRef,
    pub kind: ItemKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

impl Item {
    pub fn name(&self) -> &str {
        &self.table.name
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub primary_key: bool,
    /// e.g. `auto_increment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// "schema.table(column)" when this column references another table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableStructure {
    pub table: TableRef,
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
}

impl TableStructure {
    pub fn primary_key(&self) -> Vec<String> {
        self.columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.clone())
            .collect()
    }

    pub fn column(&self, name: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// Fills `ColumnInfo::references` from the foreign keys.
    pub fn link_foreign_keys(&mut self) {
        for fk in &self.foreign_keys {
            for (col, ref_col) in fk.columns.iter().zip(&fk.ref_columns) {
                if let Some(c) = self.columns.iter_mut().find(|c| &c.name == col) {
                    let table = TableRef::new(fk.ref_schema.as_deref(), &fk.ref_table);
                    c.references = Some(format!("{}({ref_col})", table.qualified()));
                }
            }
        }
    }
}
