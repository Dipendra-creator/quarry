//! Keyboard Shortcuts (F1): every shortcut, grouped and searchable, and the
//! place to change them. Click a key to record a new one, + to add another,
//! × to remove one. Changes are saved to `keymap.json` and apply at once.

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::{self, BUILT_IN, GROUPS, SHORTCUTS, Shortcut, display_key};
use crate::theme::{MONO, PaletteExt as _, UI_TEXT};
use crate::widgets::{hline, icon};

/// Opens the guide in `window`, in place of any open dialog.
pub fn open(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        window.close_all_dialogs(cx);
    }
    let view = cx.new(|cx| ShortcutsView::new(window, cx));
    let focus = view.read(cx).search.read(cx).focus_handle(cx);
    window.open_dialog(cx, move |dialog, _, _| dialog.title("Keyboard Shortcuts").w(px(700.)).margin_top(px(36.)).child(view.clone()));
    window.focus(&focus, cx);
}

/// The key being recorded: a shortcut and which of its keys (`None` adds one).
#[derive(Clone, PartialEq)]
struct Recording {
    id: &'static str,
    slot: Option<usize>,
}

struct ShortcutsView {
    focus: FocusHandle,
    search: Entity<InputState>,
    recording: Option<Recording>,
    /// Outcome of the last change; `true` marks a problem.
    message: Option<(String, bool)>,
    _subs: Vec<Subscription>,
}

impl ShortcutsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search by name or key"));
        let mut subs = vec![cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })];
        // Sees keys before any binding does, so recording ⌘W records it
        // instead of closing the tab.
        let this = cx.entity().downgrade();
        let window_handle = window.window_handle();
        subs.push(cx.intercept_keystrokes(move |ev, window, cx| {
            if window.window_handle() != window_handle {
                return;
            }
            let Some(this) = this.upgrade() else { return };
            if this.read(cx).recording.is_none() {
                return;
            }
            cx.stop_propagation();
            let keystroke = ev.keystroke.clone();
            this.update(cx, |v, cx| v.recorded(&keystroke, cx));
        }));
        Self { focus: cx.focus_handle(), search, recording: None, message: None, _subs: subs }
    }

    fn record(&mut self, id: &'static str, slot: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = Some(Recording { id, slot });
        self.message = Some((hint_text(), false));
        // Off the search field, so the keys are not typed into it.
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        self.message = None;
        cx.notify();
    }

    fn recorded(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let Some(rec) = self.recording.clone() else { return };
        // A modifier on its own: wait for the key.
        if matches!(keystroke.key.as_str(), "shift" | "control" | "alt" | "platform" | "function" | "") {
            return;
        }
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            return self.cancel(cx);
        }
        let key = keystroke.unparse();
        if let Some(why) = actions::invalid_reason(&key) {
            self.message = Some((format!("{} can't be a shortcut. {why}", display_key(&key)), true));
            cx.notify();
            return;
        }
        let me = actions::shortcut(rec.id).expect("recorded shortcut exists");
        let mut notes = Vec::new();
        // The same key in the same place does one thing: take it from the other shortcut.
        for other in actions::conflicts(rec.id, &key, cx) {
            let rest = actions::keys(other.id, cx).into_iter().filter(|k| actions::normalize(k) != key).collect();
            if let Err(e) = actions::set_keys(other.id, rest, cx) {
                return self.failed(e, cx);
            }
            notes.push(format!("It was removed from {}.", other.name));
        }
        // Elsewhere both stay; say which one wins where they meet.
        for other in actions::overlaps(rec.id, &key, cx) {
            let winner = if actions::depth(other) > actions::depth(me) { other.name } else { me.name };
            notes.push(format!("{} also uses it; where both apply, {winner} wins.", other.name));
        }
        let mut keys = actions::keys(rec.id, cx);
        keys.retain(|k| actions::normalize(k) != key || rec.slot.is_some());
        match rec.slot {
            Some(i) if i < keys.len() => {
                keys[i] = key.clone();
                let mut seen = Vec::new();
                keys.retain(|k| {
                    let n = actions::normalize(k);
                    let fresh = !seen.contains(&n);
                    seen.push(n);
                    fresh
                });
            }
            _ => keys.push(key.clone()),
        }
        if let Err(e) = actions::set_keys(rec.id, keys, cx) {
            return self.failed(e, cx);
        }
        self.recording = None;
        let mut text = format!("{} is now {}.", me.name, display_key(&key));
        for n in notes {
            text.push(' ');
            text.push_str(&n);
        }
        self.message = Some((text, false));
        cx.notify();
    }

    fn remove(&mut self, id: &'static str, slot: usize, cx: &mut Context<Self>) {
        let mut keys = actions::keys(id, cx);
        if slot < keys.len() {
            keys.remove(slot);
        }
        self.recording = None;
        match actions::set_keys(id, keys, cx) {
            Ok(()) => self.message = None,
            Err(e) => return self.failed(e, cx),
        }
        cx.notify();
    }

    fn reset(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let defaults = actions::shortcut(id).map(actions::default_keys).unwrap_or_default();
        self.recording = None;
        match actions::set_keys(id, defaults, cx) {
            Ok(()) => self.message = None,
            Err(e) => return self.failed(e, cx),
        }
        cx.notify();
    }

    fn failed(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        self.recording = None;
        self.message = Some((format!("Could not save keymap.json: {e:#}"), true));
        cx.notify();
    }

    fn matches(&self, s: &Shortcut, query: &str, cx: &App) -> bool {
        if query.is_empty() {
            return true;
        }
        let keys = actions::keys(s.id, cx).iter().map(|k| display_key(k).to_lowercase()).collect::<Vec<_>>().join(" ");
        let hay = format!("{} {} {} {}", s.name, s.description, s.group, keys).to_lowercase();
        query.split_whitespace().all(|w| hay.contains(w))
    }

    fn render_row(&self, s: &'static Shortcut, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let keys = actions::keys(s.id, cx);
        let recording = self.recording.as_ref().filter(|r| r.id == s.id).map(|r| r.slot);
        let customized = actions::is_customized(s.id, cx);
        let mut chips: Vec<AnyElement> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                if recording == Some(Some(i)) {
                    return recording_chip(cx).into_any_element();
                }
                key_chip(SharedString::from(format!("key-{}-{i}", s.id)), display_key(k), cx)
                    .on_click(cx.listener(move |v, _, window, cx| v.record(s.id, Some(i), window, cx)))
                    .child(
                        div()
                            .id(SharedString::from(format!("rm-{}-{i}", s.id)))
                            .ml(px(2.))
                            .rounded(px(3.))
                            .hover(|d| d.bg(p.hover))
                            .child(icon(IconName::X, 10., p.secondary_text))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                cx.stop_propagation();
                                v.remove(s.id, i, cx);
                            }))
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Remove this key").build(window, cx)),
                    )
                    .into_any_element()
            })
            .collect();
        if recording == Some(None) {
            chips.push(recording_chip(cx).into_any_element());
        }
        if keys.is_empty() && recording.is_none() {
            chips.push(div().text_size(px(11.)).text_color(p.secondary_text).child("None").into_any_element());
        }
        h_flex()
            .id(SharedString::from(format!("row-{}", s.id)))
            .w_full()
            .px(px(8.))
            .py(px(5.))
            .gap(px(12.))
            .rounded(px(4.))
            .hover(|d| d.bg(p.window_secondary))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .text_size(UI_TEXT)
                            .child(s.name)
                            .when(customized, |d| d.child(div().text_size(px(10.)).text_color(p.accent).child("Changed"))),
                    )
                    .child(div().text_size(px(11.)).text_color(p.secondary_text).truncate().child(s.description)),
            )
            .child(h_flex().flex_wrap().justify_end().gap(px(4.)).max_w(px(330.)).children(chips))
            .child(
                h_flex()
                    .w(px(44.))
                    .flex_shrink_0()
                    .gap(px(2.))
                    .justify_end()
                    .child(
                        small_icon_button(SharedString::from(format!("add-{}", s.id)), IconName::Plus, "Add a key", cx)
                            .on_click(cx.listener(move |v, _, window, cx| v.record(s.id, None, window, cx))),
                    )
                    .when(customized, |d| {
                        d.child(
                            small_icon_button(SharedString::from(format!("reset-{}", s.id)), IconName::RotateCcw, "Back to the default", cx)
                                .on_click(cx.listener(move |v, _, _, cx| v.reset(s.id, cx))),
                        )
                    }),
            )
    }
}

impl Render for ShortcutsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let query = self.search.read(cx).value().trim().to_lowercase();
        let any_changed = SHORTCUTS.iter().any(|s| actions::is_customized(s.id, cx));
        let list_height = (window.viewport_size().height - px(236.)).max(px(180.));

        let mut list = v_flex().gap(px(2.));
        let mut shown = 0;
        for group in GROUPS {
            let rows: Vec<&'static Shortcut> = SHORTCUTS.iter().filter(|s| s.group == group && self.matches(s, &query, cx)).collect();
            if rows.is_empty() {
                continue;
            }
            shown += rows.len();
            list = list.child(group_header(group, cx));
            for s in rows {
                list = list.child(self.render_row(s, cx));
            }
        }
        let built_in: Vec<_> = BUILT_IN
            .iter()
            .filter(|(name, key)| query.is_empty() || format!("{name} {}", display_key(key)).to_lowercase().contains(&query))
            .collect();
        if !built_in.is_empty() {
            shown += built_in.len();
            list = list.child(group_header("Text fields and SQL editor (built in)", cx));
            for (name, key) in built_in {
                list = list.child(
                    h_flex()
                        .w_full()
                        .px(px(8.))
                        .py(px(4.))
                        .gap(px(12.))
                        .child(div().flex_1().text_size(UI_TEXT).child(*name))
                        .child(key_chip(SharedString::from(format!("builtin-{name}")), display_key(key), cx).cursor_default())
                        // Lines up with the + and reset buttons above.
                        .child(div().w(px(44.)).flex_shrink_0()),
                );
            }
        }
        if shown == 0 {
            list = list.child(div().py(px(24.)).w_full().text_center().text_size(UI_TEXT).text_color(p.secondary_text).child("No shortcut matches."));
        }

        let message = self.message.clone().map(|(text, error)| {
            div().text_size(px(11.)).text_color(if error { p.danger } else { p.secondary_text }).child(text)
        });
        let this = cx.entity().downgrade();
        v_flex()
            .track_focus(&self.focus)
            .pt(px(4.))
            .gap(px(10.))
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                if v.recording.is_some() {
                    v.cancel(cx);
                }
            }))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(div().flex_1().child(Input::new(&self.search).prefix(crate::widgets::search_icon(cx)).cleanable(true).small()))
                    .child(Button::new("reset-all").label("Reset all").small().outline().disabled(!any_changed).on_click(move |_, window, cx| {
                        let this = this.clone();
                        crate::prompt::confirm(
                            "Reset all shortcuts",
                            "Every shortcut goes back to Quarry's default keys.",
                            "Reset all",
                            window,
                            cx,
                            move |_, cx| {
                                let r = actions::reset_all(cx);
                                let _ = this.update(cx, |v, cx| match r {
                                    Ok(()) => {
                                        v.recording = None;
                                        v.message = Some(("Every shortcut is back to its default.".into(), false));
                                        cx.notify();
                                    }
                                    Err(e) => v.failed(e, cx),
                                });
                            },
                        );
                    })),
            )
            .child(
                div()
                    .min_h(px(16.))
                    .child(message.unwrap_or_else(|| {
                        div().text_size(px(11.)).text_color(p.secondary_text).child("Click a key to change it, + to add one, × to remove it.")
                    })),
            )
            .child(
                div()
                    .id("shortcut-list")
                    .h(list_height)
                    .overflow_y_scroll()
                    .border_1()
                    .border_color(p.border)
                    .rounded(px(6.))
                    .p(px(4.))
                    .child(list),
            )
            .child(hline(cx))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(11.))
                            .text_color(p.secondary_text)
                            .child("Your changes are saved in keymap.json in Quarry's settings folder."),
                    )
                    .child(Button::new("reveal-keymap").label("Show file").small().ghost().on_click(|_, _, cx| {
                        let path = actions::keymap_file();
                        if path.exists() {
                            cx.reveal_path(&path);
                        } else if let Some(dir) = path.parent() {
                            let _ = std::fs::create_dir_all(dir);
                            cx.reveal_path(dir);
                        }
                    })),
            )
    }
}

fn hint_text() -> String {
    "Press the new shortcut, or Esc to cancel.".into()
}

fn group_header(title: &str, cx: &App) -> Div {
    let p = cx.palette();
    div()
        .px(px(8.))
        .pt(px(10.))
        .pb(px(2.))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.secondary_text)
        .child(title.to_uppercase())
}

fn key_chip(id: SharedString, text: String, cx: &App) -> Stateful<Div> {
    let p = cx.palette();
    h_flex()
        .id(id)
        .h(px(22.))
        .px(px(6.))
        .gap(px(2.))
        .rounded(px(4.))
        .border_1()
        .border_color(p.button_border)
        .bg(p.button)
        .font_family(MONO)
        .text_size(px(11.))
        .text_color(p.text)
        .cursor_pointer()
        .hover(|d| d.bg(p.button_hover))
        .child(text)
}

fn recording_chip(cx: &App) -> Div {
    let p = cx.palette();
    h_flex()
        .h(px(22.))
        .px(px(8.))
        .rounded(px(4.))
        .border_1()
        .border_color(p.accent)
        .text_size(px(11.))
        .text_color(p.accent)
        .child("Press keys…")
}

fn small_icon_button(id: SharedString, name: IconName, tooltip: &'static str, cx: &App) -> Stateful<Div> {
    let p = cx.palette();
    div()
        .id(id)
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .hover(|d| d.bg(p.hover))
        .child(icon(name, 12., p.icon))
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx))
}
