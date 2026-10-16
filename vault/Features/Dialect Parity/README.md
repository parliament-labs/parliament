# Dialect Parity

Per-DBMS inventories of native-CLI parity — meta-commands and SQL feature coverage.

The framework these docs operate on lives at [[Dialect Coverage Framework]]. Read that first if you're new to the project or plan to contribute inventory rows — it defines the two axes, the tier scheme (T1 / T2 / T3 / BL / BD), the runtime grace contract, and the canonical inventory doc shape.

## Completed inventories

- **[[MySQL]]** — the `mysql` CLI command inventory for MySQL 8.0+. Interview complete across both axes. ~180 rows catalogued. Six implementation tracks identified; no implementation started.
- **[[Postgres]]** — the `psql` CLI command inventory for PostgreSQL 15+. Interview complete across both axes. ~310 rows catalogued (Axis 1 + Axis 2). Tracks 1–6 are cross-plane extensions of MySQL's; two new Postgres-specific tracks: a metadata catalog sprint (~28 `\d*` commands) and an operator-highlighter polish.

## Planned inventories

- **MariaDB** — a separate inventory, not merged with MySQL. MariaDB is a MySQL fork but has diverged enough that assuming feature parity silently breaks users. Each metadata schema, sys view, and performance-schema surface drifts between versions and vendors; we don't cross-populate.
- **Percona Server** — same reasoning as MariaDB. Percona Server is upstream-MySQL-compatible for most purposes but ships its own tooling, additional `performance_schema` surfaces, and management extensions. Separate inventory.
- **Aurora MySQL** and **Aurora Postgres** — eventually, and also as separate inventories since Aurora's control plane has its own quirks.

## No cross-dialect command borrowing

A deliberate project-level decision: **we do not borrow commands across DBMS family lines**. Postgres users who type `SHOW TABLES` inside our Postgres shell get an error from the Postgres server (because `SHOW` in PG is a different thing); we do not translate it into `\dt`. Conversely, MySQL users who type `\dt` get whatever MySQL does with it (an error), not a translated `SHOW TABLES`.

Why: every cross-translation bakes in an assumption that the user actually meant the other dialect, and that assumption silently rots the moment the user hits a command that *does* exist in their dialect and expects it to work. "Helpful" cross-translation is a category of footgun with no way out. Better to be strict — the plane dispatches to its own dialect, and a DBA who wants something else opens a shell against a different connection.

The same principle applies within a family: we don't add MariaDB-specific metadata commands to the MySQL plane just because MariaDB is "MySQL-compatible". The underlying metadata may or may not exist on the server the user is actually connected to.

## How to read an inventory

Each per-DBMS doc has these sections:

1. **Summary** — what CLI version is the benchmark, current coverage status, family variant policy.
2. **Meta-commands** (Axis 1) — tables grouped by category, one row per command: Command / Aliases / Status / Tier / Notes.
3. **SQL dialect features** (Axis 2) — tables per SQL layer (DQL / DML / DDL / DCL / TCL / session-admin / procedural). Identifies vendor-specific syntax that needs highlighter / parser / UX-hook attention. SQL-92 baseline is assumed to work.
4. **Browser limitations for this DBMS** — rows that are BL (browser-blocked) or BD (by-design-skipped). Cross-refs [[OS Provider Seam]].
5. **Out of scope** — features we're intentionally not implementing, with reasons.

## Related docs

- [[Dialect Coverage Framework]] — the framework itself.
- [[DBMS Planes]] — where meta-command implementation actually lives.
- [[OS Provider Seam]] — the primary unblock for file-I/O parity items.
- [[Deployment Topologies]] — determines which BL items lift in which modes.
