//! Keyboard actions and their default bindings (from the original's
//! WorkspaceWindow input bindings).

use gpui_kit::{App, KeyBinding};

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
    ]
);

pub fn bind(cx: &mut App) {
    let ws = Some("Workspace");
    cx.bind_keys([
        KeyBinding::new("ctrl-p", OpenAnything, ws),
        KeyBinding::new("ctrl-e", NewQueryTab, ws),
        KeyBinding::new("ctrl-t", NewQueryTab, ws),
        KeyBinding::new("ctrl-i", InsertRow, ws),
        KeyBinding::new("ctrl-d", DuplicateRow, Some("DataView")),
        KeyBinding::new("ctrl-n", OpenConnections, None),
        KeyBinding::new("ctrl-s", Commit, ws),
        KeyBinding::new("ctrl-r", Reload, ws),
        KeyBinding::new("f5", Reload, ws),
        KeyBinding::new("ctrl-shift-r", Reconnect, ws),
        KeyBinding::new("ctrl-k", DatabaseList, ws),
        KeyBinding::new("ctrl-shift-p", PreviewChanges, ws),
        KeyBinding::new("ctrl-space", ToggleDetail, ws),
        KeyBinding::new("ctrl-w", CloseTab, ws),
        KeyBinding::new("ctrl-f", ShowFilter, Some("DataView")),
        KeyBinding::new("ctrl-shift-delete", DiscardChanges, ws),
        KeyBinding::new("ctrl-shift-backspace", DiscardChanges, ws),
        KeyBinding::new("shift-f5", Disconnect, ws),
        KeyBinding::new("ctrl-alt-c", ToggleConsole, ws),
        KeyBinding::new("ctrl-0", ToggleSidebar, ws),
        KeyBinding::new("ctrl-,", Preferences, None),
        // Bound on the editor itself so they win over its own Ctrl+Enter.
        KeyBinding::new("ctrl-enter", RunCurrent, Some("QueryEditor > Input")),
        KeyBinding::new("ctrl-shift-enter", RunAll, Some("QueryEditor > Input")),
        KeyBinding::new("ctrl-enter", RunCurrent, Some("QueryEditor")),
        KeyBinding::new("ctrl-shift-enter", RunAll, Some("QueryEditor")),
        KeyBinding::new("delete", DeleteRows, Some("DataView")),
        KeyBinding::new("ctrl-tab", NextTab, ws),
        KeyBinding::new("ctrl-shift-tab", PrevTab, ws),
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-.", OpenProcessList, ws),
        KeyBinding::new("ctrl-o", OpenFile, ws),
        KeyBinding::new("ctrl-shift-s", SaveFile, Some("QueryEditor")),
    ]);
}
