Native database client, SQL editor and AI assistant for PostgreSQL, MySQL, MariaDB, Microsoft SQL Server, SQLite, CockroachDB, YugabyteDB, TiDB, Redis and MongoDB. MIT licensed.

## What's new in 0.3.0

**Redis.** Connect to Redis, Valkey, KeyDB or Dragonfly. The sidebar lists keys by type (strings, hashes, lists, sets, sorted sets, streams), and opening a key shows its value as rows you can filter, edit, add and delete like a table. Quarry turns your edits into Redis commands, shows them in the preview, and applies them in one `MULTI`/`EXEC`. The editor runs redis-cli style commands with autocomplete for commands and key names, and shows results as grids (`HGETALL` as fields and values, `INFO` by section). Back up with `redis-cli --rdb`.

**MongoDB.** Connect to MongoDB or Atlas (paste a `mongodb+srv://` string into Host). Collections and views are in the sidebar; documents are rows, with top-level fields as columns and nested values as editable shell literals. The filter bar becomes a MongoDB query (with a raw JSON option), and edits become `insertOne`, `updateOne` and `deleteOne` that keep each field's type. The editor runs mongosh-style calls: `find` with `sort`, `limit` and `skip`, `aggregate`, `countDocuments`, `distinct`, `insertMany`, `updateMany`, `createIndex`, `db.runCommand`, `show collections`, with `ObjectId()`, `ISODate()` and the other shell helpers. Back up and restore with `mongodump` and `mongorestore`.

**CockroachDB, YugabyteDB and TiDB.** Each has its own connection type with the right port, user and database, and everything works: browsing, editing, structure changes, processes, backups. Where a server keeps the schema changes that ran before an error (CockroachDB, YugabyteDB, MySQL, MariaDB, TiDB), Quarry now applies structure edits one at a time and tells you which ones were made, instead of implying they were all undone.

**Cloud services.** Create connection has tiles for Supabase, Neon, TimescaleDB, Amazon RDS / Aurora and PlanetScale, with their ports, users, databases and SSL settings filled in.

The AI assistant and MCP agents work on all of these, in each database's own language.

**Fixes**

- Restoring a SQLite database from a `.sql` script works again (it was refused as a write in read-only mode).
- On TiDB, "case insensitive" filters and search ignore case, as they do elsewhere.

## What's new in 0.2.4

**Keyboard shortcuts guide, and your own shortcuts.** Press **F1** (or Ctrl+Alt+K, ⌥⌘K on a Mac) anywhere in Quarry to see every keyboard shortcut, grouped and searchable. It also opens from the keyboard icon in the connections window, the workspace menu, Preferences → General, and Help → Keyboard Shortcuts on macOS.

Every shortcut can be changed there: click a key and press the new one, click **+** to add another key, or **×** to remove one. If the key already belongs to another command in the same place, it moves to the one you are editing, and Quarry tells you. Changes apply at once, and tooltips, menus and hints show your keys. They are saved in `keymap.json` in Quarry's settings folder; **Reset all** brings back the defaults.

## What's new in 0.2.3

**Fixes a crash when switching a connection to Full access.** In 0.2.2, clicking **Full access** in the title bar closed Quarry (connections tagged production, which ask first, were not affected). Switching between Read only and Full access now works both ways.

## What's new in 0.2.2

**Read only / Full access toggle.** Every connection now has a toggle in the title bar, and **starts in Read only**. In read-only mode nothing can change the database: the data grid and structure view cannot be edited, the SQL editor refuses statements that are not reads (UPDATE, DELETE, DROP, CREATE, …), sidebar Import / Truncate / Delete are hidden, processes cannot be killed, and the AI assistant and MCP agents are refused too. On PostgreSQL, MySQL, MariaDB and SQLite the connection itself is also switched to read-only, so even a write hidden inside a function call is refused by the database. Switch to **Full access** to make changes; connections tagged *production* ask first.

**Fixes**

- Keyboard shortcuts work in the connections window as soon as it opens, without clicking it first.
- Running a query with Ctrl+Enter closes the autocomplete list.

## What's new in 0.2.1

**The Claude Code and Codex assistants work when Quarry is opened from the Dock or Finder.** Apps opened that way on macOS (and from Linux desktop launchers) only get the system's basic PATH, so Quarry could not find a `claude` or `codex` installed with npm, Homebrew or their installers, and the assistant would not connect. Quarry now looks up programs on your login shell's PATH and in the usual install folders, the way your terminal does. The same fix lets backup and restore find `pg_dump`, `mysqldump` and `sqlcmd` from Homebrew.

If you have 0.2.0, click **Update to 0.2.1** in the title bar, or use Check for Updates.

## What's new in 0.2.0

**Microsoft SQL Server.** SQL Server (2012 and later) and Azure SQL are now in Create connection. Browse schemas, tables, views, procedures, functions and sequences; edit rows and structure; run scripts with `GO` batches; see query plans; and use the AI assistant and MCP tools, like the other databases.

**Schema switching that works.** Pick a schema from the bottom of the sidebar (now searchable). On PostgreSQL, SQL you run then resolves unqualified names in that schema, and it stays that way after a reconnect. Before, only the sidebar changed and queries kept using `public`.

**Automatic updates.** Quarry now checks this page for new versions at launch and shows an **Update** button when one is available. It downloads the right package for your system, verifies it against `SHA256SUMS.txt`, installs it and restarts. Turn the check off in Preferences → General. Earlier versions cannot update themselves, so install 0.2.0 by hand once.

**Interface fixes**

- The row detail pane shows long values, line breaks and JSON in full, and can be dragged wider.
- On macOS, the window buttons no longer cover the toolbar, keyboard shortcuts work with ⌘ (and ⌘Q quits), monospace text uses a real monospace font, and there is a menu bar.
- The Structure tab sizes its tables to their contents instead of fixed boxes.
- Pressing Enter in the database, schema and Open Anything lists picks the highlighted entry.
- The driver search finds SQL Server as "mssql" or "microsoft", and Create always opens the driver you see.

## Downloads

| Platform | Architectures | Downloads |
| --- | --- | --- |
| Windows | x64, ARM64 | Installer EXE and portable ZIP |
| macOS | Intel x64, Apple silicon ARM64 | DMG and ZIP containing Quarry.app |
| Linux | x64, ARM64 | Debian package and portable tar.gz |

SHA256SUMS.txt covers all 12 downloads.

**Installing**

- Windows x64: Windows 10 1809 or later; Windows ARM64: Windows 11. Run the matching installer.
- macOS: macOS 12 or later. Open the DMG and drag Quarry to Applications. The app is **ad hoc signed, not Apple notarized**. If macOS blocks a trusted download, use System Settings → Privacy & Security → Open Anyway.
- Linux: built on Ubuntu 22.04 (glibc 2.35 baseline). Install the matching DEB with apt, or extract the tarball on a compatible system with the required graphics libraries. See the README for dependencies.

The MCP server is included in every package. On macOS it is inside Quarry.app/Contents/MacOS; the app's Preferences shows the exact registration command. AI backends use your CLI login or API credentials; provider charges may apply. Server backup and restore also need the corresponding database tools on PATH.
