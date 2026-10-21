//! Searchable list dialog: database switcher (Ctrl+K) and Open Anything (Ctrl+P).

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme::{PaletteExt as _, UI_TEXT};
use crate::widgets::{icon, search_icon};

#[derive(Clone)]
pub struct PickerItem {
    pub label: String,
    pub detail: String,
    pub icon: IconName,
}

pub struct Picker {
    input: Entity<InputState>,
    items: Vec<PickerItem>,
    selected: usize,
    on_pick: Rc<dyn Fn(usize, &mut Window, &mut App)>,
    _sub: Subscription,
}

impl Picker {
    fn filtered(&self, cx: &App) -> Vec<usize> {
        let q = self.input.read(cx).value().to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, i)| q.is_empty() || fuzzy(&i.label.to_lowercase(), &q))
            .map(|(ix, _)| ix)
            .take(200)
            .collect()
    }

    fn pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.filtered(cx).get(self.selected).copied() {
            let f = self.on_pick.clone();
            window.close_dialog(cx);
            f(ix, window, cx);
        }
    }
}

/// Characters of `q` appear in order in `s`.
fn fuzzy(s: &str, q: &str) -> bool {
    let mut chars = s.chars();
    q.chars().all(|c| chars.any(|x| x == c))
}

pub fn open(
    title: &'static str,
    placeholder: &'static str,
    items: Vec<PickerItem>,
    window: &mut Window,
    cx: &mut App,
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
    let picker = cx.new(|cx| {
        let _sub = cx.subscribe_in(&input, window, |p: &mut Picker, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                p.selected = 0;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => p.pick(window, cx),
            _ => {}
        });
        Picker { input: input.clone(), items, selected: 0, on_pick: Rc::new(on_pick), _sub }
    });
    let focus = input.read(cx).focus_handle(cx);
    window.open_dialog(cx, move |dialog, _, _| dialog.title(title).w(px(520.)).child(picker.clone()));
    window.focus(&focus, cx);
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let rows = self.filtered(cx);
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        v_flex()
            .gap(px(8.))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| match ev.keystroke.key.as_str() {
                "down" => {
                    this.selected += 1;
                    cx.notify();
                }
                "up" => {
                    this.selected = this.selected.saturating_sub(1);
                    cx.notify();
                }
                _ => {}
            }))
            .child(Input::new(&self.input).prefix(search_icon(cx)))
            .child(
                v_flex()
                    .id("picker-list")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .children(rows.iter().enumerate().map(|(n, ix)| {
                        let item = &self.items[*ix];
                        let selected = n == self.selected;
                        h_flex()
                            .id(("pick", n))
                            .h(px(28.))
                            .px(px(8.))
                            .gap(px(8.))
                            .rounded(px(4.))
                            .text_size(UI_TEXT)
                            .when(selected, |d| d.bg(p.selection).text_color(p.selection_text))
                            .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
                            .child(icon(item.icon.clone(), 14., if selected { p.selection_text } else { p.accent }))
                            .child(div().flex_1().truncate().child(item.label.clone()))
                            .child(div().text_color(if selected { p.selection_text } else { p.secondary_text }).child(item.detail.clone()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.selected = n;
                                this.pick(window, cx);
                            }))
                    })),
            )
    }
}
