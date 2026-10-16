# ERD Panel — Implementation Spec

A read-only ERD panel that shows a target table and its surrounding relationship graph (grandparent → parent → target → child → grandchild). Optimized for DBAs picking join candidates while writing SQL.

The panel supports **two layout modes** with a toggle:

- **Grid view**: five vertical bands (grandparent | parent | target | child | grandchild), with horizontal/orthogonal edges. Reads like a layered schema.
- **Arc view**: target centered, generations placed on concentric arcs (parents/grandparents on the left, children/grandchildren on the right), with smooth Bézier curves. Reads like a radial fan.

Both views share the same data model, the same anchor rule, the same crow's-foot notation, and the same palette/typography/card styling. They differ only in geometric arrangement and the path shape of the edges.

---

## 1. Inputs

The panel takes:

- A **target table**: schema-qualified name plus its column list. For each column, the name and a kind tag if applicable: `pk` (primary key), `fk` (foreign key), `a` (indexed). Type/size/nullable/default are NOT shown in the ERD (they live in the schema panel).
- A list of **related tables** keyed by name, each with:
  - `gen`: integer in `[-2, -1, 1, 2]` indicating generation relative to target. `-2` = grandparent, `-1` = parent, `+1` = child, `+2` = grandchild.
  - `cols`: same shape as target.
- A list of **edges**, each: `{ from, fromCol, to, toCol }` where `from` is the parent table (PK side) and `to` is the child table (FK side). `fromCol` is the PK column on the parent, `toCol` is the FK column on the child.

Edges are between adjacent generations only (no skipping). Self-references and many-to-many junction tables are out of scope for v1.

---

## 2. Within-layer ordering (applies to both views)

Within each generation (band in grid view, ring-side in arc view), order tables to minimize edge crossings to adjacent generations. Use the **median / barycenter sweep** heuristic:

1. For each table in layer L, compute the median index of its connected tables in layer L-1 (forward sweep) or L+1 (backward sweep).
2. Sort the layer by median.
3. Sweep forward through layers [-1, 0, 1, 2], then backward through [1, 0, -1, -2]. Repeat ~8 passes (it converges fast; for small inputs the order stabilizes in 1–2 passes).
4. Tables with no connections to the sweep direction sort to the end (treat their median as +∞).

For sakila.address as target, the optimal child-layer order is `[customer, staff, store]` and the grandchild-layer order is `[payment, rental, inventory]` — the sweep arrives at this naturally.

---

## 3. Grid view: generations as columns

The diagram uses **five fixed vertical bands**, left to right:

| Band | Generation | Width |
|---|---|---|
| GRANDPARENT | -2 | 220px |
| PARENT | -1 | 220px |
| TARGET | 0 | 240px (slightly wider) |
| CHILD | +1 | 220px |
| GRANDCHILD | +2 | 220px |

- **Gap between bands**: 110px.
- Bands are labeled with tracked-out small caps (`GRANDPARENT`, `PARENT`, `TARGET`, `CHILD`, `GRANDCHILD`) at the top of the canvas. The `TARGET` label uses an accent (Phosphor Amber, see §10); the others use a quiet bone color.
- Bands collapse if empty (e.g. no grandparents → render four bands, not five with an empty slot).

---

## 4. Grid view: vertical placement (enables horizontal edges)

The grid layout positions tables vertically so that connected FK/PK column rows line up — that's what allows pure horizontal edge lines (see §5). Use generous vertical spacing; the canvas scrolls when needed, so don't try to fit everything in the viewport.

### 4a. Target

Place the target table at canvas vertical center (or at a fixed `headroom` y, e.g. `y = max(80, headroom)` where `headroom = 200`). Its top-left x is determined by band 0's x position.

### 4b. Parent chain (single-parent case)

For each parent table P with one outgoing edge `P.pkCol → C.fkCol`:

```
P.y = C.y + colYOffset(C, fkCol) - colYOffset(P, pkCol)
```

Where `colYOffset(table, col)` is the y of the vertical center of that column row within the table card (header height + (col index × row height) + half row height).

This places the parent so its PK row aligns horizontally with the child's FK row → the connecting edge is a single straight horizontal line.

Walk the chain outward from target: place gen -1 first (using target as child), then gen -2 (using gen -1 as child).

### 4c. Children (gen +1)

Children are stacked with min vertical gap between cards. Goal: distribute their FK rows (the columns referencing target) evenly across target's right-edge inner range.

1. For each child C, find its FK column to target. Compute desired anchor y on target's right edge:
   - Target inner range: `[target.y + 18, target.y + target.h - 18]`.
   - For N children sorted in band order, child `i` gets desired anchor `y_i = top + (bot - top) * i / (N - 1)` (or `(top + bot) / 2` if N == 1).
2. Compute desired y for each child card: `C.y = anchor_y_i - colYOffset(C, fkColToTarget)`.
3. Sort children by desired y, then enforce min vertical gap between cards (32px). If two children would overlap, push the lower one down. Don't push the topmost above `y = 80`.

### 4d. Grandchildren (gen +2)

For each grandchild G:

1. Collect all incoming edges from gen +1 (its parents in the children band).
2. For each parent P, compute "ideal" y for G: `P.y + colYOffset(P, P.pkCol) - colYOffset(G, G.fkCol)`. This is the y that would horizontally align G's FK row with P's PK row.
3. Average across all parents → `G.y_desired`.
4. Sort grandchildren by desired y; enforce min vertical gap (32px) by shifting later ones down.

### 4e. Final normalization

After all bands placed, find the minimum y across all cards. If `minY < 80`, shift everything down by `(80 - minY)` so the top card sits at y=80.

---

## 5. Grid view: edge routing

### 5a. Path geometry

Given a parent anchor `(px, py)` and child anchor `(cx, cy)` on opposite sides (anchors computed per the shared rule in §7):

- If `|py - cy| < 1`: draw a single straight horizontal line `M px py L cx cy`.
- Otherwise: draw a sharp three-segment H-V-H L-path: `M px py L midX py L midX cy L cx cy` with **no rounded corners**.

`midX` is the vertical-segment x. When multiple non-horizontal edges cross the same band gap, give each its own lane: sort by parent anchor y, then for N edges spread `midX` evenly between `min(px, cx)` and `max(px, cx)` at fractions `(i+1)/(N+1)`.

### 5b. Forbidden routings (grid)

- **No diagonal / free-angle lines.** Always horizontal or H-V-H orthogonal.
- **No rounded corners** at the L-path turns. Sharp 90° only.
- **No shared trunks.** Each edge is its own line; no two edges share a segment or visually overlap on a stretch.
- **No tiny jogs.** A 3px vertical zigzag from minor anchor mismatch reads as sloppy. The Pinned-Perfect rule (§7b) prevents these.

---

## 6. Arc view: concentric arcs

### 6a. Geometry

- **Target placed at canvas center** (cx, cy). The normalization pass at the end shifts everything if any card ends up at negative x or y.
- **Inner ring radius**: r1 = 400px. Holds parents (gen -1, left hemisphere) and children (gen +1, right hemisphere).
- **Outer ring radius**: r2 = 840px. Holds grandparents (gen -2, left) and grandchildren (gen +2, right).
- **Hemispheres**: parent side on the left (polar axis at angle π), child side on the right (polar axis at angle 0).
- **No band labels** in arc mode — there's no horizontal column structure to label. Hide the label row.

### 6b. Card placement on a side

Each card sits with its **inner edge** (the edge facing target) tangent to the appropriate ring. The card extends *outward* from there:

- Right hemisphere: top-left.x = `cx + r * cos(θ)`, so the card's left edge is on the arc.
- Left hemisphere: top-left.x = `cx + r * cos(θ) - cardWidth`, so the card's right edge is on the arc.
- top-left.y = `cy + r * sin(θ) - cardHeight / 2`.

For one item on a side, place it on the polar axis (θ = π for left, θ = 0 for right).

For N ≥ 2 items, distribute symmetrically around the polar axis with a **computed angular step** so cards never vertically overlap regardless of N or card heights:

```
requiredSep = max(cardHeights in this side) + 30px
stepAngle   = min(0.45π, asin(min(0.95, requiredSep / r)))
totalSpread = (N - 1) * stepAngle           // capped at ~0.9π by the stepAngle ceiling
startAngle  = sideAngle - totalSpread / 2
angles[i]   = startAngle + i * stepAngle    for i in [0, N-1]
```

The step formula guarantees that the vertical separation between adjacent card centers (`r * sin(stepAngle)`) is at least `cardHeight + 30px`. The 0.45π cap (~81° per pair) prevents both poles from being crowded when N is small but `r` is small relative to `cardHeight`. For sakila with 3 children at r1=400px and ~178px tall cards, this produces ~32° per step (~64° total spread), which gives ~210px vertical separation — exactly `cardHeight + minGap`.

If a side has more items than the spread can fit, increase `r` for that ring. Don't try to compress further; the cap exists to keep the visual readable.

### 6c. Edge routing (arc)

Smooth cubic Béziers. Given parent anchor `(px, py)` and child anchor `(cx, cy)`:

```
dx   = cx - px
dy   = cy - py
dist = sqrt(dx² + dy²)
k    = min(dist * 0.5, 160)               // control point offset

c1x  = parent.side === 'right' ? px + k : px - k
c2x  =  child.side === 'right' ? cx + k : cx - k

path = M px py C c1x py, c2x cy, cx cy
```

Both control points share the y of their endpoint, so the curve enters and exits **horizontally** at both ends. This means the crow's foot at the child end and the perpendicular tick at the parent end are oriented the same way as in grid mode — no special arc-mode handling needed for those markers.

The curves naturally arc out/in and don't cross cards at typical radii. If a curve does pass through another card, the radii are too small for the card sizes — bump r1 and r2 (and the radial gap r2 - r1) until clearance is restored.

### 6d. Forbidden routings (arc)

- **No shared trunks.** Each edge is its own curve.
- **No straight diagonals.** Use the Bézier; the curvature is what gives the arc view its identity.
- **No looping** (control points placed on the *wrong* side of an endpoint that produce a back-curve). The control point sign should follow `parent.side` and `child.side` per the formula above.

---

## 7. Anchor convention and routing rule (applies to both views)

### 7a. Anchor convention

Every edge has two anchors:

- **Child anchor** (FK side): precise. Always at the FK column row, on the side of the child card facing the parent.
- **Parent anchor** (PK side): forgiving. Lands on the parent card's facing edge at a y chosen to enable a horizontal line (grid) or a clean curve (arc). See §7b.

The PK is *implicit*. Don't anchor to the PK column row on the parent — it crowds every parent's signature column with a knot of converging lines for no information gain.

### 7b. Parent anchor placement (the Pinned-Perfect / Largest-Gap rule)

For each parent card and each side (left or right), batch all edges that anchor on that side and place them with this rule:

1. For each edge, compute its **ideal y** on the parent's facing edge: `idealY = childCard.y + colYOffset(childCard, fkCol)`. This is the y that would produce a straight horizontal line (or a flat curve, in arc mode).
2. Define the parent's inner range on that side: `[parent.y + 14, parent.y + parent.h - 14]`.
3. Categorize each edge:
   - **Pinned** if `idealY` is within the inner range.
   - **Floating** if `idealY` is outside (above or below).
4. **Place pinned anchors at their ideal y. Never shift them to make room for anything else.** Multiple pinned anchors on the same edge with close ideal y values are fine — even if their ticks visually overlap a bit, that beats forcing small zigzags.
5. **Place each floating anchor in the largest available gap** between already-placed anchors, with min 28px spacing on either side. Process floating anchors in ideal-y order so top-wanters take top gaps and bottom-wanters take bottom gaps. Compute gaps fresh for each floating edge so successive floats don't bunch up.

The result: every horizontally-alignable edge stays horizontal; every clamped edge gets a visually distinct slot. In grid view this means horizontal lines plus L-paths that cross pinned ones. In arc view this means flat curves plus more steeply-arcing curves that pass over pinned ones.

---

## 8. Cardinality notation (crow's foot)

Standard ERD crow's-foot, never open arrows.

### 8a. At the child end (the "many" side / FK)

A three-prong fan that **opens TOWARD the entity**. The apex sits ON the relationship line, ~10px back from the entity edge. Three prongs extend FROM the apex TO three points on the entity edge:

- One prong straight along the line direction.
- Two prongs splayed perpendicular to the line by ±5px on the entity edge.

In both views the line approaches the child end horizontally (the last segment of an L-path or horizontal line in grid mode; the bezier's horizontal tangent in arc mode). Crow's-foot orientation is therefore identical across modes.

### 8b. At the parent end (the "one" side / PK)

A single perpendicular tick crossing the line, positioned ~8px back from the parent's edge along the line, with half-length 5px (so total tick length is 10px).

### 8c. Stroke

Same color and weight as the relationship line itself. No fill.

---

## 9. Edge styling

Two visual weights to differentiate target-touching edges from generation-link edges:

- **Target relations** (any edge where target is either parent or child): stroke `oklch(80% 0.13 240)` at 85% opacity (≈ `#9ed1ffd9`), 1.25px stroke width.
- **Generation links** (gen-1 ↔ gen-2 edges, no target involvement): stroke `oklch(73% 0.12 240)` at 55% opacity (≈ `#6cb3f08c`), 1px stroke width.

Both: solid stroke, `stroke-linecap: round`, `stroke-linejoin: round`. No gradients, no glow, no animation.

The crow's foot prongs and the perpendicular tick use the same stroke as their parent edge.

---

## 10. Visual design

Mono everything. No sans-serif anywhere in the panel.

### 10a. Palette (warm-tinted dark)

| Token | Hex | Use |
|---|---|---|
| `ink` | `#0e0a08` | Canvas background |
| `soot` | `#15110d` | Card background |
| `char` | `#1f1a14` | Subtle highlight (FK/PK row tinting) |
| `rule` | `#231d16` | Hairline borders, dividers |
| `bone` | `#6e644f` | Tracked-caps section labels, `schema.` prefix |
| `ash` | `#a89a82` | Muted text, type annotations |
| `parchment` | `#ece2d2` | Primary text (column names) |
| `sentinel` | `#6cb3f0` | Default table name color, `(pk)` and `(fk)` annotations |
| `sentinel-bright` | `#9ed1ff` | Target table name (and target-relation edges) |
| `phosphor` | `#f0a04a` | TARGET band label, `(a)` annotation, brand mark |

No `#000`, no `#fff`. All neutrals carry warm chroma (~55–70° hue).

### 10b. Typography

Font: **JetBrains Mono Variable**, with `ui-monospace, SFMono-Regular, monospace` fallback. Single typeface across the entire panel.

| Role | Size | Weight | Notes |
|---|---|---|---|
| Wordmark / brand | 14px | 500 | `letter-spacing: -0.005em` |
| Card name (table) | 13px | 500 | Sentinel by default, sentinel-bright on target |
| Column name | 12px | 400 | Parchment |
| Column annotation `(pk)` `(fk)` `(a)` | 10.5px | 400 | Right-aligned within row, color per kind |
| Band label / section label | 10px | 500 | UPPERCASE, `letter-spacing: 0.16em`, Bone |

### 10c. Card geometry

- Border-radius: **4px** (`rounded.md`).
- Background: `soot`.
- Padding: 8px top/bottom on the name row; 4px top / 6px bottom on the columns container; 14px horizontal everywhere.
- Header divider: 1px `rule` stroke between the name row and the columns.
- **Target card**: 1px `sentinel` border (the only card with a border). Width 240px instead of 220px.
- No shadows. No glow. No gradient. Tonal layering only (`ink → soot → char`).

### 10d. Card row

- Row height: 22px.
- Column name left-aligned, annotation right-aligned, both on the same line.
- Rows that participate in any edge (FK or PK) get a `char`-colored background tint. This is the only "this is a join column" affordance — no icon, no underline.

### 10e. Top bar

- 52px tall, full-width, 1px `rule` bottom border.
- Left side: brand mark (small owl glyph in `phosphor`), wordmark, vertical separator (1px `rule`), breadcrumb (`schema · target · ERD`) where the target name is in `sentinel-bright`.
- Right side: `[Grid] [Arc]` toggle (small caps, 10px, 0.16em tracking, padded 9px×16px). Default is `Grid`.

### 10f. Legend

Bottom-right of the canvas, fixed (doesn't scroll with content). Two entries: a brighter swatch labeled `TARGET RELATIONS` and a quieter one labeled `GENERATION LINKS`. 10px, 0.16em tracking, Bone.

---

## 11. Interaction (v1)

The panel is read-only. No hover, no click, no zoom, no pan beyond the toggle between Grid and Arc. The user reads it, gets their answer, returns to the SQL editor.

If the layout exceeds the viewport (which it often will, especially with deep parent chains aligned horizontally in grid view, or with the wide canvas of arc view), the canvas scrolls with normal scrollbars. Style scrollbars to match the dark surface (10px wide, `char` thumb on `ink` track).

Switching between Grid and Arc re-runs the layout and re-renders edges. No animated transition — mode change is instant.

---

## 12. Edge cases

### Both views

- **Multi-edge from one parent to one child** (e.g. two FKs): treat each as a separate edge. The Pinned-Perfect / Largest-Gap rule (§7b) handles them independently.
- **Single parent / single child** in a layer: the inner range collapses to a single midpoint anchor (grid §4c.1) or the polar axis (arc §6b).

### Grid view

- **No parents**: collapse the gen-1 and gen-2 (parent-side) bands. Three bands rendered.
- **No children**: same on the other side.
- **No grandparents but yes parents**: collapse only the gen-2 (parent-side) band. Four bands.
- **Many children (>5 or so)**: not yet stress-tested. Current placement assumes children's stacked total height fits within reasonable scroll. Vertical gap can shrink to 24px before things look cramped.

### Arc view

- **No parents**: skip the left hemisphere entirely. The canvas is right-heavy but reads fine.
- **No children**: same on the other side.
- **Many children (5+)**: the angular spread widens until the per-pair `stepAngle` cap (~81°) kicks in. Beyond ~6 items per side at r1=400px, increase r1 (and r2 in proportion) rather than compress the spread.
- **Card positions go negative** (left-arc grandparent extending past x=0, top-arc card past y=0): the normalization pass shifts everything so the leftmost / topmost card sits at >= 80px.

---

## 13. Reference implementation

A working static implementation is in `erd.html` in this directory. It uses:

- HTML/SVG (no framework).
- CSS custom properties for the palette.
- `<svg>` with absolutely-positioned `<line>` and `<path>` elements for the edge routing.
- Absolutely-positioned `<div>` cards inside a `position: absolute` "stage" that's sized to the bounding box of all placed content.

The reference uses sakila.address as the target. The data model and edge list are at the top of the inline `<script>` block (search `const TABLES` and `const EDGES`). Both Grid and Arc are implemented; toggle in the top-right.

For a production implementation in another stack, the key behaviors to port (in order of importance):

1. **§7b (Pinned-Perfect / Largest-Gap)** — applies to BOTH views and is what makes the lines/curves read cleanly. Get this wrong and the diagram looks like every other ERD tool.
2. **§4 (vertical placement to enable horizontal lines)** — the grid layout exists to serve §5.
3. **§5a (sharp orthogonal H-V-H, no rounded corners)** — easy to get wrong by accident in the grid view.
4. **§8 (crow's foot opening toward the entity)** — DBAs read this fluently; reversed crow's feet read as wrong.
5. **§6b (computed angular step for arc placement)** — the formula prevents card overlap regardless of count or card height. A naive fixed spread will silently break for 4+ items.
6. **§6c (Bézier with horizontal-tangent control points)** — this is what makes crow's feet/ticks orient consistently across modes.
7. **§2 (median sweep ordering)** — small payoff for sakila but matters for richer schemas.

---

## 14. Variances in Tyto's implementation

This spec is the design source of truth; the Tyto implementation in `tyto/src/explorer.rs` adopts most of it but diverges where engineering pragmatism or scope concerns made another choice the right call. Captured here so future sessions don't read a discrepancy as a bug.

| Spec section | What Tyto did instead | Why |
|---|---|---|
| §6 Arc view | Not shipped | Grid alone delivered ~80% of the visual win; toggle deferred until there's user demand. |
| §10a Warm palette | Tyto's existing cool dark (`#0d1117` etc.) | Theme migration is its own pass, app-wide; the structural ideas (target-vs-generation edge weights, focal-card accent border) shipped using existing variables. |
| §5a Sharp H-V-H | Cubic Bézier with horizontal-tangent control points | Even on the Grid layout — Béziers degenerate to flat lines when Pinned-Perfect aligns endpoints, but curve gracefully when they don't. The §5a lane-assignment pass is replaced by a per-source-card lane shift on the Bézier control points (different cards in the same band gap take distinct horizontal paths). |
| §2 Median-sweep ordering | Alphabetical within each band | Deferred; the per-source-card lane shift covers the most common crossings on sakila/pagila. |
| §10 All-columns rendering | Lazy-loaded behind a "Keys only" checkbox (default checked) | Default is keys-only because that's the cheap path. Toggling off fires a single bulk-columns introspect query (new plane method `table_columns_bulk_sql`) that hits both planes via a tuple `IN` clause. |
| §11 No interaction (read-only) | Click-to-navigate on non-target card name; cursor-anchored zoom; hover focus mode; generations dropdown | The spec's "the user reads it and returns to SQL" is right for static export but Tyto is interactive — these affordances don't fight that goal, they accelerate it. |

The reference `erd.html` mock is left intact as the original design baseline. When porting any of the deferred ideas (Arc view, median sweep, warm palette), the mock and §6 / §2 / §10a are still authoritative.
