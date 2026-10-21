//! AI assistant pane (Ctrl+L): a chat about the workspace's connection.
//! The model learns the schema and data and runs queries through the
//! quarry-ai tools; changes wait for the user's approval (`ask_approval`).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_ai::{Approval, WritePolicy};
use quarry_ai::assistant::{Chat, ChatEvent};
use quarry_core::Session;
use tokio::sync::mpsc;

use crate::state::AppState;
use crate::theme::{MONO, PaletteExt as _, UI_TEXT};
use crate::widgets::{icon, placeholder};

pub enum AssistantEvent {
    /// SQL from a reply, for an editor tab; `run` runs it too.
    OpenSql { sql: String, run: bool },
    Close,
}

enum Entry {
    User(String),
    Assistant(Vec<Part>),
    Error(String),
}

enum Part {
    Text(String),
    Tool(ToolCall),
}

struct ToolCall {
    id: String,
    name: String,
    input: String,
    /// Output and whether it is an error; `None` while running.
    output: Option<(String, bool)>,
    open: bool,
}

const SUGGESTIONS: [&str; 3] = [
    "Give me an overview of this database: its tables, how they relate and what they hold.",
    "Which tables hold the most rows?",
    "Find tables without a primary key, and columns that need an index.",
];

pub struct AssistantPanel {
    chat: Arc<tokio::sync::Mutex<Chat>>,
    session: Arc<Session>,
    entries: Vec<Entry>,
    input: Entity<TextareaState>,
    running: Option<tokio::task::AbortHandle>,
    scroll: ScrollHandle,
    /// Code-block buttons are built outside this entity; they report here.
    code_actions: mpsc::UnboundedSender<(String, bool)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<AssistantEvent> for AssistantPanel {}

impl Drop for AssistantPanel {
    /// A closed workspace stops its reply, and with it any CLI process.
    fn drop(&mut self) {
        if let Some(task) = &self.running {
            task.abort();
        }
    }
}

impl AssistantPanel {
    pub fn new(session: Arc<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .submit_on_enter(true)
                .placeholder("Ask about your data, or paste a query…")
        });
        let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = ev {
                let text = input.read(cx).value().to_string();
                this.send(text, window, cx);
            }
        });
        let (code_actions, mut rx) = mpsc::unbounded_channel::<(String, bool)>();
        cx.spawn(async move |this, cx| {
            while let Some((sql, run)) = rx.recv().await {
                if this.update(cx, |_, cx| cx.emit(AssistantEvent::OpenSql { sql, run })).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            chat: Arc::new(tokio::sync::Mutex::new(Chat::new(session.clone()))),
            session,
            entries: vec![],
            input,
            running: None,
            scroll: ScrollHandle::new(),
            code_actions,
            _subs: vec![sub],
        }
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }

    /// Sends a message, unless a reply is still coming.
    pub fn send(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.running.is_some() {
            // Kept for when the current reply is done.
            self.input.update(cx, |s, cx| s.set_value(text, window, cx));
            return;
        }
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        self.entries.push(Entry::User(text.clone()));
        self.entries.push(Entry::Assistant(vec![]));
        let state = AppState::get(cx);
        let (settings, env) = (state.settings.ai.clone(), state.ai_env());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let chat = self.chat.clone();
        let task = quarry_core::runtime().spawn(async move {
            let mut chat = chat.lock().await;
            if let Err(e) = chat.send(&settings, &env, &text, &tx).await {
                let _ = tx.send(ChatEvent::Error(format!("{e:#}")));
            }
        });
        self.running = Some(task.abort_handle());
        // Ends when the turn ends or is stopped: both drop the sender.
        cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.recv().await {
                if this.update(cx, |p, cx| p.apply(ev, cx)).is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |p, cx| {
                p.running = None;
                cx.notify();
            });
        })
        .detach();
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self.running.take() {
            task.abort();
            self.entries.push(Entry::Error("Stopped.".into()));
            cx.notify();
        }
    }

    fn new_chat(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
        self.entries.clear();
        self.chat = Arc::new(tokio::sync::Mutex::new(Chat::new(self.session.clone())));
        cx.notify();
    }

    fn apply(&mut self, ev: ChatEvent, cx: &mut Context<Self>) {
        if !matches!(self.entries.last(), Some(Entry::Assistant(_))) {
            self.entries.push(Entry::Assistant(vec![]));
        }
        let Some(Entry::Assistant(parts)) = self.entries.last_mut() else { return };
        match ev {
            ChatEvent::Text(t) => match parts.last_mut() {
                Some(Part::Text(s)) => s.push_str(&t),
                _ => parts.push(Part::Text(t)),
            },
            ChatEvent::ToolStart { id, name, input } => parts.push(Part::Tool(ToolCall { id, name, input, output: None, open: false })),
            ChatEvent::ToolEnd { id, output, is_error } => {
                // The CLIs report a call's end without its start when they retry.
                match parts.iter_mut().rev().find_map(|p| match p {
                    Part::Tool(t) if t.id == id => Some(t),
                    _ => None,
                }) {
                    Some(t) => t.output = Some((output, is_error)),
                    None => parts.push(Part::Tool(ToolCall { id, name: "tool".into(), input: String::new(), output: Some((output, is_error)), open: false })),
                }
            }
            ChatEvent::Error(e) => self.entries.push(Entry::Error(e)),
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn toggle_tool(&mut self, entry: usize, part: usize, cx: &mut Context<Self>) {
        if let Some(Entry::Assistant(parts)) = self.entries.get_mut(entry) {
            if let Some(Part::Tool(t)) = parts.get_mut(part) {
                t.open = !t.open;
                cx.notify();
            }
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let summary = AppState::get(cx).settings.ai.summary();
        let button = |id: &'static str, name: IconName, tip: &'static str| {
            div()
                .id(id)
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .hover(|s| s.bg(p.hover))
                .child(icon(name, 14., p.icon))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx))
        };
        h_flex()
            .h(px(36.))
            .px(px(8.))
            .gap(px(6.))
            .flex_shrink_0()
            .border_b_1()
            .border_color(p.separator)
            .child(icon(IconName::Sparkles, 14., p.accent))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .child(div().text_size(UI_TEXT).font_weight(FontWeight::SEMIBOLD).child("AI Assistant"))
                    .child(div().text_size(px(10.)).text_color(p.secondary_text).truncate().child(summary)),
            )
            .child(button("ai-settings", IconName::Settings, "AI settings").on_click(|_, window, cx| crate::preferences::open_ai(window, cx)))
            .child(button("ai-new", IconName::SquarePen, "New chat").on_click(cx.listener(|p, _, _, cx| p.new_chat(cx))))
            .child(button("ai-close", IconName::X, "Close (Ctrl+L)").on_click(cx.listener(|_, _, _, cx| cx.emit(AssistantEvent::Close))))
    }

    fn render_part(&self, entry: usize, part: usize, value: &Part, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.palette().clone();
        match value {
            Part::Text(text) => {
                let actions = self.code_actions.clone();
                TextView::markdown(("ai-text", entry * 1000 + part), text.clone())
                    .selectable(true)
                    .text_size(px(13.))
                    .code_block_actions(move |block, _, _| {
                        let code = block.code().to_string();
                        let is_sql = block.lang().is_none_or(|l| l.eq_ignore_ascii_case("sql"));
                        let (open, run) = (actions.clone(), actions.clone());
                        let (open_sql, run_sql, copy) = (code.clone(), code.clone(), code);
                        h_flex()
                            .gap(px(2.))
                            .when(is_sql, |d| {
                                d.child(code_button("open", IconName::SquarePen, "Open in SQL editor", move |_, _| {
                                    let _ = open.send((open_sql.clone(), false));
                                }))
                                .child(code_button("run", IconName::Play, "Open and run", move |_, _| {
                                    let _ = run.send((run_sql.clone(), true));
                                }))
                            })
                            .child(code_button("copy", IconName::Copy, "Copy", move |_, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            }))
                    })
                    .into_any_element()
            }
            Part::Tool(t) => {
                let (status, color) = match &t.output {
                    None => (IconName::LoaderCircle, p.secondary_text),
                    Some((_, false)) => (IconName::CircleCheck, p.success),
                    Some((_, true)) => (IconName::CircleX, p.danger),
                };
                let detail = |label: &'static str, text: &str| {
                    v_flex()
                        .gap(px(2.))
                        .child(div().text_size(px(10.)).text_color(p.secondary_text).child(label))
                        .child(
                            div()
                                .id(SharedString::from(format!("ai-tool-{entry}-{part}-{label}")))
                                .max_h(px(180.))
                                .overflow_y_scroll()
                                .p(px(6.))
                                .rounded(px(4.))
                                .bg(p.window)
                                .border_1()
                                .border_color(p.border)
                                .font_family(MONO)
                                .text_size(px(11.))
                                .child(text.to_string()),
                        )
                };
                v_flex()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.window_secondary)
                    .child(
                        h_flex()
                            .id(("ai-tool", entry * 1000 + part))
                            .h(px(26.))
                            .px(px(8.))
                            .gap(px(6.))
                            .text_size(UI_TEXT)
                            .hover(|s| s.bg(p.hover))
                            .child(icon(if t.open { IconName::ChevronDown } else { IconName::ChevronRight }, 12., p.secondary_text))
                            .child(icon(IconName::Database, 12., p.icon))
                            .child(div().font_family(MONO).child(t.name.clone()))
                            .child(div().flex_1().min_w(px(0.)).truncate().text_color(p.secondary_text).child(tool_summary(&t.input)))
                            .child(icon(status, 13., color))
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_tool(entry, part, cx))),
                    )
                    .when(t.open, |d| {
                        d.child(
                            v_flex()
                                .p(px(8.))
                                .gap(px(6.))
                                .child(detail("Input", &t.input))
                                .when_some(t.output.as_ref(), |d, (out, _)| d.child(detail("Result", out))),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    fn render_entries(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.palette().clone();
        if self.entries.is_empty() {
            return v_flex()
                .flex_1()
                .p(px(12.))
                .gap(px(8.))
                .justify_center()
                .child(placeholder(format!("Ask anything about {}.", self.session.config.display_name()), cx).size_auto())
                .children(SUGGESTIONS.iter().enumerate().map(|(i, s)| {
                    div()
                        .id(("ai-suggest", i))
                        .p(px(8.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(p.border)
                        .text_size(UI_TEXT)
                        .hover(|st| st.bg(p.hover))
                        .child(*s)
                        .on_click(cx.listener(move |this, _, window, cx| this.send(SUGGESTIONS[i].to_string(), window, cx)))
                }))
                .into_any_element();
        }
        let running = self.running.is_some();
        let last = self.entries.len() - 1;
        let mut list = v_flex().p(px(10.)).gap(px(10.));
        for (i, entry) in self.entries.iter().enumerate() {
            list = list.child(match entry {
                Entry::User(text) => h_flex()
                    .justify_end()
                    .child(
                        div()
                            .max_w(px(300.))
                            .px(px(10.))
                            .py(px(6.))
                            .rounded(px(8.))
                            .bg(p.button_hover)
                            .text_size(px(13.))
                            .child(text.clone()),
                    )
                    .into_any_element(),
                Entry::Assistant(parts) => v_flex()
                    .gap(px(6.))
                    .children(parts.iter().enumerate().map(|(j, part)| self.render_part(i, j, part, cx)))
                    .when(running && i == last && parts.is_empty(), |d| {
                        d.child(div().text_size(UI_TEXT).text_color(p.secondary_text).child("Thinking…"))
                    })
                    .into_any_element(),
                Entry::Error(e) => h_flex()
                    .gap(px(6.))
                    .items_start()
                    .text_size(UI_TEXT)
                    .text_color(p.danger)
                    .child(icon(IconName::TriangleAlert, 13., p.danger))
                    .child(div().flex_1().child(e.clone()))
                    .into_any_element(),
            });
        }
        div().id("ai-log").flex_1().overflow_y_scroll().track_scroll(&self.scroll).child(list).into_any_element()
    }
}

fn code_button(id: &'static str, name: IconName, tip: &'static str, on_click: impl Fn(&mut Window, &mut App) + 'static) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .hover(|s| s.opacity(0.7))
        .child(Icon::new(name).size(px(12.)))
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx))
        .on_click(move |_, window, cx| on_click(window, cx))
}

/// One line about a tool call: its SQL or table, else its arguments.
fn tool_summary(input: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
    let text = ["sql", "table", "term"]
        .iter()
        .find_map(|k| v[k].as_str().map(str::to_string))
        .unwrap_or_else(|| input.to_string());
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Render for AssistantPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let running = self.running.is_some();
        v_flex()
            .key_context("Assistant")
            .size_full()
            .bg(p.window)
            .child(self.render_header(cx))
            .child(self.render_entries(cx))
            .child(
                v_flex()
                    .p(px(8.))
                    .gap(px(6.))
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(p.separator)
                    .child(Textarea::new(&self.input))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(div().flex_1().text_size(px(10.)).text_color(p.secondary_text).child(match AppState::get(cx).settings.ai.writes {
                                WritePolicy::Ask => "Enter sends, Shift+Enter adds a line. Changes need your approval.",
                                WritePolicy::Allow => "Enter sends, Shift+Enter adds a line. Changes run without asking.",
                                WritePolicy::ReadOnly => "Enter sends, Shift+Enter adds a line. Read only.",
                            }))
                            .child(
                                div()
                                    .id("ai-send")
                                    .size(px(26.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(13.))
                                    .bg(if running { p.danger } else { p.accent })
                                    .hover(|s| s.opacity(0.85))
                                    .child(icon(if running { IconName::Square } else { IconName::ArrowUp }, 13., white()))
                                    .tooltip(move |window, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(if running { "Stop" } else { "Send (Enter)" }).build(window, cx)
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if running {
                                            this.stop(cx);
                                        } else {
                                            let text = this.input.read(cx).value().to_string();
                                            this.send(text, window, cx);
                                        }
                                    })),
                            ),
                    ),
            )
    }
}

/// Shows a change an AI tool wants to make and sends back the user's
/// answer. Closing the dialog refuses.
pub fn ask_approval(request: Approval, cx: &mut App) {
    let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else { return };
    let Approval { connection, sql, reply } = request;
    let reply = Rc::new(RefCell::new(Some(reply)));
    let title = SharedString::from(format!("Allow the AI assistant to change \"{connection}\"?"));
    let sql = SharedString::from(sql);
    let _ = window.update(cx, |_, window, cx| {
        window.activate_window();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (reply, sql, p) = (reply.clone(), sql.clone(), cx.palette().clone());
            alert
                .title(title.clone())
                .width(px(560.))
                .content(move |content, _, _| {
                    content.child(
                        v_flex().gap(px(8.)).child("It wants to run:").child(
                            div()
                                .id("approval-sql")
                                .max_h(px(260.))
                                .overflow_y_scroll()
                                .p(px(8.))
                                .rounded(px(4.))
                                .bg(p.window_secondary)
                                .border_1()
                                .border_color(p.border)
                                .font_family(MONO)
                                .text_size(UI_TEXT)
                                .child(sql.clone()),
                        ),
                    )
                })
                .ok_text("Allow")
                .cancel_text("Deny")
                .show_cancel(true)
                .on_ok(move |_, _, _| {
                    if let Some(r) = reply.borrow_mut().take() {
                        let _ = r.send(true);
                    }
                    true
                })
        });
    });
}
