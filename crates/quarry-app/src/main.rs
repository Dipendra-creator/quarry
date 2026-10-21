//! Quarry desktop app: an open-source database client in the style of TablePlus.

// Release builds are GUI apps; debug builds keep a console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod assistant;
mod backup;
mod completion;
mod connection_form;
mod data_view;
mod grid;
mod guard;
mod picker;
mod preferences;
mod processes;
mod prompt;
mod query_editor;
mod row_detail;
mod sidebar;
mod state;
mod theme;
mod welcome;
mod widgets;
mod workspace;


fn main() {
    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(|cx| {
        cx.set_app_identity("io.github.dipendra-creator.quarry", "Quarry");
        gpui_kit::init(cx);
        let mut approvals = match state::AppState::init(cx) {
            Ok(rx) => rx,
            Err(e) => {
                eprintln!("could not load settings: {e:#}");
                cx.quit();
                return;
            }
        };
        cx.spawn(async move |cx| {
            while let Some(request) = approvals.recv().await {
                let _ = cx.update(|cx| assistant::ask_approval(request, cx));
            }
        })
        .detach();
        let dark = state::AppState::get(cx).settings.appearance == state::Appearance::Dark;
        theme::apply(dark, None, cx);
        actions::bind(cx);
        cx.on_action(|_: &actions::Quit, cx| cx.quit());
        cx.on_action(|_: &actions::OpenConnections, cx| welcome::open(cx));
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        welcome::open(cx);
        cx.activate(true);
    });
}
