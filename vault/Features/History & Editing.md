# History & Editing

## Current State

- Arrow Up/Down walks the in-memory command history
- Prompt shows the historical command number while browsing
- ArrowDown at position 0 restores the pre-browse input (`saved_input`)
- Multi-line commands: ArrowUp only exits browsing if already in browse mode (preserves cursor movement in multi-line input)

## Editing an older entry

`\e N` once loaded history entry N into the input. It was retired on 2026-10-01: in psql `\e N` is a line number, and `\e` now opens the real external editor (see CLAUDE.md's `\e` bullet). Editing an older entry comes back with the shell rework as a block-menu action — right-click a block → Edit in input / Open in editor. psql itself has no command for it.

## Future: History Search

`\f pattern` or Ctrl+R style incremental search through history.

In a browser context, Ctrl+R is typically "reload page" — so we'd need a different keybinding. Options:
- A hotkey that opens a floating history search overlay
- A dedicated [[History Search]] pane

The search pane idea is compelling: a modal or side-panel that shows all historical queries with fuzzy search, and clicking one loads it into the input. See [[History Search]].

## Future: Persistent History (SQLite)

Currently, history disappears when the page reloads or the session disconnects. 

A SQLite database in the middle tier (or client-side via `idb` / localStorage) would persist history across sessions. See [[Session History Persistence]].

## Future: `!n` Bang Syntax

Many SQL clients support `!n` to re-run command N without loading it into the editor. The real `mysql` CLI doesn't have this, but bash history does. Could be useful.

```
12 SQL> !7     ← re-run command 7 as-is
```

Implementation: detect `!` followed by a number in the Enter handler, look up the SQL, call `submit_query` directly.

## Future: `/run n` Alternative Syntax

Some clients use `/run n` or `/exec n`. The plane architecture (see [[DBMS Planes]]) could define DBMS-agnostic shell commands with a `/` prefix distinct from DBMS-specific `\` commands.

Open question: is `/command` notation worth the complexity, or is the block menu enough?
