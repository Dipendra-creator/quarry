//! Keyboard actions, their default shortcuts, and the user's own shortcuts.
//!
//! Every configurable shortcut is one entry of [`SHORTCUTS`]. The user's
//! changes live in `keymap.json` in the config dir (shortcut id → keys) and
//! override the defaults. Applying them rebuilds the whole keymap from the
//! component library's own bindings plus Quarry's, so nothing stale stays bound.
//! On macOS every default Ctrl shortcut also works with Cmd.

use std::collections::BTreeMap;

use gpui_kit::{Action, App, BorrowAppContext as _, Global, KeyBinding, KeyBindingContextPredicate, Keystroke};
use serde::{Deserialize, Serialize};

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
        ShowShortcuts,
    ]
);

/// One configurable shortcut.
pub struct Shortcut {
    /// Stable key in `keymap.json`.
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub group: &'static str,
    /// Where it applies: `None` everywhere, else a key context.
    pub contexts: &'static [Option<&'static str>],
    /// Default keys in Ctrl form; macOS adds the Cmd twin.
    defaults: &'static [&'static str],
}

const WS: &[Option<&str>] = &[Some("Workspace")];
const DATA: &[Option<&str>] = &[Some("DataView")];
const APP: &[Option<&str>] = &[None];
// Bound on the editor itself too, so they win over the editor's own Ctrl+Enter.
const EDITOR_RUN: &[Option<&str>] = &[Some("QueryEditor > Input"), Some("QueryEditor")];

macro_rules! shortcut {
    ($id:literal, $name:literal, $desc:literal, $group:literal, $ctx:expr, [$($key:literal),*]) => {
        Shortcut { id: $id, name: $name, description: $desc, group: $group, contexts: $ctx, defaults: &[$($key),*] }
    };
}

pub const GROUPS: [&str; 4] = ["General", "Workspace", "Table data", "SQL editor"];

pub const SHORTCUTS: &[Shortcut] = &[
    shortcut!("open_connections", "Connections", "Show the list of saved connections", "General", APP, ["ctrl-n"]),
    shortcut!("preferences", "Preferences", "Open Preferences", "General", APP, ["ctrl-,"]),
    shortcut!("show_shortcuts", "Keyboard shortcuts", "Show this list of shortcuts", "General", APP, ["f1", "ctrl-alt-k"]),
    shortcut!("check_for_updates", "Check for updates", "Look for a newer version of Quarry", "General", APP, []),
    shortcut!("close_window", "Close window", "Close the connections, preferences or form window", "General", APP, ["ctrl-w"]),
    shortcut!("quit", "Quit", "Quit Quarry", "General", APP, ["ctrl-q"]),
    shortcut!("open_anything", "Open anything", "Find a table, view, function or saved query", "Workspace", WS, ["ctrl-p"]),
    shortcut!("database_list", "Switch database", "Show the databases on the server", "Workspace", WS, ["ctrl-k"]),
    shortcut!("new_query", "New SQL editor", "Open an SQL editor tab", "Workspace", WS, ["ctrl-e", "ctrl-t"]),
    shortcut!("open_file", "Open SQL file", "Open a .sql file in an editor tab", "Workspace", WS, ["ctrl-o"]),
    shortcut!("close_tab", "Close tab", "Close the current tab", "Workspace", WS, ["ctrl-w"]),
    shortcut!("next_tab", "Next tab", "Go to the next tab", "Workspace", WS, ["ctrl-tab"]),
    shortcut!("prev_tab", "Previous tab", "Go to the previous tab", "Workspace", WS, ["ctrl-shift-tab"]),
    shortcut!("toggle_sidebar", "Toggle sidebar", "Show or hide the items sidebar", "Workspace", WS, ["ctrl-0"]),
    shortcut!("toggle_detail", "Toggle row detail", "Show or hide the row detail pane", "Workspace", WS, ["ctrl-space"]),
    shortcut!("toggle_console", "Toggle console", "Show or hide the log of queries sent", "Workspace", WS, ["ctrl-alt-c"]),
    shortcut!("toggle_assistant", "AI assistant", "Show or hide the AI assistant", "Workspace", WS, ["ctrl-l"]),
    shortcut!("process_list", "Processes", "Show the server's sessions and running queries", "Workspace", WS, ["ctrl-."]),
    shortcut!("reload", "Reload", "Reload the current table", "Workspace", WS, ["ctrl-r", "f5"]),
    shortcut!("reconnect", "Reconnect", "Open a fresh connection to the server", "Workspace", WS, ["ctrl-shift-r"]),
    shortcut!("disconnect", "Disconnect", "Close this connection", "Workspace", WS, ["shift-f5"]),
    shortcut!("insert_row", "Insert row", "Add a row (or a column, in Structure)", "Table data", WS, ["ctrl-i"]),
    shortcut!("duplicate_row", "Duplicate row", "Copy the selected row into a new one", "Table data", DATA, ["ctrl-d"]),
    shortcut!("delete_rows", "Delete row", "Mark the selected row for deletion", "Table data", DATA, ["delete"]),
    shortcut!("show_filter", "Filter", "Show the filter bar", "Table data", DATA, ["ctrl-f"]),
    shortcut!("commit", "Commit / Save", "Commit staged changes; in an SQL editor, save the query", "Table data", WS, ["ctrl-s"]),
    shortcut!("preview_changes", "Preview changes", "Show the SQL of staged changes", "Table data", WS, ["ctrl-shift-p"]),
    shortcut!("discard_changes", "Discard changes", "Drop staged changes", "Table data", WS, ["ctrl-shift-delete", "ctrl-shift-backspace"]),
    shortcut!("run_current", "Run current", "Run the selection, or the statement at the cursor", "SQL editor", EDITOR_RUN, ["ctrl-enter"]),
    shortcut!("run_all", "Run all", "Run every statement in the editor", "SQL editor", EDITOR_RUN, ["ctrl-shift-enter"]),
    shortcut!("save_file", "Save as file", "Save the editor to a .sql file", "SQL editor", &[Some("QueryEditor")], ["ctrl-shift-s"]),
];

const MAC: bool = cfg!(target_os = "macos");

/// Built-in keys of text fields and the SQL editor (the component library's
/// own), listed for reference.
pub const BUILT_IN: &[(&str, &str)] = &[
    ("Undo", if MAC { "cmd-z" } else { "ctrl-z" }),
    ("Redo", if MAC { "cmd-shift-z" } else { "ctrl-y" }),
    ("Cut", if MAC { "cmd-x" } else { "ctrl-x" }),
    ("Copy", if MAC { "cmd-c" } else { "ctrl-c" }),
    ("Paste", if MAC { "cmd-v" } else { "ctrl-v" }),
    ("Select all", if MAC { "cmd-a" } else { "ctrl-a" }),
    ("Find in the SQL editor", if MAC { "cmd-f" } else { "ctrl-f" }),
    ("Previous word (→ for the next)", if MAC { "alt-left" } else { "ctrl-left" }),
    ("New line in the AI chat", "shift-enter"),
];

fn action(id: &str) -> Option<Box<dyn Action>> {
    Some(match id {
        "open_connections" => Box::new(OpenConnections),
        "preferences" => Box::new(Preferences),
        "show_shortcuts" => Box::new(ShowShortcuts),
        "check_for_updates" => Box::new(CheckForUpdates),
        "close_window" => Box::new(CloseWindow),
        "quit" => Box::new(Quit),
        "open_anything" => Box::new(OpenAnything),
        "database_list" => Box::new(DatabaseList),
        "new_query" => Box::new(NewQueryTab),
        "open_file" => Box::new(OpenFile),
        "close_tab" => Box::new(CloseTab),
        "next_tab" => Box::new(NextTab),
        "prev_tab" => Box::new(PrevTab),
        "toggle_sidebar" => Box::new(ToggleSidebar),
        "toggle_detail" => Box::new(ToggleDetail),
        "toggle_console" => Box::new(ToggleConsole),
        "toggle_assistant" => Box::new(ToggleAssistant),
        "process_list" => Box::new(OpenProcessList),
        "reload" => Box::new(Reload),
        "reconnect" => Box::new(Reconnect),
        "disconnect" => Box::new(Disconnect),
        "insert_row" => Box::new(InsertRow),
        "duplicate_row" => Box::new(DuplicateRow),
        "delete_rows" => Box::new(DeleteRows),
        "show_filter" => Box::new(ShowFilter),
        "commit" => Box::new(Commit),
        "preview_changes" => Box::new(PreviewChanges),
        "discard_changes" => Box::new(DiscardChanges),
        "run_current" => Box::new(RunCurrent),
        "run_all" => Box::new(RunAll),
        "save_file" => Box::new(SaveFile),
        _ => return None,
    })
}

pub fn shortcut(id: &str) -> Option<&'static Shortcut> {
    SHORTCUTS.iter().find(|s| s.id == id)
}

/// Platform defaults: on macOS each Ctrl shortcut gets a Cmd twin, listed
/// first. Ctrl+Tab and Ctrl+Space keep Control there, where Cmd+Tab and
/// Cmd+Space belong to the system.
pub fn default_keys(s: &Shortcut) -> Vec<String> {
    let mut out = Vec::new();
    for key in s.defaults {
        if cfg!(target_os = "macos") && key.contains("ctrl-") && !key.ends_with("tab") && !key.ends_with("space") {
            out.push(key.replace("ctrl-", "cmd-"));
        }
        out.push(key.to_string());
    }
    out
}

// ---- the user's shortcuts ------------------------------------------------------

#[derive(Default, Serialize, Deserialize)]
struct KeymapFile {
    /// Shortcut id → keys; an empty list unbinds it.
    #[serde(default)]
    shortcuts: BTreeMap<String, Vec<String>>,
}

fn keymap_path() -> std::path::PathBuf {
    quarry_core::store::config_dir().join("keymap.json")
}

/// The component library's bindings (taken before Quarry adds its own) and
/// the user's overrides.
pub struct Keymap {
    base: Vec<KeyBinding>,
    overrides: BTreeMap<String, Vec<String>>,
}

impl Global for Keymap {}

/// Loads `keymap.json` and binds every shortcut. Call after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    let base: Vec<KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
    let overrides = std::fs::read_to_string(keymap_path())
        .ok()
        .and_then(|s| serde_json::from_str::<KeymapFile>(&s).ok())
        .map(|f| f.shortcuts)
        .unwrap_or_default();
    cx.set_global(Keymap { base, overrides });
    apply(cx);
}

/// Current keys of a shortcut: the user's, else the defaults.
pub fn keys(id: &str, cx: &App) -> Vec<String> {
    if let Some(keys) = cx.try_global::<Keymap>().and_then(|k| k.overrides.get(id)) {
        return keys.clone();
    }
    shortcut(id).map(default_keys).unwrap_or_default()
}

pub fn is_customized(id: &str, cx: &App) -> bool {
    cx.try_global::<Keymap>().is_some_and(|k| k.overrides.contains_key(id))
}

/// Sets a shortcut's keys (the defaults again drop the override), saves and applies.
pub fn set_keys(id: &str, keys: Vec<String>, cx: &mut App) -> anyhow::Result<()> {
    let defaults = shortcut(id).map(default_keys).unwrap_or_default();
    cx.update_global::<Keymap, _>(|k, _| {
        if keys == defaults {
            k.overrides.remove(id);
        } else {
            k.overrides.insert(id.to_string(), keys);
        }
    });
    save(cx)?;
    apply(cx);
    Ok(())
}

pub fn reset_all(cx: &mut App) -> anyhow::Result<()> {
    cx.update_global::<Keymap, _>(|k, _| k.overrides.clear());
    save(cx)?;
    apply(cx);
    Ok(())
}

fn save(cx: &App) -> anyhow::Result<()> {
    let file = KeymapFile { shortcuts: cx.global::<Keymap>().overrides.clone() };
    let path = keymap_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&file)?)?;
    Ok(())
}

pub fn keymap_file() -> std::path::PathBuf {
    keymap_path()
}

/// Rebuilds the keymap: the library's bindings, then every shortcut's keys.
fn apply(cx: &mut App) {
    let mut bindings = cx.global::<Keymap>().base.clone();
    for s in SHORTCUTS {
        for key in keys(s.id, cx) {
            for context in s.contexts {
                let Some(action) = action(s.id) else { continue };
                let predicate = context.and_then(|c| KeyBindingContextPredicate::parse(c).ok()).map(Into::into);
                match KeyBinding::load(&key, action, predicate, false, None, &gpui_kit::DummyKeyboardMapper) {
                    Ok(b) => bindings.push(b),
                    Err(e) => eprintln!("keymap.json: ignoring {key:?} for {}: {e}", s.id),
                }
            }
        }
    }
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
    // The macOS menu bar shows each item's key.
    crate::set_menus(cx);
}

/// A key in GPUI's own spelling ("shift-cmd-p" is "cmd-shift-p"), so keys
/// compare equal however they were written.
pub fn normalize(key: &str) -> String {
    key.split_whitespace()
        .map(|stroke| Keystroke::parse(stroke).map(|k| k.unparse()).unwrap_or_else(|_| stroke.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn uses(s: &Shortcut, key: &str, cx: &App) -> bool {
    keys(s.id, cx).iter().any(|k| normalize(k) == key)
}

/// Other shortcuts using `key` in the same place as `id`: one key can do
/// only one of them.
pub fn conflicts(id: &str, key: &str, cx: &App) -> Vec<&'static Shortcut> {
    let Some(me) = shortcut(id) else { return vec![] };
    let key = normalize(key);
    SHORTCUTS
        .iter()
        .filter(|s| s.id != id && s.contexts.iter().any(|c| me.contexts.contains(c)))
        .filter(|s| uses(s, &key, cx))
        .collect()
}

/// Other shortcuts using `key` elsewhere, such as Ctrl+W closing a tab in a
/// connection window and the window everywhere else. Both keep working.
pub fn overlaps(id: &str, key: &str, cx: &App) -> Vec<&'static Shortcut> {
    let Some(me) = shortcut(id) else { return vec![] };
    let key = normalize(key);
    SHORTCUTS
        .iter()
        .filter(|s| s.id != id && !s.contexts.iter().any(|c| me.contexts.contains(c)))
        .filter(|s| uses(s, &key, cx))
        .collect()
}

/// How deep in the window a shortcut applies; the deeper one wins a key.
pub fn depth(s: &Shortcut) -> usize {
    s.contexts
        .iter()
        .map(|c| match c {
            None => 0,
            Some("Workspace") => 1,
            Some(c) => 2 + c.matches('>').count(),
        })
        .max()
        .unwrap_or(0)
}

/// Why a recorded key cannot be a shortcut, if it cannot: plain keys would
/// stop people typing them.
pub fn invalid_reason(key: &str) -> Option<&'static str> {
    let Some(Ok(k)) = key.split_whitespace().last().map(Keystroke::parse) else {
        return Some("It is not a key Quarry knows.");
    };
    let m = k.modifiers;
    let function_key = k.key.len() > 1 && k.key.starts_with('f') && k.key[1..].chars().all(|c| c.is_ascii_digit());
    if m.control || m.alt || m.platform || m.function || function_key || k.key == "delete" {
        None
    } else {
        Some(if cfg!(target_os = "macos") {
            "Use ⌘, ⌃ or ⌥ with the key (or a function key), so typing is not affected."
        } else {
            "Use Ctrl or Alt with the key (or a function key), so typing is not affected."
        })
    }
}

// ---- labels ----------------------------------------------------------------------

/// A key as people read it: "cmd-shift-p" is "⌘⇧P" on macOS and "ctrl-shift-p"
/// is "Ctrl+Shift+P" elsewhere. Sequences ("ctrl-k ctrl-s") keep their space.
pub fn display_key(key: &str) -> String {
    key.split_whitespace().map(display_stroke).collect::<Vec<_>>().join(" ")
}

fn display_stroke(stroke: &str) -> String {
    let mac = cfg!(target_os = "macos");
    let Ok(k) = Keystroke::parse(stroke) else { return stroke.to_string() };
    let m = k.modifiers;
    let has = |name: &str| match name {
        "ctrl" => m.control,
        "alt" => m.alt,
        "shift" => m.shift,
        "fn" => m.function,
        _ => false,
    };
    let platform = m.platform;
    let key = match k.key.as_str() {
        "enter" => if mac { "↩" } else { "Enter" }.to_string(),
        "backspace" => if mac { "⌫" } else { "Backspace" }.to_string(),
        "delete" => if mac { "⌦" } else { "Delete" }.to_string(),
        "escape" => if mac { "⎋" } else { "Esc" }.to_string(),
        "tab" => if mac { "⇥" } else { "Tab" }.to_string(),
        "space" => "Space".to_string(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "pageup" => "PgUp".into(),
        "pagedown" => "PgDn".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        other => other.to_uppercase(),
    };
    if mac {
        let mut s = String::new();
        for (m, sym) in [("ctrl", "⌃"), ("alt", "⌥"), ("shift", "⇧")] {
            if has(m) {
                s.push_str(sym);
            }
        }
        if platform {
            s.push('⌘');
        }
        if has("fn") {
            s.insert_str(0, "fn ");
        }
        s + &key
    } else {
        let mut parts: Vec<String> = Vec::new();
        for (m, name) in [("ctrl", "Ctrl"), ("alt", "Alt"), ("shift", "Shift"), ("fn", "Fn")] {
            if has(m) {
                parts.push(name.into());
            }
        }
        if platform {
            parts.insert(0, if cfg!(windows) { "Win".into() } else { "Super".into() });
        }
        parts.push(key);
        parts.join("+")
    }
}

/// The first key of a shortcut, as people read it, or "" when unbound.
pub fn label(id: &str, cx: &App) -> String {
    keys(id, cx).first().map(|k| display_key(k)).unwrap_or_default()
}

/// "Name (⌘P)", or just "Name" when the shortcut has no key.
pub fn with_key(text: &str, id: &str, cx: &App) -> String {
    match label(id, cx) {
        l if l.is_empty() => text.to_string(),
        l => format!("{text} ({l})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn every_shortcut_has_an_action_and_parses() {
        for s in SHORTCUTS {
            assert!(action(s.id).is_some(), "{} has no action", s.id);
            assert!(GROUPS.contains(&s.group), "{} has an unknown group", s.id);
            for key in default_keys(s) {
                assert!(gpui_kit::Keystroke::parse(key.split_whitespace().next().unwrap()).is_ok(), "{key}");
            }
        }
        let mut ids: Vec<&str> = SHORTCUTS.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), SHORTCUTS.len(), "ids are unique");
    }

    #[::core::prelude::v1::test]
    fn labels() {
        if cfg!(target_os = "macos") {
            assert_eq!(display_key("cmd-shift-p"), "⇧⌘P");
            assert_eq!(display_key("ctrl-tab"), "⌃⇥");
            assert_eq!(display_key("shift-f5"), "⇧F5");
            assert_eq!(display_key("cmd-,"), "⌘,");
        } else {
            assert_eq!(display_key("ctrl-shift-p"), "Ctrl+Shift+P");
            assert_eq!(display_key("ctrl-alt-c"), "Ctrl+Alt+C");
            assert_eq!(display_key("shift-f5"), "Shift+F5");
            assert_eq!(display_key("ctrl-enter"), "Ctrl+Enter");
        }
        assert_eq!(display_key("ctrl--").chars().last(), Some('-'));
    }

    #[::core::prelude::v1::test]
    fn plain_keys_are_refused() {
        assert!(invalid_reason("a").is_some());
        assert!(invalid_reason("shift-a").is_some());
        assert!(invalid_reason("space").is_some());
        assert!(invalid_reason("ctrl-a").is_none());
        assert!(invalid_reason("cmd-shift-k").is_none());
        assert!(invalid_reason("f2").is_none());
        assert!(invalid_reason("shift-f5").is_none());
        assert!(invalid_reason("delete").is_none());
    }

    #[::core::prelude::v1::test]
    fn mac_gets_cmd_twins() {
        let open = shortcut("open_anything").unwrap();
        let keys = default_keys(open);
        if cfg!(target_os = "macos") {
            assert_eq!(keys, vec!["cmd-p", "ctrl-p"]);
            assert_eq!(default_keys(shortcut("next_tab").unwrap()), vec!["ctrl-tab"]);
        } else {
            assert_eq!(keys, vec!["ctrl-p"]);
        }
    }
}
