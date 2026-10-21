//! Running server sessions (Ctrl+.), with cancel and kill (ProcessingListWindow).

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use quarry_core::{Session, Value};

use crate::grid::GridDelegate;
use crate::state::db;
use crate::theme::{PaletteExt as _, UI_TEXT};
use crate::{guard, prompt};

pub struct ProcessList {
    session: Arc<Session>,
    table: Entity<TableState<GridDelegate>>,
    status: String,
}

pub fn open(session: Arc<Session>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let table = cx.new(|cx| TableState::new(GridDelegate::new(), window, cx).row_selectable(true));
        let mut v = ProcessList { session, table, status: "Loading...".into() };
        v.refresh(window, cx);
        v
    });
    window.open_dialog(cx, move |dialog, _, _| dialog.title("Processes").w(px(900.)).child(view.clone()));
}

impl ProcessList {
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = db(async move { session.processes().await }).await;
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(result) => {
                        this.status = result.summary();
                        this.table.update(cx, |t, cx| {
                            t.delegate_mut().set_data(result.columns, result.rows);
                            t.refresh(cx);
                        });
                    }
                    Err(e) => this.status = format!("{e:#}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn selected_id(&self, cx: &App) -> Option<i64> {
        let t = self.table.read(cx);
        let row = t.selected_row().or(t.selected_cell().map(|(r, _)| r))?;
        match t.delegate().rows.get(row)?.value(0) {
            Value::Int(i) => Some(*i),
            v => v.display().parse().ok(),
        }
    }

    fn kill(&mut self, cancel_only: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected_id(cx) else {
            return prompt::info("Processes", "Select a process first.", window, cx);
        };
        let session = self.session.clone();
        let this = cx.entity().downgrade();
        let what = if cancel_only { "cancel the running query of" } else { "kill" };
        guard::guarded(&self.session.clone(), &format!("-- {what} process {id}\nKILL {id}"), window, cx, move |window, cx| {
            let _ = this.update(cx, |_, cx| {
                cx.spawn_in(window, async move |this, cx| {
                    let r = db(async move { session.kill(id, cancel_only).await }).await;
                    let _ = this.update_in(cx, |this, window, cx| match r {
                        Ok(()) => this.refresh(window, cx),
                        Err(e) => prompt::error("Failed", &format!("{e:#}"), window, cx),
                    });
                })
                .detach();
            });
        });
    }
}

impl Render for ProcessList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        v_flex()
            .gap(px(8.))
            .child(div().h(px(380.)).child(DataTable::new(&self.table).stripe(true).xsmall()))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(div().flex_1().text_size(UI_TEXT).text_color(p.secondary_text).child(self.status.clone()))
                    .child(Button::new("p-refresh").label("Refresh").small().on_click(cx.listener(|v, _, window, cx| v.refresh(window, cx))))
                    .child(Button::new("p-cancel").label("Cancel Query").small().on_click(cx.listener(|v, _, window, cx| v.kill(true, window, cx))))
                    .child(Button::new("p-kill").label("Kill Process").small().danger().on_click(cx.listener(|v, _, window, cx| v.kill(false, window, cx)))),
            )
    }
}
