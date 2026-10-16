---
description: Rules for Tyto (Leptos 0.6 WASM frontend) — reactive gotchas, WASM check command, CSS traps
paths:
  - tyto/**
---

# Tyto rules (`tyto/**`)

Browser WASM frontend. Leptos 0.6 CSR, compiled to `wasm32-unknown-unknown`. Communicates with Noctua via WebSocket only.

## WASM check — mandatory after every edit

```bash
cargo check -p tyto --target wasm32-unknown-unknown
```

`cargo check` alone only checks native targets. Tyto must compile clean for both. Some crates (e.g. anything pulling in `tokio`'s full feature set, filesystem APIs) break WASM — if a new dep fails here, reconsider the dep.

## Leptos RwSignal Copy/non-Copy rule

`RwSignal<T>` is `Copy` in Leptos 0.6 — closures can freely capture it. But `String` (and other non-Copy values) captured by closures need `.clone()` aliases inside reactive blocks. Each `view!` that needs a string reactively usually declares its own `.clone()` alias. Closures that capture non-Copy values across multiple reactive scopes are a subtle correctness hazard — look for `move ||` blocks reusing a `String` without cloning.

## CSS shorthand vs longhand trap

Writing `background: var(--x)` in an override **resets** `background-repeat` / `background-position` back to defaults. The schema-explorer dropdown caret shipped tiled because of this. When a parent rule relies on longhand (`background-repeat: no-repeat`), child rules must use longhand too (`background-color:` or `background-image:`), not the `background:` shorthand.

Same trap exists for `margin`, `padding`, `border`, `font`.

## Dark/light theme contrast

Dark theme uses light text; light theme uses dark text. Any hover / selected / active rule that changes background must also address text contrast. The `.tab:hover` bug shipped for exactly this — background changed, text color stayed, result was invisible text on the new background.

## Module layout reference

- `main.rs` — App component + WebSocket handler + `ServerMessage::IntrospectResult` parser (column-alias lookup lives here).
- `types.rs`, `shell.rs`, `explorer.rs`, `activity.rs`, `format.rs`, `export.rs`, `history.rs`, `dom.rs`, `parser.rs`, `highlight.rs`.
- `plane/` — per-DBMS plane modules. See the plane-scoped rules (auto-loaded when editing under `tyto/src/plane/**`).

## ConnectionDef serde contract

Any new field on `ConnectionDef` (in `shared/`) needs `#[serde(default)]`. Existing `~/.sqlclient/connections.json` files must keep deserializing — no breaking changes to the on-disk format without migration logic.

## Pane focus and cross-pane actions

- Bring a pane to the front only through `focus_pane` (exposed to render code as `PaneOps::focus`). It sets the workspace tab, the leaf's active tab and `active_pane_id` together; setting `active_pane_id` alone selects a pane that stays hidden behind its leaf's current tab.
- `SessionOpened` doesn't change focus — whoever creates a pane decides whether it's in front (set `active_pane_id` at creation for a foreground pane).
- SQL that one pane runs in another goes through `shell::run_in_shell` (the Enter path), never a raw `ExecuteQuery` onto a shell's session: results aren't correlated to requests, so an injected query is misread as whatever that shell was waiting for.
- A shell's user statements go out through `shell::send_to` + `shell::deliver`: to the live session; or, the session lost, as the plane's CLI carries on (`LostConnection`) — to a new one opened first (mysql), or refused with "You are currently not connected to a database." after psql's reset failed. Don't guard a new send path on `session_id` / `session_stale` by hand; ask `send_to`.
- Callbacks render code needs from the app (which owns the layout) travel as Copy values like `PaneOps`, captured at construction — `use_context` returns None inside click handlers.

## Rendering panes without rebuilding them

- A pane is built once and stays until it's closed. Each workspace holds its panes in one flat keyed `<For>` (`render_workspace_panes` in main.rs), each placed with `position: absolute` over its leaf by `pane_layout::place` — the split tree's geometry as `calc(N% + Mpx)` rects, pure and unit-tested. A split, close, zoom, divider drag or window resize only restyles panes. Don't nest panes inside layout boxes: then every layout change rebuilds every pane (scrollback back to the top, explorer list position lost) and Leptos panics (`OwnerDisposed`) when a pane built in one update is dropped before its effects first run.
- The pane list keeps panes in first-seen order (`pane_layout::stable_order`), not tree order: a keyed `<For>` whose order changes moves nodes in the DOM, and a moved node loses its scroll.
- The chrome follows the layout from the same `Placement` memo: tab bars keyed by leaf, dividers keyed by their split's ratio signal. Tab-bar height and divider thickness are `TAB_BAR_PX` / `DIVIDER_PX` in `pane_layout.rs`, not CSS.
- A pane off screen (an inactive tab, or behind another leaf's zoom) keeps its place and size, unpainted (`content-visibility: hidden` + `visibility: hidden`). Workspaces stay mounted: an inactive one — or every one while the connection form shows — is `display: none`, which keeps scroll positions.
- A tab bar re-renders when its leaf's tabs change, not on a tab click or a `\c` — its action buttons follow the active tab, and each label its tab's database, in closures of their own. Replacing the tab under the pointer swallows a double-click's second click (zoom never fired).
- Write `layout` and `zoomed_pane_id` together inside one `batch`, and the zoom only when it changes (`set` always notifies): one layout update per change.
- Focus a shell input with `dom::focus_in_place` (`preventScroll`) when it isn't a response to typing — a plain `focus()` scrolls a reader who scrolled up back to the bottom.
- Don't create signals that must outlive the moment inside an effect: they belong to the effect and are disposed when it re-runs. Create them from an event (a click, a WebSocket message) — e.g. a `\d`-opened explorer's `TableDetail` is created in `SessionOpened`, not in an effect.
- Regression: `tests/playwright/regression/pane-tabs-keep-state.spec.ts`.

## Shell idioms

- Shell is one div; input textarea lives inline at the bottom of scroll content, not fixed footer.
- Prompt format: `N SQL>` where N is the sequence number.
- History persists to localStorage keyed by connection identity. Only through `history::persist_command`, which refuses anything `may_hold_secret` flags — never write typed text to browser storage another way. Persist what was typed (`TypedStatement::history_text`), not the `:var`-substituted SQL: a variable's value may be a password.
- Meta-commands route through the active plane (see plane rules).
- The input is read in order through `shell::process_input` (a `Callback`, so the shell can read on later): statements before the first command, the command, what follows. Anything that makes the shell wait must show in `shell::shell_busy` — a new reply-awaiting state goes there, or what's left of the input (`resume_input`) reads on before the reply. Clear `querying` (or your state) on the reply and its error. Drop the rest only through `shell::drop_rest`.
- A meta-command that needs several catalog queries (psql's `\d <table>`, `\dx+`, `\dF+`, `\dFp+`, `\dRp+`, `\lo_unlink`'s transaction; the mysql CLI's `help topic`, the server's `HELP`) is **staged**: the plane returns `MetaAction::Staged(queries)`, `shell::start_staged_meta` sends them as one `IntrospectBatch` (tag `meta:<nonce>`, state in `ShellTab.pending_meta`, `querying` set), and `shell::continue_staged_meta` hands each reply to `DbmsPlane::meta_continue` until it returns the entries to show. Don't add per-query introspect tags and reply bookkeeping for such commands.
- `ShellEntry::Printed { lines, links }` is text a native CLI would print, line for line (copy = the lines); `links` are spans that open the Schema Explorer via `PaneOps::open_explorer`, also listed in the block's right-click menu.
