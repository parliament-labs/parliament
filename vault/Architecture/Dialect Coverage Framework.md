# Dialect Coverage Framework

The framework the dialect-parity interview operates on. Defines what "parity" means, the two axes of coverage (meta-commands + SQL dialect), the tier scheme, the runtime grace contract, and the shape each per-DBMS inventory doc follows.

**Why this exists.** We're targeting a client experience that feels native to DBAs working in each supported DBMS — "the `mysql` CLI plus everything else the browser gives us" — and "native" is a moving target across dozens of categories and hundreds of individual features. Without a structured inventory we'll implement what's convenient, miss what matters, and discover a year in that seasoned DBAs feel at home in our MySQL support but adrift in our Postgres support. The framework is how we avoid that.

## Two axes of coverage

Most parity discussions conflate "native CLI features" with "SQL syntax" into a single bucket. They're different things and they want different kinds of work:

### Axis 1 — Meta-commands

The non-SQL verbs the native CLI understands. Always prefixed with something that marks them as client-side: `\` (psql backslash commands), `\.` (MariaDB delimiter), `help`/`status`/`use` (mysql bare words), `$` (some CLIs), `:` (others). Each is a client-side directive the native CLI interprets before (or instead of) sending anything to the server.

Typical categories:

- **Metadata / introspection** — `SHOW TABLES`, `\dt`, `DESCRIBE`, `\d`, `\l`, `\dn`, `SHOW CREATE TABLE`, `\ef function`.
- **Session control** — `USE db`, `\c dbname`, `SET`, `\set`, `\unset`, `\conninfo`, prompt customization.
- **Output formatting** — `\G` (vertical), `\x`, `\pset format/border/null`, `\a`, `\H`, `\t`, `\timing`, `\echo`.
- **History / editing** — `\e` (external editor) / `edit`, `\r`, history navigation, persistent history across sessions, Ctrl+R history search.
- **Scripting** — `SOURCE file`, `\i file`, `\ir file`, delimiter changes (`DELIMITER //`), transaction wrapping.
- **File I/O** — `LOAD DATA [LOCAL] INFILE`, `SELECT INTO OUTFILE`, `\copy FROM/TO`, `COPY FROM STDIN`, `\o file`, `\T file`.
- **Monitoring** — `SHOW PROCESSLIST`, `KILL`, `SHOW ENGINE INNODB STATUS`, `\watch`.
- **Admin** — `FLUSH`, `PURGE BINARY LOGS`, `CHECK/OPTIMIZE/REPAIR`, `VACUUM`, `ANALYZE`, `REINDEX`, `pg_dump`/`mysqldump` equivalents.
- **Auth / connection** — `GRANT`, `REVOKE`, `CREATE USER`, `\password`, TLS/cert options, `\conninfo`.
- **OS escape** — `\!`, `SYSTEM`, `\shell`, `$EDITOR`. Mostly blocked — see the Browser Limitations section.
- **Help & discovery** — `help`, `\h SQL_STATEMENT`, `\?`, tab completion.

Meta-command coverage lives in the DBMS plane (`tyto/src/plane/`) — each plane's `meta_cmd()` method maps an input string to a `MetaAction`. Adding a meta-command is usually adding a match arm.

### Axis 2 — SQL dialect features

The actual SQL syntax the DBMS understands. Divided into the standard taxonomy:

- **DQL** — Data Query Language — `SELECT`, `VALUES`, `TABLE`, set operators, CTEs, window functions, subqueries.
- **DML** — Data Manipulation Language — `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `REPLACE`, `RETURNING` clauses, `CALL`.
- **DDL** — Data Definition Language — `CREATE`, `ALTER`, `DROP`, `TRUNCATE`, `RENAME`, `COMMENT ON`, plus vendor-specific clauses (`ENGINE=`, `INHERITS`, `TABLESPACE`, partitioning, generated columns).
- **DCL** — Data Control Language — `GRANT`, `REVOKE`, `DENY`.
- **TCL** — Transaction Control Language — `BEGIN`, `START TRANSACTION`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `SET TRANSACTION`, `LOCK TABLES`.
- **Session / admin SQL** — `SET`, `USE`, `SHOW` (MySQL flavor), `RESET`, `FLUSH`, `VACUUM`, `ANALYZE`.
- **Procedural** — inside stored procedures: `IF`/`CASE`/`WHILE`/`LOOP`, cursors, exceptions, `RAISE`. Only matters for stored-proc editing; for interactive SQL these are wrapped.

We don't *implement* individual SQL keywords — the DB parses them. SQL dialect coverage affects us in four places:

1. **Syntax highlighting** — each plane ships keyword + function lists for `tyto/src/highlight.rs`. A keyword-list gap just means a keyword renders as plain identifier. Cosmetic but visible.
2. **Tab completion** (future work) — completing against keyword lists + schema metadata.
3. **Multi-statement parsing** — the shell needs to know where one statement ends and the next begins. MySQL's `DELIMITER //` changes this; Postgres has `$$ ... $$` quoting for function bodies. Gaps here cause silent wrong-behavior, not cosmetic issues — worth treating as bugs.
4. **Dialect-specific UX hooks** — detecting `BEGIN`/`COMMIT` to drive a transaction-state indicator; detecting `CREATE TABLE` to invalidate the Schema Explorer cache; detecting long-running queries to offer cancellation. Each requires knowing the shape the dialect uses.

## Tier scheme

Every parity item gets exactly one tier. The tiers drive ordering inside a branch, effort estimation, and what the runtime says when a user hits an unimplemented item.

| Tier | Meaning | Effort | Runtime response |
|:---:|---|---|---|
| **T1** | Easy — drop into an existing plane method. No new infrastructure. | <1 day | Implemented, normal behavior. |
| **T2** | Needs plumbing — a file picker, a new message type, a new UI widget, but no architectural choices. | 1–3 days | Either implemented, or `NotYetImplemented` pointing at the tracking reference. |
| **T3** | Individual engineering item — each one needs its own design pass, its own pressure-test, its own plan file. Examples: query cancellation, tab completion, `LOAD DATA LOCAL INFILE` streaming, `COPY FROM STDIN` protocol, authentication flows. | Days to weeks | `NotYetImplemented` with a tracking reference until the individual feature ships. |
| **BL** | Blocked by the browser boundary. Requires a seam implementation (usually [[OS Provider Seam]]) and/or a specific [[Deployment Topologies|deployment mode]] that has that seam active. | Waiting on prereq | `NotSupported { reason: BrowserLimitation }`. Lifts automatically when the prereq ships. |
| **BD** | Blocked by design. Not planned for implementation — wrong tool, wrong shape, or security-unacceptable. | Never | `NotSupported { reason: ByDesign }`. Permanent. |

BL and BD look similar at runtime but differ in communication:

- BL says "you can't do this from a browser today — see [[OS Provider Seam]] for what would unblock it." It's a temporary no that the user can file a feature request on.
- BD says "we're not planning to support this, and here's why." Permanent. Typical examples: `\!` / `SYSTEM` / `\shell` (never), arbitrary binary execution (wrong product), raw TCP escape (wrong product).

Tiers are reviewed **at the start of each parity interview round** and revisited when a seam or infrastructure lands — a bunch of T3 items may move to T2 when `BrowserOsProvider` is built, a bunch of BL items may move to T2 when `BackendOsProvider` is built.

## Runtime grace contract

When a user types a meta-command that the plane recognizes but hasn't yet implemented (or never will), the shell should respond with a clear, differentiated message — not a generic "unknown command" error and not silence.

### `MetaAction` extension

`tyto/src/main.rs` already defines:

```rust
pub(crate) enum MetaAction {
    Disconnect,
    RunSql(String, bool),
    Output(Vec<String>),
    EditHistory(u32),
}
```

The framework adds two variants:

```rust
pub(crate) enum MetaAction {
    Disconnect,
    RunSql(String, bool),
    Output(Vec<String>),
    EditHistory(u32),

    /// Recognized command that isn't implemented yet. The UI renders this
    /// in the "yellow" style — informational, not an error. `tracking`
    /// optionally points at a vault doc section or GitHub-issue style ref.
    NotYetImplemented {
        command: &'static str,
        tracking: Option<&'static str>,
    },

    /// Recognized command that will never be supported in this form, or
    /// cannot be supported without a deployment-mode upgrade. The UI renders
    /// this distinctly — grey dim for BrowserLimitation, red for ByDesign.
    NotSupported {
        command: &'static str,
        reason: NotSupportedReason,
    },
}

pub(crate) enum NotSupportedReason {
    /// Can't be done in a browser sandbox without a seam implementation.
    /// Lifts when the referenced seam ships in a compatible deployment mode.
    BrowserLimitation(&'static str),

    /// Intentionally unsupported. Not on the roadmap.
    ByDesign(&'static str),

    /// Reserved for future PAM use — the command is recognized and could be
    /// implemented, but the current PAM policy says no. Not used in
    /// solo-local mode.
    SecurityPolicy(&'static str),
}
```

The shell renders each `MetaAction::Output`/`NotYetImplemented`/`NotSupported` as a distinct entry type in the scroll history, with three differentiated visual styles:

| Variant | Color | Prefix | Example |
|---|---|---|---|
| `NotYetImplemented` | Yellow | `~ coming soon:` | `~ coming soon: '\copy' — file I/O pending, see vault/Architecture/OS Provider Seam.md` |
| `NotSupported { BrowserLimitation }` | Dim grey | `✗ not in browser:` | `✗ not in browser: '\!' — shell escape requires a backend OS provider, see vault/Architecture/OS Provider Seam.md` |
| `NotSupported { ByDesign }` | Red | `✗ not planned:` | `✗ not planned: '\!' — shell escape is never supported; the browser isn't a terminal` |

One place to change the rendering, consistent across every plane, every DBMS, every command.

### Why this matters

- **Users know what the tool can do.** A clear "coming soon" is infinitely better than a cryptic error.
- **Users don't file bugs for things we already know about.** The tracking reference points at the vault doc so anyone curious can read the context.
- **The docs and the code are aligned.** Every `NotYetImplemented` entry should correspond to an inventory row with a matching tier; the grep-ability makes drift obvious.
- **Onboarding is honest.** A new DBA evaluating the tool runs `\!` and gets "not in browser — here's why" instead of a confusing error. They now know our posture.

## Per-DBMS inventory doc shape

Each DBMS gets a doc at `vault/Features/Dialect Parity/<DBMS>.md`. The shape is canonical so new DBMSes plug in without re-negotiating the schema.

**Variants of a family get their own doc, not a unified one.** MariaDB, Percona Server, Aurora MySQL, and MySQL itself are all "MySQL-family" but are listed as *separate* inventories. Same for Postgres + Aurora Postgres + any Postgres-compatible downstream. The reasoning: (1) a command "available in the family" may not be available on the specific server a user is connected to because metadata schemas, sys views, and performance-schema surfaces drift between versions and vendors; (2) silently cross-populating rows creates the worst kind of parity-inventory footgun, where the doc says the command is supported but the server errors out; (3) each variant's plane is its own implementation that can diverge from its siblings, so the inventory should mirror the plane. The tradeoff is duplication across sibling docs — acceptable, because the duplication is explicit rather than silently assumed.

**We also don't cross-borrow commands across family lines.** Postgres users typing `SHOW TABLES` get whatever Postgres does with it; we do not translate to `\dt`. MySQL users typing `\dt` get whatever MySQL does; we do not translate to `SHOW TABLES`. The plane dispatches to its own dialect, full stop. A DBA who wants the other dialect's verbs opens a shell against the other dialect's connection. Cross-translation is the category of "helpful" behavior that silently rots the moment a user hits a command that *does* exist in their real dialect and expects it to work.

### Section 1: Summary

A short paragraph or two identifying the DBMS, the native CLI(s) this doc is benchmarking against (`mysql` / `mariadb` / `psql` / etc.), and the overall coverage level ("v0.0.1 implements most of the metadata + session-control categories; file I/O and OS escape are deferred behind the OS Provider Seam").

### Section 2: Axis 1 — Meta-commands

Table format:

| Command | Aliases | Category | Tier | Status | Notes |
|---|---|---|:---:|---|---|
| `\dt [pattern]` | `\d+` | Metadata | T1 | ✓ | Maps to `information_schema.tables` via the explorer's list_sql. |
| `\c dbname` | — | Session | T2 | ✗ pending | Needs reconnect flow; today returns an informational "not in PG on existing connection" message. |
| `\i path.sql` | `SOURCE` | Scripting | T2 | ✗ pending | Needs `BrowserOsProvider`. See [[OS Provider Seam]]. |
| `\!` | `SYSTEM` | OS Escape | BD | ✗ by-design | Browser isn't a terminal. See Browser Limitations below. |
| `\copy FROM file` | — | File I/O | T3 | ✗ pending | Needs streaming protocol. See [[OS Provider Seam]] Phase 2. |

**Column definitions:**
- **Command** — the primary form, with placeholder syntax (`[pattern]`, `dbname`, etc.).
- **Aliases** — other forms that invoke the same behavior. Empty or `—` if none.
- **Category** — matches the Axis 1 taxonomy above.
- **Tier** — `T1`/`T2`/`T3`/`BL`/`BD` from the tier scheme.
- **Status** — `✓` implemented, `✗` not implemented, `⚠` partial (some sub-behaviors work, others don't). Plus a human-readable qualifier (`pending`, `by-design`, etc.).
- **Notes** — implementation pointer, tracking reference, or the `MetaAction::NotSupported` text.

### Section 3: Axis 2 — SQL dialect features

Separate tables per SQL category (DQL, DML, DDL, DCL, TCL, session/admin, procedural). Each entry identifies whether the keyword is in the highlighter list, whether multi-statement parsing handles it correctly, and whether there's UX hook logic around it.

The goal isn't enumerate every keyword — it's to identify the *vendor-specific* clauses and constructs that demand attention. SQL-92 stuff is assumed to work and only called out if it doesn't.

### Section 4: Browser limitations for this DBMS

The canonical list for this DBMS of meta-commands or SQL features that are BL or BD. Every entry cites either [[OS Provider Seam]] (BL) or a one-sentence "not a product we're building" (BD).

### Section 5: What's intentionally out of scope

Features of the native CLI we're choosing not to implement even though we could. Usually because they duplicate a pane we already have or a UX pattern that's better in a browser. Examples: psql's `\pset pager on` (our scroll IS the pager), mysql's `--prompt` format (use our prompt customization instead), psql's `\timing` toggle (we show exec_time_ms unconditionally).

Being explicit here prevents "why don't you have X?" from becoming a scope debate later.

## Browser Limitations — canonical list

The things a browser-based SQL client fundamentally cannot do, or can do only with a specific seam implementation in a specific [[Deployment Topologies|deployment mode]].

| Capability | Tier | Reason | Unblocks with |
|---|:---:|---|---|
| `\!` / `SYSTEM` / `\shell` — OS shell escape | BD | Browser isn't a terminal. Wrong tool. | Never — by design. |
| External pager (`less`) | BD | Browser's native scroll IS the pager. | Never — replaced. |
| Read arbitrary file path without user consent | BL | Browsers sandbox the filesystem. | [[OS Provider Seam]] `BrowserOsProvider` (reads via file picker) or `BackendOsProvider` (reads on backend host with policy). |
| `SELECT INTO OUTFILE` to arbitrary path | BL | Same reason. | `BrowserOsProvider.write_download` (save as download) or `BackendOsProvider.write_file` (write on backend). |
| `LOAD DATA LOCAL INFILE` | T3+BL | Needs file-picker + streaming through the MySQL wire protocol. Non-trivial. | `BrowserOsProvider` + a streaming `ExecuteQuery` variant. |
| `COPY FROM STDIN` | T3+BL | Same shape as LOAD DATA LOCAL. | `BrowserOsProvider` + streaming `ExecuteQuery` for Postgres. |
| Raw TCP connections to arbitrary hosts | BD | Browser sandbox and, more importantly, wrong product. | Never — not a SQL client's job. |
| Client cert auth where cert lives on local filesystem | T3 | Needs a path for the browser to collect the cert. | `BrowserOsProvider` cert import + backend-side TLS config. |
| Arbitrary binary execution | BD | Not a product feature. | Never — use SSH on the host instead. |
| Session persistence across backend restart | T3 | Backend restarts drop every live session. | Backend-side session snapshot/restore story; see [[Deployment Topologies]]. |

Future Tier 3 items should cite rows from this table as their unblocking prereqs instead of repeating the reasoning.

## What happens at the start of a parity interview

A parity interview is a structured conversation between me (the assistant) and the user (the DBA) that produces an inventory doc for one DBMS. The rough shape of a round:

1. I propose a category from Axis 1 and name every command I know of in that category for the target DBMS.
2. The user confirms / adds / corrects the list, flags anything I missed.
3. We assign a tier to each command. When we disagree on a tier, the user wins.
4. I commit the rows to the per-DBMS inventory doc.
5. I flag every new `T3 needs X` row so the `X` prereq gets captured in a vault doc if it isn't already.
6. We move to the next category.

Each category is its own round. 15 minutes of back-and-forth per round, maybe 6–10 rounds per DBMS. The doc grows incrementally and merges at the end of each round so future sessions pick up where we left off.

After all the categories are walked, I do one consolidation pass to look for cross-category inconsistencies (same command listed twice under different tiers, tracking references pointing at non-existent docs, tier assignments that violate the [[OS Provider Seam]] matrix). Then the doc is final for v0.0.1.

## Related docs

- [[DBMS Planes]] — where meta-command implementation actually lives.
- [[Shell Design Philosophy]] — the terminal-like surface this framework drives.
- [[OS Provider Seam]] — the primary unblock for file I/O parity items.
- [[Deployment Topologies]] — determines which BL items lift in which modes.
- [[PAM Integration Seams]] — determines which `SecurityPolicy` denials apply in enterprise mode.
