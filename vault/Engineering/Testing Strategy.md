# Testing Strategy

## Four layers

### Layer 1 — Unit tests (pure functions)

`#[cfg(test)] mod tests` inline in each module. No database, no browser, no network. Runs via `cargo test -p tyto`. Covers:

- **parser.rs** — `split_statements` (delimiter detection, quote/comment awareness per CLI lexer — psql `E'…'`, dollar quotes, nested comments; mysql backslash escapes, `#`, `/*!`, `\g` / `\G` — custom delimiters), `trailing_command` (where a backslash command after SQL starts) + `substitute_vars` (`:name`, `:'name'`, `:"name"`, undefined variable preservation)
- **format.rs** — `parse_column_type`, `format_int_with_commas`, `human_bytes`, `truncate`, index tag computation
- **export.rs** — `csv_quote`, `to_csv`, `to_json`, `to_sql_inserts` (quoting, escaping, NULL handling)
- **highlight.rs** — tokenizer token-kind correctness for keywords, functions, strings, comments, operators, numbers

**Status:** implementing now. These are the highest-ROI tests — pure functions with complex edge cases.

### Layer 2 — Backend integration tests (real database)

`tests/` directory in `noctua/`. Requires a running MySQL and Postgres instance. Tests:

- Connection lifecycle (connect, query, disconnect)
- `execute_query` with known SQL → verify `ServerMessage` shape
- Error handling (bad SQL, missing tables)
- Type mapping edge cases (BINARY, BIGINT UNSIGNED, JSON, arrays, timestamps)

**Fixture management:**

```
tests/fixtures/
├── postgres_schema.sql    -- CREATE TABLE/VIEW/FUNCTION/TRIGGER/INDEX
├── postgres_data.sql      -- INSERT statements with representative data
├── postgres_teardown.sql   -- DROP everything cleanly
├── mysql_schema.sql
├── mysql_data.sql
└── mysql_teardown.sql
```

Fixture data should cover: normal values, NULLs, empty strings, unicode, very long text, timestamps, booleans, JSON/JSONB columns, Postgres arrays, edge-case numeric types.

Each test run: teardown → schema → data → tests → teardown. Idempotent.

**Status:** planned for next session.

### Layer 3 — End-to-end browser tests

Automated browser testing via headless Chrome (MCP browser tools or Playwright/Puppeteer through a Node.js harness). Tests:

- Connect to a database via the UI
- Run shell commands and verify rendered output
- Test meta-commands (\d, \sf, \safe-mode, etc.) end-to-end
- Test interactive features (up-arrow history, Ctrl+R search, split resize)
- Test Schema Explorer navigation and DDL tab rendering
- Test Activity pane sessions/locks display

**Status:** future. Needs investigation on the best tooling approach (MCP browser integration vs standalone harness).

**Why this layer matters — a real example:** In v0.3.5, moving toolbar buttons from per-split toolbars into the tab bar caused the pane-type dropdown menu to silently disappear. The root cause was `overflow: hidden` on `.pane-tab-bar` clipping the absolutely-positioned dropdown. This bug:
- Was invisible to `cargo check` (valid Rust)
- Was invisible to `cargo clippy` (no lint issues)
- Was invisible to `cargo test` (no unit test touches CSS layout)
- Was invisible to code review (the CSS property wasn't changed in that commit — it was *inherited* from the existing tab bar style)
- Was only caught by manual testing in the browser

A Layer 3 test that hovers the pane-menu icon and asserts the dropdown has non-zero dimensions would have caught this immediately. CSS layout interaction bugs — where a parent's overflow/position/z-index silently breaks a child's rendering — are a recurring class of failure in UI work that only browser-level testing can detect.

### Layer 4 — CI/CD pipeline

GitHub Actions workflow:

1. `cargo check` (native + WASM targets)
2. `cargo clippy` (zero warnings policy — eventually fix the pre-existing 10)
3. `cargo test -p tyto` (Layer 1 unit tests)
4. Spin up MySQL + Postgres via Docker
5. `cargo test -p noctua` (Layer 2 integration tests)
6. Optional: headless browser tests (Layer 3)

**Status:** future. Depends on Layers 1 + 2 being solid first.

## Future: Workload generator

A tool for generating realistic query workloads against test databases — parameterized by query mix (SELECT/INSERT/UPDATE/DELETE ratios), concurrency, data volume. Useful for:

- Performance testing the backend's connection handling
- Stress-testing the Activity pane's monitoring under load
- Validating the \watch auto-repeat under high-frequency polling

Complex problem; parks alongside the big-feature items. Noted here so the desire is captured.

## Conventions

- Unit tests live inline (`#[cfg(test)] mod tests` at the bottom of each module)
- Integration tests live in `tests/` directories within each crate
- Fixture SQL lives in `tests/fixtures/`
- Test functions use `#[test]` and descriptive names: `test_split_statements_quoted_semicolon`
- No test frameworks beyond `#[test]` + `assert!` / `assert_eq!` — keep it simple
- Tests must be deterministic — no randomness, no time-dependence, no network
