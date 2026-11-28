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
            │  Session ── Driver (trait) ── PG / MySQL / SQL Server / SQLite / Redis / MongoDB
            │  sql: dialect, filters, splitting, auto-limit, format
            │  commands, mongo_shell: Redis and mongosh command languages
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
  - CockroachDB and YugabyteDB use the PostgreSQL driver, and TiDB the MySQL one. Each
    `DriverKind` has a `base()` engine whose dialect it shares; the drivers branch on the
    kind only where the servers differ (CockroachDB's hex session ids and own system
    schemas, TiDB's missing read-only sessions and binary default collation).
    `DriverKind::transactional_ddl` says whether a failed batch of schema changes is
    undone; where it is not, `Session::apply_structure` runs the changes one by one and
    reports which ones were made.
  - SQLite uses `rusqlite` (bundled) on `spawn_blocking`.
  - Redis uses `redis` (one multiplexed connection). Keys are items whose `ItemKind`
    is their type; a key's value is browsed as rows, filtered, sorted and paged in memory
    (`query::filter_sort_page`). Grid edits become commands applied in `MULTI`/`EXEC`.
    `commands` parses and quotes redis-cli lines and knows which commands only read.
  - MongoDB uses the official `mongodb` driver. Collections are items; documents are
    rows whose columns are sampled top-level fields with their BSON types, which type
    edited cells. The filter bar becomes a query document. `mongo_shell` parses the
    editor's mongosh subset (`db.c.find(…).sort(…)`, shell helpers, extended JSON),
    prints values back in the same syntax, splits scripts and classifies reads.
  - Engines that do not speak SQL answer `Driver::browse`, `count`, `browse_text` and
    `changes_text` themselves; the defaults return `None` and leave them to `Dialect`.
  - SQL Server uses `tiberius` on one connection behind a mutex. Batches run inside
    `BEGIN TRANSACTION … COMMIT` with `XACT_ABORT` on. Plans come from `SHOWPLAN_TEXT`
    (or `STATISTICS PROFILE` for analyze) through `Driver::explain`, because they are
    session settings rather than an `EXPLAIN` prefix. macOS builds use rustls for TLS
    (SQL Server's self-generated certificates fail Apple's TLS stack even when trusted);
    the other platforms use their native TLS.
- `Session` is an open connection plus its current database, schema and safe mode. It is
  the API both clients use: `browse` (one page, ordered by primary key by default),
  `count`, `execute`, `run_script`, `apply_changes`, `apply_structure`, `import_csv`,
  `search`, `overview`, `all_columns`, `processes` and `kill`, `switch_database`,
  `use_schema`, `reconnect`. `SessionManager` shares open sessions by connection id.
  `use_schema` calls `Driver::use_schema` (PostgreSQL sets `search_path`), and reconnects
  re-run the bootstrap commands and re-apply the schema.
- `sql::Dialect` handles quoting, literals, filter-to-WHERE translation and the SELECT and
  COUNT queries of the data view (`LIMIT … OFFSET`, or `OFFSET … FETCH` on SQL Server).
  `sql::split_for` splits scripts per engine: SQL Server scripts are cut at `GO` lines,
  and `BEGIN … END` blocks and procedure bodies are kept whole. The statement splitter understands quotes, comments
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
| (new) Self-update from GitHub releases | `update.rs` |

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

1. Add a `DriverKind` variant (label, badge, default port and user). A database that
   speaks another one's protocol only needs a `base()` engine and the branches where it
   differs. A hosted flavor with nothing to change is a `Preset` instead.
2. Implement `Driver` in `crates/quarry-core/src/driver/<name>.rs` and route it in
   `driver::connect`. A non-SQL engine also implements `browse`, `count` and
   `changes_text`, and returns `false` from `DriverKind::is_sql`.
3. If its SQL differs, adjust `Dialect`: quoting, literals, `LIMIT`, structure SQL.
4. Add a case to `tests/live.rs` gated by an environment variable.
5. Give it a badge color in `theme.rs`. The UI and the AI tools need no other change.
