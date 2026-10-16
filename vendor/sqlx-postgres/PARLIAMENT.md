# sqlx-postgres 0.8.6, patched for Parliament

This is the published `sqlx-postgres` 0.8.6 crate (upstream commit
`bab1b022bd56a64f9a08b46b36b97c5cff19d77e`, see `.cargo_vcs_info.json`).
The workspace uses it through `[patch.crates-io]` in the root
`Cargo.toml`. The patch leaves the session's time zone and date style
to the server, as psql does, and keeps three things psql shows, which
upstream sqlx reads from the server and then throws away or keeps
private (0.9.0 still does):

- **The command tag.** The server sends it in each statement's
  CommandComplete (`UPDATE 3`, `INSERT 0 1`, `CREATE TABLE`, and
  `ROLLBACK` for a COMMIT of a failed transaction). psql prints it for
  a statement that returns no rows. Upstream sqlx keeps only the count.
- **The row description.** Upstream attaches a result's columns to its
  rows, so an empty result has no columns. psql prints an empty
  result's header over `(0 rows)`.
- **The transaction status.** Every ReadyForQuery carries it: idle, in
  a transaction block, or in a failed one. psql's `%x` prompt shows it
  (libpq's `PQtransactionStatus`), and so does Parliament's TXN badge.
  Upstream keeps it crate-private.

And it leaves out what upstream sends that psql doesn't:

- **`TimeZone=UTC` and `DateStyle=ISO, MDY`** in the startup message.
  The server takes startup parameters as the session's defaults, so a
  session showed (and read) every timestamp in UTC whatever
  postgresql.conf, `ALTER ROLE` or `ALTER DATABASE` set. psql sends
  neither. Parliament reads every value as the server's text, so no
  decoder of its depends on them.

And it adds what libpq has and upstream lacks: a way to send a startup
parameter of its own (`startup_param`), as libpq sends `PGTZ` and the
rest; whether the server asked for a password when the session
opened (`password_used`, libpq's `PQconnectionUsedPassword`), which
psql's `\conninfo` shows; and a statement's result columns as psql's
`\gdesc` reads them (`describe_result_columns`: the unnamed statement
parsed and described, each column's name, type OID and type modifier —
upstream's `describe` prepares a named statement it keeps, runs
nullability and type queries, and drops the modifier).

## The patch

Every change is marked `Parliament patch`:

| File | Change |
|---|---|
| `src/message/command_complete.rs` | `CommandComplete::tag()`: the tag without its NUL, plus two test assertions |
| `src/query_result.rs` | `PgQueryResult` gains `command_tag()` and `columns()`. `Extend` keeps the last statement's tag and columns. |
| `src/connection/executor.rs` | `run` tracks the running statement's row description. A prepared statement's is known up front; in a simple query, each statement's arrives before its rows. `run` hands it, with the tag, to that statement's `PgQueryResult`. |
| `src/message/ready_for_query.rs` | `TransactionStatus` derives `Clone, Copy, PartialEq, Eq` |
| `src/connection/mod.rs` | `PgConnection::password_used()`: libpq's `PQconnectionUsedPassword`. `PgConnection::transaction_status()`. It is async: after an error, the ReadyForQuery that carries the new status is still unread, so it reads that first (no extra round trip). |
| `src/connection/establish.rs` | The startup message no longer carries `DateStyle` and `TimeZone`; `client_encoding=UTF8` stays. It carries each `startup_param`, after `options`. The authentication loop notes when the server asks for the password (cleartext, MD5, SASL) and keeps it on the connection. |
| `src/connection/executor.rs` (also) | `PgConnection::describe_result_columns(sql)`: Parse as the unnamed statement, Describe, Sync — no other query — and each RowDescription field's name, `data_type_id` and `type_modifier` (libpq's `PQprepare(conn, "", …)` + `PQdescribePrepared`, as psql's `\gdesc` uses them). |
| `src/options/mod.rs` | `PgConnectOptions::startup_param(name, value)`: a parameter of its own in the startup message, as libpq sends `PGTZ` / `PGDATESTYLE` / `PGGEQO` — `-c` in `options` would do the same, but PgBouncer and RDS Proxy refuse `options`. noctua sends those variables, and the browser's chosen time zone, this way. |
| `src/lib.rs` | `#![allow(warnings, clippy::all)]`. Path dependencies get every lint; registry crates are spared by `--cap-lints`. Also re-exports `TransactionStatus` as `PgTransactionStatus`. |

Run `diff -r ~/.cargo/registry/src/*/sqlx-postgres-0.8.6 vendor/sqlx-postgres`
to see the whole patch. Only `PARLIAMENT.md` and the files above should differ.

noctua reads the result accessors in `LiveConnection::fetch`, the status in
`LiveConnection::transaction_state`, `password_used` in
`LiveConnection::password_used` and the described columns in
`LiveConnection::describe_columns`, and opens sessions through `connect_pg`.
The contracts are pinned live by
`cargo test -p noctua -- --ignored pg_results_keep_tag_and_columns pg_transaction_state_follows_the_server pg_session_matches_psql pg_describe_matches_psql`.

## Upgrading sqlx

1. Bump `sqlx` in `noctua/Cargo.toml`. Cargo will then refuse the patch,
   because the vendored version no longer matches.
2. Copy the new published crate over this directory, from
   `~/.cargo/registry/src/*/sqlx-postgres-<version>` or the `.crate`
   file on crates.io. Keep this file.
3. Re-apply the changes in the table above, then run the live tests.

If upstream sqlx starts exposing all three, and lets the startup
parameters go, drop the patch: delete
`vendor/` and the `[patch.crates-io]` entry, and read them from sqlx
directly.

## Testing it on its own

`cargo test --manifest-path vendor/sqlx-postgres/Cargo.toml --lib` creates
a lockfile and a `target/` here. Both are git-ignored.
