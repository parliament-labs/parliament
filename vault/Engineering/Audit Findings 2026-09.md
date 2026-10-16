# Parliament — Project review (2026-09)

**Review date:** 2026-09-26
**Branch state:** `main` at `3d97382` (post PR #117, Storage tab Autovacuum Run action)
**Method:** 6 parallel reviewers, each with one lens — Schema Explorer, Shell, App core (main.rs / types.rs / activity / CSS), DBMS planes, Noctua + shared wire types, Docs / tests / tooling. Every High finding was then re-verified on the main thread (code read, or run live — marked below). Supersedes nothing: the [[Audit Findings 2026-06]] doc stays as the historical record; § 9 here gives the current status of each of its items.

**Verification key:** **V** = re-verified on the main thread · **L** = reproduced live against Pagila (PG 18.4) / Sakila (MySQL 9.6) · **C** = reviewer code read, not independently re-checked.

---

## 0. Baseline

- `cargo check --workspace` clean (native), `cargo check -p tyto --target wasm32-unknown-unknown` clean.
- 330 unit tests pass (tyto 281 · noctua 40 · shared 9). 69 clippy warnings, 0 errors (tyto 65, noctua 4).
- `Cargo.toml` has `default-members = ["noctua"]`, so the bare `cargo check` / `cargo clippy` / `cargo test` documented in CLAUDE.md cover **noctua only** (281 of 330 tests and 65 of 69 warnings are invisible to them). **V**
- Git on this machine fails (`exit 69`) until the Xcode license is accepted: `sudo xcodebuild -license accept`. Workaround used for this review: `DEVELOPER_DIR=/Library/Developer/CommandLineTools git …`. The session-start / pre-compact / stop hooks and `/review-diff` all silently misbehave while this is broken.

---

## 1. Executive summary

The codebase is healthy at the build level and rich in DBA-grade features. The debt concentrates in **five root causes**. Fixing them retires most of the findings below, and each maps onto work already planned (explorer polish, shell cleanup).

| # | Root cause | Where it shows up |
|---|---|---|
| **R1** | **No request correlation on the wire.** `ExecuteQuery { session_id, sql }` carries no id. The shell decides what a result "means" from ~8 latch signals (`watch_pending`, `pending_txn`, `pending_vertical`, `pending_no_rows_msg`, `pending_post_action`, `pending_stmts`, `querying`, `pending_confirm`). | Explorer Run button misattribution; multi-statement results under the wrong prompt; `\gset`/`\gexec` applied to the wrong query after reconnect; error marking on the wrong block; schema-cache invalidation reading the wrong query. |
| **R2** | **God units.** `shell.rs::render_shell_leaf` 2,560 lines (its `on_keydown` closure 1,540, the Enter arm 830 with 30 `if let MetaAction::X` blocks). `main.rs::App` 5,944 lines (WS `onmessage` 2,230). `explorer.rs::render_explorer_pane` 930. `noctua::process_message` 527. `ShellTab` = **112 fields** spanning shell + explorer + admin + activity, allocated for every pane kind. | Every behavior change touches a giant closure; 8 separate "send SQL" paths each apply a different subset of the rules; duplicated queue-advance (×3), trailing-meta-strip (×5), pane-minting (×5). |
| **R3** | **Data fidelity on the wire.** Noctua's converters call `try_get::<String>` on non-text types (sqlx rejects them); NULL travels as the string `"NULL"`; errors travel as one message string; PG notices are never forwarded; the shell treats textarea UTF-16 offsets as Rust byte offsets. | `?` in result cells for NUMERIC / JSON / UUID / DECIMAL; literal `'NULL'` indistinguishable from NULL; no SQLSTATE / DETAIL / HINT; `VACUUM VERBOSE` / `RAISE NOTICE` silent; non-ASCII input corrupts or panics. |
| **R4** | **Reactive lifetimes.** No `Owner` / `dispose` / `on_cleanup` anywhere; unguarded `pane.set(Pane::Shell)` (Leptos 0.6 `set` has no equality check) rebuilds every pane's DOM; `ws_send` uses a tracked `ws.get()`. | Scroll / undo / selection loss on reconnect or pane open; signals of closed panes never freed; `\watch` silently dies after a WS blip; stuck spinners. |
| **R5** | **Noctua trust boundary.** Any `http://localhost:<any port>` origin (or none) is accepted with no token; the client supplies `editor_command`; saved-password substitution keys on `id` but keeps the client's host. | Local command execution and saved-password exfiltration by any localhost-served page or other local user. |

---

## 2. Fix-now list (verified, independent of the larger refactors)

Small, high-value fixes. None depends on the shell or explorer restructure.

| # | Sev | Where | Defect | Fix direction | Ver |
|---|---|---|---|---|---|
| F1 | **High** | `noctua/src/main.rs:1741-1748, 1770-1785` (PG), `1596-1677` (MySQL) | Converters render `?` for **NUMERIC, JSON, JSONB, UUID, infinity / BC dates, arrays with NULL, 2-D arrays**, MySQL **DECIMAL, BIT, TIME > 24h, zero dates**; `<MONEY>` / `<TIMETZ>` / `<MACADDR>` / `<_NUMERIC>` placeholders; silently wrong: timestamptz forced to UTC (ignores `SET TIME ZONE`), bytea `\x41` → `A`, inet → `10.0.0.1/32`, `{"a,b",c}` → `{a,b,c}`, TINYINT(1)=5 → `true`. `SELECT amount FROM payment` is a column of `?`. | `raw_sql` already returns the server's text rendering: read every non-binary column with `try_get_unchecked::<Option<String>>`; keep typed handling only for BLOB / VARBINARY / BIT / GEOMETRY. Add converter unit tests (none exist beyond interval). | **V L** (probe re-run on main thread) |
| F2 | **High** | `noctua/src/main.rs:282-291, 302-310` | Any `connections.json` parse error → empty list; the next Save overwrites the file → **every saved connection lost**. Write is non-atomic, errors ignored (client still gets `ConnectionSaved`); live file is `0644`. The test at `:2011` locks in the empty-list behavior. | Quarantine a corrupt file (rename aside) and refuse to overwrite; temp + fsync + rename at `0600`; surface write errors. | **V** |
| F3 | **High** | `noctua/src/main.rs:386-392`, `:1025-1032`, `:645-655` (+ `700-707`, `784-791`) | Origin check accepts any scheme / port on localhost and a missing Origin; no auth token. `EditExternally.editor_command` comes from the client and is executed. Blank-password substitution by `id` keeps the client-supplied host / user, so `TestConnection{id:<prod>, host:<attacker>, password:""}` authenticates to the attacker with the saved password. | Exact-origin allowlist (same as CORS) + per-launch token; resolve `editor_command` server-side from the saved connection; substitute the saved password only when host / port / user / db_type match the saved record. | **V** |
| F4 | **High** | `tyto/src/shell.rs:997-1006, 1353`, previews at `1705, 2559, 2616` | Textarea UTF-16 cursor offsets used as byte offsets. `SELECT 'é', now(` → `SELECT 'é', no()w`; `SELECT 'é` + Ctrl+U **panics**; Tab → Ctrl+U → Enter panics (`replace_start` unclamped); `&sql[..77]` previews panic on multibyte text. A panic takes down the whole WASM app. | One `EditBuffer` type that converts UTF-16 ↔ byte offsets in exactly one place; `char_indices`-safe truncation helper. | **V** (offset use) / C (panic paths) |
| F5 | **High** | `noctua/src/main.rs:88-129, 411, 415` | Results fully materialized (`Vec<Row>` + `Vec<Vec<String>>` + one JSON string) through an unbounded channel. `SELECT *` on a 100M-row table OOMs noctua — every pane in every tab loses its session and open transaction (psql would only lose itself). | Default row cap with a `truncated` flag (psql-like `FETCH_COUNT` later), chunked result frames, bounded channel. | C |
| F6 | **High** | `noctua/src/main.rs:486, 493-498` | WS-close cleanup joins every in-flight task **before** dropping sessions. Pane A holds a row lock in `BEGIN`; pane B waits on it; user reloads → cleanup waits on B, B waits on A, A is never dropped → both backends leak and the lock is held indefinitely. Same with a long query or an open GUI `\e` editor. | Remove the socket's sessions and `abort_all()` first; late-finishing Connects check a per-socket `closed` flag and drop themselves. | C |
| F7 | **High** | `tyto/src/plane/mysql.rs:917, 991, 1095` | `table_columns_sql` / `table_ddl_sql` / `object_ddl_sql` interpolate identifiers into backticks without doubling. A table named `` weird`table `` breaks Columns + DDL. The DML methods in the same file already use a `qid` closure. | Route all three through one `quote_ident` helper. | **V L** |
| F8 | Med-High | `tyto/src/main.rs:761-803`, SessionOpened | TXN badge and `pending_txn` survive session replacement. `BEGIN; UPDATE…` → backend terminated → Reconnect → badge still says TXN; a later `COMMIT` only warns, the badge clears, and the DBA believes rolled-back work was committed. *(Badge fixed 2026-10-10: it follows the server, and a gone session resets it.)* `pending_post_action` also survives, so the next ordinary query after reconnect gets `\gset` / `\gexec` applied. | Reset `txn_state`, `pending_txn`, `pending_post_action`, `watch_sql` in the shared reset; emit a "new session — prior transaction and session state are gone" MetaOutput on reconnect. | C |
| F9 | Med | `tyto/src/main.rs:5180` + unguarded `pane.set(Pane::Shell)` at `:950, :974, :855, :4493, :4621, :5170, :5635` | Every SessionOpened / reconnect / connection save / top-left tab click rebuilds the DOM of **every** pane: scroll, textarea undo, selection lost; Activity panes re-fire 3 queries; active `\watch` fires an off-schedule tick (consumes a `count=N` slot). | Guard the sets (or `create_memo`), read `connections` untracked in the workspace closure, keyed `<For>` for leaf bodies. | **V** |
| F10 | Med | `tyto/src/dom.rs:9` | `ws_send` does a tracked `ws.get()`, subscribing every sending effect to the socket signal. On reconnect the effect re-runs while the new socket is CONNECTING, the send throws and is swallowed → `\watch` silently dead; admin "affected objects" stuck Loading. | `with_untracked`, check `readyState == OPEN`, queue until open. | C |

---

## 3. Phase 1 — Explorer polish (Storage tab, PRs #105–#117)

In priority order.

### 3.1 Autovacuum **Run** button (`explorer.rs:6403-6447`) — **High**
The button sends raw `ExecuteQuery` onto a shell pane's session, bypassing the shell's submit path (no Query entry, no `query_count`, no `querying`, no `pending_txn`, no `cancel_watch`). **V**
- **Shell inside `BEGIN`:** `VACUUM` errors "cannot run inside a transaction block" → **PG aborts the DBA's open transaction** (same blast class as PR #107); then `main.rs:1545` flips TXN FAILED and `:1554` marks the DBA's previous successful query red.
- **`ANALYZE` inside `BEGIN`:** silently joins the transaction and holds `ShareUpdateExclusiveLock` until the DBA commits — a lock held across think-time.
- **Batch in flight:** the injected result advances `pending_stmts` early (`main.rs:1484`), breaking ON_ERROR_STOP; an injected error aborts the batch.
- **`\watch` running:** the VACUUM result is consumed as the watch tick; the real tick lands as an orphan entry.
- **Target selection** (`:6423-6439`) is `HashMap.values().find()` on `conn_id` only — non-deterministic with several shells, ignores `current_db` (a PG explorer can be on a different database than the shell → wrong table or error), ignores `session_stale`, and sets `active_pane_id` without `leaf.active` (output lands in a hidden tab).
- **Verbose** checkbox produces nothing visible: noctua never forwards notices (see § 7.2).
- `compose_vacuum_sql` (`:6513`) is PG SQL living outside the plane.

**Open design question for this phase — where should Run execute?**
(a) the shell's session, through the real submit path, gated on `txn_state == Idle`, not querying, no watch, empty queue, not stale, same `current_db`; DBA gets an auditable history entry;
(b) the explorer pane's own session (always autocommit, can never abort a user transaction; result shown inline, stats auto-refresh) — but a long VACUUM blocks that explorer's introspection until it finishes;
(c) a dedicated short-lived session (isolated, costs a connection, needs a protocol addition).
The current design chose (a) for visibility; (a) done properly depends on R1, so an interim gate + hoisted `submit_to_pane` helper is the pragmatic step.

### 3.2 Remaining Storage-tab items
1. **Extensions plumbing** — PR #113's diagnosis is right (Leptos 0.6 event handlers run with no reactive owner, so `use_context` returns None there), but the fix can be simpler: `tab.session_extensions` is already a Copy `RwSignal` set by SessionOpened. Capture it at construction and `get_untracked()` at click time. Removes `SESSION_EXTENSIONS` (unbounded, never pruned on close / DB switch), `stash_session_extensions`, `provide_context`, and the "two mirrors" comment (`main.rs:912`). Leftovers: `explorer.rs:576-581` references a non-existent `current_extensions()`; `SessionExtensions` now only feeds the badge tooltip. An `ExplorerCtx { plane, ws, session_id, extensions, table_cache, selected }` (all Copy) would also collapse the 6–8 argument signatures at 11 `ensure_table_detail_loaded` call sites. **V** (thread_local present)
2. **Insert-progress math is wrong** — uses lifetime `n_tup_ins` (`main.rs:2417`, `explorer.rs:6178`); PG's trigger counts `n_ins_since_vacuum`. Any table with insert history shows thousands of percent, red. **V**
3. **Autovac knob semantics** (`postgres.rs:~4279`, `explorer.rs:6139-6298`): PG 18's `autovacuum_vacuum_max_threshold` missing; `-1` shown raw (cost_limit −1 means `vacuum_cost_limit`); disabled insert trigger (−1) still gets a progress cell; freeze reloptions larger than the GUC are ignored by PG but shown as effective; "Enabled" ignores the cluster `autovacuum` GUC; PG uses `reltuples` not `n_live_tup`, and the `live > 0` guard hides progress right after a stats reset; no units (e.g. `log_autovacuum_min_duration` ms). Partitioned parents are never autovacuumed and can't carry these reloptions, yet show cluster values. The query returns 16 rows even for a non-existent relation. **L** (GUCs on PG 18) / C
4. **One severity source** — Dead tuples % and Stale stats % band on hard-coded 0.2 / 0.1 while the Autovacuum section knows the table's effective scale factors; the two sections can disagree about the same table. Move `progress_severity` + one `trigger_progress_cell()` into `storage_factor.rs` (removes 5 near-identical blocks).
5. **VM section errors are hidden** — the pg_visibility SQL includes relkind `'p'` (errors "wrong relation kind" on partitioned parents); `EXECUTE` on `pg_visibility_map_summary` is superuser / `pg_stat_scan_tables` only; call is not schema-qualified. All errors are swallowed (`explorer.rs:5915`; autovac `_ => <div/>`), so sections just vanish. Surface errors; fall back to `pg_class` on error. **L**
6. **Refresh** — ↻ collapses the tab to "Loading storage detail…", VM + autovac vanish, scroll jumps; keep last values visible while reloading; auto-refresh after Run completes. DDL invalidation (`main.rs:1432-1455`) refreshes only ancestors / descendants / partitions — Overview, columns, indexes, DDL, TOAST, VM, autovac stay stale (the "refetch on next Schema-tab open" comment is false); it also fires 3 queries per cached table and the cache is never evicted.
7. **Relkind awareness** — roll up VM on partitioned parents; note that autovacuum skips parents (manual ANALYZE needed); disable ANALYZE for TOAST relations.
8. **Tests** — `explorer.rs` has zero tests. Pin `compose_vacuum_sql`, `parse_autovac_rows`, `parse_vm_rows`, `parse_toast_rows`, and the progress math.
9. **Small** — `leptos::event_target_checked` in the 4 checkbox handlers; stale "pg_stat / PG 17" and "Two sub-sections… VM lands in a follow-up" comments; ↻ tooltip omits VM + autovac; undefined CSS tokens `--metric-good`, `--metric-bad`, `--hover-bg` (the hover fallback is a white tint, invisible in light theme).

### 3.3 Other explorer findings
- **`parse_row_estimate` strips the minus sign** (`explorer.rs:6592`): PG `reltuples = -1` → 1 row → never-analyzed tables bypass the 1M "filter first" gate and the 50K unindexed-sort gate; "Show all" sets `page_size=1`. Parse as `i64`, treat negative as unknown. **V**
- **Data-tab DML ignores `rows_affected`** (`main.rs:2791`): an UPDATE / DELETE matching 0 rows (row changed elsewhere, or a PK text value that doesn't round-trip — worse once F1 is fixed) still flashes "Saved" and patches the page. Treat 0 as failure.
- **`\d <name>` inspector likely stuck on "Loading…"** — `add_pane_to_leaf` sets `explorer_selected` without `ensure_table_detail_loaded`, and `render_inline_detail` reads the cache untracked (`explorer.rs:2770`). Unverified at runtime; check first thing in Phase 1.
- Document-global `querySelector` in `preserve_explorer_list_scroll` (`:2331`) and the inspector close button (`:1459`) hits the wrong explorer when ≥2 are open.
- `find_table_detail` splits the tag on the first `.` (`:35`) — schemas containing a dot lose results.
- `TableDetail::new()` (~60 signals) created in click handlers with no owner, cache never evicted (see R4).
- Data-tab DELETE banner has no autofocus / Enter / Esc (`:7761-7839`); the global Esc never clears `data_delete_pending` though its doc says it does.

### 3.4 Explorer structure
Largest functions: `render_explorer_pane` 556–1483 (~930), `render_erd_tab` 4740–5525 (~785), `render_data_grid` 7621–8105 (~485), `render_data_tab` 7151–7620 (~470), `render_autovac_section` 6057–6488 (~430), `render_admin_role_detail` 1879–2277 (~400), `render_privileges_matrix` 6785–7138 (~355).

Proposed `tyto/src/explorer/`: `mod.rs` (+ `controls.rs`, `inspector.rs`) · `loaders.rs` (`ExplorerCtx`, `ensure_*`, one `fire_table_introspects()` — the 10-query fan-out is duplicated at 412-435 / 478-501 plus a partial copy in main.rs) · `parse.rs` (all `parse_*` + the status / columns / indexes parsers moved out of main.rs, with a table-driven tag → signal dispatch replacing the parallel success / error ladders) · `list.rs` · `meta_grid.rs` · `schema_tab.rs` · `storage_tab.rs` + `autovac_action.rs` · `erd/{layout,render}.rs` (layout pure + testable) · `data_tab/{mod,grid,vertical,insert,delete_banner}.rs` · `privileges.rs` · `admin.rs`.

Also: 27 parallel `Option` fields (value / severity / tooltip) on `TableOverview` → `Option<Metric { display, sev, tooltip }>`; `col_idx` / `get` closures redefined per parser; one `render_load_state(sig, msg, f)` for the Loading / Error / Loaded boilerplate.

---

## 4. Phase 2 — Shell cleanup

### 4.1 Diagnosis
The shell treats the input buffer as **one SQL blob**: a whole-buffer `meta_cmd` check first, the `\if` pre-pass second, split-on-delimiter last. psql and mysql read the buffer as a **line-oriented script lexer** would. Responses are matched to requests by latches (R1). Key dispatch is a fixed if-chain over ~20 independent signals — password → prompt → Ctrl+R → search → Ctrl+L → bracket pairing → readline → Tab → popup → Enter / ↑ / ↓ — with no state machine, so modes stack.

### 4.2 Mode table (condensed)

| Mode | Entry → exit | Known conflicts |
|---|---|---|
| Edit | default | Readline keys act on the whole buffer, override macOS line-scoped bindings, bypass undo |
| History walk | ↑ on one-line draft → ↓ past 0 / submit | Typing doesn't exit it; prompt number wrong under prefix filter (`shell.rs:2675-2684`) |
| Ctrl+R search | Ctrl+R → Esc (discards) / Enter (loads, doesn't run) | Handled before popup check (audit 4.4); active inside `\password` |
| Completion popup | Tab (>1 candidate) → Esc / accept / typing out | Readline, Ctrl+R, Ctrl+L run first and leave it open with stale `replace_start` |
| Safe-mode confirm | destructive stmt → mouse only | No keys; input stays live; a second destructive stmt silently replaces the first |
| `\password` | → Enter ×2 / Esc | ↑ recalls SQL into the masked field; Tab + Ctrl+R work; no `spellcheck="false"` |
| `\prompt` | → Enter / Esc | Duplicates `\set` logic, drifted (PROMPT1 via `\prompt` stored but not applied) |
| Querying | send → result / error / disconnect | Textarea hidden; no cancel; Explorer sends don't set it |
| Batch in flight | multi-stmt / `\gexec` | No progress; queue dropped silently on disconnect |
| Watching | `\watch` → Stop / submit / count / min_rows | No Esc; output vanishes on stop; ticks don't set `querying` |
| Hidden latches | `\gset`, `\gexec`, `\crosstabview`, `\gx`, `\d name`, BEGIN | Consumed by *whatever* arrives next (watch tick, Explorer SQL) |
| `\e` round-trip | `\e` → `EditExternallyResult` | No state: input stays editable and gets overwritten |
| DELIMITER / stale | — | No continuation indicator; stale Enter is a silent no-op |

Global handler takes **Cmd+Shift+Z** (macOS Redo) for "reopen closed pane" even while typing in the shell (`main.rs:4314`).

### 4.3 Top wonky behaviors (ranked)
1. **Pasted scripts are swallowed.** Meta parsing splits at the first whitespace so the rest of the buffer becomes the argument (`postgres.rs:681-686`, `mysql.rs:133-148`). `\set ON_ERROR_STOP on⏎BEGIN;⏎…` → "expected on|off", buffer cleared, nothing runs. `\x⏎SELECT …;` runs only `\x`. MySQL `DELIMITER //⏎CREATE PROCEDURE … END //⏎DELIMITER ;` makes the whole script the delimiter; every later Enter inserts a newline. `SELECT 1;⏎\echo done` sends `\echo` to the server. **V**
2. **Multi-statement output lands under the wrong prompt.** `run_multi` pushes every Query entry first (`shell.rs:635-643`), results append at the end. `SELECT 1/0; SELECT 2; SELECT 3;` turns prompt 3 red and 2–3 look executed (audit 1.15, now worse). Cache invalidation and `USE` tracking read the last Query in history (`main.rs:1359`), so `CREATE TABLE t(…); SELECT 1;` never refreshes the Explorer. `\x` ignored for batches.
3. **Terminator check isn't quote / comment aware** (`shell.rs:2166`: `stmts.len() > 1 || trimmed.ends_with(delim)`). `SELECT 'abc;` runs; Enter after `x := 1;` inside `$$` submits a half-written function; `SELECT 1; -- done` sends a comment-only statement (MySQL ERROR 1065); `SELECT 1; SELECT 2` runs the unterminated tail (psql keeps it); MySQL `'it\'s; x'` splits wrongly (`parser.rs:84-98`, no backslash-escape handling). **V**
4. **psql one-liners don't work:** `SELECT now() \gx`, `SELECT now() \watch 1`, `SELECT 1 \g` insert a newline. Unknown `\foo` inserts a newline (both CLIs print an error). `\dt` then `\watch 2` re-sends the literal `\dt` to the server.
5. **`\watch` output disappears** on Stop / count / min_rows; every tick scrolls to bottom; keeps retrying after an error (psql stops).
6. **Safe-mode bypasses:** `\g`, `\gx`, `\gset`, `\gexec`, `\crosstabview` call `submit_query` directly (`DELETE FROM users \g` runs ungated); `\gexec`-generated statements are never inspected (`SELECT format('DROP TABLE %I',…) FROM pg_tables \gexec` drops tables with safe-mode on); MySQL batch `\G` classified by its first statement only.
7. **History walk:** prompt number indexes the wrong list; Ctrl+L / "Clear shell buffer" / eviction remove this session's commands from ↑, `\g`, `\watch`, `\e N`; commands duplicate after a WS reconnect; `\dt` recalls as `\dt` in-session but as expanded SQL after reload; `\x`, `\set`, `\c` never recallable; editing a recalled entry then ↑ loses the edit.
8. **Non-ASCII input corrupts or panics** — F4.
9. **Display settings rewrite old output:** the whole history re-renders with current settings (`shell.rs:2357-2388`) — `\timing`, `\pset null/title/t/footer`, PROMPT1, `%x` / `%/` in old prompts change (after `BEGIN` every old prompt shows `*`), while output format is captured at arrival — mixed rules. Vertical results ignore null / tuples_only / title; >12 columns forces vertical even with `\x off`; `\pset expanded auto` toggles instead of meaning auto (`postgres.rs:426`); `\pset format aligned` turns `\x` off (`shell.rs:1783`).
10. **Interactive `\if` wipes the input:** `\if :flag` + Enter → "unterminated \if block" and the line is cleared (`shell.rs:2122-2130`). Playwright covers only the paste path.

### 4.4 Other shell findings
- **Every `\d*` flushes caches** — its Query text fails `sql_inspect` parsing, and parse failure is treated as DDL (`main.rs:1371`), re-introspecting every cached TableDetail and clearing completion caches.
- **`substitute_vars` isn't lexer-aware** (`parser.rs:223`): substitutes inside quotes, comments, `$$` bodies and after `::` (`\set date x` breaks `col::date`); batch statements 2..N substituted twice (`shell.rs:2189` + `main.rs:1487`); single statements classified for safe-mode before substitution, batches after; runs on MySQL too (dialect purity).
- `\set PROMPT1 '%/%R%# '` keeps the literal quotes; `%p` always empty (PID never populated); `%#` hard-coded non-superuser; `%l` means line-in-statement in psql.
- MySQL `use MyDB` sends `USE mydb` (lowercased, `mysql.rs:133`); `\u db` never updates `current_db`.
- PG null display defaults to `"NULL"` (psql shows empty). *(Result wording fixed 2026-10-10: PG prints psql's command tags and an empty result's header over "0 rows"; "Query OK" / "Empty set" are MySQL's only.)*
- *(Fixed 2026-10-10: the badge and `%x` follow the server's transaction status on both planes — `ROLLBACK TO SAVEPOINT` clears TXN FAILED; MySQL's implicit commits and `autocommit = 0` show.)*
- Enter during IME composition executes; Ctrl+R has no failing-search indicator; history prefix match is case-sensitive.
- **Performance:** the history closure also tracks `variables` (updated on every result) → the entire scrollback DOM rebuilds twice per query; `active_pane_id.set` on every keystroke; adding a tab re-renders every shell in the leaf.

### 4.5 Target structure

**Protocol (shared + noctua pass-through)** — `request_id: u64` on `ExecuteQuery` / `DescribeQuery`, echoed on every result / error. `CancelQuery { session_id }`: noctua runs the plane-supplied cancel SQL (`pg_cancel_backend` / `KILL QUERY`) on a side connection using the known backend id (sqlx 0.8 doesn't expose the PG CancelRequest secret key).

**`tyto/src/shell/`**
- `script.rs` — per-plane **lexer**: `lex(buf, LexState) -> { items, remainder, state }`, `ScriptItem = Sql{text, Terminator} | Meta(MetaAction) | Cond(If/Elif/Else/Endif) | Delimiter`, `Terminator = Semi | Delim | G | Go | GoX | Gset | Gexec | Gdesc | Crosstab | Watch`. Quote / comment / `$$` / backslash-escape / DELIMITER aware; meta-commands at line start or trailing SQL. Plane supplies a `ScriptDialect` (`conditionals()`, `substitutes_vars()`, `backslash_escapes()`, `parse_meta(line)`), keeping `\if` and `:var` PG-only. Enter runs complete items and keeps the incomplete remainder in the buffer (psql behavior); no complete items → newline + PROMPT2 continuation marker.
- `pipeline.rs` — one submit path, ordered stages: conditional stack (persists across Enters, so interactive `\if` works) → variable substitution once, lexer-aware → meta handler table (`fn apply_meta(&mut ShellCtx, MetaAction) -> Outcome`, one function per group) → safe-mode on the final SQL of every executable item, including the `\g` family, `\gexec` output (gated as a batch), and watch → `ExecQueue::enqueue(ExecSpec { id, sql, display, post, txn, block_id, origin: Shell | Watch | Explorer | Password | Describe })`. Explorer Run, `\password`, `\gdesc`, watch ticks all enqueue here.
- `responses.rs` — `on_server_message(tab, msg)` moves out of main.rs (main keeps routing). Completion looks up the spec by `request_id`, renders into **that spec's block**, applies post-action + txn transition, pops the next item; ON_ERROR_STOP marks remaining blocks `Skipped`.
- `state.rs` — `InputMode { Edit, HistoryWalk{pos,draft,prefix}, Search{term,pos,draft}, Completion(popup), Confirm(PendingConfirm), Password{…}, PromptVar{…}, AwaitingEditor{id} }` separate from `ExecState { Idle, Running{current,done,total,started}, Watching(WatchState) }`; typed `DisplaySettings` (`OutputFormat`, `Expanded{Off,On,Auto}` enums; plane-supplied defaults for null display and footer wording); a `Variables` type whose `set()` owns classification + side effects, shared by `\set`, `\prompt`, `\gset`.
- `keymap.rs` — pure `fn keymap(&InputMode, &ExecState, KeyChord) -> Option<Command>` + one `run(Command)`. Esc cancels the running query or stops the watch; Enter / Esc drive the confirm dialog.
- `edit.rs` — `EditBuffer` (UTF-16 ↔ byte conversion in one place); readline, bracket pairing, completion all return `{value, cursor}` through one `apply_edit()`.
- `scrollback.rs` — `Vec<Block { id, num, input, prompt_snapshot, outputs, status }>` rendered with keyed `<For>`; display settings snapshotted per output; watch iterations appended as collapsible children.
- `history.rs` — one in-memory list loaded once, storing input as typed (including meta-commands); clearing scrollback leaves it intact; each entry keeps its number.

**Tests first (native `cargo test`)** — lexer golden tests for every repro in § 4.3; keymap table (mode × key → command); `EditBuffer` with é and emoji; history walk + numbering; substitution (quotes, `::`, `$$`); safe-mode coverage at every entry point. Playwright characterization for batch placement + error marking, Ctrl+R with popup open, keyboard safe-mode confirm, watch retention, reconnect notice.

**Suggested order:** `request_id` + `ExecQueue` → lexer + pipeline → `InputMode` + keymap + `EditBuffer` → Block scrollback → unified history.

---

## 5. Cross-cutting architecture

- **`ShellTab` (112 fields) → identity core + sub-structs** — `SessionState` (session_id, version, extensions, db_connection_id, stale, connecting, current_db, txn_state, pending_txn; one `reset()`), `ShellState` (history, input, walk, `SearchState`, querying, prompt_state, password_prompt, variables, `CompletionCache`), `WatchState` (9 `watch_*`, `stop()`), `PsqlSettings` (delimiter, expanded, timing, safe_mode, null_display, output_format, `pset_*`, on_error_stop, prompt1–3, format_style), `ExplorerState` (+ nested `AdminState`, 15 fields), `ActivityState` (14 fields). As `enum PaneBody { Shell(..), Explorer(..), Activity(..) }` with one constructor — replaces 3 near-identical ~117-line constructors (`types.rs:2009-2360`) and gives disposal one place to live.
- **Ownership** — create each pane under a child `Owner` stored on the ShellTab; dispose it in the close effect (R4).
- **main.rs split** — `app/state.rs` (Copy `AppState` via context, replacing 16-parameter `render_pane_tree`), `ws/{conn,dispatch,session,query,dml,introspect}.rs` (typed `IntrospectTag` enum with `FromStr` / `Display` so Result and Error route through one table; one `advance_queue`), `workspace.rs` (`mint_pane` — the lookup → constructor → Connect → insert sequence exists 5×), `pane_tree.rs`, `sidebar/{tree,view,menu}.rs`, `conn_form.rs` (`ConnForm` holds the 18 `nc_*` signals), `keyboard.rs`, `overlays/{dialog,notifications,menus}.rs`, `types/{layout,explorer,meta,ui,activity,shell_tab}.rs`.
- **noctua split** — `ws.rs` (upgrade, origin + token, reader / writer, cleanup), `session.rs` (registry, **per-session worker queue** — also fixes unordered same-session tasks and is the home for cancel + row caps), `live.rs` (one `connect(def, db, app_name)`; connect-options code exists 4×), `handlers/{store,probe,session,editor}.rs`, `convert/{pg,mysql}.rs`, `store.rs`, `editor/{mod,macos}.rs`.
- **Wire fidelity** — `rows: Vec<Vec<Option<String>>>` (NULL today is the string `"NULL"`: SQL export, JSON export, `\gexec`, and Data-tab `WHERE col = NULL` all act on the confusion); optional structured error fields (`sqlstate`, `detail`, `hint`, `position`, `context`) with `#[serde(default)]` from `as_database_error()` — unlocks `:SQLSTATE`, `:LAST_ERROR_SQLSTATE`, `\errverbose`; multiple result sets split at each `Either::Left` instead of merged under the first header; zero-row results get headers via `describe()` (today `SELECT * FROM film WHERE false` returns `cols=[]` on both engines).
- **PG notices** — sqlx 0.8.6 emits every NoticeResponse as a `tracing` event, target `sqlx::postgres::notice` (`sqlx-postgres/src/connection/stream.rs:161-188`). A `tracing_subscriber::Layer` that reads a `session_id` field from the current span can buffer notices per session; noctua ships them alongside the result. No sqlx fork needed. Unlocks `VACUUM VERBOSE`, `RAISE NOTICE`, and psql-faithful `WARNING` / `INFO` display. MySQL equivalent: `SHOW WARNINGS` count on OK packets. **V** (sqlx source)

---

## 6. Planes

- **MySQL backtick escaping** — F7.
- **MySQL `locks_sql` Pid is `THREAD_ID`, not the connection id** (`mysql.rs:1568-1577`): live `THREAD_ID = 71` vs `CONNECTION_ID() = 33`. The Locks tab shows a number that never matches Sessions or a valid `KILL QUERY` target. Join `performance_schema.threads` and emit `PROCESSLIST_ID`. (Kill is wired from `SessionRow.pid`, so no wrong-session kill today.) **L**
- **`pg_partition_tree` / `pg_partition_root` referenced unconditionally** (`postgres.rs:3128-3482`, relationship SQL ~4400-4459) — PG 12+ only; same parse-time hazard class as PR #107, version-gated instead of extension-gated. On PG < 12 the Overview, TOAST, and ERD fail for every table. Runs on the explorer's own session, so no shell-transaction blast radius. Either declare PG 12 the floor at connect time or branch on server version in Rust.
- **PG Functions list excludes aggregates / window functions** (`postgres.rs:3008-3017`, `routine_type = 'FUNCTION'`; `routine_type` is blank for aggregates). Audit 3.9, still open. **L**
- **`table_status_sql` performance on big partition trees** — ~9 independent `pg_partition_tree` walks per open (two correlated against `pg_stats` per leaf). One CTE reused by all projections cuts this ~9×. Not measured (Pagila has no large trees).
- `kill_query_sql` interpolates pid without numeric validation (safe by provenance only).
- `esc()` closures double `'` only — correct under PG `standard_conforming_strings=on` and MySQL defaults; add a comment stating the assumption.
- **Structure** — PG `meta_cmd` is one ~1,380-line match (`postgres.rs:632-2012`); split into family helpers (`d_family`, `l_family`, `x_family`, misc). The shell lexer work in § 4.5 changes the meta-parse contract, so this split pairs naturally with Phase 2.
- **Transaction safety confirmed** — explorer introspection runs on the explorer pane's own session (`main.rs:1897-1901`), never a shell's. The only path that puts explorer SQL onto a shell session is the Run button (§ 3.1).

---

## 7. Backend (noctua + shared), remaining

1. **No query cancel** — Disconnect removes the session from the map but the query keeps running and holding locks. Store `SessionEntry { conn, def, backend_id }` (backend id already fetched at `:932`). Interim: tyto can send `pg_cancel_backend` / `KILL QUERY` over another live session on the same connection today.
2. **Error details dropped** — PG errors arrive as `error returned from database: column "nosuchcol" does not exist`; SQLSTATE 42703 and position 8 were available. **L**
3. **Result-set shape** — see § 5 (headers from first row; multi-result merged). **L**
4. **`.pgpass` vs libpq** — requires exactly 0600 (libpq only forbids group / other bits, so 0400 is wrongly rejected); `trim_end` strips trailing spaces from passwords (libpq strips only `\r\n`); escaped `\*` treated as wildcard; tests mutate `PGPASSFILE` from parallel threads (comment says single-threaded).
5. **`\e` edge cases** — wrapper path escaped for AppleScript but not quoted for the shell (a `$HOME` with a space breaks terminal editors); terminal editors not in the list (`hx`, `kak`) take the GUI path.
6. **Wire hygiene** — `tag` / `default_pane` stringly-typed; no `#[serde(other)]` fallback; `TestConnectionResult` uncorrelated; `""` overloaded as "use saved password"; invalid-JSON errors carry an empty `session_id`; the read loop exits on any non-text frame (future ping / pong would tear down every session).
7. Checked out fine: interval sign, enum / domain / regclass / xml / tsvector rendering; the global session-map lock is never held across a DB call; lock ordering can't deadlock.

---

## 8. App core, remaining

- **Signals never released** (R4) — `shell_history` of a closed 200k-row pane stays in the arena for the day. Not measured at runtime.
- **Panes created while the WS is down never connect** — reconnect only re-dials `session_stale` panes; new panes start `stale=false, connecting=true` with their Connect dropped → "Connecting…" forever. (`main.rs:3088`)
- **SessionOpened steals focus** — sets `active_pane_id` without `active_wt_id` / `leaf.active`; after a noctua restart the last pane to reconnect (HashMap order + jitter) becomes "active", possibly in a hidden workspace; Cmd+Shift+W then closes a pane the user can't see. Closing a pane before its SessionOpened arrives sends no Disconnect → upstream session leak.
- **`reset_in_flight_signals` gaps** (`:761-803`) — misses toast, vm, autovac, object_grants, erd_neighbour_columns, admin_*, object DDLs, completion caches, in-flight DML (phantom "1 pending edit" confirm forever).
- **IntrospectError drops Shell-pane tags** (`:2626-2754`) — `completion_tables` / `completion_columns:*` stuck Loading (Tab only refetches from NotLoaded); `\conninfo`, `\s`, `\ef` errors vanish after the input was already cleared; `data_count:` errors silent.
- **Notifications grow without limit** — every background `\watch` tick pushes one; `sql_snippet_for` clones the entire `shell_history` per push; `\watch 1` for an hour = 3,600 entries; entries for closed panes never removed.
- **Editing a linked connection deletes its other folder placements** — form loads `paths.first()`, save sends one path, noctua overwrites `paths` (`noctua/src/main.rs:603`), while the banner says "Edits apply everywhere."
- **Activity refresh has no in-flight guard** — at 1s, a slow `pg_locks` query queues requests on the pinned session without bound; sessions table rebuilt every tick (destroys text selection).
- WS reconnect backoff not jittered (fixed 1/2/4…30s); each "Reconnect now" click adds another retry chain.
- Edit-form "Delete Connection" skips confirmation (sidebar path confirms).
- Sidebar status footer picks the first pane in HashMap order; `is_active` ignores `target_wt_id`.
- Closing the active tab in a multi-tab leaf focuses the workspace's first leaf, not the sibling; `close_others` overwrites the single dialog slot.
- Global shortcuts are Cmd-only (`:4296`).
- **CSS** (4,574 lines, ordered by history): pane-tab close "×" invisible on hover (`--text` #e6edf3 on `--sidebar-hover` #dde1e7, ~1.1:1 — `style.css:193, 1351, 1371`); undefined tokens with no fallback (`--bg-hover` → Data-grid row hover + pager hover do nothing; `--bg`, `--bg-elev`, `--sidebar-active`) plus six fallback-only tokens; the explorer is themed twice (dark base 1666–2570 / 3515–4006 fully overridden by the cream `.explorer-shell` layer 4010–4290, with rival `!important` pairs — 30 `!important` total); ~54 dead class selectors (legacy tree-column explorer, `.shell-toolbar*`, `.tab-action-btn*`, `.conn-pin-*`, `.working-set-*`, `.slot-*`, `.btn-disconnect`); no semantic tokens (115 hex + 100 rgba literals; severity green has two values); `.pane-tab-bar { overflow: visible }`.

---

## 9. Docs, tests, tooling

- **CLAUDE.md is ~44 KB (~11k tokens per session), ~83% feature changelog.** "Shell Behavior" alone is 25.9 KB, including one 8.8 KB bullet of Overview meta-grid semantics. It contradicts itself (Storage factor both in Overview and moved; "Two sub-sections today" followed by a "fourth" and "fifth"). Target ~8–10 KB: what it is, commands (with `--workspace`), crate one-liners, invariants, ~12 shell UX bullets, workflow, doc map. Move psql meta-command detail → rewrite `vault/Features/Shell Meta-Commands.md` (today a 5-row stub); Overview / Storage / TOAST / index cells → new `vault/Features/Schema Explorer/Table Inspector.md` with one thresholds table; partition nesting + keyboard nav → `Outer List & Navigation.md`; ERD → "as built" appendix in `ERD_PANEL_SPEC.md`; Data tab → `Data Tab.md`; pgpass → `Connections & Credentials.md`; `\e` → `OS Provider Seam.md`.
- **Drift** — CLAUDE.md module list misses 9 of 21 files; ERD "lines 2629–4333" is really 4000–5717 (same stale range in `explorer.rs:9` and `code-reviewer.md`); `.claude/rules/noctua.md` lists 5 + 7 wire messages (missing 9 client + 17 server variants), calls the tag `kind`, names `value_to_string`, and states a lock rule that is impossible as written (the real invariant: never hold the sessions / connections *map* guard across a DB await); `.claude/rules/plane.md` lists 22 of 71 trait methods, lacks newer tags, still says `vm:*` is context-based, and its `paths:` should also cover the parsers in `main.rs` / `explorer.rs`; README claims 270 tests + zero clippy warnings; ARCHITECTURE says planes are traits `MysqlPlane` / `PostgresPlane`; `vault/Architecture/DBMS Planes.md` shows an invented trait and "Postgres Plane (planned)"; `Dialect Parity/Postgres.md` still lists shipped verbs as pending; the Agent Playbook references a missing plan file.
- **Agent briefs** — `sql-dialect-expert` is the stalest (says PG DDL is a placeholder; says "not expected to run the SQL yourself", contradicting the #107 lesson): replace its embedded contract with "read `.claude/rules/plane.md` first" and require running new queries on servers with and without the extension. `code-reviewer` bug class 9 ("was it added to the other plane?") inverts the dialect-purity rule; says there's no frontend test harness (28 Playwright specs exist). Both should learn: extension function refs inside CASE, `use_context` in `on:` handlers, map-lock-across-await, right-click firing actions, GUI features spanning a transaction. The Leptos lesson belongs in `rules/tyto.md`.
- **Hooks** — `pre-tool-use-bash.sh` blocks harmless commands that merely contain the text (`grep "rm -rf"`) and misses `rm -r -f`, `rm --recursive --force`, PG `TRUNCATE payment`, `push origin HEAD:main --force`, `push origin +main`. `session-start.sh` dies under `set -euo pipefail` while git is broken. `pre-compact.sh` writes `memory/pre_compact_snapshot.md` into the working tree on every compact and nothing reads it. `stop.sh` notifies after every turn.
- **Tests** — zero tests in `explorer.rs` (8.4K), `main.rs` (6.8K, including the ~930-line IntrospectResult parser), shell key handling, `completion.rs`, `format_sql.rs`, noctua value conversion (beyond interval), plane DML builders. Playwright: 28 specs / 119 tests, last touched Jun 21; won't run clean today (`storage-factor.spec.ts` asserts header cells that #105 moved; MySQL specs connect with an empty password — `global-setup.ts` should inject `PARLIAMENT_*_PASS`). No coverage of Storage tab, TOAST, Freeze, VM, Autovac, pgpass, ERD.
- **`integration-test-sql` doesn't test the plane's SQL** — queries are hand-copied and drifted (`columns_film.sql` uses `pg_attribute`; the plane emits `information_schema.columns`), covers 9 of 71 trait methods, none of status / vm / autovac / toast; goldens contain volatile `reltuples`. Rebuild around a `cargo test -p tyto -- --ignored emit_plane_sql` step that prints the real plane SQL (including both `table_vm_sql` paths) and pipe that to psql / mysql. `run.sh`'s `conn_field` still reads the legacy array form of connections.json.
- **Repo `memory/`** — 21 tracked files, newest content May 3; `feedback_code_quality.md` and `project_version_policy.md` duplicate user memory; `next_session_agenda.md` (Apr 17) and `project_claude_code_overhaul.md` are dead; `user_profile.md` / `user_typing_context.md` put personal information in git. Recommendation: retire it — personal / feedback files to user memory, product notes to `vault/Product` or `vault/Roadmap`, `feedback_agent_patterns` merged into the Agent Playbook.
- **Hygiene** — untracked `strix/dist/` (2.1 MB build output) and `tests/playwright/screenshots/_*/`; `.gitignore` `dist/` only matches at root; `.readme-preview.html` at root; unreferenced `docs/screenshots/activity-04-lock-scroll.png`; 4 merged local branches (incl. empty `feature/notebook`) and 101 remote branches.
- **Clippy real signals** — schema / table passed as anonymous `(String, String)` (and `explorer_partition_menu: Option<(f64, f64, (String, String), bool)>`) → a `QualifiedName` type; explorer render fns take 8–9 signals → context structs; `SidebarRow` variant size 313 vs 57 bytes; ~35 auto-fixable.

---

## 10. Status of the 2026-06 audit items

**Fixed (22):** 1.2, 1.3, 1.4, 1.5, 1.6, 1.13, 2.2, 2.3, 2.7, 2.10, 3.1, 3.2, 3.3 (parser), 3.4, 3.5, 3.7, 3.8, 3.10, 3.12, 3.13, 5.1 (undercut by F3's password path), 2.8 (timer cleared; `watch_sql` survives, so the UI still says "watching").

**Partial (10):** 1.1 (Enter-path batches gated; `\g` family, `\gexec` output, MySQL batch `\G` bypass), 1.7 (AST-based now, but batch invalidation reads the wrong query and every `\d*` flushes), 1.9 (dies-mid-query detected; idle kills invisible until next query), 2.1 (reset exists, gaps in § 8), 2.4 (cleanup exists, blocks on in-flight tasks — F6), 2.6 (per-pane reconnect jittered; WS backoff not), 2.9, 2.11 (queue cleared, no notice, post-action left armed), 2.12 (pending-DML confirm added; results still lost after close; confirm not keyboard-operable), 2.15, 3.6 (bare / pattern `\d` search_path-aware; exact-name `\d film` still hard-codes `public`).

**Open (29):** 1.8, 1.10, **1.11 (understated — F1)**, **1.12 (worse — `-1` now parses as 1 row)**, **1.15 (worse — error lands on the last Query of the batch)**, 2.13, 2.14, 2.16, 3.9, 3.11 (JSON / JSONB both render `?`), 4.1, 4.2, 4.3 (safe-mode confirm and Data-tab DELETE banner), 4.4, 4.5, 4.6, 4.7 (open by design — "no chevron"), 4.8, 4.9, 4.10, 4.11, 4.12, 4.13, 4.15, 5.3, 5.4, 5.5 (now part of F2).

**Not re-checked:** 1.14, 4.14.

**Pillar 6 (docs / tests):** resolved — tab completion listed as shipped, `\d*` count, README module-size claim, `crates/` claim, `global-setup` message, `main.rs` ↔ `plane.md` cross-reference, `//!` docs on the five largest files, `tab_completion.spec.ts` assertions. Fixed-then-drifted — ERD line range, test count, module count. Still open — CodeMirror framing in `SQL Completion.md` / `Syntax Highlighting.md` / `SQL Beautifier.md` / `Settings.md`, `GUI Panes Overview.md` "Planned Panes", `Shell Meta-Commands.md` stub, `Module Structure.md` (claims main.rs ~1,500 lines), `Testing Strategy.md` Layer 3 "future", no Admin-pane / Active-Connections vault doc, `vault/Home.md` missing links to Engineering / ERD spec / Overview 2026-06 / Tyto vs pgAdmin.

---

## 11. Review process notes

- The plane reviewer was instructed to run SELECT / catalog queries only. It created and dropped two scratch tables in `sakila` (`` weird`table `` for F7 and `_lock_test` for the Pid finding). Verified afterwards: neither table remains; no other `sakila` table was created in the prior 24 hours.
- The converter probe used for F1 lives in the session scratchpad, outside the repo (session-scoped statements only: `SET TIME ZONE`, a MySQL `TEMPORARY` table).
