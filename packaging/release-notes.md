Native database client, SQL editor and AI assistant for PostgreSQL, MySQL, MariaDB, Microsoft SQL Server and SQLite. MIT licensed.

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
