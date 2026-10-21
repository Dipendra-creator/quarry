//! Small elements with the original app's look: title-bar capsules,
//! segmented controls, footer buttons, pill tabs and driver badges.

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::DriverKind;

use crate::theme::{PaletteExt as _, UI_TEXT};

/// Double click, judged by the press: Windows reports 1 on the release.
pub fn is_double(ev: &ClickEvent) -> bool {
    match ev {
        ClickEvent::Mouse(m) => m.down.click_count >= 2 || m.up.click_count >= 2,
        other => other.click_count() >= 2,
    }
}

pub fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

/// Rounded group of title-bar buttons (TitleBarCapsuleGroupStyle).
pub fn capsule(cx: &App) -> Div {
    let p = cx.palette();
    h_flex()
        // Keep presses from reaching the title bar, which turns them into window drags.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .h(px(38.))
        .p(px(4.))
        .gap(px(4.))
        .ml(px(8.))
        .rounded(px(19.))
        .border_1()
        .border_color(p.button_border)
        .bg(p.title_bar)
        .flex_shrink_0()
}

pub fn capsule_separator(cx: &App) -> Div {
    div().w(px(1.)).h(px(16.)).mx(px(2.)).bg(cx.palette().button_border)
}

/// 28px round icon button inside a capsule.
pub fn capsule_button(
    id: impl Into<ElementId>,
    icon_name: IconName,
    tooltip: &'static str,
    enabled: bool,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let p = cx.palette();
    div()
        .id(id)
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(14.))
        .child(icon(icon_name, 15., p.icon))
        .when(!enabled, |d| d.opacity(0.35))
        .when(enabled, |d| d.hover(|s| s.bg(p.capsule_hover)).on_click(on_click))
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx))
}

/// Same as [`capsule_button`] with text instead of an icon ("SQL").
pub fn capsule_text_button(
    id: impl Into<ElementId>,
    label: &'static str,
    tooltip: &'static str,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let p = cx.palette();
    div()
        .id(id)
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(14.))
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.icon)
        .child(label)
        .hover(|s| s.bg(p.capsule_hover))
        .on_click(on_click)
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx))
}

/// Bordered 24px button used in footers and forms (FooterBarButtonStyle).
pub fn footer_button(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let p = cx.palette();
    h_flex()
        .id(id)
        .h(px(24.))
        .px(px(8.))
        .gap(px(5.))
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .border_color(p.button_border)
        .bg(p.button)
        .text_size(UI_TEXT)
        .text_color(p.text)
        .flex_shrink_0()
        .hover(|s| s.bg(p.button_hover))
        .active(|s| s.opacity(0.85))
}

/// Segmented control: Data | Structure | Index, Data | Message, ….
pub fn segmented(
    id: &'static str,
    labels: &[&'static str],
    selected: usize,
    width: f32,
    cx: &App,
    on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Div {
    let p = cx.palette().clone();
    let on_select = std::rc::Rc::new(on_select);
    let n = labels.len();
    let mut row = h_flex()
        .h(px(24.))
        .rounded(px(4.))
        .border_1()
        .border_color(p.button_border)
        .bg(p.button)
        .overflow_hidden()
        .flex_shrink_0();
    for (i, label) in labels.iter().enumerate() {
        if i > 0 {
            row = row.child(div().w(px(1.)).h_full().bg(p.button_border));
        }
        let cb = on_select.clone();
        row = row.child(
            div()
                .id((id, i))
                .w(px(width))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(UI_TEXT)
                .text_color(p.text)
                .when(i == selected, |d| d.bg(p.segment_selected))
                .when(i != selected, |d| d.hover(|s| s.bg(p.button_hover)))
                .when(i == 0, |d| d.rounded_l(px(3.)))
                .when(i + 1 == n, |d| d.rounded_r(px(3.)))
                .child(*label)
                .on_click(move |_, window, cx| cb(i, window, cx)),
        );
    }
    row
}

/// "Items | Queries | History" style switch: flat buttons, selected filled.
pub fn flat_switch(
    id: &'static str,
    labels: &[&'static str],
    selected: usize,
    cx: &App,
    on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Div {
    let p = cx.palette().clone();
    let on_select = std::rc::Rc::new(on_select);
    let selected_bg = if p.dark { p.button } else { rgb(0xE0E0E0).into() };
    let idle_fg: Hsla = if p.dark { p.secondary_text } else { rgb(0x424242).into() };
    h_flex().gap(px(8.)).justify_center().children(labels.iter().enumerate().map(|(i, label)| {
        let cb = on_select.clone();
        div()
            .id((id, i))
            .w(px(65.))
            .h(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .text_size(UI_TEXT)
            .text_color(if i == selected { p.text } else { idle_fg })
            .when(i == selected, |d| d.bg(selected_bg))
            .hover(|s| s.bg(selected_bg).text_color(p.text))
            .child(*label)
            .on_click(move |_, window, cx| cb(i, window, cx))
    }))
}

/// Rounded square standing in for a vendor logo.
pub fn driver_badge(driver: DriverKind, size: f32, cx: &App) -> Div {
    div()
        .size(px(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size * 0.22))
        .bg(cx.palette().driver(driver))
        .text_color(white())
        .font_weight(FontWeight::BOLD)
        .text_size(px(size * 0.38))
        .child(driver.badge())
}

/// Search box chrome around an input (magnifier on the left).
pub fn search_icon(cx: &App) -> Icon {
    icon(IconName::Search, 12., cx.palette().secondary_text)
}

/// Thin separator line.
pub fn hline(cx: &App) -> Div {
    div().h(px(1.)).w_full().bg(cx.palette().separator).flex_shrink_0()
}

pub fn vline(cx: &App) -> Div {
    div().w(px(1.)).h_full().bg(cx.palette().separator).flex_shrink_0()
}

/// Centered gray hint text ("No item", "No row selected").
pub fn placeholder(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(16.))
        .text_center()
        .text_size(UI_TEXT)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.palette().secondary_text)
        // Bounded, so long hints wrap in narrow panes.
        .child(div().max_w_full().child(text.into()))
}
