# sqlx-mysql 0.8.6, patched for Parliament

This is the published `sqlx-mysql` 0.8.6 crate (see `.cargo_vcs_info.json`).
The workspace uses it through `[patch.crates-io]` in the root
`Cargo.toml`. The patch makes a session, and what it reports, match the
mysql CLI. Upstream sqlx differs in four ways (0.9.0 still does):

- **Rows affected.** sqlx's handshake asks for `CLIENT_FOUND_ROWS`, so
  an UPDATE reports the rows it matched. The CLI reports the rows it
  changed: `Query OK, 0 rows affected` for an UPDATE that changes
  nothing.
- **`IGNORE_SPACE`.** sqlx's handshake asks for `CLIENT_IGNORE_SPACE`,
  which adds `IGNORE_SPACE` to the session's `sql_mode` and turns
  built-in function names into reserved words. The CLI doesn't ask for it.
- **The info line and warnings.** The server's OK reply carries a
  warning count and an info string (`Rows matched: 1  Changed: 0
  Warnings: 0`, `Records: 2  Duplicates: 0  Warnings: 0`). The CLI
  prints both: `Query OK, 0 rows affected, 1 warning` and the info
  line under it. sqlx drops them.
- **The transaction status.** Every OK reply carries
  `SERVER_STATUS_IN_TRANS`. Parliament's TXN badge shows it, which
  catches `autocommit = 0` and implicit commits. sqlx keeps it
  crate-private, and doesn't record the status a ping returns, even
  though that is the only status reply after an error.

## The patch

Every change is marked `Parliament patch`:

| File | Change |
|---|---|
| `src/connection/stream.rs` | The handshake leaves out `FOUND_ROWS` and `IGNORE_SPACE` |
| `src/protocol/response/ok.rs` | `OkPacket.info`, read by `info_string`. MySQL and MariaDB write the info string length-encoded (`net_store_data`), though the protocol docs say "rest of packet". `info_string` reads the length-encoded form when the length exactly fills the rest of the packet, and raw text otherwise; it never fails. Plus tests. |
| `src/protocol/response/eof.rs` | `EofPacket.warnings` is used, so its `allow(dead_code)` goes |
| `src/query_result.rs` | `MySqlQueryResult` gains `warnings()` and `info()`. `Extend` keeps the last statement's. |
| `src/connection/executor.rs` | `run` fills them from the OK packet and the end-of-rows packet |
| `src/connection/mod.rs` | `in_transaction()` is public. `ping` records the status its OK reply carries. |

Run `diff -r ~/.cargo/registry/src/*/sqlx-mysql-0.8.6 vendor/sqlx-mysql` to
see the whole patch. Only `PARLIAMENT.md` and the files above should differ.

noctua reads the accessors in `LiveConnection::fetch` and
`LiveConnection::transaction_state`. After an error it pings
(`is_alive`) before reading the status. The contracts are pinned live by
`cargo test -p noctua -- --ignored mysql_results_and_transactions_match_the_cli mysql_session_matches_the_cli`.

## Upgrading sqlx

1. Bump `sqlx` in `noctua/Cargo.toml`. Cargo will then refuse the patch,
   because the vendored version no longer matches.
2. Copy the new published crate over this directory, from
   `~/.cargo/registry/src/*/sqlx-mysql-<version>` or the `.crate` file
   on crates.io. Keep this file.
3. Re-apply the changes in the table above, then run the live tests.

## Testing it on its own

`cargo test --manifest-path vendor/sqlx-mysql/Cargo.toml --lib` creates
a lockfile and a `target/` here. Both are git-ignored.
