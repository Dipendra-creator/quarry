//! Right pane: every field of the selected row, editable for tables
//! (RowDetailController). Fields are multi-line and grow with their value,
//! so long text, JSON and line breaks are shown whole.

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::Value;

use crate::data_view::{DataView, RowSnapshot};
use crate::theme::{PaletteExt as _, UI_TEXT};
use crate::widgets::{placeholder, search_icon};

/// Tallest a field grows before it scrolls.
const MAX_ROWS: usize = 12;

struct Field {
    col: usize,
    name: String,
    type_name: String,
    original: Value,
    input: Entity<TextareaState>,
}

pub struct RowDetail {
    snapshot: Option<RowSnapshot>,
    source: Option<WeakEntity<DataView>>,
    fields: Vec<Field>,
    search: Entity<InputState>,
    _subs: Vec<Subscription>,
}

/// Rows a field needs for `text` in the default pane width: its lines, with
/// long lines counted as wrapped, capped at [`MAX_ROWS`] (longer values scroll).
fn rows_for(text: &str) -> usize {
    const CHARS_PER_ROW: usize = 40;
    text.lines().map(|l| l.chars().count().div_ceil(CHARS_PER_ROW).max(1)).sum::<usize>().clamp(1, MAX_ROWS)
}

/// Text a field starts with: the value as is, JSON indented when it is read-only.
fn field_text(v: &Value, type_name: &str, editable: bool) -> String {
    let text = v.as_edit_text().unwrap_or_default();
    let looks_json = type_name.to_ascii_lowercase().contains("json") || text.trim_start().starts_with(['{', '[']);
    if !editable && looks_json {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            if json.is_object() || json.is_array() {
                return serde_json::to_string_pretty(&json).unwrap_or(text);
            }
        }
    }
    text
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
                let text = field_text(v, &c.type_name, s.editable);
                let rows = rows_for(&text);
                let input = cx.new(|cx| {
                    TextareaState::new(window, cx).rows(rows).placeholder(if v.is_null() { "NULL" } else { "" }).default_value(text)
                });
                if s.editable {
                    // Enter makes a new line in a multi-line field; leaving it applies the edit.
                    self._subs.push(cx.subscribe_in(&input, window, move |this, input, ev: &InputEvent, _, cx| {
                        if matches!(ev, InputEvent::Blur) {
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
                    .gap(px(10.))
                    .overflow_y_scroll()
                    .children(self.fields.iter().filter(|f| q.is_empty() || f.name.to_lowercase().contains(&q)).map(|f| {
                        let null = f.original.is_null() && f.input.read(cx).value().is_empty();
                        v_flex()
                            .gap(px(3.))
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .text_size(UI_TEXT)
                                    .child(div().min_w(px(0.)).truncate().text_color(p.text).font_weight(FontWeight::MEDIUM).child(f.name.clone()))
                                    .when(!f.type_name.is_empty(), |d| d.child(div().flex_shrink_0().text_color(p.secondary_text).child(f.type_name.clone())))
                                    .when(null, |d| d.child(div().ml_auto().text_size(px(10.)).italic().text_color(p.null_value).child("NULL"))),
                            )
                            // Read-only fields stay selectable so values can be copied.
                            .child(Textarea::new(&f.input).xsmall().readonly(!editable))
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn json_is_indented_only_when_read_only() {
        let v = Value::Text(r#"{"a":[1,2]}"#.into());
        assert_eq!(field_text(&v, "jsonb", false), "{\n  \"a\": [\n    1,\n    2\n  ]\n}");
        assert_eq!(field_text(&v, "jsonb", true), r#"{"a":[1,2]}"#);
        assert_eq!(field_text(&Value::Text("{not json".into()), "text", false), "{not json");
        assert_eq!(field_text(&Value::Null, "text", false), "");
    }

    #[::core::prelude::v1::test]
    fn rows_follow_lines_and_length() {
        assert_eq!(rows_for(""), 1);
        assert_eq!(rows_for("short"), 1);
        assert_eq!(rows_for("one\ntwo"), 2);
        assert_eq!(rows_for(&"x".repeat(100)), 3);
        assert_eq!(rows_for(&"line\n".repeat(50)), MAX_ROWS);
    }
}
