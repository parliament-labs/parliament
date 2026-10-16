# Module Structure

The frontend (`tyto/`) is split into focused modules, each owning a distinct responsibility. The split was done at ~4,300 lines to keep each file navigable and to enforce clean module boundaries.

## Module inventory

| Module | Lines | Responsibility |
|--------|------:|---|
| `main.rs` | ~1,500 | App component, WebSocket handler, workspace management, sidebar, render_workspace_panes |
| `pane_layout.rs` | ~250 | Pane geometry: a workspace's split tree as absolute rects for its panes, tab bars and dividers (panes are built once and only moved) |
| `types.rs` | ~650 | All shared types: ShellEntry, ShellTab, PaneTree, MetaAction, TxnState, etc. |
| `shell.rs` | ~900 | Shell pane rendering, meta-command dispatch, submit_query, on_keydown, render_entry |
| `explorer.rs` | ~600 | Schema Explorer pane, object list, inline detail, column/index/DDL renderers |
| `activity.rs` | ~500 | Activity pane, sessions/locks tables, blocker tree, refresh timer |
| `parser.rs` | ~420 | The native CLI's lexer (`Lexicon`, `Pieces`): statement splitter (split_statements), backslash-command start (trailing_command), comment-only check + variable substitution (substitute_vars) |
| `format.rs` | ~250 | Pure formatting: type parser, number formatting, index tags, truncation |
| `export.rs` | ~100 | Result download: trigger_download, to_csv, to_json, to_sql_inserts, render_save_links |
| `history.rs` | ~35 | localStorage persistence: load_history, persist_command, make_history_key |
| `dom.rs` | ~100 | DOM helpers: ws_send, ws_introspect, scroll, textarea resize, timer setup |
| `highlight.rs` | ~200 | SQL syntax tokenizer for history entry highlighting |
| `plane/` | — | DBMS planes (mod.rs + mysql.rs + postgres.rs) |

## Dependencies between modules

- `types.rs` has no internal dependencies (pure data types)
- `format.rs`, `export.rs`, `history.rs`, `parser.rs`, `pane_layout.rs` depend only on `types.rs`
- `dom.rs` depends on `types.rs` + `web_sys`
- `shell.rs` depends on all of the above + `highlight.rs` + `plane/`
- `explorer.rs` and `activity.rs` depend on `types.rs`, `dom.rs`, `format.rs`, `plane/`
- `main.rs` depends on everything (it's the root)

## Design principles

- **Leaf modules first**: types, format, export, history, parser, dom have no cross-dependencies
- **Pane modules own their rendering**: shell.rs, explorer.rs, activity.rs are self-contained view subtrees
- **main.rs owns state + routing**: all app-level signals, the WebSocket handler, and workspace management stay in main.rs
- **No circular dependencies**: the dependency graph is a DAG rooted at main.rs
