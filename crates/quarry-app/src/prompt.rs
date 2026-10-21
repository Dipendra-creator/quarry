//! Alert, confirmation and single-field input dialogs.

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::v_flex;
use gpui_kit::*;

pub fn error(title: &str, message: &str, window: &mut Window, cx: &mut App) {
    let (title, message) = (SharedString::from(title.to_string()), SharedString::from(message.to_string()));
    window.open_alert_dialog(cx, move |alert, _, _| alert.title(title.clone()).description(message.clone()));
}

pub fn info(title: &str, message: &str, window: &mut Window, cx: &mut App) {
    error(title, message, window, cx)
}

pub fn confirm(
    title: &str,
    message: &str,
    ok: &str,
    window: &mut Window,
    cx: &mut App,
    on_ok: impl Fn(&mut Window, &mut App) + 'static,
) {
    let (title, message, ok) = (
        SharedString::from(title.to_string()),
        SharedString::from(message.to_string()),
        SharedString::from(ok.to_string()),
    );
    let on_ok = Rc::new(on_ok);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let on_ok = on_ok.clone();
        alert
            .title(title.clone())
            .description(message.clone())
            .ok_text(ok.clone())
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                on_ok(window, cx);
                true
            })
    });
}

/// Asks for one line of text; `secret` masks it (passwords).
pub fn ask(
    title: &str,
    label: &str,
    default: &str,
    secret: bool,
    window: &mut Window,
    cx: &mut App,
    on_ok: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(default.to_string()).masked(secret));
    let (title, label) = (SharedString::from(title.to_string()), SharedString::from(label.to_string()));
    let on_ok = Rc::new(on_ok);
    let focus = input.read(cx).focus_handle(cx);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (on_ok, input, input2, label) = (on_ok.clone(), input.clone(), input.clone(), label.clone());
        alert
            .title(title.clone())
            .content(move |content, _, _| content.child(v_flex().gap(px(6.)).child(label.clone()).child(Input::new(&input2))))
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                let value = input.read(cx).value().to_string();
                on_ok(value, window, cx);
                true
            })
    });
    window.focus(&focus, cx);
}
