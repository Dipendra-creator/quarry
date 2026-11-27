//! Keyboard actions and their default bindings (from the original's
//! WorkspaceWindow input bindings). On macOS every Ctrl shortcut also works
//! with Cmd, as Mac users expect.

use gpui_kit::{Action, App, KeyBinding};

gpui_kit::actions!(
    quarry,
    [
        OpenAnything,
        NewQueryTab,
        InsertRow,
        DuplicateRow,
        OpenConnections,
        Commit,
        Reload,
        Reconnect,
        DatabaseList,
        PreviewChanges,
        ToggleDetail,
        CloseTab,
        CloseWindow,
        ShowFilter,
        DiscardChanges,
        Disconnect,
        ToggleConsole,
        ToggleSidebar,
        Preferences,
        RunCurrent,
        RunAll,
        DeleteRows,
        NextTab,
        PrevTab,
        Quit,
        OpenProcessList,
        OpenFile,
        SaveFile,
        ToggleAssistant,
        CheckForUpdates,
    ]
);

/// A shortcut label as this platform spells it: "Ctrl+Shift+P" stays as is
/// on Windows and Linux and becomes "⌘⇧P" on macOS. Ctrl+Tab and Ctrl+Space
/// keep Control on macOS, where Cmd+Tab and Cmd+Space belong to the system.
pub fn kbd(keys: &str) -> String {
    if !cfg!(target_os = "macos") {
        return keys.to_string();
    }
    let ctrl = if keys.ends_with("Tab") || keys.ends_with("Space") { "⌃" } else { "⌘" };
    keys.replace("Ctrl+", ctrl).replace("Shift+", "⇧").replace("Alt+", "⌥").replace("Enter", "↩").replace("Delete", "⌦")
}

/// The binding, plus its Cmd twin on macOS.
fn bind_ctrl<A: Action + Clone>(out: &mut Vec<KeyBinding>, keys: &str, action: A, context: Option<&str>) {
    if cfg!(target_os = "macos") && keys.contains("ctrl-") && !keys.ends_with("tab") && !keys.ends_with("space") {
        out.push(KeyBinding::new(&keys.replace("ctrl-", "cmd-"), action.clone(), context));
    }
    out.push(KeyBinding::new(keys, action, context));
}

pub fn bind(cx: &mut App) {
    let ws = Some("Workspace");
    let mut b = Vec::new();
    bind_ctrl(&mut b, "ctrl-p", OpenAnything, ws);
    bind_ctrl(&mut b, "ctrl-e", NewQueryTab, ws);
    bind_ctrl(&mut b, "ctrl-t", NewQueryTab, ws);
    bind_ctrl(&mut b, "ctrl-i", InsertRow, ws);
    bind_ctrl(&mut b, "ctrl-d", DuplicateRow, Some("DataView"));
    bind_ctrl(&mut b, "ctrl-n", OpenConnections, None);
    bind_ctrl(&mut b, "ctrl-s", Commit, ws);
    bind_ctrl(&mut b, "ctrl-r", Reload, ws);
    bind_ctrl(&mut b, "f5", Reload, ws);
    bind_ctrl(&mut b, "ctrl-shift-r", Reconnect, ws);
    bind_ctrl(&mut b, "ctrl-k", DatabaseList, ws);
    bind_ctrl(&mut b, "ctrl-shift-p", PreviewChanges, ws);
    bind_ctrl(&mut b, "ctrl-space", ToggleDetail, ws);
    bind_ctrl(&mut b, "ctrl-w", CloseTab, ws);
    // Outside a workspace (welcome, connection forms) the same keys close the window.
    bind_ctrl(&mut b, "ctrl-w", CloseWindow, None);
    bind_ctrl(&mut b, "ctrl-f", ShowFilter, Some("DataView"));
    bind_ctrl(&mut b, "ctrl-shift-delete", DiscardChanges, ws);
    bind_ctrl(&mut b, "ctrl-shift-backspace", DiscardChanges, ws);
    bind_ctrl(&mut b, "shift-f5", Disconnect, ws);
    bind_ctrl(&mut b, "ctrl-alt-c", ToggleConsole, ws);
    bind_ctrl(&mut b, "ctrl-0", ToggleSidebar, ws);
    bind_ctrl(&mut b, "ctrl-,", Preferences, None);
    // Bound on the editor itself so they win over its own Ctrl+Enter.
    bind_ctrl(&mut b, "ctrl-enter", RunCurrent, Some("QueryEditor > Input"));
    bind_ctrl(&mut b, "ctrl-shift-enter", RunAll, Some("QueryEditor > Input"));
    bind_ctrl(&mut b, "ctrl-enter", RunCurrent, Some("QueryEditor"));
    bind_ctrl(&mut b, "ctrl-shift-enter", RunAll, Some("QueryEditor"));
    bind_ctrl(&mut b, "delete", DeleteRows, Some("DataView"));
    bind_ctrl(&mut b, "ctrl-tab", NextTab, ws);
    bind_ctrl(&mut b, "ctrl-shift-tab", PrevTab, ws);
    bind_ctrl(&mut b, "ctrl-q", Quit, None);
    bind_ctrl(&mut b, "ctrl-.", OpenProcessList, ws);
    bind_ctrl(&mut b, "ctrl-o", OpenFile, ws);
    bind_ctrl(&mut b, "ctrl-shift-s", SaveFile, Some("QueryEditor"));
    bind_ctrl(&mut b, "ctrl-l", ToggleAssistant, ws);
    cx.bind_keys(b);
}

#[cfg(test)]
mod tests {
    #[::core::prelude::v1::test]
    fn labels() {
        let (p, tab) = (super::kbd("Ctrl+Shift+P"), super::kbd("Ctrl+Tab"));
        if cfg!(target_os = "macos") {
            assert_eq!((p.as_str(), tab.as_str()), ("⌘⇧P", "⌃Tab"));
        } else {
            assert_eq!((p.as_str(), tab.as_str()), ("Ctrl+Shift+P", "Ctrl+Tab"));
        }
    }
}
