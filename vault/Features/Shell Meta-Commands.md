# Shell Meta-Commands

## Implemented (MySQL)

| Command | Alias | Status | Notes |
|---------|-------|--------|-------|
| `\q` | `quit`, `exit` | ✅ Done | Disconnects session |
| `\h` | `\?`, `help` | ✅ Done | Shows command list |
| `\s` | `status` | ✅ Done | Server status as vertical display |
| `\u dbname` | `use dbname` | ✅ Done | Translates to `USE dbname` |
| `\G` | — | ✅ Done | Vertical result terminator |

## Next Up

### `\e` — external editor (built)

The early idea here — `\e N` loads history entry N into the textarea — shipped for a while and was replaced on 2026-10-01 by the native behaviour: `\e` opens the query buffer in the user's real editor (noctua launches it; see [[Settings]]), `\e N` is a **line number** as in psql, MySQL's `\e` takes no arguments and `edit` is its long form. Recalling an older entry for editing will come back as a block-menu action in the shell rework (psql has no command for it). Current behaviour: CLAUDE.md's `\e` bullet.

### `\c` — Clear current input

MySQL's `\c` (not to be confused with psql's `\c dbname`) clears the in-progress input statement. In MySQL multi-line:
```
mysql> SELECT *
    -> FROM users
    -> \c
mysql>
```
Simple to implement: just clear `current_input` and reset height. No history entry.

### `\r [db [host]]` — Reconnect

Reconnect to the server, optionally switching database and/or host. Useful when a session drops.

Implementation: send `Disconnect` then `Connect` with optionally modified connection def.

## Future / Deferred

### `\W` / `\w` — Warnings on/off

After each statement, show warnings if any exist. Implement by appending `SHOW WARNINGS` after every DML and displaying the result inline.

### `\!` — System shell passthrough

`\! ls -la` — passes the command to the OS shell. This works in the native `mysql` CLI because it's a terminal process.

In our browser context this is more complex:
- The WASM frontend can't shell out
- Option: forward `\!` to the backend via a new WebSocket message type, backend runs the command (sandboxed), returns output
- Security surface is significant — needs careful thought
- Deferred until there's a clear use case driving it

### `\T` / `\t` — Tee / notee (log to file)

In native MySQL: `\T /tmp/query.log` appends all output to a file. In browser context, "file" means download. Possible implementation: buffer all shell output since `\T`, then on `\t` offer a file download.

### `\P pager` / `\n` — Pager support

In native MySQL, `\P less` pipes output through `less`. In browser context: pagination could mean virtual scrolling of large result sets. Already partially addressed by the scrolling shell surface.

### `\R prompt` — Change prompt format

Let the user customize the prompt string. Current: `N SQL>`. Real MySQL default: `mysql>`. Could allow things like `\u@\h [\d]>` (user@host [database]).

## Postgres Plane (planned)

When the Postgres plane is implemented, psql commands will be handled by the `PostgresPlane`. See [[DBMS Planes]] for the full command list.
