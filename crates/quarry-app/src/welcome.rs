//! Welcome window: app identity on the left, saved connections on the right
//! (MainWindow in the original).

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{TitleBar, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::{ConnTag, ConnectionConfig};

use crate::state::{AppState, db};
use crate::theme::PaletteExt as _;
use crate::widgets::{driver_badge, icon};
use crate::{connection_form, prompt, workspace};

pub struct Welcome {
    search: Entity<InputState>,
    selected: Option<String>,
    connecting: Option<String>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

pub fn open(cx: &mut App) {
    if let Some(w) = cx.try_global::<WelcomeWindow>().and_then(|w| w.0) {
        if w.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let size = size(px(760.), px(536.));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
        window_min_size: Some(size),
        ..TitleBar::window_options()
    };
    let handle = gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Welcome::new(window, cx)))
        .ok()
        .map(|(w, _)| w);
    cx.set_global(WelcomeWindow(handle));
}

/// Closes the welcome window, if open.
pub fn close(cx: &mut App) {
    if let Some(w) = cx.try_global::<WelcomeWindow>().and_then(|w| w.0) {
        let _ = w.update(cx, |_, window, _| window.remove_window());
    }
    cx.set_global(WelcomeWindow(None));
}

struct WelcomeWindow(Option<AnyWindowHandle>);
impl Global for WelcomeWindow {}

impl Welcome {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search for connection..."));
        let subs = vec![
            cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.observe_global::<AppState>(|_, cx| cx.notify()),
        ];
        Self { search, selected: None, connecting: None, focus: cx.focus_handle(), _subs: subs }
    }

    fn connections(&self, cx: &App) -> Vec<ConnectionConfig> {
        let q = self.search.read(cx).value().to_lowercase();
        AppState::get(cx)
            .store
            .list
            .connections
            .iter()
            .filter(|c| q.is_empty() || c.display_name().to_lowercase().contains(&q) || c.detail().to_lowercase().contains(&q))
            .cloned()
            .collect()
    }

    fn connect(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = AppState::get(cx).store.resolve(&id) else {
            return;
        };
        if config.password.is_none() && !config.save_password && !config.driver.is_file_based() {
            let this = cx.entity().downgrade();
            prompt::ask(
                "Password",
                &format!("Enter the password for {}", config.display_name()),
                "",
                true,
                window,
                cx,
                move |pw, window, cx| {
                    let mut config = config.clone();
                    config.password = Some(pw);
                    let _ = this.update(cx, |this, cx| this.open_session(config, window, cx));
                },
            );
            return;
        }
        self.open_session(config, window, cx);
    }

    fn open_session(&mut self, config: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        self.connecting = Some(config.id.clone());
        cx.notify();
        let sessions = AppState::get(cx).sessions.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = db(async move { sessions.connect(config).await }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.connecting = None;
                cx.notify();
                match result {
                    Ok(session) => {
                        workspace::open_session(session, cx);
                        window.remove_window();
                        cx.set_global(WelcomeWindow(None));
                    }
                    Err(e) => prompt::error("Connection failed", &format!("{e:#}"), window, cx),
                }
            });
        })
        .detach();
    }

    fn delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = AppState::get(cx).store.find(&id).map(|c| c.display_name()).unwrap_or_default();
        prompt::confirm(
            "Delete connection",
            &format!("Do you want to delete the connection \"{name}\"?"),
            "Delete",
            window,
            cx,
            move |_, cx| {
                cx.update_global::<AppState, _>(|s, _| {
                    let _ = s.store.remove(&id);
                });
            },
        );
    }

    fn duplicate(&mut self, id: String, cx: &mut Context<Self>) {
        cx.update_global::<AppState, _>(|s, _| {
            if let Some(mut c) = s.store.resolve(&id) {
                c.id = uuid_like();
                c.name = format!("{} copy", c.display_name());
                let _ = s.store.upsert(c);
            }
        });
    }

    fn new_group(window: &mut Window, cx: &mut App) {
        prompt::ask("New group", "Group name", "", false, window, cx, |name, _, cx| {
            if !name.trim().is_empty() {
                cx.update_global::<AppState, _>(|s, _| {
                    let _ = s.store.add_group(name.trim());
                });
            }
        });
    }

    fn move_to_group(id: String, group: Option<String>, cx: &mut App) {
        cx.update_global::<AppState, _>(|s, _| {
            if let Some(mut c) = s.store.resolve(&id) {
                c.group_id = group;
                let _ = s.store.upsert(c);
            }
        });
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let big_button = |id: &'static str, icon_name: IconName, label: &'static str| {
            h_flex()
                .id(id)
                .h(px(40.))
                .px(px(12.))
                .gap(px(12.))
                .rounded(px(6.))
                .border_1()
                .border_color(p.button_border)
                .bg(p.button)
                .text_size(px(15.))
                .text_color(p.text)
                .hover(|s| s.bg(p.button_hover))
                .child(icon(icon_name, 18., p.text))
                .child(label)
        };
        v_flex()
            .w(px(277.))
            .h_full()
            .flex_shrink_0()
            .bg(p.sidebar)
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .pt(px(46.))
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        div()
                            .size(px(120.))
                            .rounded_full()
                            .bg(p.accent)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(IconName::Database, 60., white())),
                    )
                    .child(div().mt(px(8.)).text_size(px(36.)).font_weight(FontWeight::BOLD).text_color(p.text).child("Quarry"))
                    .child(
                        div()
                            .mt(px(8.))
                            .text_size(px(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.secondary_text)
                            .child(format!("Version {} (open source)", env!("CARGO_PKG_VERSION"))),
                    ),
            )
            .child(
                v_flex()
                    .px(px(16.))
                    .pb(px(34.))
                    .gap(px(16.))
                    .child(big_button("backup", IconName::DatabaseBackup, "Backup database...").on_click(|_, window, cx| {
                        crate::backup::open_backup(window, cx)
                    }))
                    .child(big_button("restore", IconName::Upload, "Restore database...").on_click(|_, window, cx| {
                        crate::backup::open_restore(window, cx)
                    }))
                    .child(big_button("create", IconName::Plug, "Create connection...").on_click(|_, _, cx| {
                        connection_form::open_driver_picker(cx)
                    })),
            )
    }

    fn render_row(&self, c: &ConnectionConfig, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let selected = self.selected.as_deref() == Some(c.id.as_str());
        let connecting = self.connecting.as_deref() == Some(c.id.as_str());
        let id = c.id.clone();
        let groups = AppState::get(cx).store.list.groups.clone();
        let fg = if selected { p.selection_text } else { p.text };
        let sub = if selected { p.selection_text.opacity(0.8) } else { p.secondary_text };
        h_flex()
            .id(SharedString::from(format!("conn-{}", c.id)))
            .mx(px(8.))
            .px(px(8.))
            .py(px(4.))
            .gap(px(8.))
            .rounded(px(4.))
            .when(selected, |d| d.bg(p.selection))
            .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
            .child(driver_badge(c.driver, 36., cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .text_size(px(14.))
                            .child(div().text_color(fg).truncate().child(c.display_name()))
                            .when(c.tag != ConnTag::None, |d| {
                                d.child(div().text_color(p.tag(c.tag)).child(format!("({})", c.tag.label())))
                            })
                            .when(connecting, |d| d.child(div().text_color(sub).child("connecting..."))),
                    )
                    .child(div().text_size(px(12.)).text_color(sub).truncate().child(c.detail())),
            )
            .on_click(cx.listener({
                let id = id.clone();
                move |this, ev: &ClickEvent, window, cx| {
                    this.selected = Some(id.clone());
                    cx.notify();
                    if crate::widgets::is_double(ev) {
                        this.connect(id.clone(), window, cx);
                    }
                }
            }))
            .context_menu({
                let this = cx.entity().downgrade();
                move |menu: PopupMenu, _, _| {
                    let item = |label: &'static str, f: fn(&mut Welcome, String, &mut Window, &mut Context<Welcome>)| {
                        let this = this.clone();
                        let id = id.clone();
                        PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            let _ = this.update(cx, |w, cx| f(w, id.clone(), window, cx));
                        })
                    };
                    let mut move_menu = menu
                        .item(item("Connect", |w, id, window, cx| w.connect(id, window, cx)))
                        .item(item("Edit...", |_, id, _, cx| connection_form::open_editor(&id, cx)))
                        .item(item("Duplicate", |w, id, _, cx| w.duplicate(id, cx)))
                        .item(item("Delete", |w, id, window, cx| w.delete(id, window, cx)))
                        .separator()
                        .item(PopupMenuItem::new("New Group...").on_click(|_, window, cx| Welcome::new_group(window, cx)))
                        .separator()
                        .label("Move to group");
                    {
                        let id = id.clone();
                        move_menu = move_menu.item(PopupMenuItem::new("No group").on_click(move |_, _, cx| {
                            Welcome::move_to_group(id.clone(), None, cx)
                        }));
                    }
                    for g in &groups {
                        let (id, gid) = (id.clone(), g.id.clone());
                        move_menu = move_menu.item(PopupMenuItem::new(g.name.clone()).on_click(move |_, _, cx| {
                            Welcome::move_to_group(id.clone(), Some(gid.clone()), cx)
                        }));
                    }
                    move_menu
                }
            })
    }

    fn render_group_header(&self, id: String, name: String, expanded: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        h_flex()
            .id(SharedString::from(format!("group-{id}")))
            .mx(px(8.))
            .px(px(4.))
            .py(px(4.))
            .gap(px(8.))
            .rounded(px(4.))
            .hover(|s| s.bg(p.hover))
            .child(icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight }, 12., p.secondary_text))
            .child(icon(IconName::Folder, 28., p.accent))
            .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(name))
            .on_click({
                let id = id.clone();
                move |_, _, cx| {
                    cx.update_global::<AppState, _>(|s, _| {
                        if let Some(g) = s.store.list.groups.iter_mut().find(|g| g.id == id) {
                            g.expanded = !g.expanded;
                        }
                        let _ = s.store.save();
                    });
                }
            })
            .context_menu(move |menu: PopupMenu, _, _| {
                let id = id.clone();
                menu.item(PopupMenuItem::new("New Group...").on_click(|_, window, cx| Welcome::new_group(window, cx)))
                    .item(PopupMenuItem::new("Delete Group").on_click(move |_, _, cx| {
                        cx.update_global::<AppState, _>(|s, _| {
                            let _ = s.store.remove_group(&id);
                        });
                    }))
            })
    }
}

fn uuid_like() -> String {
    ConnectionConfig::default().id
}

impl Render for Welcome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let conns = self.connections(cx);
        let groups = AppState::get(cx).store.list.groups.clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        for g in &groups {
            let members: Vec<&ConnectionConfig> = conns.iter().filter(|c| c.group_id.as_deref() == Some(g.id.as_str())).collect();
            rows.push(self.render_group_header(g.id.clone(), g.name.clone(), g.expanded, cx).into_any_element());
            if g.expanded {
                for c in members {
                    rows.push(div().pl(px(24.)).child(self.render_row(c, cx)).into_any_element());
                }
            }
        }
        for c in conns.iter().filter(|c| c.group_id.as_ref().is_none_or(|gid| !groups.iter().any(|g| &g.id == gid))) {
            rows.push(self.render_row(c, cx).into_any_element());
        }
        let empty = rows.is_empty();

        h_flex()
            .id("welcome")
            .size_full()
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &crate::actions::Preferences, window, cx| crate::preferences::open(window, cx)))
            .bg(p.window)
            .text_color(p.text)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "enter" {
                    if let Some(id) = this.selected.clone() {
                        this.connect(id, window, cx);
                    }
                }
            }))
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .child(TitleBar::new().h(px(35.)).bg(p.window).border_b_0())
                    .child(
                        h_flex()
                            .h(px(28.))
                            .px(px(8.))
                            .gap(px(4.))
                            .child(
                                div()
                                    .id("new-conn")
                                    .size(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(4.))
                                    .hover(|s| s.bg(p.hover))
                                    .child(icon(IconName::Plus, 12., p.icon))
                                    .on_click(|_, _, cx| connection_form::open_driver_picker(cx)),
                            )
                            .child(Input::new(&self.search).prefix(crate::widgets::search_icon(cx)).cleanable(true).h(px(24.))),
                    )
                    .child(
                        div()
                            .id("connections")
                            .flex_1()
                            .mt(px(8.))
                            .overflow_y_scroll()
                            .child(v_flex().gap(px(2.)).children(rows))
                            .when(empty, |d| {
                                d.child(crate::widgets::placeholder(
                                    "No connections yet. Click + or \"Create connection...\" to add one.",
                                    cx,
                                ))
                            }),
                    ),
            )
    }
}

impl Focusable for Welcome {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[allow(dead_code)]
fn _window_ext(window: &mut Window, cx: &mut App) {
    window.close_dialog(cx);
}
