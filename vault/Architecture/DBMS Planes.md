# DBMS Planes

## The Concept

Each supported database type has a **plane** — a module that owns everything database-specific. The shell routes all `\` commands through the active plane. This is what makes the shell feel native for each DBMS.

The plane is a plugin boundary. MySQL users get MySQL commands. Postgres users get psql commands. A user connecting to a Postgres instance will never see `\u` (MySQL's USE equivalent) — they'll see `\c dbname` (psql's equivalent).

## Trait Design (sketch)

```rust
pub enum MetaCmdResult {
    RunQuery(String),           // translate to SQL and execute
    RunQueryVertical(String),   // same but \G display
    ClientAction(ClientAction), // UI-side action (disconnect, etc.)
    CustomOutput(Vec<String>),  // pre-formatted text output
    Unknown,                    // not a meta-command for this plane
}

pub enum ClientAction {
    Disconnect,
    UseDatabase(String),
    ClearInput,
    // ...
}

pub trait DbmsPlane: Send + Sync {
    fn dbms_name(&self) -> &'static str;
    fn handle_meta_cmd(&self, cmd: &str, args: &str) -> MetaCmdResult;
    fn status_query(&self) -> &'static str;
    fn help_lines(&self) -> Vec<&'static str>;
    // Future:
    // fn complete(&self, partial: &str, schema: &SchemaCache) -> Vec<String>;
    // fn format_error(&self, code: u32, msg: &str) -> String;
    // fn translate_type(&self, val: &Value) -> String;
}
```

## MySQL Plane — Commands

Currently implemented directly in `tyto/src/main.rs`. Will move into a proper plane struct.

| Command | Alias | Action |
|---------|-------|--------|
| `\q` | `quit`, `exit` | Disconnect |
| `\h` | `\?`, `help` | Show help |
| `\s` | `status` | Server status (`\G` display) |
| `\u dbname` | `use dbname` | `USE dbname` |
| `\G` | — | Execute vertical (terminator) |
| `\e` | `edit` | Edit the buffer (or previous command) in the external editor |
| `\c` | `clear` | Clear current input (coming soon) |
| `\r` | — | Reconnect (coming soon) |
| `\W` | `warnings` | Show warnings after each stmt (coming soon) |
| `\!` | `system` | Shell passthrough (deferred — needs discussion) |

## Postgres Plane — Commands (planned)

When Postgres support is added, the plane would handle all standard psql backslash commands:

| Command | Description |
|---------|-------------|
| `\l` | List databases |
| `\c dbname` | Connect to database |
| `\dt` | List tables |
| `\dt schema.*` | List tables in schema |
| `\d tablename` | Describe table |
| `\di` | List indexes |
| `\dv` | List views |
| `\df` | List functions |
| `\dn` | List schemas |
| `\du` | List roles/users |
| `\x` | Toggle expanded (vertical) output |
| `\timing` | Toggle query timing display |
| `\pset` | Output format settings |
| `\i filename` | Execute SQL file |
| `\e [line]` | Edit the buffer (or previous query) in the external editor, at a line |
| `\q` | Quit |

The psql plane would translate `\dt` into `SELECT tablename FROM pg_tables WHERE schemaname = 'public'`, etc.

## Architecture Placement

Currently: meta-command logic lives in `tyto/src/main.rs` as free functions (`mysql_meta_cmd`, `mysql_help_lines`, `mysql_status_sql`).

Migration path:
1. Define `DbmsPlane` trait in `shared`
2. Implement `MySqlPlane` in `noctua` (server-side) AND `tyto` (client-side)
   - Server side: SQL translation lives here
   - Client side: UI actions (disconnect, clear, etc.) live here
3. OR: keep all plane logic client-side; backend is DBMS-agnostic relay
   - This is cleaner — the backend just runs SQL strings; the plane lives in the frontend
   - Backend doesn't need to know what `\dt` means

**Recommendation**: Keep planes client-side (in `tyto`). The backend is a credential holder and connection relay, not a SQL interpreter. The plane translates meta-commands to SQL *before* sending `ExecuteQuery`. This keeps the backend simple and planes fully pluggable in WASM.

## Open Questions

- Where do planes live in the open source / commercial split? Probably open, as plugins.
- Schema caching: the plane needs schema awareness for completions. Where does the cache live? Middle tier seems right — it holds the connection.
- How does the frontend know which plane to activate? Currently resolved client-side via `plane_for(conn_def.db_type)`; the frontend already holds the `ConnectionDef` it sent, so `SessionOpened` doesn't need a dbms_type field. If we later split the middle tier across processes, re-evaluate.

## Status (as of Postgres v0.0.1)

- `DbmsPlane` trait lives in `tyto/src/plane/mod.rs`. Two implementations:
  `MySqlPlane` (`plane/mysql.rs`) and `PostgresPlane` (`plane/postgres.rs`).
- Planes are zero-sized stateless singletons resolved via `plane_for(DbType)`.
- Backend (`noctua/src/main.rs`) holds sessions as `LiveConnection`
  — an enum over `MySqlConnection` / `PgConnection` — and dispatches
  connect/version/query/introspect through per-DBMS helpers. The backend
  is still a pure SQL relay: it does not parse or translate meta-commands.

### MySQL plane

Fully implemented. Schema explorer covers Tables, Views, Procedures,
Functions, Triggers, Events. Meta-commands: `\q \h \s \u \e \G` and the
`quit / exit / help / status / use` bare-word aliases.

### PostgreSQL plane — v0.0.1 coverage

- **Meta-commands**: `\q \h \s \l \dn \du \dt [schema] \dv [schema]
  \df [schema] \d name \e n` plus `\c dbname` (which explains that
  Postgres can't switch databases on a live connection and suggests
  creating a new connection instead). Bare-word aliases: `quit / exit
  / help / status`.
- **Schema explorer**: Tables / Views / Procedures / Functions / Triggers
  (Events is dropped — Postgres has no equivalent). All queries alias
  their output columns to match MySQL's column names (`Engine`, `Rows`,
  `Field`, `Type`, `Key_name`, …) so the existing introspect parsers
  remain dialect-agnostic.
- **Table DDL reconstruction**: not yet implemented; the DDL tab shows a
  "-- reconstruction not implemented" placeholder and directs the user
  to the Columns + Indexes sections. View / function / procedure /
  trigger DDL works via `pg_get_viewdef` / `pg_get_functiondef` /
  `pg_get_triggerdef`.
- **Syntax highlighting**: ~200-keyword `POSTGRES_KEYWORDS` + ~130-function
  `POSTGRES_FUNCTIONS` set. `#` line-comment syntax disabled for PG.
- **Column type display**: `parse_column_type` picks up PG aliases
  (`int2`/`int4`/`int8`, `float4`/`float8`, `bpchar`, `timestamptz`,
  `bytea`, `jsonb`, `uuid`, …).

### Known gaps

- `\c` doesn't actually reconnect. Post-v1 we could support cross-database
  reconnect by disconnecting the old session and connecting a new one with
  a modified `ConnectionDef` — needs a new `Reconnect` meta-action.
- Postgres DDL reconstruction for tables is a todo.
- `\x` (toggle expanded output) and `\timing` aren't wired up yet — both
  need a new `MetaAction` variant for toggling display state.
