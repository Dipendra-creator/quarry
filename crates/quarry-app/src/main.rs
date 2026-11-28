//! Quarry desktop app: an open-source database client in the style of TablePlus.

// Release builds are GUI apps; debug builds keep a console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod assistant;
mod backup;
mod completion;
mod connection_form;
mod data_view;
mod document_view;
mod grid;
mod guard;
mod picker;
mod preferences;
mod processes;
mod prompt;
mod query_editor;
mod row_detail;
mod shortcuts;
mod sidebar;
mod state;
mod theme;
mod update;
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
        // After gpui_kit::init: the library's own keys are kept as the base.
        actions::init(cx);
        cx.on_action(|_: &actions::Quit, cx| cx.quit());
        cx.on_action(|_: &actions::OpenConnections, cx| welcome::open(cx));
        cx.on_action(|_: &actions::CloseWindow, cx| {
            cx.defer(|cx| {
                if let Some(w) = cx.active_window() {
                    let _ = w.update(cx, |_, window, _| window.remove_window());
                }
            })
        });
        // From the macOS menu bar, which has no window to dispatch to, and from
        // keys in windows without their own handler. Deferred: a key arrives
        // while its window is busy dispatching it and cannot be updated.
        cx.on_action(|_: &actions::Preferences, cx| cx.defer(|cx| with_window(cx, crate::preferences::open)));
        cx.on_action(|_: &actions::CheckForUpdates, cx| cx.defer(|cx| with_window(cx, update::check_now)));
        cx.on_action(|_: &actions::ShowShortcuts, cx| cx.defer(|cx| with_window(cx, shortcuts::open)));
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        welcome::open(cx);
        update::init(cx);
        cx.activate(true);
    });
}

/// Runs `f` in the active window, or the first one.
fn with_window(cx: &mut gpui_kit::App, f: fn(&mut gpui_kit::Window, &mut gpui_kit::App)) {
    if let Some(w) = cx.active_window().or_else(|| cx.windows().into_iter().next()) {
        let _ = w.update(cx, |_, window, cx| f(window, cx));
    }
}

/// The macOS menu bar: the app menu (with Quit, so Cmd+Q works) and an Edit
/// menu whose items reach the focused text field. Other platforms have none.
/// Set again whenever shortcuts change, as the items show their keys.
pub(crate) fn set_menus(cx: &mut gpui_kit::App) {
    use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    use gpui_kit::{Menu, MenuItem};
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.set_menus(vec![
        Menu::new("Quarry").items([
            MenuItem::action("Check for Updates…", actions::CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Settings…", actions::Preferences),
            MenuItem::action("Keyboard Shortcuts…", actions::ShowShortcuts),
            MenuItem::separator(),
            MenuItem::action("Quit Quarry", actions::Quit),
        ]),
        Menu::new("Edit").items([
            MenuItem::action("Undo", Undo),
            MenuItem::action("Redo", Redo),
            MenuItem::separator(),
            MenuItem::action("Cut", Cut),
            MenuItem::action("Copy", Copy),
            MenuItem::action("Paste", Paste),
            MenuItem::action("Select All", SelectAll),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Connections", actions::OpenConnections),
            MenuItem::action("Close Window", actions::CloseWindow),
        ]),
        Menu::new("Help").items([MenuItem::action("Keyboard Shortcuts", actions::ShowShortcuts)]),
    ]);
}
