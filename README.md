# Quarry

An open-source, native database client written in Rust with [GPUI](https://gpui.rs). Its
layout, colors, shortcuts and workflows follow TablePlus for Windows, but it has no
accounts, no license keys and no paywalls. Every database feature is also available to
AI agents through a tool API and an MCP server.

## Status

| Area | What works |
| --- | --- |
| Databases | PostgreSQL, MySQL, MariaDB, SQLite. The PostgreSQL, MySQL and SQLite drivers pass live tests against real servers. |
| Connections | Saved connections and groups, search, duplicate, delete, status color, environment tag, safe mode, SSL modes, bootstrap commands, passwords in the OS keychain or asked each time, **Over SSH** (password or key, with host-key checking) |
| Workspace | Several connections in one window, a capsule title bar, a status pill, Items/Queries/History sidebar, preview tabs, row detail pane, console log, database switcher (Ctrl+K), Open Anything (Ctrl+P), process list (Ctrl+.) |
| Data | Data grid with sorting, a filter bar (all 22 operators, "Any column", "Raw SQL"), paging, row count, inline editing, add/duplicate/delete rows, Set NULL, follow foreign keys, staged changes colored yellow/green/red, Preview SQL, Commit (Ctrl+S), Discard, export to CSV/JSON/SQL, CSV import |
| Structure | Columns, indexes and foreign keys; add, edit and drop columns and indexes as staged ALTER statements; DDL view |
| SQL editor | Syntax highlighting, autocomplete (keywords, tables, columns, `alias.column`), run current statement or all (Ctrl+Enter / Ctrl+Shift+Enter), automatic LIMIT, Beautify/Uglify, a result tab per statement, messages and errors, export, save to the Queries list or to a `.sql` file, open `.sql` files |
| Safety | Five safe-mode levels, as in the original. Silent sends queries directly. Alert asks before sending. Safe requires typing the connection name. The "except read" variants skip the prompt for SELECT/SHOW/EXPLAIN. |
| Other | Light and dark themes, preferences, backup and restore (pg_dump/mysqldump/psql/mysql from PATH, SQLite built in) |
| AI assistant | A chat pane per connection (Ctrl+L) that reads the schema, views, functions and data, answers questions, writes and explains SQL, and optimizes queries from their plans. Runs on Claude Code, Codex, the Anthropic API or the OpenAI API (Preferences → AI). SQL in replies opens in the editor with one click; the SQL editor's **Ask AI** menu explains, optimizes or fixes the current query. Changes need your approval. |
| AI tools | 24 JSON tools covering connections, schema, reading, searching, query plans, SQL, row edits, structure edits, import, history and saved queries, plus the `quarry-mcp` stdio server, which works on the app's open connections while it runs |

The roadmap at the end lists what TablePlus has that Quarry does not have yet.

## Install

Get the matching download from the [latest release](https://github.com/Dipendra-creator/quarry/releases/latest).
`SHA256SUMS.txt` covers every package. Both the desktop app and the MCP server are included.

| Platform | CPU | Packages | Build baseline |
| --- | --- | --- | --- |
| Windows | x64 (Intel/AMD) | Installer EXE, portable ZIP | Windows 10 1809+ |
| Windows | ARM64 | Installer EXE, portable ZIP | Windows 11 |
| macOS | Intel x64 | DMG, zipped Quarry.app | macOS 12+ deployment target; tested on macOS 15 |
| macOS | Apple silicon ARM64 | DMG, zipped Quarry.app | macOS 12+ deployment target; tested on macOS 15 |
| Linux | x64 | DEB, tar.gz | Ubuntu 22.04 / glibc 2.35 |
| Linux | ARM64 | DEB, tar.gz | Ubuntu 22.04 / glibc 2.35 |

**Windows:** run `QuarrySetup-<version>-<arch>.exe`. It installs for the current user
without admin rights, or for all users if selected. It adds Start menu and optional
desktop shortcuts and an uninstaller. For portable use, extract the ZIP and run
`quarry.exe`. Uninstalling keeps saved connections and passwords.

**macOS:** open the matching DMG and drag Quarry to Applications, or extract the app
ZIP. These builds are ad hoc signed and **not Apple notarized**. If macOS blocks a
trusted download, use System Settings → Privacy & Security → Open Anyway
([Apple's instructions](https://support.apple.com/en-us/102445)).
The MCP executable is `/Applications/Quarry.app/Contents/MacOS/quarry-mcp`.

**Linux:** on Ubuntu/Debian-compatible systems, install the matching DEB with apt:

```sh
sudo apt install ./Quarry-0.1.2-linux-x64.deb
```

Or extract the tarball and run its `quarry` executable. Portable builds use the host's
graphics and TLS libraries; on Ubuntu 22.04, install `libfontconfig1 libfreetype6
libssl3 libvulkan1 libwayland-client0 libx11-xcb1 libxkbcommon-x11-0` if needed.
The DEB declares its shared-library dependencies automatically. A working X11 or
Wayland session and Vulkan-capable graphics driver are required.

To build the installer yourself, install [Inno Setup 6](https://jrsoftware.org/isinfo.php)
(`winget install JRSoftware.InnoSetup`) and run:

```powershell
packaging\windows\build-installer.ps1   # → target\dist\installer\QuarrySetup-<version>-x64.exe
```

The installer build links the C runtime statically, so target machines need no Visual
C++ Redistributable. It requires Windows 10 1809 or later.

## Build and run

Requirements: a stable Rust toolchain (edition 2024; tested with 1.98). On Windows you also
need the MSVC build tools. On Linux, install the libraries GPUI needs: X11/Wayland,
xkbcommon, fontconfig and Vulkan.

```sh
cargo run -p quarry-app --release          # the desktop app
cargo build -p quarry-ai --release         # target/release/quarry-mcp(.exe), the MCP server
cargo test                                 # unit and SQLite end-to-end tests
```

The first build compiles GPUI and takes several minutes.

Settings live in `%APPDATA%\Quarry` on Windows, `~/Library/Application Support/Quarry` on
macOS and `~/.config/Quarry` on Linux: `connections.json`, `queries.json`,
`history.jsonl`, `settings.json` and `known_hosts`. Set `QUARRY_CONFIG_DIR` to use
another folder. Passwords never go into these files. They are stored in Windows
Credential Manager, the macOS Keychain or the Linux kernel keyring, under the service
name `quarry`.

## Keyboard shortcuts

| Keys | Action |
| --- | --- |
| Ctrl+N | Connection list |
| Ctrl+P | Open anything (tables, views, functions, saved queries) |
| Ctrl+K | Switch database |
| Ctrl+E / Ctrl+T | New SQL editor tab |
| Ctrl+Enter / Ctrl+Shift+Enter | Run current statement or selection / run all |
| Ctrl+S | Commit staged changes (in an SQL tab: save to Queries) |
| Ctrl+Shift+P | Preview the SQL of staged changes |
| Ctrl+Shift+Delete | Discard staged changes |
| Ctrl+I / Ctrl+D / Delete | Insert row / duplicate row / delete selected row |
| Ctrl+F | Filter bar |
| Ctrl+R / F5 | Reload |
| Ctrl+Shift+R | Reconnect |
| Shift+F5 | Disconnect |
| Ctrl+W, Ctrl+Tab, Ctrl+Shift+Tab | Close, next and previous tab |
| Ctrl+Space | Toggle the row detail pane |
| Ctrl+Alt+C | Toggle the console log |
| Ctrl+0 | Toggle the sidebar |
| Ctrl+. | Processes (cancel/kill) |
| Ctrl+L | AI assistant |
| Ctrl+O / Ctrl+Shift+S | Open a `.sql` file / save the editor to a file |
| Ctrl+, | Preferences |

## AI access

Everything the UI does goes through `quarry_core::session::Session`. The `quarry-ai`
crate exposes the same operations as tools with JSON schemas. The in-app assistant uses
them directly, and `quarry-mcp` serves them over the Model Context Protocol. To register the
server with Claude Code (Preferences → AI shows this command for your install):

```sh
claude mcp add quarry -- "C:\Program Files\Quarry\quarry-mcp.exe"
```

While the app is open, `quarry-mcp` works on the app's open connections, and every change
an agent makes waits for your approval in the app (Preferences → AI → AI changes: Ask
first, Allow or Read only). Without the app it opens saved connections itself, and treats
any connection in a safe mode other than Silent as read-only. See [docs/AI.md](docs/AI.md)
for the backends, the tool list and the write policy.

## Tests

`cargo test` runs the unit tests and the SQLite end-to-end tests. The live tests for the
server drivers run only when you point them at real servers:

```sh
docker run -d --rm --name quarry-test-pg -e POSTGRES_PASSWORD=quarry -p 127.0.0.1:55432:5432 postgres:17
docker run -d --rm --name quarry-test-mysql -e MYSQL_ROOT_PASSWORD=quarry -e MYSQL_DATABASE=shop -p 127.0.0.1:53306:3306 mysql:8.4
QUARRY_TEST_PG=127.0.0.1:55432:postgres:quarry \
QUARRY_TEST_MYSQL=127.0.0.1:53306:root:quarry \
cargo test -p quarry-core --test live
```

`QUARRY_TEST_SSH` and `QUARRY_TEST_PG_VIA_SSH` add the SSH tunnel test (see
`crates/quarry-core/tests/live.rs`).

## Releasing

The [Release workflow](.github/workflows/release.yml) builds and tests all six desktop
targets on pushes to main. It publishes all packages together when a version tag is pushed.
Update the workspace version in `Cargo.toml`, run `cargo check` to refresh `Cargo.lock`,
and commit the changes before tagging. For example, after updating the version to
`0.1.3`:

```sh
git tag -a v0.1.3 -m "Quarry v0.1.3"
git push origin main
git push origin v0.1.3
```

The workflow checks the tag against the workspace version, runs the unit and SQLite
tests, verifies native executables and MCP startup, and publishes the 12 packages
with SHA-256 checksums only after all six targets pass. A branch run builds without
publishing; a tag run publishes. No extra secrets are required for these unsigned/ad
hoc signed packages. See [docs/RELEASING.md](docs/RELEASING.md) for native packaging
commands and the signing status.

## Project layout

```
crates/quarry-core   UI-free engine: drivers, sessions, SQL generation, staged edits,
                     structure edits, import/export, SSH tunnels, storage
crates/quarry-ai     Tool registry (JSON schemas) and the quarry-mcp server
crates/quarry-app    The GPUI desktop app
docs/                Architecture and AI notes
```

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains how the pieces fit together and how
to add a database driver.

## Roadmap

Features TablePlus has that Quarry does not have yet:

- Drivers: SQL Server, Redis, MongoDB, ClickHouse, DuckDB, CockroachDB, Redshift,
  Greenplum, Vertica, Cassandra, Snowflake, BigQuery, libSQL/Turso, Cloudflare D1,
  DynamoDB, Elasticsearch
- Metrics board, ER diagram, a chart tab for results, user and privilege management
- Query parameters and variables, JSON viewer and quick look, map view
- Create Table and Clone Table dialogs (today: SQL templates from the sidebar "+" menu)
- Import of SQL dumps and JSON through dialogs (today: open a `.sql` file and run it)
- Customizable keymap, plugins, more UI languages

## License

MIT. See [LICENSE](LICENSE). Quarry is not affiliated with TablePlus. It uses its own
name and icons (Lucide), and none of TablePlus's artwork.
