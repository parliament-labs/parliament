# SQL Beautifier

## The Vision

A user-definable SQL formatter with a **WYSIWYG settings panel** — you adjust a knob, the preview pane updates live to show exactly what your SQL will look like. No guessing, no surprise line breaks.

This is a differentiator. Every SQL client has a format button. Almost none let you configure *how* it formats. DBAs have strong opinions about SQL style — house standards, personal preference, team conventions. We respect that.

## Per-DBMS Beautifier

Each DBMS plane owns its beautifier configuration. MySQL conventions differ from Postgres conventions, and something like MongoDB (document query language, JSON-like) is structurally different enough to need its own formatter entirely.

The beautifier config is part of [[Settings]] — stored in `~/.sqlclient/settings.json` under a per-DBMS key.

## WYSIWYG Panel Design

The settings pane for the beautifier has two columns:

```
┌─ Beautifier: MySQL ─────────────────────────────────────────────────────────────┐
│                                                                                  │
│  KEYWORDS          ┌─ Preview ──────────────────────────────────────────────────┤
│  Casing: [UPPER ▾] │                                                            │
│                    │  SELECT                                                    │
│  CLAUSES           │      u.name,                                               │
│  New line: [✓]     │      o.total,                                              │
│  Indent:   [4  ▾]  │      COUNT(*) AS order_count                               │
│                    │  FROM                                                      │
│  COMMAS            │      users u                                               │
│  Position: [end ▾] │          LEFT JOIN orders o                                │
│                    │              ON u.id = o.user_id                           │
│  JOINS             │  WHERE                                                     │
│  Indent:   [✓]     │      u.active = 1                                          │
│  Each on own line: │      AND o.status = 'pending'                              │
│  [✓]               │  GROUP BY                                                  │
│                    │      u.name,                                               │
│  PARENTHESES       │      o.total                                               │
│  Inner spaces: [ ] │  ORDER BY                                                  │
│                    │      order_count DESC                                      │
│  [Reset defaults]  │  LIMIT 100;                                                │
│  [Apply]           └────────────────────────────────────────────────────────────┤
└─────────────────────────────────────────────────────────────────────────────────┘
```

Every change to a control on the left immediately re-renders the preview on the right. The preview SQL is a fixed representative sample that exercises all the configured rules — JOIN, WHERE, GROUP BY, subquery, etc.

## Configuration Options

### Keyword Casing
- `UPPERCASE` (most common in enterprise SQL)
- `lowercase`
- `Title Case` (unusual but some teams use it)
- `preserve` (don't change what the user typed)

### Clause Formatting
- Each major clause (`SELECT`, `FROM`, `WHERE`, `GROUP BY`, `ORDER BY`, `HAVING`, `LIMIT`) on its own line: yes/no
- Indentation of clause content: 0 / 2 / 4 / 8 spaces or tab
- Comma position: trailing (end of line) vs leading (start of next line)
  - Trailing: `col1,\n col2,\n col3`
  - Leading:  `  col1\n, col2\n, col3`

### JOIN Formatting
- Each JOIN on its own line: yes/no
- Indent JOIN relative to FROM: yes/no
- ON condition on same line or new line

### Subquery Formatting
- Indent subqueries: yes/no
- Subquery opening paren on same line or new line

### Parentheses
- Space inside parens: `f( x )` vs `f(x)`
- Opening paren on same line as keyword: `IN (\n` vs `IN (`

### Line Length
- Max line length before wrapping (0 = no limit)
- How to wrap: at operators, at commas, etc.

### Whitespace
- Blank line between major clauses: yes/no
- Trailing whitespace: strip (always yes)

## Beautifier Architecture

### Where Does Formatting Run?

**Frontend (WASM)** — the formatter runs in the browser, on the SQL string the user has typed. This is the right call:
- No round-trip to the backend
- Immediate, synchronous (format on keystroke or on demand)
- The formatter doesn't need DB access

### What Triggers Formatting?

Two modes:
1. **On demand** — a `\b` meta-command or a Format button in the toolbar. Replaces the textarea content with the formatted SQL.
2. **On execute** — optionally format before adding to history, so the scrollback always shows nicely formatted SQL regardless of how the user typed it.

Mode 2 is optional and configurable. Some people want to see exactly what they typed in history; others want normalized output.

### Implementation Options

**Option A: Write our own formatter in Rust**
- Full control over output
- We know exactly how our rules work
- Probably 1,000–2,000 lines of Rust
- Runs in WASM with no JS interop

**Option B: Wrap an existing SQL formatter library**
- `sqlformat` crate (Rust) — basic MySQL/Postgres support, limited configurability
- `sql-formatter` npm package — very configurable, has dialect support, actively maintained — but it's JS, requires interop
- `pgFormatter` — Postgres-focused, Perl, not usable in WASM

**Recommendation**: Start with the `sqlformat` Rust crate to validate the UX. Replace with a custom formatter when the configurability gaps become limiting. The WYSIWYG panel architecture works the same regardless — the formatter is swappable behind an interface.

```rust
pub trait SqlFormatter {
    fn format(&self, sql: &str, config: &BeautifierSettings) -> String;
}
```

## MongoDB / Non-SQL Note

MongoDB uses a JSON-like query syntax (`db.collection.find({ field: value })`). A "beautifier" for MongoDB is really a JSON/BSON pretty-printer with awareness of MongoDB query operators (`$match`, `$group`, `$lookup`, etc.).

When MongoDB support is added, the MongoDB plane would provide its own `SqlFormatter` implementation — likely wrapping a JSON pretty-printer with custom handling for `$`-prefixed operator keys.

This is far future. Note captured here so the abstraction is designed with it in mind.

## Integration with CodeMirror

Once CodeMirror replaces the `<textarea>`, the formatter feeds formatted SQL back into the CodeMirror editor instance. CodeMirror has a `Transaction` / `ChangeSet` API for programmatic edits that preserves undo history — so the user can `\b` to format, then Ctrl+Z to undo if they don't like it.

See [[Syntax Highlighting]] for the CodeMirror integration plan.
