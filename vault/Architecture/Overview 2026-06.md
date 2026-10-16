# Day-1 architectural overview (2026-06)

Audience: a brand-new contributor on day 1. Reads after `ARCHITECTURE.md`
(the top-level positioning) and before drilling into specific subsystems.
This doc traces the data flows end-to-end and names the conventions the
rest of the code expects you to know.

## The three-tier picture

```
Browser (Tyto / Leptos 0.6 → WASM)  ←─WebSocket─→  Noctua (Axum + sqlx, Rust)  ←─TCP─→  MySQL / PostgreSQL
       port 8080 (trunk serve)                            port 3000 localhost
```

- **Tyto** (`tyto/`, ~16k LOC) — Leptos 0.6 client-side-rendered app
  compiled to `wasm32-unknown-unknown`, shipped to the browser by Trunk in
  development. Holds zero database credentials. Talks only to Noctua over
  a single WebSocket.
- **Noctua** (`noctua/`, ~1.1k LOC) — A single Rust binary that does two
  jobs in the same process:
  1. Serves the compiled WASM bundle as static assets
  2. Hosts the WebSocket endpoint that holds live sqlx connections to the databases
- **`shared/`** — Workspace crate holding the wire types (`ClientMessage`,
  `ServerMessage`, `ConnectionDef`). Compiles for both `wasm32` and native
  so both ends agree on JSON shape.

There is **no desktop binary**. The browser is the UI host because (a)
browser isolation prevents Tyto from ever holding raw DB credentials — a
prerequisite for the eventual governed-access positioning in
`vault/Roadmap/Secure Backend Roadmap.md`; (b) WASM gives a
native-feeling reactive shell without a packaging story; (c) collapsing
the security boundary onto Noctua means future auth / audit / masking
lands in exactly one Rust process.

### Session pinning — never pooled

Each UI pane gets a **dedicated** database connection, never returned to
a pool:

```rust
enum LiveConnection { MySql(MySqlConnection), Postgres(PgConnection) }
type Sessions = Arc<Mutex<HashMap<String, Arc<Mutex<LiveConnection>>>>>;
```

A session UUID generated when `Connect` succeeds keys into that map; every
subsequent message (`ExecuteQuery`, `IntrospectQuery`, `ExecuteDml`,
`Disconnect`) carries the `session_id` so Noctua can route it back to the
same `Arc<Mutex<LiveConnection>>`. Per-session serialization (one query
at a time on a given connection) is enforced by the inner `Mutex`.

Pooling would silently wipe temp tables, open transactions, `SET`
session variables, `USE dbname`. The pinning rule is non-negotiable;
codified in `.claude/rules/noctua.md`.

To prevent one slow query from starving the WebSocket, `handle_socket`
splits into a reader + writer connected by an mpsc; round-trip messages
(`needs_spawn(...)`) dispatch onto `tokio::spawn` tasks so a long UPDATE
on session A doesn't block a COMMIT on session B.

## End-to-end query flow

User types `SELECT * FROM actor;` in a shell pane and hits Enter:

1. **Keydown capture.** The shell pane's `on:keydown` handler (registered
   in `tyto/src/main.rs`, helpers in `tyto/src/shell.rs`) detects Enter
   without Shift.
2. **Statement boundary detection.** `tyto/src/parser.rs::split_statements`
   reads the text as the plane's native CLI lexes it (`plane.lexicon()`:
   psql's quotes, `E'…'` strings, dollar quotes and nested comments, or
   mysql's backslash escapes, backticks and `#` comments), splitting at
   the delimiter (`DELIMITER //`) — and on MySQL at `\g` / `\G` — and
   keeping each statement's terminator as typed. A backslash command
   after SQL on the input's last line (`select 1 \gx`) starts at its
   first backslash outside quotes and comments (`parser::trailing_command`).
3. **Wire send.** Tyto fires `ws_send(ws, &ClientMessage::ExecuteQuery
   { session_id, sql })`. The `session_id` was minted by Noctua at
   `Connect` time and stored on the pane's `ShellTab`.
4. **Noctua routes.** The reader in `handle_socket` JSON-parses, sees
   `ExecuteQuery`, `needs_spawn` returns true, a task spawns
   `process_message`, which looks up
   `state.sessions.lock().await.get(&session_id)` →
   `Arc<Mutex<LiveConnection>>`.
5. **SQL execution.** `execute_query(conn, sql, session_id)` detects a
   row-producing prefix (`SELECT`, `WITH`, `SHOW`, `EXPLAIN`, `VALUES`,
   `TABLE …`) and calls `conn.fetch_all_strings(sql)`, which under the
   hood is `sqlx::raw_sql(sql)` against the held connection. Each cell
   goes through `mysql_value_to_string` or `pg_value_to_string`
   (per-RDBMS converters with arms for every sqlx type name; see
   `.claude/rules/noctua.md` for the VARBINARY / `BIGINT UNSIGNED` edge
   cases).
6. **Wire return.** Noctua emits `ServerMessage::QueryResult
   { session_id, columns, rows, exec_time_ms, rows_affected }`.
7. **Frontend dispatch.** Tyto's WebSocket `onmessage` handler (the giant
   match in `tyto/src/main.rs` around the `QueryResult` arm) finds the
   `ShellTab` whose `session_id` matches, then `tab.shell_history.update
   (|h| h.push(ShellEntry::Table { ... }))`. That signal write triggers
   Leptos's reactive re-render through `tyto/src/shell.rs::render_shell_entry`,
   the new result block appears at the bottom, and `scroll_shell_to_bottom`
   keeps the prompt in view.

Errors take the symmetric path: `ServerMessage::QueryError
{ session_id, message }` → the same arm pushes `ShellEntry::Error` into
history.

## End-to-end introspect flow

Structurally identical to the query path but uses a different message
pair (`IntrospectQuery` / `IntrospectResult`) and — critically — carries
a `tag` string the frontend uses to demultiplex responses. Results never
land in shell history.

User clicks a table row in Schema Explorer:

1. **Tag construction.** `tyto/src/explorer.rs` builds tag strings like
   `columns:public.actor`, `status:public.actor`,
   `indexes:public.actor`, `ddl:public.actor`,
   `relancestors:public.actor`, `reldescendants:public.actor`,
   `partitions:public.actor`, `erdcols:public.actor`,
   `data_page:public.actor`. The tag encodes the response routing —
   the SQL itself comes from the active plane
   (`plane.table_columns_sql("public", "actor")`, etc.).
2. **Wire send.** `ws_introspect(ws, &session_id, sql, tag)` sends
   `ClientMessage::IntrospectQuery { session_id, sql, tag }`.
3. **Noctua: dumb relay.** `process_message` routes to
   `execute_introspect`, which calls `conn.fetch_all_strings(sql)` and
   ships back `ServerMessage::IntrospectResult
   { session_id, tag, columns, rows }`. **Noctua does not look at the
   tag or the SQL** — it has no idea this is an introspect about
   `actor`'s columns vs. a DDL reconstruction vs. an FK ancestor walk.
   That ignorance is the design.
4. **Tyto: tag parsing.** The `IntrospectResult` arm in
   `tyto/src/main.rs` is a long ladder of `tag.strip_prefix("columns:")`,
   `strip_prefix("ddl:")`, `strip_prefix("relancestors:")`, etc., each
   splitting `rest` with `split_once('.')` to recover `(schema, name)`
   and find the right `TableDetail` to write into. The branches feed
   signals like `td.columns`, `td.indexes`, `td.ancestors`,
   `td.partitions`, `td.erd_neighbour_columns`.
5. **Row parsing — case-insensitive column lookup.** Inside each branch,
   columns are matched by name (`columns.iter().position(|c|
   c.eq_ignore_ascii_case("Field"))`) rather than by ordinal. This is
   the **column-alias contract**.

## The column-alias contract (most load-bearing convention)

The introspect parser is single-sourced. Each handler branch expects a
fixed set of column names, looked up case-insensitively. Plane SQL must
`AS`-alias output columns to match. From `.claude/rules/plane.md`:

| Handler branch | Required aliases |
|---|---|
| `schemas` | first column → schema name |
| `objects:*` | col 0 → schema, col 1 → name, remaining → `extras` |
| `status:*` | `Engine`, `Collation`, `Rows`, `Data_length`, `Index_length`, `Avg_row_length`, `Auto_increment`, `Create_time`, `Update_time`, `Comment`, `Owner` |
| `columns:*` | `Field`, `Type`, `Null`, `Default`, `Extra`, `Comment` |
| `indexes:*` | `Key_name`, `Non_unique`, `Index_type`, `Column_name`, `Seq_in_index` |
| `ddl:*` / `objddl:*` | any column whose name starts with `"Create "` |
| `relancestors:*` / `reldescendants:*` | `Depth`, `Ref_schema`, `Ref_table`, `From_columns`, `To_columns`, `Via_schema`, `Via_table` |
| `partitions:*` | `Partition_schema`, `Partition_name`, `Partition_bound` (+ optional `Partition_rows`, `Partition_data_length`) |
| `erdcols:*` | `Schema_`, `Table_`, `Field`, `Ord`, `Is_pk` |

A missing alias produces no error — `position(...)` returns `None`, the
parser silently falls back, the cell becomes empty. Postgres in
particular folds unquoted identifiers to lowercase, so `"Engine"` and
`"Rows"` must be double-quoted in PG plane SQL. **If plane SQL can't
fit the parser's shape, change the SQL — not the parser.** See
`.claude/rules/plane.md` for the full table and per-RDBMS quirks.

## The plane abstraction

`tyto/src/plane/mod.rs` defines `trait DbmsPlane` (56 methods). Per-RDBMS
implementations live alongside it:

- `tyto/src/plane/mysql.rs` — `MySqlPlane`
- `tyto/src/plane/postgres.rs` — `PostgresPlane`

Resolution is via a singleton:

```rust
static MYSQL_PLANE: MySqlPlane = MySqlPlane;
static POSTGRES_PLANE: PostgresPlane = PostgresPlane;
pub fn plane_for(t: DbType) -> &'static dyn DbmsPlane { ... }
```

A plane owns everything that varies per RDBMS:

- **Connection-form defaults** — `default_port`, `default_user`,
  `database_required`
- **Meta-command translation** — `meta_cmd(input)` converts `\dt`, `\df`,
  `\sf`, `\dnS`, etc. into `MetaAction::RunSql(...)` so the backend never
  sees the backslash form
- **Schema-explorer SQL** — `list_schemas_sql`, `list_sql`,
  `table_status_sql`, `table_columns_sql`, `table_indexes_sql`,
  `table_ddl_sql`, `table_relationship_ancestors_sql`,
  `table_relationship_descendants_sql`, `table_partitions_sql`,
  `table_columns_bulk_sql`
- **Data-tab paged read + DML** — `count_rows_sql`, `select_paged_sql`,
  `update_row_sql`, `insert_row_sql`, `delete_row_sql`, `upsert_row_sql`.
  All four DML primitives share one value-quoting contract (single
  quotes doubled; literal `"NULL"` → bare keyword; native identifier
  quoting)
- **Activity-pane SQL** — `sessions_sql`, `blockers_sql`, `locks_sql`,
  `kill_query_sql`
- **Admin-area SQL** — roles, privileges, grants/revokes,
  affected-objects walks
- **Syntax-highlighting vocabularies** — `keywords()`, `major_keywords()`,
  `set_operators()`, `functions()`, `meta_command_names()` (all sorted
  slices; the tokenizer does `binary_search`)
- **Tab-completion sources** — `completion_tables_sql`,
  `completion_columns_sql`, `default_completion_schema`
- **Lexer hints** — `lexicon()` (the native CLI's lexer: `Psql` / `Mysql`),
  `sql_dialect()` (returns a `&'static dyn sqlparser::dialect::Dialect`)
- **Safe-mode rules** — `destructive_patterns()` (prefixes that gate
  behind a confirmation in the shell)

RDBMS awareness lives **only** here. Noctua takes whatever SQL string
arrives, runs it via `sqlx::raw_sql`, and ships rows back. The
`.claude/rules/noctua.md` rule is explicit: "If you find yourself
wanting to add `match db_type { ... }` in noctua to change SQL behavior,
stop — that logic belongs in the plane." This makes the backend
swappable and adding a new RDBMS a matter of writing a new plane — no
changes to noctua, no changes to the introspect parser, no changes to
`shared/`.

## Reactive signal map

`tyto/src/main.rs::App()` is the single source of every top-level
`RwSignal`. The hot ones — meaning subscribers fan out across the entire
UI — are worth keeping in mind any time you touch render code:

| Signal | Type | Subscribers |
|---|---|---|
| `connections` | `Vec<ConnectionDef>` | Sidebar render, every conn-item, conn-form, Recents resolver |
| `shell_panes` | `HashMap<String, ShellTab>` | Every pane render, every pane-tab in the tab bar, the Active Connections section, Notification badge |
| `workspace_tabs` | `Vec<WorkspaceTab>` | Workspace-tab strip, per-workspace pane-tree render, the active-tab visibility CSS |
| `active_pane_id` | `Option<String>` | Focused-pane styling, keyboard routing, the "active connection" indicator |
| `active_wt_id` | `Option<String>` | Workspace-tab activation, `display:flex` vs `display:none` |
| `dialog` | `Option<DialogKind>` | The modal renderer |
| `completion_popup` | `Option<CompletionPopup>` | Tab-completion popup |
| `ws`, `ws_status` | `Option<WebSocket>`, `WsStatus` | Every `ws_send` callsite, connection-status indicator, reconnect logic |

A signal change ripples through **every** subscriber in subscription
order. The failure mode to watch for: a reactive closure that subscribes
to one signal (say `leaf.tabs`) but reads — without subscribing — from
another (say `leaf.active`) sees stale state when both are updated
sequentially. That bug shipped once already (commit 6df1366); the lesson
is to read both signals inside the same reactive block when their state
must be consistent, or restructure into a derived `Memo` over the pair.

`ShellTab` and `TableDetail` carry their own per-pane / per-table
signals (`shell_history`, `current_input`, `session_id`, `session_stale`,
`data_selected_row`, `schema_selected_row`, `erd_zoom`,
`erd_generations`, `erd_keys_only`, `erd_neighbour_columns`, ...). The
pattern: top-level signals fan to every pane; per-pane signals fan only
within that pane's subtree. That separation is what keeps Leptos's
reactive cost bounded as the user opens dozens of panes.

## What is deliberately NOT there

Equally important as what the codebase contains is what it refuses:

- **No backend RDBMS awareness.** Noctua never branches on `db_type` to
  alter SQL behavior. All meta-command translation, identifier quoting,
  value escaping, and catalog-query phrasing happen client-side in
  `tyto/src/plane/`. The on-the-wire SQL is final.
- **No client-side semantic SQL parsing for execution.** `parser.rs::
  split_statements` only finds statement boundaries (it understands
  quotes, comments, `DELIMITER`, dollar-quotes, mysql's `\g` / `\G`). It does not parse the
  statement. The DB is the parser of record for what the SQL means.
  `sqlparser-rs` is pulled in for the on-the-fly beautifier and for
  future ergonomic features (safe-mode AST inspection, etc.), not for
  execution.
- **No GUI feature that holds a transaction across user think-time.**
  Data-tab UPDATE / INSERT / DELETE / UPSERT are all single-statement
  and auto-commit. Multi-row / multi-table atomicity is what the shell
  pane is for — a DBA types `BEGIN; … COMMIT;` themselves and owns the
  boundary. From `CLAUDE.md`: "any feature that lets [a transaction]
  span [user think-time] creates think-time inside it — the textbook
  long-running-transaction incident pattern". When evaluating future
  GUI features, ask "does this introduce think-time inside a
  transaction?" — if yes, redesign or drop.
- **No auto-trigger completion.** Tab completion fires only on the Tab
  key (psql-style), never on every keystroke. By design — DBAs don't
  want a popup whenever they type a dot.
- **No desktop binary.** Noctua serves the WASM bundle; the browser
  hosts the UI. No Electron, no native shell.
- **No pooled DB connections per user.** Strict session pinning: one
  `LiveConnection` per pane, held until `Disconnect`. Pooling silently
  breaks temp tables, transactions, and session variables.
- **No pre-commit credential scanner.** Credentials live in
  `~/.parliament/secrets.env` (untracked, sourced from `~/.zshrc`); the
  env-var pattern removed the temptation.

## File references for orientation

When you're trying to find the code behind a specific feature:

| Looking for | Look in |
|---|---|
| Top-level positioning + repo layout | `ARCHITECTURE.md`, `README.md` |
| Operating guidance + design principles | `CLAUDE.md` |
| Wire types (`ClientMessage` / `ServerMessage` / `ConnectionDef`) | `shared/src/lib.rs` |
| WebSocket lifecycle, session map, value converters | `noctua/src/main.rs` (single file) |
| App top-level signals, WS `onmessage` match, `IntrospectResult` parser | `tyto/src/main.rs` |
| `DbmsPlane` trait surface + `plane_for(DbType)` | `tyto/src/plane/mod.rs` |
| Per-RDBMS plane implementations | `tyto/src/plane/postgres.rs`, `mysql.rs` |
| Statement splitter (no AST — boundaries only) | `tyto/src/parser.rs` |
| Per-pane rendering | `tyto/src/shell.rs`, `explorer.rs`, `activity.rs` |
| Tab completion (analyzer + candidate gathering) | `tyto/src/completion.rs` |
| SQL beautifier (River / Block / Inline) | `tyto/src/format_sql.rs` |
| Column-alias contract + per-RDBMS quirks | `.claude/rules/plane.md` |
| Backend RDBMS-agnostic relay rule + `value_to_string` edge cases | `.claude/rules/noctua.md` |
| WASM check, Leptos signal idioms, CSS-shorthand trap | `.claude/rules/tyto.md` |
| Deeper design notes | `vault/Architecture/` |
| Strategic future direction | `vault/Roadmap/Secure Backend Roadmap.md` |

## Audit reference

This overview was extracted from the 2026-06 critical audit
(`vault/Engineering/Audit Findings 2026-06.md`). The findings document
also identified concrete drift between the implementation and several
existing vault pages — particularly the CodeMirror-shaped future
described in `vault/Features/SQL Completion.md`, `Syntax Highlighting.md`,
and `SQL Beautifier.md`, which describe an architecture that doesn't
match what shipped. This overview reflects the as-built shape; treat
it as the canonical day-1 starting point until a follow-up overview
replaces it.
