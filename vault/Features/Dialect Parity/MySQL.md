# MySQL — Dialect Parity Inventory

## Summary

Benchmarks against the `mysql` command-line client shipped with MySQL 8.0+ (compatible with 5.7 where explicitly noted). Covers the meta-command and SQL feature surface a seasoned MySQL DBA expects when they sit down at a shell — what's implemented, what's pending, and what's explicitly blocked by the browser boundary or by design.

**Interview complete.** Both axes walked:

- **Axis 1** — meta-commands — nine categories walked across Rounds 1–9. ~180 rows catalogued.
- **Axis 2** — SQL dialect features — vendor-specific surface identified across Round 10. ~40 rows consolidated into a single keyword/function-list audit work item plus two new UX hooks.

**Tier totals across both axes:**

| Tier | Count | Meaning |
|:---:|:---:|---|
| T1 | ~130 | Implemented today, or trivial SQL passthrough. |
| T2 | ~34 | Pending infrastructure (shell polish, confirmation gates, persistent history, UX hooks, file picker plumbing, structured renderers, etc.). |
| T3 | ~10 | Individual engineering items — each needs its own design pass. Includes the Plan Viewer pane, Permissions Explorer pane, tab completion, `help 'SQL_STATEMENT'`, LOAD DATA LOCAL INFILE spike. |
| BL | ~6 | Blocked by the browser sandbox, will lift when [[OS Provider Seam]] implementations ship. |
| BD | ~4 | Blocked by design — not planned for implementation. OS shell escape, external pager, arbitrary binary execution, raw TCP. (External editor moved out of BD on 2026-10-01 — see `edit` / `\e`.) |
| Out of scope / legacy | ~12 | Explicit skips documented per row; aggregated in [the Out of scope section](#out-of-scope) at the bottom of this doc. |

**Framework:** [[Dialect Coverage Framework]]. Tier scheme, runtime grace contract (`NotYetImplemented` / `NotSupported`), and inventory doc shape all defined there. This doc follows that shape.

**Variants are NOT unified here.** MariaDB, Percona Server, Aurora MySQL, and other MySQL-family engines get their own inventory docs when we formally support them. See [[README|the parity README]] for the "no cross-dialect borrowing" policy and the broader family policy.

## How to read this doc

- Sections under **Meta-commands (Axis 1)** are ordered by the canonical category list from the framework (Metadata → Session Control → Output Formatting → History & Editing → OS Escape → Help & Discovery → Scripting → File I/O → Monitoring & Replication → Query Planning → Admin → Auth). The interview that populated them ran the categories in a different order; each section notes the round that walked it.
- Sections under **SQL dialect features (Axis 2)** are ordered by SQL layer (DQL / DML / DDL / DCL / TCL / Session-admin / Procedural).
- Every row has `Command`, `Aliases`, `Status`, `Tier`, and `Notes`. The framework's tier definitions are canonical; specific per-row caveats live in the notes.
- **Browser limitations** and **Out of scope** sections at the bottom consolidate every BL / BD / OOS row across every category into one scannable table each.
- Every `[[link]]` is a wiki-style reference to another vault doc. Verified consistent as of the consolidation pass.

## Implementation tracks

Cross-cutting work streams identified during the interview. Each is a single design doc and single implementation pass when the implementation phase starts — not ~60 separate tickets. Tracks are deliberately orthogonal so one track's work doesn't block another's:

### Track 1 — Shell rendering polish (6 items)

Shared `render_entry` infrastructure + result-table rendering path. One pass lights up all six:

1. Long-text result rendering (Round 4) — detect single-column multi-line results, render as full-width monospace. Lights up `EXPLAIN FORMAT=TREE`, `EXPLAIN ANALYZE`, `SHOW CREATE TABLE` (already usable, improved), and other long-text content.
2. Wide-result auto-vertical rendering (Round 5) — detect >~15 columns, auto-switch to vertical `\G`-style with an explicit toggle shortcut. Lights up `SHOW REPLICA STATUS`.
3. "Save result as..." download action (Round 6) — row-action on result tables triggering a browser download as CSV / TSV / JSON / Markdown / SQL INSERTs.
4. `INTO OUTFILE` / `INTO DUMPFILE` advisory passthrough (Round 6) — one-line hint explaining the server-side filesystem boundary.
5. `SHOW GRANTS` structured renderer (Round 8) — break each grant-statement row into fields (user · privs · object scope · grant-option flag).
6. Runtime `\output {table|vertical|csv|tsv|json|markdown|html}` toggle (Round 9) — parity-plus feature on top of the save-as action.

### Track 2 — Shell-layer SQL intercept (6 items)

Shared "shell observes SQL flow before / during / after submit" pipeline. Plane-driven hooks. One intercept layer covers all six:

1. `\W` / `warnings` auto-show toggle (Round 2) — post-query hook that runs `SHOW WARNINGS` when `warning_count > 0`.
2. Advisory passthrough pattern (Round 6) — `passthrough + informational hint` used by `INTO OUTFILE` and similar "works but may not be what you mean" commands.
3. `\safe-mode` confirm-then-run gate (Round 7) — per-session toggle that intercepts destructive SQL patterns from `DbmsPlane::destructive_patterns()` and requires explicit `yes` confirmation. New `MetaAction::ConfirmThenRun { sql, reason }` variant.
4. `\help <client-command>` per-command detail (Round 9) — extension of `\h` via new `DbmsPlane::command_help(name)` method.
5. Transaction-state indicator (Round 10) — a visible in-transaction indicator in the shell toolbar. *Shipped 2026-10-10: it reads the server's `SERVER_STATUS_IN_TRANS`, so `autocommit = 0` and implicit commits show correctly.*
6. Schema-cache invalidation on DDL (Round 10) — detect `CREATE` / `ALTER` / `DROP` / `RENAME` / `TRUNCATE` in this shell and invalidate the Schema Explorer's cache. Explicitly best-effort; out-of-band changes from other connections are not detected.

### Track 3 — `BrowserOsProvider` consumers (5 items)

All unblock when [[OS Provider Seam]] Phase 1 lands. File picker plumbing + browser download API:

1. `SOURCE filename` / `\. filename` (Round 3) — multi-statement parser + file picker. First Phase-1 parity item per the seam doc.
2. "Save result as..." download action (Round 6) — already listed in Track 1; shares the download API.
3. `\T` tee (Round 6) — shell-output capture buffered, flushed as download (`\t` notee already says `Outfile disabled.`, as the CLI; it closes the capture once `\T` lands).
4. Drag-and-drop file attach (Round 6, deprioritized) — drag `.sql` → SOURCE; drag `.csv` → prefill a `LOAD DATA LOCAL INFILE` template.
5. `LOAD DATA LOCAL INFILE` (Round 6, **deferred — spike required**) — three-part implementation gated on sqlx upstream work. Explicitly deferred from v0.0.1.

### Track 4 — Cross-plane shell features (5 items)

Features that benefit every plane, not just MySQL. Listed under MySQL parity because that's the benchmark; implementation is plane-agnostic:

1. Persistent history across sessions (Round 9) — `localStorage` keyed by connection-id + DB.
2. Ctrl+R reverse history search (Round 9) — inline search overlay.
3. Readline keybindings bundle (Round 9) — `Ctrl+A` / `Ctrl+E` / `Ctrl+W` / `Ctrl+U` / `Ctrl+K` / `Alt+B` / `Alt+F` / `Ctrl+P` / `Ctrl+N`. T2 per binding.
4. Tab completion (Round 9) — T3 with its own vault design doc (`vault/Features/Tab Completion.md`) when implementation starts. Command-only completion is easy; schema-aware completion needs metadata + context.
5. `\help <cmd>` detail strings (Round 9) — cross-plane via `DbmsPlane::command_help(name)` method.

### Track 5 — New pane types (3 items)

Each a distinct product surface; each gets its own vault design doc before implementation:

1. **Plan Viewer pane** (Round 4) — three views (Raw JSON / Readable Text / Graphical Tree) sharing a parsed-plan model. Hosts `EXPLAIN FORMAT=JSON` and the optimizer trace flow. Biggest T3 item from the interview. `vault/Features/Plan Viewer.md` TBD.
2. **Permissions Explorer pane** (Round 8, deferred) — "who has access to what" visualization. Intrinsic value across all deployment modes. `vault/Features/Permissions Explorer.md` TBD.
3. **Activity-pane Replication sub-view** (Round 5, deferred) — structured `SHOW REPLICA STATUS` / `SHOW REPLICAS` view. Deferred indefinitely unless driven by user demand; crowded competitive landscape.

### Track 6 — Keyword / function list audit (1 consolidated item)

Single work item: "audit + expand `MYSQL_KEYWORDS` and `MYSQL_FUNCTIONS` in `tyto/src/plane/mysql.rs` against the MySQL 8.4 reference manual." Covers all ~40 T1 polish items from Round 10's Axis 2 walk — vendor-specific clauses, newer 8.x surface (invisible columns, vector types, 8.4 additions), procedural keywords (IF/WHILE/HANDLER/SIGNAL/DECLARE CURSOR/etc.), JSON operators (`->` / `->>`), online-DDL hints, query-hint comment colouring (niche, separate T2).

---

## Meta-commands (Axis 1)

### Metadata & Introspection

Nearly every MySQL metadata command is a real SQL statement (`SHOW`, `DESCRIBE`), not a client-side shortcut. That means they **work today via plain SQL passthrough**: the user types the command at our shell, it goes through the normal query path, and the results render in the terminal table view.

The "T1 passthrough" tier means **no per-command work is needed** — these commands inherit whatever shell improvements we make (output formatting, truncation, cancellation, warnings handling, color). The interesting rows in this category are the handful where raw output is ugly enough that a structured pane would help, or where the command deserves a rich secondary path (like `SHOW PROCESSLIST` → Activity pane) while still being first-class in the shell.

| Command | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `SHOW DATABASES` | `SHOW SCHEMAS` | ✓ passthrough | T1 | Also used internally by Schema Explorer dropdown. |
| `SHOW TABLES [FROM db] [LIKE pat]` | — | ✓ passthrough | T1 | |
| `SHOW FULL TABLES [FROM db]` | — | ✓ passthrough | T1 | Returns `Table_type` column for view vs table disambiguation. |
| `SHOW TABLE STATUS [FROM db] [LIKE pat]` | — | ✓ passthrough | T1 | Used internally by Activity pane and Schema Explorer overview. |
| `SHOW CREATE DATABASE dbname` | `SHOW CREATE SCHEMA` | ✓ passthrough | T1 | |
| `SHOW CREATE TABLE tbl` | — | ✓ passthrough | T1 | Schema Explorer DDL tab uses this internally. |
| `SHOW CREATE VIEW v` | — | ✓ passthrough | T1 | |
| `SHOW CREATE PROCEDURE p` | — | ✓ passthrough | T1 | |
| `SHOW CREATE FUNCTION f` | — | ✓ passthrough | T1 | |
| `SHOW CREATE TRIGGER t` | — | ✓ passthrough | T1 | |
| `SHOW CREATE EVENT e` | — | ✓ passthrough | T1 | |
| `SHOW COLUMNS FROM tbl [FROM db]` | — | ✓ passthrough | T1 | |
| `SHOW FULL COLUMNS FROM tbl [FROM db]` | — | ✓ passthrough | T1 | Includes collation, privileges, comment. |
| `DESCRIBE tbl` | `DESC tbl` | ✓ passthrough | T1 | Alias for `SHOW COLUMNS` — real SQL in MySQL, no intercept needed. |
| `SHOW INDEX FROM tbl [FROM db]` | `SHOW INDEXES`, `SHOW KEYS` | ✓ passthrough | T1 | |
| `SHOW TRIGGERS [FROM db] [LIKE pat]` | — | ✓ passthrough | T1 | |
| `SHOW EVENTS [FROM db] [LIKE pat]` | — | ✓ passthrough | T1 | |
| `SHOW PROCEDURE STATUS [WHERE ...]` | — | ✓ passthrough | T1 | |
| `SHOW FUNCTION STATUS [WHERE ...]` | — | ✓ passthrough | T1 | |
| `SHOW ENGINES` | — | ✓ passthrough | T1 | |
| `SHOW CHARACTER SET [LIKE pat]` | `SHOW CHARSET` | ✓ passthrough | T1 | |
| `SHOW COLLATION [LIKE pat]` | — | ✓ passthrough | T1 | |
| `SHOW OPEN TABLES [FROM db]` | — | ✓ passthrough | T1 | Niche; works. |
| `SHOW PLUGINS` | — | ✓ passthrough | T1 | |
| `SHOW PROCESSLIST` | — | ✓ passthrough | T1 | **Shell-first path is first-class.** DBAs who want the keyboard-driven view type `SHOW PROCESSLIST;` and get the raw table. DBAs who want a live mouse-driven view use the Activity pane ([[Monitor Pane]]). Both are supported. Do NOT deprecate the shell path in favor of the pane. |
| `SHOW FULL PROCESSLIST` | — | ✓ passthrough | T1 | Same as above but includes full query text. |
| `SHOW ENGINE INNODB STATUS` | — | ⚠ ugly passthrough | T3 | Passes through but dumps a wall of text across multiple named sections (Deadlock, Transactions, Buffer Pool, Log, Semaphores, File I/O). Parsing into a structured "InnoDB Status" pane is referenced in [[Monitor Pane]]. Each section parse is its own engineering item, so T3 rather than T2. Passthrough stays available; the structured view is additive. |
| `SHOW ENGINE PERFORMANCE_SCHEMA STATUS` | — | ✓ passthrough | T1 | |
| `SHOW PROFILES` | — | ✓ passthrough | T1 | Deprecated in MySQL 8.0 but still functional. Legacy. |
| `SHOW PROFILE [type_list] [FOR QUERY n]` | — | ✓ passthrough | T1 | Same. |

**Round 1 summary:**
- 29 commands listed, all T1 passthrough except for `SHOW ENGINE INNODB STATUS` (T3 — rich pane referenced in [[Monitor Pane]]).
- No new shell infrastructure required for Round 1 parity — every T1 passthrough row is already functional via the normal query path.
- `SHOW ENGINE INNODB STATUS` is the only Tier 3 item from this round and it's already known work in [[Monitor Pane]].

### Session Control

Session control is where the shell's interactive nature matters: these commands modify *connection state* — the currently-selected database, session variables, the prompt format, whether query output auto-shows warnings, the statement delimiter, etc. Several are SQL statements (`USE`, `SET`) that pass through cleanly. Several are **client-only directives** that the native `mysql` CLI intercepts — these are where meta-commands in the plane's `meta_cmd` handler earn their keep.

| Command | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `USE dbname` | `\u dbname` | ✓ implemented | T1 | As the mysql CLI's `use` (com_use): `use db`, `use db;`, `\u db` and a `USE` statement of a batch print `Database changed` (not "Query OK") — onto another database after the CLI's `Reading table information for completion…` lines (2026-10-16; one difference: the CLI asks the server for its database, this shell compares the one it tracked — a name differing only in case on a server folding names, or a change it didn't see such as the current database dropped from under it, can differ) — and the shell is then on that database — tab label, `:DBNAME`, and where a reconnect goes (`DbmsPlane::switches_database`). `use db; select …` ends `use` at its `;` and runs the rest after it. A bare `use` prints `ERROR: ` / `USE must be followed by a database name`; an unknown database the server's `ERROR 1049 (42000)` (2026-10-10). |
| `SET @var = expr` | — | ✓ passthrough | T1 | Plain SQL. User variables persist for the session's lifetime because sessions are pinned. |
| `SET SESSION var = val` | `SET @@session.var = val` | ✓ passthrough | T1 | Plain SQL; session-scoped variable assignment. |
| Error messages | — | ✓ implemented | T1 | A server error prints as the mysql CLI prints it: `ERROR 1146 (42S02): Table 'sakila.nosuch' doesn't exist`. |
| Session defaults at connect | — | ✓ implemented | T1 | A session starts with the mysql CLI's settings: the server's `time_zone`, `sql_mode` (no `PIPES_AS_CONCAT` — `||` is OR; no handshake-added `IGNORE_SPACE`), and the server's default utf8mb4 collation (`connect_mysql`, pinned by `mysql_session_matches_the_cli` against MySQL; on MariaDB 11.4.5+ the server default can differ from the CLI's). The patched sqlx-mysql (`vendor/`) leaves out the handshake flags the CLI doesn't send, so an UPDATE reports rows changed and the session keeps the server's `sql_mode`; statements print the CLI's `Query OK, N rows affected[, W warnings]` and info line. |
| `SET GLOBAL var = val` | `SET @@global.var = val` | ✓ passthrough | T1 | Plain SQL; server-scoped, requires SUPER/SYSTEM_VARIABLES_ADMIN. |
| `SHOW VARIABLES [LIKE pat]` | `SHOW SESSION VARIABLES` | ✓ passthrough | T1 | Two-column name/value output, usable as-is. |
| `SHOW GLOBAL VARIABLES [LIKE pat]` | — | ✓ passthrough | T1 | |
| `SHOW STATUS [LIKE pat]` | `SHOW SESSION STATUS` | ✓ passthrough | T1 | Two-column counter output, usable as-is. |
| `SHOW GLOBAL STATUS [LIKE pat]` | — | ✓ passthrough | T1 | |
| `status` (bare word) | `\s` | ✓ implemented | T1 | A named command (`MySqlPlane::named_command` / `statement_command`; see Named commands below). Runs our `mysql_status_sql` against the server and displays the result vertically. |
| `quit` | `exit`, `\q` | ✓ implemented | T1 | A named command (see Named commands below). Disconnects the session. |
| `edit` / `\e` | — | ✓ implemented | T1 | `\e` in `MySqlPlane::meta_cmd`, `edit` a named command. Opens the buffer — or the previous command when it's empty, as the CLI's `old_buffer` keeps it: nothing after `;`, `\g`, `go` or `ego` alone, which `com_go` copies before "No query specified", or after a server help typed alone, the statement after one typed as one (`com_server_help`; 2026-10-16; one difference: a help typed after SQL on its line copies that SQL), pinned against the CLI by `edit-previous-query.spec.ts` — in the user's external editor (`EDITOR` → `VISUAL` unless Settings / the connection name one). Like the mysql CLI, `\e` ignores arguments; `edit` counts only on a line of its own with nothing pending, or as a statement of its own (`edit;`), so `SELECT edit FROM t` stays SQL. (The CLI's `edit;` opens its editor on the word `edit` and drops what comes back; here it edits as `edit` does.) Returned text waits for Enter. |
| `help` | `\h`, `\?` | ✓ implemented | T1 | A named command (see below). Prints the MySQL shell command list via `mysql_help_lines`; `help select` / `? select` is `\h select` (2026-10-13; before, `help` ignored its topic). |
| Named commands ended by the delimiter (`status;`, `select 1; use db;`) | — | ✓ implemented | T1 | As mysql 9.6 reads them (`find_command`, 2026-10-13; before, `status;` went to the server — `ERROR 1064` — and only `use db;` worked): every statement its delimiter ends is checked against the CLI's 27 named commands (`NAMED_COMMANDS`) — name in any case, past leading whitespace and `/* */` comments, arguments only for a command that takes them — and runs as that command, in order with the rest of the line (`select 1 as a; Status ; select 3 as c;` → a, status, c; `DbmsPlane::statement_command`, `parser::CommandStart::Statement`). A line typed with nothing pending is one too unless it has `\g` or the delimiter in it (`delimiter //;` sets `//;`; `DbmsPlane::named_command`). `status foo;` goes to the server (`status` takes no arguments); `go;` / `ego;` send their own name (1064), while `go` alone says `No query specified`; `status\n;` (a line break after the name) goes to the server, as `find_command` reads it. The ones the shell doesn't run yet (`warnings`, `pager`, `tee`, `source`, `connect`, `prompt`, `charset`, `query_attributes` …) say `warnings isn't supported here yet.` instead of going to the server. `use` with a backtick sends the text typed (`normalize_dbname`). Checked under a pty. |
| `connect [db [host]]` | `\r` | ✓ implemented | T2 | As the mysql CLI's `com_connect` (2026-10-16; before, "isn't supported here yet"): arguments read as `get_arg` reads them, the buffer cleared, the session closed and a new one opened on the database named (or the one it's on), saying `Connection id:` / `Current database:` — after `Reading table information…` when a database was named. A database the server refuses: its error, and no connection; the next statement's reconnect tries it again and says `Can't connect to the server`. Another host isn't supported yet (it needs its own credentials). Pinned against the CLI under a pty by `mysql-connect.spec.ts`. |
| `prompt [format]` | `\R format` | ✗ pending | T2 | Customize the shell prompt with format specifiers (`\u@\h [\d]>` style). Currently hard-coded to `N SQL>`. New state on `ShellTab`, a small format-specifier parser, and a reactive render hook. |
| `charset name` | `\C name` | ✗ pending | T2 | Change the client character set mid-session. For the browser case this is mostly `SET NAMES <name>` against the server + accepting whatever the server returns as UTF-8. Latin1 / non-UTF round-trips are edge-case territory to revisit later. |
| `warnings` | `\W` | ✓ implemented | T2 | As the mysql CLI's `com_warnings` / `print_warnings` (2026-10-15; before, "isn't supported here yet"): `Show warnings enabled.`; from then on each statement that has warnings, or fails, is followed by the server's `SHOW WARNINGS` — a line each, `Warning (Code 1365): Division by 0` — but for a failure's own error alone; then the next statement (`shell::fetch_warnings` → a `warnings:` batch → `print_warnings`, `DbmsPlane::warning_lines`). Off to start, as the CLI's. Pinned against the CLI under a pty by `mysql-warnings.spec.ts`. |
| `nowarning` | `\w` | ✓ implemented | T1 | As the CLI's `com_nowarnings` (2026-10-15): `Show warnings disabled.`, and no warnings after statements. |
| `delimiter //` | `\d //` | ✗ pending | T2 | Change the statement delimiter. Required for typing stored-procedure / trigger definitions containing semicolons. Touches the shell's submit logic directly — the shell must know the current delimiter and switch. **Unblocks stored-proc authoring from the shell, which is otherwise impossible.** Cross-referenced to the Scripting round, where it affects multi-statement parsing. |
| `rehash` | `\#` | ✓ implemented | T1 | As the mysql CLI's `com_rehash` (2026-10-15; before, "isn't supported here yet"): the shell's cached table and column names for completion are dropped, so the next Tab fetches them afresh (`MetaAction::RebuildCompletion`); it says nothing, as the CLI's doesn't. Pinned by `mysql-nopager-notee-rehash.spec.ts`. |
| `clear` | `\c` | ✓ implemented | T1 | Clears the in-progress input without executing it and without a word, as mysql does — typed alone, on the line below, or after SQL on the same line (`select * from t where \c`). What's typed after it on its line reads on, as the CLI reads on (`select 4 \c select 5;` runs `select 5`, 2026-10-12). |
| Any of its commands typed after SQL (`select 1 \s`, `\u db`, `\d //`, `\h`) | — | ✓ implemented | T1 | Runs there and leaves the query buffer in the input, as the CLI does (`MetaAction::works_on_buffer`). A command without arguments takes `\` and its letter and the line reads on after it as SQL, joining the buffer (`select 3 as c \s from dual;` runs `select 3 as c  from dual` after the status — the text around the command, joined as the CLI joins it); one with arguments (`\u`, `\d`, `\h`, `\?`, `\r`, `\R`, `\P`, `\T`, `\C`, `\.`, `\!`) its line up to the delimiter after them, that included — `\d` the one it sets — or to the line's end, the rest of the line reading on (`\u mysql ; select 1;` changes database, then runs `select 1`; `DbmsPlane::command_extent`, `parser::mysql_command_len`), while reading its arguments from the whole line, as the CLI hands them over (`\u sakila;` is database `sakila;` — `ERROR 1049 (42000): Unknown database 'sakila;'`; `\h select;` topic `select;`; `\d //;` delimiter `//;`). A command typed first reads only its line, and the lines after it read on (before, they were dropped). A `;` after a command reads on like any text (2026-10-13; before, it was let go): `\s;` and `\c;` print the CLI's `ERROR: ` / `No query specified` after the command, `select 2 \s;` runs `select 2` after the status. Its commands this shell doesn't run yet — `\r`, `\R`, `\P`, `\T`, `\C`, `\.`, `\!`, `\x` — say `\T isn't supported here yet.` instead of going to the server. The input is read in order, as the CLI reads it (`shell::process_input`, 2026-10-12): statements ended before the command run first, then the command, then what follows — at once, or once the shell is free (after `use`'s `USE`, `\s`'s status) — so `select 3 as c \s from dual;` shows status and then runs `select 3 as c  from dual`, and a pasted `DELIMITER //` … `DELIMITER ;` script runs as the CLI runs it. After `\e`, what follows comes back after the editor's text, as the CLI appends it. Parliament's own `\safe-mode` takes its line. Checked against mysql 9.6 under a pty, 2026-10-12. |
| `help 'SQL_STATEMENT'` | `\h topic`, `? topic` | ✓ implemented | T3 | As the mysql CLI's `com_server_help` (2026-10-15; before, a table of syntax summaries and doc links of our own): the server's `HELP 'topic'` — the topic as typed after the command's first space (`\h select;` asks for `select;`), quoted as the CLI quotes it (one typed with its quote goes as typed; a `'` in it breaks the statement — the server's `ERROR 1064`), cut to the CLI's 101 bytes — staged, its answer printed as the CLI prints each: one topic (`Name: 'X'` / `Description:` / `Examples:`), several items (`Many help items for your request exist.` …) or a category's contents (`You asked for help about help category: "X"`), each list under `categories:` / `topics:`, or `Nothing found` (and for `contents`, `Please check if 'help tables' are loaded.`). Pinned against the CLI under a pty by `mysql-server-help.spec.ts`. |
| `pager [cmd]` | `\P [cmd]` | ✗ not supported | BD | Native `mysql` pipes output through `less` or `$PAGER`. Our shell's native scroll IS the pager — no external pager to configure. Renders as `NotSupported { reason: ByDesign("the shell's native scroll is the pager; no external pager exists in a browser context") }`. |
| `nopager` | `\n` | ✓ implemented | T1 | As the mysql CLI's `com_nopager` (2026-10-15; before, "isn't supported here yet"): `PAGER set to stdout` — a pane pages nothing, so that's all there is to do. Pinned against the CLI under a pty by `mysql-nopager-notee-rehash.spec.ts`. |
| `print` | `\p` | ✓ implemented | T1 | As the mysql CLI's `com_print` (2026-10-15; before, "isn't supported here yet"): `--------------`, the buffer — every line since the last statement ended, blank lines and comments kept, the space before `\p` too — `--------------` and a blank line; an empty line between the rules when nothing is pending (no previous query: the buffer empties when a statement runs). `print;` as a statement prints `print`; typed after SQL it leaves the buffer, which reads on (`select 7 as g \p from dual;` prints `select 7 as g ` and runs). One difference: a first line's indent (`  select 1 \p`), which the CLI keeps, is dropped here. Pinned against the CLI under a pty by `print-buffer.spec.ts`. |
| `system cmd` | `\!` | ✗ not supported | BL | OS shell escape. Not possible from a browser sandbox. Renders as `NotSupported { reason: BrowserLimitation("browser cannot invoke OS processes; see vault/Architecture/OS Provider Seam.md") }`. |

**Round 2 summary:**
- 24 commands catalogued (first counted at this round's start; since then `help 'SELECT'` — the server's `HELP` — `nopager`, `notee` and `rehash` have shipped, each row above saying when): 11 then T1-implemented, 9 T2 pending (shell infrastructure work), 1 T3 deferred to later round (`help 'SELECT'`), 2 BD (pager/nopager), 1 BL (`\!`).
- The three most valuable T2 items to implement when we start coding parity work: **`delimiter`** (unblocks stored-proc authoring), **`warnings` / `\W`** (high DBA muscle memory), **`connect` / `\r`** (since implemented; operational reconnect flow already in the vault's Next-Up list).
- The two BD rows (`pager` / `nopager` — the latter since implemented: `PAGER set to stdout`) plus the one BL row (`system` / `\!`) are the first entries that would populate the **Browser Limitations** section of this doc when we finalize.
- The "no runtime output-format toggle" gap (e.g. no psql `\pset format`) is out of scope — native `mysql` only exposes output formatting as CLI startup flags (`--html`, `--xml`, `--batch`, `--vertical`). Flagged for the **Output Formatting** round to decide whether we make them runtime-toggleable even though native MySQL doesn't.

### Output Formatting

MySQL's native CLI exposes output formatting mostly as CLI startup flags (`--table` / `--vertical` / `--html` / `--xml` / `--batch` / `--silent` / `--raw`), not as runtime-toggleable meta-commands. Psql has `\pset format` for runtime toggling; MySQL doesn't, except for the `\G` terminator. **Our decision: expose runtime toggles even though native MySQL doesn't**, consistent with the "structured rendering polish" track we've accumulated across Rounds 4–8.

| Command / feature | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `\G` (statement terminator) | — | ✓ implemented | T1 | Ends a statement wherever it's typed outside quotes and comments, as `;` does — `select 1\G select 2;` runs both, the first vertically (`parser::split_statements`, 2026-10-11). The rows print as the CLI's `print_table_data_vertically` prints them (2026-10-14; before, a key/value grid of our own): `*************************** 1. row ***************************` over each row, names right-justified to the longest by bytes (`%*s`), values as they are, `NULL` for a null (`MySqlPlane::print_vertical`). Pinned against `mysql -E` by `vertical-output.spec.ts`. |
| `\g` (statement terminator) | `;` | ✓ implemented | T1 | Identical to `;`, wherever it's typed: `select 1 \g`, or `\g` on the line below (2026-10-11; before, only claimed). With nothing before it — as `;` alone, or the second `;` of `select 1;;` — it says mysql's `ERROR: ` / `No query specified`, in order with the rest of the line (2026-10-12). |
| **Runtime output-format toggle** — `\output {table | vertical | csv | tsv | json | markdown | html}` (new, parity-plus) | — | ✗ pending | T2 | Not a native MySQL command; a parity-plus feature consistent with Round 6's "save as" download action. Persists per-session. Default `table`. Shares infrastructure with save-as. |
| `\timing on` / `\timing off` | — | ✗ pending | T1 (low priority) | We already show `exec_time_ms` on every result by default. Toggle would hide/show. Listed for completeness. |
| Runtime NULL display override | — | ✗ pending | T1 | Native MySQL has no runtime equivalent; psql has `\pset null`. Small parity-plus feature. |
| `--batch` / `--html` / `--xml` / `--silent` / `--raw` CLI startup flags | — | — | out of scope | Covered by runtime `\output` toggle above. No need for CLI-flag analogs — browsers don't launch with flags. |
| Column-width / truncation control | — | — | out of scope | Browsers handle wrapping natively. Won't add flags where the platform already does it better. |

### History & Editing

Most commands already covered in Round 2 (Session Control). This section picks up persistent history, readline keybindings, history-search, and tab completion. **Cross-cutting note:** readline keys, Ctrl+R search, persistent history, and tab completion are **shell-level features that apply across all dialects** — listed here because we benchmark MySQL CLI parity, but the implementation is shared across planes and benefits Postgres and future DBMSes too.

| Command / feature | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `edit` / `\e` | — | ✓ implemented (Round 2 cross-ref) | T1 | Handled in `MySqlPlane::meta_cmd`; opens the buffer in the external editor — see the Round 2 row. |
| `\c` (clear current input) | — | ✗ pending (Round 2 cross-ref) | T1 | Catalogued in Round 2. |
| `\r [db [host]]` (reconnect) | — | ✓ implemented (Round 2 cross-ref) | T2 | See `connect` above. |
| NULL display | — | ✓ implemented | T1 | SQL NULL prints `NULL`, as the CLI prints it, and a text 'NULL' prints as itself — told apart now that noctua sends NULL apart from text (2026-10-15; before, the two arrived alike). MySQL has no `\pset null`. |
| Column alignment | — | ✓ implemented | T1 | The grid lines each column up as the CLI's `print_table_data` does (2026-10-14; before, everything left): a numeric column (`IS_NUM` — the integer types signed or not, `tinyint(1)`, DECIMAL, FLOAT, DOUBLE, YEAR, and NULL's own type, as `select null` has) right-justified, NULLs in it too; the rest, and every header, at the left. By the column's type as sqlx names it (`QueryResult.types`), which keeps apart every type `IS_NUM` does. Pinned against the CLI's own table by `grid-alignment.spec.ts`. |
| Result footers (`N rows in set`, warnings, `Empty set`, `(0.002 sec)`) | — | ✓ implemented | T1 | Under tables, vertical (`\G`) output and `Query OK` lines, worded and timed as the mysql CLI (2026-10-11). The boxed table is Parliament's own grid (columns aligned as the CLI aligns them); vertical output is the CLI's text (see `\G`). |
| Auto-reconnect on a lost connection (`--reconnect`, default on) | — | ✓ implemented | T1 | The next statement prints `ERROR 2013 … Lost connection …` / `No connection. Trying to reconnect...` / `Connection id:` / `Current database:` and runs in a new session on the same database; a statement that finds its connection dead runs again, twice at most (2026-10-10). |
| Up/Down arrow history navigation | — | ✓ implemented | T1 | Per-pane history already working. |
| **Persistent history across sessions** | — | ✗ pending | T2 | `localStorage` keyed by connection-id + DB. Size cap ~2000 entries, FIFO eviction. Native `mysql` persists to `~/.mysql_history`; we persist to browser storage. **Shell-level feature** — shared across planes. |
| **Ctrl+R reverse history search** | — | ✗ pending | T2 | Inline search overlay that filters history by typed substring. Classic terminal muscle memory. **Shell-level feature.** |
| **Readline keybindings** — `Ctrl+A` / `Ctrl+E` / `Ctrl+W` / `Ctrl+U` / `Ctrl+K` / `Alt+B` / `Alt+F` / `Ctrl+P` / `Ctrl+N` | — | ✗ pending | T2 per-binding | Terminal muscle memory; intercepts on the input textarea's keydown handler. Some (`Ctrl+A` / `Ctrl+E`) conflict with browser defaults — we override within the pane. **Shell-level feature.** |
| **Tab completion** (schema / table / column / keyword completion) | — | ✗ pending | T3 | Tab-key intercept on the input textarea. Data source split: meta-command completion is cheap, schema/column completion uses the Schema Explorer's cached tree. Complex enough to warrant its own vault design doc (`vault/Features/Tab Completion.md`) when implementation starts. **Shell-level feature with per-plane metadata source.** Cross-reference to the Round 2 `rehash` / `\#` row — when tab completion exists, `rehash` upgrades from T2-stub to T1-real. |
| `--histfile` / `--no-auto-rehash` CLI flags | — | — | out of scope | We handle persistence via localStorage and auto-rehash as always-on given browser constraints. |

### OS Escape

All entries here are BL/BD — most already cross-referenced from Round 2. Listed for category completeness; these rows populate the **Browser Limitations** section at the bottom of MySQL.md at finalization time.

| Command / feature | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `\!` / `system cmd` | — | ✗ not supported (Round 2 cross-ref) | BL | Browser sandbox prevents OS process invocation. Cites [[OS Provider Seam]]. |
| `\P pager` / `pager cmd` | — | ✗ not supported (Round 2 cross-ref) | BD | Browser's native scroll is the pager. |
| `\n nopager` | — | ✓ implemented (Round 2 cross-ref) | T1 | See above. |
| `$EDITOR` external editor integration | — | ✓ implemented (solo-local) | T1 | Via `edit` / `\e` — see that row, [[OS Provider Seam]] and [[Settings]]. |
| File reads from arbitrary paths without consent | — | ✗ not supported | BL | Browsers sandbox the filesystem; every read requires explicit user consent via `<input type="file">`. See [[OS Provider Seam]]. |
| Process spawning / script execution outside the browser sandbox | — | ✗ not supported | BD | Permanently — the backend is a database client, not an SSH client. Users who need shell access on the backend host use SSH. |

### Help & Discovery

| Command / feature | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `help` / `\h` / `\?` | — | ✓ implemented (Round 2 cross-ref) | T1 | Already handled. Prints the MySQL shell command list via `MySqlPlane::help_lines()`. |
| `help 'SQL_STATEMENT'` | — | ✓ implemented (Round 2 cross-ref) | T3 | The server's `HELP`, as the CLI prints it — see above. |
| `\help <client-command>` (per-command detail) | — | ✗ pending | T2 | Extension of `\h` — `\help \e` shows detailed usage. Shared across planes via a new trait method `DbmsPlane::command_help(name: &str) -> Option<&'static str>`. |
| Tab completion for client commands | — | ✗ pending | T3 (part of tab completion above) | Typing `\<Tab>` lists the active plane's meta-commands. Rolls into the general tab-completion design. |
| `?\G` and similar "help this shape" shortcuts | — | — | out of scope | Not a native MySQL feature. Reserved for later if demand surfaces. |

---

**Round 9 summary:**

- 17 rows catalogued across four sub-categories, plus 11 cross-references to Rounds 2 / 6 / 7 / 8. Mix of already-implemented T1 (4), pending T1 (3), T2 infrastructure (8), T3 (2 — `help 'SELECT'` already catalogued and tab completion), BL/BD (6).
- **`\output` runtime toggle (T2) shipped as a parity-plus feature** even though native MySQL doesn't have one. Browser-plus approach consistent with the Round 6 save-as action and the accumulated rendering-polish track.
- **Four shell-level cross-plane features surfaced** that benefit Postgres and future DBMSes too: persistent history, Ctrl+R reverse search, readline keybindings (per-binding T2), tab completion (T3). Listed under MySQL parity because that's the benchmark, but implementation is plane-agnostic.
- **Browser Limitations section** will be populated at finalization (when MySQL Axis 1 is formally wrapped in Round 10+). All BD/BL rows from this round + Rounds 2, 6, and 7 pulled into one canonical list with [[OS Provider Seam]] cross-refs.

---

### Scripting

Scripting is the first round with real infrastructure bite. `SOURCE filename` depends on `BrowserOsProvider`, multi-statement parsing, and a new "batched script run" render path; `DELIMITER //` is the meta-command that unblocks stored-procedure authoring and makes the shell's submit logic delimiter-aware; pasting a multi-statement blob is the user-facing bug that reveals the same underlying infrastructure gap. Getting this category right unblocks the workflow you mostly can't do in the shell today: writing a stored procedure.

**Three key design decisions taken this round** (all matching native-`mysql` behavior where applicable):

1. **Script error handling** — abort on first error, matching native `mysql`. A future `--force`-style opt-out lands in a follow-up round. Opinionated defaults ("continue past errors") are the kind of "helpful" behavior that bites later when a DBA expects native semantics.
2. **Script transaction wrapping** — let the script drive. No implicit `BEGIN...COMMIT` around a SOURCE'd file. Users who want atomicity add explicit TCL statements to their script. Matches native `mysql --execute`.
3. **Multi-statement paste splits by default** — pasting `SELECT 1; SELECT 2;` runs both, rendered as a batched script run. Not a per-user preference; fixing it is the same infrastructure as SOURCE + DELIMITER.

| Command / feature | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `SOURCE filename` | `\. filename` | ✗ pending | T2 + BL dep | Run every statement in a file. Needs `BrowserOsProvider` — user picks a file via `<input type="file">`, frontend reads it, splits on the current delimiter, runs sequentially. Abort on first error. Script drives its own transactions. Result rendered as a batched `ShellEntry::ScriptRun`. See [[OS Provider Seam]] Phase 1. |
| `DELIMITER //` | `\d //` | ✓ implemented | T1 | Changes the statement delimiter for subsequent input (per-pane `delimiter` signal, default `;`), as mysql's `com_delimiter` does: the first word after it, set silently; `delimiter ;` goes back; a bare `delimiter` is mysql's `ERROR: ` / `DELIMITER must be followed by a 'delimiter' character or string`. A delimiter string alone says `No query specified` (2026-10-12). |
| Multi-statement parser (infrastructure) | — | ✓ implemented | T1 | The mysql CLI's lexer (`parser::Lexicon::Mysql`): the current delimiter, `\g` / `\G`, `'…'` / `"…"` with backslash escapes, backticks, `/* */` (`/*!` / `/*+` as code), `#` and `-- ` comments, and a backslash outside them taking the character after it, as `add_line` does (`\'` opens no string, `\;` ends nothing; 2026-10-12). Comments go where the CLI puts them (`--comments`, its default): a line comment starting a line with nothing pending is sent on its own (`Query OK, 0 rows affected`), one after a statement on its line goes with it, a block-comment-only statement goes at its `;` (2026-10-12). |
| Unknown backslash commands (`\i`, `\l`; psql's `\if`) | — | ✓ implemented | T1 | Read as mysql's `add_line` reads each line — every line of a paste in turn (2026-10-15; before, only the last): a statement's notes when it's sent, after what the statements before it printed, as the CLI says them as it reads; text left waiting, at once or once what runs before it has answered; a line noted once. Pinned against the CLI under a pty. Each line: `\` and a character it has no command for gets `ERROR: ` / `Unknown command '\i'.` (one each) and stays in the statement, which the server gets (`ERROR 1064`). Quotes and comments are skipped, `\N` (NULL) isn't one, a command's arguments (`\u db`, `\h …`) are skipped as they are — quotes and all — up to the delimiter that ends them, the line read on after it (`\u sakila ; select 1 \i;` says it for `\i`), and the CLI's own commands still run past one (`\if 1 \c` clears). psql's `\if` blocks aren't the MySQL plane's (it has no `MetaAction::Branch`; `parser::mysql_unknown_commands`, `DbmsPlane::line_notes`, 2026-10-12). As the CLI, the shell reads `\` + one letter as the command whatever follows it (2026-10-13): `\endif` / `\else` / `\elif` are its `\e` (the editor opens on the previous statement — kept even unchanged — and `ndif` follows the edited text), `\sf f` its `\s` (status, then `f f` as SQL), `\usakila` `use sakila`, `\d//` the delimiter `//`, `\hselect` plain `\h` (its topic follows a space). The psql-named `\sf` / `\sv` shortcuts (SHOW CREATE FUNCTION / VIEW) are gone; Parliament's own `\safe-mode` stays a word. |
| Multi-statement paste submit | — | ⚠ broken | T2 | Paste `SELECT 1; SELECT 2; SELECT 3;` today and only `SELECT 1` runs — the rest lingers as input. Same infrastructure as the parser above, different trigger path. The user-visible bug that motivates the whole round. |
| Batched script result render | — | ✗ pending | T2 | New `ShellEntry::ScriptRun { statements: Vec<(sql, result)>, source_label: Option<String> }` variant. Collapsed by default (shows "Ran 20 statements from `audit.sql` — 20 OK, 0 errors"), expandable to walk each statement's result. Keeps a script run from flooding the history with 20 entries. |
| `\. filename` | — | ✗ pending | T1 (after SOURCE) | Backslash alias for SOURCE supported by some forks / older clients. Wiring is a one-liner in `MySqlPlane::meta_cmd` once SOURCE exists. |
| Script `--force` opt-out | — | ✗ pending | T3 | "Continue past errors and report failures at end" — matches native `mysql --force`. Deferred to a follow-up: needs UI for opt-in per-run, plus a summary renderer. |
| Nested sourcing (SOURCE inside a SOURCE'd file) | — | — | **out of scope** | Native `mysql` SOURCE nests but has relative-path foot-guns and no cycle detection. We support one level only — a SOURCE'd file containing another `SOURCE` statement gets a `NotSupported { ByDesign("nested SOURCE is not supported — re-structure your scripts or run them at the top level") }` response. |
| Parameter substitution / CLI variables | — | — | **out of scope** | Native `mysql` doesn't have interactive parameter binding the way psql's `\set` / `:var` does. MySQL's user variables (`SET @foo = ?`; then `? = @foo`) are the substitute and pass through as plain SQL. Nothing to implement. |
| `PREPARE stmt FROM '...'` / `EXECUTE stmt` / `DEALLOCATE PREPARE stmt` | — | ✓ passthrough | T1 | Server-side prepared statements. Plain SQL, work as passthrough. Rarely used interactively — listed so they're not confused with client-side scripting features. Goes in the Axis 2 DML row too. |

**Round 3 summary:**

- 10 rows catalogued. No T1 already-implemented rows this round — everything is infrastructure-pending. 6 T2 items, 1 T1-after-SOURCE, 1 T3 deferred, 2 explicitly out of scope.
- The **two decoupled quick wins** that can land without the full OS Provider Seam: **DELIMITER** (unblocks stored-proc authoring immediately) and **multi-statement paste fix** (fixes a user-visible bug, same underlying parser infrastructure). Suggested implementation order: parser → DELIMITER + paste fix together → SOURCE + batched render when `BrowserOsProvider` lands.
- **SOURCE is really three features in one trench coat**: file picker plumbing, multi-statement parsing, batched result rendering. Landing SOURCE means doing all three. [[OS Provider Seam]] calls it out as the first Phase-1 parity item to implement.
- Two policies locked in that match native `mysql`: abort on first error, script-drives-its-own-transactions. Both preserve the principle of least surprise for DBAs transferring from the terminal.

### File I/O

The first round dominated by BL (browser-blocked) entries and the place where [[OS Provider Seam]] earns most of its parity rent. Covers loading data from files into tables, exporting query results, shell output capture (tee), and the "local vs server-side" file boundary that every MySQL file command distinguishes.

**The "local" vs "server-side" boundary matters here.** Every MySQL file command distinguishes:

- **LOCAL (client-side)**: `LOAD DATA LOCAL INFILE` streams through the wire protocol from the client. In a browser, "client" is the user's laptop via `<input type="file">` — covered by `BrowserOsProvider`.
- **Server-side** (no LOCAL): `LOAD DATA INFILE`, `SELECT INTO OUTFILE`, `INTO DUMPFILE`. Resolve paths on the MySQL **server's** filesystem, governed by `secure_file_priv`. Out of the OS Provider Seam's scope — the backend just relays the SQL and the server does what it does.

**Priority decision for this round:** the **"Save query result as..." download action** ships ahead of `LOAD DATA LOCAL INFILE`. The download path covers more of the real-world use case with less implementation risk (no sqlx upstream work), so it earns first place on the File I/O implementation track. LOAD DATA LOCAL INFILE is explicitly deferred as a **future spike item** — it may even require forking sqlx or contributing upstream, and the investigation time before we know if it's viable is the kind of risk that shouldn't block visible forward motion.

| Command / feature | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| **"Save query result as..." download** (new UX action) | — | ✗ pending | T2 | **Priority #1 for this round's track.** Not a SQL command — a row-action on the shell's result table that triggers a browser download of the result as CSV / TSV / JSON / Markdown / SQL INSERT statements. Covers 80% of the real-world "I want this data as a file" use case with no sqlx complications. Reuses `BrowserOsProvider::write_download`. Part of the "shell result rendering polish" track started in Round 4 (long-text) and Round 5 (wide-result). |
| `\T filename` (tee) | `tee filename` | ✗ pending | T2 + BL | Capture subsequent shell output to a file. Buffers in-memory; on `\t` / `notee` the buffer is flushed as a browser download. Cross-refs `BrowserOsProvider::write_download`. Pairs with `\t` / `notee` from Round 2's Session Control table. |
| `\t` (no tee) | `notee` | ✓ implemented | T1 | As the mysql CLI's `com_notee` (2026-10-15): `Outfile disabled.` — there's no outfile here to close (`\T` isn't run yet); when `\T` lands, closing it goes here. Pinned against the CLI under a pty. |
| `SELECT ... INTO OUTFILE 'path'` | — | ✓ passthrough with advisory | T1 + T2 advisory | Writes a tabular result to the **MySQL server's** filesystem, subject to `secure_file_priv`. Passthrough works, but this is almost never what a browser user wants — they want the result on their laptop, not on the DB server. **Runtime advisory pattern**: `MetaAction::Output` wrapper alongside the passthrough that emits a one-line hint ("this writes to the MySQL server's filesystem, not your laptop — use the 'Save result as' action or `\T` to save locally"). Advisory only — not a hard warning, not a confirmation gate. Users who know what they're doing already know; users who don't get the friendly explanation. |
| `SELECT ... INTO DUMPFILE 'path'` | — | ✓ passthrough with advisory | T1 + T2 advisory | Same as `INTO OUTFILE` but writes raw bytes (for blob-like output). Same advisory pattern. |
| `LOAD DATA INFILE 'path' INTO TABLE t [options]` | — | ✓ passthrough | T1 | Server-side path — MySQL resolves on its own filesystem, subject to `secure_file_priv`. Not our problem; the SQL passes through and the server either succeeds or errors with a privilege message. Nothing to build. |
| `LOAD XML INFILE 'path' INTO TABLE t` | — | ✓ passthrough | T1 | Server-side XML variant. Same passthrough story. |
| `LOAD DATA LOCAL INFILE 'path' INTO TABLE t [options]` | — | ✗ deferred — **spike required** | **T3 future + BL** | **Explicitly deferred from v0.0.1 near-term scope.** Three-part implementation: (a) `BrowserOsProvider::read_file` picks a file; (b) sqlx needs to accept the file contents at the LOAD DATA handshake point — **sqlx's LOAD DATA LOCAL support is limited and may need a fork or upstream contribution**; (c) the frontend must recognize the LOAD DATA LOCAL INFILE shape in user input and intercept *before* sending the SQL so the file is picked at the right point. The sqlx dependency is the concerning part — investigation time before we know if it's viable is open-ended, so we're parking this until a future sprint dedicated to it. **The "Save query result as..." action above covers the common real-world use case** (export data from DB) and lands first to keep forward motion visible. |
| `LOAD XML LOCAL INFILE 'path' INTO TABLE t` | — | ✗ deferred — **spike required** | T3 future + BL | XML variant of LOAD DATA LOCAL. Unlocks automatically when the main LOAD DATA LOCAL work lands, whenever that spike happens. |
| `LOAD_FILE('path')` function | — | ✓ passthrough | T1 | SQL function (expression-level) that reads a file from the SERVER's filesystem and returns its contents as a BLOB. Requires FILE privilege. Listed for completeness — belongs in Axis 2 SQL functions, but file-I/O-adjacent and DBAs look for it here. |
| **Drag-and-drop file attach** (new) | — | ✗ pending | T2 **(deprioritized)** | Drag a `.sql` file onto the shell → treat as SOURCE. Drag a `.csv` / `.tsv` → prefill a `LOAD DATA LOCAL INFILE` template in the input textarea. **Low priority** — nice UX polish that DBAs notice in evaluations, but a whole bunch of other functionality is more interesting. Keep on the list so we don't forget; don't prioritize for early implementation. |
| `--local-infile` CLI flag equivalent | — | — | **out of scope** | Native `mysql` has `--local-infile[=value]` to enable LOCAL INFILE handling. In our world this isn't a user-facing setting — we rely on the server's `local_infile` variable when the LOAD DATA LOCAL spike eventually lands. |
| `--secure-file-priv` CLI flag | — | — | **out of scope** | Server-side configuration. Irrelevant to the client. Flagged so nobody proposes a user-visible setting for it. |
| MySQL Shell's `util.importTable()` / `util.importJson()` | — | — | **out of scope** | These are `mysqlsh` utility functions, not classic `mysql` CLI features. Different product entirely. If we later benchmark against `mysqlsh` alongside classic `mysql`, they become their own inventory rows. |

**Round 6 summary:**

- 13 rows catalogued. Dominated by BL (browser-blocked) and deferred/out-of-scope entries for the first time in the interview.
- **Implementation priority order** for this round's track:
  1. **"Save query result as..." download action** — T2, no sqlx complications, covers the common "I want this data" use case. Ships first.
  2. **`\T` tee** (and `\t` notee closing its capture; notee itself already says `Outfile disabled.`) — T2 + T1, uses the same `BrowserOsProvider::write_download` plumbing as (1), natural second item.
  3. **`SELECT INTO OUTFILE` / `INTO DUMPFILE` advisory hooks** — T2 advisory, small. Rides along with (1)/(2) since they share the "shell meta-action that emits an informational message" pattern.
  4. **Drag-and-drop file attach** — T2 deprioritized. On the list, not in the schedule.
  5. **`LOAD DATA LOCAL INFILE` spike** — T3 future, explicitly deferred. Own investigation before committing scope.
- **LOAD DATA LOCAL INFILE is the first MySQL parity item we're explicitly punting from near-term scope.** It's documented, it's not forgotten, it has a clear prerequisite (sqlx feasibility spike), but it's not blocking forward motion on other rounds or on the implementation tracks that *are* viable today.
- **The advisory pattern for `INTO OUTFILE` / `INTO DUMPFILE`** is a new shell-UX primitive: passthrough-plus-hint. It's lighter than `NotYetImplemented` / `NotSupported` (the SQL actually works) and more informative than silence. Good candidate for generalization — other commands with a "works but may not be what you mean" shape (like `\c dbname` in the Postgres plane) could use the same hook.

### Monitoring & Replication

Combining these because they share an audience (the operational DBA watching things break in real time) and because monitoring bleeds into replication diagnostics. Most of the monitoring commands were already cross-referenced in Round 1 (`SHOW PROCESSLIST`, `SHOW ENGINE INNODB STATUS`) or picked up by the [[Monitor Pane|Activity pane]] we already shipped. The new material in this round is `KILL` and its variants, replication status commands, binary log inspection, the handful of hooks that connect the Activity pane to the shell for handoff, and the "wide result rendering" infrastructure item.

| Command / feature | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `SHOW PROCESSLIST` / `SHOW FULL PROCESSLIST` | — | ✓ passthrough | T1 (Round 1 cross-ref) | Shell path is first-class; Activity pane is the mouse-driven sibling. |
| `SHOW ENGINE INNODB STATUS` | — | ⚠ ugly passthrough | T3 (Round 1 cross-ref) | Structured "InnoDB Status" pane referenced in [[Monitor Pane]]. |
| `KILL connection_id` | `KILL CONNECTION connection_id` | ✓ passthrough | T1 | Hard kill — terminates the entire connection, rolls back any in-flight transaction, closes the session. Passes through as SQL. |
| `KILL QUERY connection_id` | — | ✓ passthrough + ✓ in Activity pane | T1 | Soft kill. The variant the Activity pane already uses via `MySqlPlane::kill_query_sql`. Typing it in the shell also works. Both paths first-class. |
| Activity pane — add `KILL CONNECTION` row-action | — | ✗ pending | T2 UX hook | Today's Activity pane exposes only the soft kill. Add a second row-action button (with a clearly-differentiated confirmation — hard kill is destructive to in-flight transactions) for the hard-kill path. v0.0.2 item. |
| Activity pane → shell handoff (open a shell with pre-filled context for a selected PID) | — | ✗ pending | T2 UX hook | Reverse of the Round-4 "Explain this query" hook. Opens a new shell tab on the same connection, pre-filled with `SHOW PROCESSLIST WHERE Id = <pid>\G` or similar. Nice-to-have, not critical. |
| `SHOW BINARY LOG STATUS` | `SHOW MASTER STATUS` (deprecated alias) | ✓ passthrough | T1 | Current binary log file + position. Critical for replication setup and point-in-time recovery workflows. |
| `SHOW REPLICA STATUS` | `SHOW SLAVE STATUS` (deprecated alias) | ⚠ ugly passthrough | T1 + wide-render polish | Replication lag, IO/SQL thread state, last error, relay log position, GTID state. **~60 columns wide** — brutal in the terminal table view unless the user remembers `\G`. Flagged for the wide-result rendering polish below. |
| `SHOW REPLICAS` | `SHOW SLAVE HOSTS` (deprecated alias) | ✓ passthrough | T1 | Lists replicas connected to a source. Small result. |
| `SHOW BINARY LOGS` | `SHOW MASTER LOGS` (deprecated alias) | ✓ passthrough | T1 | Binary log files + sizes. Small; fine as-is. |
| `SHOW BINLOG EVENTS [IN 'log_name'] [FROM pos] [LIMIT n]` | — | ✓ passthrough | T1 | Binary log events. DBAs scope with `LIMIT n`. |
| `SHOW RELAYLOG EVENTS [IN 'log_name'] [FROM pos] [LIMIT n]` | — | ✓ passthrough | T1 | Same, relay log side. |
| `START REPLICA` | `START SLAVE` (deprecated alias) | ✓ passthrough | T1 | |
| `STOP REPLICA` | `STOP SLAVE` (deprecated alias) | ✓ passthrough | T1 | |
| `RESET REPLICA [ALL]` | `RESET SLAVE` (deprecated alias) | ✓ passthrough | T1 | Destructive when `ALL` is used. |
| `CHANGE REPLICATION SOURCE TO ...` | `CHANGE MASTER TO` (deprecated alias) | ✓ passthrough | T1 | Long statement; usually typed from scripts, not interactively. |
| `FLUSH BINARY LOGS` | — | ✓ passthrough | T1 | Rotate the binary log. |
| `PURGE BINARY LOGS TO 'log_name'` / `PURGE BINARY LOGS BEFORE datetime` | — | ✓ passthrough | T1 | Destructive if the argument is wrong. |
| `RESET MASTER` / `RESET BINARY LOGS AND GTIDS` (MySQL 8.4+) | — | ✓ passthrough | T1 | Wipes all binary logs. Very destructive; listed for completeness. |
| Group Replication — `START GROUP_REPLICATION`, `STOP GROUP_REPLICATION`, `SELECT * FROM performance_schema.replication_group_members`, etc. | — | ✓ passthrough | T1 | All SQL passthrough. Out of scope for first-class UI treatment — niche, and dedicated monitoring tools already cover this space well. |
| XA distributed transactions — `XA START` / `XA END` / `XA PREPARE` / `XA COMMIT` / `XA ROLLBACK` / `XA RECOVER` | — | ✓ passthrough | T1 | Two-phase commit. Listed because `XA RECOVER` is a monitoring-adjacent diagnostic for finding in-doubt transactions. |
| **Wide-result auto-vertical rendering** (infrastructure) | — | ✗ dropped (2026-10-14) | — | The grid once switched to a vertical layout of its own past twelve columns, and showed a one-column result with line breaks as a text block; the mysql CLI does neither — it prints the table, however wide, and `\G` goes vertical — so neither does the shell now. A wide table scrolls sideways in the grid. |
| Activity pane "Replication" sub-view | — | ✗ deferred | T3 (speculative) | Structured replication status under the Sessions / Locks sections in the Activity pane. Deferred until real users ask for it — replication monitoring has a crowded competitive landscape (Orchestrator, MySQL Workbench, Percona Monitoring) and we'd be duplicating without a clear differentiator. Flagged for revisit, not planned. |

**Round 5 summary:**

- 22 rows catalogued. The replication surface is almost entirely T1 passthrough — every command works via plain SQL and renders adequately (except for the wide-result problem flagged below).
- **One shared polish infrastructure item** identified: **wide-result auto-vertical rendering**. Joins the Round-4 long-text rendering fix on the "shell result rendering polish" track. Together these two T2 items light up a significant slice of the monitoring + planning rounds.
- **Two Activity pane UX hooks** identified, both T2: hard-kill row action (for KILL CONNECTION) and session-context shell handoff (for opening a pre-filled shell on a selected PID). Neither is critical for v0.0.1 — both are v0.0.2 polish.
- **One explicitly-deferred T3**: Activity pane Replication sub-view. Not planned unless driven by real user demand; the competitive landscape is crowded and first-class replication monitoring is a product-positioning decision, not a parity-gap decision.
- **Group Replication and XA transactions** are acknowledged as SQL-passthrough T1 rows but explicitly out of scope for first-class UI treatment in v0.0.1. Listed for inventory completeness, not for prioritization.

### Query Planning

Deferred from Round 1 so the structured-pane design question got its own pass. Covers EXPLAIN in all its MySQL forms, the optimizer trace surface, `EXPLAIN FOR CONNECTION` for inspecting plans on other sessions, the legacy `SHOW WARNINGS`-after-EXPLAIN flow, and the deprecated EXTENDED / PARTITIONS variants.

**Big decisions this round — two new product surfaces:**

1. **A dedicated Plan Viewer pane** is the right answer for `EXPLAIN FORMAT=JSON`. Raw JSON blobs are useless rendered as a table; MySQL's JSON plan is a deeply nested tree that deserves real estate. Three-view design so different DBAs see what helps them:
   - **Raw JSON view** — the actual blob as-is, for copy/paste into tools like Percona's plan analyzers or for pasting into tickets.
   - **Readable text layout** — a pretty-printed indented form akin to `EXPLAIN FORMAT=TREE` output but organized with section headers (query_block, nested_loop, attached_subqueries) and aligned cost/row columns. The "I prefer text" DBA path.
   - **Graphical display** — a visual tree diagram with cost-weighted node sizing, color-coded access types, click-to-inspect nodes. The "I prefer pictures" DBA path.

   All three views show the **same underlying parsed plan**, switchable via a tab strip similar to the Schema Explorer's Schema/DDL tabs. T3 — it's the biggest net-new surface from the MySQL interview so far, warrants its own vault design doc (`vault/Features/Plan Viewer.md` when we tackle it).

2. **An "Explain" row-action in the Activity pane** pipes `EXPLAIN FOR CONNECTION N` against a running session into the Plan Viewer pane. Natural workflow: DBA sees a slow query in the Activity pane, clicks "Plan", gets the current plan without interrupting the query. T2 UX hook gated on the Plan Viewer existing.

| Command / feature | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `EXPLAIN stmt` | `DESCRIBE stmt`, `DESC stmt` | ✓ passthrough | T1 | Default tabular plan: `id / select_type / table / type / possible_keys / key / key_len / ref / rows / filtered / Extra`. Works as-is; the terminal table view handles it fine. |
| `EXPLAIN FORMAT=TRADITIONAL stmt` | — | ✓ passthrough | T1 | Same output as default EXPLAIN, format name explicit. MySQL 5.7+. |
| `EXPLAIN FORMAT=TREE stmt` | — | ⚠ ugly passthrough | T2 | MySQL 8.0.16+. Returns a single-column text blob with indented operator tree. Passthrough works, but our terminal table view renders it as a single column containing a multi-line blob that gets truncated. T2 fix: detect "single-column result with `\n` in content" shape and render full-width in a monospace code block. Cheap and shared — also lights up EXPLAIN ANALYZE, SHOW CREATE TABLE results, and other long-text content. |
| `EXPLAIN FORMAT=JSON stmt` | — | ⚠ ugly passthrough | T3 | MySQL 5.7+. Deeply nested JSON tree — useless as a raw column. **Dedicated Plan Viewer pane** (see above). New `PaneKind::PlanViewer`, three-view tab strip (Raw JSON / Readable Text / Graphical Tree), entry in `DbmsPlane::supported_panes()`, vault design doc. Biggest T3 item in the MySQL interview. |
| `EXPLAIN ANALYZE stmt` | — | ✓ passthrough | T1 | MySQL 8.0.18+. Runs the query AND reports tree-format plan with actuals (actual row counts, actual time, loops). Same single-column text-blob shape as FORMAT=TREE. The Round-4 T2 long-text rendering lights this up too; the content is more valuable. |
| `EXPLAIN ANALYZE FORMAT=TREE stmt` | — | ✓ passthrough | T1 | Same as above; TREE is the only valid FORMAT for EXPLAIN ANALYZE. |
| `EXPLAIN FOR CONNECTION N stmt` | — | ✓ passthrough | T1 | Inspect the currently-running plan for a specific connection_id. Critical diagnostic flow. Works as-is. |
| Activity pane "Explain" row-action | — | ✗ pending | T2 UX hook | Button in the Activity pane's sessions table (next to the existing kill action) that runs `EXPLAIN FOR CONNECTION <pid>` against the current session, captures the JSON plan, routes it into the Plan Viewer pane. **Gated on Plan Viewer existing** — the hook is pointless without a useful viewer. |
| Plan Viewer pane (new pane type) | — | ✗ pending | T3 | New `PaneKind::PlanViewer`. Three views (Raw JSON / Readable Text / Graphical Tree). Shared render primitives so the three views operate on the same parsed plan. Intended to host BOTH EXPLAIN FORMAT=JSON output AND optimizer trace output — the Readable Text and Graphical Tree primitives are general enough to cover trace steps (sequential, not a tree) as a secondary shape. Vault design doc TBD. |
| `EXPLAIN EXTENDED stmt` | — | ⚠ deprecated | T1 legacy | Deprecated in MySQL 5.7+, extended info now default. Still passes through without error; listed so DBAs following old docs aren't confused. |
| `EXPLAIN PARTITIONS stmt` | — | ⚠ deprecated | T1 legacy | Deprecated in MySQL 5.7+, partition info now always included. Same handling. |
| `SHOW WARNINGS` after `EXPLAIN EXTENDED` | — | ✓ passthrough | T1 legacy | Legacy flow for the optimizer's post-rewrite SQL. Still works. If the Round-2 `warnings` / `\W` auto-show toggle is on, the rewrite surfaces automatically after the EXPLAIN runs. |
| Optimizer trace flow (`SET optimizer_trace='enabled=on'` → query → `SELECT * FROM information_schema.optimizer_trace`) | — | ✓ passthrough as-is, T3 for rich view | T1 today, T3 future | Three-statement sequence all passes through as SQL. The OPTIMIZER_TRACE column is a JSON blob an order of magnitude larger than EXPLAIN FORMAT=JSON with per-step rewrite/optimization decisions. **Shares Plan Viewer infrastructure** — the Readable Text and Graphical Tree primitives are general enough to render trace steps (they're sequential, not a hierarchical tree). Lands as a secondary shape in the Plan Viewer pane, not a separate pane type. |
| `optimizer_switch` tuning | — | ✓ passthrough | T1 | `SET SESSION optimizer_switch='index_merge=off';` — plain SQL. Listed for completeness. |
| Query hints (`/*+ HINT */`, `USE INDEX`, `FORCE INDEX`, `IGNORE INDEX`) | — | — | → Axis 2 DQL | SQL syntax features, not meta-commands. Flagged here so they don't get re-proposed; they belong in the Axis 2 DQL inventory pass. |
| `SHOW PROFILES` / `SHOW PROFILE [FOR QUERY n]` | — | ✓ passthrough | T1 (Round 1 cross-ref) | Already in Round 1 — deprecated in MySQL 8.0 but still works. Listed here as a cross-ref because it's plan-diagnosis-adjacent. |

**Round 4 summary:**

- 16 rows catalogued. Most are T1 passthrough (the SQL path already works). Three non-trivial items:
  1. **T2 cheap win — long-text result rendering.** Detect single-column multi-line results and switch to a full-width monospace block. Lights up `EXPLAIN FORMAT=TREE`, `EXPLAIN ANALYZE`, and a handful of other long-text results (`SHOW CREATE TABLE`, etc). Maybe 40 lines in `render_entry`. Suggested landing order: do this first.
  2. **T3 new pane — Plan Viewer.** Three-view (Raw JSON / Readable Text / Graphical Tree) pane for `EXPLAIN FORMAT=JSON` and, as a secondary shape, the optimizer trace flow. Biggest T3 item from the MySQL interview. Requires its own vault design doc.
  3. **T2 UX hook — Activity pane → Plan Viewer.** "Explain this query" row-action in the Activity pane's sessions table. Gated on the Plan Viewer existing.

- **Optimizer trace and EXPLAIN FORMAT=JSON share Plan Viewer infrastructure** rather than each getting their own pane. One pane type, multiple plan shapes (tree and sequential-steps). Keeps the product surface smaller without sacrificing capability.

- **Query hints are deferred to Axis 2 DQL** — they're SQL syntax features, not meta-commands. Explicitly flagged so the boundary is clear.

- **No BD / BL entries this round** — everything in query planning is either implemented (T1 passthrough) or pending infrastructure (T2/T3). The only blockers are "is the result rendered usefully" questions, not browser-capability ones.

### Admin

Server-level maintenance and control — the commands a DBA runs to *change the server's state or physical storage*, not introspect it. Almost every row passes through as plain SQL; MySQL's admin surface is intentionally SQL-based so clients don't re-implement a second control plane. The job here is cataloguing parity and identifying the one shell-layer primitive worth building: a **`\safe-mode` confirmation gate** for the worst destructive commands.

**Big decision locked in this round: `\safe-mode` shell primitive.**

- **Plane-level method, not shell-hardcoded.** `DbmsPlane::destructive_patterns() -> &'static [&'static str]` returns a list of SQL-pattern prefixes (case-insensitive) the shell should gate when safe-mode is active. Each plane defines its own list — MySQL's is different from Postgres's (`DROP TABLE CASCADE` for example), and future DBMSes get to opt in. More flexible than a hardcoded shell check.
- **Off by default.** Most DBAs find confirmation-on-every-drop infantilizing and will disable it immediately. Opt-in per session via `\safe-mode on` / `\safe-mode off`. Not a global preference; not persistent across reconnects.
- **Initial pattern list for MySQL:** `DROP DATABASE`, `DROP SCHEMA`, `DROP TABLE`, `TRUNCATE`, `SHUTDOWN`, `RESET MASTER`, `RESET BINARY LOGS AND GTIDS`, `DROP TABLESPACE`. `UPDATE` / `DELETE` without `WHERE` are *not* on the initial list — pattern-matching those correctly is a higher-bar item that deserves its own round. Start conservative, expand later.
- **UX shape:** when safe-mode intercepts a statement, the shell shows a one-line inline prompt (`"⚠ destructive: `DROP TABLE orders`. Type 'yes' to proceed, anything else to cancel:"`) and blocks the submit loop until the user confirms. A new `MetaAction::ConfirmThenRun { sql: String, reason: String }` variant handles the round-trip.

| Command / feature | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `FLUSH PRIVILEGES` | — | ✓ passthrough | T1 | Reload grant tables after manual changes to `mysql.user`. |
| `FLUSH TABLES` | — | ✓ passthrough | T1 | Close and reopen all tables. |
| `FLUSH TABLES WITH READ LOCK` | `FTWRL` (jargon) | ✓ passthrough | T1 | Global read lock for backup coordination. **Dangerous hold** — if the holding session dies without `UNLOCK TABLES`, every writer on the server blocks. Worth surfacing in the Activity pane's blocker tree when detected. |
| `FLUSH TABLES tbl1, tbl2 WITH READ LOCK` | — | ✓ passthrough | T1 | Scoped version. |
| `FLUSH LOGS` | — | ✓ passthrough | T1 | Rotate error / general / slow / binary logs. |
| `FLUSH HOSTS` | — | ✓ passthrough | T1 | Clear the host cache. |
| `FLUSH STATUS` | — | ✓ passthrough | T1 | Reset session/global status counters. |
| `FLUSH QUERY CACHE` | — | ✓ passthrough | T1 legacy | Query cache removed in MySQL 8.0 — errors on modern servers. Kept for pre-8.0 users. |
| `FLUSH USER_RESOURCES` | — | ✓ passthrough | T1 | Reset per-user MAX_QUERIES_PER_HOUR etc. |
| `RESET QUERY CACHE` | — | ✓ passthrough | T1 legacy | Also query-cache-era. |
| `RESET PERSIST [var]` | — | ✓ passthrough | T1 | Removes persisted variables from `mysqld-auto.cnf`. MySQL 8.0+. |
| `OPTIMIZE [NO_WRITE_TO_BINLOG | LOCAL] TABLE tbl1, tbl2, ...` | — | ✓ passthrough | T1 | Rebuilds storage for fragmented tables. Long-running on big tables; "show progress in the shell" is a future UX item, not this round. |
| `ANALYZE [NO_WRITE_TO_BINLOG | LOCAL] TABLE tbl1, tbl2, ...` | — | ✓ passthrough | T1 | Update optimizer statistics. |
| `CHECK TABLE tbl1, tbl2, ... [FOR UPGRADE | QUICK | FAST | MEDIUM | EXTENDED | CHANGED]` | — | ✓ passthrough | T1 | Consistency check. Common pre-migration step. |
| `REPAIR [NO_WRITE_TO_BINLOG | LOCAL] TABLE tbl1, tbl2, ... [QUICK] [EXTENDED] [USE_FRM]` | — | ✓ passthrough | T1 | MyISAM-only in modern MySQL. |
| `CHECKSUM TABLE tbl1, tbl2, ... [QUICK | EXTENDED]` | — | ✓ passthrough | T1 | Row-data checksum. Used for replication verification workflows. |
| `TRUNCATE [TABLE] tbl` | — | ✓ passthrough | T1 + **safe-mode gate** | Deletes all rows without firing DELETE triggers. Fast but destructive — implicit-commit DDL on most engines, no rollback even inside a transaction. On `\safe-mode on`. |
| `RENAME TABLE old_tbl TO new_tbl [, ...]` | — | ✓ passthrough | T1 | Atomic rename; chainable for table swaps. Not a safe-mode candidate — usually deliberate. |
| `DROP DATABASE [IF EXISTS] dbname` | `DROP SCHEMA` | ✓ passthrough | T1 + **safe-mode gate** | Top of the "oh no" list. On `\safe-mode on`. |
| `DROP TABLE [IF EXISTS] tbl1, tbl2, ...` | — | ✓ passthrough | T1 + **safe-mode gate** | Second "oh no". On `\safe-mode on`. |
| `LOCK TABLES tbl1 [READ|WRITE] [, ...]` | — | ✓ passthrough | T1 | Explicit table-level locks. Changes session semantics — after LOCK TABLES you can only touch locked tables until UNLOCK. Common in legacy scripts; rare in modern code. |
| `UNLOCK TABLES` | — | ✓ passthrough | T1 | Releases LOCK TABLES locks AND implicitly releases FTWRL from `FLUSH TABLES WITH READ LOCK`. |
| `CACHE INDEX tbl1, ... [PARTITION spec] IN key_cache_name` | — | ✓ passthrough | T1 | MyISAM-only key cache assignment. Niche. |
| `LOAD INDEX INTO CACHE tbl1 [, ...]` | — | ✓ passthrough | T1 | Preload MyISAM indexes into the key cache. Niche. |
| `SHUTDOWN` | — | ✓ passthrough | T1 + **safe-mode gate** | Stops the server. Requires `SHUTDOWN` privilege. The most destructive passthrough command in MySQL. On `\safe-mode on`. |
| `INSTALL PLUGIN plugin_name SONAME 'shared_library_name'` | — | ✓ passthrough | T1 | Load a plugin. Operationally sensitive — in PAM mode `AccessPolicy` would gate it. |
| `UNINSTALL PLUGIN plugin_name` | — | ✓ passthrough | T1 | |
| `INSTALL COMPONENT 'component_name'` | — | ✓ passthrough | T1 | MySQL 8.0+ component model (successor to plugins). |
| `UNINSTALL COMPONENT 'component_name'` | — | ✓ passthrough | T1 | |
| `CREATE TABLESPACE name ADD DATAFILE '...' ...` | — | ✓ passthrough | T1 | InnoDB tablespace management. |
| `DROP TABLESPACE name` | — | ✓ passthrough | T1 + **safe-mode gate** | Can take data with it. On `\safe-mode on`. |
| `ALTER TABLESPACE name ...` | — | ✓ passthrough | T1 | |
| `CREATE LOGFILE GROUP ...` / `DROP LOGFILE GROUP ...` | — | ✓ passthrough | T1 | NDB-specific; extremely niche for InnoDB workloads. |
| `RESET MASTER` / `RESET BINARY LOGS AND GTIDS` | — | ✓ passthrough | T1 + **safe-mode gate** (Round 5 cross-ref) | Also listed in Round 5 as "very destructive". On `\safe-mode on`. |
| **`\safe-mode on` / `\safe-mode off`** (new meta-command) | — | ✗ pending | T2 | New per-session toggle. Plane-driven via `DbmsPlane::destructive_patterns()`. Shell-layer intercept of SQL content (not just backslash commands). New `MetaAction::ConfirmThenRun { sql, reason }` variant. Off by default. |
| `mysqladmin` CLI wrappers (`mysqladmin shutdown`, `mysqladmin flush-privileges`, `mysqladmin processlist`, etc.) | — | — | **out of scope** | Separate binaries that wrap the SQL commands. Our shell supports the SQL forms; the CLI binary wrappers are not in-scope for a browser SQL client. |
| `BACKUP TABLE` / `RESTORE TABLE` | — | — | **out of scope** | Deprecated in MySQL 5.0, removed in 5.5. Modern backup is `mysqldump`, `XtraBackup`, or `mysql-clone-plugin` — none of which are CLI-interactive-shell operations. |

**Round 7 summary:**

- ~35 rows catalogued. The admin surface is overwhelmingly T1 passthrough — that's the right outcome, because MySQL's admin commands are SQL and our job is not to re-implement them. The cataloguing value is in knowing every command is accounted for.
- **One new T2 item — `\safe-mode`.** Plane-driven destructive-command confirmation gate. `DbmsPlane::destructive_patterns() -> &'static [&'static str]` returns the gatable SQL-prefix list per plane. Shell intercepts SQL content on `\safe-mode on`, prompts for explicit `yes` confirmation, forwards only on confirm. Off by default; per-session opt-in. New `MetaAction::ConfirmThenRun { sql, reason }` variant.
- **Initial MySQL gate list (8 patterns):** `DROP DATABASE`, `DROP SCHEMA`, `DROP TABLE`, `TRUNCATE`, `SHUTDOWN`, `RESET MASTER`, `RESET BINARY LOGS AND GTIDS`, `DROP TABLESPACE`. `UPDATE` / `DELETE` without `WHERE` are reserved for a later round (pattern-matching those correctly is harder than a simple prefix-match).
- **Three new shell-UX primitives are accumulating across rounds**, all candidates for a shared "shell intercepts SQL content" infrastructure:
  1. Round 6 — **advisory pattern** (`passthrough + informational hint`, for `SELECT INTO OUTFILE` etc.).
  2. Round 7 — **confirm-then-run pattern** (`\safe-mode` gate, for destructive commands).
  3. Round 2 — **post-query hook pattern** (`warnings` / `\W` auto-show).
  These three should share plumbing when implementation lands: a pipeline of "shell sees SQL about to run → shell sees SQL finished running" observers the plane can tap. Worth a single design doc when we start building any of them.
- **Query cache rows flagged as "T1 legacy"** so readers know they error on modern servers but are kept for inventory completeness and pre-8.0 coverage.
- **`mysqladmin` and deprecated `BACKUP/RESTORE TABLE` explicitly out of scope**, flagged so nobody re-proposes them in a future round.

### Auth

User and privilege management — creating / dropping / altering users and roles, granting and revoking privileges, inspecting grants, password management, authentication plugin configuration. Everything in `mysql.user` / `mysql.role_edges` / `mysql.global_grants` and friends that's exposed through SQL. Excludes `FLUSH PRIVILEGES` (Round 7, cross-referenced below). Excludes PAM/SSO at the app level — that's a different design, see [[PAM Integration Seams]].

**Round-7 safe-mode list expanded to 10 patterns.** Based on this round's cataloguing, `DROP USER` and `DROP ROLE` join the initial MySQL safe-mode pattern list. They're destructive to live systems in the same shape as `DROP TABLE` — dropping a user revokes access immediately and can take down dependent apps; dropping a role yanks privileges from everyone who held it. Updated initial list:

`DROP DATABASE`, `DROP SCHEMA`, `DROP TABLE`, `TRUNCATE`, `DROP USER`, `DROP ROLE`, `SHUTDOWN`, `RESET MASTER`, `RESET BINARY LOGS AND GTIDS`, `DROP TABLESPACE`.

| Command / feature | Aliases | Status | Tier | Notes |
|---|---|---|:---:|---|
| `CREATE USER [IF NOT EXISTS] user [IDENTIFIED WITH plugin BY 'password' / AS 'hash'] [REQUIRE ...] [WITH resource_options ...] [password_option ...] [lock_option]` | — | ✓ passthrough | T1 | Wide clause surface — auth plugin, TLS requirements, resource limits, password expiry, account locking. Works as plain SQL. The "Create User wizard" T3 item below is the ergonomic follow-up for v0.0.2+. |
| `DROP USER [IF EXISTS] user [, user, ...]` | — | ✓ passthrough | T1 + **safe-mode gate** | Destructive to live systems — revokes access immediately, can break dependent apps. **Added to the Round-7 safe-mode pattern list.** |
| `ALTER USER [IF EXISTS] user [IDENTIFIED WITH ... BY ...] [REQUIRE ...] [WITH ...] [PASSWORD EXPIRE ... / HISTORY n / REUSE INTERVAL n DAY / REQUIRE CURRENT] [ACCOUNT LOCK | UNLOCK]` | — | ✓ passthrough | T1 | The "change anything about a user" command. Same wide clause surface as CREATE USER. |
| `ALTER USER user DEFAULT ROLE role [, role, ...]` / `ALTER USER user DEFAULT ROLE ALL` / `ALTER USER user DEFAULT ROLE NONE` | — | ✓ passthrough | T1 | MySQL 8.0+. Sets the roles activated on login. |
| `RENAME USER old_user TO new_user [, old_user TO new_user, ...]` | — | ✓ passthrough | T1 | Rename accounts. Useful for service-account housekeeping. |
| `SET PASSWORD [FOR user] = 'new_password'` / `SET PASSWORD [FOR user] TO RANDOM` | — | ✓ passthrough | T1 | Legacy password-change path; `ALTER USER` is the modern preferred form but SET PASSWORD still works. `TO RANDOM` (MySQL 8.0+) generates a random password server-side and returns it in the result. |
| `GRANT priv_list ON object TO user [, user, ...] [WITH GRANT OPTION] [AS user WITH ROLE ...]` | — | ✓ passthrough | T1 | Main privilege-granting statement. Wide object surface (`*.*`, `db.*`, `db.tbl`, `db.proc` + procedure/function variants), wide priv surface (SELECT/INSERT/UPDATE/DELETE/ALL PRIVILEGES/CREATE/DROP/ALTER/INDEX/EXECUTE/FILE/RELOAD/PROCESS/SUPER/…), role grants. All SQL. |
| `REVOKE priv_list ON object FROM user [, user, ...]` | — | ✓ passthrough | T1 | Inverse of GRANT. |
| `REVOKE ALL PRIVILEGES, GRANT OPTION FROM user [, user, ...]` | `REVOKE ALL FROM user` | ✓ passthrough | T1 | The "yank everything" form; common cleanup step before DROP USER. |
| `REVOKE role FROM user` | — | ✓ passthrough | T1 | Role-revocation (MySQL 8.0+). |
| `REVOKE PROXY ON user FROM user` | — | ✓ passthrough | T1 | Proxy user unlinking. Legacy-ish. |
| `SHOW GRANTS [FOR user [USING role [, role, ...]]]` | — | ✓ passthrough + T2 renderer | T1 + T2 polish | Returns grants as a one-column list of literal `GRANT ... TO 'user'@'host'` statement strings. Works as-is but is dense to read. **T2 polish renderer**: structured cards that break each grant-statement row into fields (user · privileges list · object scope · grant-option flag · AS-role clause). Joins the existing rendering-polish track from Rounds 4 (long-text), 5 (wide-result auto-vertical), and 6 (save-result-as). |
| `SHOW GRANTS FOR CURRENT_USER()` | `SHOW GRANTS` (when no user specified) | ✓ passthrough | T1 | The "what can I do?" shortcut. Benefits from the same T2 renderer above. |
| `SHOW PRIVILEGES` | — | ✓ passthrough | T1 | Lists the privileges the server supports and what they apply to. Reference-material, rarely interactive. |
| `SHOW CREATE USER user` | — | ✓ passthrough | T1 | Returns the CREATE USER statement that would recreate the account. Useful for documenting or replicating user definitions. |
| `CREATE ROLE [IF NOT EXISTS] role [, role, ...]` | — | ✓ passthrough | T1 | MySQL 8.0+. Roles are users-that-you-GRANT-to-other-users; they group privileges. |
| `DROP ROLE [IF EXISTS] role [, role, ...]` | — | ✓ passthrough | T1 + **safe-mode gate** | **Added to Round-7 safe-mode pattern list.** Dropping a role yanks privileges from everyone who held it. |
| `GRANT role [, role, ...] TO user [, user, ...]` | — | ✓ passthrough | T1 | Grant roles to users. |
| `SET ROLE {DEFAULT | NONE | ALL | ALL EXCEPT role_list | role_list}` | — | ✓ passthrough | T1 | Activate roles for the current session. MySQL 8.0+. |
| `SET DEFAULT ROLE ... TO user` | `ALTER USER user DEFAULT ROLE ...` | ✓ passthrough | T1 | Persist the default activation for future logins. |
| `CURRENT_ROLE()` | — | ✓ passthrough | T1 | SQL function returning the currently active roles for this session. Reference-material. |
| `ROLES_GRAPHML()` | — | ✓ passthrough | T1 | MySQL 8.0+ SQL function returning the server's role hierarchy as GraphML XML. Candidate data source for the Permissions Explorer pane's graphical view. |
| `FLUSH PRIVILEGES` | — | ✓ passthrough | T1 (Round 7 cross-ref) | Already listed in Round 7. Re-listed here as a cross-ref because DBAs look for it in the auth context after manual grant-table edits. |
| Auth plugin clauses: `IDENTIFIED WITH caching_sha2_password / mysql_native_password / sha256_password / authentication_ldap_sasl / authentication_kerberos / authentication_fido / authentication_webauthn` | — | ✓ passthrough | T1 | Server-side auth plugins. The user names the plugin as part of `CREATE USER` / `ALTER USER`; MySQL handles plumbing. We don't interact with plugin internals. |
| Password validation component (`validate_password.*` system variables) | — | ✓ passthrough | T1 | Component-level password policy (min length, char classes, dictionary check). Configured via `SET GLOBAL validate_password.*`. Plain SQL. |
| Account resource limits (`MAX_QUERIES_PER_HOUR`, `MAX_UPDATES_PER_HOUR`, `MAX_CONNECTIONS_PER_HOUR`, `MAX_USER_CONNECTIONS`) | — | ✓ passthrough | T1 | Set via `CREATE USER` / `ALTER USER` clauses. Plain SQL. |
| `CREATE USER ... REQUIRE {NONE | SSL | X509 | CIPHER ... | ISSUER ... | SUBJECT ...}` | — | ✓ passthrough | T1 | TLS/certificate requirements. Plain SQL. |
| `PASSWORD('str')` function | — | ✗ removed | T1 legacy | Pre-5.7 password-hash function for direct `UPDATE mysql.user` edits. Removed in MySQL 8.0. Listed for historical completeness. |
| `OLD_PASSWORD('str')` function | — | ✗ removed | T1 legacy | Same story — removed. |
| Direct `UPDATE mysql.user` edits | — | ✓ passthrough | T1 legacy | Pre-5.7 way to manually edit user records. Still works if the table retains the expected columns; `ALTER USER` is strongly preferred. Listed for completeness. |
| **"Create User" wizard** (new UX) | — | ✗ pending | T3 deferred | Structured form that builds `CREATE USER` / `ALTER USER` statements from inputs (username, host, auth plugin, password, TLS requirements, resource limits, role assignments, default role). **Planned but deferred** — on the roadmap for v0.0.2+. Not urgent; DBAs who manage users daily have scripts. Valuable as an ergonomic onboarding feature for less-experienced users and for visually building complex multi-clause statements. |
| **Permissions Explorer pane** (new pane type) | — | ✗ pending | T3 deferred | A new pane showing "who has access to what" — structured visualization of grants across users / roles / objects, role-hierarchy graphical view (use `ROLES_GRAPHML()` as the data source), "effective permissions for this user right now" resolution. Similar object-browser-plus-inspector shape to the Schema Explorer, scoped to auth objects. **Planned but deferred.** Valuable in its own right — access audit and "who did we just grant SELECT on revenue to?" are workflows that benefit every DBA, not just enterprise/PAM users. Parks on the roadmap; future vault design doc when implementation starts. |

**Round 8 summary:**

- ~28 rows catalogued. Auth is the biggest all-T1-passthrough round — MySQL's user and privilege management is entirely SQL-driven, and our job is to catalog it, not re-implement it.

- **Safe-mode pattern list expanded to 10** (was 8 from Round 7): adding `DROP USER` and `DROP ROLE` as destructive-to-live-systems shapes. `DbmsPlane::destructive_patterns()` for MySQL now covers: `DROP DATABASE`, `DROP SCHEMA`, `DROP TABLE`, `TRUNCATE`, `DROP USER`, `DROP ROLE`, `SHUTDOWN`, `RESET MASTER`, `RESET BINARY LOGS AND GTIDS`, `DROP TABLESPACE`.

- **One T2 rendering-polish item**: structured renderer for `SHOW GRANTS` output. Joins the existing rendering-polish track (Rounds 4/5/6) — four items now share that track and should land together.

- **Two T3 deferred items** both explicitly on the roadmap:
  1. **Create User wizard** — ergonomic form for building `CREATE USER`/`ALTER USER` statements. v0.0.2+ item; valuable for less-experienced users and for complex multi-clause statements.
  2. **Permissions Explorer pane** — structured "who has access to what" visualization. Valuable in its own right for access-audit workflows across all deployment modes, not just enterprise/PAM. Future vault design doc when implementation starts.

- **Legacy removed items** (PASSWORD(), OLD_PASSWORD(), direct mysql.user UPDATE) listed for historical completeness with clear "removed in 8.0" annotation. No special handling — modern servers return clear error messages, pre-5.7 users get passthrough.


---

## SQL dialect features (Axis 2)

Axis 2 is not about meta-commands. We don't implement MySQL's SQL dialect — the server does. Our responsibilities are the **four places** where SQL dialect coverage shows up on our side (from [[Dialect Coverage Framework]]):

1. **Syntax highlighting** — each plane's keyword + function list. A gap means a keyword renders as plain identifier. Cosmetic but visible.
2. **Tab completion** — each plane's keyword set plus schema metadata. Covered by the T3 tab-completion item from Round 9.
3. **Multi-statement parsing** — the shell must know where one statement ends and the next begins. Covered by the T2 parser item from Round 3.
4. **Dialect-specific UX hooks** — detecting `BEGIN`/`COMMIT` for a transaction-state indicator, detecting DDL for schema-cache invalidation, etc. Plane-driven observers.

We don't enumerate every keyword here. SQL-92 baseline is assumed; we only call out **vendor-specific clauses, constructs, and quirks** that need attention from one of the four responsibilities. Most of what follows is a cue for a single **keyword/function list audit pass** against the MySQL 8.4 reference manual — tracking ~40 individual "add X to the keyword list" items as separate tickets is bureaucratic overhead; one consolidated audit work item captures it.

### DQL — Data Query Language (vendor-specific)

| Feature | Responsibility | Tier | Notes |
|---|---|:---:|---|
| CTEs (`WITH`, `RECURSIVE`) | Highlighter | T1 polish (audit) | Already in `MYSQL_KEYWORDS`. Verify they stay highlighted as keywords. |
| Window functions (`OVER`, `PARTITION BY`, `ROWS BETWEEN`, `RANGE`, frame specs, `ROW_NUMBER()`, `RANK()`, `LEAD`/`LAG`/`NTH_VALUE`/`FIRST_VALUE`/`LAST_VALUE`/`CUME_DIST`/`PERCENT_RANK`/`NTILE`) | Highlighter | T1 polish (audit) | MySQL 8.0+. Most in keyword/function lists; audit against 8.4 reference. |
| JSON functions (`JSON_OBJECT`, `JSON_ARRAY`, `JSON_EXTRACT`, `JSON_SET`, `JSON_INSERT`, `JSON_REMOVE`, `JSON_CONTAINS`, `JSON_TABLE`, `JSON_VALUE`, `->` / `->>`) | Highlighter | T1 polish (audit) | Some in `MYSQL_FUNCTIONS`; `->` / `->>` are operators, not functions — operator highlighting is a gap worth flagging. |
| MySQL-specific functions — `GROUP_CONCAT`, `FIELD()`, `FIND_IN_SET()`, `MATCH() AGAINST()`, `UUID_SHORT()`, `INET6_ATON()`, `ST_*` spatial, `CONVERT_TZ()`, `PERIOD_ADD()` | Highlighter | T1 polish (audit) | Common ones covered; spatial and newer additions likely missing. Not breaking — rendered as plain identifiers when missed. |
| Query hints (`/*+ USE INDEX(...) */`, `FORCE INDEX`, `STRAIGHT_JOIN`, `MAX_EXECUTION_TIME`, `SET_VAR`, `QB_NAME`) | Highlighter | T2 niche | The `/*+ ... */` form is currently a block comment; hint names aren't flagged distinctly. Polish would detect the `/*+` prefix. Niche. |
| `MATCH ... AGAINST` fulltext | Highlighter | T1 polish (audit) | `IN BOOLEAN MODE`, `IN NATURAL LANGUAGE MODE`, `WITH QUERY EXPANSION`. |
| Regex operators — `REGEXP`, `RLIKE`, `REGEXP_REPLACE`, `REGEXP_SUBSTR`, `REGEXP_LIKE`, `REGEXP_INSTR` | Highlighter | T1 polish (audit) | Keyword forms covered; function forms may be missing. |
| Vector type (MySQL 8.4+) / `CREATE VECTOR INDEX` / `VECTOR`-specific functions | Highlighter | T1 polish (audit) | New in 8.4. Audit scope. |

### DML — Data Manipulation Language (vendor-specific)

| Feature | Responsibility | Tier | Notes |
|---|---|:---:|---|
| `INSERT ... ON DUPLICATE KEY UPDATE` | Highlighter | T1 polish (audit) | MySQL-specific idiom; partially covered. |
| `INSERT IGNORE` | Highlighter | ✓ | `IGNORE` already covered. |
| `UPDATE ... LIMIT n` / `DELETE ... LIMIT n` | Highlighter | ✓ | MySQL-specific; works via existing keyword coverage. |
| `REPLACE INTO` | Highlighter | ✓ | `REPLACE` covered. MySQL-specific; no standard-SQL equivalent. |
| **Transaction-state UX hook** (new) — detect `BEGIN` / `START TRANSACTION` / `COMMIT` / `ROLLBACK` and surface a visible in-transaction indicator in the shell toolbar / pane tab | Dialect-specific UX hook | T2 | **New UX feature emerging from Axis 2.** Referenced in earlier vault docs (`Transaction State Indicator.md`, `Next Up` list) — formalized here with trigger patterns. Yellow dot in the shell toolbar + "TRANSACTION" badge on the pane tab while a session is in an open transaction. Joins the **shell-layer SQL intercept track**. |

### DDL — Data Definition Language (vendor-specific)

| Feature | Responsibility | Tier | Notes |
|---|---|:---:|---|
| `CREATE TABLE ... ENGINE={InnoDB | MyISAM | ARCHIVE | MEMORY | NDB | CSV | BLACKHOLE | FEDERATED}` | Highlighter | T1 polish (audit) | Engine names not highlighted distinctly. Minor. |
| Table options — `CHARACTER SET`, `COLLATE`, `ROW_FORMAT`, `COMPRESSION`, `STATS_PERSISTENT`, `KEY_BLOCK_SIZE` | Highlighter | T1 polish (audit) | |
| Partitioning — `PARTITION BY {RANGE | LIST | HASH | KEY}`, `SUBPARTITION BY`, `VALUES LESS THAN` | Highlighter | T1 polish (audit) | |
| Generated columns — `col TYPE GENERATED ALWAYS AS (expr) [VIRTUAL | STORED]` | Highlighter | T1 polish (audit) | `GENERATED`, `VIRTUAL`, `STORED`. |
| Invisible columns — `col TYPE INVISIBLE` (8.0+) | Highlighter | T1 polish (audit) | |
| Full-text / Spatial indexes — `FULLTEXT INDEX`, `SPATIAL INDEX` | Highlighter | T1 polish (audit) | |
| Index algorithms — `USING {BTREE | HASH}` | Highlighter | ✓ | Covered. |
| Online-DDL hints — `ALGORITHM={DEFAULT | INPLACE | COPY | INSTANT}`, `LOCK={DEFAULT | NONE | SHARED | EXCLUSIVE}` | Highlighter | T1 polish (audit) | Useful for DBAs doing schema migrations. |
| Event scheduler — `CREATE EVENT ... ON SCHEDULE EVERY n SECOND/MINUTE/HOUR/DAY/MONTH/YEAR` | Highlighter | T1 polish (audit) | |
| **Schema-cache invalidation UX hook** (new) — detect `CREATE` / `ALTER` / `DROP` / `RENAME` / `TRUNCATE` statements run in THIS shell and invalidate the Schema Explorer's cached tree for this connection | Dialect-specific UX hook | T2 | **New UX feature emerging from Axis 2.** **Best-effort and explicitly bounded** — the hook catches DDL run in our shell, but other connections (DBAs in another tool, ORM-driven app migrations, ops scripts) can change the schema and our cache can't know. Documented caveat: "if you don't see an expected object, use the Schema Explorer's refresh button". No cross-user invalidate signal, no WebSocket push, no pretense of comprehensiveness — this is a convenience for the common case of running DDL in our shell. The existing Schema Explorer refresh (↻) button is the escape hatch for stale state from out-of-band changes. Joins the shell-layer SQL intercept track. |

### DCL — Data Control Language

Already catalogued in Round 8 (Auth). Cross-referenced — no new Axis 2 rows.

### TCL — Transaction Control Language (vendor-specific)

| Feature | Responsibility | Tier | Notes |
|---|---|:---:|---|
| `BEGIN` / `START TRANSACTION [WITH CONSISTENT SNAPSHOT | READ WRITE | READ ONLY]` | Highlighter + UX hook (DML section above) | ✓ | Keywords covered; transaction-state indicator catches the triggering. |
| `COMMIT [AND CHAIN] [RELEASE | NO RELEASE]` | Highlighter + UX hook | ✓ | |
| `ROLLBACK [TO SAVEPOINT name] [AND CHAIN]` | Highlighter + UX hook | ✓ | |
| `SAVEPOINT name` / `RELEASE SAVEPOINT name` | Highlighter | ✓ | |
| `SET TRANSACTION [READ WRITE | READ ONLY] [ISOLATION LEVEL ...]` | Highlighter | T1 polish (audit) | Isolation-level keywords; audit scope. |
| `LOCK TABLES` / `UNLOCK TABLES` | — | — | Round 7 cross-ref. |
| `XA START` / `XA END` / `XA PREPARE` / `XA COMMIT` / `XA ROLLBACK` / `XA RECOVER` | — | — | Round 5 cross-ref. |

### Session / Admin SQL

Already catalogued in Rounds 2, 5, 7. Cross-referenced — no new Axis 2 rows.

### Procedural (inside stored procedures / functions / triggers / events)

Matters only when allowing stored-routine text editing. The Schema Explorer DDL tab displays them; Round 3's `DELIMITER //` allows typing them.

| Feature | Responsibility | Tier | Notes |
|---|---|:---:|---|
| Control flow — `IF ... THEN ... ELSEIF ... ELSE ... END IF`, `CASE ... WHEN ... THEN ... ELSE ... END CASE` (statement form) | Highlighter | T1 polish (audit) | |
| Loops — `WHILE ... DO ... END WHILE`, `REPEAT ... UNTIL ... END REPEAT`, `LOOP ... END LOOP`, `ITERATE label`, `LEAVE label` | Highlighter | T1 polish (audit) | Labels don't need highlighting — they're identifiers. |
| Cursors — `DECLARE cur CURSOR FOR`, `OPEN cur`, `FETCH cur INTO var`, `CLOSE cur` | Highlighter | T1 polish (audit) | |
| Handlers — `DECLARE CONTINUE HANDLER FOR`, `DECLARE EXIT HANDLER FOR`, `DECLARE UNDO HANDLER`, `SQLSTATE '...'` | Highlighter | T1 polish (audit) | |
| Variable declarations — `DECLARE var TYPE DEFAULT expr` | Highlighter | T1 polish (audit) | |
| Signals — `SIGNAL SQLSTATE '...'`, `RESIGNAL`, `SET MESSAGE_TEXT = '...'` | Highlighter | T1 polish (audit) | |
| Routine characteristics — `DETERMINISTIC` / `NOT DETERMINISTIC`, `CONTAINS SQL` / `NO SQL` / `READS SQL DATA` / `MODIFIES SQL DATA`, `SQL SECURITY {DEFINER | INVOKER}`, `COMMENT '...'` | Highlighter | T1 polish (audit) | |

We deliberately do NOT build a separate "stored-routine editor mode" with a different highlighter inside `BEGIN ... END` blocks. Scope creep for a parity exercise; the T1 audit pass covers the keyword surface.

---

**Axis 2 summary — MySQL:**

- Most of Axis 2 reduces to a **single keyword/function list audit pass** against the MySQL 8.4 reference manual. All the `T1 polish (audit)` rows collapse into one consolidated work item rather than ~40 individual tickets. One ticket: "audit + expand `MYSQL_KEYWORDS` and `MYSQL_FUNCTIONS` against MySQL 8.4 reference, including newer (vector, invisible columns) + procedural keywords."
- **Two new UX hooks** emerged from Axis 2 that are genuinely new features:
  1. **Transaction-state indicator (T2)** — in-shell indicator when session has an open transaction. Formalizes an item that was already on the Next Up list.
  2. **Schema-cache invalidation on DDL (T2)** — best-effort refresh of the Schema Explorer's cached tree when DDL runs in THIS shell. Explicitly bounded: out-of-band changes from other connections (DBAs in other tools, ORM migrations, ops scripts) can invalidate the cache without our knowledge, and the user needs to use the existing refresh (↻) button for those cases. Not a comprehensive invalidation system; a convenience for the common case.
- **One T2 niche polish**: query-hint comment colouring (`/*+ ... */`). Low priority.
- Both new UX hooks join the **shell-layer SQL intercept track** (from Axis 1), bringing the track total to **six items** (`\W` warnings auto-show, advisory passthrough, safe-mode confirm-then-run, `\help <cmd>` detail, transaction-state indicator, schema-cache invalidation). One design doc + one infrastructure pass covers them all.

The MySQL inventory is now complete across both axes. Next step is a consolidation pass — finalize the Browser Limitations section, write the Summary section properly, cross-check every cross-reference — before starting the Postgres inventory.

---

## Browser limitations for this DBMS

Consolidated list of every BL (browser-limited) and BD (by-design-skipped) row across every category. Runtime text is what the user sees when they hit the command — surfaced via `NotSupported { reason: BrowserLimitation(...) | ByDesign(...) }` per the runtime grace contract in [[Dialect Coverage Framework]]. "Unblocks with" identifies the seam or deployment-mode prereq that would lift the limitation, where applicable.

| Command / feature | Reason | Runtime text | Unblocks with |
|---|:---:|---|---|
| `\!` / `system cmd` | BL | "browser cannot invoke OS processes; see vault/Architecture/OS Provider Seam.md" | Never (by design) — OS shell escape is permanently off the roadmap. Listed as BL for discoverability even though functionally equivalent to BD. |
| `\P pager cmd` / `pager cmd` | BD | "the shell's native scroll is the pager; no external pager exists in a browser context" | Never — replaced by native scroll. |
| `\n` / `nopager` | — | Implemented: `PAGER set to stdout`, as the CLI says it (a pane never pages). | — |
| File reads from arbitrary paths without user consent | BL | "browsers sandbox the filesystem; every read requires explicit user consent via the file picker" | [[OS Provider Seam]] `BrowserOsProvider` (user picker) or `BackendOsProvider` (backend host with policy). |
| Process spawning / script execution outside the browser sandbox | BD | "the backend is a database client, not a shell; use SSH for host-level access" | Never — by design. |
| `\T filename` (tee) | BL | "capture shell output to a browser download on stop; pending OS Provider Seam Phase 1" | [[OS Provider Seam]] `BrowserOsProvider::write_download`. |
| Drag-and-drop file attach | BL | "file attach via drag-and-drop pending OS Provider Seam Phase 1" | [[OS Provider Seam]] `BrowserOsProvider::read_files`. Deprioritized v0.0.1+. |
| `SELECT ... INTO OUTFILE 'path'` — server-side write to the MySQL server's filesystem | — (advisory, not blocked) | "this writes to the MySQL server's filesystem, not your laptop — use 'Save result as...' or `\T` to save locally" | N/A — passes through; advisory is additive. |
| `SELECT ... INTO DUMPFILE 'path'` — same | — (advisory) | Same advisory as `INTO OUTFILE`. | N/A. |
| `LOAD DATA LOCAL INFILE 'path'` | BL — deferred spike | "pending sqlx LOAD DATA LOCAL support and OS Provider Seam Phase 1; see vault/Architecture/OS Provider Seam.md" | [[OS Provider Seam]] `BrowserOsProvider` + sqlx upstream/fork work. **Explicitly deferred from v0.0.1.** |
| `LOAD XML LOCAL INFILE 'path'` | BL — deferred spike | Same as `LOAD DATA LOCAL INFILE`. | Same. |

---

## Out of scope

Features we're intentionally not implementing. Different from BD in that these are either (a) covered by a better pattern already (`--local-infile` → runtime toggle), (b) equivalent to something we already ship (mysqladmin CLI → in-shell SQL), or (c) belong to a different product entirely (`mysqlsh` utilities). Listed so a future contributor doesn't re-propose any of them.

| Feature | Why out of scope |
|---|---|
| `--local-infile[=value]` CLI startup flag equivalent | Not user-facing in our model. We rely on the server's `local_infile` variable when the LOAD DATA LOCAL spike eventually lands. |
| `--secure-file-priv` CLI startup flag | Server-side configuration. Irrelevant to the client. |
| `--batch` / `--html` / `--xml` / `--silent` / `--raw` CLI startup flags | Covered by the runtime `\output {table|vertical|csv|tsv|json|markdown|html}` toggle. Browsers don't launch with flags. |
| `--histfile` / `--no-auto-rehash` CLI startup flags | We handle persistence via `localStorage` and auto-rehash is always on given browser constraints. |
| NULL display column-width / truncation CLI flags | Browsers handle wrapping natively; won't add flags where the platform already handles it. |
| `mysqladmin` CLI wrappers (`mysqladmin shutdown`, `mysqladmin flush-privileges`, `mysqladmin processlist`, etc.) | Separate binaries that wrap SQL. Our shell supports the SQL forms; wrapping binaries aren't in-scope for a browser SQL client. |
| `BACKUP TABLE` / `RESTORE TABLE` | Deprecated in MySQL 5.0, removed in 5.5. Modern backup is `mysqldump` / `XtraBackup` / clone plugin — none are CLI-interactive-shell operations. |
| MySQL Shell (`mysqlsh`) utilities — `util.importTable()`, `util.importJson()`, `util.dumpSchemas()`, etc. | Different product. If we later benchmark against `mysqlsh` alongside classic `mysql`, they become their own inventory rows. |
| CLI-style parameter binding (`:var` substitution in the shell) | Native `mysql` doesn't have it. MySQL user variables (`SET @foo = ?`; then `? = @foo`) are the substitute and pass through as plain SQL. |
| Nested `SOURCE` (SOURCE inside a SOURCE'd file) | Native `mysql` supports it but with relative-path foot-guns and no cycle detection. We support one level only. Nested `SOURCE` emits `NotSupported { ByDesign(...) }`. |
| Separate "stored-routine editor mode" with a different highlighter inside `BEGIN ... END` blocks | Scope creep for a parity exercise. The Round 10 T1 audit pass covers the procedural keyword surface in the main highlighter. |
| Activity pane "Replication" structured sub-view | Crowded competitive landscape (Orchestrator, MySQL Workbench, Percona Monitoring) and no clear differentiator for v0.0.1. Product-positioning decision, not a parity-gap decision. Revisit if real users ask. |
| Query cache error suppression (commands error on MySQL 8.0+) | `FLUSH QUERY CACHE` / `RESET QUERY CACHE` pass through and the server's error is the right UX for modern users. Pre-8.0 users get passthrough working. Listed `T1 legacy`, not suppressed. |
| `PASSWORD()` / `OLD_PASSWORD()` SQL functions | Removed from MySQL 8.0. Modern users get a clear server error; pre-5.7 users with legacy code get passthrough. No special handling. |
| `?\G` and similar "help this shape" shortcuts | Not a native MySQL feature. Reserved for later if demand surfaces. |
| Group Replication first-class UI treatment | Niche. `START GROUP_REPLICATION` / `STOP GROUP_REPLICATION` pass through; dedicated monitoring tools cover this space better than we would. |
| XA transaction first-class UI treatment | Niche. `XA START` / `XA END` / `XA PREPARE` / `XA COMMIT` / `XA ROLLBACK` / `XA RECOVER` pass through. |
