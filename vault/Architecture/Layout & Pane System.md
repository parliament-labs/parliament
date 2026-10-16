# Layout & Pane System

## Inspiration: cmux

cmux achieves a lot of density without feeling cluttered:
- Full-width title bar at the top: app title + action icons (bell, +) on the far right, tight against the OS window controls
- Sidebar below the title bar: session/window list, no wasted space
- Main content area: tab bar at top with tab labels + close ×, split/add controls on the far right
- Each tab hosts a content surface (terminal, browser, etc.)

We can't put icons next to the OS window controls (that's the browser's chrome), but we can put them in a full-width **app header bar** that spans the top of our viewport — visually analogous to cmux's title bar.

## Target Layout

```
┌─ App Header (full width, ~38px) ──────────────────────────────────────────────┐
│  ⬡  Parliament                                                         ⚙     │
├─────────────────┬─────────────────────────────────────────────────────────────┤
│                 │  [Local MySQL ×]  [Monitor ×]      [+Shell][+Pane][‖][⊟]  │
│  CONNECTIONS    ├─────────────────────────────────────────────────────────────┤
│  ● Local MySQL  │  (content of active tab)                                   │
│  ○ Staging      │                                                             │
│                 │                                                             │
└─────────────────┴─────────────────────────────────────────────────────────────┘
```

## App Header Bar

Full-width, spans sidebar + main content. ~38px tall.

**Left side**: app icon (⬡) + "Parliament" title

**Right side** action buttons:
- ⚙ Settings — opens Settings pane or modal
- Future: 🔔 notifications indicator

The + for new connection stays in the sidebar CONNECTIONS section header — it's a connection-management action, not an app-level action.

## Tab Bar

Lives at the top of the workspace (right side, below app header). Shows open panes as tabs.

**Left section** — tab list:
- Each tab: label + × close button
- Active tab: accent underline, full-brightness text, background matches content below
- Inactive tabs: muted text, hover brightens
- Tabs can be dragged to reorder (future)

**Right section** — pane actions (tight together, like cmux):
- `[+ Shell]` — add a new shell pane (requires new session = new connection)
- `[+ Pane]` — dropdown or popover: Object Browser / Monitor / History / etc.
- `[‖]` — split workspace vertically (left/right)
- `[⊟]` — split workspace horizontally (top/bottom)

## Pane Types

| Pane | Tab label | Notes |
|------|-----------|-------|
| Shell | Connection name (e.g. "Local MySQL") | Primary surface |
| Object Browser | "Objects" or db name | Schema tree |
| Monitor | "Monitor" | Live process list |
| History Search | "History" | Search past queries |
| Settings | "Settings" | Global + beautifier config |
| Notebook | Notebook name | Saved query notebook |

## Split View Architecture

A split divides the workspace into two **panels**. Each panel has its own independent tab bar and tab list. Splits can be nested (panel → split → panel), like a tiling window manager.

```
┌─ Tab Bar ─────────────────────────────────────────────────────────────────┐
│ [Local MySQL ×]                              [‖][⊟]                      │
└─────────────────────────────────────────────────────────────────────────── ┘
┌────────────────────────┬──────────────────────────────────────────────────┐
│ Panel A                │ Panel B                                          │
│ [Local MySQL ×]        │ [Objects ×]  [Monitor ×]                        │
│─────────────────────── │─────────────────────────────────────────────────│
│ (shell content)        │ (active pane content)                           │
└────────────────────────┴──────────────────────────────────────────────────┘
```

After a split, each panel has its own tab bar. New tabs open in the focused panel.

### Split State Model (sketch)

```rust
enum WorkspaceNode {
    Leaf(PanelState),
    SplitH { left: Box<WorkspaceNode>, right: Box<WorkspaceNode>, ratio: f32 },
    SplitV { top: Box<WorkspaceNode>, bottom: Box<WorkspaceNode>, ratio: f32 },
}

struct PanelState {
    tabs: Vec<TabItem>,
    active_tab: usize,
    focused: bool,
}
```

This recursive tree represents any arrangement of splits. A single panel is a `Leaf`. Splitting a leaf creates a parent `SplitH` or `SplitV` with two `Leaf` children.

Resize handles: draggable dividers between split panels, adjusting the `ratio`.

Closing the last tab in a panel: panel collapses, its sibling takes over the full space.

## Implementation Phases

> **Naming note:** these phases predate the **Data tab CRUD** roadmap (Phases 4–7 there cover UPDATE / Insert / Delete / multi-cell / Upsert and ship under their own track). Phase numbers in this file are the Layout & Pane System track only — they are unrelated to the Data tab phase numbers documented in `CLAUDE.md` and the commit log.

**Phase 1 (now)**: App header bar + tab bar chrome. Single panel, one tab (shell). Split icons are visual stubs.

**Phase 2**: Settings pane. `⚙` opens it as a tab.

**Phase 3**: Object Browser pane. `[+ Pane]` → Object Browser opens as a second tab.

**Phase 4**: Split view. `[‖]` or `[⊟]` splits the workspace, each panel gets its own tab bar.

**Phase 5 (Layout track)**: Multiple shell tabs. Adding a second shell tab requires selecting which connection it uses (a mini-connection picker popup).

## Browser Constraints vs cmux

| cmux (native) | Our browser client |
|---------------|-------------------|
| Can overlay icon buttons on OS title bar | App header bar is our equivalent |
| OS window resize controls | Browser handles those |
| Full keyboard capture (Ctrl+A, etc.) | Some keys intercepted by browser |
| Pixel-perfect resize handles | CSS resize or pointer event drag |
| Tabs can detach into new OS windows | Browser tabs handle that naturally |

The constraints are real but manageable. The cmux *feel* — dense, professional, everything a tab — is fully achievable in a browser.
