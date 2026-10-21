//! Data grid shared by table data, query results and structure views, with
//! the staged-edit state the original colors yellow/green/red.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::changes::{KeyKind, RowChange, row_key};
use quarry_core::{ResultColumn, Value};

use crate::theme::{PaletteExt as _, UI_TEXT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Normal,
    Inserted,
    Deleted,
}

#[derive(Clone, Debug)]
pub struct GridRow {
    pub original: Vec<Value>,
    pub edits: HashMap<usize, Value>,
    pub state: RowState,
}

impl GridRow {
    pub fn new(values: Vec<Value>) -> Self {
        Self { original: values, edits: HashMap::new(), state: RowState::Normal }
    }

    pub fn value(&self, col: usize) -> &Value {
        self.edits.get(&col).unwrap_or(&self.original[col])
    }

    pub fn current(&self) -> Vec<Value> {
        (0..self.original.len()).map(|c| self.value(c).clone()).collect()
    }
}

/// Grid actions the owning view performs (context menu, header sort).
pub enum GridCommand {
    Sort(usize, ColumnSort),
    CopyCell(usize, usize),
    CopyRows(Vec<usize>),
    SetValue(usize, usize, Value),
    EditCell(usize, usize),
    Duplicate(usize),
    Delete(usize),
    OpenReference(usize, usize),
}

pub type CommandHandler = Rc<dyn Fn(GridCommand, &mut Window, &mut App)>;

pub struct CellEditor {
    pub row: usize,
    pub col: usize,
    pub input: Entity<InputState>,
}

pub struct GridDelegate {
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<GridRow>,
    pub editable: bool,
    pub editing: Option<CellEditor>,
    /// Columns that reference another table (shown with an arrow).
    pub references: Vec<bool>,
    /// Right-aligned columns (numbers, also decimals kept as text).
    pub numeric: Vec<bool>,
    /// Columns that cannot be edited even when the grid is editable.
    pub readonly_cols: Vec<usize>,
    pub widths: Vec<Pixels>,
    pub on_command: Option<CommandHandler>,
}

impl GridDelegate {
    pub fn new() -> Self {
        Self {
            columns: vec![],
            rows: vec![],
            editable: false,
            editing: None,
            references: vec![],
            numeric: vec![],
            readonly_cols: vec![],
            widths: vec![],
            on_command: None,
        }
    }

    pub fn set_data(&mut self, columns: Vec<ResultColumn>, rows: Vec<Vec<Value>>) {
        self.widths = columns.iter().enumerate().map(|(i, c)| guess_width(c, &rows, i)).collect();
        self.references = vec![false; columns.len()];
        self.numeric = columns.iter().map(|c| is_numeric_type(&c.type_name)).collect();
        self.columns = columns;
        self.rows = rows.into_iter().map(GridRow::new).collect();
        self.editing = None;
    }

    pub fn can_edit(&self, col: usize) -> bool {
        self.editable && !self.readonly_cols.contains(&col)
    }

    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|c| c.name.clone()).collect()
    }

    pub fn has_changes(&self) -> bool {
        self.rows.iter().any(|r| r.state != RowState::Normal || !r.edits.is_empty())
    }

    pub fn discard(&mut self) {
        self.rows.retain(|r| r.state != RowState::Inserted);
        for r in &mut self.rows {
            r.edits.clear();
            r.state = RowState::Normal;
        }
        self.editing = None;
    }

    pub fn set_cell(&mut self, row: usize, col: usize, value: Value) {
        let Some(r) = self.rows.get_mut(row) else { return };
        if r.original.get(col) == Some(&value) && r.state != RowState::Inserted {
            r.edits.remove(&col);
        } else {
            r.edits.insert(col, value);
        }
    }

    pub fn insert_row(&mut self, values: Option<Vec<Value>>) -> usize {
        let n = self.columns.len();
        let mut row = GridRow::new(vec![Value::Null; n]);
        row.state = RowState::Inserted;
        if let Some(values) = values {
            for (i, v) in values.into_iter().enumerate().take(n) {
                row.edits.insert(i, v);
            }
        }
        self.rows.push(row);
        self.rows.len() - 1
    }

    pub fn toggle_delete(&mut self, row: usize) {
        let Some(r) = self.rows.get_mut(row) else { return };
        match r.state {
            RowState::Inserted => {
                self.rows.remove(row);
            }
            RowState::Deleted => r.state = RowState::Normal,
            RowState::Normal => r.state = RowState::Deleted,
        }
    }

    /// Staged edits as row changes, keyed by `primary_key` when possible.
    pub fn changes(&self, primary_key: &[String]) -> (Vec<RowChange>, KeyKind) {
        let names = self.column_names();
        let mut key_kind = if primary_key.is_empty() { KeyKind::AllColumns } else { KeyKind::PrimaryKey };
        let mut out = Vec::new();
        for r in &self.rows {
            let edits = || {
                let mut cols: Vec<&usize> = r.edits.keys().collect();
                cols.sort();
                cols.into_iter().map(|c| (names[*c].clone(), r.edits[c].clone())).collect::<Vec<_>>()
            };
            match r.state {
                RowState::Inserted => out.push(RowChange::Insert { values: edits() }),
                RowState::Deleted => {
                    let (key, kind) = row_key(&names, &r.original, primary_key);
                    key_kind = kind;
                    out.push(RowChange::Delete { key });
                }
                RowState::Normal if !r.edits.is_empty() => {
                    let (key, kind) = row_key(&names, &r.original, primary_key);
                    key_kind = kind;
                    out.push(RowChange::Update { key, values: edits() });
                }
                RowState::Normal => {}
            }
        }
        (out, key_kind)
    }

    fn send(&self, cmd: GridCommand, window: &mut Window, cx: &mut App) {
        if let Some(f) = &self.on_command {
            let f = f.clone();
            window.defer(cx, move |window, cx| f(cmd, window, cx));
        }
    }
}

/// Initial column width from header and first rows, like auto-fit.
fn guess_width(c: &ResultColumn, rows: &[Vec<Value>], i: usize) -> Pixels {
    let longest = rows
        .iter()
        .take(50)
        .map(|r| r.get(i).map(|v| v.display().chars().take(60).count()).unwrap_or(0))
        .chain(std::iter::once(c.name.chars().count() + c.type_name.chars().count() + 4))
        .max()
        .unwrap_or(8);
    px((longest as f32 * 7.2 + 20.).clamp(60., 360.))
}

fn is_numeric_type(t: &str) -> bool {
    let t = t.to_ascii_lowercase();
    ["int", "numeric", "decimal", "float", "double", "real", "money", "serial", "oid", "year"]
        .iter()
        .any(|n| t.starts_with(n) || t.contains(&format!(" {n}")))
        || matches!(t.as_str(), "bigint" | "smallint" | "tinyint" | "mediumint")
}

/// One-line cell text: newlines shown as spaces, long values cut.
pub fn cell_text(v: &Value) -> SharedString {
    match v {
        Value::Null => "NULL".into(),
        v => {
            let s = v.display();
            let one_line: String = s.chars().take(300).map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
            one_line.into()
        }
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let c = &self.columns[col_ix];
        Column::new(SharedString::from(format!("c{col_ix}")), c.name.clone())
            .width(self.widths.get(col_ix).copied().unwrap_or(px(120.)))
            .sortable()
            .resizable(true)
    }

    fn render_th(&mut self, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let p = cx.palette().clone();
        let c = &self.columns[col_ix];
        h_flex()
            .size_full()
            .gap(px(4.))
            .text_size(UI_TEXT)
            .child(div().text_color(p.text).font_weight(FontWeight::MEDIUM).truncate().child(c.name.clone()))
            .when(!c.type_name.is_empty(), |d| {
                d.child(div().text_size(px(10.)).text_color(p.secondary_text).child(c.type_name.clone()))
            })
    }

    fn perform_sort(&mut self, col_ix: usize, sort: ColumnSort, window: &mut Window, cx: &mut Context<TableState<Self>>) {
        self.send(GridCommand::Sort(col_ix, sort), window, cx);
    }

    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let p = cx.palette().clone();
        if let Some(ed) = &self.editing {
            if ed.row == row_ix && ed.col == col_ix {
                return div().size_full().child(Input::new(&ed.input).xsmall().appearance(false)).into_any_element();
            }
        }
        let row = &self.rows[row_ix];
        let v = row.value(col_ix);
        let bg = match row.state {
            RowState::Deleted => Some(p.deleted),
            RowState::Inserted => Some(p.added),
            RowState::Normal if row.edits.contains_key(&col_ix) => Some(p.modified),
            RowState::Normal => None,
        };
        let numeric = v.is_numeric() || (self.numeric.get(col_ix).copied().unwrap_or(false) && !v.is_null());
        div()
            .size_full()
            .flex()
            .items_center()
            .px(px(4.))
            .text_size(UI_TEXT)
            .when_some(bg, |d, bg| d.bg(bg))
            .when(numeric, |d| d.justify_end())
            .child(
                div()
                    .truncate()
                    .text_color(if v.is_null() { p.null_value } else { p.data_text })
                    .when(v.is_null(), |d| d.italic())
                    .child(cell_text(v)),
            )
            .into_any_element()
    }

    fn context_menu(&mut self, row_ix: usize, menu: PopupMenu, _: &mut Window, cx: &mut Context<TableState<Self>>) -> PopupMenu {
        let Some(on) = self.on_command.clone() else { return menu };
        let col = cx.entity().read(cx).selected_cell().filter(|(r, _)| *r == row_ix).map(|(_, c)| c).unwrap_or(0);
        let item = |label: &'static str, make: Rc<dyn Fn() -> GridCommand>| {
            let on = on.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| on(make(), window, cx))
        };
        let mut menu = menu
            .item(item("Copy", Rc::new(move || GridCommand::CopyCell(row_ix, col))))
            .item(item("Copy Row", Rc::new(move || GridCommand::CopyRows(vec![row_ix]))));
        if self.references.get(col).copied().unwrap_or(false) {
            menu = menu.item(item("Open Referenced Row", Rc::new(move || GridCommand::OpenReference(row_ix, col))));
        }
        if self.editable {
            menu = menu
                .separator()
                .item(item("Edit Cell", Rc::new(move || GridCommand::EditCell(row_ix, col))))
                .item(item("Set as NULL", Rc::new(move || GridCommand::SetValue(row_ix, col, Value::Null))))
                .item(item("Set as Empty String", Rc::new(move || GridCommand::SetValue(row_ix, col, Value::Text(String::new())))))
                .separator()
                .item(item("Duplicate Row", Rc::new(move || GridCommand::Duplicate(row_ix))))
                .item(item("Delete Row", Rc::new(move || GridCommand::Delete(row_ix))));
        }
        menu
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.rows.get(row_ix).map(|r| r.value(col_ix).display()).unwrap_or_default()
    }
}

/// A read-only grid entity for the given data.
pub fn readonly_table(columns: Vec<ResultColumn>, rows: Vec<Vec<Value>>, window: &mut Window, cx: &mut App) -> Entity<TableState<GridDelegate>> {
    let mut d = GridDelegate::new();
    d.set_data(columns, rows);
    cx.new(|cx| TableState::new(d, window, cx).cell_selectable(true).col_movable(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> GridDelegate {
        let mut g = GridDelegate::new();
        g.set_data(
            vec![ResultColumn { name: "id".into(), ..Default::default() }, ResultColumn { name: "name".into(), ..Default::default() }],
            vec![vec![Value::Int(1), "a".into()], vec![Value::Int(2), "b".into()]],
        );
        g
    }

    #[::core::prelude::v1::test]
    fn staged_changes() {
        let mut g = grid();
        g.set_cell(0, 1, "z".into());
        g.toggle_delete(1);
        let r = g.insert_row(Some(vec![Value::Int(3), "c".into()]));
        assert_eq!(r, 2);
        let (changes, kind) = g.changes(&["id".into()]);
        assert_eq!(kind, KeyKind::PrimaryKey);
        assert_eq!(changes, vec![
            RowChange::Update { key: vec![("id".into(), Value::Int(1))], values: vec![("name".into(), "z".into())] },
            RowChange::Delete { key: vec![("id".into(), Value::Int(2))] },
            RowChange::Insert { values: vec![("id".into(), Value::Int(3)), ("name".into(), "c".into())] },
        ]);
        // Setting a cell back to its original value drops the edit.
        g.set_cell(0, 1, "a".into());
        g.discard();
        assert!(!g.has_changes());
        assert_eq!(g.rows.len(), 2);
    }
}
