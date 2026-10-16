# Notebook & Scripts

## The Concept

A notebook is a named collection of SQL cells, similar to a Jupyter notebook but for SQL. Each cell is a query (or multi-query block) with associated output. Notebooks can be saved, shared, and replayed.

This is the "collaboration" surface — not just a personal query history but a shareable artifact.

## Use Cases

- **DBA runbook**: a set of diagnostic queries the DBA runs when a performance incident happens. Saved as a notebook, runnable with one click.
- **Onboarding docs**: "here are the queries to understand our schema" — a living document newer developers can run.
- **Migration checklist**: pre/post migration verification queries. Each step is a cell. Run them in order, see the results inline.
- **Incident postmortem**: replay what was run during an incident; share with the team.

## Cell Types

- **SQL cell**: a query + its most recent output (table or vertical)
- **Markdown cell**: prose, headings, notes, diagrams
- **Result cell**: read-only, shows output from a previous run

## Notebook vs Script

**Script**: a flat `.sql` file. Sequential queries, no prose, no output storage. Closer to `mysql -e < script.sql`. Useful for automation.

**Notebook**: cells with preserved output, prose between queries, interactive. Closer to Jupyter.

Both are worth supporting. Scripts first (simpler) — just an editor pane with a run button. Notebooks are a bigger project.

## Storage

### Open Source
Local storage in `~/.sqlclient/notebooks/`. Directory of markdown-ish files (could be a SQLite table instead).

A simple format:
```markdown
---
title: Schema diagnostic
connection: production
created: 2025-06-15
---

## Check table sizes

```sql
SELECT table_name, 
       round(((data_length + index_length) / 1024 / 1024), 2) AS size_mb
FROM information_schema.tables 
WHERE table_schema = database()
ORDER BY size_mb DESC
LIMIT 20;
```

## Active connections

```sql
SELECT user, count(*) FROM information_schema.processlist GROUP BY user;
```
```

This format is readable outside the app and renderable by Obsidian/GitHub.

### PAM-backed (enterprise)
Notebooks stored as the PAM system's shared records. A team member creates a notebook, shares it with the DBA team. Everybody sees it. Changes sync.

This is the bridge between "personal tool" and "team tool."

## UI Design

The notebook pane would look something like:

```
┌────────────────────────────────────────────────┐
│  Schema Diagnostic                [Run All] [+] │
├────────────────────────────────────────────────┤
│  ## Check table sizes                          │
│  ┌──────────────────────────────┐              │
│  │ SELECT table_name, ...       │  [▶ Run]     │
│  └──────────────────────────────┘              │
│  ┌─────────────────┬──────────┐                │
│  │ table_name      │ size_mb  │                │
│  ├─────────────────┼──────────┤                │
│  │ orders          │  842.30  │                │
│  │ users           │  124.50  │                │
│  └─────────────────┴──────────┘                │
│  2 rows in set (4 ms)                          │
├────────────────────────────────────────────────┤
│  ## Active connections                         │
│  ...                                           │
└────────────────────────────────────────────────┘
```

## Relationship to the Shell

The shell and notebook are complementary. The shell is for exploration — ad hoc queries, one-off checks. The notebook is for repeatable, documented work.

A "save to notebook" action from the shell would bridge the two: highlight a query in history, click "save to notebook," it becomes a new cell in a selected notebook.

## Implementation Priority

Low priority — this is a significant feature. Order:
1. Shell (done ✅)
2. Meta-commands (done ✅)  
3. [[Object Browser]]
4. [[Session History Persistence]] (prerequisite for notebook storage model)
5. Script editor (simpler version of notebook)
6. Full notebook with markdown cells
