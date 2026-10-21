//! Workspace window: capsule title bar, one workspace per open connection
//! (left vertical strip), and per workspace the sidebar, tabs, row detail
//! pane and console (WorkspaceWindow / WorkspaceController).

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::{Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::{Filter, Item, ItemKind, SafeMode, Session};

use crate::actions::*;
use crate::data_view::{DataView, DataViewEvent, Mode, RowSnapshot};
use crate::picker::{self, PickerItem};
use crate::query_editor::{QueryEditor, QueryEvent};
use crate::row_detail::RowDetail;
use crate::sidebar::{Sidebar, SidebarEvent, item_icon};
use crate::state::{AppState, db};
use crate::theme::{MONO, PaletteExt as _, UI_TEXT};
use crate::widgets::{capsule, capsule_button, capsule_separator, capsule_text_button, icon, placeholder};
use crate::{prompt, welcome};

struct WorkspaceHandle(Option<WindowHandle<WorkspaceWindow>>);
impl Global for WorkspaceHandle {}

/// Shows a connected session: adds it to the open workspace window, or opens one.
pub fn open_session(session: Arc<Session>, cx: &mut App) {
    if let Some(handle) = cx.try_global::<WorkspaceHandle>().and_then(|h| h.0) {
        let added = handle.update(cx, |w, window, cx| {
            w.add(session.clone(), window, cx);
            window.activate_window();
        });
        if added.is_ok() {
            return;
        }
    }
    let size = size(px(1280.), px(800.));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
        window_min_size: Some(gpui_kit::size(px(900.), px(600.))),
        ..TitleBar::window_options()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| {
            let mut w = WorkspaceWindow { workspaces: vec![], active: 0, _subs: vec![] };
            w.add(session, window, cx);
            w
        })
    });
    if let Ok((any, _)) = opened {
        cx.set_global(WorkspaceHandle(any.downcast::<WorkspaceWindow>()));
    }
}

pub struct WorkspaceWindow {
    workspaces: Vec<Entity<ConnWorkspace>>,
    active: usize,
    _subs: Vec<Subscription>,
}

impl WorkspaceWindow {
    fn add(&mut self, session: Arc<Session>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.workspaces.iter().position(|w| w.read(cx).session.config.id == session.config.id) {
            self.active = i;
            cx.notify();
            return;
        }
        let ws = cx.new(|cx| ConnWorkspace::new(session, window, cx));
        self._subs.push(cx.observe(&ws, |_, _, cx| cx.notify()));
        self._subs.push(cx.subscribe_in(&ws, window, |this, ws, ev: &WorkspaceEvent, window, cx| match ev {
            WorkspaceEvent::Close => this.close(ws.clone(), window, cx),
        }));
        self.workspaces.push(ws.clone());
        self.active = self.workspaces.len() - 1;
        ws.read(cx).focus_handle.clone().focus(window, cx);
        cx.notify();
    }

    fn close(&mut self, ws: Entity<ConnWorkspace>, window: &mut Window, cx: &mut Context<Self>) {
        let id = ws.read(cx).session.config.id.clone();
        AppState::get(cx).sessions.disconnect(&id);
        self.workspaces.retain(|w| *w != ws);
        self.active = self.active.min(self.workspaces.len().saturating_sub(1));
        if self.workspaces.is_empty() {
            cx.set_global(WorkspaceHandle(None));
            welcome::open(cx);
            window.remove_window();
        }
        cx.notify();
    }

    fn current(&self) -> Option<&Entity<ConnWorkspace>> {
        self.workspaces.get(self.active)
    }

    fn render_title_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let Some(ws) = self.current().cloned() else { return TitleBar::new() };
        let w = ws.read(cx);
        let has_changes = w.has_changes(cx);
        let safe = w.session.safe_mode();
        let status_ix = (w.session.config.status_color as usize).min(5);
        let title = w.session.title();
        let tag = w.session.config.tag;
        let lock_icon = if safe == SafeMode::Silent { IconName::LockOpen } else { IconName::Lock };
        let on = |f: fn(&mut ConnWorkspace, &mut Window, &mut Context<ConnWorkspace>)| {
            let ws = ws.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| ws.update(cx, |w, cx| f(w, window, cx))
        };
        let safe_menu = {
            let ws = ws.clone();
            div().child(
                gpui_kit::component::button::Button::new("safe-mode")
                    .icon(lock_icon)
                    .small()
                    .rounded(gpui_kit::component::button::ButtonRounded::Large)
                    .ghost()
                    .tooltip("Safe mode level")
                    .dropdown_menu(move |menu, _, _| {
                        SafeMode::ALL.iter().fold(menu, |m, mode| {
                            let (ws, mode) = (ws.clone(), *mode);
                            m.item(PopupMenuItem::new(mode.label()).checked(mode == safe).on_click(move |_, _, cx| {
                                ws.update(cx, |w, cx| w.set_safe_mode(mode, cx));
                            }))
                        })
                    }),
            )
        };
        TitleBar::new()
            .h(px(54.))
            .pl(px(0.))
            .bg(p.title_bar)
            .border_color(p.separator)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .child(
                        capsule(cx)
                            .child(capsule_button("toggle-left", IconName::PanelLeft, "Toggle left sidebar", true, cx, on(|w, _, cx| w.toggle_sidebar(cx))))
                            .child(capsule_button("menu", IconName::Menu, "Menu", true, cx, on(|w, window, cx| w.show_menu(window, cx))))
                            .child(capsule_button("connections", IconName::Plug, "Show connections list", true, cx, |_, _, cx| welcome::open(cx))),
                    )
                    .child(
                        capsule(cx)
                            .child(capsule_button("discard", IconName::Undo2, "Discard changes", has_changes, cx, on(|w, window, cx| w.discard(window, cx))))
                            .child(capsule_separator(cx))
                            .child(capsule_button("preview", IconName::Eye, "Preview changes", has_changes, cx, on(|w, window, cx| w.preview(window, cx))))
                            .child(capsule_separator(cx))
                            .child(capsule_button("commit", IconName::Check, "Commit changes (Ctrl+S)", has_changes, cx, on(|w, window, cx| w.commit(window, cx)))),
                    )
                    .child(
                        capsule(cx)
                            .child(safe_menu)
                            .child(capsule_button("databases", IconName::Database, "Show databases list (Ctrl+K)", true, cx, on(|w, window, cx| w.database_list(window, cx))))
                            .child(capsule_text_button("sql", "SQL", "Open SQL editor (Ctrl+E)", cx, on(|w, window, cx| w.new_query(window, cx)))),
                    ),
            )
            .child(
                div().flex_1().min_w(px(0.)).px(px(12.)).flex().justify_center().child(
                    h_flex()
                        .h(px(30.))
                        .max_w(px(560.))
                        .px(px(14.))
                        .gap(px(6.))
                        .rounded(px(6.))
                        .bg(p.status[status_ix])
                        .border_1()
                        .border_color(p.button_border)
                        .font_family(MONO)
                        .text_size(UI_TEXT)
                        .text_color(p.status_text[status_ix])
                        .child(div().truncate().child(title))
                        .when(tag != quarry_core::ConnTag::None, |d| d.child(div().text_color(p.tag(tag)).child(format!("({})", tag.label())))),
                ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .mr(px(8.))
                    .child(
                        capsule(cx)
                            .child(capsule_button("refresh", IconName::RefreshCw, "Refresh workspace", true, cx, on(|w, window, cx| w.refresh(window, cx))))
                            .child(capsule_button("anything", IconName::Command, "Open anything (Ctrl+P)", true, cx, on(|w, window, cx| w.open_anything(window, cx)))),
                    )
                    .child(
                        capsule(cx)
                            .child(capsule_button("console", IconName::PanelBottom, "Toggle console log (Ctrl+Alt+C)", true, cx, on(|w, _, cx| w.toggle_console(cx))))
                            .child(capsule_separator(cx))
                            .child(capsule_button("detail", IconName::PanelRight, "Toggle right sidebar (Ctrl+Space)", true, cx, on(|w, _, cx| w.toggle_detail(cx)))),
                    ),
            )
    }

    fn render_strip(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        v_flex()
            .w(px(80.))
            .h_full()
            .flex_shrink_0()
            .bg(p.window)
            .border_r_1()
            .border_color(p.separator)
            .children(self.workspaces.iter().enumerate().map(|(i, ws)| {
                let w = ws.read(cx);
                let selected = i == self.active;
                v_flex()
                    .id(("ws", i))
                    .size(px(80.))
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .px(px(2.))
                    .when(selected, |d| d.bg(p.workspace_tab_selected).border_y_1().border_color(p.separator))
                    .hover(|s| s.bg(p.workspace_tab_selected))
                    .child(icon(IconName::Database, 15., p.icon))
                    .child(div().text_size(px(10.)).text_center().text_color(p.text).child(w.session.config.display_name()))
                    .tooltip({
                        let t = w.session.title();
                        move |window, cx| gpui_kit::component::tooltip::Tooltip::new(t.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active = i;
                        let h = this.workspaces[i].read(cx).focus_handle.clone();
                        h.focus(window, cx);
                        cx.notify();
                    }))
            }))
    }
}

impl Render for WorkspaceWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        v_flex()
            .size_full()
            .bg(p.window)
            .text_color(p.text)
            .on_action(cx.listener(|_, _: &OpenConnections, _, cx| welcome::open(cx)))
            .on_action(cx.listener(|_, _: &Preferences, window, cx| crate::preferences::open(window, cx)))
            .child(self.render_title_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .overflow_hidden()
                    .when(self.workspaces.len() > 1, |d| d.child(self.render_strip(cx)))
                    .when_some(self.current().cloned(), |d, ws| d.child(div().flex_1().h_full().overflow_hidden().child(ws))),
            )
    }
}

// ---------------------------------------------------------------------------

pub enum WorkspaceEvent {
    Close,
}

enum TabContent {
    Data(Entity<DataView>),
    Query(Entity<QueryEditor>),
}

struct Tab {
    content: TabContent,
    /// A preview tab is replaced by the next single-clicked item.
    preview: bool,
}

pub struct ConnWorkspace {
    pub session: Arc<Session>,
    sidebar: Entity<Sidebar>,
    row_detail: Entity<RowDetail>,
    tabs: Vec<Tab>,
    active_tab: Option<usize>,
    show_sidebar: bool,
    show_detail: bool,
    show_console: bool,
    console: Vec<(String, String)>,
    query_counter: usize,
    pub focus_handle: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<WorkspaceEvent> for ConnWorkspace {}

impl ConnWorkspace {
    fn new(session: Arc<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| Sidebar::new(session.clone(), window, cx));
        let row_detail = cx.new(|cx| RowDetail::new(window, cx));
        let subs = vec![cx.subscribe_in(&sidebar, window, Self::on_sidebar_event)];
        Self {
            session,
            sidebar,
            row_detail,
            tabs: vec![],
            active_tab: None,
            show_sidebar: true,
            show_detail: AppState::get(cx).settings.show_row_detail,
            show_console: false,
            console: vec![],
            query_counter: 0,
            focus_handle: cx.focus_handle(),
            _subs: subs,
        }
    }

    fn log(&mut self, sql: String, cx: &mut Context<Self>) {
        let now = time_of_day();
        self.console.push((now, sql));
        if self.console.len() > 1000 {
            self.console.remove(0);
        }
        cx.notify();
    }

    fn has_changes(&self, cx: &App) -> bool {
        self.tabs.iter().any(|t| matches!(&t.content, TabContent::Data(d) if d.read(cx).has_changes(cx)))
    }

    fn active_data(&self) -> Option<Entity<DataView>> {
        match self.active_tab.and_then(|i| self.tabs.get(i)).map(|t| &t.content) {
            Some(TabContent::Data(d)) => Some(d.clone()),
            _ => None,
        }
    }

    fn active_query(&self) -> Option<Entity<QueryEditor>> {
        match self.active_tab.and_then(|i| self.tabs.get(i)).map(|t| &t.content) {
            Some(TabContent::Query(q)) => Some(q.clone()),
            _ => None,
        }
    }

    fn on_sidebar_event(&mut self, _: &Entity<Sidebar>, ev: &SidebarEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            SidebarEvent::Open(item) => self.open_item(item.clone(), vec![], false, window, cx),
            SidebarEvent::OpenNewTab(item) => self.open_item(item.clone(), vec![], true, window, cx),
            SidebarEvent::OpenStructure(item) => {
                self.open_item(item.clone(), vec![], true, window, cx);
                if let Some(d) = self.active_data() {
                    d.update(cx, |d, cx| d.set_mode(Mode::Structure, cx));
                }
            }
            SidebarEvent::OpenSql { title, sql, saved_id } => {
                self.open_sql(title.clone(), sql.clone(), saved_id.clone(), window, cx)
            }
            SidebarEvent::ShowDdl(item) => self.show_ddl(item.clone(), window, cx),
            SidebarEvent::Log(sql) => self.log(sql.clone(), cx),
        }
    }

    pub fn open_item(&mut self, item: Item, filters: Vec<Filter>, new_tab: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !item.kind.has_rows() {
            self.show_ddl(item, window, cx);
            return;
        }
        if filters.is_empty() {
            if let Some(i) = self.tabs.iter().position(|t| matches!(&t.content, TabContent::Data(d) if d.read(cx).item == item)) {
                if new_tab {
                    self.tabs[i].preview = false;
                }
                self.activate(i, window, cx);
                return;
            }
        }
        let view = cx.new(|cx| DataView::new(self.session.clone(), item, filters, window, cx));
        self._subs.push(cx.subscribe_in(&view, window, |this, view, ev: &DataViewEvent, window, cx| match ev {
            DataViewEvent::Changed => cx.notify(),
            DataViewEvent::Log(sql) => this.log(sql.clone(), cx),
            DataViewEvent::RowSelected(snap) => this.show_row(snap.clone(), Some(view.downgrade()), window, cx),
            DataViewEvent::OpenTable(item, filters) => this.open_item(item.clone(), filters.clone(), true, window, cx),
            DataViewEvent::OpenSql(sql) => this.open_sql("Query".into(), sql.clone(), None, window, cx),
        }));
        let tab = Tab { content: TabContent::Data(view), preview: !new_tab };
        let reuse = self
            .active_tab
            .filter(|i| self.tabs.get(*i).is_some_and(|t| t.preview && matches!(&t.content, TabContent::Data(d) if !d.read(cx).has_changes(cx))));
        match reuse {
            Some(i) if !new_tab => {
                self.tabs[i] = tab;
                self.activate(i, window, cx);
            }
            _ => {
                self.tabs.push(tab);
                self.activate(self.tabs.len() - 1, window, cx);
            }
        }
    }

    pub fn open_sql(&mut self, title: String, sql: String, saved_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = &saved_id {
            if let Some(i) = self.tabs.iter().position(|t| matches!(&t.content, TabContent::Query(q) if q.read(cx).saved_id.as_ref() == Some(id))) {
                self.activate(i, window, cx);
                return;
            }
        }
        let editor = cx.new(|cx| {
            let mut e = QueryEditor::new(self.session.clone(), title, sql, window, cx);
            e.saved_id = saved_id;
            e
        });
        self._subs.push(cx.subscribe_in(&editor, window, |this, _, ev: &QueryEvent, window, cx| match ev {
            QueryEvent::Log(sql) => {
                this.log(sql.clone(), cx);
                this.sidebar.update(cx, |s, cx| s.refresh_history(cx));
            }
            QueryEvent::RowSelected(snap) => this.show_row(snap.clone(), None, window, cx),
            QueryEvent::Changed => cx.notify(),
        }));
        self.tabs.push(Tab { content: TabContent::Query(editor.clone()), preview: false });
        self.activate(self.tabs.len() - 1, window, cx);
        let h = editor.read(cx).editor_focus(cx);
        window.focus(&h, cx);
    }

    fn show_ddl(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let it = item.clone();
            let r = db(async move { session.ddl(&it).await }).await;
            let _ = this.update_in(cx, |this, window, cx| match r {
                Ok(ddl) => this.open_sql(item.name().to_string(), ddl, None, window, cx),
                Err(e) => prompt::error("Could not load definition", &format!("{e:#}"), window, cx),
            });
        })
        .detach();
    }

    fn show_row(&mut self, snap: Option<RowSnapshot>, source: Option<WeakEntity<DataView>>, window: &mut Window, cx: &mut Context<Self>) {
        self.row_detail.update(cx, |r, cx| r.set_row(snap, source, window, cx));
    }

    fn activate(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab = Some(i);
        self.row_detail.update(cx, |r, cx| r.set_row(None, None, window, cx));
        match self.tabs.get(i).map(|t| &t.content) {
            Some(TabContent::Data(d)) => d.read(cx).focus_handle(cx).focus(window, cx),
            Some(TabContent::Query(q)) => {
                let h = q.read(cx).editor_focus(cx);
                window.focus(&h, cx);
            }
            None => {}
        }
        cx.notify();
    }

    fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let dirty = matches!(self.tabs.get(i).map(|t| &t.content), Some(TabContent::Data(d)) if d.read(cx).has_changes(cx));
        if dirty {
            let this = cx.entity().downgrade();
            prompt::confirm("Unsaved changes", "This tab has uncommitted changes. Close it and discard them?", "Discard", window, cx, move |window, cx| {
                let _ = this.update(cx, |w, cx| w.remove_tab(i, window, cx));
            });
        } else {
            self.remove_tab(i, window, cx);
        }
    }

    fn remove_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        self.active_tab = if self.tabs.is_empty() { None } else { Some(i.min(self.tabs.len() - 1)) };
        if let Some(a) = self.active_tab {
            self.activate(a, window, cx);
        }
        cx.notify();
    }

    fn new_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query_counter += 1;
        let title = if self.query_counter == 1 { "SQL Query".to_string() } else { format!("SQL Query {}", self.query_counter) };
        self.open_sql(title, String::new(), None, window, cx);
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.show_sidebar = !self.show_sidebar;
        cx.notify();
    }

    fn toggle_detail(&mut self, cx: &mut Context<Self>) {
        self.show_detail = !self.show_detail;
        let show = self.show_detail;
        cx.update_global::<AppState, _>(|s, _| {
            s.settings.show_row_detail = show;
            s.settings.save();
        });
        cx.notify();
    }

    fn toggle_console(&mut self, cx: &mut Context<Self>) {
        self.show_console = !self.show_console;
        cx.notify();
    }

    fn set_safe_mode(&mut self, mode: SafeMode, cx: &mut Context<Self>) {
        self.session.set_safe_mode(mode);
        let id = self.session.config.id.clone();
        cx.update_global::<AppState, _>(|s, _| {
            if let Some(mut c) = s.store.find(&id).cloned() {
                c.safe_mode = mode;
                let _ = s.store.upsert(c);
            }
        });
        cx.notify();
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.active_data() {
            d.update(cx, |d, cx| d.commit(window, cx));
        }
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.active_data() {
            d.update(cx, |d, cx| d.discard(window, cx));
        }
    }

    fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.active_data() {
            d.update(cx, |d, cx| d.preview(window, cx));
        }
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.active_data() {
            d.update(cx, |d, cx| d.reload(window, cx));
        }
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |s, cx| s.reload(window, cx));
        self.reload(window, cx);
    }

    fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = db(async move { session.reconnect().await }).await;
            let _ = this.update_in(cx, |this, window, cx| match r {
                Ok(()) => this.refresh(window, cx),
                Err(e) => prompt::error("Reconnect failed", &format!("{e:#}"), window, cx),
            });
        })
        .detach();
    }

    fn disconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_changes(cx) {
            let this = cx.entity().downgrade();
            prompt::confirm("Disconnect", "There are uncommitted changes. Disconnect anyway?", "Disconnect", window, cx, move |_, cx| {
                let _ = this.update(cx, |_, cx| cx.emit(WorkspaceEvent::Close));
            });
        } else {
            cx.emit(WorkspaceEvent::Close);
        }
    }

    fn database_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let s = session.clone();
            let dbs = db(async move { s.list_databases().await }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let dbs = match dbs {
                    Ok(d) => d,
                    Err(e) => return prompt::error("Could not list databases", &format!("{e:#}"), window, cx),
                };
                let current = this.session.database();
                let items = dbs
                    .iter()
                    .map(|d| PickerItem { label: d.clone(), detail: if *d == current { "current".into() } else { String::new() }, icon: IconName::Database })
                    .collect();
                let this = cx.entity().downgrade();
                picker::open("Open database", "Search for database...", items, window, cx, move |ix, window, cx| {
                    let name = dbs[ix].clone();
                    let _ = this.update(cx, |w, cx| w.switch_database(name, window, cx));
                });
            });
        })
        .detach();
    }

    fn switch_database(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_changes(cx) {
            prompt::error("Uncommitted changes", "Commit or discard your changes before switching database.", window, cx);
            return;
        }
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = db(async move { session.switch_database(&name).await }).await;
            let _ = this.update_in(cx, |this, window, cx| match r {
                Ok(()) => {
                    this.tabs.retain(|t| matches!(t.content, TabContent::Query(_)));
                    this.active_tab = this.tabs.len().checked_sub(1);
                    this.sidebar.update(cx, |s, cx| s.reload(window, cx));
                    cx.notify();
                }
                Err(e) => prompt::error("Could not open database", &format!("{e:#}"), window, cx),
            });
        })
        .detach();
    }

    fn open_anything(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<Item> = self.sidebar.read(cx).items().to_vec();
        let saved: Vec<quarry_core::store::SavedQuery> = AppState::get(cx).saved.for_connection(&self.session.config.id).into_iter().cloned().collect();
        let mut entries: Vec<PickerItem> = items
            .iter()
            .map(|i| PickerItem { label: i.name().to_string(), detail: i.kind.section().trim_end_matches('s').to_lowercase(), icon: item_icon(i.kind) })
            .collect();
        entries.extend(saved.iter().map(|q| PickerItem { label: q.name.clone(), detail: "saved query".into(), icon: IconName::FileCode }));
        let this = cx.entity().downgrade();
        picker::open("Open anything", "Search for table, view, function, query...", entries, window, cx, move |ix, window, cx| {
            let _ = this.update(cx, |w, cx| {
                if let Some(item) = items.get(ix) {
                    w.open_item(item.clone(), vec![], true, window, cx);
                } else if let Some(q) = saved.get(ix - items.len()) {
                    w.open_sql(q.name.clone(), q.sql.clone(), Some(q.id.clone()), window, cx);
                }
            });
        });
    }

    fn open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            for path in paths {
                let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let read = std::fs::read_to_string(&path);
                let _ = this.update_in(cx, |w, window, cx| match read {
                    Ok(sql) => w.open_sql(title, sql, None, window, cx),
                    Err(e) => prompt::error("Could not open file", &e.to_string(), window, cx),
                });
            }
        })
        .detach();
    }

    fn show_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = vec![
            PickerItem { label: "New SQL query".into(), detail: "Ctrl+E".into(), icon: IconName::FileCode },
            PickerItem { label: "Open anything".into(), detail: "Ctrl+P".into(), icon: IconName::Search },
            PickerItem { label: "Open database".into(), detail: "Ctrl+K".into(), icon: IconName::Database },
            PickerItem { label: "Open SQL file...".into(), detail: "Ctrl+O".into(), icon: IconName::FolderOpen },
            PickerItem { label: "Processes".into(), detail: "Ctrl+.".into(), icon: IconName::Cpu },
            PickerItem { label: "Reconnect".into(), detail: "Ctrl+Shift+R".into(), icon: IconName::RefreshCw },
            PickerItem { label: "Disconnect".into(), detail: "Shift+F5".into(), icon: IconName::Unplug },
            PickerItem { label: "Preferences".into(), detail: "Ctrl+,".into(), icon: IconName::Settings },
        ];
        let this = cx.entity().downgrade();
        picker::open("Menu", "Search for command...", items, window, cx, move |ix, window, cx| {
            let _ = this.update(cx, |w, cx| match ix {
                0 => w.new_query(window, cx),
                1 => w.open_anything(window, cx),
                2 => w.database_list(window, cx),
                3 => w.open_file(window, cx),
                4 => crate::processes::open(w.session.clone(), window, cx),
                5 => w.reconnect(window, cx),
                6 => w.disconnect(window, cx),
                _ => crate::preferences::open(window, cx),
            });
        });
    }

    fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        h_flex()
            .id("tab-bar")
            .h(px(30.))
            .px(px(2.))
            .gap(px(2.))
            .flex_shrink_0()
            .bg(p.tab)
            .border_b_1()
            .border_color(p.separator)
            .overflow_x_scroll()
            .children(self.tabs.iter().enumerate().map(|(i, t)| {
                let (title, dirty, query) = match &t.content {
                    TabContent::Data(d) => {
                        let d = d.read(cx);
                        (d.title(), d.has_changes(cx), false)
                    }
                    TabContent::Query(q) => (q.read(cx).title.clone(), false, true),
                };
                let active = Some(i) == self.active_tab;
                h_flex()
                    .id(("tab", i))
                    .group("tab")
                    .h(px(26.))
                    .min_w(px(110.))
                    .max_w(px(220.))
                    .pl(px(24.))
                    .pr(px(4.))
                    .gap(px(4.))
                    .rounded(px(14.))
                    .text_size(UI_TEXT)
                    .text_color(p.text)
                    .when(active, |d| d.bg(p.tab_selected))
                    .hover(|s| s.bg(p.tab_selected))
                    .child(icon(if query { IconName::FileCode } else { IconName::Table }, 11., p.secondary_text))
                    .child(div().flex_1().truncate().when(t.preview, |d| d.italic()).child(if dirty { format!("{title} *") } else { title }))
                    .child(
                        div()
                            .id(("close", i))
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .invisible()
                            .group_hover("tab", |s| s.visible())
                            .hover(|s| s.bg(p.capsule_hover))
                            .child(icon(IconName::X, 10., p.text))
                            .on_click(cx.listener(move |w, _, window, cx| {
                                cx.stop_propagation();
                                w.close_tab(i, window, cx)
                            })),
                    )
                    .on_click(cx.listener(move |w, ev: &ClickEvent, window, cx| {
                        if crate::widgets::is_double(ev) {
                            w.tabs[i].preview = false;
                        }
                        w.activate(i, window, cx)
                    }))
            }))
    }

    fn render_console(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        v_flex()
            .h(px(180.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(p.separator)
            .child(
                div()
                    .id("console")
                    .flex_1()
                    .p(px(6.))
                    .overflow_y_scroll()
                    .font_family(MONO)
                    .text_size(UI_TEXT)
                    .when(self.console.is_empty(), |d| d.child(placeholder("Queries sent to the server are logged here.", cx)))
                    .children(self.console.iter().map(|(at, sql)| {
                        h_flex().gap(px(8.)).items_start().child(div().text_color(p.secondary_text).child(at.clone())).child(div().flex_1().child(sql.clone()))
                    })),
            )
            .child(
                h_flex().h(px(32.)).px(px(8.)).border_t_1().border_color(p.border).child(
                    div()
                        .id("clear-console")
                        .size(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .hover(|s| s.bg(p.hover))
                        .child(icon(IconName::Trash, 14., p.icon))
                        .on_click(cx.listener(|w, _, _, cx| {
                            w.console.clear();
                            cx.notify();
                        })),
                ),
            )
    }
}

fn time_of_day() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let s = secs % 86400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

impl Render for ConnWorkspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let content: AnyElement = match self.active_tab.and_then(|i| self.tabs.get(i)).map(|t| &t.content) {
            Some(TabContent::Data(d)) => d.clone().into_any_element(),
            Some(TabContent::Query(q)) => q.clone().into_any_element(),
            None => placeholder(
                if self.tabs.is_empty() { "No item\n\nSelect a table in the left sidebar, press Ctrl+P to open anything or Ctrl+E for a SQL editor." } else { "No item" },
                cx,
            )
            .text_size(px(16.))
            .into_any_element(),
        };
        h_flex()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(|w, _: &OpenAnything, window, cx| w.open_anything(window, cx)))
            .on_action(cx.listener(|w, _: &NewQueryTab, window, cx| w.new_query(window, cx)))
            .on_action(cx.listener(|w, _: &InsertRow, window, cx| {
                if let Some(d) = w.active_data() {
                    d.update(cx, |d, cx| d.add_row(window, cx));
                }
            }))
            .on_action(cx.listener(|w, _: &DuplicateRow, window, cx| {
                if let Some(d) = w.active_data() {
                    d.update(cx, |d, cx| d.duplicate_selected(window, cx));
                }
            }))
            .on_action(cx.listener(|w, _: &DeleteRows, window, cx| {
                if let Some(d) = w.active_data() {
                    d.update(cx, |d, cx| d.delete_selected(window, cx));
                }
            }))
            .on_action(cx.listener(|w, _: &ShowFilter, window, cx| {
                if let Some(d) = w.active_data() {
                    d.update(cx, |d, cx| d.toggle_filters(window, cx));
                }
            }))
            .on_action(cx.listener(|w, _: &Commit, window, cx| {
                if let Some(q) = w.active_query() {
                    q.update(cx, |q, cx| q.save_query(window, cx));
                } else {
                    w.commit(window, cx);
                }
            }))
            .on_action(cx.listener(|w, _: &Reload, window, cx| w.reload(window, cx)))
            .on_action(cx.listener(|w, _: &OpenProcessList, window, cx| crate::processes::open(w.session.clone(), window, cx)))
            .on_action(cx.listener(|w, _: &OpenFile, window, cx| w.open_file(window, cx)))
            .on_action(cx.listener(|w, _: &Reconnect, window, cx| w.reconnect(window, cx)))
            .on_action(cx.listener(|w, _: &DatabaseList, window, cx| w.database_list(window, cx)))
            .on_action(cx.listener(|w, _: &PreviewChanges, window, cx| w.preview(window, cx)))
            .on_action(cx.listener(|w, _: &DiscardChanges, window, cx| w.discard(window, cx)))
            .on_action(cx.listener(|w, _: &ToggleDetail, _, cx| w.toggle_detail(cx)))
            .on_action(cx.listener(|w, _: &ToggleConsole, _, cx| w.toggle_console(cx)))
            .on_action(cx.listener(|w, _: &ToggleSidebar, _, cx| w.toggle_sidebar(cx)))
            .on_action(cx.listener(|w, _: &Disconnect, window, cx| w.disconnect(window, cx)))
            .on_action(cx.listener(|w, _: &CloseTab, window, cx| {
                if let Some(i) = w.active_tab {
                    w.close_tab(i, window, cx);
                }
            }))
            .on_action(cx.listener(|w, _: &NextTab, window, cx| {
                if let Some(i) = w.active_tab {
                    let n = (i + 1) % w.tabs.len().max(1);
                    w.activate(n, window, cx);
                }
            }))
            .on_action(cx.listener(|w, _: &PrevTab, window, cx| {
                if let Some(i) = w.active_tab {
                    let n = (i + w.tabs.len() - 1) % w.tabs.len().max(1);
                    w.activate(n, window, cx);
                }
            }))
            .when(self.show_sidebar, |d| {
                d.child(div().w(px(230.)).h_full().flex_shrink_0().child(self.sidebar.clone())).child(crate::widgets::vline(cx))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .bg(p.window)
                    .child(self.render_tab_bar(cx))
                    .child(div().flex_1().overflow_hidden().child(content))
                    .when(self.show_console, |d| d.child(self.render_console(cx))),
            )
            .when(self.show_detail, |d| {
                d.child(crate::widgets::vline(cx)).child(div().w(px(260.)).h_full().flex_shrink_0().child(self.row_detail.clone()))
            })
    }
}

impl Focusable for ConnWorkspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[allow(dead_code)]
fn _kind(_: ItemKind) {}
