//! SQL editor tab with per-statement result tabs (CustomQuery / ResultController).

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, resizable_panel, v_flex, v_resizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use quarry_core::export::ExportFormat;
use quarry_core::session::{Outcome, StatementOutcome};
use quarry_core::{Session, TableRef, sql};

use crate::completion::{Catalog, SqlCompletion};
use crate::data_view::RowSnapshot;
use crate::grid::{GridDelegate, readonly_table};
use crate::state::{AppState, db};
use crate::theme::{MONO, PaletteExt as _, UI_TEXT};
use crate::widgets::segmented;
use crate::{guard, prompt};

pub enum QueryEvent {
    Log(String),
    RowSelected(Option<RowSnapshot>),
    Changed,
    /// A prompt for the AI assistant about this editor's query.
    AskAi(String),
}

struct ResultTab {
    outcome: StatementOutcome,
    table: Option<Entity<TableState<GridDelegate>>>,
}

pub struct QueryEditor {
    session: Arc<Session>,
    pub title: String,
    /// Saved-query id when opened from the Queries list.
    pub saved_id: Option<String>,
    pub editor: Entity<EditorState>,
    results: Vec<ResultTab>,
    active: usize,
    show_message: bool,
    running: bool,
    limit: Option<u32>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<QueryEvent> for QueryEditor {}

const LIMITS: [Option<u32>; 5] = [Some(100), Some(300), Some(1000), Some(5000), None];

impl QueryEditor {
    pub fn new(session: Arc<Session>, title: String, sql: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .folding(true)
                .default_value(sql)
        });
        let subs = vec![cx.subscribe(&editor, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })];
        let catalog = std::rc::Rc::new(std::cell::RefCell::new(Catalog::default()));
        editor.update(cx, |e, _| {
            e.lsp_mut().completion_provider = Some(std::rc::Rc::new(SqlCompletion { catalog: catalog.clone() }));
        });
        let s = session.clone();
        cx.spawn(async move |_, _| {
            let loaded = db(async move {
                let items = s.list_items(None).await?;
                let columns = s.all_columns(None).await.unwrap_or_default();
                Ok((items, columns))
            })
            .await;
            if let Ok((items, columns)) = loaded {
                catalog.borrow_mut().set(&items, columns);
            }
        })
        .detach();
        let limit = AppState::get(cx).settings.query_limit;
        Self {
            session,
            title,
            saved_id: None,
            editor,
            results: vec![],
            active: 0,
            show_message: false,
            running: false,
            limit,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    pub fn sql(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    pub fn editor_focus(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }

    /// Selection if any, else the statement under the cursor.
    fn current_sql(&self, cx: &App) -> Option<String> {
        let state = self.editor.read(cx);
        let selected = state.selected_value().to_string();
        if !selected.trim().is_empty() {
            return Some(selected);
        }
        sql::statement_at(&state.value(), state.cursor()).map(|s| s.text)
    }

    pub fn run_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(script) = self.current_sql(cx) {
            self.run(script, window, cx);
        }
    }

    /// Asks the assistant to explain, optimize or fix the current query.
    fn ask_ai(&mut self, kind: &str, cx: &mut Context<Self>) {
        let Some(sql) = self.current_sql(cx).filter(|s| !s.trim().is_empty()) else { return };
        let sql = sql.trim();
        let error = self.results.iter().find_map(|r| match &r.outcome.outcome {
            Outcome::Error(e) => Some(e.clone()),
            _ => None,
        });
        let prompt = match (kind, error) {
            ("explain", _) => format!("Explain what this query does, step by step, and point out any mistakes:
```sql
{sql}
```"),
            ("optimize", _) => format!(
                "Optimize this query. Read its plan with explain_query and the indexes of the tables it uses, then give me a faster version and any indexes worth adding, and say why they help:
```sql
{sql}
```"
            ),
            (_, Some(e)) => format!("This query fails with:
{e}

Find the cause and give me a corrected query:
```sql
{sql}
```"),
            (_, None) => format!("Check this query for mistakes and give me a corrected version if it needs one:
```sql
{sql}
```"),
        };
        cx.emit(QueryEvent::AskAi(prompt));
    }

    pub fn run_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let script = self.sql(cx);
        self.run(script, window, cx);
    }

    fn run(&mut self, script: String, window: &mut Window, cx: &mut Context<Self>) {
        if script.trim().is_empty() || self.running {
            return;
        }
        let this = cx.entity().downgrade();
        let session = self.session.clone();
        let limit = self.limit;
        let preview = script.clone();
        guard::guarded(&self.session.clone(), &preview, window, cx, move |window, cx| {
            let _ = this.update(cx, |this, cx| {
                this.running = true;
                cx.notify();
                cx.spawn_in(window, async move |this, cx| {
                    let outcomes = db(async move { Ok(session.run_script(&script, limit).await) }).await.unwrap_or_default();
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.running = false;
                        this.set_results(outcomes, window, cx);
                    });
                })
                .detach();
            });
        });
    }

    fn set_results(&mut self, outcomes: Vec<StatementOutcome>, window: &mut Window, cx: &mut Context<Self>) {
        self.results = outcomes
            .into_iter()
            .map(|o| {
                cx.emit(QueryEvent::Log(o.statement.clone()));
                let table = match &o.outcome {
                    Outcome::Result(r) if r.has_rows() => {
                        let t = readonly_table(r.columns.clone(), r.rows.clone(), window, cx);
                        self._subs.push(cx.subscribe(&t, |this: &mut Self, t, ev: &TableEvent, cx| {
                            if let TableEvent::SelectCell(row, _) | TableEvent::SelectRow(row) = ev {
                                let d = t.read(cx).delegate();
                                let snap = d.rows.get(*row).map(|r| RowSnapshot {
                                    row: *row,
                                    columns: d.columns.clone(),
                                    values: r.current(),
                                    editable: false,
                                });
                                cx.emit(QueryEvent::RowSelected(snap));
                            }
                            let _ = this;
                        }));
                        Some(t)
                    }
                    _ => None,
                };
                ResultTab { outcome: o, table }
            })
            .collect();
        // Show the last statement's result, or the error that stopped the run.
        self.active = self.results.len().saturating_sub(1);
        self.show_message = self.results.last().is_some_and(|r| r.table.is_none());
        cx.emit(QueryEvent::Changed);
        cx.notify();
    }

    pub fn beautify(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let formatted = sql::format_sql(&self.sql(cx));
        self.editor.update(cx, |e, cx| e.set_value(formatted, window, cx));
    }

    pub fn uglify(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let min = sql::minify_sql(&self.sql(cx));
        self.editor.update(cx, |e, cx| e.set_value(min, window, cx));
    }

    pub fn save_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.sql(cx);
        let conn = self.session.config.id.clone();
        let existing = self.saved_id.clone();
        let this = cx.entity().downgrade();
        let default = self.title.clone();
        prompt::ask("Save query", "Name", &default, false, window, cx, move |name, _, cx| {
            let name = if name.trim().is_empty() { "Untitled".to_string() } else { name.trim().to_string() };
            let id = existing.clone().unwrap_or_else(|| quarry_core::ConnectionConfig::default().id);
            let q = quarry_core::store::SavedQuery { id: id.clone(), connection_id: conn.clone(), name: name.clone(), sql: sql.clone() };
            cx.update_global::<AppState, _>(|s, _| {
                let _ = s.saved.upsert(q);
            });
            let _ = this.update(cx, |e, cx| {
                e.saved_id = Some(id);
                e.title = name;
                cx.emit(QueryEvent::Changed);
            });
        });
    }

    pub fn save_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.sql(cx);
        let name = format!("{}.sql", self.title.trim_end_matches(".sql"));
        let rx = cx.prompt_for_new_path(&crate::backup::home(), Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let r = std::fs::write(&path, sql);
            let _ = this.update_in(cx, |e, window, cx| match r {
                Ok(()) => {
                    e.title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(e.title.clone());
                    cx.emit(QueryEvent::Changed);
                }
                Err(err) => prompt::error("Could not save file", &err.to_string(), window, cx),
            });
        })
        .detach();
    }

    fn export(&mut self, format: ExportFormat, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ResultTab { outcome: StatementOutcome { outcome: Outcome::Result(r), .. }, .. }) = self.results.get(self.active) else {
            return;
        };
        let text = quarry_core::export::export(r, format, self.session.dialect(), &TableRef::new(None, "exported"));
        let dir = crate::backup::home();
        let rx = cx.prompt_for_new_path(&dir, Some(&format!("result.{}", format.extension())));
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let r = std::fs::write(&path, text);
                let _ = cx.update(|window, cx| {
                    if let Err(e) = r {
                        prompt::error("Export failed", &e.to_string(), window, cx);
                    }
                });
            }
        })
        .detach();
    }

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        let state = self.editor.read(cx);
        let pos = state.cursor_position();
        let limit_label = match self.limit {
            Some(n) => format!("Limit {n}"),
            None => "No limit".to_string(),
        };
        let this = cx.entity().downgrade();
        h_flex()
            .h(px(40.))
            .px(px(8.))
            .gap(px(8.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(p.separator)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .font_family(MONO)
                    .text_size(px(11.))
                    .text_color(p.secondary_text)
                    .child(format!("Line {}, Column {}", pos.line + 1, pos.character + 1)),
            )
            .child(Button::new("q-ai").icon(IconName::Sparkles).label("Ask AI").xsmall().outline().dropdown_caret(true).dropdown_menu({
                let this = this.clone();
                move |menu, _, _| {
                    [("Explain this query", "explain"), ("Optimize this query", "optimize"), ("Fix this query", "fix")].into_iter().fold(menu, |m, (label, kind)| {
                        let t = this.clone();
                        m.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            let _ = t.update(cx, |e, cx| e.ask_ai(kind, cx));
                        }))
                    })
                }
            }))
            .child(Button::new("q-save").label("Save").xsmall().outline().dropdown_caret(true).dropdown_menu({
                let this = this.clone();
                move |menu, _, _| {
                    let (a, b) = (this.clone(), this.clone());
                    menu.item(PopupMenuItem::new("Save to Queries list (Ctrl+S)").on_click(move |_, window, cx| {
                        let _ = a.update(cx, |e, cx| e.save_query(window, cx));
                    }))
                    .item(PopupMenuItem::new("Save as .sql file... (Ctrl+Shift+S)").on_click(move |_, window, cx| {
                        let _ = b.update(cx, |e, cx| e.save_file(window, cx));
                    }))
                }
            }))
            .child(
                Button::new("q-limit").label(limit_label).xsmall().outline().dropdown_caret(true).dropdown_menu({
                    let this = this.clone();
                    move |menu, _, _| {
                        LIMITS.iter().fold(menu, |m, l| {
                            let (t, l) = (this.clone(), *l);
                            m.item(PopupMenuItem::new(l.map(|n| format!("Limit {n}")).unwrap_or("No limit".into())).on_click(move |_, _, cx| {
                                cx.update_global::<AppState, _>(|s, _| {
                                    s.settings.query_limit = l;
                                    s.settings.save();
                                });
                                let _ = t.update(cx, |e, cx| {
                                    e.limit = l;
                                    cx.notify();
                                });
                            }))
                        })
                    }
                }),
            )
            .child(
                h_flex()
                    .child(
                        Button::new("q-beautify")
                            .label("Beautify")
                            .xsmall()
                            .outline()
                            .tooltip("Pretty the queries")
                            .on_click(cx.listener(|e, _, window, cx| e.beautify(window, cx))),
                    )
                    .child(Button::new("q-beautify-menu").icon(IconName::ChevronDown).xsmall().outline().dropdown_menu({
                        let this = this.clone();
                        move |menu, _, _| {
                            let (a, b) = (this.clone(), this.clone());
                            menu.item(PopupMenuItem::new("Beautify").on_click(move |_, window, cx| {
                                let _ = a.update(cx, |e, cx| e.beautify(window, cx));
                            }))
                            .item(PopupMenuItem::new("Uglify").on_click(move |_, window, cx| {
                                let _ = b.update(cx, |e, cx| e.uglify(window, cx));
                            }))
                        }
                    })),
            )
            .child(
                h_flex()
                    .child(
                        Button::new("q-run")
                            .label("Run Current")
                            .xsmall()
                            .primary()
                            .loading(self.running)
                            .tooltip("Run the current query (Ctrl+Enter)")
                            .on_click(cx.listener(|e, _, window, cx| e.run_current(window, cx))),
                    )
                    .child(Button::new("q-run-menu").icon(IconName::ChevronDown).xsmall().primary().dropdown_menu({
                        let this = this.clone();
                        move |menu, _, _| {
                            let (a, b) = (this.clone(), this.clone());
                            menu.item(PopupMenuItem::new("Run Current (Ctrl+Enter)").on_click(move |_, window, cx| {
                                let _ = a.update(cx, |e, cx| e.run_current(window, cx));
                            }))
                            .item(PopupMenuItem::new("Run All (Ctrl+Shift+Enter)").on_click(move |_, window, cx| {
                                let _ = b.update(cx, |e, cx| e.run_all(window, cx));
                            }))
                        }
                    })),
            )
    }

    fn render_results(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        if self.results.is_empty() {
            return v_flex()
                .size_full()
                .child(crate::widgets::placeholder("Run a query to see its results here.", cx))
                .into_any_element();
        }
        let tabs = h_flex()
            .h(px(30.))
            .px(px(2.))
            .gap(px(2.))
            .flex_shrink_0()
            .bg(p.tab)
            .border_b_1()
            .border_color(p.separator)
            .children(self.results.iter().enumerate().map(|(i, r)| {
                let failed = matches!(r.outcome.outcome, Outcome::Error(_));
                let label: String = r.outcome.statement.chars().take(40).map(|c| if c.is_whitespace() { ' ' } else { c }).collect();
                div()
                    .id(("result-tab", i))
                    .h(px(26.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .rounded(px(14.))
                    .max_w(px(220.))
                    .text_size(UI_TEXT)
                    .text_color(if failed { p.danger } else { p.text })
                    .when(i == self.active, |d| d.bg(p.tab_selected))
                    .hover(|s| s.bg(p.tab_selected))
                    .child(div().truncate().child(label))
                    .on_click(cx.listener(move |e, _, _, cx| {
                        e.active = i;
                        e.show_message = e.results[i].table.is_none();
                        cx.notify();
                    }))
            }));
        let current = &self.results[self.active.min(self.results.len() - 1)];
        let (status, count, message) = match &current.outcome.outcome {
            Outcome::Result(r) => (format!("{} ms", r.duration_ms), r.summary(), format!("{}\n\n{}", current.outcome.statement, r.summary())),
            Outcome::Error(e) => ("Error".to_string(), String::new(), format!("{}\n\n{e}", current.outcome.statement)),
        };
        let body = match (&current.table, self.show_message) {
            (Some(t), false) => DataTable::new(t).stripe(true).bordered(false).xsmall().into_any_element(),
            _ => div()
                .id("message")
                .size_full()
                .p(px(8.))
                .overflow_y_scroll()
                .font_family(MONO)
                .text_size(UI_TEXT)
                .text_color(if matches!(current.outcome.outcome, Outcome::Error(_)) { p.danger } else { p.text })
                .child(message)
                .into_any_element(),
        };
        let this = cx.entity().downgrade();
        let has_table = current.table.is_some();
        v_flex()
            .size_full()
            .child(tabs)
            .child(div().flex_1().overflow_hidden().child(body))
            .child(
                h_flex()
                    .h(px(40.))
                    .px(px(8.))
                    .gap(px(8.))
                    .flex_shrink_0()
                    .bg(p.window_secondary)
                    .border_t_1()
                    .border_color(p.separator)
                    .child(segmented("q-mode", &["Data", "Message"], self.show_message as usize, 60., cx, move |i, _, cx| {
                        let _ = this.update(cx, |e, cx| {
                            e.show_message = i == 1;
                            cx.notify();
                        });
                    }))
                    .child(div().w(px(110.)).text_size(px(11.)).text_color(p.text).child(status))
                    .child(div().flex_1().flex().justify_center().text_size(UI_TEXT).child(count))
                    .child(Button::new("q-export").label("Export...").xsmall().outline().disabled(!has_table).dropdown_menu({
                        let this = cx.entity().downgrade();
                        move |menu, _, _| {
                            ExportFormat::ALL.iter().fold(menu, |m, f| {
                                let (t, f) = (this.clone(), *f);
                                m.item(PopupMenuItem::new(format!("Export as {}...", f.label())).on_click(move |_, window, cx| {
                                    let _ = t.update(cx, |e, cx| e.export(f, window, cx));
                                }))
                            })
                        }
                    })),
            )
            .into_any_element()
    }
}

impl Focusable for QueryEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for QueryEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().clone();
        v_flex()
            .key_context("QueryEditor")
            .track_focus(&self.focus)
            .size_full()
            .bg(p.window)
            .on_action(cx.listener(|e, _: &crate::actions::RunCurrent, window, cx| e.run_current(window, cx)))
            .on_action(cx.listener(|e, _: &crate::actions::RunAll, window, cx| e.run_all(window, cx)))
            .on_action(cx.listener(|e, _: &crate::actions::SaveFile, window, cx| e.save_file(window, cx)))
            .child(
                v_resizable("query-split")
                    .child(
                        resizable_panel().size(px(260.)).child(
                            v_flex()
                                .size_full()
                                .child(div().flex_1().overflow_hidden().child(Editor::new(&self.editor).h_full().bordered(false)))
                                .child(self.render_toolbar(cx)),
                        ),
                    )
                    .child(resizable_panel().child(self.render_results(cx))),
            )
    }
}
