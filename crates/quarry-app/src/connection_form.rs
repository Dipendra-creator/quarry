//! "Create connection" driver picker and the connection form windows.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::{ConnTag, ConnectionConfig, DriverKind, Session, SslMode};

use crate::state::{AppState, db};
use crate::theme::{PaletteExt as _, UI_TEXT};
use crate::widgets::{driver_badge, footer_button, icon};
use crate::{prompt, welcome, workspace};

fn window_options(w: f32, h: f32, cx: &App) -> WindowOptions {
    let size = size(px(w), px(h));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
        window_min_size: Some(size),
        is_resizable: false,
        ..TitleBar::window_options()
    }
}

pub fn open_driver_picker(cx: &mut App) {
    let _ = gpui_kit::open_window(window_options(764., 490., cx), cx, |window, cx| cx.new(|cx| DriverPicker::new(window, cx)));
}

pub fn open_new(driver: DriverKind, cx: &mut App) {
    open_form(ConnectionConfig::new(driver), true, cx);
}

pub fn open_editor(id: &str, cx: &mut App) {
    if let Some(c) = AppState::get(cx).store.resolve(id) {
        open_form(c, false, cx);
    }
}

fn open_form(config: ConnectionConfig, is_new: bool, cx: &mut App) {
    let h = if config.driver.is_file_based() { 420. } else { 760. };
    let _ = gpui_kit::open_window(window_options(560., h, cx), cx, move |window, cx| {
        cx.new(|cx| ConnectionForm::new(config, is_new, window, cx))
    });
}

pub struct DriverPicker {
    search: Entity<InputState>,
    selected: DriverKind,
    _sub: Subscription,
}

impl DriverPicker {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search..."));
        let _sub = cx.subscribe_in(&search, window, |this: &mut Self, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                this.create(window, cx);
            }
            cx.notify();
        });
        // Typing filters right away.
        let focus = search.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        Self { search, selected: DriverKind::Postgres, _sub }
    }

    fn matches(&self, cx: &App) -> Vec<DriverKind> {
        let q = self.search.read(cx).value().to_lowercase();
        DriverKind::ALL
            .into_iter()
            .filter(|d| q.is_empty() || d.label().to_lowercase().contains(&q) || d.keywords().iter().any(|k| k.contains(q.as_str())))
            .collect()
    }

    /// The selected driver, or the first one the search shows when the search hides it.
    fn effective(&self, cx: &App) -> Option<DriverKind> {
        let shown = self.matches(cx);
        if shown.contains(&self.selected) { Some(self.selected) } else { shown.first().copied() }
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(driver) = self.effective(cx) else { return };
        open_new(driver, cx);
        window.remove_window();
    }
}

impl Render for DriverPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let current = self.effective(cx);
        let items = self.matches(cx);
        let none = items.is_empty();
        v_flex()
            .size_full()
            .bg(p.window)
            .text_color(p.text)
            .child(TitleBar::new().bg(p.window).border_b_0().child(div().text_size(UI_TEXT).child("Create connection")))
            .child(
                v_flex()
                    .flex_1()
                    .p(px(16.))
                    .pt(px(4.))
                    .gap(px(8.))
                    .child(Input::new(&self.search).prefix(crate::widgets::search_icon(cx)))
                    .child(
                        div()
                            .id("drivers")
                            .flex_1()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(p.border)
                            .overflow_y_scroll()
                            .when(none, |d| d.child(crate::widgets::placeholder("No matching database", cx)))
                            .child(h_flex().flex_wrap().justify_center().children(items.into_iter().map(|d| {
                                let selected = Some(d) == current;
                                v_flex()
                                    .id(d.label())
                                    .w(px(100.))
                                    .h(px(104.))
                                    .my(px(12.))
                                    .items_center()
                                    .pt(px(4.))
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(if selected { p.accent } else { transparent_black() })
                                    .child(driver_badge(d, 52., cx))
                                    .child(div().mt(px(8.)).text_size(px(14.)).child(d.label()))
                                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                                        this.selected = d;
                                        cx.notify();
                                        if crate::widgets::is_double(ev) {
                                            this.create(window, cx);
                                        }
                                    }))
                            }))),
                    )
                    .child(
                        h_flex()
                            .mt(px(8.))
                            .justify_between()
                            .child(Button::new("cancel").label("Cancel").small().on_click(|_, window, _| window.remove_window()))
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(Button::new("group").label("New group").small().on_click(|_, window, cx| {
                                        prompt::ask("New group", "Group name", "", false, window, cx, |name, _, cx| {
                                            if !name.trim().is_empty() {
                                                cx.update_global::<AppState, _>(|s, _| {
                                                    let _ = s.store.add_group(name.trim());
                                                });
                                            }
                                        })
                                    }))
                                    .child(
                                        Button::new("create")
                                            .label("Create")
                                            .primary()
                                            .small()
                                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                                    ),
                            ),
                    ),
            )
    }
}

enum TestState {
    Idle,
    Running,
    Ok(String),
    Failed(String),
}

pub struct ConnectionForm {
    config: ConnectionConfig,
    is_new: bool,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    file_path: Entity<InputState>,
    startup: Entity<InputState>,
    over_ssh: bool,
    ssh_host: Entity<InputState>,
    ssh_port: Entity<InputState>,
    ssh_user: Entity<InputState>,
    ssh_password: Entity<InputState>,
    ssh_key: Entity<InputState>,
    ssh_passphrase: Entity<InputState>,
    test: TestState,
}

impl ConnectionForm {
    fn new(config: ConnectionConfig, is_new: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        fn field(window: &mut Window, cx: &mut App, value: &str, placeholder: &str, masked: bool) -> Entity<InputState> {
            let (value, placeholder) = (value.to_string(), placeholder.to_string());
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value).masked(masked))
        }
        let default_port = config.driver.default_port().map(|p| p.to_string()).unwrap_or_default();
        let host_hint = if config.driver == DriverKind::SqlServer { "127.0.0.1, or server.database.windows.net" } else { "127.0.0.1" };
        let port = config.port.map(|p| p.to_string()).unwrap_or_default();
        let ssh = config.ssh.clone().unwrap_or_default();
        let ssh_port = if ssh.port == 0 { String::new() } else { ssh.port.to_string() };
        Self {
            over_ssh: config.ssh.is_some(),
            name: field(window, cx, &config.name, "Connection name", false),
            host: field(window, cx, &config.host, host_hint, false),
            port: field(window, cx, &port, &default_port, false),
            user: field(window, cx, &config.user, "user name", false),
            password: field(window, cx, config.password.as_deref().unwrap_or_default(), "password", true),
            database: field(window, cx, &config.database, "database name", false),
            file_path: field(window, cx, &config.file_path, "Path to the database file", false),
            startup: field(window, cx, &config.startup_commands, match config.driver {
                DriverKind::Postgres => "SQL run after connecting, e.g. SET search_path TO app;",
                DriverKind::SqlServer => "SQL run after connecting, e.g. SET LANGUAGE us_english;",
                DriverKind::Sqlite => "SQL run after opening, e.g. PRAGMA journal_mode = WAL;",
                _ => "SQL run after connecting, e.g. SET time_zone = '+00:00';",
            }, false),
            ssh_host: field(window, cx, &ssh.host, "ssh.example.com", false),
            ssh_port: field(window, cx, &ssh_port, "22", false),
            ssh_user: field(window, cx, &ssh.user, "ssh user", false),
            ssh_password: field(window, cx, ssh.password.as_deref().unwrap_or_default(), "ssh password", true),
            ssh_key: field(window, cx, &ssh.key_path, "Private key file (empty = password)", false),
            ssh_passphrase: field(window, cx, ssh.key_passphrase.as_deref().unwrap_or_default(), "key passphrase", true),
            config,
            is_new,
            test: TestState::Idle,
        }
    }

    /// The config with the current field values.
    fn collect(&self, cx: &App) -> Result<ConnectionConfig, String> {
        let v = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let mut c = self.config.clone();
        c.name = v(&self.name);
        c.host = v(&self.host);
        c.user = v(&self.user);
        c.database = v(&self.database);
        c.file_path = v(&self.file_path);
        c.startup_commands = self.startup.read(cx).value().to_string();
        let pw = self.password.read(cx).value().to_string();
        c.password = (!pw.is_empty()).then_some(pw);
        let port = v(&self.port);
        c.port = if port.is_empty() { None } else { Some(port.parse().map_err(|_| format!("Invalid port \"{port}\""))?) };
        if c.driver.is_file_based() && c.file_path.is_empty() {
            return Err("Select a database file.".into());
        }
        c.ssh = if self.over_ssh && !c.driver.is_file_based() {
            let port = v(&self.ssh_port);
            let secret = |e: &Entity<InputState>| Some(e.read(cx).value().to_string()).filter(|s| !s.is_empty());
            Some(quarry_core::ssh::SshConfig {
                host: v(&self.ssh_host),
                port: if port.is_empty() { 0 } else { port.parse().map_err(|_| format!("Invalid SSH port \"{port}\""))? },
                user: v(&self.ssh_user),
                password: secret(&self.ssh_password),
                key_path: v(&self.ssh_key),
                key_passphrase: secret(&self.ssh_passphrase),
            })
        } else {
            None
        };
        if c.ssh.as_ref().is_some_and(|s| s.host.is_empty()) {
            return Err("Enter the SSH server.".into());
        }
        Ok(c)
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<ConnectionConfig> {
        match self.collect(cx) {
            Ok(c) => {
                let result = cx.update_global::<AppState, _>(|s, _| s.store.upsert(c.clone()));
                match result {
                    Ok(()) => Some(c),
                    Err(e) => {
                        prompt::error("Could not save", &format!("{e:#}"), window, cx);
                        None
                    }
                }
            }
            Err(e) => {
                prompt::error("Invalid connection", &e, window, cx);
                None
            }
        }
    }

    fn run_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = match self.collect(cx) {
            Ok(c) => c,
            Err(e) => {
                self.test = TestState::Failed(e);
                cx.notify();
                return;
            }
        };
        self.test = TestState::Running;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = db(async move { Session::connect(config).await.map(|s| s.title()) }).await;
            let _ = this.update(cx, |this, cx| {
                this.test = match result {
                    Ok(title) => TestState::Ok(format!("Connection is OK — {title}")),
                    Err(e) => TestState::Failed(format!("{e:#}")),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.save(window, cx) else { return };
        self.test = TestState::Running;
        cx.notify();
        let sessions = AppState::get(cx).sessions.clone();
        sessions.disconnect(&config.id);
        cx.spawn_in(window, async move |this, cx| {
            let result = db(async move { sessions.connect(config).await }).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(session) => {
                    workspace::open_session(session, cx);
                    welcome::close(cx);
                    window.remove_window();
                }
                Err(e) => {
                    this.test = TestState::Failed(format!("{e:#}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.file_path.clone();
        Self::pick_into(input, window, cx);
    }

    fn pick_into(input: Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = input.update_in(cx, |state, window, cx| {
                        state.set_value(path.to_string_lossy().to_string(), window, cx)
                    });
                }
            }
        })
        .detach();
    }

    fn row(label: &'static str, field: impl IntoElement, cx: &App) -> Div {
        h_flex()
            .gap(px(10.))
            .child(div().w(px(110.)).flex_shrink_0().text_right().text_size(UI_TEXT).text_color(cx.palette().text).child(label))
            .child(div().flex_1().child(field))
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let d = self.config.driver;
        let status = self.config.status_color;
        let tag = self.config.tag;
        let swatches = h_flex().gap(px(6.)).children((0..6u8).map(|i| {
            div()
                .id(("status", i as usize))
                .size(px(18.))
                .rounded_full()
                .bg(p.status[i as usize])
                .border_1()
                .border_color(p.border)
                .when(i == status, |s| s.border_2().border_color(p.accent))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.config.status_color = i;
                    cx.notify();
                }))
        }));
        let tag_button = Button::new("tag")
            .label(tag.label())
            .small()
            .outline()
            .dropdown_caret(true)
            .dropdown_menu({
                let this = cx.entity().downgrade();
                move |menu, _, _| {
                    ConnTag::ALL.iter().fold(menu, |menu, t| {
                        let (this, t) = (this.clone(), *t);
                        menu.item(PopupMenuItem::new(t.label()).on_click(move |_, _, cx| {
                            let _ = this.update(cx, |f, cx| {
                                f.config.tag = t;
                                cx.notify();
                            });
                        }))
                    })
                }
            });
        let ssl_button = Button::new("ssl")
            .label(self.config.ssl_mode.label())
            .small()
            .outline()
            .dropdown_caret(true)
            .dropdown_menu({
                let this = cx.entity().downgrade();
                move |menu, _, _| {
                    SslMode::ALL.iter().fold(menu, |menu, m| {
                        let (this, m) = (this.clone(), *m);
                        menu.item(PopupMenuItem::new(m.label()).on_click(move |_, _, cx| {
                            let _ = this.update(cx, |f, cx| {
                                f.config.ssl_mode = m;
                                cx.notify();
                            });
                        }))
                    })
                }
            });
        let pw_mode = Button::new("pwmode")
            .label(if self.config.save_password { "Store in keychain" } else { "Ask every time" })
            .small()
            .outline()
            .dropdown_caret(true)
            .dropdown_menu({
                let this = cx.entity().downgrade();
                move |menu, _, _| {
                    let (a, b) = (this.clone(), this.clone());
                    menu.item(PopupMenuItem::new("Store in keychain").on_click(move |_, _, cx| {
                        let _ = a.update(cx, |f, cx| {
                            f.config.save_password = true;
                            cx.notify();
                        });
                    }))
                    .item(PopupMenuItem::new("Ask every time").on_click(move |_, _, cx| {
                        let _ = b.update(cx, |f, cx| {
                            f.config.save_password = false;
                            cx.notify();
                        });
                    }))
                }
            });

        let fields = if d.is_file_based() {
            v_flex().gap(px(10.)).child(Self::row(
                "Database path",
                h_flex()
                    .gap(px(8.))
                    .child(div().flex_1().child(Input::new(&self.file_path)))
                    .child(footer_button("pick", cx).child("Select file...").on_click(cx.listener(|this, _, window, cx| this.pick_file(window, cx)))),
                cx,
            ))
        } else {
            v_flex()
                .gap(px(10.))
                .child(Self::row(
                    "Host/Socket",
                    h_flex()
                        .gap(px(8.))
                        .child(div().flex_1().child(Input::new(&self.host)))
                        .child(div().text_size(UI_TEXT).child("Port"))
                        .child(div().w(px(80.)).child(Input::new(&self.port))),
                    cx,
                ))
                .child(Self::row("User", Input::new(&self.user), cx))
                .child(Self::row(
                    "Password",
                    h_flex().gap(px(8.)).child(div().flex_1().child(Input::new(&self.password).mask_toggle())).child(pw_mode),
                    cx,
                ))
                .child(Self::row("Database", Input::new(&self.database), cx))
                .child(Self::row(if d == DriverKind::SqlServer { "Encryption" } else { "SSL mode" }, h_flex().child(ssl_button), cx))
                .when(d.has_schemas(), |el| {
                    el.child(Self::row(
                        "System schemas",
                        h_flex().gap(px(8.)).child(
                            gpui_kit::component::switch::Switch::new("system-schemas")
                                .checked(self.config.show_system_schemas)
                                .on_click(cx.listener(|f, on: &bool, _, cx| {
                                    f.config.show_system_schemas = *on;
                                    cx.notify();
                                })),
                        )
                        .child(div().text_size(UI_TEXT).text_color(p.secondary_text).child(if d == DriverKind::SqlServer {
                            "Show sys, INFORMATION_SCHEMA and role schemas"
                        } else {
                            "Show pg_catalog and information_schema"
                        })),
                        cx,
                    ))
                })
                .child(Self::row(
                    "Over SSH",
                    h_flex().child(
                        gpui_kit::component::switch::Switch::new("over-ssh")
                            .checked(self.over_ssh)
                            .on_click(cx.listener(|f, on: &bool, _, cx| {
                                f.over_ssh = *on;
                                cx.notify();
                            })),
                    ),
                    cx,
                ))
                .when(self.over_ssh, |d| {
                    d.child(Self::row(
                        "SSH server",
                        h_flex()
                            .gap(px(8.))
                            .child(div().flex_1().child(Input::new(&self.ssh_host)))
                            .child(div().text_size(UI_TEXT).child("Port"))
                            .child(div().w(px(80.)).child(Input::new(&self.ssh_port))),
                        cx,
                    ))
                    .child(Self::row("SSH user", Input::new(&self.ssh_user), cx))
                    .child(Self::row("SSH password", Input::new(&self.ssh_password).mask_toggle(), cx))
                    .child(Self::row(
                        "Private key",
                        h_flex()
                            .gap(px(8.))
                            .child(div().flex_1().child(Input::new(&self.ssh_key)))
                            .child(footer_button("pick-key", cx).child("Select...").on_click(cx.listener(|f, _, window, cx| {
                                let input = f.ssh_key.clone();
                                Self::pick_into(input, window, cx);
                            }))),
                        cx,
                    ))
                    .child(Self::row("Passphrase", Input::new(&self.ssh_passphrase).mask_toggle(), cx))
                })
        };

        let test_line = match &self.test {
            TestState::Idle => None,
            TestState::Running => Some(("Connecting...".to_string(), p.secondary_text)),
            TestState::Ok(m) => Some((m.clone(), p.success)),
            TestState::Failed(m) => Some((m.clone(), p.danger)),
        };

        v_flex()
            .size_full()
            .bg(p.window)
            .text_color(p.text)
            .child(
                TitleBar::new().bg(p.window).border_b_0().child(
                    h_flex()
                        .gap(px(8.))
                        .text_size(UI_TEXT)
                        .child(driver_badge(d, 18., cx))
                        .child(format!("{} Connection", d.label())),
                ),
            )
            .child(
                v_flex()
                    .id("form-body")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(px(20.))
                    .py(px(12.))
                    .gap(px(10.))
                    .child(Self::row("Name", Input::new(&self.name), cx))
                    .child(Self::row("Status color", h_flex().gap(px(16.)).child(swatches).child(div().text_size(UI_TEXT).child("Tag")).child(tag_button), cx))
                    .child(crate::widgets::hline(cx))
                    .child(fields)
                    .child(Self::row(
                        "Bootstrap",
                        Input::new(&self.startup),
                        cx,
                    ))
                    .when_some(test_line, |el, (msg, color)| {
                        el.child(div().text_size(UI_TEXT).text_color(color).child(msg))
                    }),
            )
            .child(
                h_flex()
                    .px(px(20.))
                    .pb(px(16.))
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        h_flex()
                            .flex_1()
                            .gap(px(6.))
                            .text_size(UI_TEXT)
                            .text_color(p.secondary_text)
                            .child(icon(IconName::Lock, 12., p.secondary_text))
                            .child("Passwords are kept in the system keychain."),
                    )
                    .child(Button::new("save").label("Save").small().on_click(cx.listener(|this, _, window, cx| {
                        if this.save(window, cx).is_some() {
                            window.remove_window();
                        }
                    })))
                    .child(Button::new("test").label("Test").small().on_click(cx.listener(|this, _, window, cx| this.run_test(window, cx))))
                    .child(
                        Button::new("connect")
                            .label(if self.is_new { "Connect" } else { "Save & Connect" })
                            .primary()
                            .small()
                            .loading(matches!(self.test, TestState::Running))
                            .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                    ),
            )
    }
}
