# Shell Design Philosophy

## The Core Rule

**The shell must feel like the native CLI for the target database.**

A MySQL DBA who has used `mysql` for 20 years should sit down, connect, and immediately feel at home. The prompt numbering, the backslash commands, the vertical `\G` output, the error messages — all of it should be familiar. Same principle applies to Postgres users with `psql`.

This is a primary differentiator. Most GUI clients feel foreign to CLI users. We want CLI users to *prefer* this shell.

## Current Shell Behavior

- Numbered prompts: `1 SQL>`, `2 SQL>`, `3 SQL>`
- `;` at end + Enter → execute, horizontal table output
- `\G` at end + Enter → execute, vertical output
- Shift+Enter → newline within input (multi-line query)
- Arrow Up/Down → walk command history
  - Prompt shows the *historical* command number while browsing
  - Execute stamps the *current* sequence number (e.g., re-running command 3 from position 11 creates entry 11)
- Click anywhere in shell area → focuses input
- Auto-focus on session connect

## Layout Decision: Shell Is One Div

The shell is **one scrolling `div`**. The prompt and textarea live inline at the bottom of the scroll content — they are not a fixed footer.

Why this matters:
- In a terminal, content flows. The prompt is always at the end of the content, not pinned to the bottom of the viewport.
- A fixed footer creates a jarring split — history up there, input down here.
- When the user scrolls up to read old results, the current prompt scrolls out of view too. That's correct. Terminal behavior.

```
┌────────────────────────────┐
│  1 SQL> SELECT 1           │  ← history
│  ┌──────┐                  │
│  │  1   │                  │
│  └──────┘                  │
│  1 row in set (2 ms)       │
│                            │
│  2 SQL> SHOW TABLES\G      │  ← history
│  ──── row 1 ────           │
│  Tables_in_db: users       │
│  ...                       │
│                            │
│  3 SQL> ▊                  │  ← live input, inline
└────────────────────────────┘
```

## History Navigation Detail

- `saved_input` preserves what was typed before the user started browsing
- ArrowUp enters browsing mode; ArrowDown exits at pos=0, restoring saved_input
- ArrowUp is intercepted only if input is single-line OR already browsing (multi-line SQL stays editable)
- Prompt shows the historical sequence number while browsing
- On execute: stamps the *current* `query_count` as the history entry number, then increments counter

## Multi-line Input

Shift+Enter inserts a newline. The textarea auto-grows with content (scroll_height trick). When navigating to a multi-line historical command via arrow keys, `schedule_textarea_resize()` fires a requestAnimationFrame resize after the programmatic value set.

## Ideas for Future Shell Enhancements

- `\c` — clear current input (MySQL: clear in-progress statement)
- `\r` — reconnect (MySQL: reconnect with optional db and host args)
- Syntax highlighting in the textarea — even basic keyword highlighting would help readability
- Auto-complete — column names, table names from the current schema
- Persistent scrollback — the current shell clears on reconnect; SQLite-backed history would survive restarts (see [[Session History Persistence]])
