# GUI Panes Overview

## The Vision

The shell is the primary interface — power users live there. GUI panes are *additional surfaces* that sit alongside the shell. Think of it like a terminal multiplexer: the shell is always there, but you can open panes next to it for specific tasks.

The layout model (inspired by cmux): sidebar + main area, where the main area can host the shell or any pane.

## Planned Panes

### [[Object Browser]]

A tree view of the database schema. Databases → Tables → Columns / Indexes / Foreign Keys. Clicking a table loads its structure and inserts the table name into the shell input. 

This is the most-requested feature in SQL clients. A DBA exploring an unfamiliar schema needs this constantly.

### [[Monitor Pane]]

A live view of database activity. `SHOW PROCESSLIST` + `SHOW INNODB STATUS`, refreshing on a timer. Shows active queries, locked waiting queries, connection count, etc.

This is a DBA tool, not a developer tool. Aurora-specific metrics could come from CloudWatch via the middle tier.

### [[History Search]]

A searchable log of all queries executed in this session (and eventually across sessions via [[Session History Persistence]]). Shows query text, execution time, rows affected, timestamp. Filter by keyword, date, connection.

### Query Plan Pane

When a SELECT is executed, offer to run `EXPLAIN` and display the query plan visually. Could be a DAG visualization of the optimizer's plan. Very useful for performance work.

### Table Data Pane

A grid-style data browser for a selected table. Paginated. Sortable columns. Inline cell editing (generates and executes UPDATE statements). This is the "spreadsheet view" that non-CLI users often want.

### Variable Explorer

Browse `SHOW VARIABLES` and `SHOW STATUS` with search and filtering. A DBA tuning a MySQL instance needs to see hundreds of variables — a searchable pane beats `SHOW VARIABLES LIKE '%buffer%'` over and over.

### Connections Pane (expanded sidebar)

The current sidebar is minimal. A richer version could show:
- Active connection indicators
- Last query time per connection
- Schema name
- Right-click actions: duplicate, edit, delete, copy connection string

## Layout Design (open question)

Options:
1. **Tab model**: shell + panes as tabs in the main area. Simple, familiar. Loss: can't see shell and pane at same time.
2. **Split model**: main area can be split horizontally or vertically (cmux-style). More powerful but more complex to implement in Leptos.
3. **Dockable panels**: panes dock to sides (left/right/bottom). IDE-style. Common in DataGrip / VSCode.

The cmux-style split model is the most aligned with the [[Shell Design Philosophy]] but requires implementing a proper tiling layout engine.

**Starting point recommendation**: Tab model first. Ship the shell + Object Browser as two tabs. Refactor to split layout later once pane content is proven out.

## Priority Order

1. Object Browser — most universally useful, drives schema discovery
2. Monitor Pane — DBA differentiator, natural next step after meta-commands
3. History Search — builds on [[Session History Persistence]]
4. Query Plan Pane — performance-focused DBAs will love this
5. Table Data Pane — broader audience appeal (non-CLI users)
6. Variable Explorer — niche but powerful for MySQL tuning
