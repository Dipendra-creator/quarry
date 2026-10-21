# AI access

Quarry exposes every database operation as a tool with a JSON schema (`quarry-ai`'s
`Toolbox`). Three things use those tools:

- the **AI assistant** pane in the app (Ctrl+L);
- the **`quarry-mcp`** server, for any MCP client: Claude Code, Codex, Cursor and others;
- your own code, through `Toolbox::call(name, args)`.

## The assistant

Open it with the sparkles button in the title bar or Ctrl+L. Each connection has its own
chat. The assistant reads the schema, views, functions and data through the tools, so it
can:

- answer questions about the data, running the queries it needs;
- write SQL from a description, including joins, CTEs, aggregates and window functions;
- explain a query you paste, and point out its mistakes;
- optimize a query: it reads the plan (`explain_query`, with `EXPLAIN ANALYZE` for
  read-only queries) and the indexes, then proposes a rewrite or indexes and says why;
- change data or structure, when you allow it (see the write policy below).

Every SQL block in a reply has three buttons: open it in a new SQL editor tab, open and
run it, and copy it. The SQL editor's **Ask AI** menu sends the current query (the
selection, or the statement under the cursor) to the assistant with "Explain", "Optimize"
or "Fix" (which includes the last error).

### Backends

Choose one in Preferences → AI (Ctrl+,, or the gear in the pane):

| Backend | How it runs | Needs |
| --- | --- | --- |
| Claude Code | `claude -p` in JSON streaming mode, resuming the chat's session each turn | The Claude Code CLI, signed in |
| Codex | `codex exec --json`, resuming the chat's thread each turn | The Codex CLI, signed in |
| Anthropic API | Messages API with streaming and tool use | An API key; default model `claude-opus-5-5` |
| OpenAI API | Chat Completions with streaming and tool calls | An API key; the endpoint can point at any OpenAI-compatible server |

The CLIs get Quarry's tools, and nothing else: Quarry starts them with
`quarry-mcp --attach` as their only MCP server, turns off Claude Code's built-in tools,
other MCP servers, hooks and plugins, and runs Codex with a read-only sandbox. Their
working folder is `<config dir>/assistant`. The API backends call the toolbox in-process.

API keys are kept in the OS keychain (`ai:anthropic`, `ai:openai`). Without one, Quarry
uses `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` from the environment. **Test** checks the
CLI path or the key.

## Write policy

Every statement that changes a database goes through a gate. In the app, Preferences → AI
→ **AI changes** sets it:

- **Ask first** (default): reading is free; each change shows the exact SQL in a dialog,
  and runs only if you click Allow. Deny, or closing the dialog, refuses it, and the model
  is told so.
- **Allow**: changes run without asking, except on connections in an Alert or Safe mode,
  which still ask.
- **Read only**: every change is refused.

This applies to the assistant and to every external agent attached to the app. Writes are
SQL that is not read-only (`EXPLAIN ANALYZE` of a write counts as a write), row edits,
`alter_table` and `import_csv`.

`quarry-mcp` started while the app is closed has no one to ask, so it falls back to the
connections' safe mode: **Silent** connections accept changes, every other mode makes
them read-only for tools.

## Running the MCP server

```sh
claude mcp add quarry -- "C:\Program Files\Quarry\quarry-mcp.exe"     # installed app
claude mcp add quarry -- /path/to/target/release/quarry-mcp            # from source
```

Preferences → AI shows the exact command for your install. Any MCP client works: the server
speaks newline-delimited JSON-RPC 2.0 over stdio and implements `initialize`, `ping`,
`tools/list` and `tools/call`.

- **While the app is open**, `quarry-mcp` attaches to it: tools work on the connections open
  in the app (and can open saved ones), and changes wait for your approval in the app. The
  tools that would close or repoint a connection under your open tabs (`disconnect`,
  `save_connection`, `switch_database`) are not offered then.
- **Otherwise** it opens saved connections itself, reading the same connections file
  (`QUARRY_CONFIG_DIR` overrides the folder) and keychain entries as the app.

`quarry-mcp --attach [bridge.json]` requires the app, and `quarry-mcp --standalone` never
attaches.

### How attaching works

The app listens on a random localhost port and writes the port and a random token to
`<config dir>/bridge.json`. `quarry-mcp` reads that file and forwards each tool call as one
JSON line, `{"token", "tool", "arguments"}`, getting back `{"ok": …}` or `{"error": "…"}`.
Calls without the token are refused.

## Tools

| Tool | Purpose |
| --- | --- |
| `list_connections` | Saved and open connections, with driver, tag, safe mode and connected state |
| `connect` / `disconnect` | Open or close a connection |
| `save_connection` | Create or update a connection; the password goes to the keychain |
| `list_databases` / `switch_database` | Databases on the server |
| `list_schemas` | Schemas (PostgreSQL) |
| `list_tables` | Tables, views, materialized views, functions, procedures and sequences |
| `describe_table` | Columns (type, nullable, default, primary key, references), indexes and foreign keys |
| `get_ddl` | CREATE statement of any item, including views, functions and procedures |
| `schema_overview` | Every table and view with its columns; the best first call |
| `select_rows` | Rows with filters (all data-view operators), sorting and paging |
| `count_rows` | Row count, optionally filtered |
| `search_data` | Find a text in any column of every table |
| `explain_query` | The execution plan; `analyze` adds real timings for read-only queries |
| `run_sql` | Run one or more statements (adds a LIMIT to SELECTs that have none) |
| `insert_row` / `update_row` / `delete_rows` | Row edits by primary key, with `dry_run` |
| `alter_table` | Add, drop or modify columns and indexes, with `dry_run` |
| `import_csv` | Insert CSV rows (text or file) in one transaction |
| `query_history` | Recent statements |
| `list_saved_queries` / `save_query` | The Queries tab |

Rows come back as JSON objects keyed by column name. NULL is `null`, numbers are JSON
numbers (exact decimals are strings) and binary data is a `0x…` string.

## Using the toolbox from code

`Toolbox::new(store, sessions)` takes a `SessionManager`; `with_gate` sets the write policy
(`SafeModeGate`, the default, or `ApprovalGate`, which sends each change to a channel for a
person to answer). Give a model `Toolbox::tools()` as its tool definitions and send each
call to `Toolbox::call(name, args)`. `quarry_ai::assistant::Chat` is the complete agent loop
the app uses, for all four backends.

## Tests

`cargo test -p quarry-ai` covers the tools, the approval gate, the bridge, and both API
backends against a local mock server. `tests/live_assistant.rs` runs real turns and spends
tokens, so it runs only when asked:

```sh
QUARRY_TEST_AI=claude_code QUARRY_TEST_AI_MODEL=haiku cargo test -p quarry-ai --test live_assistant
```

`QUARRY_TEST_AI` is `claude_code`, `codex`, `anthropic` or `open_ai`; `QUARRY_TEST_MYSQL`
runs it against MySQL instead of SQLite.
