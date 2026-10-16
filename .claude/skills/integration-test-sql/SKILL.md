---
name: integration-test-sql
description: Run canonical plane SQL against seeded Pagila + Sakila and check for drift. Use before committing plane-layer changes to tyto/src/plane/** — catches dialect regressions that type-checking and unit tests can't see.
allowed-tools: Bash, Read, Write, Edit, Grep, Glob
---

# /integration-test-sql

Run a curated set of plane-representative SQL queries against the seeded Pagila (Postgres) and Sakila (MySQL) fixtures, then compare the output against golden files. Any drift is reported as a diff.

## When to invoke

- Before committing any change under `tyto/src/plane/**`.
- When debugging "the schema explorer renders differently on Postgres vs MySQL" kinds of reports.
- After upgrading sqlx, rust toolchain, or DB server versions.

## Prerequisites

Pagila and Sakila must be seeded. If not:
```bash
tests/fixtures/setup.sh all
```

Both databases must be running locally:
- Postgres on `127.0.0.1:5432`, database `pagila`, user `postgres` (no password)
- MySQL on `127.0.0.1:3306`, database `sakila`, user `root` (no password)

These match `tests/playwright/fixtures/connections.sample.json` — same seeds.

## Usage

From repo root:

```bash
# Check for drift against existing golden files
./.claude/skills/integration-test-sql/run.sh check

# Capture new golden output (run once after establishing a correct baseline).
# Overwrites existing golden files — use deliberately.
./.claude/skills/integration-test-sql/run.sh capture

# List what queries exist
./.claude/skills/integration-test-sql/run.sh list
```

## How to interpret results

- **No output + exit 0** on `check` → no drift. Plane is stable across the queried methods.
- **Diff output + exit 1** on `check` → one or more queries returned different results than last capture. The diff shows plane/method granularity.
- If a diff is the result of **intentional** plane work (e.g., you changed `table_status_sql` to add a new column), re-capture: `./run.sh capture`, verify the new baseline is correct, and commit.
- If the diff is **unexpected**, the plane has regressed — investigate before committing.

## Extending (adding a new query)

Each query lives as one file per DBMS under `queries/<dbms>/<name>.sql`. The filename (minus `.sql`) becomes the comparison key. Example layout:

```
queries/
├── postgres/
│   ├── list_schemas.sql
│   ├── list_tables_public.sql
│   ├── status_film.sql
│   └── columns_film.sql
└── mysql/
    ├── list_schemas.sql
    ├── list_tables_sakila.sql
    ├── status_film.sql
    └── columns_film.sql
```

**Golden output** lives at `golden/<dbms>/<name>.txt`.

Rules when adding a query:
1. Mirror the actual SQL the plane emits — copy from `tyto/src/plane/<dbms>.rs` and substitute concrete schema/table names (Pagila: `public` / `film`; Sakila: `sakila` / `film`).
2. Keep the output deterministic — avoid `NOW()`, random ordering (`ORDER BY 1` every listing), cache-sensitive counts.
3. Use `AS` column aliases that match the plane's column-alias contract (see `.claude/rules/plane.md`).
4. After adding, run `capture` to write the baseline, verify it looks correct, and commit both the query and the golden file.

## Known limitations

- Row-count-sensitive queries can drift when Pagila/Sakila are updated upstream. If that happens, re-capture is the right answer.
- The harness uses `psql` / `mysql` CLI tools, not the actual tyto WASM parser. It tests the SQL, not the full introspect pipeline. Playwright regressions still catch the rendering side.
- Only schema-introspection SQL is covered today. Meta-command translation, DDL reconstruction, syntax-highlighting keyword lists are not exercised here.

## Scope

This skill is a drift detector, not a test writer. It does not propose new queries or fix broken ones — it just flags that something changed. When it flags drift, delegate analysis to `sql-dialect-expert` (agent) or read the plane source yourself.
