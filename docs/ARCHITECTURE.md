# Architecture

Quarry has three crates. The rule that keeps them apart: **database behavior lives in
`quarry-core`**, and both the UI and the AI layer call it. Nothing that touches a
database is written in UI code, so anything a person can do in the app, an agent can do
through `quarry-ai`.

```
            ┌──────────────┐        ┌──────────────┐
            │  quarry-app  │        │  quarry-ai   │── quarry-mcp (stdio MCP server)
            │ (GPUI views) │        │ (JSON tools) │
            └──────┬───────┘        └──────┬───────┘
                   │  Arc<Session>          │
                   ▼                        ▼
            ┌─────────────────────────────────────┐
            │ quarry-core                         │
            │  Session ── Driver (trait) ── PG / MySQL / SQLite
            │  sql: dialect, filters, splitting, auto-limit, format
            │  changes: staged row edits → INSERT/UPDATE/DELETE
            │  alter: structure edits → ALTER/CREATE/DROP INDEX
            │  import / export, ssh tunnel, store (files + keychain)
            └─────────────────────────────────────┘
```

## quarry-core

- `driver::Driver` is one async trait per engine: list databases, schemas and items,
  read a table's structure, produce DDL, run one statement, run a batch atomically, and
  list a schema's columns. `driver::connect` picks the implementation.
  - PostgreSQL uses `tokio-postgres`. User SQL goes through the simple (text) protocol, so
    every value comes back exactly as the server prints it. A `prepare` beforehand
    supplies the column types. Batches go out as one multi-statement query, which
    PostgreSQL runs as a single implicit transaction.
  - MySQL and MariaDB use `mysql_async` on one connection behind a mutex. Batches run
    inside `START TRANSACTION … COMMIT`. Columns with the binary charset come back as
    bytes.
  - SQLite uses `rusqlite` (bundled) on `spawn_blocking`.
- `Session` is an open connection plus its current database, schema and safe mode. It is
  the API both clients use: `browse` (one page, ordered by primary key by default),
  `count`, `execute`, `run_script`, `apply_changes`, `apply_structure`, `import_csv`,
  `search`, `overview`, `all_columns`, `processes` and `kill`, `switch_database`,
  `reconnect`. `SessionManager` shares open sessions by connection id.
- `sql::Dialect` handles quoting, literals, filter-to-WHERE translation and the SELECT and
  COUNT queries of the data view. The statement splitter understands quotes, comments
  and `$$` bodies, and it backs "run current statement", automatic LIMIT and read-only
  detection, which safe mode uses.
- `changes` and `alter` are pure SQL generators. Previews show exactly what will run.
- `store` keeps connections, saved queries and history as JSON. Secrets go to the OS
  keychain (`keyring`): the database password, the SSH password and the SSH key
  passphrase.
- `ssh` opens a `russh` session and forwards a local port. It accepts host keys found in
  `~/.ssh/known_hosts`, remembers new keys in Quarry's own `known_hosts`, and refuses a
  key that changed.
- `runtime()` is a shared Tokio runtime for hosts that are not built on Tokio, such as
  GPUI.

## quarry-app

GPUI with `gpui-kit` components: inputs, the SQL editor, the virtual data table, menus
and dialogs. Chrome specific to TablePlus is drawn with plain GPUI elements: the capsule
title bar, segmented footers, pill tabs and driver badges.

| Original (reverse-engineered) | Quarry |
| --- | --- |
| `MainWindow` (welcome + connection tree) | `welcome.rs` |
| `ConnectionListWindow`, `Form*` | `connection_form.rs` |
| `WorkspaceWindow` (title bar capsules, workspace strip) | `workspace.rs` (`WorkspaceWindow`) |
| `WorkspaceController` (sidebar, content, detail, console) | `workspace.rs` (`ConnWorkspace`) |
| `ItemController`, `FavoriteTreeView`, `HistoryTreeView` | `sidebar.rs` |
| `TableDataController`, `TableStructureController` | `data_view.rs` + `grid.rs` |
| `CustomQuery`, `ResultController` | `query_editor.rs`, `completion.rs` |
| `RowDetailController` | `row_detail.rs` |
| `OpenAnythingWindow`, database list | `picker.rs` |
| `ProcessingListWindow` | `processes.rs` |
| `SafeModeConfirmWindow` | `guard.rs` |
| `PreferencesWindow` | `preferences.rs` |
| `DatabaseBackupWindow`, `DatabaseRestoreWindow` | `backup.rs` |
| `lightcolors.xaml` / `darkcolors.xaml` | `theme.rs` (`Palette`) |
| Window input bindings | `actions.rs` |
| (new) AI assistant pane and change approvals | `assistant.rs` |

Database calls run on the core runtime and come back into GPUI through `state::db`:

```rust
cx.spawn_in(window, async move |this, cx| {
    let result = db(async move { session.browse(&req).await }).await;
    this.update_in(cx, |view, window, cx| { /* apply result */ }).ok();
}).detach();
```

Two GPUI rules the code depends on:

- Never update an entity from inside its own update. Work that may run synchronously,
  such as `guard::guarded` with safe mode off, is deferred with `window.defer`.
- Title-bar buttons stop mouse-down propagation (see `widgets::capsule`). Otherwise the
  title bar starts a window drag and the click is lost.

## quarry-ai

- `Toolbox` turns `Session` methods into JSON tools. Every change passes a `WriteGate`:
  `SafeModeGate` (standalone `quarry-mcp`) or `ApprovalGate`, whose requests the app
  shows as dialogs (`assistant::ask_approval`).
- `bridge` serves the app's `Toolbox` on localhost (port and token in `bridge.json`), so
  `quarry-mcp` started by any MCP client works on the app's open sessions.
- `assistant::Chat` is the agent loop behind the assistant pane. The Anthropic and OpenAI
  backends (`assistant/api.rs`) stream over HTTP and call the toolbox in-process. The
  Claude Code and Codex backends (`assistant/cli.rs`) run the CLI per turn, resuming its
  session, with `quarry-mcp --attach` as its only tools.

The app owns one `Toolbox` and `ApprovalGate` in `AppState`, starts the bridge at launch,
and runs each chat turn on the core runtime, streaming `ChatEvent`s back to the pane.

## Adding a driver

1. Add a `DriverKind` variant (label, badge, default port and user).
2. Implement `Driver` in `crates/quarry-core/src/driver/<name>.rs` and route it in
   `driver::connect`.
3. If its SQL differs, adjust `Dialect`: quoting, literals, `LIMIT`, structure SQL.
4. Add a case to `tests/live.rs` gated by an environment variable.
5. Give it a badge color in `theme.rs`. The UI and the AI tools need no other change.
