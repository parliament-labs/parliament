# History Search

## Purpose

Search all past queries — current session and persisted history (see [[Session History Persistence]]) — to find and replay previous work. The DBA equivalent of shell `history | grep`.

## UX Sketch

A modal overlay triggered by a hotkey (e.g., Ctrl+Shift+H, or a toolbar button):

```
┌─────────────────────────────────────────────────────┐
│  Search history...                    [✕]           │
├─────────────────────────────────────────────────────┤
│  ○ This session  ● All history                      │
│                                                     │
│  Filter: [_________________]  Connection: [all ▾]   │
│                                                     │
│  42  SELECT * FROM orders WHERE status = 'pending'  │
│      production · 2 min ago · 1,204 rows · 12 ms    │
│                                                     │
│  38  UPDATE orders SET status = 'shipped' WHERE ... │
│      production · 15 min ago · 47 rows · 3 ms       │
│                                                     │
│  31  EXPLAIN SELECT * FROM orders JOIN users ...    │
│      production · 1 hr ago · 8 rows · 1 ms          │
│                                                     │
│  ...                                                │
└─────────────────────────────────────────────────────┘
```

Clicking a result:
- Closes the modal
- Loads the SQL into the shell textarea
- Focuses the textarea

Double-clicking a result: immediately executes it.

## Search Behavior

- Fuzzy match on the SQL text
- Filter by connection name
- Filter by date range
- Filter by success / error
- Sort: most recent, most frequent

## Implementation Notes

This is a frontend feature once [[Session History Persistence]] exists. The query is something like:

```sql
SELECT id, sql, connection_id, timestamp, exec_time_ms, rows_affected, had_error
FROM query_history
WHERE sql LIKE '%pattern%'
  AND connection_id = 'optional_filter'
  AND had_error = 0
ORDER BY timestamp DESC
LIMIT 50
```

The middle tier would expose a new WebSocket message `SearchHistory { pattern, connection_id, limit }` → `HistoryResults { entries }`.

## Keybinding

Ctrl+R (standard reverse-search in bash) would be ideal but typically triggers a page reload in browsers. Options:
- Ctrl+Shift+H (H for History)
- Ctrl+Shift+R (override the reload shortcut — risky)
- A toolbar button (no keybinding required for MVP)
- The `\f pattern` meta-command in the shell (CLI-native feel)

The `\f pattern` (find) meta-command would fit the [[Shell Meta-Commands]] philosophy well: CLI users type `\f orders` and see matching past queries inline.
