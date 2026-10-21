//! Table tab: data grid with filters, paging and staged edits, plus the
//! editable structure view (TableDataController / TableStructureController).

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::table::{ColumnSort, DataTable, TableEvent, TableState};
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::alter::{ColumnDef, StructureChange};
use quarry_core::query::{ANY_COLUMN, RAW_SQL};
use quarry_core::{BrowseRequest, Filter, FilterOp, Item, ItemKind, ResultColumn, Session, Sort, TableRef, TableStructure, Value};

use crate::grid::{CellEditor, GridCommand, GridDelegate, RowState};
use crate::state::db;
use crate::theme::{MONO, PaletteExt as _, UI_TEXT};
use crate::widgets::{footer_button, icon, segmented};
use crate::{guard, prompt};

const PG_OPS: [FilterOp; 22] = [
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

/// Operators each engine offers in the filter bar, as in the original.
pub fn operators(driver: quarry_core::DriverKind) -> Vec<FilterOp> {
    if driver == quarry_core::DriverKind::Postgres {
        PG_OPS.to_vec()
    } else {
        PG_OPS
            .iter()
            .copied()
            .filter(|o| !matches!(o, FilterOp::ILike | FilterOp::ContainsCi | FilterOp::NotContainsCi | FilterOp::HasPrefixCi | FilterOp::HasSuffixCi))
            .collect()
    }
}

/// The selected row, for the right-hand detail pane.
#[derive(Clone)]
pub struct RowSnapshot {
    pub row: usize,
    pub columns: Vec<ResultColumn>,
    pub values: Vec<Value>,
    pub editable: bool,
}

pub enum DataViewEvent {
    /// Staged changes, selection or loading state changed.
    Changed,
    Log(String),
    RowSelected(Option<RowSnapshot>),
    OpenTable(Item, Vec<Filter>),
    OpenSql(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Data,
    Structure,
}

/// The three editable grids of a table tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grid {
    Data,
    Columns,
    Indexes,
}

// Column layout of the structure grids.
const COLUMNS_HEADERS: [&str; 8] = ["column_name", "data_type", "is_nullable", "column_default", "comment", "primary_key", "extra", "foreign_key"];
const COLUMNS_READONLY: [usize; 3] = [5, 6, 7];
const INDEX_HEADERS: [&str; 6] = ["index_name", "columns", "is_unique", "is_primary", "method", "definition"];
const INDEX_READONLY: [usize; 3] = [3, 4, 5];

struct FilterRow {
    enabled: bool,
    column: String,
    op: FilterOp,
    value: Entity<InputState>,
}

pub struct DataView {
    pub session: Arc<Session>,
    pub item: Item,
    pub structure: Option<TableStructure>,
    pub table: Entity<TableState<GridDelegate>>,
    mode: Mode,
    columns_table: Option<Entity<TableState<GridDelegate>>>,
    indexes_table: Option<Entity<TableState<GridDelegate>>>,
    fks_table: Option<Entity<TableState<GridDelegate>>>,
    editing: Option<Grid>,
    filters: Vec<FilterRow>,
    show_filters: bool,
    sort: Vec<Sort>,
    offset: u64,
    limit: u32,
    limit_input: Entity<InputState>,
    offset_input: Entity<InputState>,
    total: Option<u64>,
    loading: bool,
    error: Option<String>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DataViewEvent> for DataView {}

fn yes(b: bool) -> Value {
    Value::Text(if b { "YES" } else { "NO" }.into())
}

fn opt(o: &Option<String>) -> Value {
    o.clone().map(Value::Text).unwrap_or(Value::Null)
}

fn text(v: &Value) -> Option<String> {
    v.as_edit_text().filter(|s| !s.is_empty())
}

/// A column definition from a row of the columns grid.
fn column_def(values: &[Value]) -> ColumnDef {
    ColumnDef {
        name: values[0].display(),
        data_type: values[1].display(),
        nullable: !values[2].display().eq_ignore_ascii_case("no"),
        default: text(&values[3]),
        comment: text(&values[4]),
    }
}

impl DataView {
    pub fn new(session: Arc<Session>, item: Item, filters: Vec<Filter>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let limit = session.config.page_size.max(1);
        let table = Self::make_grid(Grid::Data, item.kind == ItemKind::Table, vec![], window, cx);
        let limit_input = cx.new(|cx| InputState::new(window, cx).default_value(limit.to_string()));
        let offset_input = cx.new(|cx| InputState::new(window, cx).default_value("0"));
        let subs = vec![cx.subscribe_in(&table, window, |v, _, ev, window, cx| v.on_table_event(Grid::Data, ev, window, cx))];
        let mut view = Self {
            session,
            item,
            structure: None,
            table,
            mode: Mode::Data,
            columns_table: None,
            indexes_table: None,
            fks_table: None,
            editing: None,
            filters: vec![],
            show_filters: !filters.is_empty(),
            sort: vec![],
            offset: 0,
            limit,
            limit_input,
            offset_input,
            total: None,
            loading: false,
            error: None,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        for f in filters {
            view.add_filter(Some(f), window, cx);
        }
        view.load_structure(true, window, cx);
        view
    }

    fn make_grid(grid: Grid, editable: bool, readonly: Vec<usize>, window: &mut Window, cx: &mut Context<Self>) -> Entity<TableState<GridDelegate>> {
        let mut delegate = GridDelegate::new();
        delegate.editable = editable;
        delegate.readonly_cols = readonly;
        let this = cx.entity().downgrade();
        delegate.on_command = Some(std::rc::Rc::new(move |cmd, window, cx| {
            let _ = this.update(cx, |v, cx| v.command(grid, cmd, window, cx));
        }));
        cx.new(|cx| TableState::new(delegate, window, cx).cell_selectable(true).col_movable(false))
    }

    fn grid(&self, g: Grid) -> Option<Entity<TableState<GridDelegate>>> {
        match g {
            Grid::Data => Some(self.table.clone()),
            Grid::Columns => self.columns_table.clone(),
            Grid::Indexes => self.indexes_table.clone(),
        }
    }

    fn grids(&self) -> Vec<Entity<TableState<GridDelegate>>> {
        [Grid::Data, Grid::Columns, Grid::Indexes].into_iter().filter_map(|g| self.grid(g)).collect()
    }

    pub fn title(&self) -> String {
        self.item.name().to_string()
    }

    pub fn has_changes(&self, cx: &App) -> bool {
        self.grids().iter().any(|g| g.read(cx).delegate().has_changes())
    }

    fn table_ref(&self) -> TableRef {
        self.item.table.clone()
    }

    fn on_table_event(&mut self, grid: Grid, ev: &TableEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            TableEvent::DoubleClickedCell(r, c) => self.edit_cell(grid, *r, *c, window, cx),
            TableEvent::SelectCell(r, _) | TableEvent::SelectRow(r) if grid == Grid::Data => {
                let snap = self.snapshot(*r, cx);
                cx.emit(DataViewEvent::RowSelected(snap));
            }
            TableEvent::ClearSelection if grid == Grid::Data => cx.emit(DataViewEvent::RowSelected(None)),
            _ => {}
        }
    }

    pub fn snapshot(&self, row: usize, cx: &App) -> Option<RowSnapshot> {
        let d = self.table.read(cx).delegate();
        let r = d.rows.get(row)?;
        Some(RowSnapshot { row, columns: d.columns.clone(), values: r.current(), editable: d.editable && r.state != RowState::Deleted })
    }

    fn selected_row(&self, grid: Grid, cx: &App) -> Option<usize> {
        let t = self.grid(grid)?;
        let t = t.read(cx);
        t.selected_cell().map(|(r, _)| r).or(t.selected_row())
    }

    // ---- loading -------------------------------------------------------

    fn load_structure(&mut self, then_reload: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.item.kind.has_rows() {
            return;
        }
        let (session, t) = (self.session.clone(), self.table_ref());
        cx.spawn_in(window, async move |this, cx| {
            let r = db(async move { session.structure(&t).await }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Ok(s) = r {
                    this.set_structure(s, window, cx);
                }
                if then_reload {
                    // First page after the structure, so it can be ordered by primary key.
                    this.reload(window, cx);
                }
            });
        })
        .detach();
    }

    fn set_structure(&mut self, s: TableStructure, window: &mut Window, cx: &mut Context<Self>) {
        let editable = self.item.kind == ItemKind::Table;
        let headers = |h: &[&str]| h.iter().map(|n| ResultColumn { name: n.to_string(), type_name: String::new() }).collect::<Vec<_>>();
        let columns = Self::make_grid(Grid::Columns, editable, COLUMNS_READONLY.to_vec(), window, cx);
        columns.update(cx, |t, cx| {
            t.delegate_mut().set_data(
                headers(&COLUMNS_HEADERS),
                s.columns
                    .iter()
                    .map(|c| {
                        vec![
                            c.name.clone().into(),
                            c.data_type.clone().into(),
                            yes(c.nullable),
                            opt(&c.default),
                            opt(&c.comment),
                            yes(c.primary_key),
                            opt(&c.extra),
                            opt(&c.references),
                        ]
                    })
                    .collect(),
            );
            t.refresh(cx);
        });
        let indexes = Self::make_grid(Grid::Indexes, editable, INDEX_READONLY.to_vec(), window, cx);
        indexes.update(cx, |t, cx| {
            t.delegate_mut().set_data(
                headers(&INDEX_HEADERS),
                s.indexes
                    .iter()
                    .map(|i| vec![i.name.clone().into(), i.columns.join(", ").into(), yes(i.unique), yes(i.primary), opt(&i.method), opt(&i.definition)])
                    .collect(),
            );
            t.refresh(cx);
        });
        let fks = Self::make_grid(Grid::Data, false, vec![], window, cx);
        fks.update(cx, |t, cx| {
            t.delegate_mut().set_data(
                headers(&["constraint_name", "columns", "referenced_table", "referenced_columns", "on_update", "on_delete"]),
                s.foreign_keys
                    .iter()
                    .map(|f| {
                        vec![
                            f.name.clone().into(),
                            f.columns.join(", ").into(),
                            TableRef::new(f.ref_schema.as_deref(), &f.ref_table).qualified().into(),
                            f.ref_columns.join(", ").into(),
                            f.on_update.clone().into(),
                            f.on_delete.clone().into(),
                        ]
                    })
                    .collect(),
            );
            t.refresh(cx);
        });
        self._subs.push(cx.subscribe_in(&columns, window, |v, _, ev, window, cx| v.on_table_event(Grid::Columns, ev, window, cx)));
        self._subs.push(cx.subscribe_in(&indexes, window, |v, _, ev, window, cx| v.on_table_event(Grid::Indexes, ev, window, cx)));
        self.columns_table = Some(columns);
        self.indexes_table = Some(indexes);
        self.fks_table = Some(fks);
        self.structure = Some(s);
        self.mark_references(cx);
        cx.emit(DataViewEvent::Changed);
        cx.notify();
    }

    fn mark_references(&mut self, cx: &mut Context<Self>) {
        let Some(s) = &self.structure else { return };
        let refs: Vec<String> = s.columns.iter().filter(|c| c.references.is_some()).map(|c| c.name.clone()).collect();
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.references = d.columns.iter().map(|c| refs.contains(&c.name)).collect();
            cx.notify();
        });
    }

    fn active_filters(&self, cx: &App) -> Vec<Filter> {
        self.filters
            .iter()
            .map(|f| Filter { column: f.column.clone(), op: f.op, value: f.value.read(cx).value().to_string(), enabled: f.enabled })
            .filter(|f| f.enabled && !f.column.is_empty())
            .collect()
    }

    fn browse_request(&self, cx: &App) -> BrowseRequest {
        let sort = match (&self.structure, self.sort.is_empty()) {
            (Some(s), true) => s.primary_key().into_iter().map(|column| Sort { column, descending: false }).collect(),
            _ => self.sort.clone(),
        };
        BrowseRequest { table: self.table_ref(), filters: self.active_filters(cx), sort, limit: self.limit, offset: self.offset, columns: vec![] }
    }

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.item.kind.has_rows() {
            return;
        }
        let req = self.browse_request(cx);
        self.loading = true;
        self.error = None;
        cx.notify();
        let session = self.session.clone();
        let count_filters = req.filters.clone();
        let t = self.table_ref();
        cx.spawn_in(window, async move |this, cx| {
            let s2 = session.clone();
            let r = db(async move { s2.browse(&req).await }).await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.loading = false;
                match r {
                    Ok(result) => {
                        cx.emit(DataViewEvent::Log(result.statement.clone()));
                        this.table.update(cx, |t, cx| {
                            t.delegate_mut().set_data(result.columns, result.rows);
                            t.refresh(cx);
                            cx.notify();
                        });
                        this.mark_references(cx);
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.emit(DataViewEvent::Changed);
                cx.emit(DataViewEvent::RowSelected(None));
                cx.notify();
            });
            let count = db(async move { session.count(&t, &count_filters).await }).await;
            let _ = this.update(cx, |this, cx| {
                this.total = count.ok();
                cx.notify();
            });
        })
        .detach();
    }

    fn page(&mut self, delta: i64, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.offset as i64 + delta * self.limit as i64;
        if next < 0 || (delta > 0 && self.total.is_some_and(|t| next as u64 >= t)) {
            return;
        }
        self.offset = next as u64;
        self.sync_paging_inputs(window, cx);
        self.reload(window, cx);
    }

    fn sync_paging_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (l, o) = (self.limit.to_string(), self.offset.to_string());
        self.limit_input.update(cx, |s, cx| s.set_value(l, window, cx));
        self.offset_input.update(cx, |s, cx| s.set_value(o, window, cx));
    }

    fn apply_paging(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.limit = self.limit_input.read(cx).value().trim().parse().unwrap_or(self.limit).max(1);
        self.offset = self.offset_input.read(cx).value().trim().parse().unwrap_or(0);
        self.reload(window, cx);
    }

    // ---- filters ---------------------------------------------------------

    pub fn toggle_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_filters = !self.show_filters;
        if self.show_filters && self.filters.is_empty() {
            self.add_filter(None, window, cx);
        }
        if let Some(f) = self.filters.first() {
            let h = f.value.read(cx).focus_handle(cx);
            window.focus(&h, cx);
        }
        cx.notify();
    }

    fn add_filter(&mut self, f: Option<Filter>, window: &mut Window, cx: &mut Context<Self>) {
        let f = f.unwrap_or_else(|| Filter { column: ANY_COLUMN.into(), op: FilterOp::ContainsCi, value: String::new(), enabled: true });
        let op = if operators(self.session.kind()).contains(&f.op) { f.op } else { FilterOp::Contains };
        let value = cx.new(|cx| InputState::new(window, cx).placeholder(op.placeholder()).default_value(f.value.clone()));
        self._subs.push(cx.subscribe_in(&value, window, |this, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                this.offset = 0;
                this.reload(window, cx);
            }
        }));
        self.filters.push(FilterRow { enabled: f.enabled, column: f.column, op, value });
        cx.notify();
    }

    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filters.clear();
        self.add_filter(None, window, cx);
        self.offset = 0;
        self.reload(window, cx);
    }

    fn sort_by(&mut self, col: usize, sort: ColumnSort, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.table.read(cx).delegate().columns.get(col).map(|c| c.name.clone());
        self.sort = match (name, sort) {
            (Some(column), ColumnSort::Ascending) => vec![Sort { column, descending: false }],
            (Some(column), ColumnSort::Descending) => vec![Sort { column, descending: true }],
            _ => vec![],
        };
        self.reload(window, cx);
    }

    // ---- editing ---------------------------------------------------------

    fn command(&mut self, grid: Grid, cmd: GridCommand, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.grid(grid) else { return };
        match cmd {
            GridCommand::Sort(col, sort) if grid == Grid::Data => self.sort_by(col, sort, window, cx),
            GridCommand::Sort(..) => {}
            GridCommand::CopyCell(r, c) => {
                let text = t.read(cx).delegate().rows.get(r).map(|row| row.value(c).display()).unwrap_or_default();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            GridCommand::CopyRows(rows) => {
                let d = t.read(cx).delegate();
                let data: Vec<Vec<Value>> = rows.iter().filter_map(|r| d.rows.get(*r).map(|r| r.current())).collect();
                cx.write_to_clipboard(ClipboardItem::new_string(quarry_core::export::to_tsv(None, &data)));
            }
            GridCommand::SetValue(r, c, v) => self.set_cell(grid, r, c, v, cx),
            GridCommand::EditCell(r, c) => self.edit_cell(grid, r, c, window, cx),
            GridCommand::Duplicate(r) => self.duplicate_row(grid, Some(r), window, cx),
            GridCommand::Delete(r) => {
                t.update(cx, |t, cx| {
                    t.delegate_mut().toggle_delete(r);
                    t.refresh(cx);
                    cx.notify();
                });
                cx.emit(DataViewEvent::Changed);
            }
            GridCommand::OpenReference(r, c) => self.open_reference(r, c, cx),
        }
    }

    pub fn set_cell(&mut self, grid: Grid, row: usize, col: usize, value: Value, cx: &mut Context<Self>) {
        let Some(t) = self.grid(grid) else { return };
        if !t.read(cx).delegate().can_edit(col) {
            return;
        }
        t.update(cx, |t, cx| {
            t.delegate_mut().set_cell(row, col, value);
            cx.notify();
        });
        cx.emit(DataViewEvent::Changed);
        if grid == Grid::Data {
            let snap = self.snapshot(row, cx);
            cx.emit(DataViewEvent::RowSelected(snap));
        }
    }

    fn edit_cell(&mut self, grid: Grid, row: usize, col: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.grid(grid) else { return };
        if !t.read(cx).delegate().can_edit(col) {
            return;
        }
        if self.editing.is_some() {
            self.finish_edit(true, window, cx);
        }
        let current = t.read(cx).delegate().rows.get(row).map(|r| r.value(col).as_edit_text().unwrap_or_default());
        let Some(current) = current else { return };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        self._subs.push(cx.subscribe_in(&input, window, move |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.finish_edit(true, window, cx),
            _ => {}
        }));
        let focus = input.read(cx).focus_handle(cx);
        t.update(cx, |t, cx| {
            t.delegate_mut().editing = Some(CellEditor { row, col, input });
            cx.notify();
        });
        self.editing = Some(grid);
        window.focus(&focus, cx);
    }

    fn finish_edit(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(grid) = self.editing.take() else { return };
        let Some(t) = self.grid(grid) else { return };
        let Some(ed) = t.update(cx, |t, cx| {
            cx.notify();
            t.delegate_mut().editing.take()
        }) else {
            return;
        };
        if commit {
            let text = ed.input.read(cx).value().to_string();
            let current = t.read(cx).delegate().rows.get(ed.row).map(|r| r.value(ed.col).clone());
            let value = match &current {
                // Leaving a NULL cell empty keeps it NULL.
                Some(Value::Null) if text.is_empty() => None,
                Some(v) if v.display() == text => None,
                _ => Some(Value::Text(text)),
            };
            if let Some(value) = value {
                self.set_cell(grid, ed.row, ed.col, value, cx);
            }
        }
        window.focus(&self.focus, cx);
    }

    /// "+ Row" / "+ Column" / "+ Index".
    pub fn add_row_to(&mut self, grid: Grid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.grid(grid) else { return };
        if !t.read(cx).delegate().editable {
            return;
        }
        let defaults = match grid {
            Grid::Data => None,
            Grid::Columns => Some(vec![
                Value::Text(String::new()),
                Value::Text(self.default_type().into()),
                yes(true),
                Value::Null,
                Value::Null,
                yes(false),
                Value::Null,
                Value::Null,
            ]),
            Grid::Indexes => Some(vec![Value::Text(format!("{}_idx", self.item.name())), Value::Text(String::new()), yes(false), yes(false), Value::Null, Value::Null]),
        };
        let row = t.update(cx, |t, cx| {
            let r = t.delegate_mut().insert_row(defaults);
            t.refresh(cx);
            t.scroll_to_row(r, cx);
            r
        });
        cx.emit(DataViewEvent::Changed);
        self.edit_cell(grid, row, if grid == Grid::Indexes { 1 } else { 0 }, window, cx);
    }

    pub fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let grid = if self.mode == Mode::Structure { Grid::Columns } else { Grid::Data };
        self.add_row_to(grid, window, cx);
    }

    fn default_type(&self) -> &'static str {
        match self.session.kind() {
            quarry_core::DriverKind::Postgres | quarry_core::DriverKind::Sqlite => "text",
            _ => "varchar(255)",
        }
    }

    pub fn duplicate_row(&mut self, grid: Grid, row: Option<usize>, _: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.grid(grid) else { return };
        let Some(row) = row.or(self.selected_row(grid, cx)) else { return };
        if !t.read(cx).delegate().editable {
            return;
        }
        let pk = if grid == Grid::Data { self.structure.as_ref().map(|s| s.primary_key()).unwrap_or_default() } else { vec![] };
        t.update(cx, |t, cx| {
            let d = t.delegate_mut();
            let Some(src) = d.rows.get(row) else { return };
            // Primary-key values are left out so the database assigns new ones.
            let values: Vec<Value> = d
                .columns
                .iter()
                .enumerate()
                .map(|(i, c)| if pk.contains(&c.name) { Value::Null } else { src.value(i).clone() })
                .collect();
            let r = d.insert_row(None);
            for (i, v) in values.into_iter().enumerate() {
                if !v.is_null() {
                    d.set_cell(r, i, v);
                }
            }
            t.refresh(cx);
            t.scroll_to_row(r, cx);
            cx.notify();
        });
        cx.emit(DataViewEvent::Changed);
    }

    pub fn delete_selected(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let grid = if self.mode == Mode::Structure { Grid::Columns } else { Grid::Data };
        let Some(t) = self.grid(grid) else { return };
        let Some(row) = self.selected_row(grid, cx) else { return };
        if !t.read(cx).delegate().editable {
            return;
        }
        t.update(cx, |t, cx| {
            t.delegate_mut().toggle_delete(row);
            t.refresh(cx);
            cx.notify();
        });
        cx.emit(DataViewEvent::Changed);
    }

    pub fn duplicate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.duplicate_row(Grid::Data, None, window, cx);
    }

    pub fn discard(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        for t in self.grids() {
            t.update(cx, |t, cx| {
                t.delegate_mut().discard();
                t.refresh(cx);
                cx.notify();
            });
        }
        cx.emit(DataViewEvent::Changed);
    }

    /// Staged structure edits as changes the core turns into ALTER statements.
    fn structure_changes(&self, cx: &App) -> Vec<StructureChange> {
        let mut out = Vec::new();
        if let Some(t) = &self.columns_table {
            for r in &t.read(cx).delegate().rows {
                match r.state {
                    RowState::Inserted => out.push(StructureChange::AddColumn { column: column_def(&r.current()) }),
                    RowState::Deleted => out.push(StructureChange::DropColumn { name: r.original[0].display() }),
                    RowState::Normal if !r.edits.is_empty() => {
                        out.push(StructureChange::ModifyColumn { old: column_def(&r.original), new: column_def(&r.current()) })
                    }
                    RowState::Normal => {}
                }
            }
        }
        if let Some(t) = &self.indexes_table {
            let add = |v: &[Value]| StructureChange::AddIndex {
                name: v[0].display(),
                columns: v[1].display().split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect(),
                unique: v[2].display().eq_ignore_ascii_case("yes"),
            };
            for r in &t.read(cx).delegate().rows {
                match r.state {
                    RowState::Inserted => out.push(add(&r.current())),
                    RowState::Deleted => out.push(StructureChange::DropIndex { name: r.original[0].display() }),
                    RowState::Normal if !r.edits.is_empty() => {
                        out.push(StructureChange::DropIndex { name: r.original[0].display() });
                        out.push(add(&r.current()));
                    }
                    RowState::Normal => {}
                }
            }
        }
        out
    }

    fn pending_sql(&self, cx: &App) -> Result<Vec<String>, String> {
        let mut sql = self.session.dialect().structure_sql(&self.table_ref(), &self.structure_changes(cx)).map_err(|e| e.to_string())?;
        let pk = self.structure.as_ref().map(|s| s.primary_key()).unwrap_or_default();
        let (changes, kind) = self.table.read(cx).delegate().changes(&pk);
        sql.extend(self.session.changes_sql(&self.table_ref(), &changes, kind));
        Ok(sql)
    }

    pub fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text: SharedString = match self.pending_sql(cx) {
            Ok(sql) if sql.is_empty() => "No changes.".into(),
            Ok(sql) => sql.join("\n").into(),
            Err(e) => e.into(),
        };
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog.title("Preview changes").w(px(640.)).child(
                div()
                    .id("preview")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .p(px(8.))
                    .rounded(px(4.))
                    .bg(cx.palette().window_secondary)
                    .font_family(MONO)
                    .text_size(UI_TEXT)
                    .child(text.clone()),
            )
        });
    }

    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.finish_edit(true, window, cx);
        }
        let sql = match self.pending_sql(cx) {
            Ok(sql) if sql.is_empty() => return,
            Ok(sql) => sql.join("\n"),
            Err(e) => return prompt::error("Cannot apply structure changes", &e, window, cx),
        };
        let structure = self.structure_changes(cx);
        let pk = self.structure.as_ref().map(|s| s.primary_key()).unwrap_or_default();
        let (changes, kind) = self.table.read(cx).delegate().changes(&pk);
        let this = cx.entity().downgrade();
        let (session, t) = (self.session.clone(), self.table_ref());
        guard::guarded(&self.session.clone(), &sql, window, cx, move |window, cx| {
            let _ = this.update(cx, |_, cx| {
                cx.spawn_in(window, async move |this, cx| {
                    let r = db(async move {
                        let mut run = session.apply_structure(&t, &structure).await?;
                        run.extend(session.apply_changes(&t, &changes, kind).await?);
                        Ok(run)
                    })
                    .await;
                    let _ = this.update_in(cx, |this, window, cx| match r {
                        Ok(stmts) => {
                            for s in stmts {
                                cx.emit(DataViewEvent::Log(s));
                            }
                            this.load_structure(true, window, cx);
                        }
                        Err(e) => prompt::error("Could not commit changes", &format!("{e:#}"), window, cx),
                    });
                })
                .detach();
            });
        });
    }

    fn open_reference(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        let Some(s) = &self.structure else { return };
        let d = self.table.read(cx).delegate();
        let Some(name) = d.columns.get(col).map(|c| c.name.clone()) else { return };
        let Some(fk) = s.foreign_keys.iter().find(|f| f.columns.contains(&name)) else { return };
        let pos = fk.columns.iter().position(|c| *c == name).unwrap_or(0);
        let value = d.rows.get(row).map(|r| r.value(col).display()).unwrap_or_default();
        let target = Item {
            table: TableRef::new(fk.ref_schema.as_deref().or(self.item.table.schema.as_deref()), &fk.ref_table),
            kind: ItemKind::Table,
            comment: None,
        };
        let filter = Filter { column: fk.ref_columns[pos].clone(), op: FilterOp::Eq, value, enabled: true };
        cx.emit(DataViewEvent::OpenTable(target, vec![filter]));
    }

    pub fn show_ddl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (session, item) = (self.session.clone(), self.item.clone());
        cx.spawn_in(window, async move |this, cx| {
            let r = db(async move { session.ddl(&item).await }).await;
            let _ = this.update_in(cx, |_, window, cx| match r {
                Ok(ddl) => cx.emit(DataViewEvent::OpenSql(ddl)),
                Err(e) => prompt::error("Could not load DDL", &format!("{e:#}"), window, cx),
            });
        })
        .detach();
    }

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        cx.notify();
    }

    pub fn export(&mut self, format: quarry_core::export::ExportFormat, window: &mut Window, cx: &mut Context<Self>) {
        let mut req = self.browse_request(cx);
        req.limit = u32::MAX;
        req.offset = 0;
        let (session, t) = (self.session.clone(), self.table_ref());
        let rx = cx.prompt_for_new_path(&crate::backup::home(), Some(&format!("{}.{}", self.item.name(), format.extension())));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let r = db(async move {
                let result = session.browse(&req).await?;
                let text = quarry_core::export::export(&result, format, session.dialect(), &t);
                std::fs::write(&path, text)?;
                Ok(result.rows.len())
            })
            .await;
            let _ = cx.update(|window, cx| match r {
                Ok(n) => prompt::info("Export finished", &format!("Exported {n} rows."), window, cx),
                Err(e) => prompt::error("Export failed", &format!("{e:#}"), window, cx),
            });
        })
        .detach();
    }

    // ---- rendering -------------------------------------------------------

    fn column_choices(&self, cx: &App) -> Vec<String> {
        let mut cols = vec![ANY_COLUMN.to_string()];
        match &self.structure {
            Some(s) => cols.extend(s.columns.iter().map(|c| c.name.clone())),
            None => cols.extend(self.table.read(cx).delegate().column_names()),
        }
        cols.push(RAW_SQL.to_string());
        cols
    }

    fn render_filters(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let columns = self.column_choices(cx);
        let ops = operators(self.session.kind());
        let this = cx.entity().downgrade();
        let rows = self.filters.iter().enumerate().map(|(i, f)| {
            let (t1, t2, t3, t4, t5) = (this.clone(), this.clone(), this.clone(), this.clone(), this.clone());
            let cols = columns.clone();
            let ops = ops.clone();
            h_flex()
                .h(px(24.))
                .gap(px(8.))
                .child(Checkbox::new(("f-on", i)).checked(f.enabled).on_click(move |on, _, cx| {
                    let _ = t1.update(cx, |v, cx| {
                        v.filters[i].enabled = *on;
                        cx.notify();
                    });
                }))
                .child(
                    Button::new(("f-col", i))
                        .label(f.column.clone())
                        .xsmall()
                        .outline()
                        .dropdown_caret(true)
                        .w(px(130.))
                        .dropdown_menu(move |menu, _, _| {
                            cols.iter().fold(menu.max_h(px(360.)).scrollable(true), |m, c| {
                                let (t, c) = (t2.clone(), c.clone());
                                m.item(PopupMenuItem::new(c.clone()).on_click(move |_, _, cx| {
                                    let _ = t.update(cx, |v, cx| {
                                        v.filters[i].column = c.clone();
                                        cx.notify();
                                    });
                                }))
                            })
                        }),
                )
                .when(f.column != RAW_SQL, |row| {
                    row.child(
                        Button::new(("f-op", i))
                            .label(f.op.label())
                            .xsmall()
                            .outline()
                            .dropdown_caret(true)
                            .w(px(130.))
                            .dropdown_menu(move |menu, _, _| {
                                ops.iter().fold(menu.max_h(px(400.)).scrollable(true), |m, op| {
                                    let (t, op) = (t3.clone(), *op);
                                    m.item(PopupMenuItem::new(op.label()).on_click(move |_, window, cx| {
                                        let _ = t.update(cx, |v, cx| {
                                            v.filters[i].op = op;
                                            v.filters[i].value.update(cx, |s, cx| s.set_placeholder(op.placeholder(), window, cx));
                                            cx.notify();
                                        });
                                    }))
                                })
                            }),
                    )
                })
                .child(div().flex_1().child(Input::new(&f.value).xsmall()))
                .child(footer_button(("f-apply", i), cx).min_w(px(55.)).child("Apply").on_click(move |_, window, cx| {
                    let _ = t4.update(cx, |v, cx| {
                        for (j, f) in v.filters.iter_mut().enumerate() {
                            f.enabled = j == i;
                        }
                        v.offset = 0;
                        v.reload(window, cx);
                    });
                }))
                .child(footer_button(("f-del", i), cx).w(px(23.)).child(icon(IconName::Minus, 10., p.icon)).on_click(move |_, window, cx| {
                    let _ = t5.update(cx, |v, cx| {
                        v.filters.remove(i);
                        if v.filters.is_empty() {
                            v.show_filters = false;
                        }
                        v.reload(window, cx);
                    });
                }))
                .child(footer_button(("f-add", i), cx).w(px(23.)).child(icon(IconName::Plus, 10., p.icon)).on_click(cx.listener(
                    move |v, _, window, cx| {
                        let f = Filter { column: v.filters[i].column.clone(), op: v.filters[i].op, value: String::new(), enabled: true };
                        v.add_filter(Some(f), window, cx);
                    },
                )))
        });
        v_flex()
            .bg(p.window)
            .px(px(8.))
            .pt(px(8.))
            .pb(px(8.))
            .gap(px(6.))
            .child(v_flex().gap(px(4.)).children(rows))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(footer_button("f-sql", cx).child("SQL").on_click(cx.listener(|v, _, _, cx| {
                        let req = v.browse_request(cx);
                        let sql = v.session.dialect().select_query(&req, &v.column_choices(cx)[1..].to_vec());
                        cx.emit(DataViewEvent::OpenSql(format!("{sql};")));
                    })))
                    .child(
                        div()
                            .flex_1()
                            .text_center()
                            .text_size(UI_TEXT)
                            .text_color(p.secondary_text)
                            .truncate()
                            .child("Press Enter in a value field to apply all filters"),
                    )
                    .child(footer_button("f-clear", cx).child("Clear filter").on_click(cx.listener(|v, _, window, cx| v.clear_filters(window, cx))))
                    .child(footer_button("f-all", cx).child("Apply All").on_click(cx.listener(|v, _, window, cx| {
                        v.offset = 0;
                        v.reload(window, cx);
                    }))),
            )
    }

    fn render_footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let this = cx.entity().downgrade();
        let count = if self.loading {
            "loading rows...".to_string()
        } else {
            let shown = self.table.read(cx).delegate().rows.len() as u64;
            match self.total {
                Some(t) if self.offset > 0 || t > shown => format!("{}-{} of {} rows", self.offset + 1, self.offset + shown, t),
                Some(t) => format!("{t} {}", if t == 1 { "row" } else { "rows" }),
                None => format!("{shown} rows"),
            }
        };
        let editable = self.item.kind == ItemKind::Table;
        h_flex()
            .h(px(40.))
            .px(px(8.))
            .gap(px(8.))
            .flex_shrink_0()
            .bg(p.window_secondary)
            .border_t_1()
            .border_color(p.separator)
            .child(segmented("dv-mode", &["Data", "Structure"], (self.mode == Mode::Structure) as usize, 64., cx, move |i, _, cx| {
                let _ = this.update(cx, |v, cx| v.set_mode(if i == 0 { Mode::Data } else { Mode::Structure }, cx));
            }))
            .when(self.mode == Mode::Data && editable, |d| {
                d.child(
                    footer_button("add-row", cx)
                        .child(icon(IconName::Plus, 10., p.icon))
                        .child("Row")
                        .on_click(cx.listener(|v, _, window, cx| v.add_row_to(Grid::Data, window, cx))),
                )
            })
            .when(self.mode == Mode::Structure && editable, |d| {
                d.child(
                    footer_button("add-col", cx)
                        .child(icon(IconName::Plus, 10., p.icon))
                        .child("Column")
                        .on_click(cx.listener(|v, _, window, cx| v.add_row_to(Grid::Columns, window, cx))),
                )
                .child(
                    footer_button("add-index", cx)
                        .child(icon(IconName::Plus, 10., p.icon))
                        .child("Index")
                        .on_click(cx.listener(|v, _, window, cx| v.add_row_to(Grid::Indexes, window, cx))),
                )
            })
            .when(self.mode == Mode::Structure, |d| {
                d.child(footer_button("ddl", cx).w(px(80.)).child("DDL").on_click(cx.listener(|v, _, window, cx| v.show_ddl(window, cx))))
            })
            .child(div().flex_1().flex().justify_center().text_size(UI_TEXT).text_color(p.text).when(self.mode == Mode::Data, |d| d.child(count)))
            .when(self.mode == Mode::Data, |d| {
                d.child(Button::new("export").label("Export").xsmall().outline().dropdown_caret(true).dropdown_menu({
                    let this = cx.entity().downgrade();
                    move |menu, _, _| {
                        quarry_core::export::ExportFormat::ALL.iter().fold(menu, |m, f| {
                            let (t, f) = (this.clone(), *f);
                            m.item(PopupMenuItem::new(format!("Export table as {}...", f.label())).on_click(move |_, window, cx| {
                                let _ = t.update(cx, |v, cx| v.export(f, window, cx));
                            }))
                        })
                    }
                }))
                .child(
                    footer_button("filters", cx)
                        .when(self.show_filters, |b| b.bg(p.segment_selected))
                        .child(icon(IconName::Search, 10., p.icon))
                        .child("Filters")
                        .on_click(cx.listener(|v, _, window, cx| v.toggle_filters(window, cx))),
                )
                .child(
                    h_flex()
                        .child(
                            footer_button("prev", cx)
                                .w(px(32.))
                                .rounded_r(px(0.))
                                .child(icon(IconName::ChevronLeft, 12., p.icon))
                                .on_click(cx.listener(|v, _, window, cx| v.page(-1, window, cx))),
                        )
                        .child(
                            Popover::new("paging")
                                .trigger(Button::new("paging-btn").icon(IconName::Settings).xsmall().outline().tooltip("Page settings"))
                                .child(
                                    v_flex()
                                        .w(px(180.))
                                        .gap(px(8.))
                                        .child(h_flex().gap(px(8.)).child(div().w(px(44.)).text_size(UI_TEXT).child("Limit")).child(Input::new(&self.limit_input).xsmall()))
                                        .child(h_flex().gap(px(8.)).child(div().w(px(44.)).text_size(UI_TEXT).child("Offset")).child(Input::new(&self.offset_input).xsmall()))
                                        .child(Button::new("go").label("Go").xsmall().primary().on_click(cx.listener(|v, _, window, cx| v.apply_paging(window, cx)))),
                                ),
                        )
                        .child(
                            footer_button("next", cx)
                                .w(px(32.))
                                .rounded_l(px(0.))
                                .child(icon(IconName::ChevronRight, 12., p.icon))
                                .on_click(cx.listener(|v, _, window, cx| v.page(1, window, cx))),
                        ),
                )
            })
    }

    fn render_structure(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let section = |title: &'static str, t: &Option<Entity<TableState<GridDelegate>>>, h: f32| {
            v_flex()
                .h(px(h))
                .flex_shrink_0()
                .child(div().px(px(8.)).py(px(4.)).text_size(UI_TEXT).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
                .when_some(t.clone(), |d, t| d.child(div().flex_1().child(DataTable::new(&t).stripe(true).bordered(false).xsmall())))
        };
        v_flex()
            .id("structure")
            .size_full()
            .overflow_y_scroll()
            .child(section("Columns", &self.columns_table, 300.))
            .child(section("Indexes", &self.indexes_table, 180.))
            .child(section("Foreign Keys", &self.fks_table, 180.))
    }
}

impl Focusable for DataView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DataView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let body = if !self.item.kind.has_rows() {
            crate::widgets::placeholder("This item has no rows. Use DDL to see its definition.", cx).into_any_element()
        } else if self.mode == Mode::Structure {
            self.render_structure(cx).into_any_element()
        } else if let Some(err) = &self.error {
            div().size_full().p(px(16.)).text_size(UI_TEXT).text_color(p.danger).font_family(MONO).child(err.clone()).into_any_element()
        } else {
            DataTable::new(&self.table).stripe(true).bordered(false).xsmall().into_any_element()
        };
        v_flex()
            .key_context("DataView")
            .track_focus(&self.focus)
            .size_full()
            .bg(p.window)
            .when(self.show_filters && self.mode == Mode::Data, |d| d.child(self.render_filters(cx)).child(crate::widgets::hline(cx)))
            .child(div().flex_1().overflow_hidden().child(body))
            .child(self.render_footer(cx))
    }
}
