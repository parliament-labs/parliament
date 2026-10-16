---
description: Rules for Noctua (Axum backend) — connection lifecycle, DBMS-agnostic relay contract, sqlx edge cases
paths:
  - noctua/**
---

# Noctua rules (`noctua/**`)

Axum 0.7 + sqlx 0.8 backend. Holds persistent DB connections; serves compiled WASM assets; relays `ExecuteQuery` messages over WebSocket.

## DBMS-agnostic relay contract

**The backend does not parse, translate, or dialect-branch on SQL.** It runs whatever SQL the frontend hands it and ships rows back. All dialect awareness lives in `tyto/src/plane/`. Meta-commands (`\dt`, `\u`, etc.) are translated to SQL **client-side** before being sent over the wire.

If you find yourself wanting to add `match db_type { ... }` in noctua to change SQL behavior, stop — that logic belongs in the plane.

## Session pinning — one connection per session

Each WebSocket session maps to exactly **one** `LiveConnection` (enum over `MySqlConnection` / `PgConnection`), held as `Arc<Mutex<...>>`. **Never pooled.** Required for:

- `TEMPORARY TABLE` (scoped to connection)
- `TRANSACTION` state (BEGIN / COMMIT / ROLLBACK)
- `SET @variable` (session variables)
- `USE dbname` (affects the connection)
- Any MySQL/PG session state

Dropping or replacing the connection mid-session silently wipes all of the above.

## Value rendering edge cases (per-DBMS) — `values.rs`

Both drivers: **no per-type decoding.** Queries go through `sqlx::raw_sql`, so every value arrives as the server's text — what the native CLI prints. The converters pass it through; don't reintroduce `try_get::<T>` arms: sqlx's typed decoders refuse whole families (PG numeric / uuid / json / arrays, MySQL DECIMAL / JSON → the old `?`) and re-render the rest unlike the CLI. Contracts (live, `#[ignore]`): `cargo test -p noctua -- --ignored pg_values_render_like_psql mysql_values_render_like_the_cli` — add a row when a new type matters.

MySQL (`mysql_value`, bytes via `try_get_unchecked::<&[u8]>` — sqlx keeps the raw accessor crate-private):
- **Binary that is UTF-8 shows as text** — `information_schema` stores `ENGINE`, `TABLE_COMMENT`, column `Type`, `Default` as VARBINARY with text collation. Only bytes that aren't UTF-8 become `<binary NB>`.
- **BIT(n)** shows its integer value (Workbench convention), not the raw bits the CLI prints.

Postgres (`pg_value`):
- Text format is the type's own output function under the session's TimeZone / DateStyle / bytea_output — numeric, money, uuid, json, ranges, geometric, arrays, enums, domains, composites and extension types all render as in psql with nothing to maintain. A binary-format value (prepared statements only; noctua issues none) shows as `<binary TYPE>`.

## Sessions as the native CLI gets them

A session should mean the same as one the native CLI opens: same time zone, `sql_mode`, collation — otherwise the same SQL returns different results. sqlx alters sessions by default:

- **MySQL**: open every connection through `connect_mysql`. It turns off sqlx's `time_zone = '+00:00'`, `PIPES_AS_CONCAT` / `NO_ENGINE_SUBSTITUTION` additions and `SET NAMES … COLLATE utf8mb4_unicode_ci`, then sends `SET NAMES utf8mb4` (the server's default collation — what the CLI gets on MySQL; on MariaDB 11.4.5+ it can differ). The patched sqlx-mysql's handshake leaves out `CLIENT_IGNORE_SPACE` and `CLIENT_FOUND_ROWS`, as the CLI does: `sql_mode` is the server's, an UPDATE's rows-affected is rows changed. Contract: `cargo test -p noctua -- --ignored mysql_session_matches_the_cli`. The setup overrides character-set changes a server's `init_connect` made; a server that refuses it keeps the session with sqlx's settings (logged).
- **Postgres**: open every connection through `connect_pg`. Upstream sqlx sends `TimeZone=UTC` and `DateStyle=ISO, MDY` in the startup message (sqlx issue #4098), which the server would take as the session's own defaults; the patched sqlx-postgres leaves them out, and `connect_pg` drops sqlx's `extra_float_digits=2`, so the session has the server's settings, as psql's does — timestamps print, and are read, in the server's zone and style. Contract: `cargo test -p noctua -- --ignored pg_session_matches_psql`. On top, libpq's environment settings — `PGTZ`, `PGDATESTYLE`, `PGGEQO` in noctua's environment, read as libpq reads them (`pg_session_settings`; unset or `default` sends nothing) — and, above `PGTZ`, the time zone the browser chose (`Connect.timezone`, Settings › Time zone): sent as libpq sends them — each a parameter of its own in the startup message (`connect_pg_with`, the patched sqlx's `startup_param`), not `-c` in `options`, which PgBouncer and RDS Proxy refuse — source `client`, so they beat `ALTER ROLE` / `ALTER DATABASE`. (sqlx already reads `PGOPTIONS`.) The live contract checks each against psql run with the same variables.

## Patched sqlx drivers (`vendor/`)

The workspace builds sqlx-postgres and sqlx-mysql 0.8.6 from `vendor/` (`[patch.crates-io]` in the root `Cargo.toml`), patched so a session and what it reports match the native CLI. Upstream throws away, or keeps private, what the CLIs show:
- **sqlx-postgres:** each statement's command tag and an empty result's row description (`PgQueryResult::command_tag()` / `columns()`), the transaction status (`PgConnection::transaction_status()` — async: after an error it reads the ReadyForQuery still in flight first), and a statement's result columns as psql's `\gdesc` reads them (`PgConnection::describe_result_columns()`: the unnamed statement parsed and described, nothing else — each column's name, type OID and type modifier, which upstream's `PgColumn` drops; upstream's `describe` also leaves a named statement and runs nullability / type queries).
- **sqlx-mysql:** the warning count and info line (`MySqlQueryResult::warnings()` / `info()`), the in-transaction flag (`MySqlConnection::in_transaction()`, which `ping` keeps current — an error reply carries no status), and a handshake without `CLIENT_FOUND_ROWS` / `CLIENT_IGNORE_SPACE`.

Change nothing else there — each `vendor/*/PARLIAMENT.md` lists every patched line and how to re-apply them when sqlx is upgraded (the patch must match the `sqlx` version, or Cargo refuses it). Contracts: `cargo test -p noctua -- --ignored pg_results_keep_tag_and_columns pg_transaction_state_follows_the_server pg_describe_matches_psql mysql_results_and_transactions_match_the_cli mysql_session_matches_the_cli`.

## ConnectionDef serde contract (shared with tyto)

New fields on `ConnectionDef` need `#[serde(default)]`. The on-disk format (`~/.sqlclient/connections.json`) must keep deserializing across versions. No breaking changes without migration logic.

It holds passwords: write it only through `save_connections_to` — atomic, 0600, via `settings::write_atomic_0600` — and `make_private` makes one others can read 0600 when noctua starts.

## Lock-wait / concurrency

A prior incident (Apr 9): concurrent sessions on the same DB hitting row locks caused the activity pane's lock-wait display to hang. Fixed by separating the monitoring query path from the session's own connection — the monitoring queries open their own transient connections, not borrow the session's.

When touching session lifetime or connection handling: the session's `Arc<Mutex<LiveConnection>>` must not be held across `.await` points that could block on DB-side locks, or the monitoring UI goes dark.

## WebSocket message shape

Canonical types in `shared/src/lib.rs` — read the enums there; this is only the core. Client → server includes `ListConnections`, `SaveConnection`, `Connect`, `Disconnect`, `ExecuteQuery`, `IntrospectQuery`, `IntrospectBatch`, `ExecuteDml`, `DescribeQuery`. Server → client includes `ConnectionList`, `SessionOpened`, `SessionError`, `SessionClosed`, `QueryResult`, `QueryError`, `Notice`, `TransactionStatus`, `IntrospectResult`, `IntrospectError`, `IntrospectBatchResult`. `IntrospectBatch` runs its queries as one simple-query message on the pinned session (`LiveConnection::fetch_result_sets`): one round trip, a `ResultSet` per query with SQL NULL as `None` (`values::pg_value` / `mysql_value`; the `*_to_string` forms write "NULL"), stop at the first error, which comes back as a `DbError` (the server's fields, or just a message). One statement per query — the set count is checked. `QueryResult` (from `LiveConnection::fetch`) carries the last statement's `status: CommandStatus` — Postgres's command tag, MySQL's warnings and info line — and, on Postgres, an empty result's columns; `types` gives each column's type as the server identifies it (Postgres: the type OID, which sqlx knows even for types it can't name; MySQL: sqlx's type name), for the plane to align by. Every reply on a live session is preceded by `TransactionStatus { state }` (`finalize_session_response` → `LiveConnection::transaction_state`, read after the error ping on MySQL): any command can change it — a failing introspect batch inside the user's transaction, DDL's implicit commit — so add new session-bound handlers through `finalize_session_response` too.

Errors: `QueryError` / `DescribeError` / `IntrospectError` / `SessionError` carry the server's fields (`error: Option<DbError>`, from `db_error::fields`), as does a batch's `BatchResult.error`; every error `message` noctua sends is `db_error::message` — the server's primary message, or sqlx's text for anything else (a lost connection) — never sqlx's "error returned from database:" Display. A `QueryError` or `IntrospectBatchResult` whose session died with it (`finalize_session_response` then sends `SessionClosed`) has `connection_lost: true`, so a client can carry on as its CLI does (mysql: reconnect and run it again; psql: reset at once). Live contracts: `cargo test -p noctua -- --ignored error_fields_contract`.

`SessionOpened` also carries what the client knows and the server doesn't, for the CLI's own output: `password_used` and `ssl_library` (psql's `\conninfo`), and `numeric_locale` — the C library's numeric conventions for the locale noctua's environment names (`LC_ALL` / `LC_NUMERIC` / `LANG`), as psql's `setlocale(LC_ALL, "")` + `localeconv()` read its own (`locale.rs`: `newlocale` + `uselocale` on one thread, so the process locale stays C; read once). noctua sends them raw; what psql makes of them (`\pset numericlocale`, `setDecimalLocale`'s fallbacks) is the PG plane's.

`IntrospectResult` carries a `tag` string (`"schemas"`, `"objects:tables"`, `"status:<t>"`, `"columns:<t>"`, `"vm:<t>"`, `"autovac:<t>"`, …) + raw rows; the parser is client-side — see the tyto / plane rules.

## Server notices

`notices.rs` streams PG notices for shell queries: sqlx has no notice callback but emits each NoticeResponse as a `tracing` event (target `sqlx::postgres::notice`, `message` field) from inside the task awaiting the query. `handle_socket` wraps only `ExecuteQuery` tasks in `notices::forward(sink, …)` (a tokio task-local); the `NoticeLayer`, installed with its own `Targets` filter so `RUST_LOG` output is unchanged, sends `ServerMessage::Notice` into the socket's outgoing channel. Same channel as the result, so notices always arrive first. The layer depends on sqlx internals — `cargo test -p noctua -- --ignored sqlx_notice_contract` checks them against a live PG after any sqlx upgrade. `init_tracing` also forces sqlx's statement log (`sqlx::query`) and its notice log off in the log output (`log_filter`), whatever `RUST_LOG` says — statement text can carry a password, notices can carry data. Don't log SQL text or result data from noctua either.

## External editor and Settings (the one OS-process exception)

`editor/` opens the user's editor for `\e` / `\ef` / `\ev` — the only process noctua spawns, acceptable because in solo-local mode noctua runs on the user's machine as the user. Rules that keep it safe:

- **No command crosses the wire.** `EditExternally` / `TestEditor` carry only closed enums (`EditorVar`, `LineFlagVar`) and a connection id; `editor/resolve.rs` (pure, unit-tested) picks the editor from noctua's own files: connection override → `settings.json` → the env chain the plane named → `vi` on macOS.
- **The WebSocket accepts only `ALLOWED_ORIGINS`** (same list as CORS) **or a missing `Origin`** (browsers always send one, so that's non-browser tools on this machine). That check is what keeps another local page from saving an editor command and running it.
- Never run an editor through a shell. Direct launches get null stdin/stdout, stderr to a file, `setsid`; terminal launches run the generated `run.sh` (every word shell-quoted) and report through the atomic `done` marker (`editor/wrapper.rs`).
- Editor tasks live in their own `JoinSet`, aborted when the socket closes; a direct editor gets SIGTERM, a terminal editor keeps its files and its script cleans up when it exits. No recursive deletes in generated shell — named files + `rmdir`.

`settings.rs` keeps `~/.sqlclient/settings.json`: atomic 0600 writes, never overwrite an unreadable file (move it to `.broken-<ts>` on save), refuse to save over a newer `version`.

## Dev command

```bash
cargo run -p noctua   # port 3000 by default
```

Manual restart required after backend edits (no hot-reload).
