//! The document view of MongoDB collections and Redis keys: documents as
//! pretty-printed, colored JSON instead of table rows. MongoDB shows a page
//! of documents with a filter and a sort; Redis shows a key's value (a hash
//! as an object, a list as an array, …) with its type, TTL and size. Both
//! are edited as JSON, through statements the preview shows first.

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::query::{DocumentPage, DocumentRequest};
use quarry_core::{DriverKind, Item, Session};

use crate::state::{AppState, Appearance, db};
use crate::theme::{MONO, PaletteExt as _, UI_TEXT};
use crate::widgets::{footer_button, icon, placeholder, segmented};
use crate::{guard, prompt};

/// Documents per page.
const PAGE: u64 = 20;
/// Longest text shown in a card; the editor shows all of it.
const SHOWN_CHARS: usize = 60_000;

pub enum DocumentViewEvent {
    Log(String),
    OpenSql(String),
    /// Show this item in the table grid instead.
    ShowTable,
}

pub struct DocumentView {
    session: Arc<Session>,
    pub item: Item,
    focus: FocusHandle,
    filter: Entity<InputState>,
    sort: Entity<InputState>,
    page: Option<DocumentPage>,
    skip: u64,
    loading: bool,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DocumentViewEvent> for DocumentView {}

impl DocumentView {
    pub fn new(session: Arc<Session>, item: Item, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter, e.g. { age: { $gt: 30 } }"));
        let sort = cx.new(|cx| InputState::new(window, cx).placeholder("Sort, e.g. { name: 1 }"));
        let subs = [&filter, &sort]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |v: &mut Self, _, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. }) {
                        v.skip = 0;
                        v.reload(window, cx);
                    }
                })
            })
            .collect();
        let mut v = Self { session, item, focus: cx.focus_handle(), filter, sort, page: None, skip: 0, loading: false, error: None, _subs: subs };
        v.reload(window, cx);
        v
    }

    pub fn title(&self) -> String {
        self.item.name().to_string()
    }

    fn is_mongo(&self) -> bool {
        self.session.kind() == DriverKind::MongoDb
    }

    /// Whether documents can be changed here: full access, and not a view.
    fn editable(&self) -> bool {
        !self.session.read_only() && self.item.kind.is_editable()
    }

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        cx.notify();
        let req = DocumentRequest {
            table: self.item.table.clone(),
            filter: self.filter.read(cx).value().to_string(),
            sort: self.sort.read(cx).value().to_string(),
            skip: self.skip,
            limit: PAGE,
        };
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = db(async move { session.documents(&req).await }).await;
            let _ = this.update(cx, |v, cx| {
                v.loading = false;
                match r {
                    Ok(page) => {
                        cx.emit(DocumentViewEvent::Log(page.statement.clone()));
                        v.page = Some(page);
                        v.error = None;
                    }
                    Err(e) => v.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the JSON editor for a document (`id`) or a new one (`None`).
    pub fn edit(&mut self, id: Option<String>, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        if text.len() > 2_000_000 {
            return prompt::error("Too large to edit here", "This value is over 2 MB. Change it with commands in the command editor.", window, cx);
        }
        let editor = cx.new(|cx| EditorState::new(window, cx).language("json").line_number(true).default_value(text));
        let title: SharedString = match (&id, self.is_mongo()) {
            (None, _) => "Add document".into(),
            (Some(_), true) => "Edit document".into(),
            (Some(_), false) => format!("Edit {}", self.item.name()).into(),
        };
        let hint: SharedString = if self.is_mongo() {
            "Shell syntax works: ObjectId(\"…\"), ISODate(\"…\"), NumberLong(…), unquoted keys.".into()
        } else {
            "The whole value is replaced, and its time to live kept.".into()
        };
        let this = cx.entity().downgrade();
        let focus = editor.read(cx).focus_handle(cx);
        window.open_dialog(cx, move |dialog, window, cx| {
            let p = cx.palette().clone();
            let (editor, this, id) = (editor.clone(), this.clone(), id.clone());
            let height = (window.viewport_size().height - px(260.)).clamp(px(200.), px(560.));
            dialog
                .title(title.clone())
                .w(px(760.))
                .margin_top(px(30.))
                .child(
                    v_flex()
                        .gap(px(8.))
                        .child(div().h(height).border_1().border_color(p.border).rounded(px(6.)).overflow_hidden().child(Editor::new(&editor).h_full()))
                        .child(div().text_size(px(11.)).text_color(p.secondary_text).child(hint.clone())),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap(px(8.))
                        .child(Button::new("doc-cancel").label("Cancel").small().on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("doc-save").label("Save").small().primary().on_click(move |_, window, cx| {
                            let text = editor.read(cx).value().to_string();
                            let _ = this.update(cx, |v, cx| v.save(id.clone(), text, window, cx));
                        })),
                )
        });
        window.focus(&focus, cx);
    }

    /// Turns edited text into statements, shows them (with the connection's
    /// safe mode), runs them and reloads.
    fn save(&mut self, id: Option<String>, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let table = self.item.table.clone();
        cx.spawn_in(window, async move |this, cx| {
            let s = session.clone();
            let r = db(async move { s.document_save(&table, id.as_deref(), &text).await }).await;
            let _ = this.update_in(cx, |v, window, cx| match r {
                Ok(statements) => {
                    window.close_dialog(cx);
                    v.run(statements, window, cx);
                }
                Err(e) => prompt::error("Cannot save", &format!("{e:#}"), window, cx),
            });
        })
        .detach();
    }

    fn delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        match self.session.document_delete(&self.item.table, &id) {
            Ok(statements) => {
                let what = if self.is_mongo() { "this document" } else { "this key and its value" };
                let this = cx.entity().downgrade();
                prompt::confirm("Delete", &format!("Delete {what}?\n\n{}", statements.join("\n")), "Delete", window, cx, move |window, cx| {
                    let _ = this.update(cx, |v, cx| v.run(statements.clone(), window, cx));
                });
            }
            Err(e) => prompt::error("Cannot delete", &format!("{e:#}"), window, cx),
        }
    }

    fn run(&mut self, statements: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let preview = statements.join("\n");
        let this = cx.entity().downgrade();
        guard::guarded(&self.session.clone(), &preview, window, cx, move |window, cx| {
            let _ = this.update(cx, |_, cx| {
                cx.spawn_in(window, async move |this, cx| {
                    let s = session.clone();
                    let statements2 = statements.clone();
                    let r = db(async move { s.run_statements(&statements2).await }).await;
                    let _ = this.update_in(cx, |v, window, cx| match r {
                        Ok(()) => {
                            for s in &statements {
                                cx.emit(DocumentViewEvent::Log(s.clone()));
                            }
                            v.reload(window, cx);
                        }
                        Err(e) => prompt::error("Failed", &format!("{e:#}"), window, cx),
                    });
                })
                .detach();
            });
        });
    }

    pub fn add_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_mongo() {
            self.edit(None, "{\n  \n}".into(), window, cx);
        }
    }

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.palette().clone();
        if self.is_mongo() {
            return h_flex()
                .px(px(8.))
                .py(px(6.))
                .gap(px(6.))
                .border_b_1()
                .border_color(p.separator)
                .child(div().flex_1().child(Input::new(&self.filter).small().font_family(MONO)))
                .child(div().w(px(220.)).child(Input::new(&self.sort).small().font_family(MONO)))
                .child(Button::new("doc-find").label("Find").small().primary().on_click(cx.listener(|v, _, window, cx| {
                    v.skip = 0;
                    v.reload(window, cx);
                })))
                .child(Button::new("doc-reset").label("Reset").small().ghost().on_click(cx.listener(|v, _, window, cx| {
                    v.filter.update(cx, |s, cx| s.set_value("", window, cx));
                    v.sort.update(cx, |s, cx| s.set_value("", window, cx));
                    v.skip = 0;
                    v.reload(window, cx);
                })))
                .into_any_element();
        }
        // Redis: the key and its facts.
        let info = self.page.as_ref().map(|pg| pg.info.clone()).unwrap_or_default();
        h_flex()
            .px(px(10.))
            .py(px(8.))
            .gap(px(8.))
            .flex_wrap()
            .border_b_1()
            .border_color(p.separator)
            .child(icon(crate::sidebar::item_icon(self.item.kind), 13., p.icon))
            .child(div().font_family(MONO).text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(self.item.name().to_string()))
            .children(info.into_iter().map(|(k, v)| {
                h_flex()
                    .h(px(20.))
                    .px(px(7.))
                    .gap(px(4.))
                    .rounded(px(10.))
                    .bg(p.window_secondary)
                    .border_1()
                    .border_color(p.border)
                    .text_size(px(11.))
                    .child(div().text_color(p.secondary_text).child(k))
                    .child(div().text_color(p.text).child(v))
            }))
            .into_any_element()
    }

    fn render_card(&self, ix: usize, id: Option<String>, text: String, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.palette().clone();
        let dark = AppState::get(cx).settings.appearance == Appearance::Dark;
        let shown: String = if text.len() > SHOWN_CHARS {
            let mut cut = SHOWN_CHARS;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}\n… {} more characters: Edit or Copy shows them all", &text[..cut], text.len() - cut)
        } else {
            text.clone()
        };
        let highlights = json_highlights(&shown, dark);
        let editable = self.editable();
        let mongo = self.is_mongo();
        let (copy_text, edit_text, edit_id, del_id) = (text.clone(), text, id.clone(), id.clone());
        let button = |id: SharedString, label: &'static str, icon_name: IconName| {
            h_flex()
                .id(id)
                .h(px(22.))
                .px(px(7.))
                .gap(px(4.))
                .rounded(px(4.))
                .text_size(px(11.))
                .text_color(p.secondary_text)
                .hover(|s| s.bg(p.hover).text_color(p.text))
                .child(icon(icon_name, 11., p.secondary_text))
                .child(label)
        };
        v_flex()
            .id(("doc", ix))
            .group("doc")
            .w_full()
            .rounded(px(8.))
            .border_1()
            .border_color(p.border)
            .bg(p.window)
            .child(
                h_flex()
                    .px(px(10.))
                    .pt(px(6.))
                    .gap(px(2.))
                    .child(div().flex_1().text_size(px(11.)).text_color(p.secondary_text).font_family(MONO).truncate().child(match (&id, mongo) {
                        (Some(id), true) => format!("_id: {id}"),
                        _ => String::new(),
                    }))
                    .child(button(SharedString::from(format!("doc-copy-{ix}")), "Copy", IconName::Copy).on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                    }))
                    .when(editable && edit_id.is_some(), |d| {
                        d.child(button(SharedString::from(format!("doc-edit-{ix}")), "Edit", IconName::SquarePen).on_click(cx.listener(move |v, _, window, cx| {
                            v.edit(edit_id.clone(), edit_text.clone(), window, cx);
                        })))
                    })
                    .when(editable, |d| {
                        d.when_some(del_id, |d, id| {
                            d.child(button(SharedString::from(format!("doc-del-{ix}")), "Delete", IconName::Trash).on_click(cx.listener(move |v, _, window, cx| {
                                v.delete(id.clone(), window, cx);
                            })))
                        })
                    }),
            )
            .child(
                div()
                    .px(px(12.))
                    .pt(px(2.))
                    .pb(px(10.))
                    .font_family(MONO)
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .text_color(p.data_text)
                    .child(StyledText::new(shown).with_highlights(highlights)),
            )
            .into_any_element()
    }
}

impl Focusable for DocumentView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DocumentView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let toolbar = self.render_toolbar(cx);
        let body: AnyElement = match (&self.error, &self.page) {
            (Some(e), _) => placeholder(e.clone(), cx).into_any_element(),
            (None, None) => placeholder("Loading...", cx).into_any_element(),
            (None, Some(pg)) if pg.documents.is_empty() => {
                placeholder(if self.is_mongo() { "No documents match." } else { "The key is empty." }, cx).into_any_element()
            }
            (None, Some(pg)) => {
                let docs = pg.documents.clone();
                v_flex()
                    .id("documents")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p(px(10.))
                    .gap(px(8.))
                    .children(docs.into_iter().enumerate().map(|(i, d)| self.render_card(i, d.id, d.text, cx)))
                    .into_any_element()
            }
        };
        let total = self.page.as_ref().map(|pg| pg.total).unwrap_or(0);
        let shown = self.page.as_ref().map(|pg| pg.documents.len() as u64).unwrap_or(0);
        let mongo = self.is_mongo();
        let count = if !mongo {
            String::new()
        } else if shown == 0 {
            format!("0 of {total}")
        } else {
            format!("{}–{} of {total}", self.skip + 1, self.skip + shown)
        };
        let (at_start, at_end) = (self.skip == 0, self.skip + shown >= total);
        let read_only = self.session.read_only();
        let this = cx.entity().downgrade();
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(p.window_secondary)
            .child(toolbar)
            .child(div().flex_1().min_h(px(0.)).flex().flex_col().child(body))
            .child(
                h_flex()
                    .h(px(40.))
                    .px(px(8.))
                    .gap(px(8.))
                    .flex_shrink_0()
                    .bg(p.window_secondary)
                    .border_t_1()
                    .border_color(p.separator)
                    .child(segmented("doc-mode", &["JSON", "Table"], 0, 56., cx, move |i, _, cx| {
                        if i == 1 {
                            let _ = this.update(cx, |_, cx| cx.emit(DocumentViewEvent::ShowTable));
                        }
                    }))
                    .when(mongo && self.editable(), |d| {
                        d.child(
                            footer_button("doc-add", cx)
                                .child(icon(IconName::Plus, 10., p.icon))
                                .child("Document")
                                .on_click(cx.listener(|v, _, window, cx| v.add_document(window, cx))),
                        )
                    })
                    .when(!mongo && self.editable(), |d| {
                        let doc = self.page.as_ref().and_then(|pg| pg.documents.first().cloned());
                        d.when_some(doc, |d, doc| {
                            d.child(footer_button("doc-edit-value", cx).child(icon(IconName::SquarePen, 10., p.icon)).child("Edit value").on_click(cx.listener(
                                move |v, _, window, cx| v.edit(doc.id.clone(), doc.text.clone(), window, cx),
                            )))
                        })
                    })
                    .when(read_only && self.item.kind.is_editable(), |d| {
                        d.child(
                            h_flex()
                                .gap(px(4.))
                                .text_size(UI_TEXT)
                                .text_color(p.secondary_text)
                                .child(icon(IconName::Eye, 11., p.secondary_text))
                                .child("Read only"),
                        )
                    })
                    .child(div().flex_1().text_center().text_size(UI_TEXT).text_color(p.text).child(if self.loading { "Loading…".to_string() } else { count }))
                    .child(footer_button("doc-query", cx).child(if mongo { "Query" } else { "Command" }).on_click(cx.listener(|v, _, _, cx| {
                        if let Some(pg) = &v.page {
                            cx.emit(DocumentViewEvent::OpenSql(pg.statement.clone()));
                        }
                    })))
                    .child(footer_button("doc-reload", cx).child(icon(IconName::RefreshCw, 11., p.icon)).on_click(cx.listener(|v, _, window, cx| v.reload(window, cx))))
                    .when(mongo, |d| {
                        d.child(
                            Button::new("doc-prev")
                                .icon(IconName::ChevronLeft)
                                .xsmall()
                                .ghost()
                                .disabled(at_start)
                                .on_click(cx.listener(|v, _, window, cx| {
                                    v.skip = v.skip.saturating_sub(PAGE);
                                    v.reload(window, cx);
                                })),
                        )
                        .child(
                            Button::new("doc-next")
                                .icon(IconName::ChevronRight)
                                .xsmall()
                                .ghost()
                                .disabled(at_end)
                                .on_click(cx.listener(|v, _, window, cx| {
                                    v.skip += PAGE;
                                    v.reload(window, cx);
                                })),
                        )
                    }),
            )
    }
}

/// Colors for keys, strings, numbers, literals and shell helpers in
/// JSON-like text.
pub fn json_highlights(text: &str, dark: bool) -> Vec<(Range<usize>, HighlightStyle)> {
    let color = |light: u32, dark_color: u32| HighlightStyle { color: Some(rgb(if dark { dark_color } else { light }).into()), ..Default::default() };
    let (key, string, number, literal, helper) = (
        color(0x0550AE, 0x7CB7FF),
        color(0x116329, 0x9ECE6A),
        color(0x953800, 0xF5A97F),
        color(0x8250DF, 0xC6A0F6),
        color(0x9A6700, 0xEED49F),
    );
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(bytes.len());
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            let style = if bytes.get(j) == Some(&b':') { key.clone() } else { string.clone() };
            out.push((start..i, style));
        } else if c.is_ascii_digit() || (c == b'-' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || ((bytes[i] == b'-' || bytes[i] == b'+') && matches!(bytes[i - 1], b'e' | b'E'))) {
                i += 1;
            }
            out.push((start..i, number.clone()));
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$') {
                i += 1;
            }
            let word = &text[start..i];
            if matches!(word, "true" | "false" | "null" | "NaN" | "Infinity") {
                out.push((start..i, literal.clone()));
            } else if bytes.get(i) == Some(&b'(') {
                out.push((start..i, helper.clone()));
            } else if bytes.get(i) == Some(&b':') {
                out.push((start..i, key.clone()));
            }
        } else {
            // Multi-byte characters are skipped whole, so ranges stay on boundaries.
            i += text[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn highlights_land_on_tokens() {
        let text = "{\n  \"_id\": ObjectId(\"65a1\"),\n  \"name\": \"Ann 文字\",\n  \"age\": -42.5,\n  \"ok\": true\n}";
        let spans: Vec<&str> = json_highlights(text, true).iter().map(|(r, _)| &text[r.clone()]).collect();
        assert_eq!(spans, vec!["\"_id\"", "ObjectId", "\"65a1\"", "\"name\"", "\"Ann 文字\"", "\"age\"", "-42.5", "\"ok\"", "true"]);
    }
}
