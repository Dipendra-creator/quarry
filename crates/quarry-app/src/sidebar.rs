//! Left pane: database items (with expandable columns), saved queries and
//! query history (ItemController / FavoriteTreeView / HistoryTreeView).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::store::HistoryEntry;
use quarry_core::sql::Dialect;
use quarry_core::{ColumnInfo, DriverKind, Item, ItemKind, Session};

use crate::state::{AppState, db};
use crate::theme::{PaletteExt as _, UI_TEXT};
use crate::picker::{self, PickerItem};
use crate::widgets::{flat_switch, icon, placeholder, search_icon};
use crate::{guard, prompt};

pub enum SidebarEvent {
    /// Open in the current preview tab.
    Open(Item),
    OpenNewTab(Item),
    OpenStructure(Item),
    OpenSql { title: String, sql: String, saved_id: Option<String> },
    ShowDdl(Item),
    Log(String),
    /// The current schema changed: title bar and editors follow it.
    SchemaChanged,
    /// Show the database switcher (MySQL and MariaDB, whose databases are their schemas).
    PickDatabase,
}

#[derive(Clone)]
enum Row {
    Section(ItemKind, usize),
    Item(usize),
    Column(ColumnInfo),
}

pub struct Sidebar {
    session: Arc<Session>,
    pub mode: usize,
    search: Entity<InputState>,
    items: Vec<Item>,
    loading: bool,
    error: Option<String>,
    collapsed: HashSet<ItemKind>,
    columns: HashMap<String, Option<Vec<ColumnInfo>>>,
    selected: Option<String>,
    schemas: Vec<String>,
    /// A schema switch is running.
    switching: bool,
    history: Vec<HistoryEntry>,
    scroll: UniformListScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

pub fn item_icon(kind: ItemKind) -> IconName {
    match kind {
        ItemKind::Table => IconName::Table,
        ItemKind::View => IconName::Eye,
        ItemKind::MaterializedView => IconName::Layers,
        ItemKind::Function => IconName::SquareFunction,
        ItemKind::Procedure => IconName::Code,
        ItemKind::Sequence => IconName::ListOrdered,
    }
}

impl Sidebar {
    pub fn new(session: Arc<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search for item..."));
        let subs = vec![
            cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.observe_global::<AppState>(|_, cx| cx.notify()),
        ];
        let mut s = Self {
            session,
            mode: 0,
            search,
            items: vec![],
            loading: false,
            error: None,
            collapsed: HashSet::new(),
            columns: HashMap::new(),
            selected: None,
            schemas: vec![],
            switching: false,
            history: vec![],
            scroll: UniformListScrollHandle::new(),
            _subs: subs,
        };
        s.reload(window, cx);
        s
    }

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        cx.notify();
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let s = session.clone();
            let r = db(async move {
                let items = s.list_items(None).await?;
                let schemas = s.list_schemas().await.unwrap_or_default();
                Ok((items, schemas))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match r {
                    Ok((items, schemas)) => {
                        this.items = items;
                        this.schemas = schemas;
                        this.columns.clear();
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                this.history = this.session.history(500);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn refresh_history(&mut self, cx: &mut Context<Self>) {
        self.history = self.session.history(500);
        cx.notify();
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Makes `schema` current: SQL run in this connection resolves unqualified
    /// names to it (PostgreSQL), and the item list shows its objects.
    pub fn set_schema(&mut self, schema: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.schema().as_deref() == Some(schema.as_str()) || self.switching {
            return;
        }
        self.switching = true;
        cx.notify();
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = db(async move { session.use_schema(&schema).await }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.switching = false;
                match r {
                    Ok(()) => {
                        this.collapsed.clear();
                        this.reload(window, cx);
                        cx.emit(SidebarEvent::SchemaChanged);
                    }
                    Err(e) => prompt::error("Could not switch schema", &format!("{e:#}"), window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The searchable schema list (engines with schemas) or database list
    /// (MySQL, MariaDB) behind the sidebar's bottom button.
    fn pick_namespace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.session.kind().has_schemas() {
            cx.emit(SidebarEvent::PickDatabase);
            return;
        }
        let current = self.session.schema();
        let schemas = self.schemas.clone();
        let items = schemas
            .iter()
            .map(|s| PickerItem {
                label: s.clone(),
                detail: if current.as_deref() == Some(s.as_str()) { "current".into() } else { String::new() },
                icon: IconName::Layers,
            })
            .collect();
        let this = cx.entity().downgrade();
        picker::open("Switch schema", "Search for schema...", items, window, cx, move |ix, window, cx| {
            let name = schemas[ix].clone();
            let _ = this.update(cx, |sb, cx| sb.set_schema(name, window, cx));
        });
    }

    fn filter(&self, cx: &App) -> String {
        self.search.read(cx).value().to_lowercase()
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        let q = self.filter(cx);
        let mut rows = Vec::new();
        let mut kinds: Vec<ItemKind> = self.items.iter().map(|i| i.kind).collect();
        kinds.sort();
        kinds.dedup();
        for kind in kinds {
            let members: Vec<usize> = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, i)| i.kind == kind && (q.is_empty() || i.name().to_lowercase().contains(&q)))
                .map(|(ix, _)| ix)
                .collect();
            if members.is_empty() {
                continue;
            }
            rows.push(Row::Section(kind, members.len()));
            if self.collapsed.contains(&kind) {
                continue;
            }
            for ix in members {
                rows.push(Row::Item(ix));
                if let Some(Some(cols)) = self.columns.get(&self.items[ix].table.qualified()) {
                    rows.extend(cols.iter().cloned().map(Row::Column));
                }
            }
        }
        rows
    }

    fn toggle_columns(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let item = self.items[ix].clone();
        let key = item.table.qualified();
        if self.columns.remove(&key).is_some() {
            cx.notify();
            return;
        }
        self.columns.insert(key.clone(), None);
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = db(async move { session.structure(&item.table).await }).await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(s) = r {
                    this.columns.insert(key, Some(s.columns));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn drop_or_truncate(&mut self, ix: usize, truncate: bool, window: &mut Window, cx: &mut Context<Self>) {
        let item = self.items[ix].clone();
        let d = self.session.dialect();
        let t = d.table(&item.table);
        let sql = match (truncate, item.kind, self.session.kind()) {
            (true, _, DriverKind::Sqlite) => format!("DELETE FROM {t}"),
            (true, _, _) => format!("TRUNCATE TABLE {t}"),
            (false, ItemKind::View, _) => format!("DROP VIEW {t}"),
            (false, ItemKind::MaterializedView, _) => format!("DROP MATERIALIZED VIEW {t}"),
            (false, ItemKind::Function, _) => format!("DROP FUNCTION {t}"),
            (false, ItemKind::Procedure, _) => format!("DROP PROCEDURE {t}"),
            (false, ItemKind::Sequence, _) => format!("DROP SEQUENCE {t}"),
            (false, ItemKind::Table, _) => format!("DROP TABLE {t}"),
        };
        let verb = if truncate { "truncate" } else { "delete" };
        let this = cx.entity().downgrade();
        let session = self.session.clone();
        prompt::confirm(
            if truncate { "Truncate" } else { "Delete" },
            &format!("Are you sure you want to {verb} {}?\n\n{sql}", item.name()),
            if truncate { "Truncate" } else { "Delete" },
            window,
            cx,
            move |window, cx| {
                let (this, session, sql) = (this.clone(), session.clone(), sql.clone());
                guard::guarded(&session.clone(), &sql.clone(), window, cx, move |window, cx| {
                    let _ = this.update(cx, |_, cx| {
                        cx.spawn_in(window, async move |this, cx| {
                            let s2 = sql.clone();
                            let r = db(async move { session.execute(&s2).await }).await;
                            let _ = this.update_in(cx, |this, window, cx| {
                                cx.emit(SidebarEvent::Log(sql.clone()));
                                match r {
                                    Ok(_) => this.reload(window, cx),
                                    Err(e) => prompt::error("Failed", &format!("{e:#}"), window, cx),
                                }
                            });
                        })
                        .detach();
                    });
                });
            },
        );
    }

    fn import_csv(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let item = self.items[ix].clone();
        let session = self.session.clone();
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let preview = format!("-- import {} into {}", path.display(), item.table.qualified());
            let _ = this.update_in(cx, |_, window, cx| {
                let session2 = session.clone();
                let this = cx.entity().downgrade();
                guard::guarded(&session2, &preview, window, cx, move |window, cx| {
                    let _ = this.update(cx, |_, cx| {
                        cx.spawn_in(window, async move |this, cx| {
                            let table = item.table.clone();
                            let r = db(async move {
                                let text = std::fs::read_to_string(&path)?;
                                let tsv = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tsv"));
                                let delimiter = if tsv { '\t' } else { ',' };
                                session.import_csv(&table, &text, delimiter, true).await
                            })
                            .await;
                            let _ = this.update_in(cx, |_, window, cx| match r {
                                Ok(n) => prompt::info("Import finished", &format!("Imported {n} rows into {}.", item.name()), window, cx),
                                Err(e) => prompt::error("Import failed", &format!("{e:#}"), window, cx),
                            });
                        })
                        .detach();
                    });
                });
            });
        })
        .detach();
    }

    fn render_items(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(e) = &self.error {
            return placeholder(e.clone(), cx).into_any_element();
        }
        if self.loading && self.items.is_empty() {
            return placeholder("Loading...", cx).into_any_element();
        }
        if self.items.is_empty() {
            return placeholder("No items", cx).into_any_element();
        }
        let rows = self.rows(cx);
        let count = rows.len();
        uniform_list(
            "items",
            count,
            cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _window, cx| {
                range.filter_map(|i| rows.get(i).cloned()).enumerate().map(|(n, row)| this.render_row(row, n, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full()
        .into_any_element()
    }

    fn render_row(&mut self, row: Row, _n: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.palette().clone();
        match row {
            Row::Section(kind, n) => {
                let collapsed = self.collapsed.contains(&kind);
                h_flex()
                    .id(SharedString::from(format!("section-{kind:?}")))
                    .w_full()
                    .h(px(24.))
                    .px(px(4.))
                    .gap(px(4.))
                    .text_size(UI_TEXT)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.secondary_text)
                    .child(icon(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }, 11., p.secondary_text))
                    .child(format!("{} ({n})", kind.section()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&kind) {
                            this.collapsed.insert(kind);
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Row::Item(ix) => {
                let item = self.items[ix].clone();
                let key = item.table.qualified();
                let selected = self.selected.as_deref() == Some(key.as_str());
                let expanded = self.columns.contains_key(&key);
                let fg = if selected { p.selection_text } else { p.text };
                let this = cx.entity().downgrade();
                h_flex()
                    .id(SharedString::from(format!("item-{key}-{:?}", item.kind)))
                    .w_full()
                    .h(px(24.))
                    .pl(px(4.))
                    .pr(px(8.))
                    .gap(px(4.))
                    .rounded(px(4.))
                    .text_size(UI_TEXT)
                    .text_color(fg)
                    .when(selected, |d| d.bg(p.selection))
                    .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
                    .child(
                        div()
                            .id(SharedString::from(format!("exp-{key}")))
                            .w(px(14.))
                            .flex_shrink_0()
                            .when(item.kind.has_rows(), |d| {
                                d.child(icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight }, 10., if selected { fg } else { p.secondary_text }))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.toggle_columns(ix, window, cx)
                                    }))
                            }),
                    )
                    .child(icon(item_icon(item.kind), 14., if selected { fg } else { p.accent }))
                    .child(div().flex_1().truncate().child(item.name().to_string()))
                    .on_click(cx.listener({
                        let item = item.clone();
                        move |this, ev: &ClickEvent, _, cx| {
                            this.selected = Some(item.table.qualified());
                            cx.notify();
                            if crate::widgets::is_double(ev) || ev.modifiers().control {
                                cx.emit(SidebarEvent::OpenNewTab(item.clone()));
                            } else {
                                cx.emit(SidebarEvent::Open(item.clone()));
                            }
                        }
                    }))
                    .context_menu(move |menu: PopupMenu, _, _| {
                        let ev = |label: &'static str, make: fn(Item) -> SidebarEvent| {
                            let (this, item) = (this.clone(), item.clone());
                            PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                let _ = this.update(cx, |_, cx| cx.emit(make(item.clone())));
                            })
                        };
                        let act = |label: &'static str, truncate: bool| {
                            let this = this.clone();
                            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                let _ = this.update(cx, |s, cx| s.drop_or_truncate(ix, truncate, window, cx));
                            })
                        };
                        let name = item.name().to_string();
                        let mut m = menu
                            .item(ev("Open in new tab", SidebarEvent::OpenNewTab))
                            .item(ev("Open structure", SidebarEvent::OpenStructure))
                            .item(ev("Show DDL", SidebarEvent::ShowDdl))
                            .item(PopupMenuItem::new("Copy name").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(name.clone()))))
                            .separator();
                        if item.kind == ItemKind::Table {
                            let this = this.clone();
                            m = m
                                .item(PopupMenuItem::new("Import CSV...").on_click(move |_, window, cx| {
                                    let _ = this.update(cx, |s, cx| s.import_csv(ix, window, cx));
                                }))
                                .item(act("Truncate...", true));
                        }
                        m.item(act("Delete...", false))
                    })
                    .into_any_element()
            }
            Row::Column(c) => h_flex()
                .w_full()
                .h(px(22.))
                .pl(px(26.))
                .pr(px(8.))
                .gap(px(4.))
                .text_size(UI_TEXT)
                .child(div().w(px(11.)).flex_shrink_0().when(c.primary_key, |d| d.child(icon(IconName::KeyRound, 10., p.primary_key))))
                .child(div().text_color(p.text).truncate().child(c.name.clone()))
                .child(div().flex_1().text_right().text_color(p.secondary_text).truncate().child(c.data_type.clone()))
                .into_any_element(),
        }
    }

    fn render_queries(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.palette().clone();
        let q = self.filter(cx);
        let conn = self.session.config.id.clone();
        let saved: Vec<quarry_core::store::SavedQuery> = AppState::get(cx)
            .saved
            .for_connection(&conn)
            .into_iter()
            .filter(|s| q.is_empty() || s.name.to_lowercase().contains(&q) || s.sql.to_lowercase().contains(&q))
            .cloned()
            .collect();
        if saved.is_empty() {
            return placeholder("No saved queries. Use \"Save\" in a SQL editor tab.", cx).into_any_element();
        }
        v_flex()
            .id("queries")
            .size_full()
            .overflow_y_scroll()
            .children(saved.into_iter().map(|s| {
                let id = s.id.clone();
                h_flex()
                    .id(SharedString::from(format!("q-{}", s.id)))
                    .h(px(24.))
                    .px(px(8.))
                    .gap(px(6.))
                    .rounded(px(4.))
                    .text_size(UI_TEXT)
                    .hover(|st| st.bg(p.hover))
                    .child(icon(IconName::FileCode, 13., p.icon))
                    .child(div().truncate().child(s.name.clone()))
                    .on_click(cx.listener({
                        let s = s.clone();
                        move |_, _, _, cx| {
                            cx.emit(SidebarEvent::OpenSql { title: s.name.clone(), sql: s.sql.clone(), saved_id: Some(s.id.clone()) })
                        }
                    }))
                    .context_menu(move |menu: PopupMenu, _, _| {
                        let id = id.clone();
                        menu.item(PopupMenuItem::new("Delete").on_click(move |_, _, cx| {
                            cx.update_global::<AppState, _>(|st, _| {
                                let _ = st.saved.remove(&id);
                            });
                        }))
                    })
            }))
            .into_any_element()
    }

    fn render_history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.palette().clone();
        let q = self.filter(cx);
        let entries: Vec<HistoryEntry> = self.history.iter().filter(|h| q.is_empty() || h.sql.to_lowercase().contains(&q)).cloned().collect();
        if entries.is_empty() {
            return placeholder("No history", cx).into_any_element();
        }
        let count = entries.len();
        uniform_list(
            "history",
            count,
            cx.processor(move |_: &mut Self, range: std::ops::Range<usize>, _window, cx| {
                range
                    .map(|i| {
                        let h = entries[i].clone();
                        let first: String = h.sql.lines().next().unwrap_or_default().chars().take(120).collect();
                        v_flex()
                            .id(("h", i))
                            .h(px(36.))
                            .px(px(8.))
                            .justify_center()
                            .rounded(px(4.))
                            .hover(|st| st.bg(p.hover))
                            .child(div().text_size(UI_TEXT).truncate().child(first))
                            .child(div().text_size(px(10.)).text_color(p.secondary_text).child(format!("{} · {}", h.database, ago(h.at))))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(SidebarEvent::OpenSql { title: "History".into(), sql: h.sql.clone(), saved_id: None })
                            }))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .into_any_element()
    }

    fn render_bottom(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let templates = |driver: DriverKind, schema: Option<String>| -> Vec<(&'static str, String)> {
            let (id, ty) = match driver {
                DriverKind::Postgres => ("id SERIAL PRIMARY KEY", "TEXT"),
                DriverKind::Sqlite => ("id INTEGER PRIMARY KEY AUTOINCREMENT", "TEXT"),
                DriverKind::SqlServer => ("id INT IDENTITY(1,1) PRIMARY KEY", "NVARCHAR(255)"),
                _ => ("id INT AUTO_INCREMENT PRIMARY KEY", "VARCHAR(255)"),
            };
            // New objects go into the schema shown in the sidebar.
            let name = |n: &str| match &schema {
                Some(s) if driver.has_schemas() => format!("{}.{n}", Dialect(driver).quote_ident(s)),
                _ => n.to_string(),
            };
            vec![
                ("New Table...", format!("CREATE TABLE {} (\n    {id},\n    name {ty} NOT NULL\n);", name("new_table"))),
                ("New View...", format!("CREATE VIEW {} AS\nSELECT 1 AS one;", name("new_view"))),
                (
                    "New Function...",
                    match driver {
                        DriverKind::Postgres => format!("CREATE FUNCTION {}() RETURNS integer AS $$\n    SELECT 1;\n$$ LANGUAGE sql;", name("new_function")),
                        DriverKind::SqlServer => format!("CREATE FUNCTION {}() RETURNS INT AS\nBEGIN\n    RETURN 1;\nEND;", name("new_function")),
                        _ => "CREATE FUNCTION new_function() RETURNS INT DETERMINISTIC\nRETURN 1;".to_string(),
                    },
                ),
            ]
        };
        let driver = self.session.kind();
        let schema = self.session.schema();
        h_flex()
            .h(px(40.))
            .px(px(6.))
            .gap(px(8.))
            .flex_shrink_0()
            .child(Button::new("new-item").icon(IconName::Plus).xsmall().outline().dropdown_caret(true).dropdown_menu({
                let this = this.clone();
                move |menu, _, _| {
                    templates(driver, schema.clone()).into_iter().fold(menu, |m, (label, sql)| {
                        let this = this.clone();
                        m.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            let _ = this.update(cx, |_, cx| {
                                cx.emit(SidebarEvent::OpenSql { title: label.trim_end_matches("...").into(), sql: sql.clone(), saved_id: None })
                            });
                        }))
                    })
                }
            }))
            .child(self.render_namespace(cx))
    }
}

impl Sidebar {
    /// Bottom button: the current schema (PostgreSQL, SQL Server) or database
    /// (MySQL, MariaDB), opening a searchable list to switch it.
    fn render_namespace(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.palette().clone();
        let kind = self.session.kind();
        if kind.is_file_based() {
            return div().flex_1().min_w(px(0.)).text_size(UI_TEXT).text_color(p.secondary_text).truncate().child(self.session.database()).into_any_element();
        }
        let (what, name, icon_name) = if kind.has_schemas() {
            ("Schema", self.session.schema().unwrap_or_default(), IconName::Layers)
        } else {
            ("Database", self.session.database(), IconName::Database)
        };
        let label = if self.switching { "Switching...".to_string() } else if name.is_empty() { format!("Choose {}", what.to_lowercase()) } else { name };
        h_flex()
            .id("namespace")
            .flex_1()
            .min_w(px(0.))
            .h(px(24.))
            .px(px(8.))
            .gap(px(6.))
            .rounded(px(4.))
            .border_1()
            .border_color(p.button_border)
            .bg(p.button)
            .text_size(UI_TEXT)
            .text_color(p.text)
            .hover(|s| s.bg(p.button_hover))
            .child(icon(icon_name, 12., p.secondary_text))
            .child(div().flex_1().min_w(px(0.)).truncate().child(label))
            .child(icon(IconName::ChevronsUpDown, 12., p.secondary_text))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(format!("Switch {}", what.to_lowercase())).build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| this.pick_namespace(window, cx)))
            .into_any_element()
    }
}

fn ago(at: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(at);
    let s = now.saturating_sub(at);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let this = cx.entity().downgrade();
        let body = match self.mode {
            0 => self.render_items(cx),
            1 => self.render_queries(cx),
            _ => self.render_history(cx),
        };
        let placeholder_text = match self.mode {
            0 => "Search for item...",
            1 => "Search for query...",
            _ => "Search for history...",
        };
        let _ = placeholder_text;
        v_flex()
            .size_full()
            .bg(p.window_secondary)
            .child(div().pt(px(4.)).child(flat_switch("sb-mode", &["Items", "Queries", "History"], self.mode, cx, move |i, _, cx| {
                let _ = this.update(cx, |s, cx| {
                    s.mode = i;
                    if i == 2 {
                        s.refresh_history(cx);
                    }
                    cx.notify();
                });
            })))
            .child(
                h_flex()
                    .px(px(4.))
                    .pt(px(8.))
                    .pb(px(4.))
                    .child(Input::new(&self.search).prefix(search_icon(cx)).cleanable(true).xsmall()),
            )
            .child(div().flex_1().px(px(4.)).overflow_hidden().child(body))
            .when(self.mode == 0, |d| d.child(self.render_bottom(cx)))
    }
}
