# AI access

Quarry exposes every database operation as a tool with a JSON schema. Agents use them
through the `quarry-mcp` server, and an in-app assistant can call the same `Toolbox`
directly.

## Running the MCP server

```sh
cargo build -p quarry-ai --release
claude mcp add quarry -- /path/to/target/release/quarry-mcp     # Claude Code
```

Any MCP client works. The server speaks newline-delimited JSON-RPC 2.0 over stdio and
implements `initialize`, `ping`, `tools/list` and `tools/call`. It reads the same
connections file as the app (`QUARRY_CONFIG_DIR` overrides the folder) and the same
keychain entries.

## Tools

| Tool | Purpose |
| --- | --- |
| `list_connections` | Saved connections, with driver, tag, safe mode and connected state |
| `connect` / `disconnect` | Open or close a connection |
| `save_connection` | Create or update a connection; the password goes to the keychain |
| `list_databases` / `switch_database` | Databases on the server |
| `list_schemas` | Schemas (PostgreSQL) |
| `list_tables` | Tables, views, materialized views, functions, procedures and sequences |
| `describe_table` | Columns (type, nullable, default, primary key, references), indexes and foreign keys |
| `get_ddl` | CREATE statement of any item |
| `schema_overview` | Every table and view with its columns; the best first call |
| `select_rows` | Rows with filters (all data-view operators), sorting and paging |
| `count_rows` | Row count, optionally filtered |
| `search_data` | Find a text in any column of every table |
| `run_sql` | Run one or more statements (adds a LIMIT to SELECTs that have none) |
| `insert_row` / `update_row` / `delete_rows` | Row edits by primary key, with `dry_run` |
| `alter_table` | Add, drop or modify columns and indexes, with `dry_run` |
| `import_csv` | Insert CSV rows (text or file) in one transaction |
| `query_history` | Recent statements |
| `list_saved_queries` / `save_query` | The Queries tab |

Rows come back as JSON objects keyed by column name. NULL is `null`, numbers are JSON
numbers (exact decimals are strings) and binary data is a `0x…` string.

## Write policy

A connection's safe mode, set from the lock button in the title bar, also controls
tools:

- **Silent**: tools may read and write.
- **Any Alert or Safe mode**: tools are read-only. Writing SQL, row edits, `alter_table`
  and `import_csv` fail with a message that asks the user to switch the connection to
  Silent.

Marking production connections with a safe mode therefore keeps agents from changing
them. The app's own prompts still apply to people.

## Building an in-app assistant

`quarry_ai::Toolbox::new(store, sessions)` accepts the app's `SessionManager`, so an
assistant pane shares the live connections the user has open. Give the model
`Toolbox::tools()` as its tool definitions and send each tool call to
`Toolbox::call(name, args)`. Useful context to send with each request: the active
connection id, the open table (`DataView::item`) and the selected row
(`RowSnapshot`).
