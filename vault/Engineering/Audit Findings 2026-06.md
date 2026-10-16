# Parliament — Critical audit findings (v2)

**Audit date:** 2026-06-11
**Branch state:** `main` at `c417413` (post `feature/admin-tools` merge)
**Method:** 6 parallel agents, each with one specific lens — Trust, Recovery, RDBMS-parity, Power-user friction, Browser-isolation, Doc/test-vs-reality. All briefed on Parliament's intent: a browser-based RDBMS client + management tool for DBAs, that feels native to whichever RDBMS the user is connected to. Defensive-coding-for-typo-risk was explicitly excluded as not the threat model — plane SQL is deliberate, reviewed, integration-tested.

This document supersedes the v1 audit, which was framed for generic enterprise software and produced a lot of noise around `unwrap_or(0)` defensive coding that didn't matter for Parliament's actual engineering culture.

---

## Executive summary

77 findings total across 6 lenses. They cluster into **8 root causes** that, if fixed, would close most of the high-severity items:

| Cluster | Root cause | Severity | Findings closed |
|---|---|---|---|
| A | **Prefix-match blindness** — safe-mode, TXN detection, schema-cache invalidation all do `trim().to_ascii_uppercase().starts_with(pattern)` instead of using `parser.rs`'s already-existing comment/whitespace-aware tokenizer | High | 3 (Lens 1) |
| B | **Multi-statement bypass** — safe-mode only runs on single-statement input; `SELECT 1; DROP TABLE x;` is silently un-gated | High | 1 (Lens 1) |
| C | **Missing destructive patterns** — `DELETE FROM x` (no WHERE) and `UPDATE WHERE 1=1` not in `destructive_patterns()` on either plane, despite being the textbook DBA footgun | High | 1 (Lens 1) |
| D | **ListDatabases has no error path** — failure routes to `TestConnectionResult` (wrong message type), dropdown stuck `Loading` forever, response cross-routes nondeterministically because `shell_panes` HashMap iteration order isn't stable | High | 3 (Lens 1 + Lens 2 × 2) |
| E | **WS disconnect / SessionClosed cleanup gaps** — neither handler clears in-flight `Loading` / `querying` / `watch_pending`, watch timer leaks on SessionClosed, multi-tab leaves left with dead tab pills, pending DML on closed pane silently half-commits with no UI feedback | High | 7 (Lens 2) |
| F | **noctua leaks upstream DB sessions on WS close** — every Cmd+R leaks 1 connection per pane; workday hits upstream connection-cap limit | High | 1 (Lens 2) |
| G | **Credential leak via ConnectionDef** — `password: String` round-trips back to every browser tab on every `ListConnections`; Edit Connection materializes plaintext in DOM `<input value=...>` | High | 2 (Lens 5) |
| H | **`RETURNING` clause silently drops result rows** — backend routes by SQL prefix; `INSERT/UPDATE/DELETE … RETURNING` goes to `execute_raw` which only returns rows_affected; the plane help text advertises it | High | 1 (Lens 1) |

Plus standalone high-severity items not in clusters:
- **MySQL plane is structurally second-class** — `role_is_superuser` not overridden so MySQL `root` never gets the bypass banner; Admin → Databases not clickable; IntrospectResult parser reads PG-only column aliases (`Owner`, `Parent_*`) and silently suppresses for MySQL (Lens 3)
- **Workspace tabs are not keyboard-switchable** and the workspace tab strip is only rendered for the New-Connection pane — a DBA with 2 prod sessions has no way to jump between them keyboard-only (Lens 4)
- **Negative `PgInterval` strips the sign** via `.abs()` on hours/minutes/seconds (Lens 1)
- **Docs say "Tab completion is planned"** despite 1,095 LOC of it shipping (Lens 6)

Browser-isolation has good news too: loopback bind + Origin check + CORS allowlist are solid; localStorage carries no credentials; session_id is server-issued UUID and not exposed (Lens 5).

---

## Pillar 1 — Trust failures

A trust failure = the software makes a promise (in docs, UI, tooltip, feature name) that the implementation doesn't keep.

| # | Severity | File:line | What's broken |
|---|---|---|---|
| 1.1 | **High** | `tyto/src/shell.rs:1103-1146` | **Multi-statement safe-mode bypass.** `is_destructive` runs only when `stmts.len() == 1`. `SELECT 1; DROP TABLE users;` ships without confirmation. |
| 1.2 | **High** | `tyto/src/shell.rs:1075-1079` | **Safe-mode prefix-match fooled by whitespace/comments.** `/* hi */ DROP TABLE x`, `-- comment\nDROP TABLE x`, `DROP\tTABLE x`, `WITH t AS (...) DELETE FROM users` all bypass. Parser already has comment-aware tokenization; safe-mode doesn't use it. |
| 1.3 | **High** | `tyto/src/plane/postgres.rs:2854-2859` + `mysql.rs:1180-1186` | **`DELETE FROM users` is not a destructive pattern.** Neither plane lists `DELETE`/`UPDATE` without-WHERE. CLAUDE.md says safe-mode "gates destructive commands"; DBA's actual footgun pattern is `DELETE WHERE 1=1`, which is wide open. |
| 1.4 | **High** | `noctua/src/main.rs:602-650` | **`RETURNING` rows silently dropped.** Backend routes by SQL prefix — only SELECT/SHOW/WITH/etc. get `fetch_all_strings`. `INSERT … RETURNING id` hits `execute_raw` and returns only rows_affected. The plane's `\h` help text advertises RETURNING. |
| 1.5 | **High** | `noctua/src/main.rs:424-429` | **`ListDatabases` failure sends wrong message type.** Emits `TestConnectionResult { success:false }` on error; that variant only goes to the connection-form modal. Schema Explorer DB dropdown's `explorer_db_pending` is stuck `true` forever, no error surfaced. |
| 1.6 | Medium | `tyto/src/types.rs:859-884` | **TXN badge lies if BEGIN/COMMIT has leading comment.** Same trim+prefix issue as safe-mode. `/* tx */ BEGIN` registers as no-op; user's connection is in a transaction with no TXN badge. |
| 1.7 | Medium | `tyto/src/main.rs:905-924` | **Schema-cache invalidation has same prefix-match blindspot.** DDL with leading comment doesn't invalidate; DDL on another pane / outside Parliament never invalidates. CLAUDE.md's "auto-refresh on DDL" claim only holds for same-pane no-comment case. |
| 1.8 | Medium | `tyto/src/shell.rs:1216-1221` | **TXN badge is purely client-side bookkeeping.** Doesn't reflect noctua's real session state. Server-side rollback (`pg_terminate_backend`) doesn't clear the badge. A DBA reading the badge to decide whether to issue a long query is making decisions on a lie. |
| 1.9 | Medium | `tyto/src/main.rs:1970-1976` | **Connection-pill stale flag tracks only the WS link, not the upstream DB session.** MySQL `KILL` / PG FATAL idle-in-transaction-timeout leave the green pill green; the next query errors with "Connection lost". |
| 1.10 | Medium | `noctua/src/main.rs:395-398` | **`ListDatabases` silently swallows per-row decode errors.** `r.try_get::<String>(0).unwrap_or_default()` → silently fewer databases shown. |
| 1.11 | Medium | `noctua/src/main.rs:777-784` | **Value-converter `?` and `<TYPE>` fallbacks eat sqlx errors with no diagnostic.** DBA sees `?` in a result row with no actionable signal. |
| 1.12 | Medium | `tyto/src/main.rs:1442` | **PG `pg_class.reltuples = -1` (never analyzed) renders as raw `-1`.** Honest but uninterpreted. Memory entry explicitly flagged this as a DBA-meaningful signal worth surfacing. |
| 1.13 | Medium | `noctua/src/main.rs:865-882` | **Negative `PgInterval` drops the sign.** `.abs()` on h/m/s strips it. `-1h` renders as `0 mons 0 days 01:00:00.000000`. Output disagrees with psql. |
| 1.14 | Medium | `tyto/src/main.rs:1494` | **`Non_unique` parser brittle.** Anything not exactly `"0"` (e.g. `false`, `f`, `FALSE`) silently flips to non-unique. |
| 1.15 | Medium | `tyto/src/main.rs:1109-1116` | **Skipped-after-error multi-statement entries left with `errored: false`.** Prompt color lies about execution status for queued-but-aborted statements. |

**Top 5 trust issues to file:**
1. **1.1** — Multi-statement safe-mode bypass
2. **1.4** — `RETURNING` rows silently dropped
3. **1.5** — `ListDatabases` failure mis-routed
4. **1.2 + 1.6 + 1.7** — Prefix-match blindness (one root cause, three call sites)
5. **1.9** — "Connected" pill lies about upstream DB session

---

## Pillar 2 — Recovery failures

Recovery failure = DBA-realistic disruption (noctua crash, network drop, kill -9, Cmd+R, server-side session kill) leaves Parliament in a state the user can't recover from without page reload.

| # | Severity | File:line | What's broken |
|---|---|---|---|
| 2.1 | **High** | `tyto/src/main.rs:1970-1995` | **`on_disconnect` doesn't clear any `Loading` / `querying` / `watch_pending` / `*_pending` state.** Reconnect succeeds; pane is permanently "Executing…", textarea hidden. Must close pane to recover, losing shell history. |
| 2.2 | **High** | `tyto/src/main.rs:749-791` | **`SessionClosed` destroys the entire pane.** Server-initiated close (`pg_terminate_backend`, MySQL `KILL`, idle timeout) calls `shell_panes.remove(pane_id)` and rips the pane out of its workspace. Shell history, current input draft, schema state, watch state all vanish — no "session ended, click to reconnect" affordance. A DBA who `\watch`-es `pg_stat_activity` and kills a wrong PID watches their own pane disappear. |
| 2.3 | **High** | `tyto/src/main.rs:771` | **`SessionClosed` for multi-tab leaf leaves dead tab-pill.** `remove_empty_leaf` only handles single-tab case; in multi-tab leaves the pane is removed from `shell_panes` but NOT from `leaf.tabs`. Tab-pill keeps rendering for a vanished pane; click/keyboard activation silently no-ops. |
| 2.4 | **High** | `noctua/src/main.rs:255-299` | **noctua leaks upstream DB sessions on WS disconnect.** `handle_socket` falls out of read loop without touching `state.sessions`. Cmd+R leaks 1 LiveConnection per open pane. Workday hits upstream connection-cap limit. |
| 2.5 | **High** | `shared/src/lib.rs:239` (no `DatabaseListError`) + `tyto/src/main.rs:1122-1124` | **`ListDatabases` error never reaches originator.** Same root as 1.5; `explorer_databases` stuck `Loading`, no retry, no error message. |
| 2.6 | Medium | `tyto/src/main.rs:1990-1994` | **WS reconnect has no jitter.** Two tabs that disconnected together hit noctua simultaneously at 1s/2s/4s/8s. Memory entry `project_connection_storm_jitter` explicitly raised this. |
| 2.7 | Medium | `tyto/src/shell.rs:337,1043` | **`submit_query` sets `querying=true` even when WS is down.** Pane stuck "Executing…" without a disconnect — race between reconnect and user-typed query. |
| 2.8 | Medium | `tyto/src/main.rs:749-791` | **`SessionClosed` doesn't clear `watch_timer`.** Interval keeps firing into the void with the dead session_id; each tick gets `Session not found` silently dropped. |
| 2.9 | Medium | `tyto/src/shell.rs:359-362` | **`watch_pending` never resets across disconnects.** If a tick fires before WS dies and the response never arrives, watch silently dies forever even though timer keeps ticking. |
| 2.10 | Medium | `tyto/src/main.rs:1136-1148` | **`ListDatabases` routing nondeterministic across multiple pending panes.** Comment claims FIFO; `shell_panes.get_untracked().values().find(|t| t.explorer_db_pending)` iterates a HashMap whose order isn't stable. Two Schema Explorers can swap database lists. |
| 2.11 | Medium | `tyto/src/main.rs:1029-1039` | **Multi-statement queue never resumes after WS death mid-batch.** `pending_stmts` retains `[SELECT 2, SELECT 3]` forever; reconnect doesn't resume. |
| 2.12 | Medium | `tyto/src/main.rs:2078-2121` | **Pending DML on closed pane: silent half-commit.** `close_pane` sends Disconnect but doesn't cancel pending UPDATE/INSERT/DELETE; DML message may already be in-flight; noctua processes it on a separate spawned task; result never reaches frontend (pane no longer in `shell_panes`). User has no idea whether the UPDATE landed. |
| 2.13 | Medium | `tyto/src/history.rs:17,45,73` | **localStorage corruption silently wipes history.** `serde_json::from_str(&json).unwrap_or_default()` — one bad byte from a browser extension, 2000 commands gone with no warning. |
| 2.14 | Low | `tyto/src/history.rs:31` | **localStorage quota errors silently swallowed.** `let _ = storage.set_item(...)`. Once quota is hit, persists silently fail; reload finds nothing saved. |
| 2.15 | Low | `tyto/src/shell.rs:313-317` | **`cancel_watch` doesn't clear `watch_pending` or `watch_timer`.** Probably harmless because result-handler clears, but if result never arrives, stuck for next watch. |
| 2.16 | Low | `tyto/src/main.rs:2082` | **Closed-stack reopen race.** `closed_panes_stack` is populated before disconnect; spam-close 10 panes loses entries via `stack.remove(0)`. Reopen has no awareness of which workspace tab the pane came from. |

**Top 5 recovery issues:**
1. **2.1** — on_disconnect doesn't clear in-flight Loading state (the showstopper)
2. **2.2 + 2.3** — SessionClosed destroys pane + breaks multi-tab leaves (one fix can address both)
3. **2.4** — noctua leaks DB sessions on WS disconnect
4. **2.5** — Same as 1.5
5. **2.11 + 2.12** — Lost work without notification (multi-statement queue + pending DML)

---

## Pillar 3 — RDBMS-parity gaps

Parity gap = Parliament forces a common GUI shape over a real RDBMS difference, or MySQL lags PG without good reason, or the parser bakes in a PG assumption.

| # | Severity | File:line | What's broken |
|---|---|---|---|
| 3.1 | **High** | `tyto/src/plane/mod.rs:104` + `mysql.rs` (doesn't override) | **`role_is_superuser` defaults to `false`; MySQL never overrides.** MySQL `root` (or any account with `SUPER`/`SYSTEM_USER`/`*.*` grants) never gets the "Superuser bypasses privilege checks" banner. DBA sees empty Privileges tab and concludes "no privileges" — exactly the confusion the PG banner exists to prevent. |
| 3.2 | **High** | `tyto/src/plane/mysql.rs` (no `area_row_clickable` override) | **MySQL Admin → Databases not clickable.** PG makes Schemas+Tablespaces clickable; MySQL's "Database" IS its schema — the natural container — and there's no drill-in surface for it. Tables-in-DB, db-level grants, size, charset are all not navigable. |
| 3.3 | **High** | `tyto/src/main.rs:1461` | **IntrospectResult parser reads PG-only column aliases.** `pick("Owner")` / `Parent_schema` / `Parent_table` / `Partition_bound` are all PG-only. MySQL silently returns empty → suppressed by `push` filter. A PG user pivoting to MySQL reads it as "GUI lost the owner column" rather than "MySQL doesn't have table ownership." |
| 3.4 | **High** | `tyto/src/plane/mod.rs:495-502` | **`MAJOR_KEYWORDS_UNIVERSAL` lacks `SHOW`, `USE`, `EXPLAIN`, `COPY`, `VACUUM`, `ANALYZE`.** Neither plane overrides. MySQL's daily-driver `SHOW *` patterns and PG's `EXPLAIN ANALYZE` / `COPY` / `VACUUM` render as regular keywords, flat with the line. Highlighting layer pretends the language is shared SQL-92. |
| 3.5 | Medium | `tyto/src/explorer.rs:619-770` | **DB-vs-Schema dropdown labels MySQL as PG.** The "Schema" dropdown for MySQL is populated from `SHOW DATABASES`. MySQL DBA looking for "Database" finds a dropdown labeled "Schema" over a list of databases. |
| 3.6 | Medium | `tyto/src/plane/postgres.rs:664` | **PG `\d film` hard-codes `WHERE table_schema = 'public'`.** psql walks search_path; Parliament doesn't. DBA on a Sakila-on-PG schema (`sakila`, not `public`) gets "Did not find any relation" even though psql would resolve it. |
| 3.7 | Medium | `tyto/src/plane/mysql.rs:1159-1171` | **MySQL Activity `locks_sql` uses `performance_schema.data_locks` (8.0+ only).** No fallback to 5.7's `information_schema.INNODB_LOCKS`. MySQL 5.7 users see hard error on Locks tab. |
| 3.8 | Medium | `tyto/src/explorer.rs` (partition rendering) | **MySQL `Partition_count` shown but not navigable.** PG renders count and lets click; MySQL renders count with no flat informational sub-table even though `table_partitions_sql` returns the data. |
| 3.9 | Medium | `tyto/src/plane/postgres.rs:1930-1939` | **PG aggregates / window functions not in Schema Explorer object kinds.** `\da` lists them; the Functions tab queries `routine_type='FUNCTION'` which excludes `prokind IN ('a','w')`. A PG DBA can't browse aggregates from the GUI. |
| 3.10 | Medium | `tyto/src/plane/mysql.rs:467-535` | **MySQL Views privilege matrix label mismatch.** `object_grants_sql` for Views queries 9 grant types; `object_privilege_kinds(Views)` labels only 4 columns. INDEX/ALTER/DROP/TRIGGER grants on a view (legal in MySQL) come back from the query and silently land nowhere. |
| 3.11 | Low | `noctua/src/main.rs:847` | **JSON vs JSONB conflated** in value converter. PG users care; jsonb is canonicalized + sorted-keys. Currently identical. |
| 3.12 | Low | `noctua/src/main.rs:677-781` | **MySQL converter has no explicit ENUM/SET arm.** Falls through to String catch-all; works by accident (string-base extraction lands `"ENUM"`). Brittle. |
| 3.13 | Low | `tyto/src/plane/mod.rs:381-383` | **MySQL inherits default `view_columns_sql`.** No Source column, no `VIEW_DEFINITION` surfaced on detail. `\sv` exists in shell; GUI doesn't follow. |

**Top 5 parity issues:**
1. **3.1** — MySQL `root` superuser banner missing
2. **3.2** — MySQL Admin → Databases not clickable
3. **3.4** — Major-keyword highlighting flattened for both planes
4. **3.5** — DB/Schema dropdown labeling
5. **3.6** — PG `\d` ignores search_path

---

## Pillar 4 — Power-user friction

Friction = keyboard flow breaks, verbs hide without affordance, GUI silently introduces think-time in a transaction.

| # | Severity | File:line | What's broken |
|---|---|---|---|
| 4.1 | **High** | `tyto/src/main.rs:3030-3047,2540-2566` | **Workspace tabs not keyboard-switchable.** `Cmd+1..9`, `Cmd+[`, `Cmd+]` walk **panes inside the active leaf**, not workspace tabs. Two open MySQL sessions: only switchable via mousing the sidebar Active Connections rows. |
| 4.2 | **High** | (no code — verified absent) | **Sidebar has zero keyboard navigation.** No arrow-key walk, no Enter to open, no Esc to defocus, no F2 to rename. Sidebar is mouse/touch only. Connect-to-first-query keyboard-only is impossible. |
| 4.3 | **High** | `tyto/src/shell.rs:1300-1338` | **Safe-mode confirm has no keyboard binding.** Two buttons, no autofocus, no Enter, no Esc. DBA who opts into `\safe-mode on` then has to mouse for every confirm. Same shape for Data tab DELETE banner (`explorer.rs:5590-5610`). |
| 4.4 | **High** | `tyto/src/shell.rs:419-429` (before `:781` popup intercept) | **Ctrl+R while completion popup is open enters half-state.** Search bar opens, popup stays anchored, both UIs stack. |
| 4.5 | **High** | `tyto/src/main.rs:4237-4254` | **Confirm dialog can't be confirmed with Enter.** `DialogKind::Confirm` autofocuses nothing (autofocus only for Prompt at line 4266). No Enter-binds-to-confirm. Delete-folder, etc. forces mouse. |
| 4.6 | Medium | `tyto/style.css:1181-1207` | **Pane-tab-bar overflow unhandled.** `overflow: visible`, no horizontal scroll, no overflow menu. Many panes → tabs push offscreen → user permanently loses access except via Cmd+[/] cycle (which doesn't scroll into view). |
| 4.7 | Medium | `tyto/style.css:449-451` | **Folder rows have no chevron / collapse glyph.** Visible/hidden children are the only collapse indicator. Opposite of DBeaver / pgAdmin conventions. |
| 4.8 | Medium | `tyto/style.css:311-320` | **Connection rows have no affordance that right-click is meaningful.** No ⋮ button, no hover-revealed kebab, no tooltip. New users won't discover Cut/Copy/Link/Rename/Delete. |
| 4.9 | Medium | `tyto/src/shell.rs:1262-1275` | **Watch-mode UI sparse, mouse-only to stop.** No interval display, no tick counter, no Esc binding, no global keyboard escape. |
| 4.10 | Medium | `tyto/src/shell.rs:56-82` | **Format menu silently bails on non-SELECT.** No toast, no menu-item disable, no `MetaOutput("Formatter only handles SELECT")`. User picks "Format with River" against UPDATE; nothing happens. |
| 4.11 | Medium | `tyto/src/main.rs:1028-1039` | **Multi-statement paste has no in-flight progress or cancel.** No "3 of 50" counter, no Esc/Ctrl+C to abort. On error, no rollback semantics for prior committed statements. |
| 4.12 | Medium | `tyto/src/main.rs:3811-3828` | **Workspace tab strip is invisible most of the time.** `class="tab-bar"` only rendered when `cur_pane == Pane::NewConnection`. 4 open workspace tabs → no horizontal strip — switching is hidden inside the sidebar's Active Connections folder. |
| 4.13 | Low | `tyto/src/main.rs:3030-3047,4778-4803` | **No tooltips for keyboard shortcuts.** Cmd+1..9 / Cmd+[/] / Cmd+Shift+W / Cmd+Shift+Z all hidden. Tab context menu items show no keybind hint. |
| 4.14 | Low | `tyto/src/main.rs:4496-4541` | **Sidebar context menu doesn't list rename shortcut.** Folder Rename/Delete forces right-click round-trip. |
| 4.15 | Low | `tyto/src/main.rs:2552-2565` | **Active-tab after Cmd+[/] cycle doesn't scroll into view.** Combined with 4.6, pressing Cmd+] past visible window leaves the active marker invisible. |

**Top 5 friction issues:**
1. **4.1 + 4.12** — Workspace-tab keyboard switcher missing + tab strip invisible (one root cause: tab strip only rendered for new-connection pane)
2. **4.2** — Sidebar keyboard navigation entirely absent
3. **4.3 + 4.5** — Confirm dialogs without keybinding (safe-mode + Confirm dialog)
4. **4.4** — Ctrl+R + completion popup half-state
5. **4.6 + 4.15** — Pane-tab-bar overflow silently broken

---

## Pillar 5 — Browser-isolation

Verdict: **loopback/Origin/CORS triad solid; credential-shape over the wire is the soft underbelly.** The `ConnectionDef.password: String` field choice cascades through several findings.

| # | Severity | File:line | What's broken / verified |
|---|---|---|---|
| 5.1 | **High** | `shared/src/lib.rs:75` + `noctua/src/main.rs:320-326` + `tyto/src/main.rs:669-670` | **`ConnectionDef.password` sent down on `ConnectionList`/`ConnectionSaved`.** All saved DB passwords sit in WASM linear memory for tab's lifetime — readable by browser extensions and devtools heap snapshot. Renderer doesn't need passwords; noctua holds them. Fix: `#[serde(skip_serializing)]` + Edit form ships placeholder. |
| 5.2 | **High** | `tyto/src/main.rs:4328,3921` | **Edit Connection pre-populates plaintext password from that downstream copy.** Materializes plaintext in `<input value=...>` — visible in devtools and to DOM-scraping extensions. |
| 5.3 | Medium | `noctua/src/main.rs:92,112` (TODO) + connection forms `tyto/src/main.rs:3959,3997,4072` | **Passwords sent over plaintext `ws://` (loopback only today).** Fine on localhost; doesn't auto-upgrade for future shared-deployment posture. |
| 5.4 | Medium | `noctua/src/main.rs:378,488,553,622` | **TestConnection / ListDatabases errors echo `sqlx::Error::to_string()` verbatim.** Doesn't include passwords today but: operator pastes `mysql://user:pwd@host` into Host field → password comes back in error toast, lands in screenshot/clipboard. |
| 5.5 | Low-Medium | `noctua/src/main.rs:152-160` | **`~/.sqlclient/connections.json` plaintext + default umask.** Documented in CLAUDE.md but missing `set_permissions(0o600)` after write. Other user on same Mac can `cat` it. |
| 5.6 | ✅ Verified-good | `noctua/src/main.rs:182-210,236` | Loopback bind + Origin check + CORS allowlist. Defense in depth. `origin_is_localhost` correctly accepts absent Origin (browsers always emit one). |
| 5.7 | ✅ Verified-good | `tyto/src/history.rs` | localStorage carries no credentials. Only SQL text, UI state, conn_id+timestamp. |
| 5.8 | ✅ Verified-good | `noctua/src/main.rs:497` | `session_id` server-issued UUID v4. Not in URL, not in localStorage. |
| 5.9 | ✅ Verified-good | `ServerMessage::SessionOpened.db_connection_id` | Intentional exposure (DBAs correlate to `SHOW PROCESSLIST`). Benign. |
| 5.10 | Low | `noctua/src/main.rs:92,112` | Multi-user posture is single-user-only and explicitly TODO'd. |
| 5.11 | ✅ Verified-good | `.claude/skills/integration-test-sql/run.sh:38,48` + `git/hooks/` | Credential layout matches CLAUDE.md. No pre-commit scanner, env-var pattern is real. |

**Top 5 isolation issues:**
1. **5.1 + 5.2** — Password leak via ConnectionDef (one fix closes both)
2. **5.5** — connections.json file permissions
3. **5.4** — sqlx error echo can leak pasted credentials
4. **5.3** — future TLS posture
5. **5.10** — multi-user posture flagged as paper-tracked tech debt

---

## Pillar 6 — Doc / test ↔ shipped reality

### Feature ↔ doc coverage

| Feature | Status | Notes |
|---|---|---|
| SQL formatter (River/Block/Inline) | **Stale-aspirational** | `vault/Features/SQL Beautifier.md` describes wrong architecture (sqlformat crate, CodeMirror, WYSIWYG panel) |
| Tab completion engine (1,095 LOC) | **No** — shipped feature documented as "planned" in README.md:167 and CLAUDE.md:153 |
| Active Connections sidebar | **No** doc anywhere |
| ERD tab | Partial (good ERD_PANEL_SPEC.md; not in README) |
| Data tab CRUD | Partial (vault still calls it future) |
| Schema Explorer 4-layer kb nav | Partial (CLAUDE.md only) |
| `\d*` family | **Wrong number** — 37, not 31 |
| Admin pane (Roles/Tablespaces/Databases/Schemas/Extensions/ServerConfig) | **No** vault doc |

### Factual claim discrepancies

| Claim | Actual |
|---|---|
| "31 total `\d*` metadata commands" (CLAUDE.md:65, README.md:15,119) | **37** (64 total backslash commands) |
| "ERD ~700 lines in explorer.rs" (CLAUDE.md:130) | **~1,726 lines** (2593–4318) |
| "75 unit tests" (README.md:183) | **87** |
| "largest module ~1,500 lines, most under 700" (README.md:184) | Wrong — explorer.rs 6,136; main.rs 5,107; postgres.rs 3,164; shell.rs 1,885; types.rs 1,628; mysql.rs 1,394 |
| "Nine focused modules" (README.md:97) | **14** tyto src files |
| "Repo layout: crates/{shared,backend,ui}" (README.md:125-143) | Real: `tyto/`, `noctua/`, `shared/` — no `crates/` prefix; backend is `noctua` |
| "`cargo run -p backend`" (tests/playwright/global-setup.ts:48) | `noctua`, not `backend` |
| MySQL DDL reconstruction shown only in PG column | MySQL has `SHOW CREATE TABLE` via `table_ddl_sql` — same trait |

### Stale-aspirational pages (CodeMirror references)

- `vault/Features/SQL Completion.md` whole file
- `vault/Features/Syntax Highlighting.md:15,21,135-146`
- `vault/Features/SQL Beautifier.md:113-119,129-133`
- `vault/Features/Settings.md:7,40,51`
- `vault/Features/GUI Panes Overview.md` whole file (Schema Explorer "Planned", Data tab "planned")
- `vault/Features/Shell Meta-Commands.md:3-13` (only 5 shipped, line 75 "Postgres Plane (planned)" — PG plane has 64 meta-commands shipped)
- `vault/Architecture/Module Structure.md` — every LOC count stale by 5-10×
- `vault/Engineering/Testing Strategy.md:42,55,77` — Layer 2/3 marked future; both exist

### Test shape audit

| Feature | LOC | Test shape | Verdict |
|---|---|---|---|
| `format_sql.rs` | 1,114 | 0 unit, 0 integration, 0 playwright | **Wrong shape gap.** Pure `String → String` on AST — golden-file Rust unit tests are the right shape, integration-test-sql is wrong (no DB). |
| `completion.rs` | 1,095 | 0 unit; 21 playwright **snapshot-only** specs (0 `expect()` calls) | **Wrong shape.** Pure-function context analyzer — unit tests are the right shape. Behavioral regressions hide in snapshot-only specs. |
| Plane SQL DML emitters | n/a | 0 integration-test-sql coverage for `update_row_sql`, `insert_row_sql`, `delete_row_sql`, `upsert_row_sql`, `count_rows_sql`, `select_paged_sql` | Right shape (integration over real DB), but real coverage gap on DML — the SQL that mutates user data |
| Activity/Admin plane SQL | n/a | 0 integration-test-sql for `sessions_sql`, `blockers_sql`, `locks_sql`, `kill_query_sql`, `list_roles_sql`, `role_privileges_sql`, `area_list_sql`, etc. | Right shape; gap is real |
| MySQL `table_ddl_sql` | n/a | No integration query | Real lag vs PG (PG has 3 DDL queries) |
| Playwright | — | `tab_completion.spec.ts` 21 shots, 0 expects | Snapshot-only — visual regressions caught, behavioral ones not |

### New-contributor traps

1. README "Project Structure" lies about layout (claims `crates/{shared,backend,ui}` doesn't exist)
2. README + CLAUDE.md call shipped Tab Completion "planned"
3. `vault/Architecture/Module Structure.md` LOC counts off 5-10×
4. `vault/Features/SQL Completion.md` describes CodeMirror+JS-bridge architecture that doesn't exist
5. `tests/playwright/global-setup.ts:48` prints wrong crate name
6. Plane column-alias contract (`.claude/rules/plane.md`) is enforced silently; `tyto/src/main.rs:1154` has no `// see` cross-reference
7. No module-level `//!` doc on the 5 largest non-formatter files (main.rs 5,107; explorer.rs 6,136; shell.rs 1,885; types.rs 1,628; noctua/main.rs)
8. CLAUDE.md ERD claim is `~700 lines`; actual span 1,726 LOC (~2.5× off)
9. No vault doc for Admin pane (entire feature surface + ~250 lines plane trait)
10. Active Connections completely undocumented

**Top 5 doc/test issues:**
1. Demote "Tab completion" → shipped in README + CLAUDE.md; rewrite `vault/Features/SQL Completion.md` (CodeMirror framing is wrong architecture)
2. Fix count claims: `\d*` 37 not 31; ERD ~1,726 not ~700; 87 tests not 75; module-size claim is false
3. Rewrite `vault/Architecture/Module Structure.md` (or delete — worse than nothing)
4. Add golden-file unit tests for `format_sql.rs`
5. Add unit tests for `completion.rs::analyze_context`; keep one playwright as smoke, demote the rest

---

## Triage workflow

Suggested order:

1. **Read the executive summary's 8 clusters.** Each cluster is one fix that closes multiple findings — highest leverage.
2. **For each cluster: open a single GitHub issue** that names the root cause + lists the closed findings. Don't file 7 issues for the WS-disconnect cluster.
3. **Standalone Highs not in clusters** — file individually: MySQL second-class plane (3.1+3.2), Workspace tab keyboard (4.1+4.12), Negative PgInterval sign (1.13).
4. **Browser-isolation `5.1+5.2`** is one fix — file as one issue. The verified-good lines (5.6-5.9, 5.11) are worth a separate note in the security section of the README/docs.
5. **Doc fixes are cheap** — bundle the factual-claim discrepancies into one PR.

### Calibration notes (added 2026-06-15)

The audit was a read-only sweep — agents walked the source but did not
run Parliament against a live database. That methodology caught a lot
of real findings but has a known limitation: **when a deeper bug
dominates the runtime path, the audit can describe the latent bug
correctly without noticing the dominant one.**

Issue #2 (`Negative PgInterval strips its sign`) was the first finding
filed and surfaced this calibration point. The audit identified `.abs()`
calls in the `PgInterval` format function that would strip the sign
from negative intervals — correct as a code-reading finding. But the
format function was never reached at runtime: `sqlx-postgres` has no
`Decode<INTERVAL>` impl for the text protocol (which is what
`sqlx::raw_sql` uses for every user query), so `try_get::<Option<PgInterval>>`
errored before the format function ran. Every PG `INTERVAL` column in
Parliament rendered as `?`.

The audit-described symptom (sign stripped, value otherwise visible)
was the latent bug. The dominant symptom users actually saw (decode
fails, cell renders as `?`) was a layer above it. The shipped fix
addresses both — `try_get_raw` bypass to make intervals render at all,
plus the sign-preserving `format_pg_interval` helper retained as a
forward-compatible binary-protocol fallback. See PR #12 / commit fec9430.

**Recommendation for the remaining issues (#3 through #11):** before
writing or expanding each issue, spot-check the described symptom
against a running Parliament. If what you observe doesn't match what
the audit described, the underlying mechanism is probably different —
file the issue against what users actually see, with the audit's
finding as additional context. Some findings will hold up; some will
have this same "described bug is real but a dominant symptom is worse"
shape. The pattern is most likely in Lens 1 (Trust failures) and Lens 2
(Recovery failures), where runtime state dominates.

A future audit should add a "observe before claim" step: each agent
runs the described scenario in a live Parliament and captures the
actual behavior, not just the inferred-from-code behavior. The
infrastructure exists — `PLAYWRIGHT_SKIP_FIXTURE_CONNECTIONS=1 npx
playwright test` against a local Parliament is a working harness.

### Calibration note 2 (added 2026-06-15, after issue #7)

A second flavor of the same blind spot: **per-cluster, the audit
correctly traced behavior inside ONE half of the system but assumed
the other half was working as advertised.**

Cluster E (disconnect / SessionClosed cleanup) framed everything in
terms of tyto's handlers: "`on_disconnect` doesn't clear in-flight
state," "`SessionClosed` destroys the pane," etc. All accurate
descriptions of what those handlers do IF the corresponding messages
arrive.

The audit didn't trace whether those messages actually arrive in all
the scenarios it claimed. When the user ran
`SELECT pg_terminate_backend(pg_backend_pid())` to test the
SessionClosed-banner flow post-fix, the banner didn't appear — because
noctua emits SessionClosed only for explicit `Disconnect` messages,
not when an upstream connection dies mid-query. The audit's framing
of finding 2.2 silently assumed noctua's emission was working; it
wasn't.

The full Cluster E fix needed BOTH:
- tyto's banner + cleanup + non-destructive SessionClosed handler
  (first commit on PR #15)
- noctua's `is_alive()` probe + `SessionClosed` emission after
  per-session errors (second commit on PR #15)

**Recommendation for the remaining issues:** when an issue describes
behavior on ONE side of the WebSocket, code-walk the OTHER side to
confirm the messages the issue assumes are arriving actually do. Lens
1 (Trust) and Lens 2 (Recovery) findings are especially likely to be
"interesting" — both ends sometimes need work and the issue body may
have only described one.

### What we didn't audit but probably should next

- **Performance under real load** — `\watch` interval with N=1, multi-statement paste of 200 statements, opening 20 panes simultaneously. The agents flagged O(N) localStorage writes and reactivity surface but didn't measure.
- **Accessibility** — keyboard nav was checked, but screen-reader / ARIA / focus management weren't. DBAs include people who use assistive tech.
- **noctua-side behavior under adversarial input** — a malicious connections.json (huge file, malformed JSON, symlinks). Not flagged because the threat model is single-user on localhost, but worth a pass before any shared-deployment work.
