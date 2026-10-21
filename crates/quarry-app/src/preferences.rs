//! Preferences dialog (Ctrl+,). General: appearance and SQL editor row
//! limit. AI: the assistant's backend (Claude Code, Codex, Anthropic API,
//! OpenAI API) and what AI tools may change.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_ai::WritePolicy;
use quarry_ai::assistant::{AiSettings, Provider};
use quarry_core::store::{delete_password, load_password, store_password};

use crate::state::{AppState, Appearance, db, mcp_exe};
use crate::theme::{MONO, PaletteExt as _, UI_TEXT};
use crate::widgets::{hline, segmented};

pub fn open(window: &mut Window, cx: &mut App) {
    show(0, window, cx);
}

/// Opens Preferences on the AI tab.
pub fn open_ai(window: &mut Window, cx: &mut App) {
    show(1, window, cx);
}

fn show(tab: usize, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| Preferences::new(tab, window, cx));
    window.open_dialog(cx, move |dialog, _, _| dialog.title("Preferences").w(px(640.)).child(view.clone()));
}

/// The text fields of the AI tab.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    ClaudePath,
    ClaudeModel,
    CodexPath,
    CodexModel,
    AnthropicKey,
    AnthropicModel,
    AnthropicUrl,
    OpenAiKey,
    OpenAiModel,
    OpenAiUrl,
}

impl Field {
    const ALL: [Field; 10] = [
        Field::ClaudePath,
        Field::ClaudeModel,
        Field::CodexPath,
        Field::CodexModel,
        Field::AnthropicKey,
        Field::AnthropicModel,
        Field::AnthropicUrl,
        Field::OpenAiKey,
        Field::OpenAiModel,
        Field::OpenAiUrl,
    ];

    fn provider(self) -> Provider {
        match self {
            Field::ClaudePath | Field::ClaudeModel => Provider::ClaudeCode,
            Field::CodexPath | Field::CodexModel => Provider::Codex,
            Field::AnthropicKey | Field::AnthropicModel | Field::AnthropicUrl => Provider::Anthropic,
            Field::OpenAiKey | Field::OpenAiModel | Field::OpenAiUrl => Provider::OpenAi,
        }
    }

    fn is_key(self) -> bool {
        matches!(self, Field::AnthropicKey | Field::OpenAiKey)
    }

    fn label(self) -> &'static str {
        match self {
            Field::ClaudePath | Field::CodexPath => "Executable",
            Field::ClaudeModel | Field::CodexModel | Field::AnthropicModel | Field::OpenAiModel => "Model",
            Field::AnthropicKey | Field::OpenAiKey => "API key",
            Field::AnthropicUrl | Field::OpenAiUrl => "Endpoint",
        }
    }

    fn placeholder(self) -> String {
        match self {
            Field::ClaudePath => "claude, found on PATH".into(),
            Field::CodexPath => "codex, found on PATH".into(),
            Field::ClaudeModel => "Claude Code's default (or opus, sonnet, a model id)".into(),
            Field::CodexModel => "Codex's default".into(),
            Field::AnthropicKey => "sk-ant-… (or set ANTHROPIC_API_KEY)".into(),
            Field::OpenAiKey => "sk-… (or set OPENAI_API_KEY)".into(),
            Field::AnthropicModel | Field::OpenAiModel => self.provider().default_model().into(),
            Field::AnthropicUrl | Field::OpenAiUrl => self.provider().default_url().into(),
        }
    }

    /// Current value; keys come from the keychain.
    fn get(self, s: &AiSettings) -> String {
        match self {
            Field::ClaudePath => s.claude_path.clone(),
            Field::ClaudeModel => s.claude_model.clone(),
            Field::CodexPath => s.codex_path.clone(),
            Field::CodexModel => s.codex_model.clone(),
            Field::AnthropicModel => s.anthropic_model.clone(),
            Field::AnthropicUrl => s.anthropic_url.clone(),
            Field::OpenAiModel => s.openai_model.clone(),
            Field::OpenAiUrl => s.openai_url.clone(),
            Field::AnthropicKey | Field::OpenAiKey => load_password(self.provider().key_id()).unwrap_or_default(),
        }
    }

    fn set(self, s: &mut AiSettings, value: String) {
        let slot = match self {
            Field::ClaudePath => &mut s.claude_path,
            Field::ClaudeModel => &mut s.claude_model,
            Field::CodexPath => &mut s.codex_path,
            Field::CodexModel => &mut s.codex_model,
            Field::AnthropicModel => &mut s.anthropic_model,
            Field::AnthropicUrl => &mut s.anthropic_url,
            Field::OpenAiModel => &mut s.openai_model,
            Field::OpenAiUrl => &mut s.openai_url,
            Field::AnthropicKey | Field::OpenAiKey => return,
        };
        *slot = value.trim().to_string();
    }
}

struct Preferences {
    tab: usize,
    fields: Vec<(Field, Entity<InputState>)>,
    /// Outcome of the last "Test" click.
    test: Option<Result<String, String>>,
    testing: bool,
    _subs: Vec<Subscription>,
}

impl Preferences {
    fn new(tab: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ai = AppState::get(cx).settings.ai.clone();
        let mut fields = vec![];
        let mut subs = vec![];
        for f in Field::ALL {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(f.placeholder()).default_value(f.get(&ai)).masked(f.is_key()));
            subs.push(cx.subscribe(&input, move |this, input, ev: &InputEvent, cx| {
                let value = input.read(cx).value().to_string();
                match ev {
                    // Keys go to the keychain once typing is done.
                    InputEvent::Blur | InputEvent::PressEnter { .. } if f.is_key() => {
                        let id = f.provider().key_id();
                        let _ = if value.trim().is_empty() {
                            delete_password(id);
                            Ok(())
                        } else {
                            store_password(id, value.trim())
                        };
                    }
                    InputEvent::Change if !f.is_key() => update_ai(cx, |s| f.set(s, value)),
                    _ => return,
                }
                this.test = None;
                cx.notify();
            }));
            fields.push((f, input));
        }
        Self { tab, fields, test: None, testing: false, _subs: subs }
    }

    fn run_test(&mut self, cx: &mut Context<Self>) {
        // Keys are saved on blur; the Test click may come first.
        for (f, input) in &self.fields {
            if f.is_key() && f.provider() == AppState::get(cx).settings.ai.provider {
                let value = input.read(cx).value().trim().to_string();
                if !value.is_empty() {
                    let _ = store_password(f.provider().key_id(), &value);
                }
            }
        }
        let settings = AppState::get(cx).settings.ai.clone();
        self.testing = true;
        self.test = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = db(async move { quarry_ai::assistant::test(&settings).await }).await;
            let _ = this.update(cx, |p, cx| {
                p.testing = false;
                p.test = Some(r.map_err(|e| format!("{e:#}")));
                cx.notify();
            });
        })
        .detach();
    }

    fn render_general(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = AppState::get(cx).settings.clone();
        let limit_ix = match s.query_limit {
            Some(100) => 0,
            Some(300) => 1,
            Some(1000) => 2,
            Some(5000) => 3,
            _ => 4,
        };
        v_flex()
            .gap(px(14.))
            .child(row(
                "Appearance",
                segmented("appearance", &["Light", "Dark"], (s.appearance == Appearance::Dark) as usize, 70., cx, |i, window, cx| {
                    let appearance = if i == 1 { Appearance::Dark } else { Appearance::Light };
                    cx.update_global::<AppState, _>(|st, _| {
                        st.settings.appearance = appearance;
                        st.settings.save();
                    });
                    crate::theme::apply(i == 1, Some(window), cx);
                }),
            ))
            .child(row(
                "SQL editor row limit",
                segmented("limit", &["100", "300", "1000", "5000", "None"], limit_ix, 50., cx, |i, window, cx| {
                    let limit = [Some(100), Some(300), Some(1000), Some(5000), None][i];
                    cx.update_global::<AppState, _>(|st, _| {
                        st.settings.query_limit = limit;
                        st.settings.save();
                    });
                    window.refresh();
                }),
            ))
            .into_any_element()
    }

    fn render_ai(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.palette().clone();
        let ai = AppState::get(cx).settings.ai.clone();
        let provider_ix = Provider::ALL.iter().position(|x| *x == ai.provider).unwrap_or(0);
        let this = cx.entity().downgrade();
        let about = match ai.provider {
            Provider::ClaudeCode => "Runs the Claude Code CLI you are signed in to. No API key needed.",
            Provider::Codex => "Runs the OpenAI Codex CLI you are signed in to. No API key needed.",
            Provider::Anthropic => "Calls the Anthropic API with your API key.",
            Provider::OpenAi => "Calls the OpenAI API, or any server with an OpenAI-compatible API at the endpoint you set.",
        };
        let policy_ix = match ai.writes {
            WritePolicy::Ask => 0,
            WritePolicy::Allow => 1,
            WritePolicy::ReadOnly => 2,
        };
        let policy_about = match ai.writes {
            WritePolicy::Ask => "AI tools read freely. Every change to data or structure waits for your approval.",
            WritePolicy::Allow => "Changes run without asking, except on connections in a safe mode, which still ask.",
            WritePolicy::ReadOnly => "AI tools can only read. Every change is refused.",
        };
        let mcp_command = format!("claude mcp add quarry -- \"{}\"", mcp_exe().display());
        // Full width, so long hints wrap instead of widening the dialog.
        let hint = |text: &'static str| div().w_full().text_size(px(11.)).text_color(p.secondary_text).child(text);
        v_flex()
            .gap(px(12.))
            .child(row(
                "Assistant",
                segmented("ai-provider", &["Claude Code", "Codex", "Anthropic API", "OpenAI API"], provider_ix, 105., cx, move |i, _, cx| {
                    update_ai(cx, |s| s.provider = Provider::ALL[i]);
                    let _ = this.update(cx, |p, cx| {
                        p.test = None;
                        cx.notify();
                    });
                }),
            ))
            .child(row("", hint(about)))
            .children(self.fields.iter().filter(|(f, _)| f.provider() == ai.provider).map(|(f, input)| {
                row(f.label(), div().flex_1().child(Input::new(input).small().when(f.is_key(), |i| i.mask_toggle())))
            }))
            .when(ai.provider.is_cli(), |d| {
                d.child(row("", hint("The CLI gets Quarry's tools through quarry-mcp, and nothing else: no shell, no files.")))
            })
            .child(row(
                "",
                h_flex()
                    .gap(px(10.))
                    .child(
                        Button::new("ai-test")
                            .label("Test")
                            .small()
                            .outline()
                            .loading(self.testing)
                            .disabled(self.testing)
                            .on_click(cx.listener(|p, _, _, cx| p.run_test(cx))),
                    )
                    .when_some(self.test.clone(), |d, r| {
                        let (text, color) = match r {
                            Ok(t) => (t, p.success),
                            Err(e) => (e, p.danger),
                        };
                        d.child(div().flex_1().min_w(px(0.)).text_size(px(11.)).text_color(color).child(text))
                    }),
            ))
            .child(hline(cx))
            .child(row(
                "AI changes",
                segmented("ai-writes", &["Ask first", "Allow", "Read only"], policy_ix, 90., cx, |i, _, cx| {
                    let policy = [WritePolicy::Ask, WritePolicy::Allow, WritePolicy::ReadOnly][i];
                    update_ai(cx, |s| s.writes = policy);
                    AppState::get(cx).gate.set_policy(policy);
                }),
            ))
            .child(row("", hint(policy_about)))
            .child(hline(cx))
            .child(row(
                "Other AI tools",
                v_flex()
                    .w_full()
                    .gap(px(6.))
                    .child(hint(
                        "Any MCP client (Claude Code, Codex, Cursor, …) can use Quarry's tools. While Quarry is open, they work on its open connections and their changes wait for your approval here.",
                    ))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .p(px(6.))
                                    .rounded(px(4.))
                                    .bg(p.window_secondary)
                                    .border_1()
                                    .border_color(p.border)
                                    .font_family(MONO)
                                    .text_size(px(11.))
                                    .child(mcp_command.clone()),
                            )
                            .child(Button::new("ai-copy-mcp").label("Copy").small().outline().on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(mcp_command.clone()));
                            })),
                    ),
            ))
            .into_any_element()
    }
}

impl Render for Preferences {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let body = if self.tab == 1 { self.render_ai(cx) } else { self.render_general(cx) };
        v_flex()
            .gap(px(16.))
            .child(h_flex().justify_center().child(segmented("prefs-tab", &["General", "AI"], self.tab, 90., cx, move |i, _, cx| {
                let _ = this.update(cx, |p, cx| {
                    p.tab = i;
                    cx.notify();
                });
            })))
            .child(body)
    }
}

fn row(label: &'static str, control: impl IntoElement) -> Div {
    h_flex()
        .gap(px(12.))
        .items_start()
        .child(div().w(px(130.)).flex_shrink_0().pt(px(4.)).text_size(UI_TEXT).child(label))
        .child(div().flex_1().min_w(px(0.)).flex().child(control))
}

fn update_ai(cx: &mut App, f: impl FnOnce(&mut AiSettings)) {
    cx.update_global::<AppState, _>(|st, _| {
        f(&mut st.settings.ai);
        st.settings.save();
    });
}
