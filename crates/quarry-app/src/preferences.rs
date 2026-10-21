//! Preferences dialog (Ctrl+,): appearance and SQL editor row limit.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use crate::state::{AppState, Appearance};
use crate::theme::UI_TEXT;
use crate::widgets::segmented;

pub fn open(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let s = AppState::get(cx).settings.clone();
        let limit_ix = match s.query_limit {
            Some(100) => 0,
            Some(300) => 1,
            Some(1000) => 2,
            Some(5000) => 3,
            _ => 4,
        };
        dialog.title("Preferences").w(px(460.)).child(
            v_flex()
                .gap(px(14.))
                .child(
                    h_flex()
                        .gap(px(12.))
                        .child(div().w(px(130.)).text_size(UI_TEXT).child("Appearance"))
                        .child(segmented("appearance", &["Light", "Dark"], (s.appearance == Appearance::Dark) as usize, 70., cx, |i, window, cx| {
                            let appearance = if i == 1 { Appearance::Dark } else { Appearance::Light };
                            cx.update_global::<AppState, _>(|st, _| {
                                st.settings.appearance = appearance;
                                st.settings.save();
                            });
                            crate::theme::apply(i == 1, Some(window), cx);
                        })),
                )
                .child(
                    h_flex()
                        .gap(px(12.))
                        .child(div().w(px(130.)).text_size(UI_TEXT).child("SQL editor row limit"))
                        .child(segmented("limit", &["100", "300", "1000", "5000", "None"], limit_ix, 50., cx, |i, window, cx| {
                            let limit = [Some(100), Some(300), Some(1000), Some(5000), None][i];
                            cx.update_global::<AppState, _>(|st, _| {
                                st.settings.query_limit = limit;
                                st.settings.save();
                            });
                            window.refresh();
                        })),
                ),
        )
    });
}
