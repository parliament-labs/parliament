# Force-Graph Panel — Design Spec

Status: **DESIGN** — not yet implemented. Replaces and extends the design notes in `memory/project_force_graph.md`.

A new pane type that visualises database workload as an interactive force-directed graph. The product hypothesis: a DBA can see relationships, hot spots, and pressure points faster from a force layout than from any tabular telemetry view, the same way Obsidian's graph view reveals knowledge-base structure faster than a folder tree.

The hypothesis is **unproven**. Building this is partly an experiment: ship a usable v1, run it against a meaningful workload, decide whether the visualisation actually helps a DBA or whether it's a beautiful toy.

---

## 1. Vision

A pane that renders a force-directed graph derived from a workload data source, with multiple **views** that re-aggregate the same underlying data through different lenses. Pluggable inputs ranging from synthesised query logs (v1) to live `pg_stat_statements` polling (later).

The force-graph metaphor specifically buys:

- **Hot spots become visually obvious.** A heavily-queried table pulls many edges; a frequently-blocked query becomes a dense node. The eye finds these faster than scanning a table.
- **Adjacency is meaningful.** Tables that are often joined sit close; tables that are rarely co-accessed drift apart. Spatial proximity tells you what queries treat as one logical sub-system.
- **Drill-down is natural.** Clicking a node re-centres on it; the graph rebalances around the new focus. Same idiom as the ERD tab's click-to-navigate.

What it does NOT buy:

- It is **not** a replacement for `EXPLAIN`. An execution plan is a tree, not a graph.
- It is **not** a replacement for a sortable activity table. For "find me the top 10 longest-running queries", a table beats a graph every time.
- It is **not** a real-time always-on monitoring product. v1 is interactive exploration on a captured / synthesised dataset; live monitoring is a later phase with its own ergonomics concerns (sampling rate, sliding window, perf budget).

---

## 2. Goals & Non-Goals

### Goals

- Render a force-directed graph of workload-derived relationships, with **multiple view modes** sharing one input pipeline.
- Support **pluggable inputs** so a synthesised log, a captured production log, and a live polling source all feed the same analysis layer.
- Honour **per-DBMS, per-source capability differences** — some sources don't capture plans, some don't capture blocking, some give cumulative stats only. The analysis layer and UI degrade gracefully.
- Ship v1 against a **synthesised workload** so the visualisation work isn't blocked on real-source adapters.
- Be honest about the **product hypothesis being unproven**: build the smallest thing that lets us answer "is this useful?" before investing in adapter breadth.

### Non-goals

- Live continuous monitoring of a production server. Later phase, separate ergonomics.
- A general graph database visualiser (Neo4j Bloom, etc.) — the panel is workload-aware.
- Cross-DBMS workload merging — each session is rooted in one connection.
- Replacement for the existing Activity pane. The force-graph's lock/blocker view may eventually overlap, but Activity stays as the tabular default.

---

## 3. The Input Contract

The leverage point. Every adapter — synthetic, replay, live — emits the same canonical entry shape, and every view reads only this shape. Get this right and the rest of the architecture is replaceable parts.

```rust
struct QueryLogEntry {
    // Identity
    id:               u64,             // monotonic per-session
    timestamp:        DateTime<Utc>,   // when the query executed (or was sampled)
    source:           SourceTag,       // which adapter emitted this entry

    // Statement
    sql_text:         String,          // the query text (possibly normalised)
    sql_normalised:   Option<String>,  // pg_stat_statements-style normalised form
    statement_kind:   StatementKind,   // Select / Insert / Update / Delete / Ddl / Other

    // Tables touched (parsed from sql_text via sqlparser-rs)
    tables_read:      Vec<QualifiedTable>,
    tables_written:   Vec<QualifiedTable>,
    join_pairs:       Vec<(QualifiedTable, QualifiedTable, JoinKind)>,
    columns_filtered: Vec<QualifiedColumn>,  // WHERE / HAVING references

    // Execution metrics — every field is Option because not all sources fill them
    duration_ms:      Option<f64>,
    rows_affected:    Option<u64>,
    rows_scanned:     Option<u64>,    // from plan, when available
    pages_scanned:    Option<u64>,    // PG: shared_blks_hit + shared_blks_read
    plan:             Option<QueryPlan>,

    // Concurrency context
    backend_pid:      Option<u32>,
    blocked_by_pid:   Option<u32>,
    wait_event:       Option<String>,  // PG wait_event_type / wait_event
    locks_held:       Vec<LockInfo>,

    // Connection context
    user:             Option<String>,
    database:         Option<String>,
    application_name: Option<String>,
    client_address:   Option<String>,
}

enum SourceTag {
    Synthesised { generator_version: String },
    PgStatStatements,
    PgStatActivity,
    PgLogFile { format_string: String },
    MySqlSlowLog,
    MySqlProcessList,
    MySqlPerformanceSchema,
    MySqlGeneralLog,
}
```

Two principles guard this contract:

1. **Optional fields are honestly optional.** A view that needs `plan` declares that requirement; when absent, the view either degrades or gates itself out of the source-compatibility list. No silently-zero-filled fields.
2. **`source` always travels with the entry.** The UI surfaces it in tooltips and source-compatibility errors. The analysis layer can use it for source-specific corrections (e.g. processlist sampling vs full log replay).

---

## 4. Data Sources & Capability Matrix

Each (DBMS, source) is its own adapter. This matrix lives in the spec because it determines **which views work against which sources** — not all view+source combos are valid.

| Source | Kind | Currently Running | Historical | Plans | Blocking | Row counts | Key Caveats |
|---|---|---|---|---|---|---|---|
| **MySQL** processlist | poll | yes | no | no | partial | no | snapshot at poll; misses sub-poll-interval queries |
| MySQL slow query log | replay | no | yes | with extras | no | yes | only past `long_query_time`; needs `log_output=FILE` |
| MySQL general log | replay | no | yes | no | no | no | usually disabled in prod (volume); huge overhead |
| MySQL Performance Schema | poll/replay | both | bounded | sometimes | no | sampled | sampling means counts are estimates |
| **PG** `pg_stat_activity` | poll | yes | no | no | yes (`pg_blocking_pids`) | no | snapshot at poll |
| PG `pg_stat_statements` | poll | no | cumulative | no | no | yes | normalised text only; loses individual occurrences |
| PG log file | replay | depends | yes | only via auto_explain | only via deadlock logs | sometimes | **format is user-configurable** via `log_line_prefix` |
| PG `auto_explain` output | replay | no | yes | yes | no | yes | only for slow queries by default; lives in log file |
| **Synthesised** | replay | no | yes (fabricated) | yes | yes | yes | every field can be populated; reality-fidelity is the synth's responsibility |

### 4a. PG `log_line_prefix` is non-trivial

Postgres logs format every line per the running server's `log_line_prefix` setting. Standard tokens:

```
%a  application name              %p  process id
%u  user name                     %d  database name
%r  remote host + port            %h  remote host
%b  backend type                  %t  timestamp (no ms)
%m  timestamp with ms             %n  timestamp as Unix epoch
%i  command tag                   %e  SQLSTATE
%c  session id                    %l  session line number
%s  session start                 %v  virtual xid
%x  transaction id                %q  stop here in non-session lines
%%  literal %
```

Plus optional CSV log mode (`log_destination=csvlog`) and JSON log mode (PG 15+). The PG log adapter must:

1. Read `SHOW log_line_prefix` (or `pg_settings` row) on connect to learn the format.
2. Build a parser from that format string.
3. Handle CSV / JSON modes when `log_destination` is configured for them.
4. Handle continuation lines for multi-line statements.

This is **a real parser, not glue code**. We'd likely write `pg_log_parser` as its own crate. We can punt on it until a real-log replay adapter is needed (later phase).

### 4b. View ↔ source compatibility

Each view declares the canonical fields it requires. Sources that can't fill those fields are excluded from that view's source picker.

| View | Requires | Sources that work |
|---|---|---|
| Table-centric (joins) | `tables_read`, `tables_written`, `join_pairs` | all (parsing fills these from `sql_text`) |
| Query-centric (frequency / time) | `sql_normalised`, `duration_ms` (optional) | pg_stat_statements, slow log, synth |
| Plan-centric (rows, pages, cost) | `plan` | auto_explain, slow log w/ extras, synth |
| Lock/blocker (chains) | `backend_pid`, `blocked_by_pid` | pg_stat_activity, processlist (live), synth |

When the user selects a view that the current source can't power, the panel offers two paths: switch source or pick a different view.

---

## 5. View Catalogue

v1 ships **one** view fully (table-centric); the others are spec'd here so the architecture stays compatible.

### 5a. Table-centric (v1 lead view)

- **Nodes:** tables touched in the workload window
- **Node size:** proportional to query count touching that table (configurable: count vs total time vs rows scanned)
- **Edges:** join pairs from `join_pairs`
- **Edge weight:** join frequency (count of distinct queries that joined this pair)
- **Center/initial focus:** the most-queried table
- **Click-to-recenter:** clicking any node makes it the new centre and re-runs the simulation
- **Drill panel:** clicking opens a side drawer showing top-N example queries that touched this table, sorted by duration / frequency / rows
- **Inactive fade:** tables outside the current time window (or below a frequency threshold) drift into the periphery and dim

### 5b. Query-centric (later)

- **Nodes:** distinct query patterns (using `sql_normalised`)
- **Node size:** call frequency × mean duration ("query weight")
- **Edges:** shared-table relationships (queries that read the same table) and blocking relationships
- **Drill:** click → see actual occurrences with parameters, time histogram, plan if available

### 5c. Plan-centric (later)

- **Nodes:** tables, but coloured/sized by plan-derived metrics: total rows scanned, total pages read, sequential vs index scan ratio
- **Edges:** plan-operator relationships (e.g. "Hash Join between A and B")
- **Drill:** click → see top plans involving this table, with their query texts

### 5d. Lock/blocker (later — augments or replaces Activity blocker tree)

- **Nodes:** active backends/PIDs
- **Edges:** "blocked-by" relationships from `pg_blocking_pids` / processlist locks
- **Live animation:** new blocks appear, resolve when locks release
- **Cycles highlighted:** deadlock-of-three+ scenarios stand out

---

## 6. Analysis Pipeline

Each view is `(QueryLogEntry stream) → (aggregator) → (graph model: nodes + edges)`.

```rust
trait View {
    fn source_requirements(&self) -> SourceRequirements;
    fn aggregate(&self, entries: &[QueryLogEntry], window: TimeWindow) -> GraphModel;
}

struct GraphModel {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    layout_hints: LayoutHints,    // initial positions, fixed nodes, etc.
}

struct GraphNode {
    id:     NodeId,
    label:  String,                 // displayed text
    weight: f32,                    // drives node size
    kind:   NodeKind,               // Table / Query / Backend / etc.
    facets: HashMap<String, Value>, // view-specific metadata for tooltips
}

struct GraphEdge {
    from:    NodeId,
    to:      NodeId,
    weight:  f32,                   // drives edge thickness + spring strength
    kind:    EdgeKind,              // Join / SharedTable / BlockedBy / etc.
}
```

The aggregator runs in WASM (no backend round-trip needed once entries are loaded). Re-aggregation on:

- View change
- Time-window slider drag
- Filter toggle (e.g. "exclude reads", "only queries > 100ms")
- Source change

For typical workloads (10K–100K entries), full re-aggregation is sub-100ms in Rust. For larger sets we may need indexed pre-aggregations or a rolling-window incremental update. Defer that optimisation until we hit it.

---

## 7. Rendering

Two real options:

### 7a. d3-force via JS interop (recommended for v1)

- Battle-tested force simulation library
- ~10kb gzipped
- Mature collision detection, link strengths, alpha-decay scheduling
- We'd call it via `wasm-bindgen` with the canvas/SVG draw stepping in Leptos
- Familiar to anyone who's seen Obsidian's graph view (it uses d3-force)

### 7b. Rust physics in WASM (later option)

- No JS dep, pure-stack
- More code, more control
- Worth revisiting if d3-force perf hits a wall past ~500 nodes

**v1 picks d3-force.** The interop boundary is small (push nodes/edges, read back positions per tick). If we ever need to swap to a Rust sim, the GraphModel→position mapping stays the same.

### 7c. Canvas vs SVG

- **Canvas** scales better past ~100 nodes (bulk pixel ops vs DOM node count)
- **SVG** lets us reuse the ERD's interaction patterns (CSS classes, hover states, click handlers)
- **v1: Canvas** with overlay HTML for labels + click hit-testing. Worth the extra plumbing for the headroom.

### 7d. Performance budget

- 100 nodes: smooth 60fps during simulation
- 500 nodes: drops acceptable (~30fps) during simulation, snaps to 60 once stable
- 1000 nodes: may need clustering / level-of-detail; out of scope for v1

---

## 8. Test-Data Synthesis (v1-critical sub-project)

**This is the unblocker.** Without a meaningful synthesised workload, the panel renders nothing interesting against sakila / pagila and we can't tell if the visualisation works.

### 8a. Synthesis tool shape

A separate Rust binary (`bin/workload-synth` or similar) that takes:

- A target schema (introspected from a live test DB, or read from a SQL dump)
- A workload profile (configuration: hot-table skew, query-mix ratio, blocking scenarios, time span, etc.)
- A random seed (deterministic output for testing)

…and produces a stream of `QueryLogEntry` records as JSON Lines. Tyto's synth-source adapter reads that file.

### 8b. Workload profile shape

```toml
[workload]
duration_seconds = 600                    # simulated time span
total_queries    = 50000

[skew]
hot_tables      = ["customer", "rental", "payment"]
hot_share       = 0.6                     # 60% of queries hit hot tables
cold_share      = 0.05                    # 5% hit rare tables (long tail)

[mix]
select_share    = 0.85
insert_share    = 0.08
update_share    = 0.06
delete_share    = 0.01

[blocking]
contention_rate = 0.02                    # 2% of writes cause blocks
chain_depth_max = 4

[query_patterns]
# Each pattern is a SQL template + parameter ranges + execution profile
[[pattern]]
template    = "SELECT * FROM customer c JOIN rental r ON c.customer_id = r.customer_id WHERE c.active = ? LIMIT ?"
freq_share  = 0.15                        # 15% of all queries match this
duration_distribution = { kind = "lognormal", mu = 2.5, sigma = 0.8 }
rows_distribution     = { kind = "uniform", min = 1, max = 200 }
```

### 8c. What "good" synthesis looks like for v1

- **Plausible join graph.** Hot tables co-occur in joins more often than rare tables. Some tables appear only in 1-2 query patterns.
- **Realistic frequency skew.** 80/20 power law on table access — not uniform.
- **Blocking scenarios that the lock-graph view can render.** A few `UPDATE … WHERE id IN (long list)` writes that touch the same hot row.
- **Some long-running queries.** Tail-of-distribution durations so view#3 metrics aren't flat.
- **Deterministic on seed.** Same seed → same output, byte-for-byte. Critical for screenshot diffing and review.

### 8d. Phasing the synthesis tool itself

- **v1.0 of synth:** static query templates, simple mix ratios, basic time skew. Just enough to make views #1 (table-centric) light up.
- **v1.1:** blocking patterns, plan generation, write-heavy phases (so views #4 and #3 work).
- **v1.2:** importing a real production schema and inferring reasonable workload defaults from FK structure + table sizes.

The user expressed v1 priority as "synthesise a full query log file as a basis for this project" — synth v1.0 is the minimum for that.

---

## 9. UI Surface

### 9a. Pane type

A new `PaneKind::ForceGraph` (likely codenamed something owl-themed — `Strix`? Or its own term: `Murmur`, `Aria`) added to the layout system. Spawned from the per-DBMS supported-panes dropdown like the Activity pane.

### 9b. Panel layout

```
┌─────────────────────────────────────────────────────────────┐
│ [Source ▾] [View ▾] [Time window: ─●──────────] [Filters ▾] │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│                                                             │
│                  (force-graph canvas)                       │
│                                                             │
│                                                             │
├─────────────────────────────────────────────────────────────┤
│ Selected: customer | 12,455 queries | top join: rental ──── │
│ ┌─Top queries──────────────────────────────────────────────┐│
│ │ 4,012× SELECT … FROM customer JOIN rental …  avg 12ms   ││
│ │ 1,288× UPDATE customer SET last_update=…    avg 4ms     ││
│ └──────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

- **Top toolbar:** source picker, view picker, time-window scrubber, filter dropdown (mirrors Activity-pane controls)
- **Center:** the force-graph canvas
- **Bottom drawer:** drill-down for the currently-selected node — top-N queries / participants / metrics

### 9c. Interaction

- **Hover a node:** highlight it + its edges, dim everything else (same focus-mode pattern as the ERD tab)
- **Click a node:** re-centres on it, opens drill drawer
- **Drag a node:** pin it (the simulation respects pinned positions)
- **Scroll wheel:** zoom (cursor-anchored, same as ERD)
- **Search box:** find a node by name, highlight + recentre

### 9d. Source picker honesty

When the user picks a source, the panel **immediately** displays which views are available against that source (`pg_stat_statements` greys out the lock-graph view, etc.) with an inline tooltip explaining why. This avoids the "I clicked Lock view and got an empty graph, is it broken?" failure mode.

---

## 10. Phasing

### v1 — Synthesis-only, table-centric

- **Scope:** synth tool v1.0, table-centric view, source picker (synth-only), basic interactivity (hover / click / drag), drill drawer
- **Test data:** synthesised workload against pagila schema with documented profiles
- **Goal:** prove the diagram is useful at all. If it's not, v1 was the right scope to find out cheaply.

### v2 — Real-source adapters (PG)

- `pg_stat_statements` adapter (cleanest fields, no parsing)
- `pg_stat_activity` adapter (live polling, snapshot-based)
- Time-window slider works against polled data

### v3 — Additional views

- Query-centric view
- Lock/blocker view (uses `pg_stat_activity` + `pg_blocking_pids`)

### v4 — MySQL adapters

- Slow query log replay
- `processlist` polling
- Performance Schema reads

### v5 — Log-file replay

- PG log_line_prefix parser as its own crate
- MySQL slow log batch import

### v6+ — Advanced

- Plan-centric view (depends on auto_explain or slow log w/ extras)
- Rolling window live-monitoring mode
- Cross-session comparison ("workload before vs after this index")

---

## 11. Open Questions

1. **Pane name.** The panel needs a name. ForceGraph is descriptive but not in keeping with the owl-theme bestiary. Candidates: Strix (already reserved), Bubo, Glaux, Asio — or a non-owl term.
2. **d3-force interop or pure Rust?** Default is d3-force for v1; revisit if perf or the JS dep itself becomes a problem.
3. **Where does the synth tool live?** New crate `tools/workload-synth/`, or sub-binary in `noctua/`? Probably new crate so it has no `noctua` dependency.
4. **Time-window semantics.** Sliding window (last N minutes) vs fixed start/end? v1 likely fixed-range against a captured/synth log; sliding window is more meaningful when live.
5. **Synthesis profile catalogue.** Ship 3-4 named profiles (`oltp-heavy`, `read-mostly`, `write-burst`, `lock-contention`) so the user can pick one without authoring TOML.
6. **Persist drilldown selection across view changes?** Probably no — view changes redefine what "selected" means.
7. **Save / load workload sets.** Should the user be able to capture a real production run and reload it later? v1 says "the synth file is the file" — later phases might let the user import a pg_stat_statements snapshot or auto_explain log directly.
8. **Per-DBMS adapter packaging.** Adapters live in `tyto/src/plane/<dbms>/forcegraph/` paralleling the existing plane structure? Or a separate `forcegraph_source/` tree? TBD when we start v2.

---

## 12. Inspirations & Prior Art

- **Obsidian graph view** — interactive force layout with search + filter, smooth pan/zoom, node fade on activity decay. Closest spiritual sibling.
- **Neo4j Browser / Bloom** — graph database visualisation; sets the bar for force-graph polish but is graph-data native, not workload-derived.
- **Datadog APM service map** — live service dependency graph; informs the "live updates without flicker" design.
- **pgAdmin's "Database Statistics"** — a tabular cousin of what the table-centric view offers.
- **PgHero / pganalyze** — analyse and report on `pg_stat_statements` output; closest in input shape but render as tables/text, not graphs.

---

## 13. References

- `memory/project_force_graph.md` — original capture of the dream-feature note. This spec replaces it as the design source; the memory file can be left intact as historical record.
- `vault/Architecture/ERD_PANEL_SPEC.md` — sibling pane that shares some idioms (click-to-navigate, hover focus mode, cursor-anchored zoom). Where applicable, the force-graph pane reuses these patterns to feel like part of the same product.
- `tyto/src/plane/mod.rs` — DBMS plane trait. Force-graph source adapters slot here, paralleling the existing `table_*_sql` methods.
- `tyto/src/parser.rs` — Tyto's existing SQL splitter. The new sqlparser-rs integration is a meaningful upgrade and may absorb or replace this depending on how the AST API shakes out.
