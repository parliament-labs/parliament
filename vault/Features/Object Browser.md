# Object Browser

## Purpose

A tree view of the connected database's schema. Replaces having to type `SHOW DATABASES`, `SHOW TABLES`, `DESCRIBE tablename` manually.

## Layout Sketch

```
┌─────────────┐
│ ▼ mysql     │  ← database (current = bold)
│   ▶ tables  │  ← expandable groups
│   ▶ views   │
│ ▶ sys       │
│ ▶ testdb    │
└─────────────┘
```

Expanding "tables" under a database:
```
│ ▼ testdb      │
│   ▼ tables    │
│     users     │  ← click to load DDL / column list
│     orders    │
│     products  │
│   ▶ views     │
│   ▶ routines  │
```

Clicking a table name:
- Option A: Insert table name into shell input at cursor position
- Option B: Open a table detail panel showing columns, types, constraints, indexes
- Option C: Both — single click inserts, double-click opens detail

## Schema Queries

The object browser populates itself by running schema queries through the normal `ExecuteQuery` path:

```sql
-- List databases
SHOW DATABASES

-- List tables in a database  
SELECT TABLE_NAME, TABLE_TYPE, ENGINE, TABLE_ROWS, TABLE_COMMENT
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = 'dbname'
ORDER BY TABLE_TYPE, TABLE_NAME

-- Describe a table
SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT, EXTRA, COLUMN_COMMENT
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = 'dbname' AND TABLE_NAME = 'tablename'
ORDER BY ORDINAL_POSITION

-- Show indexes
SHOW INDEX FROM `tablename` IN `dbname`

-- Show create DDL
SHOW CREATE TABLE `dbname`.`tablename`
```

## Schema Cache

The object browser should not re-query the database on every expand. A schema cache in the frontend (or middle tier) with a manual refresh button.

Cache invalidation: 
- Manual refresh button (always available)
- Auto-refresh on `USE dbname` or `\u dbname`
- Time-based TTL (configurable, default 5 minutes)?

## Middle Tier Consideration

Alternatively, the middle tier could expose a `SchemaInspect { session_id }` WebSocket message that runs all the schema queries server-side and returns a structured schema tree. Benefits:
- Frontend doesn't need to drive multiple round-trips
- Schema cache lives in the middle tier (survives frontend refresh)
- The [[DBMS Planes]] plane can own the schema inspection queries

## Interaction with the Shell

When the user is in the shell and clicks a table name in the object browser:
- Insert `tablename` at the current cursor position in the shell textarea
- If the textarea is empty, insert `SELECT * FROM tablename LIMIT 100;` (quick-browse shortcut)

Open question: should inserting from the object browser auto-execute, or just insert text for editing? Probably just insert — it's less surprising.

## Design Questions

- Should the object browser be in the sidebar (replacing or extending the connections list) or in a pane alongside the shell?
- The sidebar approach keeps the shell at full width — good for long queries and wide result sets
- The pane approach allows resizing — power users might want it wide
- Recommendation: sidebar, replacing the connection list when connected (connections go to the toolbar or a smaller indicator)

## For Postgres

The Postgres plane would use `information_schema` queries that work for both MySQL and Postgres, plus Postgres-specific queries (`pg_class`, `pg_namespace`) for things not in the standard information schema.
