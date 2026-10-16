# Persistent History

Shell command history persists to `localStorage` and survives page refresh, browser crashes, and unexpected termination.

## Two-tier architecture

**Tier 1 — Session history (in-memory):** Commands from the current pane session, stored in `ShellTab.shell_history`. Up-arrow walks this first.

**Tier 2 — Connection history (localStorage):** Every command ever run against this connection, timestamped. When up-arrow exhausts Tier 1, falls through to Tier 2.

## Storage key

Identity-based: `history:<db_type>:<host>:<port>:<database>`

- NOT keyed by connection UUID — deleting and re-adding the same server discovers previous history
- MySQL and Postgres histories never mix (db_type in the key)
- User excluded from key — queries are about the database, not the credential

## Write policy

Persist-on-submit (zsh's INC_APPEND_HISTORY): every command is written to localStorage immediately when submitted, not on disconnect or page unload. Survives crashes.

## Deduplication

Fish-style: when a command is submitted, any existing entry with the same SQL text is removed and the new one appended with a fresh timestamp. Prevents repeated commands from consuming the cap.

## Cap

2,000 entries per connection, FIFO eviction of the oldest after dedup.

## Search

- **Prefix-matching up-arrow**: type partial text + up-arrow filters to commands starting with that prefix (zsh-style)
- **Ctrl+R reverse incremental search**: substring match across the full history pool, case-insensitive

## Connection deletion

History is preserved by default when a connection is deleted. Re-adding the same server discovers the history. Future: optional "also delete history" checkbox.

## Implementation

- `history.rs`: HistoryEntry struct, load_history, persist_command, make_history_key
- `types.rs`: persisted_cmds signal on ShellTab
- `shell.rs`: query_strings() combines persisted + session history for up-arrow navigation
- `main.rs`: SessionOpened handler loads history on connect
