# MySQL Schema Explorer v0.0.1 Build Brief

## Purpose

Replace the current placeholder Schema Explorer tab with a real graphical MySQL-aware explorer pane.

This is **not** a generic cross-database browser.  
For v0.0.1, this pane only needs to support **MySQL**.

The goal is to give users a familiar, clean, powerful GUI surface for exploring MySQL databases and objects inside the existing pane/tab/workspace shell of the app.

This brief is intentionally limited in scope so version 0.0.1 can be built quickly and used as a foundation for later iterations.

---

## Product Context

The application already has:

- a sidebar containing connection definitions
- a workspace per active connection
- panes within a workspace
- tabs within each pane
- terminal-like SQL tabs already working

The new MySQL Schema Explorer is a **graphical pane/tab content type** that lives inside this shell.

It should feel visually distinct from the SQL terminal tabs.

Terminal tabs are text-oriented.  
The MySQL Schema Explorer should feel like a structured GUI tool.

---

## Scope for v0.0.1

Build a first usable MySQL Schema Explorer with these capabilities:

1. Display a persistent connection context header at the top
2. Display a left-hand MySQL object tree
3. Display a right-hand object workspace
4. Support selecting a table from the tree
5. Show basic information for the selected table in a tabbed workspace
6. Include a clean visual layout that clearly differs from the terminal UI

No editing is required yet.  
No save/apply workflow is required yet.  
No permissions UI is required yet.  
No PostgreSQL support is required yet.

This version is primarily a **browse and inspect** interface.

---

## Design Goals

The MySQL Schema Explorer should:

- feel like a serious GUI tool, not a terminal screen with boxes
- visually pop against the darker terminal-oriented shell
- use a clear, structured layout with good spacing and hierarchy
- prioritize readability and clean alignment
- avoid large empty placeholder regions
- look deliberate and polished, even if some content is mock or incomplete

---

## High-Level Layout

Use this layout inside the Schema Explorer pane:

```text
┌──────────────────────────────────────────────────────────────────────┐
│ Connection Context Header                                            │
├──────────────────────────────────────────────────────────────────────┤
│ Toolbar                                                              │
├───────────────────────┬──────────────────────────────────────────────┤
│ Navigator Tree        │ Object Workspace                             │
│                       │                                              │
│                       │ Header: selected object                      │
│                       │ Tabs: Overview | Columns | Indexes | DDL     │
│                       │                                              │
│                       │ Active tab content                           │
│                       │                                              │
├───────────────────────┴──────────────────────────────────────────────┤
│ Status area (optional, minimal for v0.0.1)                           │
└──────────────────────────────────────────────────────────────────────┘
```

For v0.0.1, the bottom status area can be very small or omitted if unnecessary.

---

## Required Regions

### 1. Connection Context Header

Always visible at the top of the pane.

Its job is to clearly identify what MySQL server and database context the user is currently viewing.

For v0.0.1, show at least:

- connection display name
- database engine label: MySQL
- host and port if available
- currently selected database, when applicable
- simple connection status indicator

Example content:

- `Local MySQL`
- `MySQL · localhost:3306`
- `Database: mysql`
- connected status indicator

This header should feel like an identity bar for the pane.

### 2. Toolbar

A lightweight toolbar directly below the connection header.

For v0.0.1, include only a small useful set of actions:

- Refresh
- Expand All
- Collapse All
- Filter/search box for tree items

More actions can be added later.

### 3. Navigator Tree

Left side of the pane.

This is the main navigation area.

For v0.0.1, support this structure:

- Databases
  - database name
    - Tables
      - table names
    - Views
    - Procedures
    - Functions
    - Triggers

It is acceptable if only Tables are fully functional in v0.0.1 and the other groups are placeholders.

Requirements:

- expandable/collapsible
- clear selection state
- hover state
- readable indentation
- lightweight icons if easy to add
- filter/search should narrow visible items

### 4. Object Workspace

Right side of the pane.

This is where the selected object is displayed.

It must never feel like a blank placeholder area.

For v0.0.1, selecting a table should populate this workspace with:

- object title
- object subtitle/context
- tab strip
- active tab content

---

## Initial Supported Object Type

### MySQL Table

Version 0.0.1 only needs to meaningfully support table inspection.

When a table is selected, the workspace should show:

#### Object Header
Display at least:

- table name
- database name
- object type label such as `Table`

Example:

- `user`
- `mysql · Table`

#### Object Tabs
For v0.0.1 include these tabs:

- Overview
- Columns
- Indexes
- DDL

The first selected tab can default to `Columns` or `Overview`. Either is acceptable.

---

## Tab Content Requirements

### Overview Tab

A compact summary view for the table.

For v0.0.1, include simple metadata where available, such as:

- database
- table name
- engine
- collation
- estimated row count if available
- comment if available

This can be rendered as a clean definition list or simple two-column property grid.

### Columns Tab

This is one of the most important tabs.

Render a clean grid/table with columns such as:

- Column Name
- Type
- Nullable
- Key
- Default
- Extra
- Comment

This grid must be easy to scan.

### Indexes Tab

Render a clean grid/table with columns such as:

- Index Name
- Unique
- Index Type
- Columns

If detailed data is not fully available yet, use partial real data or structured placeholders.

### DDL Tab

Display the CREATE TABLE statement or equivalent DDL text.

This should be shown in a code-oriented viewer area.

A monospaced font is appropriate here.

---

## Visual Direction

The MySQL Schema Explorer must **not** look like the terminal tabs.

### Visual Intent

- terminal surfaces are darker and text-oriented
- the Schema Explorer should feel like a richer graphical tool
- it should visually stand apart inside the same application shell

### Basic Style Guidance

Use a layered appearance:

- shell/chrome may remain dark
- explorer workspace should be lighter or otherwise more structured than terminal tabs
- tree panel and workspace panel should be visually distinct
- toolbar, object header, tabs, and content regions should be clearly separated

### Readability

Prioritize:

- clean spacing
- consistent alignment
- good row height
- good column spacing
- strong visual hierarchy
- obvious hover and selected states

### Typography

Use a mixed approach:

- normal UI font for labels, headers, buttons, tree items
- monospaced font only where it improves clarity:
  - DDL
  - code-like values
  - optionally data grids if it looks good

Do **not** make the entire pane monospaced.

---

## Interaction Requirements

For v0.0.1:

- clicking a database expands or focuses its contents
- clicking a table loads that table into the object workspace
- switching tabs updates the workspace content
- filter box narrows the visible tree items
- refresh reloads the tree and the active object if possible

No inline editing is required yet.

No context menus are required yet, though adding them later should be easy.

---

## Layout and Spacing Guidance

The UI should feel intentional and carefully spaced.

Use a small consistent spacing system, for example:

- 4px for tight internal spacing
- 8px for normal control spacing
- 12px for grouped spacing
- 16px for section spacing

General guidance:

- tree rows should not feel cramped
- table/grid rows should be readable
- headers should have room to breathe
- avoid random spacing values

---

## Minimum Component Breakdown

Please structure the implementation into reusable components rather than one giant view.

Suggested components:

- `MySqlSchemaExplorerPane`
- `ConnectionContextHeader`
- `SchemaExplorerToolbar`
- `MySqlNavigatorTree`
- `ObjectWorkspace`
- `ObjectHeader`
- `ObjectTabStrip`
- `OverviewPanel`
- `ColumnsGrid`
- `IndexesGrid`
- `DdlViewer`

Exact names can vary, but keep the design modular.

---

## Data Model Guidance

Do not over-generalize for future databases yet.

For v0.0.1, it is acceptable to use a MySQL-shaped data model.

Examples of useful concepts:

- connection info
- database summary
- table summary
- column metadata
- index metadata
- ddl text

Keep the design extensible, but do not build a generic cross-database abstraction layer yet unless it falls out naturally.

---

## Error and Empty States

Provide at least basic states for:

- no object selected
- loading
- failed to load
- empty filter result

These states should still look intentional and styled, not like raw debug text.

---

## What Matters Most in v0.0.1

If tradeoffs are necessary, prioritize these in order:

1. clean overall layout
2. clear connection context header
3. usable navigator tree
4. good table columns view
5. good tabbed object workspace
6. reasonable visual polish
7. real data where possible

A clean, convincing browse experience is more important than deep functionality in the first version.

---

## Explicit Non-Goals for v0.0.1

Do **not** spend time yet on:

- PostgreSQL support
- generic multi-database abstraction frameworks
- permissions explorer
- object editing workflows
- save/cancel/apply logic
- inline data editing
- visual query builders
- advanced DDL diffing
- deep admin surfaces

Those can come later.

---

## Deliverable

Implement a MySQL-only graphical Schema Explorer pane that replaces the current placeholder and feels like the first version of a serious database GUI.

It should be sufficiently complete that a user can:

- open the pane
- immediately understand what MySQL connection they are looking at
- browse databases and tables
- select a table
- inspect table information through a clean tabbed workspace

That is enough for version 0.0.1.
