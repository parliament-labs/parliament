# Session History Persistence

## The Problem

Current state: shell history exists only in memory for the lifetime of the browser session. Reload the page, disconnect, or restart the backend — history is gone.

For serious DB work, you want:
- "What was that query I ran last Tuesday?"
- Search across all past queries
- Replay a previous session's work
- Audit trail of what was run against production

## Where to Persist

### Option A: SQLite in the Middle Tier

The Rust backend writes every executed query to a local SQLite database (`~/.sqlclient/history.db`).

```sql
CREATE TABLE query_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp   DATETIME DEFAULT CURRENT_TIMESTAMP,
    connection_id TEXT NOT NULL,         -- which connection
    session_id  TEXT NOT NULL,           -- which WebSocket session
    query_num   INTEGER,                 -- the N SQL> number
    sql         TEXT NOT NULL,
    rows_affected INTEGER,
    exec_time_ms INTEGER,
    had_error   BOOLEAN,
    error_msg   TEXT
);
```

Pro: survives page reloads. Single source of truth. Easy to query.
Con: tied to the backend host. In a multi-user PAM deployment, history would need a shared database.

### Option B: IndexedDB in the Browser

Store history in the browser's IndexedDB via WASM. Works offline, survives reloads.

Pro: no backend changes. Each user's history stays on their machine.
Con: lost if the user clears browser data. Doesn't work for the PAM use case where audit is centralized.

### Option C: PAM Audit Bus (future)

In the commercial tier, every query is already flowing through `SessionRecorder` (see [[PAM Integration Seams]]). History is stored in the PAM system's audit store, searchable by its admin UI.

For the open source tier, Option A (SQLite) is the right default.

## History Search UI

A [[History Search]] pane that provides:
- Full-text search across all past queries
- Filter by connection, date range, success/error
- Click to load a historical query into the shell input
- Sort by most recent or most frequently used

## Query Deduplication

If the user runs `SELECT 1` 500 times, should history store 500 entries or 1? Probably 500 — execution time and context matter. But "most frequently used" sort would benefit from deduplication.

## Retention Policy

How long to keep history?
- Default: last 10,000 entries, or 90 days (whichever is less)
- Configurable in `~/.sqlclient/config.toml` (future)
- In PAM mode: retention governed by the PAM system's audit policy

## Integration with Existing Shell

When SQLite history is implemented:
- On session start: load the most recent N entries into `shell_history` as a "previous session" marker
- Arrow Up on a fresh session should navigate into persisted history
- The persisted history source supplements in-memory history seamlessly

One UX question: should persisted history from previous sessions appear in the current shell's scrollback? Or only in the [[History Search]] pane? Mixed opinion. Probably History Search pane only — the shell scrollback is "this session" and should stay clean.
