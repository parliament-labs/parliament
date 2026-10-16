# SQL Completion

## The Vision

Intelligent completion that goes beyond keyword lists. When you type `SELECT * FROM ord`, the client knows the live schema and suggests `orders`. When you type `SELECT o.`, it knows `o` is aliased to `orders` and suggests `id`, `user_id`, `total`, `status`, `created_at`.

This is the feature that separates a real editor from a fancy text box.

## Completion Tiers

### Tier 1 — Keyword Completion (no DB required)
Complete SQL keywords and function names from the DBMS plane's static lists.
- `SEL` → `SELECT`
- `WHER` → `WHERE`
- `COUN` → `COUNT(`
- Works offline, no schema needed
- Already have the keyword lists in `highlight.rs` — completion reuses them

### Tier 2 — Schema Object Completion (requires live schema cache)
Complete database objects from the connected database:
- `FROM ord` → `orders`, `order_items`, `order_history`
- `USE ` → `production`, `staging`, `analytics` (list of databases)
- `SHOW TABLES` suggestions for the current database

Requires a schema cache. See [[Schema Cache]] section below.

### Tier 3 — Context-Aware Completion (requires parser)
Understand the query context to offer the *right* completions:
- `SELECT o.` → suggest columns of `orders` (because `o` is aliased to `orders` in the FROM clause)
- `INSERT INTO users (` → suggest column names of `users`
- `WHERE u.id = ` → suggest values? (too ambitious — but column names for the right table)
- `ON a.` → suggest columns of the aliased table

This requires partial SQL parsing — enough to understand table aliases and which clause the cursor is in. Full parse tree not required; just enough context.

### Tier 4 — Value Completion (ambitious / future)
- Complete enum values: `WHERE status = '` → `'pending'`, `'shipped'`, `'cancelled'`
- Requires knowing the column type and, for ENUMs, the defined values
- Probably too aggressive for most users — could be opt-in

## Schema Cache

The schema cache is the foundation for Tier 2 and Tier 3. It lives in the **middle tier** (the Rust backend), not the frontend:

- The middle tier holds the persistent `MySqlConnection` — it can query `information_schema` without a round-trip from the browser
- The cache survives frontend page reloads
- Multiple browser tabs share the same cache for a session
- In PAM mode, the schema may be governed by access policy (some tables hidden from some users)

### Cache Contents

```rust
pub struct SchemaCache {
    pub databases: Vec<String>,
    pub tables: HashMap<String, Vec<TableInfo>>,   // db → tables
    pub columns: HashMap<String, Vec<ColumnInfo>>, // "db.table" → columns
    pub refreshed_at: Instant,
}

pub struct TableInfo {
    pub name: String,
    pub table_type: String,  // BASE TABLE, VIEW, etc.
    pub row_count: Option<u64>,
}

pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub column_key: String,  // PRI, UNI, MUL
    pub extra: String,       // auto_increment, etc.
}
```

### Cache Population

On connect: fetch a lightweight schema snapshot (database names + table names for the current database). Column details loaded lazily — only when the user starts typing against a specific table.

WebSocket messages:
- `ServerMessage::SchemaSnapshot { databases, tables }` — sent on `SessionOpened`
- `ClientMessage::FetchColumns { database, table }` — lazy load
- `ServerMessage::ColumnList { database, table, columns }` — response

### Cache Invalidation
- Manual refresh: `\# ` meta-command (MySQL rehash equivalent), or a refresh button in the [[Object Browser]]
- Automatic on `USE dbname` / `\u dbname` — invalidate table list for the new database
- Time-based TTL: configurable, default 5 minutes
- On DDL: if the user runs `CREATE TABLE`, `ALTER TABLE`, `DROP TABLE`, invalidate the relevant cache entries

## Completion Engine Architecture

The completion engine runs **in the frontend (WASM)**:
- It receives the schema cache from the backend (via WebSocket) and stores it in a Leptos signal
- On each completion trigger, it runs the context analysis and ranking locally
- No round-trip for completions — latency must be imperceptible

```rust
pub struct CompletionEngine {
    schema: SchemaCache,
    plane: Arc<dyn DbmsPlane>,
}

impl CompletionEngine {
    pub fn complete(&self, sql: &str, cursor_pos: usize) -> Vec<CompletionItem> {
        let context = analyze_context(sql, cursor_pos);
        match context {
            Context::AfterFrom | Context::AfterJoin => self.table_completions(),
            Context::AfterDot(alias) => self.column_completions_for_alias(alias, sql),
            Context::AfterSelect => self.column_and_keyword_completions(),
            Context::AfterUse => self.database_completions(),
            _ => self.keyword_completions(),
        }
    }
}
```

### Context Analysis

A lightweight scanner (not a full parser) that determines what kind of token the cursor is in:

- Are we after `FROM`? → table name context
- Are we after `JOIN`? → table name context  
- Is there a `.` before the cursor with a word before the dot? → column context for that alias
- Are we after `USE` or `\u`? → database name context
- Are we after `INSERT INTO`? → table name context
- Are we after `(` inside an `INSERT INTO tablename (` → column name context
- Default → keyword context

This doesn't need to be perfect. Good-enough context analysis covers 90% of cases. Fall back to keyword completion when uncertain.

## Completion UI

CodeMirror 6 has a built-in completion popup system (`@codemirror/autocomplete`). We plug our `CompletionEngine` into CodeMirror's `CompletionSource` interface:

```typescript
// JS bridge
function sqlCompletionSource(context) {
    const result = wasmCompletionEngine.complete(
        context.state.doc.toString(),
        context.pos
    );
    return {
        from: result.replaceFrom,
        options: result.items.map(item => ({
            label: item.label,
            type: item.kind,   // "keyword", "table", "column", "function"
            detail: item.detail,
            boost: item.score,
        }))
    };
}
```

Completion items are typed so CodeMirror can show different icons:
- Keyword: `kw` → blue
- Table: `class` → table icon  
- Column: `property` → column icon
- Function: `function` → yellow

## Trigger Behavior

- **Auto-trigger**: after typing 2+ characters, show completions automatically
- **Manual trigger**: Ctrl+Space forces completion popup
- **Dot trigger**: after typing `.` in `tablealias.`, immediately show column completions
- **Tab/Enter**: accept completion
- **Escape**: dismiss

Auto-trigger threshold (2 chars) avoids popup spam on short words. Configurable in [[Settings]].

## Ranking Completions

Order matters. Bad ranking is annoying. Good heuristics:
1. Exact prefix match ranks above fuzzy match
2. Recently used tables/columns rank higher
3. Primary key and indexed columns rank higher in WHERE context
4. Aliases rank higher than base table names in the current query scope

## MySQL-Specific Completions

The MySQL plane provides:
- `SHOW` completions: `SHOW DATABASES`, `SHOW TABLES`, `SHOW PROCESSLIST`, `SHOW VARIABLES LIKE`, `SHOW STATUS`
- `@@` variable completions: `@@version`, `@@global.max_connections`, etc.
- Information schema shortcuts

## Postgres-Specific Completions

- `\d` → table names (psql meta-command completion)  
- `pg_catalog` schema completions
- `$1`, `$2` parameter placeholders in functions
- Schema-qualified names: `public.users`, `analytics.events`

## Connection to Object Browser

The schema cache that powers completions is the same data source that powers the [[Object Browser]]. Build the cache once, use it everywhere. The Object Browser is the visual face of the schema cache; completions are the interactive face.
