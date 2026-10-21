//! Right pane: every field of the selected row, editable for tables
//! (RowDetailController).

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Sizable as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::Value;

use crate::data_view::{DataView, RowSnapshot};
use crate::theme::{PaletteExt as _, UI_TEXT};
use crate::widgets::{placeholder, search_icon};

struct Field {
    col: usize,
    name: String,
    type_name: String,
    original: Value,
    input: Entity<InputState>,
}

pub struct RowDetail {
    snapshot: Option<RowSnapshot>,
    source: Option<WeakEntity<DataView>>,
    fields: Vec<Field>,
    search: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl RowDetail {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search for field..."));
        let sub = cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify());
        Self { snapshot: None, source: None, fields: vec![], search, _subs: vec![sub] }
    }

    pub fn set_row(&mut self, snapshot: Option<RowSnapshot>, source: Option<WeakEntity<DataView>>, window: &mut Window, cx: &mut Context<Self>) {
        self._subs.truncate(1);
        self.fields.clear();
        if let Some(s) = &snapshot {
            for (col, (c, v)) in s.columns.iter().zip(&s.values).enumerate() {
                let text = v.as_edit_text().unwrap_or_default();
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(if v.is_null() { "NULL" } else { "" }).default_value(text));
                if s.editable {
                    self._subs.push(cx.subscribe_in(&input, window, move |this, input, ev: &InputEvent, _, cx| {
                        if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                            this.commit_field(col, input.read(cx).value().to_string(), cx);
                        }
                    }));
                }
                self.fields.push(Field { col, name: c.name.clone(), type_name: c.type_name.clone(), original: v.clone(), input });
            }
        }
        self.snapshot = snapshot;
        self.source = source;
        cx.notify();
    }

    fn commit_field(&mut self, col: usize, text: String, cx: &mut Context<Self>) {
        let (Some(snap), Some(source)) = (&self.snapshot, &self.source) else { return };
        let Some(field) = self.fields.iter().find(|f| f.col == col) else { return };
        let value = match &field.original {
            Value::Null if text.is_empty() => return,
            v if v.display() == text => return,
            _ => Value::Text(text),
        };
        let row = snap.row;
        let source = source.clone();
        cx.defer(move |cx| {
            let _ = source.update(cx, |dv, cx| dv.set_cell(crate::data_view::Grid::Data, row, col, value, cx));
        });
    }
}

impl Render for RowDetail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        if self.snapshot.is_none() {
            return v_flex().size_full().bg(p.window_secondary).child(placeholder("No row selected", cx));
        }
        let q = self.search.read(cx).value().to_lowercase();
        let editable = self.snapshot.as_ref().is_some_and(|s| s.editable);
        v_flex()
            .size_full()
            .bg(p.window_secondary)
            .child(div().p(px(8.)).child(Input::new(&self.search).prefix(search_icon(cx)).cleanable(true).xsmall()))
            .child(
                v_flex()
                    .id("fields")
                    .flex_1()
                    .px(px(8.))
                    .pb(px(8.))
                    .gap(px(8.))
                    .overflow_y_scroll()
                    .children(self.fields.iter().filter(|f| q.is_empty() || f.name.to_lowercase().contains(&q)).map(|f| {
                        v_flex()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.))
                                    .text_size(UI_TEXT)
                                    .child(div().text_color(p.text).font_weight(FontWeight::MEDIUM).child(f.name.clone()))
                                    .when(!f.type_name.is_empty(), |d| d.child(div().text_color(p.secondary_text).child(f.type_name.clone()))),
                            )
                            .child(Input::new(&f.input).xsmall().disabled(!editable))
                    })),
            )
    }
}
