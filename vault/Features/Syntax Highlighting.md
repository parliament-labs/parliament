# Syntax Highlighting

## The Two Surfaces

Syntax highlighting applies to two very different surfaces in the shell, and they have different complexity profiles:

### 1. History Entries (static scrollback)

Already-submitted SQL shown in the scrollback is rendered as static DOM nodes. We can post-process the SQL string into a list of `(text, token_kind)` pairs and render them as `<span class="sql-kw">`, `<span class="sql-str">`, etc.

**Complexity**: low. A ~200-line Rust tokenizer, no external library. Implementable now.

### 2. Live Input Textarea (as-you-type)

**Decision: use CodeMirror 6 when the time comes. No DIY overlay. Full stop.**

The "fake textarea" overlay trick (transparent `<textarea>` over a styled `<div>`) is a hack with a long tail of bugs: selection color breaks, IME input breaks, mobile Safari does something unexpected, scroll sync drifts. You end up owning a text editor widget. That is not what this project is.

**CodeMirror 6** is the right answer. It is modular, actively maintained, and has first-class SQL dialect support (`@codemirror/lang-sql` with explicit MySQL and Postgres configurations). It would replace the raw `<textarea>` entirely. Integration into Leptos/WASM requires a JS interop bridge (CodeMirror is a JS library; we mount it to a DOM node from WASM via `wasm-bindgen`), but that is a known, solvable problem.

**When to do it**: once the shell is feature-stable and the DBMS plane architecture is in place. CodeMirror's SQL mode can be configured with the plane's keyword list and eventually with schema-aware completions.

**Current state**: plain `<textarea>`. History entries get highlighting (see below). Input does not. That is acceptable.

---

## On Version Constraints

**Decision: no version-specific keyword lists.**

Rationale:
- Highlighting a keyword that exists in 8.0 but not 5.7 does no harm — the user typed it, they know what they meant
- Version-specific matrices are a maintenance burden (which version added `WINDOW`? `JSON_TABLE`? `LATERAL`?)
- We capture `server_version` at connect time in `SessionOpened` — if version-aware highlighting ever becomes a compelling need, the data is available
- The meaningful vocabulary differences are dialect (MySQL vs Postgres), not version

One keyword list per dialect. "MySQL broadly" covers 5.7 and 8.x without distinction.

---

## Token Types

```
Keyword      SELECT, FROM, WHERE, INSERT, ...     → blue (--accent)
Function     COUNT, SUM, CONCAT, IFNULL, ...      → yellow (--yellow)
StringLit    'hello', "world"                      → green (--green)
NumLit       42, 3.14, 0xFF                        → purple (--purple)
Comment      -- line    /* block */                → dim + italic (--text-dim)
Operator     = < > != >= <= + - * /               → default text
Backtick     `table_name`                          → muted (--text-muted)
Plain        identifiers, whitespace, punctuation  → default text
```

---

## Tokenizer Design

Lives in `tyto/src/highlight.rs`. A hand-written scanner (no parser — we only need tokens, not a parse tree):

```
State machine:
  Start
  → ' → StringSingleQuote (handle '' and \' escapes)
  → " → StringDoubleQuote (handle "" and \" escapes)
  → ` → Backtick
  → - - → LineComment (to end of line)
  → / * → BlockComment (to */)
  → 0-9 → Number (integer or decimal, optionally 0x hex)
  → A-Za-z_ → Word → check against keyword set → Keyword | Function | Ident
  → else → Operator | Punct
```

The keyword check: a `HashSet<&'static str>` (uppercase normalized). Words are uppercased before lookup. This handles `select`, `SELECT`, `Select` all matching the keyword list.

Performance: plenty fast for query-length strings. A few hundred keyword lookups per render, in WASM, is microseconds.

---

## Keyword List — Where It Lives

The keyword and function lists belong to the **DBMS plane** (see [[DBMS Planes]]). Each plane exposes:

```rust
pub trait DbmsPlane {
    fn keywords(&self) -> &'static [&'static str];
    fn functions(&self) -> &'static [&'static str];
    // ...existing meta-command methods...
}
```

For now (before the plane trait is formalized), the lists live as statics in `highlight.rs`:

```rust
pub static MYSQL_KEYWORDS: &[&str] = &[ "SELECT", "FROM", "WHERE", ... ];
pub static MYSQL_FUNCTIONS: &[&str] = &[ "COUNT", "SUM", "AVG", ... ];
```

---

## MySQL Keyword List (representative)

**DML**: SELECT, FROM, WHERE, INSERT, UPDATE, DELETE, INTO, VALUES, SET, JOIN, LEFT, RIGHT, INNER, OUTER, CROSS, FULL, ON, AS, USING, UNION, ALL, DISTINCT, GROUP, BY, ORDER, HAVING, LIMIT, OFFSET, WITH, RECURSIVE

**DDL**: CREATE, TABLE, VIEW, INDEX, DATABASE, SCHEMA, DROP, ALTER, TRUNCATE, RENAME, ADD, COLUMN, MODIFY, CHANGE, PRIMARY, KEY, FOREIGN, REFERENCES, UNIQUE, NOT, NULL, DEFAULT, AUTO_INCREMENT, CONSTRAINT, CHECK, ENGINE, CHARSET, COLLATE, TEMPORARY

**DML/Control flow**: IF, ELSE, THEN, ELSEIF, END, CASE, WHEN, BEGIN, COMMIT, ROLLBACK, TRANSACTION, START, SAVEPOINT, RELEASE, LOOP, WHILE, REPEAT, UNTIL, LEAVE, ITERATE, RETURN, CALL, DECLARE, PROCEDURE, FUNCTION, TRIGGER, EVENT

**Predicates**: IN, AND, OR, NOT, LIKE, RLIKE, REGEXP, BETWEEN, EXISTS, IS, TRUE, FALSE, UNKNOWN, ANY, SOME, ALL, EXCEPT, INTERSECT

**Data types**: INT, INTEGER, BIGINT, SMALLINT, TINYINT, MEDIUMINT, FLOAT, DOUBLE, DECIMAL, NUMERIC, CHAR, VARCHAR, TEXT, TINYTEXT, MEDIUMTEXT, LONGTEXT, BLOB, TINYBLOB, MEDIUMBLOB, LONGBLOB, DATE, TIME, DATETIME, TIMESTAMP, YEAR, BOOLEAN, BOOL, JSON, ENUM, BINARY, VARBINARY, BIT, GEOMETRY

**MySQL-specific**: SHOW, DESCRIBE, DESC, EXPLAIN, USE, KILL, REPLACE, IGNORE, STRAIGHT_JOIN, SQL_CALC_FOUND_ROWS, FOUND_ROWS, LOAD, DATA, INFILE, OUTFILE, GRANT, REVOKE, FLUSH, OPTIMIZE, ANALYZE, REPAIR, PARTITION, SUBPARTITION, ALGORITHM, DEFINER, SECURITY, INVOKER, WINDOW, OVER, PARTITION, ROWS, RANGE, UNBOUNDED, PRECEDING, FOLLOWING, CURRENT, ROW

**Functions**: COUNT, SUM, AVG, MIN, MAX, NOW, CURDATE, CURTIME, DATE_FORMAT, STR_TO_DATE, DATEDIFF, TIMESTAMPDIFF, IFNULL, NULLIF, COALESCE, IF, CONCAT, CONCAT_WS, GROUP_CONCAT, SUBSTRING, SUBSTR, LEFT, RIGHT, LENGTH, CHAR_LENGTH, TRIM, LTRIM, RTRIM, UPPER, LOWER, REPLACE, LPAD, RPAD, REPEAT, REVERSE, LOCATE, INSTR, ROUND, FLOOR, CEIL, CEILING, ABS, MOD, POWER, SQRT, RAND, FORMAT, CAST, CONVERT, CHAR, ASCII, ORD, HEX, UNHEX, MD5, SHA1, SHA2, AES_ENCRYPT, AES_DECRYPT, UUID, LAST_INSERT_ID, ROW_COUNT, FOUND_ROWS, FIELD, ELT, MAKE_SET, INET_ATON, INET_NTOA, JSON_EXTRACT, JSON_SET, JSON_OBJECT, JSON_ARRAY, JSON_CONTAINS, JSON_ARRAYAGG, JSON_OBJECTAGG, RANK, DENSE_RANK, ROW_NUMBER, NTILE, LAG, LEAD, FIRST_VALUE, LAST_VALUE, NTH_VALUE

---

## Postgres Keyword Differences (for future plane)

Key additions over MySQL:
- `RETURNING` — INSERT/UPDATE/DELETE returning clause
- `ILIKE` — case-insensitive LIKE
- `LATERAL` — lateral subquery
- `WITHIN GROUP` — ordered-set aggregates
- `FILTER (WHERE ...)` — conditional aggregates
- `TABLESAMPLE` — sampling
- `VACUUM`, `REINDEX`, `CLUSTER` — maintenance
- `COPY` — bulk load/export
- `SERIAL`, `BIGSERIAL` — old-style sequences
- `GENERATED ALWAYS AS IDENTITY` — SQL-standard sequences
- Data types: `JSONB`, `HSTORE`, `UUID`, `INET`, `CIDR`, `MACADDR`, `TSQUERY`, `TSVECTOR`, `ARRAY`, `RECORD`

---

## CodeMirror 6 Integration (future)

CodeMirror 6 has a `@codemirror/lang-sql` package with explicit MySQL and Postgres dialect configurations. It handles:
- As-you-type highlighting
- Auto-close for parens and quotes
- Indentation
- Multi-cursor editing
- Can be configured to suggest completions from a schema

Integration approach for Leptos:
1. Include CodeMirror as a JS dependency (via `<script>` tag in `index.html` or as a bundled asset)
2. In `tyto`, use `wasm-bindgen` to call a JS bootstrap function that creates the CodeMirror instance and mounts it to a DOM node
3. Bridge the `on_change` callback back to Leptos state via a JS closure → WASM callback

This is the right long-term answer for the input surface. Worth doing once the shell is feature-stable.
