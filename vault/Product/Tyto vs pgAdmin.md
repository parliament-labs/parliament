# Tyto vs. pgAdmin — feature comparison + gap roadmap

**Audience:** Parliament maintainers. Internal design memo, not marketing copy. Honest about gaps. Companion to `Vision & Positioning.md` — that doc says *why* Parliament exists; this doc says *how it compares to the tool it means to replace*.

**Scope:** pgAdmin 4 (current stable v9.16, open-source features only). Excludes the EDB commercial variants and any pgAdmin 3 legacy features.

**Method:** Two catalog passes — Tyto features from the codebase + CLAUDE.md, pgAdmin features web-researched against pgadmin.org docs. Cross-referenced; where I was uncertain I say so.

---

## § 1. The elevator answer

Parliament is a Rust/WASM PostgreSQL and MySQL client built for DBAs who work in SQL and want the client to expose signals, not obscure them. Every Overview cell answers a specific question — *is vacuum keeping up* (Dead tuples %), *is the planner picking indexes* (Index access %), *is the buffer cache doing meaningful work* (Cache hit + Block IO magnitude pair), *is TOAST hiding your storage growth* (TOAST cell + section) — with severity coloring tied to PostgreSQL's own autovacuum defaults. The SQL shell is psql-faithful — 50+ meta-commands including `\gexec`, `\gdesc`, `\crosstabview`, `\watch`, safe-mode with an AST-based DDL gate, external-editor round-trip via `\e`, and full `\pset` parsing.

Compared to pgAdmin: Parliament is **faster** (Rust/WASM vs. Electron+Python), **smaller** (single binary + browser), **safer** (no transactions across UI think-time, AST-based safe-mode, right-click-never-fires rule), and **more focused** (dialect-pure planes, DBA-actionable derived signals instead of raw catalog fields). Where Parliament trails pgAdmin honestly: **graphical EXPLAIN**, **pg_dump/pg_restore integration**, and **Schema Diff** — all three are on the roadmap. Where Parliament will never grow features pgAdmin has: a PL/pgSQL debugger, a DDL-generating wizard, and an embedded AI chat — those are considered and rejected, and this doc explains why.

---

## § 2. Where Parliament is already ahead

Grouped by category. All items verified against pgAdmin 4 v9.16 docs.

### Performance & installation
- **Rust/WASM binary vs. pgAdmin's Electron + Python-server stack.** pgAdmin's own GitHub issues document multi-second startup on Mac, 100%+ CPU in the render process (v9.4), 4-10s click lag on M-chip Macs, and degradation after several days of use (issue #5923). Browser hangs on large BLOB columns. Parliament's cold-start is a trunk-served WASM bundle from a single binary — near-instant.
- **Browser-based zero-install.** Parliament runs as `cargo run -p noctua` (single binary) with the browser as the frontend. pgAdmin 4's browser mode requires a server-side deployment: Python runtime, web server, reverse proxy in production. Parliament installs like a game; pgAdmin installs like a web app.
- **Multi-pane splittable workspace.** Parliament has recursive Split H/V panes with keyboard nav (Cmd+1..9, Cmd+[, Cmd+], Cmd+Shift+W, Cmd+Shift+Z). pgAdmin has fixed Object Explorer + Query Tool tabs.

### DBA-actionable signal density
- **Overview meta-grid.** Every table gets Storage factor (≤1.2× green / 1.2-1.5× yellow / >1.5× red), Cache hit + Block IO magnitude pair, Index access % (≥80% green / 20-80% yellow / <20% red — mirrors autovacuum defaults), CRUD condensed magnitudes (`105M / 343K / 12.5K / 87`), Dead tuples % (thresholds match `autovacuum_vacuum_scale_factor` = 0.2), Stale stats % (matches `autovacuum_analyze_scale_factor` = 0.1), Last vacuum + Last analyze pair, TOAST cell, Pages / Ideal. pgAdmin's table properties dialog shows raw catalog fields — reltuples, relpages, last_vacuum — without derived signals or severity coloring.
- **Storage rollup on partitioned parents.** Parliament sums Page_count, Storage_factor, Storage_actual_bytes, and TOAST across leaf partitions via `pg_partition_tree`. pgAdmin shows 0 on partitioned parents.
- **Maintenance-pair layout.** Last vacuum + Dead tuples % side-by-side; Last analyze + Stale stats side-by-side. Reads as a maintenance ledger. pgAdmin's per-property dialog doesn't attempt this pairing.
- **TOAST section.** Three layouts (Single / PartitionAggregate / IsToastOf) with bidirectional navigation — clickable TOAST relation names, "TOAST OF <parent>" back-link at the top of a `pg_toast.*` detail. pgAdmin surfaces TOAST relations as tables in the browser but doesn't explain their relationship or compress the DBA's mental model.

### SQL shell
- **psql-native.** 50+ `\d*` metadata commands, `\gexec` / `\gdesc` / `\crosstabview`, `\if`-`\elif`-`\else`-`\endif` conditional blocks, `\prompt` for interactive variables, `\watch` with named args + `count=`/`min_rows=` termination, `\pset` faithful parsing (html/asciidoc/latex/markdown), `\password` with browser-side SCRAM-SHA-256 hashing (plaintext never crosses the wire), `\safe-mode` with AST-based DDL gate + bulk-batch confirmation, `\lo_list`/`\lol`/`\lo_unlink`, `\errverbose`, `\encoding`. pgAdmin's Query Tool has none of these — it's a raw editor with autocomplete + macros. Even DBAs comfortable with pgAdmin end up shelling to psql for these commands.
- **Dialect purity per plane.** MySQL connection behaves like the `mysql` CLI (backticks, `\G`, `DELIMITER //`, `#` line comments). PostgreSQL behaves like psql. Meta-commands don't leak across CLI boundaries. pgAdmin is PG-only.
- **Format-with (River / Block / Inline) built-in via right-click.** pgAdmin has SQL *formatting preferences* (keyword case, tab size, operator spacing) but no on-demand "format this query" action.
- **External editor round-trip via `\e` / `\ef` / `\ev`, psql-faithful.** Your own editor (Settings › Editor, a per-connection override, or psql's `PSQL_EDITOR` / `EDITOR` / `VISUAL`), opened at a line, in Terminal.app / iTerm2 / any terminal; focus comes back to the browser and the text waits in the input. pgAdmin's Query Tool is the web textarea only.
- **Tab completion across 7 contexts.** Meta-commands, keywords, tables, columns, FROM-table aliases, CTEs, inline views, star-expansion. pgAdmin has autocomplete on Ctrl+Space but not context-aware CTE / inline-view / alias resolution.
- **Transaction badge (yellow TXN / red TXN FAILED on toolbar).** Persistent visual signal for open transactions. pgAdmin has none.
- **CSV / JSON / SQL download links on every result grid.** pgAdmin exports Binary / CSV / Text only — no native JSON export from query results (confirmed by pgAdmin docs).

### Safety & UX architecture
- **Safe-mode with AST-based DDL gate.** Flagged shapes: DROP, TRUNCATE, DELETE/UPDATE without WHERE, ALTER TABLE ... DROP COLUMN/CONSTRAINT/FK/INDEX/PARTITION, DROP ROLE. Multi-statement pastes show one up-front dialog listing every destructive statement in the batch — the DBA signs off atomically. Tautological WHERE clauses (`WHERE 1=1`) are trusted as the DBA's intentional ritual. pgAdmin has no equivalent guard.
- **Data-tab CRUD without transaction-across-think-time.** Architectural stance (documented in memory + CLAUDE.md): single-statement auto-commit only; multi-row atomicity is the shell's job. pgAdmin's edit-in-grid holds an open transaction across think-time — textbook long-running-transaction incident pattern in production.
- **Right-click-always-opens-a-context-menu rule.** Documented UX rule ensures right-click never fires a destructive action. pgAdmin's right-click menus vary per object; some fire actions directly.
- **~/.pgpass with clear error surfacing.** Parliament's `~/.pgpass` mode fails loudly on file-not-found, wrong-permissions, or no-matching-entry via each PG handler's error variant. pgAdmin supports `.pgpass` but funnels errors into a modal.

### Navigation & workflow
- **Four-layer keyboard cursor across Schema Explorer.** Outer list → tab strip → tab body → cell editor. Arrow keys walk every layer without the mouse. pgAdmin needs point-and-click.
- **Partition tree nesting in the outer table list.** PG partitions indent under their parent with a `(N partitions)` count hotspot; > 6 collapse by default. pgAdmin flattens.
- **Partition-aware ERD focal card.** Focal card lists partitions capped at 8 with "+N more"; clicking a partition navigates to its detail. pgAdmin's ERD is view-only design tool without this drill-through.

---

## § 3. Where Parliament is behind by design (considered and rejected)

These are things pgAdmin does that Parliament explicitly won't. Turns "missing" into "decided."

- **Multi-row / multi-column pending edits in the Data tab.** pgAdmin lets you edit 10 rows and Save. Parliament refuses. Reason: any such feature holds a transaction across UI think-time (click → think → click → think → commit). That's the textbook long-running-transaction incident pattern — locks held while a user reads / deliberates / walks away. Documented in `feedback_no_gui_transactions` memory. Multi-row atomicity belongs in the shell, where the DBA owns the `BEGIN; ...; COMMIT;` boundary and the SQL is right in front of them.
- **Embedded pg_dump / pg_restore inside Parliament.** Backups belong to CI / cron / dedicated ops tools. That said: Parliament's planned **user-machine broker** is the natural home for host-OS integration (pg_dump, file picker; the external editor already runs through noctua in solo-local mode) — see § 5.
- **PL/pgSQL debugger.** pgAdmin bundles the debugger extension (breakpoints, step-into, step-over, variable inspection). It requires server-side extension + superuser + `shared_preload_libraries` restart. Recommendation: **decline.** Rationale: shell + `RAISE NOTICE` + explicit test cases beats step-through debugging in the code-review era. Adding a debugger UI would push Parliament away from being a SQL client and toward being an IDE. Different product.
- **Server maintenance wizards** (vacuum wizard, reindex wizard, analyze wizard). Parliament is a SQL client; the DBA types `VACUUM (VERBOSE, ANALYZE) foo;` in the shell. Wizards obscure the SQL and remove the DBA's ability to script the same command.
- **DDL-generation form dialogs** (CREATE TABLE / CREATE FUNCTION / CREATE ROLE with all-fields-checkable dialogs). Parliament shows DDL and expects the DBA to write SQL. Rationale: DDL is the source of truth. The DBA who can't write CREATE TABLE by hand isn't Parliament's audience. See `feedback_separate_audiences` memory — distinct audiences get distinct apps, not mode toggles.
- **AI chat / AI reports / AI Insights in EXPLAIN.** pgAdmin v9.13+ ships all three. Recommendation: **decline for v1.** Rationale: many shops prohibit LLM access to prod credentials by policy; Parliament's derived-signal severity bands already answer the questions AI reports pretend to. Bring-your-own-endpoint mode could be a v2 escape hatch, but it's not table stakes.
- **pgAgent job scheduling.** pgAdmin bundles pgAgent (SQL / Shell / Batch jobs on schedules). Decline: different tool category. Jobs belong in cron, CI, or dedicated schedulers.

---

## § 4. Where Parliament is genuinely behind (the punch list)

Grouped by severity. Each item: what pgAdmin does, why Parliament doesn't yet, disposition.

### Showstoppers — "I can't switch without X"

- **Graphical EXPLAIN plan viewer.** pgAdmin renders EXPLAIN in text / graphical (node tree with cost bubbles) / table (depesz-style) / statistics formats. Parliament shows `EXPLAIN FORMAT=JSON` as raw result rows. Already on Parliament's roadmap as "Plan Viewer pane" in CLAUDE.md. **Severity: showstopper.** One of the top-3 reasons DBAs open pgAdmin.
- **Backup / Restore dialog.** pgAdmin wraps pg_dump / pg_dumpall / pg_restore with format selection (Custom / Tar / Plain-text / Directory), object selection (table / schema / db), job control, background execution + progress. Parliament expects the DBA to shell to `pg_dump`. **Severity: showstopper** for DBAs used to point-and-click backups; **medium** for shops with CI-based backups. Design tension: Parliament's user-machine broker (planned) is the natural home for host-OS `pg_dump` invocation. Once the broker lands, the Backup dialog is a small addition.

### Major gaps — noticed within a week

- **Object property dialogs per kind.** pgAdmin has structured dialogs for 27+ object types (General / Columns / Constraints / Advanced / Partition / Parameter / Security tabs). Parliament shows raw DDL only. **Severity: medium-high.** The biggest UI-density gap. Decision needed: build 20+ dialogs (heavy scope) OR ship a "structured DDL split view" — parsed structure on the left, DDL text on the right, click a field to jump to that DDL line. Recommend the split view; ~20% of the effort for ~80% of the value.
- **Schema Diff tool.** pgAdmin compares objects between two databases OR two schemas, cross-server, generates a sync SQL script. Parliament has nothing analogous. **Severity: medium-high.** Real workflow gap. **Not on the current roadmap; recommend adding.** Reasonable scope: one pane, two connection selectors, diff engine, output as a shell-populated SQL script the DBA reviews before running.
- **Object kind coverage.** pgAdmin lists Casts, Collations, Domains, Domain Constraints, FTS Configurations / Dictionaries / Parsers / Templates, Foreign Data Wrappers, Foreign Servers, User Mappings, Foreign Tables, Publications, Subscriptions, Rules, RLS Policies, Event Triggers, Synonyms, Languages, Aggregates, Types (composite / enum / range / base). Parliament covers Tables / Views / Procedures / Functions / Triggers / Events + Roles / Tablespaces / Databases / Schemas / Extensions in Admin. Gap: ~20 kinds not enumerable from the outer list. **Severity: medium.** Most DBAs don't touch FTS parsers weekly, but they DO touch Publications / Subscriptions in modern shops.
- **Publications / Subscriptions UI.** pgAdmin has CREATE / ALTER dialogs for both. Parliament: neither. **Severity: medium** in shops using logical replication; **low** elsewhere.
- **RLS policy viewer / editor.** pgAdmin has per-table RLS policy dialogs. Parliament's "Permissions Explorer" roadmap item may cover this. **Severity: medium** in shops with RLS-heavy schemas (SaaS multi-tenant).
- **Server dashboard with graphs.** pgAdmin's Dashboard tab shows real-time TPS, cache hit, block IO, active-session graphs. Parliament has Activity (session list + lock graph) but no dashboard-style live charts. **Severity: low-medium.** The same signals live per-table in Parliament's Overview cells; a global dashboard is dashboard-shaped rather than table-shaped. Complementary, not identical.
- **Extensions CREATE / DROP dialog.** Parliament shows extensions in Admin but no install UI. **Severity: low-medium.** Small module to add.
- **Materialized view REFRESH verb.** Right-click "Refresh (with data / with no data / concurrently)". Parliament: type it in the shell. **Severity: low**, but easy to add.

### Minor gaps

- **Macros with `$SELECTION$` substitution.** pgAdmin's Query Tool has named macros triggered by hotkey with a `$SELECTION$` placeholder that inserts the current text selection. Parliament has history (persisted, Ctrl+R searchable) but no keyboard-triggerable named snippets. **Severity: low-medium.** Actually a compelling UX pattern — worth considering as a slice of the Notebook feature.
- **Bulk data import (COPY FROM file).** pgAdmin has a File → Import wizard. Parliament: shell-only (`\copy` from psql works; no upload flow in tyto). **Severity: low** unless the DBA imports files daily.
- **LISTEN / NOTIFY panel.** pgAdmin surfaces server-side notifications in the Query Tool's Notifications tab. Parliament: none. **Severity: low.**
- **Preferences page.** pgAdmin has an extensive Preferences dialog (font family / size / ligatures / brace matching / cursor blink / theme / grid pagination / column sizing). Parliament: per-connection settings only. **Severity: low** — users can adjust via CSS if needed.
- **Standalone ERD design tool.** pgAdmin's ERD tool lets you design a new schema by dragging boxes and generates CREATE TABLE DDL. Parliament's ERD is view-only from existing schema. **Severity: low.** DDL design tools are a different product category.
- **Post-connection SQL automation.** pgAdmin runs a configured SQL statement on every connect. Parliament: session-vars only. **Severity: low.**

### Not gaps (things that looked like gaps but aren't)

- **JSON export from result grids.** pgAdmin exports Binary / CSV / Text only; Parliament exports CSV / JSON / SQL. Parliament is ahead.
- **Dark mode.** pgAdmin added it in v6, so no longer a gap. Both have it.
- **~/.pgpass support.** Both have it. Parliament's error surfacing is better; both work.

---

## § 5. Recommended roadmap (priority order)

**Priority 1 — showstopper closers, already on the roadmap:**

1. **Plan Viewer pane** — closes the biggest single gap. Graphical + text + JSON views of EXPLAIN. Scoped in CLAUDE.md; needs a design doc.
2. **User-machine broker** — the natural home for Backup / Restore and the file picker (and the external editor in shared deployments). Once the broker lands, Backup UI is a small addition. Scoped in memory.
3. **Permissions Explorer / RLS view** — closes the RLS gap + strengthens "why can/can't X access Y." Scoped in CLAUDE.md.

**Priority 2 — major-gap closers, worth building:**

4. **Schema Diff tool** — cross-database comparison + sync-script generation. **Not on the current roadmap; recommend adding.**
5. **Object property "structured DDL split view"** — parsed-structure-on-left, DDL-on-right approach instead of 20 hand-built dialogs. Faster to ship, less duplicated with the DDL source of truth.
6. **Notebook** — already on roadmap. Also closes the Macros gap: a Notebook cell IS a named saved query. Pitch: "better than macros because it's a first-class artifact, not a hotkey slot."

**Priority 3 — decide explicitly, then either build or refuse:**

7. **PL/pgSQL debugger** — **decline** (moved to § 3). Update CLAUDE.md to reflect the stance so future planning conversations don't re-open the question.
8. **AI integration** — **decline for v1** (moved to § 3). Document the security posture.
9. **pgAgent job scheduling** — **decline** (moved to § 3).

**Priority 4 — small quick wins:**

10. **Materialized-view REFRESH verb** in the Schema Explorer right-click menu.
11. **Extensions CREATE / DROP dialog** in Admin.
12. **Publications / Subscriptions UI.**
13. **LISTEN / NOTIFY panel** in the shell.

---

## § 6. The elevator paragraph (for advocacy)

*(A Parliament advocate should be able to say this from memory when asked "why should I switch from pgAdmin?")*

> Parliament is a Rust/WASM PostgreSQL and MySQL client built for DBAs who work in SQL and want the client to expose signals, not obscure them. Every Overview cell tells you a specific question's answer — is vacuum keeping up, is the planner picking indexes, is the buffer cache doing meaningful work, is TOAST hiding your storage growth — with severity coloring tied to PostgreSQL's own autovacuum defaults. The SQL shell is psql-faithful with 50+ meta-commands, safe-mode, `\gexec` / `\gdesc` / `\crosstabview`, and external-editor round-trip. Compared to pgAdmin: Parliament is faster (Rust/WASM vs. Electron), smaller (single binary), safer (no transactions across UI think-time, AST-based safe-mode), and more focused (dialect-pure planes, DBA-actionable derived signals). Compared to pgAdmin's coverage, Parliament doesn't yet have graphical EXPLAIN, pg_dump integration, or a Schema Diff tool — all three are on the roadmap. And Parliament will never grow a PL/pgSQL debugger, DDL wizards, or an embedded AI chat — those are considered and rejected, and the decisions are documented.
