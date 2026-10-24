//! Schema Explorer pane.
//!
//! Owns the object-listing UI (Schemas / Tables / Views / Routines /
//! Triggers / Sequences / Types / Indexes / Admin areas), each
//! object's detail panel (Schema / DDL / ERD / Data / Privileges tabs),
//! and the keyboard cursor model that walks the four nested layers
//! (outer list → tab strip → tab body → cell editor).
//!
//! The ERD-tab layout engine (`render_erd_tab` and the `erd_*`
//! helpers, roughly 1,700 lines) handles card placement across
//! generation bands, edge routing via the Pinned-Perfect /
//! Largest-Gap rule, zoom / hover / drill-in. The
//! Data tab's inline CRUD (UPDATE / INSERT / DELETE / UPSERT) builds
//! its SQL via the plane's DML emitters and applies the
//! single-statement / auto-commit constraint documented in CLAUDE.md.
//!
//! The parsers in this module (`parse_rel_rows`, `parse_erd_columns`,
//! `parse_partition_rows`, `parse_admin_rows`) read `IntrospectResult`
//! rows by *case-insensitive column name*, not position — every plane
//! query is expected to AS-alias its output columns per the contract
//! in `.claude/rules/plane.md`.

use leptos::*;
use std::collections::{BTreeMap, HashMap};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::WebSocket;

use crate::dom::{ws_introspect, ws_send};
use crate::format::*;
use crate::plane::{DbmsPlane, plane_for};
use crate::types::*;
use shared::{ClientMessage, DbType};

pub(crate) fn find_table_detail(ep: &ShellTab, db_dot_table: &str) -> Option<TableDetail> {
    let (schema, name) = db_dot_table.split_once('.')?;
    ep.explorer_table_details
        .with_untracked(|m| m.get(&(schema.to_string(), name.to_string())).copied())
}

/// Parse an introspect result for `relancestors:` / `reldescendants:` into
/// `RelEdge` rows. Looks up `Depth`, `Ref_schema`, `Ref_table` columns
/// case-insensitively, plus the optional `From_columns`, `To_columns`,
/// `Via_schema`, `Via_table` columns the ERD tab needs. Older test SQL
/// that omits the optional columns yields edges with empty Vec / String
/// for those fields; consumers handle that gracefully.
///
/// `From_columns` / `To_columns` are comma-separated lists of column
/// names ordered by FK ordinal. Splits on `,` — column names containing
/// commas are vanishingly rare in practice, so no quoting protocol.
///
/// Reads columns by case-insensitive name. See `.claude/rules/plane.md`
/// for the column-alias contract every plane's relationship SQL must
/// emit (`Depth`, `Ref_schema`, `Ref_table`, `From_columns`,
/// `To_columns`, `Via_schema`, `Via_table`).
pub(crate) fn parse_rel_rows(columns: &[String], rows: Vec<Vec<String>>) -> Vec<RelEdge> {
    let col_idx = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let (Some(di), Some(si), Some(ti)) =
        (col_idx("Depth"), col_idx("Ref_schema"), col_idx("Ref_table"))
    else { return Vec::new() };
    let fci = col_idx("From_columns");
    let tci = col_idx("To_columns");
    let vsi = col_idx("Via_schema");
    let vti = col_idx("Via_table");
    let split_cols = |s: &str| -> Vec<String> {
        if s.is_empty() { Vec::new() }
        else { s.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect() }
    };
    rows.into_iter().filter_map(|r| {
        let depth = r.get(di)?.trim().parse::<u8>().ok()?;
        if depth != 1 && depth != 2 { return None; }
        let schema = r.get(si)?.clone();
        let table  = r.get(ti)?.clone();
        if table.is_empty() { return None; }
        let from_columns = fci.and_then(|i| r.get(i)).map(|s| split_cols(s)).unwrap_or_default();
        let to_columns   = tci.and_then(|i| r.get(i)).map(|s| split_cols(s)).unwrap_or_default();
        let via_schema   = vsi.and_then(|i| r.get(i)).cloned().unwrap_or_default();
        let via_table    = vti.and_then(|i| r.get(i)).cloned().unwrap_or_default();
        Some(RelEdge { depth, schema, table, from_columns, to_columns, via_schema, via_table })
    }).collect()
}

/// Parse an introspect result for `admin_roles:` into AdminRow values.
/// Positional: column 0 is the role name, columns 1+ become extras in
/// the same order as `plane.role_list_extra_columns()`. Empty names are
/// dropped (defensive — system tables sometimes have NULL rows).
pub(crate) fn parse_admin_rows(_columns: &[String], rows: Vec<Vec<String>>) -> Vec<AdminRow> {
    rows.into_iter().filter_map(|r| {
        let name = r.first()?.clone();
        if name.is_empty() { return None; }
        let extras: Vec<String> = r.iter().skip(1).cloned().collect();
        Some(AdminRow { name, extras })
    }).collect()
}

/// Parse an introspect result for `erdcols:` into a (schema, table) →
/// ordered column list lookup. Each input row is one column of one table;
/// rows arrive ordered by (schema, table, ordinal) so the parser groups
/// them in stream order. `Is_pk` is parsed as a u8 (0/1) — anything not
/// equal to "0" is treated as PK.
///
/// Reads columns by case-insensitive name. See `.claude/rules/plane.md`
/// for the column-alias contract every plane's bulk-ERD-columns SQL
/// must emit (`Schema_`, `Table_`, `Field`, `Ord`, `Is_pk`).
pub(crate) fn parse_erd_columns(
    columns: &[String],
    rows:    Vec<Vec<String>>,
) -> HashMap<(String, String), Vec<(String, bool)>> {
    let col_idx = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let (Some(si), Some(ti), Some(fi)) = (
        col_idx("Schema_"),
        col_idx("Table_"),
        col_idx("Field"),
    ) else { return HashMap::new() };
    let pi = col_idx("Is_pk");
    let mut out: HashMap<(String, String), Vec<(String, bool)>> = HashMap::new();
    for r in rows {
        let Some(s) = r.get(si).cloned() else { continue };
        let Some(t) = r.get(ti).cloned() else { continue };
        let Some(field) = r.get(fi).cloned() else { continue };
        if field.is_empty() { continue; }
        let is_pk = pi.and_then(|i| r.get(i))
                      .map(|v| v.trim() != "0" && !v.trim().is_empty())
                      .unwrap_or(false);
        out.entry((s, t)).or_default().push((field, is_pk));
    }
    out
}

/// Parse an introspect result for `partitions:` into `PartitionInfo` rows.
/// Aliases: `Partition_schema`, `Partition_name`, `Partition_bound`, plus
/// optional `Partition_rows` and `Partition_data_length` (older/test SQL
/// may omit them — empty strings result). Drops rows lacking a name.
/// Parse the plane's `object_grants_sql` rowset into ObjectGrantRow.
/// Column lookup is case-insensitive by name (the standard tyto
/// parser convention) — order in the SQL doesn't matter.
pub(crate) fn parse_object_grant_rows(
    columns: &[String],
    rows:    Vec<Vec<String>>,
) -> Vec<ObjectGrantRow> {
    let col_idx = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let (Some(gi), Some(pi), Some(si)) =
        (col_idx("Grantee"), col_idx("Privilege"), col_idx("Source"))
    else { return Vec::new() };
    let ci   = col_idx("Chain");
    let gri  = col_idx("Grantable");
    let rci  = col_idx("RoleCategory");
    rows.into_iter().filter_map(|r| {
        let grantee   = r.get(gi)?.clone();
        let privilege = r.get(pi)?.clone();
        let source    = r.get(si)?.clone();
        if grantee.is_empty() || privilege.is_empty() { return None; }
        let chain     = ci.and_then(|i| r.get(i)).cloned().unwrap_or_default();
        let grantable = gri.and_then(|i| r.get(i)).cloned().unwrap_or_default();
        // Fall back to `source` when the plane doesn't emit
        // RoleCategory (e.g. older MySQL plane that hasn't been
        // upgraded yet). Keeps the renderer working regardless.
        let role_category = rci.and_then(|i| r.get(i)).cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| source.clone());
        Some(ObjectGrantRow { grantee, privilege, source, chain, grantable, role_category })
    }).collect()
}

/// Reads columns by case-insensitive name. See `.claude/rules/plane.md`
/// for the column-alias contract every plane's partitions SQL must
/// emit (`Partition_schema`, `Partition_name`, `Partition_bound`,
/// optional `Partition_rows`, `Partition_data_length`).
/// Parse the `toast:*` introspect payload into an `Option<ToastDetail>`.
/// The plane emits one summary row (Row_kind="summary") + N optional
/// column rows (Row_kind="column"). Kind="none" → `None`, otherwise
/// dispatch into the appropriate `ToastKind` variant. Sizes come back
/// as raw byte counts; format them here so the render helpers can just
/// display the strings.
pub(crate) fn parse_toast_rows(columns: &[String], rows: Vec<Vec<String>>) -> Option<ToastDetail> {
    let col_idx = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let get = |row: &Vec<String>, ix: Option<usize>| -> String {
        ix.and_then(|i| row.get(i)).cloned().unwrap_or_default()
    };
    let row_kind_i = col_idx("Row_kind")?;

    // Summary row is first; column rows follow (SQL uses UNION ALL with
    // summary before cols). Walk once so both parses share iteration.
    let mut kind: Option<ToastKind> = None;
    let mut toast_columns: Vec<ToastColumn> = Vec::new();

    let kind_i          = col_idx("Kind");
    let ts_schema_i     = col_idx("Toast_schema");
    let ts_name_i       = col_idx("Toast_name");
    let heap_bytes_i    = col_idx("Heap_bytes");
    let index_bytes_i   = col_idx("Index_bytes");
    let rows_est_i      = col_idx("Rows_est");
    let storage_ratio_i = col_idx("Storage_ratio");
    let part_count_i    = col_idx("Partition_count");
    let total_toast_i   = col_idx("Total_toast_bytes");
    let largest_schema_i = col_idx("Largest_schema");
    let largest_name_i   = col_idx("Largest_name");
    let largest_bytes_i  = col_idx("Largest_bytes");
    let parent_schema_i  = col_idx("Parent_schema");
    let parent_name_i    = col_idx("Parent_name");
    let column_name_i    = col_idx("Column_name");
    let attstorage_i     = col_idx("Attstorage");

    for row in &rows {
        match row.get(row_kind_i).map(String::as_str).unwrap_or("") {
            "summary" => {
                let kind_str = get(row, kind_i);
                kind = match kind_str.as_str() {
                    "single" => Some(ToastKind::Single {
                        toast_schema:  get(row, ts_schema_i),
                        toast_name:    get(row, ts_name_i),
                        heap_size:     human_bytes(&get(row, heap_bytes_i)),
                        index_size:    human_bytes(&get(row, index_bytes_i)),
                        rows_est:      {
                            let raw = get(row, rows_est_i);
                            // reltuples can be -1 (never analyzed); pass
                            // through the raw text so the DBA sees the
                            // sentinel — same convention as the Overview.
                            raw.parse::<i64>()
                                .map(format_int_condensed)
                                .unwrap_or(raw)
                        },
                        storage_ratio: {
                            let raw = get(row, storage_ratio_i);
                            if raw.is_empty() { String::new() } else { format!("{}%", raw) }
                        },
                    }),
                    "partition_aggregate" => Some(ToastKind::PartitionAggregate {
                        partition_count: get(row, part_count_i).parse().unwrap_or(0),
                        total_toast:     human_bytes(&get(row, total_toast_i)),
                        largest_schema:  get(row, largest_schema_i),
                        largest_name:    get(row, largest_name_i),
                        largest_size:    human_bytes(&get(row, largest_bytes_i)),
                    }),
                    "is_toast_of" => Some(ToastKind::IsToastOf {
                        parent_schema: get(row, parent_schema_i),
                        parent_name:   get(row, parent_name_i),
                    }),
                    // "none" or anything unrecognised → whole section hides.
                    _ => return None,
                };
            }
            "column" => {
                let name    = get(row, column_name_i);
                let storage = get(row, attstorage_i);
                if !name.is_empty() && !storage.is_empty() {
                    toast_columns.push(ToastColumn { name, storage });
                }
            }
            _ => {}
        }
    }

    kind.map(|k| ToastDetail { kind: k, columns: toast_columns })
}

/// Parse a `vm:*` introspect result into an [`Option<VmSummary>`].
/// The plane always emits exactly one row with four columns: Source,
/// Total_pages, All_visible_pages, All_frozen_pages. `Source = "none"`
/// (no VM data path available on this server) → `None` (renderer
/// suppresses the whole section). Numeric fields that arrive empty
/// (e.g. All_frozen_pages on the pg_stat fallback path) parse to
/// `None` inside the struct so the render can hide those cells
/// individually.
pub(crate) fn parse_vm_rows(columns: &[String], rows: Vec<Vec<String>>) -> Option<VmSummary> {
    let col_idx = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let get = |row: &Vec<String>, ix: Option<usize>| -> String {
        ix.and_then(|i| row.get(i)).cloned().unwrap_or_default()
    };
    let parse_pages = |raw: String| -> Option<u64> {
        let t = raw.trim();
        if t.is_empty() { None } else { t.parse::<u64>().ok() }
    };
    let row = rows.first()?;
    let source = get(row, col_idx("Source"));
    if source.is_empty() || source == "none" {
        return None;
    }
    let partitions = (get(row, col_idx("Relkind")) == "p")
        .then(|| parse_pages(get(row, col_idx("Rel_count"))))
        .flatten();
    Some(VmSummary {
        source,
        total_pages:       parse_pages(get(row, col_idx("Total_pages"))),
        all_visible_pages: parse_pages(get(row, col_idx("All_visible_pages"))),
        all_frozen_pages:  parse_pages(get(row, col_idx("All_frozen_pages"))),
        partitions,
        live_denied: false,
    })
}

/// Parse the `autovac:*` payload: `Row_kind = 'counters'` carries the
/// relation-level counters, every other row is one knob. No knob rows
/// (relation not found, non-PG connection) → `None` so the render
/// suppresses the section.
pub(crate) fn parse_autovac_rows(columns: &[String], rows: Vec<Vec<String>>) -> Option<AutovacSettings> {
    let col_idx = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let text = |row: &[String], name: &str| -> Option<String> {
        col_idx(name).and_then(|i| row.get(i)).filter(|s| !s.is_empty()).cloned()
    };
    let int = |row: &[String], name: &str| -> Option<i64> {
        text(row, name).and_then(|s| s.trim().parse().ok())
    };

    let mut entries: Vec<AutovacEntry> = Vec::new();
    let mut counters = AutovacCounters::default();
    for row in &rows {
        if text(row, "Row_kind").as_deref() == Some("counters") {
            counters = AutovacCounters {
                relkind:             text(row, "Relkind").unwrap_or_default(),
                reltuples:           int(row, "Reltuples"),
                relpages:            int(row, "Relpages"),
                relallfrozen:        int(row, "Relallfrozen"),
                n_dead_tup:          int(row, "N_dead_tup"),
                n_ins_since_vacuum:  int(row, "N_ins_since_vacuum"),
                n_mod_since_analyze: int(row, "N_mod_since_analyze"),
                xid_age:             int(row, "Xid_age"),
                mxid_age:            int(row, "Mxid_age"),
            };
            continue;
        }
        let Some(setting) = text(row, "Setting") else { continue };
        let source = match text(row, "Source").as_deref() {
            Some("reloption") => AutovacSource::Reloption,
            Some("cluster")   => AutovacSource::Cluster,
            _                 => AutovacSource::Default,
        };
        entries.push(AutovacEntry {
            setting,
            effective_value: text(row, "Effective_value").unwrap_or_default(),
            source,
            reloption_value: text(row, "Reloption_value"),
            cluster_value:   text(row, "Cluster_value"),
            fallback_value:  text(row, "Fallback_value"),
            unit:            text(row, "Unit"),
        });
    }

    (!entries.is_empty()).then_some(AutovacSettings { entries, counters })
}

pub(crate) fn parse_partition_rows(columns: &[String], rows: Vec<Vec<String>>) -> Vec<PartitionInfo> {
    let col_idx = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let (Some(si), Some(ni), Some(bi)) =
        (col_idx("Partition_schema"), col_idx("Partition_name"), col_idx("Partition_bound"))
    else { return Vec::new() };
    let ri = col_idx("Partition_rows");
    let di = col_idx("Partition_data_length");
    rows.into_iter().filter_map(|r| {
        let schema = r.get(si)?.clone();
        let name   = r.get(ni)?.clone();
        let bound  = r.get(bi)?.clone();
        if name.is_empty() { return None; }
        let rows_raw = ri.and_then(|i| r.get(i)).cloned().unwrap_or_default();
        let data_raw = di.and_then(|i| r.get(i)).cloned().unwrap_or_default();
        let rows      = if rows_raw.is_empty() { String::new() } else { format_int_with_commas(&rows_raw) };
        let data_size = if data_raw.is_empty() { String::new() } else { human_bytes(&data_raw) };
        Some(PartitionInfo { schema, name, bound, rows, data_size })
    }).collect()
}

/// The explorer pane's session: its id (`None` until SessionOpened)
/// and the extensions installed on that server. Both are Copy signals
/// on the pane's `ShellTab`, bundled so helpers that fire introspect
/// queries read them from their arguments. That matters for the ones
/// called from click handlers, which run outside any reactive owner:
/// a Leptos context lookup returns `None` there.
#[derive(Clone, Copy)]
pub(crate) struct ExplorerSession {
    pub(crate) id:         RwSignal<Option<String>>,
    pub(crate) extensions: RwSignal<Vec<String>>,
    /// `pg_visibility_map_summary()` refused to this session's role.
    pub(crate) vm_live_denied: RwSignal<bool>,
}

impl ExplorerSession {
    pub(crate) fn of(tab: &ShellTab) -> Self {
        ExplorerSession { id: tab.session_id, extensions: tab.session_extensions, vm_live_denied: tab.vm_live_denied }
    }

    /// The extensions the explorer's SQL may use: those installed, but
    /// `pg_visibility` only until its function is refused to the role.
    fn usable_extensions(self) -> Vec<String> {
        let denied = self.vm_live_denied.get_untracked();
        self.extensions.get_untracked().into_iter().filter(|e| !(denied && e == "pg_visibility")).collect()
    }
}

/// A table's visibility-map read and its tag: `vmlive:schema.table` when
/// it's offered `pg_visibility` (so a refusal is known to be that
/// function's, however many reads are in flight), `vm:schema.table` when
/// it reads `pg_class`.
fn vm_query(plane: &'static dyn DbmsPlane, schema: &str, name: &str, session: ExplorerSession) -> (String, String) {
    let extensions = session.usable_extensions();
    let live = extensions.iter().any(|e| e == "pg_visibility");
    (plane.table_vm_sql(schema, name, &extensions), format!("{}:{schema}.{name}", if live { "vmlive" } else { "vm" }))
}

/// Whether a failed visibility-map read should be made again from
/// `pg_class`: it was offered `pg_visibility` (`live`, its `vmlive:` tag)
/// and the server refused its function to the role
/// (`insufficient_privilege`, 42501). Privileges are checked when a query
/// starts, for every function in it, so no single query can try one path
/// and take the other: the explorer reads again. A `pg_class` read's
/// failure is shown, so this can't loop.
pub(crate) fn vm_read_refused(error: Option<&shared::DbError>, live: bool) -> bool {
    live && error.and_then(|e| e.sqlstate.as_deref()) == Some("42501")
}

/// Read a table's visibility map again (`schema.table`), with the
/// session's usable extensions — after `pg_visibility` was refused.
/// False when it can't be sent (no session), for the caller to show the
/// error instead.
pub(crate) fn refire_vm(
    plane:   &'static dyn DbmsPlane,
    schema_dot_table: &str,
    ws:      RwSignal<Option<WebSocket>>,
    session: ExplorerSession,
) -> bool {
    let (Some(sid), Some((schema, name))) = (session.id.get_untracked(), schema_dot_table.split_once('.')) else { return false };
    let (sql, tag) = vm_query(plane, schema, name, session);
    ws_introspect(ws, &sid, sql, tag);
    true
}

/// Fire the ten introspect queries behind a table's detail view. Shared
/// by the first load and by ↻ so the two can't drift apart.
fn fire_table_introspects(
    plane:   &'static dyn DbmsPlane,
    schema:  &str,
    name:    &str,
    ws:      RwSignal<Option<WebSocket>>,
    session: ExplorerSession,
) {
    let Some(sid) = session.id.get_untracked() else { return };
    let q = format!("{}.{}", schema, name);
    let queries: [(String, &str); 9] = [
        (plane.table_status_sql(schema, name),                   "status"),
        (plane.table_columns_sql(schema, name),                  "columns"),
        (plane.table_indexes_sql(schema, name),                  "indexes"),
        (plane.table_ddl_sql(schema, name),                      "ddl"),
        (plane.table_relationship_ancestors_sql(schema, name),   "relancestors"),
        (plane.table_relationship_descendants_sql(schema, name), "reldescendants"),
        (plane.table_partitions_sql(schema, name),               "partitions"),
        (plane.table_toast_sql(schema, name),                    "toast"),
        (plane.table_autovac_sql(schema, name),                  "autovac"),
    ];
    for (sql, tag) in queries {
        ws_introspect(ws, &sid, sql, format!("{tag}:{q}"));
    }
    let (sql, tag) = vm_query(plane, schema, name, session);
    ws_introspect(ws, &sid, sql, tag);
}

pub(crate) fn ensure_table_detail_loaded(
    plane:      &'static dyn DbmsPlane,
    schema:     &str,
    name:       &str,
    cache:      RwSignal<HashMap<(String, String), TableDetail>>,
    ws:         RwSignal<Option<WebSocket>>,
    session:    ExplorerSession,
) -> TableDetail {
    let key = (schema.to_string(), name.to_string());
    if let Some(d) = cache.with_untracked(|m| m.get(&key).copied()) {
        if d.stale.get_untracked() {
            d.stale.set(false);
            d.refreshing.set(true);
            fire_table_introspects(plane, schema, name, ws, session);
        }
        return d;
    }
    let detail = TableDetail::new();
    cache.update(|m| { m.insert(key.clone(), detail); });
    fire_table_introspects(plane, schema, name, ws, session);
    detail
}

/// Refresh an already-cached `TableDetail` in place: re-fire the
/// introspect queries into the same signals. Current values stay on
/// screen until each reply replaces them (every handler `set`s, none
/// appends), so ↻ doesn't blank the tab or reset its scroll; the ↻ icon
/// spins while `refreshing` is set.
///
/// The `TableDetail` instance and its signal identities are preserved
/// on purpose. Dropping the cached entry and inserting a fresh one
/// would swap in new `RwSignal`s the mounted view isn't subscribed to,
/// so replies would land on orphaned signals (PR #100). UI state
/// (active tab, ERD zoom, …) is untouched; the Data tab has its own ↻.
pub(crate) fn refresh_table_detail(
    plane:      &'static dyn DbmsPlane,
    schema:     &str,
    name:       &str,
    detail:     TableDetail,
    ws:         RwSignal<Option<WebSocket>>,
    session:    ExplorerSession,
) {
    detail.stale.set(false);
    detail.refreshing.set(true);
    fire_table_introspects(plane, schema, name, ws, session);
}

pub(crate) fn ensure_view_detail_loaded(
    plane:  &'static dyn DbmsPlane,
    schema: &str,
    name:   &str,
    cache:  RwSignal<HashMap<(String, String), TableDetail>>,
    ws:     RwSignal<Option<WebSocket>>,
    sid:    Option<String>,
) -> TableDetail {
    let key = (schema.to_string(), name.to_string());
    if let Some(d) = cache.with_untracked(|m| m.get(&key).copied()) {
        return d;
    }
    let detail = TableDetail::new();
    detail.overview.set(LoadState::Loaded(TableOverview::default()));
    detail.indexes.set(LoadState::Loaded(vec![]));
    // Views don't have FK relationships in the same sense; mark loaded-empty
    // so the Relationships section renders dashes rather than "Loading…".
    detail.ancestors.set(LoadState::Loaded(vec![]));
    detail.descendants.set(LoadState::Loaded(vec![]));
    detail.partitions.set(LoadState::Loaded(vec![]));
    cache.update(|m| { m.insert(key.clone(), detail); });
    if let Some(sid) = sid {
        let q = format!("{}.{}", schema, name);
        ws_introspect(ws, &sid, plane.view_columns_sql(schema, name), format!("columns:{}", q));
        ws_introspect(ws, &sid, plane.object_ddl_sql(ObjectKind::Views, schema, name), format!("ddl:{}", q));
    }
    detail
}

pub(crate) fn ensure_object_ddl_loaded(
    plane:  &'static dyn DbmsPlane,
    kind:   ObjectKind,
    schema: &str,
    name:   &str,
    cache:  RwSignal<HashMap<(ObjectKind, String, String), LoadState<String>>>,
    ws:     RwSignal<Option<WebSocket>>,
    sid:    Option<String>,
) {
    let key = (kind, schema.to_string(), name.to_string());
    if cache.with_untracked(|m| m.contains_key(&key)) { return; }
    cache.update(|m| { m.insert(key.clone(), LoadState::Loading); });
    if let Some(sid) = sid {
        ws_introspect(ws, &sid,
            plane.object_ddl_sql(kind, schema, name),
            format!("objddl:{}:{}.{}", kind.label(), schema, name));
    }
}

// ── Explorer pane renderer (app-style) ────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_explorer_pane<F, S, T, E>(
    pane_id: String,
    tab: ShellTab,
    active_pane_id: RwSignal<Option<String>>,
    ws: RwSignal<Option<WebSocket>>,
    pane_ops: PaneOps,
    connections: RwSignal<Vec<shared::ConnectionDefPublic>>,
    shell_panes: RwSignal<HashMap<String, ShellTab>>,
    _close_pane: F,
    _do_split: S,
    _add_tab: T,
    _add_pane: E,
) -> impl IntoView
where
    F: Fn(String) + Copy + 'static,
    S: Fn(String, SplitDir) + Copy + 'static,
    T: Fn(String) + Copy + 'static,
    E: Fn(String, PaneKind, Option<ExplorerInit>) + Copy + 'static,
{
    let session       = ExplorerSession::of(&tab);
    let explorer_pane_id = pane_id.clone();
    let db_conn_id    = tab.db_connection_id;
    let schemas       = tab.explorer_schemas;
    let schema        = tab.explorer_schema;
    let kind          = tab.explorer_kind;
    let search        = tab.explorer_search;
    let all_schemas   = tab.explorer_all_schemas;
    let listing       = tab.explorer_listing;
    let selected      = tab.explorer_selected;
    let cursor_key    = tab.explorer_cursor_key;
    let table_details = tab.explorer_table_details;
    let object_ddls   = tab.explorer_object_ddls;
    let admin_active  = tab.explorer_admin_active;
    let admin_area    = tab.explorer_admin_area;
    let admin_roles    = tab.explorer_admin_roles;
    let admin_selected = tab.explorer_admin_selected;
    let admin_role_tab = tab.explorer_admin_role_tab;
    let admin_role_privs = tab.explorer_admin_role_privileges;
    let admin_role_privs_view = tab.explorer_admin_role_privs_view;
    let admin_role_privs_selected = tab.explorer_admin_role_privs_selected;
    let admin_role_privs_auto_selected = tab.explorer_admin_role_privs_auto_selected;
    let admin_role_affected_objects = tab.explorer_admin_role_affected_objects;
    let admin_role_privs_kinds = tab.explorer_admin_role_privs_kinds;
    let admin_role_affected_kinds = tab.explorer_admin_role_affected_kinds;
    let admin_area_data = tab.explorer_admin_area_data;
    let admin_row_selected = tab.explorer_admin_row_selected;
    let admin_row_detail   = tab.explorer_admin_row_detail;
    let selections_by_kind = tab.explorer_selections_by_kind;
    let sort_by_kind       = tab.explorer_sort_by_kind;
    let expanded_parents   = tab.explorer_expanded_parents;
    let partition_menu     = tab.explorer_partition_menu;

    // Switching admin area clears both selections — a role from one
    // area doesn't make sense in another, and a tablespace selection
    // shouldn't carry into the Databases area. Uses the prev-value
    // param so the initial mount doesn't fire a needless pulse.
    create_effect(move |prev: Option<Option<AdminArea>>| {
        let cur = admin_area.get();
        if let Some(prev) = prev {
            if prev != cur {
                admin_selected.set(None);
                admin_row_selected.set(None);
            }
        }
        cur
    });

    // Switching admin row clears the detail-section cache so the next
    // row's sections fetch fresh. Also clears the editable-matrix
    // signals (cell status flashes + pending-revoke banner) so they
    // don't leak across schemas — the keys (grantee, priv) overlap
    // between rows and a stale entry would paint the wrong cell.
    // Same prev-value guard as above.
    let priv_cell_status_for_admin_reset    = tab.explorer_priv_cell_status;
    let priv_pending_revoke_for_admin_reset = tab.explorer_priv_pending_revoke;
    create_effect(move |prev: Option<Option<(AdminArea, String)>>| {
        let cur = admin_row_selected.get();
        if let Some(prev) = prev {
            if prev != cur {
                admin_row_detail.update(|m| m.clear());
                priv_cell_status_for_admin_reset.update(|m| m.clear());
                priv_pending_revoke_for_admin_reset.set(None);
            }
        }
        cur
    });

    // Same reset on object-detail context change. The object
    // matrix shares the same signals; switching from `public.film`
    // to `public.actor` shouldn't carry banner / flash state.
    let priv_cell_status_for_obj_reset    = tab.explorer_priv_cell_status;
    let priv_pending_revoke_for_obj_reset = tab.explorer_priv_pending_revoke;
    let obj_selected_for_reset = tab.explorer_selected;
    create_effect(move |prev: Option<Option<(String, String)>>| {
        let cur = obj_selected_for_reset.get();
        if let Some(prev) = prev {
            if prev != cur {
                priv_cell_status_for_obj_reset.update(|m| m.clear());
                priv_pending_revoke_for_obj_reset.set(None);
            }
        }
        cur
    });

    // Lazy-fetch each detail section when it's both selected and not
    // already loaded. The cache lookup is tracked via `.with()` so
    // post-GRANT/REVOKE resets to NotLoaded (from main.rs's
    // DmlResult handler) re-fire this effect and refetch the
    // section. The early-return on non-NotLoaded (which fires when
    // this effect's own write flips status to Loading) prevents a
    // re-entry loop.
    create_effect(move |_| {
        if !admin_active.get() { return; }
        let Some((area, row_id)) = admin_row_selected.get() else { return };
        let sections = plane_for(tab.db_type).area_row_detail_sections(area);
        if sections.is_empty() { return; }
        let Some(sid) = session.id.get_untracked() else { return; };
        for section in sections {
            let key = (area, row_id.clone(), section.to_string());
            let need = admin_row_detail.with(|m| {
                matches!(m.get(&key), None | Some(LoadState::NotLoaded))
            });
            if !need { continue; }
            let Some(sql) = plane_for(tab.db_type).area_row_detail_sql(area, &row_id, section)
                else { continue; };
            admin_row_detail.update(|m| { m.insert(key, LoadState::Loading); });
            let tag = format!(
                "admin_row_detail:{}:{}:{}",
                crate::admin_area_tag(area), row_id, section
            );
            ws_introspect(ws, &sid, sql, tag);
        }
    });

    // Switching admin row resets the detail tab AND the privileges cache
    // so the next role's tabs fetch fresh data and start on General. Same
    // prev-value guard as above to skip the initial pulse.
    create_effect(move |prev: Option<Option<String>>| {
        let cur = admin_selected.get().map(|r| r.name.clone());
        if let Some(prev) = prev {
            if prev != cur {
                admin_role_tab.set(RoleDetailTab::General);
                admin_role_privs.set(LoadState::NotLoaded);
                // Reset Privileges sub-view + selection + resolver
                // cache so the next role lands cleanly. The auto-
                // selected latch resets here too so the new role's
                // first privs-load fires the select-all.
                admin_role_privs_view.set(RolePrivsView::Grants);
                admin_role_privs_selected.update(|s| s.clear());
                admin_role_affected_objects.set(LoadState::NotLoaded);
                admin_role_privs_auto_selected.set(false);
                admin_role_privs_kinds.set(None);
                admin_role_affected_kinds.set(None);
            }
        }
        cur
    });

    // When privileges first load for a role, pre-select every row —
    // the user usually wants "Show affected objects" to cover the
    // whole role; deselecting to narrow is the less-common path.
    // Gated by the `_auto_selected` latch: the role-switch reset
    // flips it false, this effect flips it true on first load,
    // subsequent loads for the same role (DDL invalidation) leave
    // the user's selection alone.
    //
    // Note: this selects every priv-row index up front, including
    // rows that may be hidden by a kind chip the user later
    // toggles. That's intentional — the selection is keyed by row
    // index, not by visible kind. If the user hides 'view', then
    // deselects a couple of remaining 'table' rows, then re-shows
    // 'view', the view rows re-appear pre-checked. The mental
    // model is "chips filter the display; row checkboxes filter
    // within the visible set." Re-checking a chip restores rows
    // to whatever state they were left in.
    create_effect(move |_| {
        if admin_role_privs_auto_selected.get_untracked() { return; }
        if let LoadState::Loaded(rows) = admin_role_privs.get() {
            let total = rows.len();
            admin_role_privs_selected.set((0..total).collect());
            admin_role_privs_auto_selected.set(true);
        }
    });

    // Fetch the resolved affected-objects list whenever the inputs
    // change. We subscribe to every signal that affects the query:
    // tab state, role selection, the loaded privilege rows, and
    // the user's row-selection. admin_role_affected_objects itself
    // is *written* but not read, so the Loading → Loaded writes
    // can't trigger a re-fire loop. Each input change fires a
    // fresh query; if two fire in rapid succession the later
    // response wins.
    let db_type_for_aff = tab.db_type;
    create_effect(move |_| {
        if !admin_active.get() { return; }
        if admin_area.get() != Some(AdminArea::Roles) { return; }
        if admin_role_tab.get() != RoleDetailTab::Privileges { return; }
        let Some(row) = admin_selected.get() else { return };
        let LoadState::Loaded(priv_rows) = admin_role_privs.get() else { return };
        let selected_idxs = admin_role_privs_selected.get();
        if selected_idxs.is_empty() {
            admin_role_affected_objects.set(LoadState::Loaded(Vec::new()));
            return;
        }
        // Object qualifier is the first column of each priv row.
        // Special case: PG's "Owner (database)" source rows have
        // their Object column set to a bare database name. Treating
        // that as a schema (the resolver's fallback) gives zero
        // results when the connecting DB doesn't have a schema by
        // that name. Map those rows to `*.*` so they expand to the
        // full visible table universe — the closest analog to
        // "owns the whole database." "Owner (schema)" rows are
        // intentionally NOT remapped: their Object column is a bare
        // schema name and the resolver's bare-name fallback already
        // treats it as a schema match, which is exactly right.
        let priv_cols = plane_for(db_type_for_aff).role_privileges_columns();
        let object_col = priv_cols.iter().position(|c| *c == "Object").unwrap_or(0);
        let source_col = priv_cols.iter().position(|c| *c == "Source");
        let kind_col = priv_cols.iter().position(|c| *c == "Kind");
        // Resolver scope = checked rows whose Kind passes the
        // chip filter. A row whose checkbox is set but whose Kind
        // chip is unchecked is invisible to the user — feeding it
        // into the resolver would inflate the count past what the
        // user can see.
        let kind_filter = admin_role_privs_kinds.get();
        let row_visible = |r: &Vec<String>| -> bool {
            match (&kind_filter, kind_col) {
                (Some(set), Some(kc)) => r.get(kc).map(|k| set.contains(k)).unwrap_or(false),
                _ => true,
            }
        };
        let mut selected_objects: Vec<String> = selected_idxs.iter()
            .filter_map(|i| {
                let r = priv_rows.get(*i)?;
                if !row_visible(r) { return None; }
                let obj = r.get(object_col)?.clone();
                let src = source_col.and_then(|si| r.get(si)).cloned().unwrap_or_default();
                if src.starts_with("Owner (database)") {
                    Some("*.*".to_string())
                } else {
                    Some(obj)
                }
            })
            .collect();
        // Dedup. A role with N global grants would otherwise build
        // a WHERE clause of N copies of TRUE — still correct, but
        // pointless SQL bloat.
        selected_objects.sort();
        selected_objects.dedup();
        let Some(sql) = plane_for(db_type_for_aff)
            .role_affected_objects_sql(&row.name, &row.extras, &selected_objects)
        else { return; };
        let Some(sid) = session.id.get_untracked() else { return; };
        admin_role_affected_objects.set(LoadState::Loading);
        ws_introspect(ws, &sid, sql, "admin_role_affected_objects".into());
    });


    // Lazy-fetch the Privileges tab the first time the user opens it for
    // a given role. Reset back to NotLoaded happens via either DDL
    // invalidation (main.rs) or the selected-role-change effect above.
    create_effect(move |_| {
        if !admin_active.get() { return; }
        if admin_area.get() != Some(AdminArea::Roles) { return; }
        if admin_role_tab.get() != RoleDetailTab::Privileges { return; }
        let Some(row) = admin_selected.get() else { return };
        if !matches!(admin_role_privs.get_untracked(), LoadState::NotLoaded) { return; }
        let Some(sql) = plane_for(tab.db_type).role_privileges_sql(&row.name, &row.extras) else { return; };
        let Some(sid) = session.id.get_untracked() else { return; };
        admin_role_privs.set(LoadState::Loading);
        ws_introspect(ws, &sid, sql, "admin_role_privs".into());
    });


    // Lazy-fetch any non-Roles area (Tablespaces / Databases / Extensions
    // / ServerConfig) the first time the user opens it. The HashMap-keyed
    // cache lets each area load independently. get_untracked on the
    // entry's LoadState avoids a self-subscription loop on Loading→Loaded.
    create_effect(move |_| {
        if !admin_active.get() { return; }
        let Some(area) = admin_area.get() else { return };
        if area == AdminArea::Roles { return; }
        let already = admin_area_data.with_untracked(|m| {
            !matches!(m.get(&area), None | Some(LoadState::NotLoaded))
        });
        if already { return; }
        let Some(sql) = plane_for(tab.db_type).area_list_sql(area) else { return; };
        let Some(sid) = session.id.get_untracked() else { return; };
        admin_area_data.update(|m| { m.insert(area, LoadState::Loading); });
        let tag = format!("admin_area:{}", crate::admin_area_tag(area));
        ws_introspect(ws, &sid, sql, tag);
    });

    // Mirror effect: every non-None `selected` change records the
    // selection under the current kind, so a future kind switch can
    // restore it. None values do NOT update the map — clearing a
    // selection (e.g. user toggles schemas) keeps the old "last viewed"
    // entry alive so a return trip can re-pick it up.
    create_effect(move |_| {
        if let Some(sel) = selected.get() {
            let cur_kind = kind.get_untracked();
            selections_by_kind.update(|m| { m.insert(cur_kind, sel); });
        }
    });

    let plane: &'static dyn DbmsPlane = plane_for(tab.db_type);

    // Whether the selected table's details are cached yet — an explorer
    // opened by `\d <name>` gets them when its session opens (main.rs),
    // after it first renders. The inspector
    // reads it so a selection made before its details existed re-renders
    // when they do; a memo, so other tables entering the cache don't
    // re-render the inspector.
    let selected_detail_cached = create_memo(move |_| {
        selected.get().is_some_and(|key| table_details.with(|m| m.contains_key(&key)))
    });

    // Lazy-fetch the Roles list the first time the user enters the Roles
    // admin area in this pane. Re-fires only on transition from NotLoaded;
    // Phase C will add DDL invalidation that resets back to NotLoaded.
    create_effect(move |_| {
        if !admin_active.get() { return; }
        let Some(area) = admin_area.get() else { return };
        if area != AdminArea::Roles { return; }
        // get_untracked: avoid self-subscribing to admin_roles, which would
        // re-queue this effect on the Loading→Loaded transition.
        if !matches!(admin_roles.get_untracked(), LoadState::NotLoaded) { return; }
        let Some(sql) = plane.list_roles_sql() else { return; };
        let Some(sid) = session.id.get_untracked() else { return; };
        admin_roles.set(LoadState::Loading);
        ws_introspect(ws, &sid, sql.to_string(), "admin_roles".into());
    });

    let _menu_open     = tab.pane_menu_open;

    let _pid_close   = pane_id.clone();
    let _pid_split_h = pane_id.clone();
    let _pid_split_v = pane_id.clone();
    let _pid_add_tab = pane_id.clone();
    let _pid_menu    = pane_id.clone();
    let pid_focus   = pane_id.clone();
    let pid_for_id  = pane_id.clone();
    let detail_expanded = tab.explorer_detail_expanded;
    let detail_visible  = tab.explorer_detail_visible;

    let last_fetch = tab.explorer_last_fetch;
    create_effect(move |_| {
        let cur_kind = kind.get();
        let cur_all  = all_schemas.get();
        let cur_schema = schema.get();
        let current = (cur_kind, cur_all, cur_schema.clone());
        let prev = last_fetch.get_untracked();
        if prev.as_ref() == Some(&current) { return; }
        if !cur_all && cur_schema.is_none() { return; }
        // Tracked: an explorer opened with its schema already chosen
        // (`\d <name>`) has nothing else to re-run this once the session
        // opens — the schema-list reply only fills a schema that's unset.
        let Some(sid) = session.id.get() else { return; };
        // Schema or all-schemas-toggle change: clear `selected` because the
        // previous selection's namespace is no longer in the listing. Pure
        // kind change (only prev.0 differs) leaves `selected` alone — the
        // typebar click handler already restored a per-kind value before
        // this effect fired, and clearing here would clobber that restore.
        let schema_or_all_changed = match prev.as_ref() {
            None    => false,  // first fetch — selected is already None
            Some(p) => p.1 != cur_all || p.2 != cur_schema,
        };
        if schema_or_all_changed {
            selected.set(None);
            cursor_key.set(None);
        }
        last_fetch.set(Some(current));
        listing.set(LoadState::Loading);
        detail_visible.set(true);
        let scope = if cur_all { None } else { cur_schema.as_deref() };
        let sql = plane.list_sql(cur_kind, scope);
        let tag = format!("objects:{}:{}",
            cur_kind.label(),
            if cur_all { "*".to_string() } else { cur_schema.clone().unwrap_or_default() });
        ws_introspect(ws, &sid, sql, tag);
    });

    // Database dropdown (PG only) — switches the explorer pane's session
    // to a different database on the same saved connection.
    let pg_only            = matches!(tab.db_type, DbType::Postgres);
    let conn_id_for_db     = tab.conn_id.clone();
    let conn_id_for_switch = tab.conn_id.clone();
    let pane_id_for_switch = pane_id.clone();
    let databases_sig      = tab.explorer_databases;
    let db_pending_sig     = tab.explorer_db_pending;
    let current_db_sig     = tab.current_db;
    let schema_sig         = tab.explorer_schema;
    let last_fetch_sig     = tab.explorer_last_fetch;

    view! {
        <div class="shell explorer-shell"
             id={format!("explorer-{}", pid_for_id)}
             on:click=move |_| active_pane_id.set(Some(pid_focus.clone()))>

            <div class="explorer-controls">
                {pg_only.then(|| {
                    let conn_id_for_load = conn_id_for_db.clone();
                    view! {
                        <div class="explorer-control-group"
                             style=move || if admin_active.get() { "display:none" } else { "" }>
                            <label class="explorer-control-label">"Database"</label>
                            {move || {
                                let cur_db = current_db_sig.get();
                                let state = databases_sig.get();
                                match state {
                                    LoadState::NotLoaded => {
                                        let cur_a   = cur_db.clone();
                                        let cur_b   = cur_db.clone();
                                        let load_id_p = conn_id_for_load.clone();
                                        let load_id_f = conn_id_for_load.clone();
                                        view! {
                                            <select class="explorer-select"
                                                on:pointerdown={
                                                    let pid = pane_id.clone();
                                                    move |_| {
                                                        if !matches!(databases_sig.get_untracked(), LoadState::NotLoaded) { return; }
                                                        let id = load_id_p.clone();
                                                        let def_opt = connections.with_untracked(
                                                            |cs| cs.iter().find(|c| c.id == id).cloned());
                                                        if let Some(pub_def) = def_opt {
                                                            // Blank password → noctua resolves
                                                            // it by saved id (browser never
                                                            // holds credentials, see #6).
                                                            let def = pub_def.with_password(String::new());
                                                            databases_sig.set(LoadState::Loading);
                                                            db_pending_sig.set(true);
                                                            ws_send(ws, &ClientMessage::ListDatabases {
                                                                def, client_tab_id: Some(pid.clone()),
                                                            });
                                                        }
                                                    }
                                                }
                                                on:focus={
                                                    let pid = pane_id.clone();
                                                    move |_| {
                                                        if !matches!(databases_sig.get_untracked(), LoadState::NotLoaded) { return; }
                                                        let id = load_id_f.clone();
                                                        let def_opt = connections.with_untracked(
                                                            |cs| cs.iter().find(|c| c.id == id).cloned());
                                                        if let Some(pub_def) = def_opt {
                                                            let def = pub_def.with_password(String::new());
                                                            databases_sig.set(LoadState::Loading);
                                                            db_pending_sig.set(true);
                                                            ws_send(ws, &ClientMessage::ListDatabases {
                                                                def, client_tab_id: Some(pid.clone()),
                                                            });
                                                        }
                                                    }
                                                }>
                                                <option value=cur_a selected=true>{cur_b}</option>
                                            </select>
                                        }.into_view()
                                    }
                                    LoadState::Loading => view! {
                                        <select class="explorer-select" disabled=true>
                                            <option>"Loading…"</option>
                                        </select>
                                    }.into_view(),
                                    LoadState::Error(e) => {
                                        // Show the failure inline with a retry affordance.
                                        // Hover the icon for the full error message; click
                                        // the ↻ to re-fire ListDatabases (which goes back
                                        // to NotLoaded → Loading → Loaded/Error per the
                                        // pointerdown/focus handlers above).
                                        let retry_id = conn_id_for_load.clone();
                                        let pid = pane_id.clone();
                                        view! {
                                            <span class="explorer-error" title=e.clone()>"⚠"</span>
                                            <button class="btn-load-dbs" title="Retry"
                                                on:click=move |_| {
                                                    let id = retry_id.clone();
                                                    let def_opt = connections.with_untracked(
                                                        |cs| cs.iter().find(|c| c.id == id).cloned());
                                                    if let Some(pub_def) = def_opt {
                                                        let def = pub_def.with_password(String::new());
                                                        databases_sig.set(LoadState::Loading);
                                                        db_pending_sig.set(true);
                                                        ws_send(ws, &ClientMessage::ListDatabases {
                                                            def, client_tab_id: Some(pid.clone()),
                                                        });
                                                    }
                                                }>"\u{27F3}"</button>
                                        }.into_view()
                                    },
                                    LoadState::Loaded(list) => {
                                        let cur_for_opts = cur_db.clone();
                                        let conn_id     = conn_id_for_switch.clone();
                                        let pane_id     = pane_id_for_switch.clone();
                                        view! {
                                            <select class="explorer-select"
                                                on:change=move |e| {
                                                    let v = event_target_value(&e);
                                                    if v.is_empty() { return; }
                                                    if v == current_db_sig.get_untracked() { return; }
                                                    let old_sid = session.id.get_untracked();
                                                    // Detach the pane from the old session
                                                    // BEFORE Disconnect so SessionClosed for
                                                    // old_sid doesn't match this pane and
                                                    // remove it.
                                                    session.id.set(None);
                                                    // Optimistic: show the target DB so the
                                                    // header/badge updates before
                                                    // SessionOpened arrives.
                                                    current_db_sig.set(v.clone());
                                                    // Invalidate every per-table cache.
                                                    table_details.update(|m| m.clear());
                                                    object_ddls.update(|m| m.clear());
                                                    // Per-kind last-selected memory holds
                                                    // (schema, name) tuples from the old DB.
                                                    // Leaving it would restore a stale name
                                                    // into `selected` on the next kind switch
                                                    // and fire an introspect for a table that
                                                    // may not exist in the new DB.
                                                    selections_by_kind.update(|m| m.clear());
                                                    listing.set(LoadState::NotLoaded);
                                                    selected.set(None);
                                                    cursor_key.set(None);
                                                    // Hide the inspector during the reconnect
                                                    // window; the next selection re-opens it.
                                                    detail_visible.set(false);
                                                    // Schemas repopulate when SessionOpened
                                                    // fires list_schemas_sql().
                                                    schemas.set(LoadState::Loading);
                                                    schema_sig.set(None);
                                                    last_fetch_sig.set(None);
                                                    databases_sig.set(LoadState::NotLoaded);
                                                    // Database-switch reconnect on the Schema
                                                    // Explorer pane — hardcode the role since
                                                    // this file is Explorer-only.
                                                    ws_send(ws, &ClientMessage::Connect {
                                                        connection_id: conn_id.clone(),
                                                        client_tab_id: pane_id.clone(),
                                                        database: Some(v),
                                                        client_role: Some("Schema Explorer".to_string()),
                                                        timezone: crate::settings::session_time_zone(),
                                                    });
                                                    if let Some(sid) = old_sid {
                                                        ws_send(ws, &ClientMessage::Disconnect {
                                                            session_id: sid,
                                                        });
                                                    }
                                                }>
                                                {list.into_iter().map(|d| {
                                                    let is_sel = cur_for_opts.as_str() == d.as_str();
                                                    view! {
                                                        <option value=d.clone() selected=is_sel>{d}</option>
                                                    }
                                                }).collect_view()}
                                            </select>
                                        }.into_view()
                                    }
                                }
                            }}
                        </div>
                    }
                })}
                <div class="explorer-control-group"
                     style=move || if admin_active.get() { "display:none" } else { "" }>
                    <label class="explorer-control-label">{plane.schema_picker_label()}</label>
                    {move || match schemas.get() {
                        LoadState::NotLoaded | LoadState::Loading => view! {
                            <select class="explorer-select" disabled=true>
                                <option>"Loading…"</option>
                            </select>
                        }.into_view(),
                        LoadState::Error(e) => view! {
                            <span class="explorer-error">{e}</span>
                        }.into_view(),
                        LoadState::Loaded(mut list) => {
                            let cur = schema.get();
                            // A schema the list hides (`\d pg_class` opens
                            // pg_catalog) still shows as the one selected.
                            if let Some(c) = cur.as_ref().filter(|c| !list.contains(c)) {
                                list.insert(0, c.clone());
                            }
                            view! {
                                <select class="explorer-select"
                                    prop:disabled=move || all_schemas.get()
                                    on:change=move |e| {
                                        let v = event_target_value(&e);
                                        schema.set(if v.is_empty() { None } else { Some(v) });
                                    }>
                                    {list.into_iter().map(|s| {
                                        let is_sel = cur.as_deref() == Some(&s);
                                        view! {
                                            <option value=s.clone() selected=is_sel>{s}</option>
                                        }
                                    }).collect_view()}
                                </select>
                            }.into_view()
                        }
                    }}
                </div>

                <div class="explorer-control-group explorer-search-group">
                    <input class="explorer-search-input" type="text"
                        prop:placeholder=move || {
                            if admin_active.get() {
                                "Search admin areas…".to_string()
                            } else {
                                format!("Search {}…", kind.get().label().to_lowercase())
                            }
                        }
                        prop:value=move || search.get()
                        on:input=move |e| search.set(event_target_value(&e)) />
                </div>

                <label class="explorer-checkbox"
                       style=move || if admin_active.get() { "display:none" } else { "" }>
                    <input type="checkbox"
                        prop:checked=move || all_schemas.get()
                        on:change=move |e| {
                            use wasm_bindgen::JsCast;
                            let checked = e.target()
                                .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                                .map(|i| i.checked()).unwrap_or(false);
                            all_schemas.set(checked);
                        } />
                    <span>"All schemas"</span>
                </label>

                <button class="explorer-refresh-btn" title="Refresh"
                    on:click=move |_| {
                        if let Some(sid) = session.id.get_untracked() {
                            schemas.set(LoadState::Loading);
                            ws_introspect(ws, &sid, plane.list_schemas_sql().to_string(), "schemas".into());
                            kind.update(|_| {});
                        }
                    }>"↻"</button>
                {crate::shell::session_id_pill(db_conn_id, tab.session_stale)}
            </div>

            <div class="explorer-typebar">
                {plane.object_kinds().iter().map(|k| {
                    let k = *k;
                    view! {
                        <button class="explorer-type-btn"
                            class:explorer-type-active=move || !admin_active.get() && kind.get() == k
                            on:click=move |_| {
                                // Switching to an object-kind: leave admin
                                // mode but DON'T touch admin_area — going
                                // back to Admin restores the user's last
                                // area.
                                admin_active.set(false);
                                if kind.get_untracked() != k {
                                    // Real kind change (Tables→Views etc.).
                                    // The current `selected` belongs to the
                                    // OLD kind's namespace; the mirror
                                    // effect already cached it under the
                                    // old kind. Look up what (if anything)
                                    // was last selected under the new kind
                                    // and restore that — this is what gives
                                    // each kind its own remembered focus
                                    // across Views ↔ Tables round trips.
                                    kind.set(k);
                                    let restored = selections_by_kind
                                        .with_untracked(|m| m.get(&k).cloned());
                                    selected.set(restored.clone());
                                    cursor_key.set(restored);
                                    // Exit fullscreen on real kind change
                                    // so the (possibly-empty) new listing
                                    // is visible. detail_expanded is per-
                                    // session, not per-kind.
                                    detail_expanded.set(false);
                                }
                            }>
                            <span class="explorer-type-icon">{k.icon()}</span>
                            <span class="explorer-type-label">{k.label()}</span>
                        </button>
                    }
                }).collect_view()}
                // Admin button — only rendered when the plane has any admin
                // areas to surface. A divider separates it from the object-
                // kind buttons so the eye groups them as distinct sections.
                {if plane.admin_areas().is_empty() {
                    view! { <span/> }.into_view()
                } else {
                    view! {
                        <span class="explorer-typebar-divider"></span>
                        <button class="explorer-type-btn"
                            class:explorer-type-active=move || admin_active.get()
                            on:click=move |_| {
                                // Enter admin: don't clear `selected` or
                                // `detail_expanded` — those belong to the
                                // object-kind side and survive the Admin
                                // round trip. Auto-select the first admin
                                // area on initial entry so the inspector
                                // isn't blank; subsequent entries restore
                                // the user's last area.
                                admin_active.set(true);
                                if admin_area.get_untracked().is_none() {
                                    if let Some(first) = plane.admin_areas().first().copied() {
                                        admin_area.set(Some(first));
                                    }
                                }
                            }>
                            <span class="explorer-type-icon">"⛭"</span>
                            <span class="explorer-type-label">"Admin"</span>
                        </button>
                    }.into_view()
                }}
            </div>

            // Admin sub-toolbar — rendered only when the user has clicked
            // Admin in the main typebar. Sits between typebar and body so
            // the body stays available for the selected area's content.
            {move || {
                if admin_active.get() {
                    render_admin_subtoolbar(plane, admin_area, selected, detail_visible)
                } else {
                    view! { <div/> }.into_view()
                }
            }}

            <div class="explorer-body">
                <div class="explorer-list-wrap"
                     style=move || if detail_expanded.get() {
                         "display:none"
                     } else {
                         ""
                     }>
                    {move || {
                        if admin_active.get() {
                            // Admin path. Each area has its own list shape;
                            // unsupported areas show a placeholder so the
                            // user can see we know about them.
                            let area = admin_area.get();
                            match area {
                                Some(AdminArea::Roles) => render_admin_roles_list(
                                    plane, admin_roles.get(), search.get(),
                                    admin_selected, detail_visible,
                                    admin_roles, ws, session,
                                ),
                                Some(other) => {
                                    let state = admin_area_data.with(|m|
                                        m.get(&other).cloned().unwrap_or(LoadState::NotLoaded));
                                    render_admin_area_list(plane, other, state, search.get(),
                                        admin_row_selected, detail_visible)
                                }
                                None => view! {
                                    <div class="explorer-msg">"Pick an admin area above."</div>
                                }.into_view(),
                            }
                        } else {
                            render_object_list(
                                plane,
                                kind.get(),
                                listing.get(),
                                search.get(),
                                all_schemas.get(),
                                selected,
                                cursor_key,
                                detail_visible,
                                table_details,
                                object_ddls,
                                ws,
                                session,
                                expanded_parents,
                                partition_menu,
                                sort_by_kind,
                            )
                        }
                    }}
                </div>

                {move || {
                    if !detail_visible.get() { return view! { <div/> }.into_view(); }

                    // Admin path: when admin_active is on AND the Roles
                    // area is active, render the per-role inspector. For
                    // the read-only areas, render a per-row drill-in
                    // inspector when the plane has marked the area
                    // clickable AND a row is selected; otherwise the
                    // table is the inspector and we early-return.
                    if admin_active.get() {
                        let Some(area) = admin_area.get() else {
                            return view! { <div/> }.into_view();
                        };
                        if area != AdminArea::Roles {
                            // Non-Roles areas: only show inspector when a
                            // clickable row has been selected.
                            let row = admin_row_selected.get();
                            let Some((sel_area, row_id)) = row else {
                                return view! { <div/> }.into_view();
                            };
                            if sel_area != area {
                                return view! { <div/> }.into_view();
                            }
                            let title = format!("{} · {}", area.label(), row_id);
                            let body = render_admin_area_row_detail(
                                plane, area, row_id.clone(), admin_row_detail,
                                ws, session,
                                tab.explorer_priv_cell_status,
                                tab.explorer_priv_pending_revoke,
                            );
                            return view! {
                                <div class="explorer-inspector"
                                     style=move || if detail_expanded.get() { "flex:1;height:auto" } else { "" }>
                                    <div class="explorer-inspector-header">
                                        <span class="explorer-inspector-title">{title}</span>
                                        <button class="explorer-inspector-expand"
                                            title=move || if detail_expanded.get() { "Collapse" } else { "Expand" }
                                            on:click=move |_| detail_expanded.update(|v| *v = !*v)>
                                            {move || if detail_expanded.get() { "⤡" } else { "⤢" }}
                                        </button>
                                        <button class="explorer-inspector-close" title="Close inspector"
                                            on:click=move |_| {
                                                admin_row_selected.set(None);
                                                detail_visible.set(false);
                                                detail_expanded.set(false);
                                            }>"×"</button>
                                    </div>
                                    {body}
                                </div>
                            }.into_view();
                        }
                        // If a role row is clicked, show role-detail in the
                        // inspector. Otherwise, show a "pick a role" hint
                        // (or the area's coming-soon for non-Roles areas).
                        let row = admin_selected.get();
                        let title = if let Some(r) = &row {
                            format!("{} · {}", area.label(), r.name)
                        } else {
                            format!("{} · {}", area.scope().label(), area.label())
                        };
                        let inspector_body = if let Some(r) = row.clone() {
                            render_admin_role_detail(
                                plane, r,
                                admin_role_tab, admin_role_privs,
                                admin_role_privs_view, admin_role_privs_selected,
                                admin_role_affected_objects,
                                admin_role_privs_kinds,
                                admin_role_affected_kinds,
                            )
                        } else {
                            view! {
                                <div class="detail-msg admin-placeholder">
                                    "Select a role from the list to view details."
                                </div>
                            }.into_view()
                        };
                        return view! {
                            <div class="explorer-inspector"
                                 style=move || if detail_expanded.get() { "flex:1;height:auto" } else { "" }>
                                <div class="explorer-inspector-header">
                                    <span class="explorer-inspector-title">{title}</span>
                                    <button class="explorer-inspector-expand"
                                        title=move || if detail_expanded.get() { "Collapse" } else { "Expand" }
                                        on:click=move |_| detail_expanded.update(|v| *v = !*v)>
                                        {move || if detail_expanded.get() { "⤡" } else { "⤢" }}
                                    </button>
                                    <button class="explorer-inspector-close" title="Close inspector"
                                        on:click=move |_| {
                                            detail_visible.set(false);
                                            detail_expanded.set(false);
                                        }>"×"</button>
                                </div>
                                {inspector_body}
                            </div>
                        }.into_view();
                    }

                    let Some((sch, name)) = selected.get() else {
                        return view! { <div/> }.into_view();
                    };
                    // Re-render once the selection's details are cached
                    // (see `selected_detail_cached`).
                    selected_detail_cached.track();
                    // Refresh button — only for Tables today, since Views'
                    // detail is DDL-only and doesn't go stale after ANALYZE.
                    // Refetches every tab's data into the cached
                    // TableDetail in place (see `refresh_table_detail`);
                    // the icon spins until the overview reply lands.
                    let is_table = kind.get() == ObjectKind::Tables;
                    let refresh_sch  = sch.clone();
                    let refresh_name = name.clone();
                    view! {
                        <div class="explorer-inspector"
                             style=move || if detail_expanded.get() { "flex:1;height:auto" } else { "" }>
                            <div class="explorer-inspector-header">
                                <span class="explorer-inspector-title">
                                    {format!("{} · {}", sch, name)}
                                </span>
                                {if is_table {
                                    let s = refresh_sch.clone();
                                    let n = refresh_name.clone();
                                    let refreshing = {
                                        let key = (s.clone(), n.clone());
                                        move || table_details.with(|m|
                                            m.get(&key).is_some_and(|d| d.refreshing.get()))
                                    };
                                    view! {
                                        <button class="explorer-inspector-expand explorer-refresh"
                                            title="Refresh — re-fetch everything on this table's tabs from the server"
                                            class:refreshing=refreshing
                                            on:click=move |_| {
                                                if let Some(detail) = table_details
                                                    .with_untracked(|m| m.get(&(s.clone(), n.clone())).copied())
                                                {
                                                    refresh_table_detail(plane, &s, &n, detail, ws, session);
                                                }
                                            }>
                                            "\u{21BB}"
                                        </button>
                                    }.into_view()
                                } else {
                                    view! {}.into_view()
                                }}
                                <button class="explorer-inspector-expand"
                                    title=move || if detail_expanded.get() { "Collapse" } else { "Expand" }
                                    on:click=move |_| detail_expanded.update(|v| *v = !*v)>
                                    {move || if detail_expanded.get() { "⤡" } else { "⤢" }}
                                </button>
                                <button class="explorer-inspector-close" title="Close inspector"
                                    on:click=move |_| {
                                        detail_visible.set(false);
                                        detail_expanded.set(false);
                                        if let Some(el) = web_sys::window()
                                            .and_then(|w| w.document())
                                            .and_then(|d| d.query_selector(".explorer-list-row-active").ok().flatten())
                                        {
                                            let opts = web_sys::ScrollIntoViewOptions::new();
                                            opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
                                            opts.set_behavior(web_sys::ScrollBehavior::Smooth);
                                            el.scroll_into_view_with_scroll_into_view_options(&opts);
                                        }
                                    }>"×"</button>
                            </div>
                            {render_inline_detail(
                                kind.get(), sch.clone(), name.clone(),
                                explorer_pane_id.clone(),
                                table_details, object_ddls,
                                plane, selected, ws, session,
                                shell_panes, pane_ops,
                                tab.explorer_priv_cell_status,
                                tab.explorer_priv_pending_revoke,
                            )}
                        </div>
                    }.into_view()
                }}
            </div>
        </div>
    }
}

#[allow(clippy::too_many_arguments)]
/// Render the horizontal sub-toolbar shown when the user has switched the
/// typebar to Admin. Areas appear as buttons grouped by AdminScope with a
/// subtle scope label preceding each group; vertical dividers separate
/// groups so the user sees the natural hierarchy without giving up
/// vertical space. Clicking an area sets `admin_area`; the inspector
/// reads it and renders the per-area view.
pub(crate) fn render_admin_subtoolbar(
    plane:          &'static dyn DbmsPlane,
    admin_area:     RwSignal<Option<AdminArea>>,
    selected:       RwSignal<Option<(String, String)>>,
    detail_visible: RwSignal<bool>,
) -> leptos::View {
    let areas = plane.admin_areas();
    if areas.is_empty() {
        return view! { <div/> }.into_view();
    }
    let mut groups: BTreeMap<i32, Vec<AdminArea>> = BTreeMap::new();
    for area in areas {
        let key = match area.scope() {
            AdminScope::Instance => 0,
            AdminScope::Database => 1,
            AdminScope::Schema   => 2,
        };
        groups.entry(key).or_default().push(*area);
    }
    view! {
        <div class="admin-subtoolbar">
            {groups.into_iter().map(|(scope_idx, group)| {
                let scope = match scope_idx {
                    0 => AdminScope::Instance,
                    1 => AdminScope::Database,
                    _ => AdminScope::Schema,
                };
                view! {
                    <div class="admin-group">
                        <span class="admin-group-label">{scope.label()}</span>
                        {group.into_iter().map(|area| {
                            let area_for_active = area;
                            view! {
                                <button class="admin-area-btn"
                                        class:admin-area-btn-active=move || admin_area.get() == Some(area_for_active)
                                        on:click=move |_| {
                                            admin_area.set(Some(area));
                                            selected.set(None);
                                            detail_visible.set(true);
                                        }>
                                    <span class="admin-area-icon">{area.icon()}</span>
                                    <span class="admin-area-label">{area.label()}</span>
                                </button>
                            }
                        }).collect_view()}
                    </div>
                }
            }).collect_view()}
        </div>
    }.into_view()
}

/// Render the flat list of admin roles (PG: pg_roles; MySQL: mysql.user
/// rows). Columns are plane-specific via `plane.role_list_extra_columns`;
/// Name is always the first column. Click a row to drill into its detail.
/// Search filters case-insensitively across name + extras so DBAs can
/// grep "localhost" or "super" without thinking about which column.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_admin_roles_list(
    plane:          &'static dyn DbmsPlane,
    state:          LoadState<Vec<AdminRow>>,
    search:         String,
    admin_selected: RwSignal<Option<AdminRow>>,
    detail_visible: RwSignal<bool>,
    admin_roles:    RwSignal<LoadState<Vec<AdminRow>>>,
    ws:             RwSignal<Option<WebSocket>>,
    session:     ExplorerSession,
) -> leptos::View {
    match state {
        LoadState::NotLoaded | LoadState::Loading => view! {
            <div class="explorer-msg">"Loading roles…"</div>
        }.into_view(),
        LoadState::Error(e) => view! {
            <div class="explorer-error">
                <div>{e}</div>
                <button class="explorer-error-retry"
                    on:click=move |_| {
                        let Some(sql) = plane.list_roles_sql() else { return; };
                        let Some(sid) = session.id.get_untracked() else { return; };
                        admin_roles.set(LoadState::Loading);
                        ws_introspect(ws, &sid, sql.to_string(), "admin_roles".into());
                    }>"Retry"</button>
            </div>
        }.into_view(),
        LoadState::Loaded(rows) => {
            let needle = search.trim().to_ascii_lowercase();
            let filtered: Vec<AdminRow> = if needle.is_empty() {
                rows
            } else {
                rows.into_iter()
                    .filter(|r| {
                        r.name.to_ascii_lowercase().contains(&needle)
                            || r.extras.iter().any(|e| e.to_ascii_lowercase().contains(&needle))
                    })
                    .collect()
            };
            if filtered.is_empty() {
                return view! { <div class="explorer-msg">"No roles match."</div> }.into_view();
            }
            let extras = plane.role_list_extra_columns();
            view! {
                <div class="explorer-list-scroll">
                    <table class="explorer-list">
                        <thead>
                            <tr>
                                <th class="ex-th-hidden"></th>
                                <th class="ex-th-name">"Name"</th>
                                {extras.iter().map(|h| view! { <th>{*h}</th> }).collect_view()}
                            </tr>
                        </thead>
                        <tbody>
                            {filtered.into_iter().map(|row| {
                                let row_for_active = row.clone();
                                let row_for_click  = row.clone();
                                view! {
                                    <tr class="explorer-list-row"
                                        class:explorer-list-row-active=move || {
                                            admin_selected.get()
                                                .map(|s| s.name == row_for_active.name && s.extras == row_for_active.extras)
                                                .unwrap_or(false)
                                        }
                                        on:click=move |_| {
                                            admin_selected.set(Some(row_for_click.clone()));
                                            detail_visible.set(true);
                                        }>
                                        <td class="ex-td-hidden"></td>
                                        <td class="ex-td-name">{row.name}</td>
                                        {row.extras.into_iter().map(|c| view! {
                                            <td class="ex-td-extra">{c}</td>
                                        }).collect_view()}
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            }.into_view()
        }
    }
}

/// Render a non-Roles admin area as a flat table — Tablespaces /
/// Databases / Extensions / ServerConfig. When `plane.area_row_clickable`
/// is true for this area, rows become clickable and set
/// `row_selected` + `detail_visible` so the inspector renders the
/// drill-in detail. Search filters case-insensitively across all
/// columns so finding `max_connections` in PG's ServerConfig is one
/// keystroke.
pub(crate) fn render_admin_area_list(
    plane:  &'static dyn DbmsPlane,
    area:   AdminArea,
    state:  LoadState<Vec<Vec<String>>>,
    search: String,
    row_selected:   RwSignal<Option<(AdminArea, String)>>,
    detail_visible: RwSignal<bool>,
) -> leptos::View {
    let columns = plane.area_list_columns(area);
    if columns.is_empty() {
        return view! {
            <div class="explorer-msg">
                {format!("{} listing not supported on this plane.", area.label())}
            </div>
        }.into_view();
    }
    match state {
        LoadState::NotLoaded | LoadState::Loading => view! {
            <div class="explorer-msg">{format!("Loading {}…", area.label().to_lowercase())}</div>
        }.into_view(),
        LoadState::Error(e) => view! {
            <div class="explorer-error">{e}</div>
        }.into_view(),
        LoadState::Loaded(rows) => {
            let needle = search.trim().to_ascii_lowercase();
            let filtered: Vec<Vec<String>> = if needle.is_empty() {
                rows
            } else {
                rows.into_iter()
                    .filter(|r| r.iter().any(|c| c.to_ascii_lowercase().contains(&needle)))
                    .collect()
            };
            if filtered.is_empty() {
                return view! { <div class="explorer-msg">{format!("No {} match.", area.label().to_lowercase())}</div> }.into_view();
            }
            let clickable = plane.area_row_clickable(area);
            view! {
                <div class="explorer-list-scroll">
                    <table class="explorer-list role-privs-list">
                        <thead>
                            <tr>
                                {columns.iter().map(|h| view! { <th>{*h}</th> }).collect_view()}
                            </tr>
                        </thead>
                        <tbody>
                            {filtered.into_iter().map(|row| {
                                // Row id = first column (Name by convention
                                // across all four areas). Click opens the
                                // drill-in inspector.
                                let row_id = row.first().cloned().unwrap_or_default();
                                let row_id_for_active = row_id.clone();
                                let row_id_for_click  = row_id.clone();
                                view! {
                                    <tr class:explorer-list-row=move || clickable
                                        class:explorer-list-row-active=move || {
                                            clickable && row_selected.get()
                                                .map(|(a, n)| a == area && n == row_id_for_active)
                                                .unwrap_or(false)
                                        }
                                        on:click=move |_| {
                                            if !clickable { return; }
                                            row_selected.set(Some((area, row_id_for_click.clone())));
                                            detail_visible.set(true);
                                        }>
                                        {row.into_iter().map(|c| view! {
                                            <td>{c}</td>
                                        }).collect_view()}
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            }.into_view()
        }
    }
}

/// Render the drill-in detail for a clicked area row. v1 implementation
/// is generic: iterates `plane.area_row_detail_sections` and renders
/// each as a sub-table fed by the per-(area, row, section) cache.
/// Sections fetch lazily; an empty-state on the Tablespaces "Relations
/// Stored Here" section explains the pg_default reltablespace=0 quirk.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_admin_area_row_detail(
    plane:        &'static dyn DbmsPlane,
    area:         AdminArea,
    row_id:       String,
    detail_data:  RwSignal<AdminRowDetails>,
    // Click-edit signals + WS handles; passed-through unconditionally
    // but only the "Privileges" section dispatch uses them. None of
    // the existing Tablespaces sections care.
    ws:                  RwSignal<Option<WebSocket>>,
    session:          ExplorerSession,
    priv_cell_status:    RwSignal<HashMap<(String, String), EditStatus>>,
    priv_pending_revoke: RwSignal<Option<(String, String)>>,
) -> leptos::View {
    let sections = plane.area_row_detail_sections(area);
    if sections.is_empty() {
        return view! {
            <div class="detail-msg admin-placeholder">
                "No detail sections defined for this area."
            </div>
        }.into_view();
    }

    let section_views: Vec<leptos::View> = sections.iter().map(|section| {
        let section_label = *section;
        let columns = plane.area_row_detail_columns(area, section_label);
        let key = (area, row_id.clone(), section_label.to_string());
        let state = detail_data.with(|m| m.get(&key).cloned().unwrap_or(LoadState::NotLoaded));
        let body = match state {
            LoadState::NotLoaded | LoadState::Loading => view! {
                <div class="explorer-msg">"Loading…"</div>
            }.into_view(),
            LoadState::Error(e) => view! {
                <div class="explorer-error">{e}</div>
            }.into_view(),
            LoadState::Loaded(rows) if rows.is_empty() => {
                let empty_hint =
                    if area == AdminArea::Tablespaces && section_label == "Relations Stored Here" {
                        "No relations are explicitly placed in this tablespace. \
                         Note: relations created without an explicit TABLESPACE clause \
                         inherit their database's default and don't appear here even \
                         if physically stored in pg_default."
                    } else {
                        "No rows."
                    };
                view! { <div class="explorer-msg">{empty_hint}</div> }.into_view()
            }
            LoadState::Loaded(rows) => {
                // "Privileges" sections pivot the flat (Grantee,
                // Privilege, Source, Chain, Grantable, RoleCategory)
                // shape into a per-grantee matrix. Same renderer the
                // object-Privileges tab uses — the matrix is shape-
                // agnostic, taking only the priv-kind labels.
                // The string-literal "Privileges" couples this
                // dispatch to schema_privilege_kinds(). Gate on a
                // non-empty kinds list so a future plane that
                // registers a "Privileges" section without overriding
                // schema_privilege_kinds() falls through to the
                // generic table renderer instead of producing a
                // grantee-only matrix with no data cells.
                let priv_kinds = plane.schema_privilege_kinds();
                if section_label == "Privileges" && !priv_kinds.is_empty() {
                    // parse_object_grant_rows wants Vec<String>
                    // headers; the trait gives us &[&str].
                    let columns_owned: Vec<String> =
                        columns.iter().map(|s| s.to_string()).collect();
                    let grant_rows = crate::explorer::parse_object_grant_rows(&columns_owned, rows);
                    // Build the schema-matrix edit context. Closures
                    // capture schema name (row_id) + plane + ws etc.
                    // by clone. Tag prefix is schema_grant / schema_revoke
                    // so main.rs's DmlResult router can target the
                    // right cache for refetch.
                    let schema_name = row_id.clone();
                    let edit = {
                        let s_g = schema_name.clone();
                        let s_r = schema_name.clone();
                        let s_tag_g = schema_name.clone();
                        let s_tag_r = schema_name.clone();
                        let grant_fire = std::rc::Rc::new(move |grantee: &str, priv_name: &str| {
                            let Some(sid) = session.id.get_untracked() else { return };
                            let Some(sql) = plane.schema_grant_sql(&s_g, grantee, priv_name)
                                else { return };
                            priv_cell_status.update(|m| { m.insert(
                                (grantee.to_string(), priv_name.to_string()),
                                EditStatus::Saving); });
                            let tag = format!("schema_grant:{}:{}:{}", s_tag_g, grantee, priv_name);
                            crate::dom::ws_dml(ws, &sid, sql, tag);
                        }) as std::rc::Rc<dyn Fn(&str, &str)>;
                        let revoke_fire = std::rc::Rc::new(move |grantee: &str, priv_name: &str| {
                            let Some(sid) = session.id.get_untracked() else { return };
                            let Some(sql) = plane.schema_revoke_sql(&s_r, grantee, priv_name)
                                else { return };
                            priv_cell_status.update(|m| { m.insert(
                                (grantee.to_string(), priv_name.to_string()),
                                EditStatus::Saving); });
                            let tag = format!("schema_revoke:{}:{}:{}", s_tag_r, grantee, priv_name);
                            crate::dom::ws_dml(ws, &sid, sql, tag);
                        }) as std::rc::Rc<dyn Fn(&str, &str)>;
                        Some(PrivilegeEdit {
                            cell_status:    priv_cell_status,
                            pending_revoke: priv_pending_revoke,
                            object_label:   format!("schema {}", schema_name),
                            grant_fire,
                            revoke_fire,
                        })
                    };
                    render_privileges_matrix(
                        grant_rows,
                        priv_kinds,
                        /* show_inherited */ true,
                        /* show_no_grant */ false,
                        /* filter_text */ String::new(),
                        edit,
                    )
                } else {
                    view! {
                        <div class="explorer-list-scroll">
                            <table class="explorer-list role-privs-list">
                                <thead>
                                    <tr>
                                        {columns.iter().map(|h| view! { <th>{*h}</th> }).collect_view()}
                                    </tr>
                                </thead>
                                <tbody>
                                    {rows.into_iter().map(|row| view! {
                                        <tr>
                                            {row.into_iter().map(|c| view! {
                                                <td>{c}</td>
                                            }).collect_view()}
                                        </tr>
                                    }).collect_view()}
                                </tbody>
                            </table>
                        </div>
                    }.into_view()
                }
            }
        };
        view! {
            <div class="schema-section-header">{section_label}</div>
            {body}
        }.into_view()
    }).collect();

    view! {
        <div class="schema-scroll">
            {section_views.into_iter().collect_view()}
        </div>
    }.into_view()
}

/// Render the role-detail inspector with General + Privileges tabs.
/// Privileges shows the role's effective object access — explicit grants
/// AND ownership-implied ALL PRIVILEGES — in a single table with a
/// Source column tagging each row. A banner above the table flags
/// superuser status, since superusers bypass checks regardless of what
/// the table shows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_admin_role_detail(
    plane:        &'static dyn DbmsPlane,
    row:          AdminRow,
    role_tab:     RwSignal<RoleDetailTab>,
    role_privs:   RwSignal<LoadState<Vec<Vec<String>>>>,
    privs_view:   RwSignal<RolePrivsView>,
    privs_selected: RwSignal<std::collections::HashSet<usize>>,
    affected_objects: RwSignal<LoadState<Vec<Vec<String>>>>,
    privs_kinds:  RwSignal<Option<std::collections::HashSet<String>>>,
    affected_kinds: RwSignal<Option<std::collections::HashSet<String>>>,
) -> leptos::View {
    let has_privs = !plane.role_privileges_columns().is_empty();
    let is_superuser = plane.role_is_superuser(&row);
    let extras = plane.role_list_extra_columns();
    let pairs: Vec<(String, String)> = std::iter::once(("Name".to_string(), row.name.clone()))
        .chain(extras.iter().zip(row.extras.iter()).map(|(h, v)| (h.to_string(), v.clone())))
        .collect();
    let pairs_for_general = pairs.clone();
    let priv_columns = plane.role_privileges_columns();

    view! {
        <div class="schema-scroll">
            // Sub-tab strip — General is always present; Privileges
            // appears only when the plane supplies columns for it.
            <div class="role-detail-tabs">
                <button class="role-detail-tab"
                    class:role-detail-tab-active=move || role_tab.get() == RoleDetailTab::General
                    on:click=move |_| role_tab.set(RoleDetailTab::General)>
                    "General"
                </button>
                {if has_privs {
                    view! {
                        <button class="role-detail-tab"
                            class:role-detail-tab-active=move || role_tab.get() == RoleDetailTab::Privileges
                            on:click=move |_| role_tab.set(RoleDetailTab::Privileges)>
                            "Privileges"
                        </button>
                    }.into_view()
                } else { view! { <span/> }.into_view() }}
            </div>

            {move || match role_tab.get() {
                RoleDetailTab::General => {
                    let pairs = pairs_for_general.clone();
                    view! {
                        <div class="object-header">
                            <div class="object-meta-grid">
                                {pairs.into_iter().map(|(k, v)| view! {
                                    <div class="object-meta-cell">
                                        <span class="object-meta-key">{k}</span>
                                        <span class="object-meta-val">{v}</span>
                                    </div>
                                }).collect_view()}
                            </div>
                        </div>
                    }.into_view()
                }
                RoleDetailTab::Privileges => {
                    // Sub-view strip — "Grants" with selection +
                    // "Show affected objects" resolution.
                    let strip = view! {
                        <div class="role-privs-subtabs">
                            <button class="role-privs-subtab"
                                class:role-privs-subtab-active=move || privs_view.get() == RolePrivsView::Grants
                                on:click=move |_| privs_view.set(RolePrivsView::Grants)>
                                "Grants"
                            </button>
                            <button class="role-privs-subtab"
                                class:role-privs-subtab-active=move || privs_view.get() == RolePrivsView::AffectedObjects
                                title="Expand the selected grants to the concrete tables they touch"
                                on:click=move |_| privs_view.set(RolePrivsView::AffectedObjects)>
                                {move || {
                                    // The badge reflects the resolved
                                    // object count — that's the number
                                    // of rows the user will see if they
                                    // click in. With nothing selected
                                    // it's plainly zero; while we wait
                                    // for the resolver we show "…"; on
                                    // failure we drop the count rather
                                    // than mislead.
                                    let sel = privs_selected.with(|s| s.len());
                                    if sel == 0 {
                                        return "Show affected objects".to_string();
                                    }
                                    match affected_objects.get() {
                                        LoadState::Loaded(rows) =>
                                            format!("Show affected objects ({})", rows.len()),
                                        LoadState::Loading | LoadState::NotLoaded =>
                                            "Show affected objects (…)".to_string(),
                                        LoadState::Error(_) =>
                                            "Show affected objects".to_string(),
                                    }
                                }}
                            </button>
                        </div>
                    };

                    let kind_col_idx = priv_columns.iter().position(|c| *c == "Kind");
                    let grants_body = move || match role_privs.get() {
                        LoadState::NotLoaded | LoadState::Loading => view! {
                            <div class="explorer-msg">"Loading privileges…"</div>
                        }.into_view(),
                        LoadState::Error(e) => view! {
                            <div class="explorer-error">{e}</div>
                        }.into_view(),
                        LoadState::Loaded(rows) if rows.is_empty() => view! {
                            <div class="explorer-msg">"No effective privileges."</div>
                        }.into_view(),
                        LoadState::Loaded(rows) => {
                            // Distinct kinds present in this role's rows,
                            // sorted for stable chip order. Chip filter
                            // hides rows of unchecked kinds; it also
                            // narrows the affected-objects resolver
                            // scope (the lazy-fetch effect cross-checks
                            // visibility before feeding rows in).
                            let all_kinds: Vec<String> = match kind_col_idx {
                                Some(kc) => {
                                    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
                                    for r in &rows {
                                        if let Some(k) = r.get(kc) { set.insert(k.clone()); }
                                    }
                                    set.into_iter().collect()
                                }
                                None => Vec::new(),
                            };
                            let is_kind_active = move |k: &str| -> bool {
                                privs_kinds.with(|f| match f {
                                    None => true,
                                    Some(set) => set.contains(k),
                                })
                            };
                            let row_visible_for_render = move |row: &Vec<String>| -> bool {
                                match (kind_col_idx, privs_kinds.get()) {
                                    (Some(kc), Some(set)) =>
                                        row.get(kc).map(|k| set.contains(k)).unwrap_or(false),
                                    _ => true,
                                }
                            };
                            // Pre-compute the visible-index set so the
                            // header checkbox's "all-checked" semantics
                            // operate on the currently-visible rows
                            // only — toggling it shouldn't sweep across
                            // hidden rows.
                            let visible_indices: Vec<usize> = rows.iter()
                                .enumerate()
                                .filter(|(_, r)| row_visible_for_render(r))
                                .map(|(i, _)| i)
                                .collect();
                            let visible_total = visible_indices.len();
                            let visible_indices_for_indeterminate = visible_indices.clone();
                            let visible_indices_for_toggle = visible_indices.clone();
                            view! {
                                {if all_kinds.is_empty() {
                                    view! {}.into_view()
                                } else {
                                    let chips = all_kinds.into_iter().map(|kind| {
                                        let kind_for_check = kind.clone();
                                        let kind_for_click = kind.clone();
                                        view! {
                                            <button class="role-privs-kind-chip"
                                                class:role-privs-kind-chip-active=move || is_kind_active(&kind_for_check)
                                                on:click=move |_| {
                                                    let k = kind_for_click.clone();
                                                    privs_kinds.update(|f| {
                                                        // First click materialises the
                                                        // implicit "all" set, minus this
                                                        // kind. Subsequent clicks toggle
                                                        // membership.
                                                        match f {
                                                            None => {
                                                                // Reconstruct full set
                                                                // minus this kind.
                                                                let mut set = std::collections::HashSet::new();
                                                                if let LoadState::Loaded(rs) = role_privs.get_untracked() {
                                                                    if let Some(kc) = kind_col_idx {
                                                                        for r in &rs {
                                                                            if let Some(rk) = r.get(kc) { set.insert(rk.clone()); }
                                                                        }
                                                                    }
                                                                }
                                                                set.remove(&k);
                                                                *f = Some(set);
                                                            }
                                                            Some(set) => {
                                                                if set.contains(&k) { set.remove(&k); }
                                                                else                { set.insert(k); }
                                                            }
                                                        }
                                                    });
                                                }>
                                                {kind}
                                            </button>
                                        }
                                    }).collect_view();
                                    view! {
                                        <div class="role-privs-kind-strip"
                                             title="Filter rows by object kind. Unchecked kinds hide from the table and don't count toward affected objects.">
                                            {chips}
                                        </div>
                                    }.into_view()
                                }}
                                <div class="explorer-list-scroll">
                                    <table class="explorer-list role-privs-list">
                                        <thead>
                                            <tr>
                                                <th class="role-privs-check-cell"
                                                    title="Select rows to feed the affected-objects resolver">
                                                    <input type="checkbox"
                                                        prop:checked=move || privs_selected.with(|s|
                                                            visible_total > 0 && visible_indices.iter().all(|i| s.contains(i)))
                                                        prop:indeterminate=move || privs_selected.with(|s| {
                                                            let any = visible_indices_for_indeterminate.iter().any(|i| s.contains(i));
                                                            let all = visible_indices_for_indeterminate.iter().all(|i| s.contains(i));
                                                            any && !all
                                                        })
                                                        on:change=move |e| {
                                                            let all = event_target_checked(&e);
                                                            let idxs = visible_indices_for_toggle.clone();
                                                            privs_selected.update(|s| {
                                                                if all { for i in &idxs { s.insert(*i); } }
                                                                else   { for i in &idxs { s.remove(i);  } }
                                                            });
                                                        }/>
                                                </th>
                                                {priv_columns.iter().map(|h| view! { <th>{*h}</th> }).collect_view()}
                                            </tr>
                                        </thead>
                                        <tbody>
                                            {rows.into_iter().enumerate()
                                                .filter(|(_, row)| row_visible_for_render(row))
                                                .map(|(idx, row)| view! {
                                                <tr>
                                                    <td class="role-privs-check-cell">
                                                        <input type="checkbox"
                                                            prop:checked=move || privs_selected.with(|s| s.contains(&idx))
                                                            on:change=move |e| {
                                                                let on = event_target_checked(&e);
                                                                privs_selected.update(|s| {
                                                                    if on { s.insert(idx); }
                                                                    else  { s.remove(&idx); }
                                                                });
                                                            }/>
                                                    </td>
                                                    {row.into_iter().map(|c| view! {
                                                        <td>{c}</td>
                                                    }).collect_view()}
                                                </tr>
                                            }).collect_view()}
                                        </tbody>
                                    </table>
                                </div>
                            }.into_view()
                        }
                    };

                    // SQL emits the Kind column at position 1
                    // (Schema, Kind, Object). Capture so closures
                    // below can read it without re-deriving.
                    const AFFECTED_KIND_COL: usize = 1;
                    let affected_body = move || match affected_objects.get() {
                        LoadState::NotLoaded | LoadState::Loading => view! {
                            <div class="explorer-msg">"Resolving affected objects…"</div>
                        }.into_view(),
                        LoadState::Error(e) => view! {
                            <div class="explorer-error">{e}</div>
                        }.into_view(),
                        LoadState::Loaded(rows) if rows.is_empty() => view! {
                            <div class="explorer-msg">
                                "No objects match the selected grants. Pick at least one grant in the Grants sub-view."
                            </div>
                        }.into_view(),
                        LoadState::Loaded(rows) => {
                            let all_kinds: Vec<String> = {
                                let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
                                for r in &rows {
                                    if let Some(k) = r.get(AFFECTED_KIND_COL) { set.insert(k.clone()); }
                                }
                                set.into_iter().collect()
                            };
                            let is_active = move |k: &str| -> bool {
                                affected_kinds.with(|f| match f {
                                    None => true,
                                    Some(set) => set.contains(k),
                                })
                            };
                            let row_visible = move |row: &Vec<String>| -> bool {
                                match (row.get(AFFECTED_KIND_COL), affected_kinds.get()) {
                                    (Some(k), Some(set)) => set.contains(k),
                                    _ => true,
                                }
                            };
                            view! {
                                {if all_kinds.is_empty() {
                                    view! {}.into_view()
                                } else {
                                    let chips = all_kinds.into_iter().map(|kind| {
                                        let kind_for_check = kind.clone();
                                        let kind_for_click = kind.clone();
                                        view! {
                                            <button class="role-privs-kind-chip"
                                                class:role-privs-kind-chip-active=move || is_active(&kind_for_check)
                                                on:click=move |_| {
                                                    let k = kind_for_click.clone();
                                                    let rows_now = match affected_objects.get_untracked() {
                                                        LoadState::Loaded(rs) => rs,
                                                        _ => Vec::new(),
                                                    };
                                                    affected_kinds.update(|f| {
                                                        match f {
                                                            None => {
                                                                let mut set = std::collections::HashSet::new();
                                                                for r in &rows_now {
                                                                    if let Some(rk) = r.get(AFFECTED_KIND_COL) { set.insert(rk.clone()); }
                                                                }
                                                                set.remove(&k);
                                                                *f = Some(set);
                                                            }
                                                            Some(set) => {
                                                                if set.contains(&k) { set.remove(&k); }
                                                                else                { set.insert(k); }
                                                            }
                                                        }
                                                    });
                                                }>
                                                {kind}
                                            </button>
                                        }
                                    }).collect_view();
                                    view! {
                                        <div class="role-privs-kind-strip"
                                             title="Filter the resolved object list by kind.">
                                            {chips}
                                        </div>
                                    }.into_view()
                                }}
                                <div class="explorer-list-scroll">
                                    <table class="explorer-list role-affected-objects-list">
                                        <thead>
                                            <tr>
                                                <th>"Schema"</th>
                                                <th>"Kind"</th>
                                                <th>"Object"</th>
                                            </tr>
                                        </thead>
                                        <tbody>
                                            {rows.into_iter().filter(|r| row_visible(r)).map(|row| view! {
                                                <tr>
                                                    {row.into_iter().map(|c| view! {
                                                        <td>{c}</td>
                                                    }).collect_view()}
                                                </tr>
                                            }).collect_view()}
                                        </tbody>
                                    </table>
                                </div>
                            }.into_view()
                        }
                    };

                    let body = view! {
                        {strip}
                        {move || match privs_view.get() {
                            RolePrivsView::Grants => grants_body(),
                            RolePrivsView::AffectedObjects => affected_body(),
                        }}
                    }.into_view();
                    if is_superuser {
                        view! {
                            <div class="role-super-banner">
                                "Superuser — bypasses all privilege checks. The table below lists \
                                 explicit grants and ownership; effective access is broader."
                            </div>
                            {body}
                        }.into_view()
                    } else { body }
                }
            }}
        </div>
    }.into_view()
}

/// Partition-children count above which a partitioned parent defaults to
/// collapsed in the outer object list. ≤ this many → always render the
/// children inline (no chevron needed); > this many → start collapsed
/// behind a `▶` toggle that the user can expand.
pub(crate) const OUTER_PARTITION_THRESHOLD: usize = 6;

/// Comparator used by the outer object list's client-side sort. `col_label`
/// is the visible header text ("Name" / "Engine" / "Rows" / "Updated" /
/// "Comment"); `kind` is used to resolve non-Name labels to the right
/// index in `ObjectRow.extras`.
///
/// Two invariants that don't compose with a simple `if is_desc { rev }`:
///
/// - **Blanks always land at the end** regardless of direction (the DBA
///   sorting by Rows Desc doesn't want a wall of blank rows at the top).
/// - **`is_desc` reverses only the value comparison**, never the blank
///   ordering. So this fn handles both concerns in-line rather than
///   deferring the reverse to the caller.
fn compare_by_column(
    a: &ObjectRow,
    b: &ObjectRow,
    col_label: &str,
    kind: ObjectKind,
    is_desc: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // Resolve the header label to the two string values being compared.
    let (av, bv) = if col_label == "Name" {
        (a.name.as_str(), b.name.as_str())
    } else {
        let idx = kind.extra_columns().iter().position(|c| *c == col_label);
        match idx {
            Some(i) => (
                a.extras.get(i).map(String::as_str).unwrap_or(""),
                b.extras.get(i).map(String::as_str).unwrap_or(""),
            ),
            None => return Ordering::Equal,
        }
    };
    // Blanks last, both directions.
    match (av.is_empty(), bv.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    let value_ord = match col_label {
        "Rows" => match (av.parse::<i64>(), bv.parse::<i64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => natural_cmp(av, bv),
        },
        // ISO 8601 (`YYYY-MM-DD HH:MM`) sorts correctly as string.
        "Updated" => av.cmp(bv),
        // Name / Engine / Comment: natural sort matches the partition-
        // child ordering (case-insensitive alpha + numeric-suffix aware).
        _ => natural_cmp(av, bv),
    };
    if is_desc { value_ord.reverse() } else { value_ord }
}

/// Snapshot `.explorer-list-scroll`'s `scrollTop` before a mutation that
/// invalidates the surrounding reactive block, then restore it on the
/// next animation frame — Leptos has flushed the new DOM by then but
/// the browser hasn't painted yet, so the user never sees the jump-to-
/// top. Used by the (N partitions) toggle and its context menu, both of
/// which flip `explorer_expanded_parents` and rebuild the outer list's
/// scroll container.
fn preserve_explorer_list_scroll<F: FnOnce()>(mutate: F) {
    use wasm_bindgen::JsCast;
    let prev_scroll = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector(".explorer-list-scroll").ok().flatten())
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
        .map(|el| el.scroll_top());
    mutate();
    if let Some(top) = prev_scroll {
        let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
            if let Some(el) = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.query_selector(".explorer-list-scroll").ok().flatten())
            {
                if let Ok(html) = el.dyn_into::<web_sys::HtmlElement>() {
                    html.set_scroll_top(top);
                }
            }
        });
        if let Some(win) = web_sys::window() {
            let _ = win.request_animation_frame(cb.as_ref().unchecked_ref());
        }
    }
}

/// Bundle of state a partitioned-parent row needs to render its clickable
/// `(N partitions)` count: the membership signal it mutates on click, the
/// context-menu signal it sets on right-click, this parent's key, and the
/// current is-expanded flag (used to label the verb on hover / in the
/// context menu).
#[derive(Clone)]
struct PartitionParentInfo {
    is_expanded:    bool,
    child_count:    usize,
    parent_key:     (String, String),
    expanded_state: RwSignal<std::collections::HashSet<(String, String)>>,
    menu_state:     RwSignal<Option<PartitionMenuAnchor>>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_object_list(
    plane:       &'static dyn DbmsPlane,
    kind:        ObjectKind,
    listing:     LoadState<Vec<ObjectRow>>,
    search:      String,
    all_schemas: bool,
    selected:    RwSignal<Option<(String, String)>>,
    cursor_key:  RwSignal<Option<(String, String)>>,
    detail_visible: RwSignal<bool>,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    ddl_cache:   RwSignal<HashMap<(ObjectKind, String, String), LoadState<String>>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
    expanded_parents: RwSignal<std::collections::HashSet<(String, String)>>,
    partition_menu:   RwSignal<Option<PartitionMenuAnchor>>,
    sort_by_kind:     RwSignal<HashMap<ObjectKind, (String, bool)>>,
) -> leptos::View {
    match listing {
        LoadState::NotLoaded => view! {
            <div class="explorer-msg">"Select a schema to begin."</div>
        }.into_view(),
        LoadState::Loading => view! {
            <div class="explorer-msg">"Loading…"</div>
        }.into_view(),
        LoadState::Error(e) => view! {
            <div class="explorer-error">{e}</div>
        }.into_view(),
        LoadState::Loaded(rows) => {
            let needle = search.trim().to_ascii_lowercase();
            let filtered: Vec<ObjectRow> = if needle.is_empty() {
                rows
            } else {
                rows.into_iter()
                    .filter(|r| r.name.to_ascii_lowercase().contains(&needle))
                    .collect()
            };
            if filtered.is_empty() {
                return view! {
                    <div class="explorer-msg">{format!("No {} match.", kind.label().to_lowercase())}</div>
                }.into_view();
            }
            let extras = kind.extra_columns();

            // Bucket rows: partition children into a parent-keyed map, the
            // rest as top-level. A child whose parent isn't in `filtered`
            // (e.g. live search hid the parent) gets surfaced as a top-
            // level row so it isn't lost from the list.
            let mut children_by_parent: std::collections::HashMap<(String, String), Vec<ObjectRow>>
                = std::collections::HashMap::new();
            let mut top_level: Vec<ObjectRow> = Vec::with_capacity(filtered.len());
            let top_level_keys: std::collections::HashSet<(String, String)> = filtered.iter()
                .filter(|r| r.parent_name.is_none())
                .map(|r| (r.schema.clone(), r.name.clone()))
                .collect();
            for row in filtered {
                if let (Some(ps), Some(pn)) = (row.parent_schema.clone(), row.parent_name.clone()) {
                    let key = (ps, pn);
                    if top_level_keys.contains(&key) {
                        children_by_parent.entry(key).or_default().push(row);
                    } else {
                        // Orphan child: parent filtered out — surface as top-level.
                        top_level.push(row);
                    }
                } else {
                    top_level.push(row);
                }
            }
            // Subscribe to the expanded-parents signal so the closure re-runs
            // when the user clicks the `(N partitions)` toggle. Read once and
            // pass the snapshot down to child closures.
            let expanded_now = expanded_parents.get();

            // Client-side sort: reads the per-kind sort spec (if any) and
            // re-orders `top_level`. Partition children stay put — they
            // remain natural-sorted under their parent regardless of the
            // top-level ordering, so the tree indent isn't disrupted.
            let current_sort = sort_by_kind.get().get(&kind).cloned();
            if let Some((ref col_label, is_desc)) = current_sort {
                top_level.sort_by(|a, b| compare_by_column(a, b, col_label, kind, is_desc));
            }

            let sortable_cols: Vec<&'static str> = std::iter::once("Name")
                .chain(extras.iter().copied())
                .collect();

            view! {
                <div class="explorer-list-scroll">
                    <table class="explorer-list">
                        <thead>
                            <tr>
                                {if all_schemas {
                                    view! { <th class="ex-th-schema">"Schema"</th> }.into_view()
                                } else {
                                    view! { <th class="ex-th-hidden"></th> }.into_view()
                                }}
                                {sortable_cols.into_iter().enumerate().map(|(i, label)| {
                                    let is_name = i == 0;
                                    let th_class = if is_name { "ex-th-name ex-th-sortable" } else { "ex-th-sortable" };
                                    let label_owned  = label.to_string();
                                    let label_click  = label_owned.clone();
                                    let label_arrow  = label_owned.clone();
                                    let kind_click   = kind;
                                    // Arrow follows the currently-sorted column;
                                    // reused .data-sort-arrow class established
                                    // by the Data-tab column-header sort.
                                    let arrow = move || {
                                        sort_by_kind.with(|m| m.get(&kind_click).and_then(|(c, d)| {
                                            if c == &label_arrow { Some(if *d { " \u{2193}" } else { " \u{2191}" }) } else { None }
                                        })).map(|arr| view! {
                                            <span class="data-sort-arrow">{arr}</span>
                                        }.into_view())
                                    };
                                    view! {
                                        <th class=th_class
                                            on:click=move |_| {
                                                let same = sort_by_kind.with(|m|
                                                    m.get(&kind_click).is_some_and(|(c, _)| c == &label_click)
                                                );
                                                sort_by_kind.update(|m| {
                                                    if same {
                                                        if let Some((_, d)) = m.get_mut(&kind_click) {
                                                            *d = !*d;
                                                        }
                                                    } else {
                                                        m.insert(kind_click, (label_click.clone(), false));
                                                    }
                                                });
                                            }>
                                            {label}
                                            {arrow}
                                        </th>
                                    }
                                }).collect_view()}
                            </tr>
                        </thead>
                        <tbody>
                            {top_level.into_iter().map(|row| {
                                let row_schema = row.schema.clone();
                                let row_name   = row.name.clone();
                                let key = (row_schema.clone(), row_name.clone());
                                let mut children = children_by_parent.remove(&key).unwrap_or_default();
                                // Partition children honour whichever sort
                                // the user picked at the top-level, so
                                // sorting by Rows Desc doesn't leave the
                                // children stuck in name order under a
                                // parent that just moved to a Rows-based
                                // position. When no top-level sort is
                                // active, natural-sort by name is the
                                // fallback (`part_2` before `part_10`).
                                match &current_sort {
                                    Some((col_label, is_desc)) => children.sort_by(|a, b| {
                                        compare_by_column(a, b, col_label, kind, *is_desc)
                                    }),
                                    None => children.sort_by(|a, b| natural_cmp(&a.name, &b.name)),
                                }
                                let child_count = children.len();
                                let has_children = child_count > 0;
                                // The HashSet stores parents the user has
                                // *toggled away from default*. Default is
                                // expanded for ≤ threshold children, collapsed
                                // above. Set membership XORs that default so
                                // clicking the count flips it either direction.
                                let default_expanded = child_count <= OUTER_PARTITION_THRESHOLD;
                                let toggled = expanded_now.contains(&key);
                                let is_expanded = has_children && (default_expanded ^ toggled);

                                let parent_info = if has_children {
                                    Some(PartitionParentInfo {
                                        is_expanded,
                                        child_count,
                                        parent_key:     key.clone(),
                                        expanded_state: expanded_parents,
                                        menu_state:     partition_menu,
                                    })
                                } else {
                                    None
                                };

                                let parent_row = render_outer_row(
                                    plane, kind, row, all_schemas,
                                    selected, cursor_key, detail_visible,
                                    table_cache, ddl_cache, ws, session,
                                    /* is_partition_child= */ false,
                                    parent_info,
                                );

                                let children_rows = if is_expanded && has_children {
                                    children.into_iter().map(|c| render_outer_row(
                                        plane, kind, c, all_schemas,
                                        selected, cursor_key, detail_visible,
                                        table_cache, ddl_cache, ws, session,
                                        /* is_partition_child= */ true,
                                        /* parent_info= */ None,
                                    )).collect_view().into_view()
                                } else {
                                    view! {}.into_view()
                                };

                                view! {
                                    <>
                                        {parent_row}
                                        {children_rows}
                                    </>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                    {render_partition_context_menu(partition_menu, expanded_parents)}
                </div>
            }.into_view()
        }
    }
}

/// Render the floating context menu that appears on right-click of the
/// `(N partitions)` count. Reuses the `.tab-context-menu*` chrome already
/// used by the sessions Copy-query menu. The single verb mirrors the
/// left-click hotspot — its text comes from the `is_expanded` flag
/// captured at right-click time, and clicking it fires the same set-
/// membership flip.
fn render_partition_context_menu(
    partition_menu:   RwSignal<Option<PartitionMenuAnchor>>,
    expanded_parents: RwSignal<std::collections::HashSet<(String, String)>>,
) -> leptos::View {
    view! {
        {move || partition_menu.get().map(|(x, y, parent_key, was_expanded)| {
            let label = if was_expanded { "Collapse partitions" } else { "Expand partitions" };
            let parent_key_for_click = parent_key.clone();
            view! {
                <div class="tab-context-menu-backdrop"
                     on:mousedown=move |_| partition_menu.set(None)
                     on:contextmenu=move |e: web_sys::MouseEvent| {
                         e.prevent_default();
                         partition_menu.set(None);
                     } />
                <div class="tab-context-menu"
                     style=format!("left:{}px; top:{}px", x, y)
                     on:mousedown=move |e: web_sys::MouseEvent| {
                         use wasm_bindgen::JsCast;
                         let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                     }>
                    <div class="tab-context-menu-item"
                         on:click=move |_| {
                             let pk = parent_key_for_click.clone();
                             preserve_explorer_list_scroll(|| {
                                 expanded_parents.update(|set| {
                                     if !set.remove(&pk) { set.insert(pk); }
                                 });
                             });
                             partition_menu.set(None);
                         }>{label}</div>
                </div>
            }
        })}
    }.into_view()
}

/// Render one `<tr>` in the outer object list. Shared between top-level
/// rows (`is_partition_child = false`) and partition children
/// (`is_partition_child = true`, which adds the `.ex-td-name-partition`
/// indent class). When `parent_info` is Some, the row is a partitioned
/// parent: a clickable `(N partitions)` span is appended to the name
/// cell and serves as both the left-click toggle and the right-click
/// context-menu hotspot. No chevron — the project avoids dedicated
/// expansion widgets; the count IS the affordance.
#[allow(clippy::too_many_arguments)]
fn render_outer_row(
    plane:        &'static dyn DbmsPlane,
    kind:         ObjectKind,
    row:          ObjectRow,
    all_schemas:  bool,
    selected:     RwSignal<Option<(String, String)>>,
    cursor_key:   RwSignal<Option<(String, String)>>,
    detail_visible: RwSignal<bool>,
    table_cache:  RwSignal<HashMap<(String, String), TableDetail>>,
    ddl_cache:    RwSignal<HashMap<(ObjectKind, String, String), LoadState<String>>>,
    ws:           RwSignal<Option<WebSocket>>,
    session:   ExplorerSession,
    is_partition_child: bool,
    parent_info:  Option<PartitionParentInfo>,
) -> leptos::View {
    let row_schema = row.schema.clone();
    let row_name   = row.name.clone();
    let key = (row_schema.clone(), row_name.clone());
    let key_for_cls       = key.clone();
    let key_for_click_cur = key.clone();
    let extras_cells = row.extras.clone();

    let name_class = if is_partition_child {
        "ex-td-name ex-td-name-partition"
    } else {
        "ex-td-name"
    };

    // Partition-count hotspot. Always rendered when this row is a parent
    // with children; left-click toggles, right-click opens a menu with
    // the same verb. stop_propagation on both events keeps the row's own
    // click (which loads the detail inspector) from firing too.
    let partitions_hotspot = parent_info.map(|info| {
        let label = format!(" ({} partitions)", info.child_count);
        let click_key  = info.parent_key.clone();
        let ctx_key    = info.parent_key.clone();
        let expanded_state = info.expanded_state;
        let menu_state     = info.menu_state;
        let is_expanded    = info.is_expanded;
        let title = if is_expanded {
            "Collapse partitions (right-click for menu)"
        } else {
            "Expand partitions (right-click for menu)"
        };
        view! {
            <span class="ex-row-partition-count ex-row-partition-toggle"
                title=title
                on:click=move |ev: web_sys::MouseEvent| {
                    ev.stop_propagation();
                    let pk = click_key.clone();
                    preserve_explorer_list_scroll(|| {
                        expanded_state.update(|set| {
                            if !set.remove(&pk) { set.insert(pk); }
                        });
                    });
                }
                on:contextmenu=move |ev: web_sys::MouseEvent| {
                    ev.prevent_default();
                    ev.stop_propagation();
                    menu_state.set(Some((
                        ev.client_x() as f64,
                        ev.client_y() as f64,
                        ctx_key.clone(),
                        is_expanded,
                    )));
                }>
                {label}
            </span>
        }.into_view()
    });

    view! {
        <tr class="explorer-list-row"
            // Highlight follows the keyboard cursor (so arrow keys
            // visibly move the same yellow-blue highlight a click
            // produces). Click handlers below also write `selected`
            // so the inspector still loads on click.
            class:explorer-list-row-active=move || cursor_key.get().as_ref() == Some(&key_for_cls)
            attr:data-schema=row_schema.clone()
            attr:data-name=row_name.clone()
            on:click=move |_| {
                let sid = session.id.get_untracked();
                if kind == ObjectKind::Tables {
                    ensure_table_detail_loaded(plane, &row_schema, &row_name, table_cache, ws, session);
                } else if kind == ObjectKind::Views {
                    ensure_view_detail_loaded(plane, &row_schema, &row_name, table_cache, ws, sid);
                } else {
                    ensure_object_ddl_loaded(plane, kind, &row_schema, &row_name, ddl_cache, ws, sid);
                }
                selected.set(Some(key.clone()));
                cursor_key.set(Some(key_for_click_cur.clone()));
                detail_visible.set(true);
            }>
            {if all_schemas {
                view! { <td class="ex-td-schema">{row.schema}</td> }.into_view()
            } else {
                view! { <td class="ex-td-hidden"></td> }.into_view()
            }}
            <td class=name_class>
                {row.name}
                {partitions_hotspot}
            </td>
            {extras_cells.into_iter().enumerate().map(|(i, c)| {
                // Pretty-print numeric columns at render time only —
                // ObjectRow.extras stays as the raw SQL string so the
                // sort comparator's parse::<i64> still works. Currently
                // just "Rows" on Tables, but keyed by header label so
                // any future kind with a "Rows" extra picks it up.
                let display = match kind.extra_columns().get(i).copied().unwrap_or("") {
                    "Rows" => format_int_with_commas(&c),
                    _ => c,
                };
                view! { <td class="ex-td-extra">{display}</td> }
            }).collect_view()}
        </tr>
    }.into_view()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_inline_detail(
    kind:           ObjectKind,
    schema:         String,
    name:           String,
    explorer_pane_id: String,
    table_cache:    RwSignal<HashMap<(String, String), TableDetail>>,
    ddl_cache:      RwSignal<HashMap<(ObjectKind, String, String), LoadState<String>>>,
    plane:          &'static dyn DbmsPlane,
    selected:       RwSignal<Option<(String, String)>>,
    ws:             RwSignal<Option<WebSocket>>,
    session:     ExplorerSession,
    shell_panes:    RwSignal<HashMap<String, ShellTab>>,
    pane_ops:       PaneOps,
    priv_cell_status:   RwSignal<HashMap<(String, String), EditStatus>>,
    priv_pending_revoke: RwSignal<Option<(String, String)>>,
) -> leptos::View {
    if kind == ObjectKind::Tables || kind == ObjectKind::Views {
        let detail = table_cache.with_untracked(|m| m.get(&(schema.clone(), name.clone())).copied());
        let Some(detail) = detail else {
            return view! { <div class="detail-msg">"Loading…"</div> }.into_view();
        };
        let active_tab = detail.active_tab;
        let tab_btn = move |label: &'static str, kind: DetailTab| -> leptos::View {
            view! {
                <button class="detail-tab-btn"
                    class:detail-tab-active=move || active_tab.get() == kind
                    on:click=move |_| {
                        active_tab.set(kind);
                        // Switching tabs exits any body cursor mode
                        // — otherwise returning to the prior tab
                        // silently re-arms its body cursor handler
                        // (Layer C / Layer C′) without the user ever
                        // descending. Cleared on every tab change so
                        // mouse and keyboard both leave clean state.
                        detail.data_selected_col.set(None);
                        detail.schema_selected_row.set(None);
                    }>
                    {label}
                </button>
            }.into_view()
        };
        let schema_for_tab = schema.clone();
        let name_for_tab   = name.clone();
        let explorer_pane_for_tab = explorer_pane_id.clone();
        // ERD tab is gated to base tables: views don't carry foreign-key
        // topology in the same sense, and the same gate already drives
        // their relationships row to render dashes (see ensure_view_detail_loaded).
        let show_erd_tab = kind == ObjectKind::Tables;
        // Storage tab is Tables-only. Same rationale as ERD / Data:
        // "storage geometry" doesn't apply to views, procedures, etc.
        let show_storage_tab = kind == ObjectKind::Tables;
        // Data tab gated the same way for v1: views select fine but the
        // Phase 4+ DML model assumes a writable base table, so showing
        // the tab on views would mis-set expectations for later phases.
        let show_data_tab = kind == ObjectKind::Tables;
        // Privileges tab visible whenever the plane returns a
        // non-empty privilege list for this kind. Tables / views /
        // routines all qualify today; sequences / matviews etc.
        // would join automatically once the planes implement them.
        let show_priv_tab = !plane.object_privilege_kinds(kind).is_empty();
        let schema_for_erd = schema.clone();
        let name_for_erd   = name.clone();
        let schema_for_data = schema.clone();
        let name_for_data   = name.clone();
        let schema_for_priv = schema.clone();
        let name_for_priv   = name.clone();
        // Tab order: Schema (logical shape) → Storage (physical shape:
        // pages / bloat / VM) → DDL → ERD → Data → Privileges. Storage
        // is Tables-only; other object kinds (views, procedures) skip it
        // just like they skip ERD / Data / Privileges. `show_storage_tab`
        // gates on `kind == ObjectKind::Tables` — mirrors show_erd_tab.
        return view! {
            <div class="detail-tabs">
                {tab_btn("Schema", DetailTab::Schema)}
                {if show_storage_tab { tab_btn("Storage", DetailTab::Storage) } else { view! { <span/> }.into_view() }}
                {tab_btn("DDL",    DetailTab::Ddl)}
                {if show_erd_tab  { tab_btn("ERD",  DetailTab::Erd)  } else { view! { <span/> }.into_view() }}
                {if show_data_tab { tab_btn("Data", DetailTab::Data) } else { view! { <span/> }.into_view() }}
                {if show_priv_tab { tab_btn("Privileges", DetailTab::Privileges) } else { view! { <span/> }.into_view() }}
            </div>
            {move || match active_tab.get() {
                DetailTab::Schema => render_schema_tab(
                    detail,
                    schema_for_tab.clone(),
                    name_for_tab.clone(),
                    plane, selected, table_cache, ws, session,
                ),
                DetailTab::Storage => render_storage_tab(
                    detail,
                    schema_for_tab.clone(),
                    name_for_tab.clone(),
                    explorer_pane_for_tab.clone(),
                    shell_panes,
                    pane_ops,
                ),
                DetailTab::Erd => render_erd_tab(
                    detail,
                    schema_for_erd.clone(),
                    name_for_erd.clone(),
                    plane, table_cache, selected, ws, session,
                ),
                DetailTab::Ddl  => render_ddl_tab(detail.ddl),
                DetailTab::Data => render_data_tab(
                    detail,
                    schema_for_data.clone(),
                    name_for_data.clone(),
                    plane, ws, session,
                ),
                DetailTab::Privileges => render_privileges_tab(
                    detail, kind,
                    schema_for_priv.clone(),
                    name_for_priv.clone(),
                    plane, ws, session,
                    priv_cell_status,
                    priv_pending_revoke,
                ),
            }}
        }.into_view();
    }

    let key = (kind, schema, name);
    view! {
        {move || match ddl_cache.with(|m| m.get(&key).cloned()) {
            None | Some(LoadState::NotLoaded) | Some(LoadState::Loading) => view! {
                <div class="detail-msg">"Loading…"</div>
            }.into_view(),
            Some(LoadState::Error(e)) => view! {
                <div class="detail-error">{e}</div>
            }.into_view(),
            Some(LoadState::Loaded(ddl)) => view! {
                <pre class="detail-ddl">{ddl}</pre>
            }.into_view(),
        }}
    }.into_view()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_schema_tab(
    detail:      TableDetail,
    this_schema: String,
    this_name:   String,
    plane:       &'static dyn DbmsPlane,
    selected:    RwSignal<Option<(String, String)>>,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
) -> leptos::View {
    let overview_sig = detail.overview;
    let columns_sig  = detail.columns;
    let indexes_sig  = detail.indexes;
    let ancestors    = detail.ancestors;
    let descendants  = detail.descendants;
    let partitions   = detail.partitions;
    let toast_sig    = detail.toast;
    let this_schema_for_header = this_schema.clone();
    let this_schema_for_parts  = this_schema.clone();
    let this_schema_for_rel    = this_schema.clone();
    view! {
        <div class="schema-scroll">
            /* "TOAST OF <parent>" back-link — rendered at the top of the
               Schema tab when this table IS a TOAST relation. Reads the
               same `toast` signal the section renderer uses; renders
               empty when the payload isn't `IsToastOf`. */
            {render_toast_of_header(
                toast_sig, plane, selected, table_cache, ws, session,
            )}
            {move || match overview_sig.get() {
                LoadState::NotLoaded | LoadState::Loading => view! {
                    <div class="detail-msg">"Loading overview…"</div>
                }.into_view(),
                LoadState::Error(e) => view! {
                    <div class="detail-error">{e}</div>
                }.into_view(),
                LoadState::Loaded(ov) => render_object_header(
                    ov,
                    this_schema_for_header.clone(),
                    plane, selected, table_cache, ws, session,
                ),
            }}

            <div class="schema-section-divider"></div>
            <div class="schema-section-header">"Columns"</div>

            {move || {
                let cols = match columns_sig.get() {
                    LoadState::NotLoaded | LoadState::Loading =>
                        return view! { <div class="detail-msg">"Loading columns…"</div> }.into_view(),
                    LoadState::Error(e) =>
                        return view! { <div class="detail-error">{e}</div> }.into_view(),
                    LoadState::Loaded(v) => v,
                };
                let idx_list: Vec<IndexInfo> = match indexes_sig.get() {
                    LoadState::Loaded(v) => v,
                    _ => Vec::new(),
                };
                let tags = compute_column_index_tags(&idx_list, &cols);
                let idx_tags = compute_index_tags(&idx_list);

                view! {
                    {render_column_rows(cols, tags, detail.schema_selected_row)}
                    <div class="schema-section-divider"></div>
                    <div class="schema-section-header">"Indexes"</div>
                    {match indexes_sig.get() {
                        LoadState::NotLoaded | LoadState::Loading => view! {
                            <div class="detail-msg">"Loading indexes…"</div>
                        }.into_view(),
                        LoadState::Error(e) => view! {
                            <div class="detail-error">{e}</div>
                        }.into_view(),
                        LoadState::Loaded(ix) if ix.is_empty() => view! {
                            <div class="detail-msg">"(no indexes)"</div>
                        }.into_view(),
                        LoadState::Loaded(ix) => render_index_rows(ix, idx_tags),
                    }}
                }.into_view()
            }}

            // Partitions section — only renders when the table has at least
            // one partition. A loaded-empty result hides the section entirely
            // (non-partitioned tables). Sits between Indexes and Relationships.
            {render_partitions_section(
                partitions, this_schema_for_parts,
                plane, selected, table_cache, ws, session,
            )}

            /* TOAST section — renders when the table has TOAST'd data (or,
               for partitioned parents, when leaves collectively do). One
               of three layouts: Single (this table's own TOAST), Partition
               Aggregate (parent's roll-up), IsToastOf (the back-link, but
               that path renders via the header at the top of the tab so
               the section body is empty here). Between Partitions and
               Relationships. */
            {render_toast_section(
                toast_sig, plane, selected, table_cache, ws, session,
            )}

            {render_relationships_row(
                ancestors, descendants,
                this_schema_for_rel, this_name,
                plane, selected, table_cache, ws, session,
                detail.relationships_expanded,
            )}
        </div>
    }.into_view()
}

/// Render the partitions section. Returns an empty view (no header, no
/// divider) when the table has no partitions, so non-partitioned tables
/// don't show a stub. Each row: clickable partition name (navigates to
/// that partition's detail when same-schema) + bound expression.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_partitions_section(
    partitions:  RwSignal<LoadState<Vec<PartitionInfo>>>,
    this_schema: String,
    plane:       &'static dyn DbmsPlane,
    selected:    RwSignal<Option<(String, String)>>,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
) -> leptos::View {
    view! {
        {move || match partitions.get() {
            // Hide entirely while loading — the rest of the schema tab
            // already renders, no need for a "Loading partitions…" stub.
            LoadState::NotLoaded | LoadState::Loading => view! { <div/> }.into_view(),
            LoadState::Error(e) => view! {
                <>
                    <div class="schema-section-divider"></div>
                    <div class="schema-section-header">"Partitions"</div>
                    <div class="detail-error">{e}</div>
                </>
            }.into_view(),
            LoadState::Loaded(parts) if parts.is_empty() =>
                view! { <div/> }.into_view(),
            LoadState::Loaded(parts) => {
                let this_schema = this_schema.clone();
                // PG declarative partitions are addressable as their own
                // tables — clicking navigates the inspector. MySQL
                // partitions are storage divisions inside one table; clicking
                // would dispatch to a non-existent table, so render plain.
                let navigable = plane.partitions_are_navigable();
                // Natural-sort so `part_2` sorts before `part_10` (the SQL
                // ORDER BY child_c.relname gives lexicographic order, which
                // is wrong for numerically-suffixed partition names).
                let mut parts = parts;
                parts.sort_by(|a, b| natural_cmp(&a.name, &b.name));
                view! {
                    <>
                        <div class="schema-section-divider"></div>
                        <div class="schema-section-header">"Partitions"</div>
                        <table class="schema-table partitions-table">
                            <thead>
                                <tr>
                                    <th class="schema-th-name">"Name"</th>
                                    <th>"Bound"</th>
                                    <th>"Rows"</th>
                                    <th>"Data"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {parts.into_iter().map(|p| {
                                    let same_schema = p.schema == this_schema;
                                    let key       = (p.schema.clone(), p.name.clone());
                                    let key_click = key.clone();
                                    let s_click   = p.schema.clone();
                                    let n_click   = p.name.clone();
                                    // Plain name for non-navigable (MySQL) — schema
                                    // qualifier would be redundant since the partition
                                    // lives in the same table's schema.
                                    let display_name = if !navigable || same_schema {
                                        p.name.clone()
                                    } else {
                                        format!("{}.{}", p.schema, p.name)
                                    };
                                    view! {
                                        <tr>
                                            <td class="schema-td-name">
                                                {if navigable && same_schema {
                                                    view! {
                                                        <span class="rel-link"
                                                              on:click=move |_| {
                                                                  ensure_table_detail_loaded(plane, &s_click, &n_click, table_cache, ws, session);
                                                                  selected.set(Some(key_click.clone()));
                                                              }>
                                                            {display_name}
                                                        </span>
                                                    }.into_view()
                                                } else if navigable {
                                                    view! { <span class="rel-cross">{display_name}</span> }.into_view()
                                                } else {
                                                    view! { <span class="rel-plain">{display_name}</span> }.into_view()
                                                }}
                                            </td>
                                            <td class="schema-td-default">{p.bound}</td>
                                            <td class="schema-td-size">{p.rows}</td>
                                            <td class="schema-td-size">{p.data_size}</td>
                                        </tr>
                                    }
                                }).collect_view()}
                            </tbody>
                        </table>
                    </>
                }.into_view()
            }
        }}
    }.into_view()
}

/// TOAST section — three layouts (Single / PartitionAggregate /
/// IsToastOf) plus a TOASTABLE COLUMNS sub-table. Returns empty when
/// the payload is None (no TOAST anywhere), still loading, or IsToastOf
/// (that variant renders via the top-of-tab `render_toast_of_header`
/// instead so the back-link sits above Overview). PG-only in practice;
/// MySQL emits an empty result set from the plane and the section stays
/// hidden.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_toast_section(
    toast_sig:   RwSignal<LoadState<Option<ToastDetail>>>,
    plane:       &'static dyn DbmsPlane,
    selected:    RwSignal<Option<(String, String)>>,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
) -> leptos::View {
    view! {
        {move || match toast_sig.get() {
            // Loading / error / no-TOAST all hide the section entirely.
            LoadState::NotLoaded | LoadState::Loading => view! { <div/> }.into_view(),
            LoadState::Error(e) => view! {
                <>
                    <div class="schema-section-divider"></div>
                    <div class="schema-section-header">"TOAST"</div>
                    <div class="detail-error">{e}</div>
                </>
            }.into_view(),
            LoadState::Loaded(None) => view! { <div/> }.into_view(),
            LoadState::Loaded(Some(td)) => {
                let columns_view = if td.columns.is_empty() {
                    view! { <div/> }.into_view()
                } else {
                    view! {
                        <>
                            <div class="schema-section-divider"></div>
                            <div class="schema-section-header">"Toastable columns"</div>
                            <table class="schema-table">
                                <thead>
                                    <tr>
                                        <th class="schema-th-name">"Column"</th>
                                        <th>"Storage"</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {td.columns.into_iter().map(|c| view! {
                                        <tr>
                                            <td class="schema-td-name">{c.name}</td>
                                            <td class="schema-td-default">{c.storage}</td>
                                        </tr>
                                    }).collect_view()}
                                </tbody>
                            </table>
                        </>
                    }.into_view()
                };

                let summary_view = match td.kind {
                    // IsToastOf renders at the top of the schema tab,
                    // not here. Skip the section body.
                    ToastKind::IsToastOf { .. } => return view! { <div/> }.into_view(),
                    ToastKind::Single {
                        toast_schema, toast_name,
                        heap_size, index_size, rows_est, storage_ratio,
                    } => {
                        let s_click = toast_schema.clone();
                        let n_click = toast_name.clone();
                        let key_click = (toast_schema.clone(), toast_name.clone());
                        view! {
                            <table class="schema-table">
                                <tbody>
                                    <tr>
                                        <td class="schema-td-name">"Table name"</td>
                                        <td class="schema-td-default">
                                            <span class="rel-link"
                                                title=format!("{}.{}", toast_schema, toast_name)
                                                on:click=move |_| {
                                                    ensure_table_detail_loaded(
                                                        plane, &s_click, &n_click,
                                                        table_cache, ws, session,
                                                    );
                                                    selected.set(Some(key_click.clone()));
                                                }>
                                                {toast_name}
                                            </span>
                                        </td>
                                    </tr>
                                    <tr>
                                        <td class="schema-td-name">"Heap size"</td>
                                        <td class="schema-td-default">{heap_size}</td>
                                    </tr>
                                    <tr>
                                        <td class="schema-td-name">"Index size"</td>
                                        <td class="schema-td-default">{index_size}</td>
                                    </tr>
                                    <tr>
                                        <td class="schema-td-name">"Rows (est.)"</td>
                                        <td class="schema-td-default">{rows_est}</td>
                                    </tr>
                                    <tr>
                                        <td class="schema-td-name">"Storage ratio"</td>
                                        <td class="schema-td-default">{storage_ratio}</td>
                                    </tr>
                                </tbody>
                            </table>
                        }.into_view()
                    }
                    ToastKind::PartitionAggregate {
                        partition_count, total_toast,
                        largest_schema, largest_name, largest_size,
                    } => {
                        let s_click = largest_schema.clone();
                        let n_click = largest_name.clone();
                        let key_click = (largest_schema.clone(), largest_name.clone());
                        let name_for_display = largest_name.clone();
                        view! {
                            <table class="schema-table">
                                <tbody>
                                    <tr>
                                        <td class="schema-td-name">"Partitions"</td>
                                        <td class="schema-td-default">
                                            {partition_count.to_string()}
                                        </td>
                                    </tr>
                                    <tr>
                                        <td class="schema-td-name">"Total TOAST"</td>
                                        <td class="schema-td-default">{total_toast}</td>
                                    </tr>
                                    <tr>
                                        <td class="schema-td-name">"Largest TOAST"</td>
                                        <td class="schema-td-default">
                                            <span class="rel-link"
                                                title=format!("{}.{}", largest_schema, largest_name)
                                                on:click=move |_| {
                                                    ensure_table_detail_loaded(
                                                        plane, &s_click, &n_click,
                                                        table_cache, ws, session,
                                                    );
                                                    selected.set(Some(key_click.clone()));
                                                }>
                                                {name_for_display}
                                            </span>
                                            {format!(" ({})", largest_size)}
                                        </td>
                                    </tr>
                                </tbody>
                            </table>
                        }.into_view()
                    }
                };

                view! {
                    <>
                        <div class="schema-section-divider"></div>
                        <div class="schema-section-header">"TOAST"</div>
                        {summary_view}
                        {columns_view}
                    </>
                }.into_view()
            }
        }}
    }.into_view()
}

/// "TOAST OF <parent>" back-link rendered at the top of the Schema tab
/// when the table IS a TOAST relation. Empty view otherwise.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_toast_of_header(
    toast_sig:   RwSignal<LoadState<Option<ToastDetail>>>,
    plane:       &'static dyn DbmsPlane,
    selected:    RwSignal<Option<(String, String)>>,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
) -> leptos::View {
    view! {
        {move || {
            let payload = match toast_sig.get() {
                LoadState::Loaded(Some(td)) => td,
                _ => return view! { <div/> }.into_view(),
            };
            match payload.kind {
                ToastKind::IsToastOf { parent_schema, parent_name } => {
                    let s_click = parent_schema.clone();
                    let n_click = parent_name.clone();
                    let key_click = (parent_schema.clone(), parent_name.clone());
                    let name_for_display = parent_name.clone();
                    view! {
                        <div class="schema-section-header">
                            {"TOAST OF"}
                            <span style="margin-left:8px">
                                <span class="rel-link"
                                    title=format!("{}.{}", parent_schema, parent_name)
                                    on:click=move |_| {
                                        ensure_table_detail_loaded(
                                            plane, &s_click, &n_click,
                                            table_cache, ws, session,
                                        );
                                        selected.set(Some(key_click.clone()));
                                    }>
                                    {name_for_display}
                                </span>
                            </span>
                        </div>
                    }.into_view()
                }
                _ => view! { <div/> }.into_view(),
            }
        }}
    }.into_view()
}

/// When any band of the Relationships row has more than this many entries
/// an "Expand" toggle surfaces in the section header. Below the threshold
/// the row keeps its default 160px per-column scroll cap with no toggle.
pub(crate) const RELATIONSHIPS_OVERFLOW_THRESHOLD: usize = 5;

/// Render the 5-column relationships row:
///   [Grand-parents] [Parents] [THIS] [Children] [Grand-children]
/// Position implies role — no column labels, no arrow separators. Same-schema
/// neighbour cells are clickable and navigate the inspector to that table;
/// cross-schema cells render as plain `schema.table`. The middle THIS cell
/// is rendered the same as a same-schema neighbour but without a click.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_relationships_row(
    ancestors:   RwSignal<LoadState<Vec<RelEdge>>>,
    descendants: RwSignal<LoadState<Vec<RelEdge>>>,
    this_schema: String,
    this_name:   String,
    plane:       &'static dyn DbmsPlane,
    selected:    RwSignal<Option<(String, String)>>,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
    expanded:    RwSignal<bool>,
) -> leptos::View {
    view! {
        {move || {
            let anc = ancestors.get();
            let des = descendants.get();
            // Surface error from either direction; otherwise show a single
            // "Loading…" until both finish.
            if let LoadState::Error(e) = &anc {
                return view! {
                    <>
                        <div class="schema-section-divider"></div>
                        <div class="schema-section-header"><span>"Relationships"</span></div>
                        <div class="detail-error">{e.clone()}</div>
                    </>
                }.into_view();
            }
            if let LoadState::Error(e) = &des {
                return view! {
                    <>
                        <div class="schema-section-divider"></div>
                        <div class="schema-section-header"><span>"Relationships"</span></div>
                        <div class="detail-error">{e.clone()}</div>
                    </>
                }.into_view();
            }
            let (a_rows, d_rows) = match (anc, des) {
                (LoadState::Loaded(a), LoadState::Loaded(d)) => (a, d),
                _ => return view! {
                    <>
                        <div class="schema-section-divider"></div>
                        <div class="schema-section-header"><span>"Relationships"</span></div>
                        <div class="detail-msg">"Loading relationships…"</div>
                    </>
                }.into_view(),
            };
            let mut grand_parents:  Vec<RelEdge> = a_rows.iter().filter(|e| e.depth == 2).cloned().collect();
            let mut parents:        Vec<RelEdge> = a_rows.into_iter().filter(|e| e.depth == 1).collect();
            let mut children:       Vec<RelEdge> = d_rows.iter().filter(|e| e.depth == 1).cloned().collect();
            let mut grand_children: Vec<RelEdge> = d_rows.into_iter().filter(|e| e.depth == 2).collect();
            // Natural-sort each band by target table name so `part_2` precedes
            // `part_10` and the DBA sees a predictable order rather than the
            // FK-constraint creation order the SQL hands back. Same idiom as
            // the partition lists.
            grand_parents.sort_by(|a, b|  natural_cmp(&a.table, &b.table));
            parents.sort_by(|a, b|        natural_cmp(&a.table, &b.table));
            children.sort_by(|a, b|       natural_cmp(&a.table, &b.table));
            grand_children.sort_by(|a, b| natural_cmp(&a.table, &b.table));

            let any_overflows =
                grand_parents.len()  > RELATIONSHIPS_OVERFLOW_THRESHOLD ||
                parents.len()        > RELATIONSHIPS_OVERFLOW_THRESHOLD ||
                children.len()       > RELATIONSHIPS_OVERFLOW_THRESHOLD ||
                grand_children.len() > RELATIONSHIPS_OVERFLOW_THRESHOLD;

            let this_schema_a = this_schema.clone();
            let this_schema_b = this_schema.clone();
            let this_schema_d = this_schema.clone();
            let this_schema_e = this_schema.clone();

            view! {
                <>
                    <div class="schema-section-divider"></div>
                    <div class="schema-section-header">
                        <span>"Relationships"</span>
                        {if any_overflows {
                            view! {
                                <button class="explorer-inspector-expand"
                                    title=move || if expanded.get() { "Collapse — shrink each column's scroll area" }
                                                  else              { "Expand — grow each column's scroll area" }
                                    on:click=move |_| { expanded.update(|v| *v = !*v); }>
                                    {move || if expanded.get() { "⤡" } else { "⤢" }}
                                </button>
                            }.into_view()
                        } else {
                            view! {}.into_view()
                        }}
                    </div>
                    <div class="rel-row">
                        {render_rel_col(grand_parents, this_schema_a,
                            plane, selected, table_cache, ws, session, expanded)}
                        {render_rel_col(parents, this_schema_b,
                            plane, selected, table_cache, ws, session, expanded)}
                        <div class="rel-col">
                            <div class="rel-this" title=this_name.clone()>{this_name.clone()}</div>
                        </div>
                        {render_rel_col(children, this_schema_d,
                            plane, selected, table_cache, ws, session, expanded)}
                        {render_rel_col(grand_children, this_schema_e,
                            plane, selected, table_cache, ws, session, expanded)}
                    </div>
                </>
            }.into_view()
        }}
    }.into_view()
}

#[allow(clippy::too_many_arguments)]
fn render_rel_col(
    edges:       Vec<RelEdge>,
    this_schema: String,
    plane:       &'static dyn DbmsPlane,
    selected:    RwSignal<Option<(String, String)>>,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
    expanded:    RwSignal<bool>,
) -> leptos::View {
    let body: leptos::View = if edges.is_empty() {
        view! { <div class="rel-empty">"—"</div> }.into_view()
    } else {
        edges.into_iter().map(|edge| {
            let same_schema = edge.schema == this_schema;
            if same_schema {
                let key       = (edge.schema.clone(), edge.table.clone());
                let key_click = key.clone();
                let s_click   = edge.schema.clone();
                let t_click   = edge.table.clone();
                // `title` reveals the full identifier on hover when the
                // 160px column ellipsizes a long name. Same idiom as the
                // inspector-expand button.
                let title_text = edge.table.clone();
                view! {
                    <div class="rel-link"
                         title=title_text
                         on:click=move |_| {
                             ensure_table_detail_loaded(plane, &s_click, &t_click, table_cache, ws, session);
                             selected.set(Some(key_click.clone()));
                         }>
                        {edge.table}
                    </div>
                }.into_view()
            } else {
                let qualified = format!("{}.{}", edge.schema, edge.table);
                let title_text = qualified.clone();
                view! {
                    <div class="rel-cross" title=title_text>{qualified}</div>
                }.into_view()
            }
        }).collect_view()
    };
    view! {
        <div class="rel-col" class:rel-col-expanded=move || expanded.get()>
            {body}
        </div>
    }.into_view()
}

/// One cell in the `object-meta-grid`. `Simple` is the long-standing
/// "label + plain value" shape (the prior `(&str, String)` tuple).
/// `Severity` carries a severity band + tooltip text so the storage-
/// factor cell can colour the value and surface a breakdown on hover.
/// Push helpers below drop empty values for both variants — the grid
/// only shows cells with content.
enum MetaCell {
    Simple   { key: &'static str, value: String },
    Severity { key: &'static str, value: String, sev: Severity, title: String },
}

/// One key/value cell of an `.object-meta-grid` (the table header's
/// at-a-glance grid). Severity cells get the metric-good/watch/bad
/// class and their tooltip; Neutral gets no colour class (tooltip-only
/// cells like CRUD, where the value has no good-vs-bad direction).
/// Plain values carry themselves as `title`, so one clipped with "…"
/// by the grid's text-overflow can still be read on hover.
fn meta_cell_view(cell: MetaCell) -> leptos::View {
    match cell {
        MetaCell::Simple { key, value } => view! {
            <div class="object-meta-cell">
                <span class="object-meta-key">{key}</span>
                <span class="object-meta-val" title=value.clone()>{value}</span>
            </div>
        }.into_view(),
        MetaCell::Severity { key, value, sev, title } => view! {
            <div class="object-meta-cell">
                <span class="object-meta-key">{key}</span>
                <span class=format!("object-meta-val {}", metric_class(sev)) title=title>{value}</span>
            </div>
        }.into_view(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_object_header(
    ov:          TableOverview,
    this_schema: String,
    plane:       &'static dyn DbmsPlane,
    selected:    RwSignal<Option<(String, String)>>,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
) -> leptos::View {
    let mut pairs: Vec<MetaCell> = Vec::new();
    let push = |pairs: &mut Vec<MetaCell>, k: &'static str, v: String| {
        if !v.is_empty() { pairs.push(MetaCell::Simple { key: k, value: v }); }
    };
    // Ordering matches the DBA's mental walk-down:
    //   identity     → Engine, Owner
    //   volume       → Rows (est.), Data size, Index size, TOAST
    //   runtime      → Cache hit, Block IO, Index access, CRUD
    //   maintenance  → Last vacuum, Last analyze, Freeze age
    // The header answers "is this table healthy right now?" — a
    // at-a-glance runtime picture. Deeper "storage geometry" cells
    // (Storage factor, Pages / Ideal, Avg row, Dead tuples %, Stale
    // stats %, MultiXact age) moved to the Storage tab where they
    // have room to breathe alongside the visibility-map section.
    // Rest (Collation / Auto-inc / Partitions / Created / Updated /
    // Comment) tail after so nothing is lost. Each push is guarded
    // by non-empty via the `push` helper.
    push(&mut pairs, "Engine",         ov.engine.clone());
    // Owner is PG-only. `Option<String>` distinguishes the two
    // semantically-distinct "no Owner shown" cases:
    //   None        → plane doesn't emit the column (MySQL — table-
    //                 level ownership isn't a MySQL concept).
    //   Some("")    → plane emitted Owner but the row resolved
    //                 blank (rare; the `pg_get_userbyid` lookup
    //                 found no role for the relowner OID).
    //   Some(name)  → render.
    // Display behavior is identical for None / Some("") — the row
    // simply doesn't show — but the type-level distinction lets a
    // future UX layer choose to surface "(not applicable on MySQL)"
    // explicitly. See `TableOverview` doc comments + audit cluster D
    // finding 3.3.
    if let Some(owner) = ov.owner.as_deref().filter(|s| !s.is_empty()) {
        push(&mut pairs, "Owner", owner.to_string());
    }
    push(&mut pairs, "Rows (est.)",    ov.row_count.clone());
    push(&mut pairs, "Data size",      ov.data_size.clone());
    push(&mut pairs, "Index size",     ov.index_size.clone());
    // TOAST — out-of-line row storage. Sits with Data size / Index
    // size since together they answer "where does this table's disk
    // footprint live." Value is a pre-composed `"1.8 GB (42%)"` from
    // the parser. Neutral cell (no severity); PG-only; suppressed on
    // tables with no TOAST relation and on partitioned parents.
    if let Some(t) = ov.toast_display.as_deref().filter(|s| !s.is_empty()) {
        push(&mut pairs, "TOAST", t.to_string());
    }
    // Cache hit — buffer-cache effectiveness (PG-only). ≥99% green /
    // 95-99% yellow / <95% red. Leads the runtime band since it's the
    // "is this table hot?" question the DBA reads first.
    if let (Some(value), Some(sev), Some(title)) = (
        ov.cache_hit_ratio.clone(),
        ov.cache_hit_severity,
        ov.cache_hit_tooltip.clone(),
    ) {
        if !value.is_empty() {
            pairs.push(MetaCell::Severity { key: "Cache hit", value, sev, title });
        }
    }
    // Block IO — magnitude companion to Cache hit ratio. Cache hit
    // tells you efficiency ("99.7%"); Block IO tells you scale
    // ("1.2G hit / 4.5M read"). Together they read as one thought:
    // is the cache doing meaningful work, and how much? Neutral
    // cell — magnitudes aren't good-vs-bad on their own; tooltip
    // breaks heap vs index apart for follow-up.
    if let (Some(value), Some(title)) = (
        ov.block_io_display.clone(),
        ov.block_io_tooltip.clone(),
    ) {
        if !value.is_empty() {
            pairs.push(MetaCell::Severity {
                key: "Block IO",
                value,
                sev: Severity::Neutral,
                title,
            });
        }
    }
    // Index access % — fraction of executor access-method choices on
    // this table where the planner picked an index (Index / Index Only
    // / Bitmap Index Scan) over a full sequential scan. Not "% of
    // queries that used an index" — counts planner picks, not queries.
    // ≥80% green / 20-80% yellow / <20% red. PG-only. Tooltip spells
    // out the subtlety.
    if let (Some(value), Some(sev), Some(title)) = (
        ov.idx_scan_pct.clone(),
        ov.idx_scan_severity,
        ov.idx_scan_tooltip.clone(),
    ) {
        if !value.is_empty() {
            pairs.push(MetaCell::Severity { key: "Index access %", value, sev, title });
        }
    }
    // CRUD — thousands-comma "C / R / U / D" volume from
    // pg_stat_user_tables. Answers "how much work has this table
    // seen?" Neutral cell (no severity — churn depends on the
    // table's purpose). Tooltip spells out what each letter means
    // and reminds the reader that R is tuples-returned, not query
    // count. PG-only; suppressed when every counter is 0.
    if let (Some(value), Some(title)) = (
        ov.crud_display.clone(),
        ov.crud_tooltip.clone(),
    ) {
        if !value.is_empty() {
            // No severity — Simple cell with a hover-only title.
            // Reuse the Severity variant with Severity::Good and a
            // dedicated "neutral" class would work but adds a
            // codepath just for the tooltip. Push as Simple and
            // rely on the cell's native `title=` support — the
            // meta-grid render already threads titles for Simple
            // cells that come from severity-adjacent sources.
            pairs.push(MetaCell::Severity {
                key: "CRUD",
                value,
                sev: Severity::Neutral,
                title,
            });
        }
    }
    // Maintenance band — timestamp of the most recent VACUUM /
    // ANALYZE plus Freeze age. The forward-looking "is autovacuum
    // keeping up" detail lives on the Storage tab (Dead tuples % in
    // Geometry, per-trigger progress in Autovacuum); Freeze age stays
    // here because anti-wraparound is a whole-cluster emergency you
    // want visible from every table detail without opening a tab.
    if let Some(lv) = ov.last_vacuum.as_deref().filter(|s| !s.is_empty()) {
        push(&mut pairs, "Last vacuum", lv.to_string());
    }
    if let Some(la) = ov.last_analyze.as_deref().filter(|s| !s.is_empty()) {
        push(&mut pairs, "Last analyze", la.to_string());
    }
    // Freeze age — how close relfrozenxid is to
    // autovacuum_freeze_max_age. Anti-wraparound proximity.
    // <50% green, 50-100% yellow, >100% red. Pure relation-level
    // catalog math — no page scans on this cell even for TB tables.
    if let (Some(value), Some(sev), Some(title)) = (
        ov.freeze_age_pct.clone(),
        ov.freeze_age_severity,
        ov.freeze_age_tooltip.clone(),
    ) {
        if !value.is_empty() {
            pairs.push(MetaCell::Severity { key: "Freeze age", value, sev, title });
        }
    }
    // Tail — fields not in the primary sequence. Collation /
    // Auto-inc / Partitions / Created / Updated / Comment append
    // in their existing order so nothing is lost.
    push(&mut pairs, "Collation",      ov.collation.clone());
    push(&mut pairs, "Auto-inc",       ov.auto_inc.clone());
    push(&mut pairs, "Partitions",     ov.partition_count.clone());
    push(&mut pairs, "Created",        ov.create_time.clone());
    push(&mut pairs, "Updated",        ov.update_time.clone());

    // "Partition of" is rendered as its own custom cell (not via push) so the
    // value can be a clickable .rel-link rather than plain text. Same-schema
    // navigates the inspector to the parent; cross-schema renders qualified
    // text without a click (matches relationships convention).
    //
    // parent_table / parent_schema are `Option<String>` to express the
    // PG-only intent at the type level — see `TableOverview` doc
    // comments. `unwrap_or_default()` collapses both None (MySQL didn't
    // emit) and Some("") (PG table isn't a partition) to "" for the
    // rendering layer, which then hides the row via the existing
    // is_empty check below.
    let parent_table   = ov.parent_table.clone().unwrap_or_default();
    let parent_schema  = ov.parent_schema.clone().unwrap_or_default();
    let same_schema    = parent_schema.is_empty() || parent_schema == this_schema;
    let s_click        = if parent_schema.is_empty() { this_schema.clone() } else { parent_schema.clone() };
    let n_click        = parent_table.clone();
    let key_click      = (s_click.clone(), n_click.clone());

    // "Partition rule" not in the grid — `FOR VALUES FROM (...) TO (...)` is
    // too wide for the 200px grid cell and gets truncated. Render full-width
    // below the grid like Comment.
    //
    // Same Option semantics as parent_table / parent_schema. None
    // means MySQL (the syntax is different); Some("") means PG table
    // isn't partitioned. Both collapse to "" via unwrap_or_default,
    // and the existing is_empty check below hides the row.
    let partition_bound = ov.partition_bound.clone().unwrap_or_default();
    let comment         = ov.comment.clone();

    view! {
        <div class="object-header">
            <div class="object-meta-grid">
                {pairs.into_iter().map(meta_cell_view).collect_view()}
                {if parent_table.is_empty() {
                    view! { <div/> }.into_view()
                } else if same_schema {
                    view! {
                        <div class="object-meta-cell">
                            <span class="object-meta-key">"Partition of"</span>
                            <span class="object-meta-val">
                                <span class="rel-link"
                                      on:click=move |_| {
                                          ensure_table_detail_loaded(plane, &s_click, &n_click, table_cache, ws, session);
                                          selected.set(Some(key_click.clone()));
                                      }>
                                    {parent_table.clone()}
                                </span>
                            </span>
                        </div>
                    }.into_view()
                } else {
                    let qualified = format!("{}.{}", parent_schema, parent_table);
                    view! {
                        <div class="object-meta-cell">
                            <span class="object-meta-key">"Partition of"</span>
                            <span class="object-meta-val">
                                <span class="rel-cross">{qualified}</span>
                            </span>
                        </div>
                    }.into_view()
                }}
            </div>
            {if partition_bound.is_empty() {
                view! { <div/> }.into_view()
            } else {
                view! {
                    <div class="object-comment">
                        <span class="object-meta-key">"Partition rule"</span>
                        <span class="object-comment-text">{partition_bound}</span>
                    </div>
                }.into_view()
            }}
            {if comment.is_empty() {
                view! { <div/> }.into_view()
            } else {
                view! {
                    <div class="object-comment">
                        <span class="object-meta-key">"Comment"</span>
                        <span class="object-comment-text">{comment}</span>
                    </div>
                }.into_view()
            }}
        </div>
    }.into_view()
}

pub(crate) fn render_column_rows(
    cols: Vec<ExplorerColumn>,
    tags: HashMap<String, Vec<String>>,
    schema_cursor: RwSignal<Option<usize>>,
) -> leptos::View {
    if cols.is_empty() {
        return view! { <div class="detail-msg">"(no columns)"</div> }.into_view();
    }
    let has_source = cols.iter().any(|c| !c.source.is_empty());
    view! {
        <table class="schema-table schema-cols-table">
            <thead>
                <tr>
                    <th class="schema-th-ord">"Ordinal"<br/>"Position"</th>
                    <th class="schema-th-name">"Column"<br/>"Name"</th>
                    <th>"Data-Type"</th>
                    <th>"Size"</th>
                    <th>"Nullable?"</th>
                    <th>"Default"</th>
                    <th>"Extra"</th>
                    {if has_source { view! { <th>"Source"</th> }.into_view() } else { view! { <span/> }.into_view() }}
                </tr>
            </thead>
            <tbody>
                {cols.into_iter().enumerate().map(|(i, col)| {
                    let (ty, sz) = parse_column_type(&col.col_type);
                    let nullable = col.nullable.eq_ignore_ascii_case("YES");
                    let default = format_default(&col.col_type, &col.default_val, nullable);
                    let col_tags = tags.get(&col.name).cloned().unwrap_or_default();
                    let title_attr = col.comment.clone();
                    let source = col.source.clone();
                    view! {
                        <tr class="schema-col-row"
                            attr:data-row-idx=i.to_string()
                            class:schema-col-row-active=move || schema_cursor.get() == Some(i)
                            on:click=move |_| schema_cursor.set(Some(i))
                            title=title_attr>
                            <td class="schema-td-ord">{i + 1}</td>
                            <td class="schema-td-name">
                                <span class="schema-col-name">{col.name}</span>
                                {col_tags.into_iter().map(|t| {
                                    let is_pk = t.starts_with("pk");
                                    view! {
                                        <sup class="schema-col-tag"
                                             class:schema-col-tag-pk=is_pk>
                                            "("{t}")"
                                        </sup>
                                    }
                                }).collect_view()}
                            </td>
                            <td class="schema-td-type">{ty}</td>
                            <td class="schema-td-size">{sz}</td>
                            <td class="schema-td-null">
                                {if nullable { "Yes" } else { "No" }}
                            </td>
                            <td class="schema-td-default">{default}</td>
                            <td class="schema-td-extra">{col.extra}</td>
                            {if has_source { view! { <td class="schema-td-source">{source}</td> }.into_view() } else { view! { <span/> }.into_view() }}
                        </tr>
                    }
                }).collect_view()}
            </tbody>
        </table>
    }.into_view()
}

pub(crate) fn render_index_rows(indexes: Vec<IndexInfo>, tags: Vec<String>) -> leptos::View {
    // Show the per-index Storage column only when at least one index in
    // this table has a factor (PG btree only). Skipping the column when
    // every value would be empty keeps the layout tidy on tables that
    // are all-MySQL / all-GIN / all-non-analyzed.
    let show_storage = indexes.iter().any(|i| i.storage_factor.as_deref().is_some_and(|s| !s.is_empty()));
    // Same logic for the Size column: when at least one index has an
    // ideal-bytes estimate, the column header reads "Size / Ideal" and
    // per-row values render as "<actual> / <ideal>". Rows whose ideal
    // is empty (non-btree, never-analyzed, etc.) just show <actual>.
    let show_size_ideal = indexes.iter().any(|i| !i.size_ideal.is_empty());
    // Index-usage signal (PG `pg_stat_user_indexes.idx_scan`): show the
    // "Uses" column when at least one row has a count. Zero counts are
    // coloured red — drop candidates — via the existing .metric-bad
    // class.
    let show_uses = indexes.iter().any(|i| !i.scan_count.is_empty());
    view! {
        <table class="schema-table schema-index-table">
            <thead>
                <tr>
                    <th class="schema-th-itag"></th>
                    <th>"Name"</th>
                    <th>"Kind"</th>
                    <th>{if show_size_ideal { "Size / Ideal" } else { "Size" }}</th>
                    {if show_storage {
                        view! { <th>"Storage"</th> }.into_view()
                    } else { view! { <span/> }.into_view() }}
                    {if show_uses {
                        view! { <th>"Uses"</th> }.into_view()
                    } else { view! { <span/> }.into_view() }}
                    <th class="schema-th-unique">"Unique"</th>
                    <th>"Columns"</th>
                </tr>
            </thead>
            <tbody>
                {indexes.into_iter().zip(tags.into_iter()).map(|(idx, tag)| {
                    // Append " (Inherited)" when the index propagated from a
                    // partitioned-parent index (PG declarative partitioning).
                    let kind = {
                        let pretty = pretty_index_kind(&idx.index_type);
                        if idx.inherited { format!("{} (Inherited)", pretty) } else { pretty }
                    };
                    let cols = idx.columns.join(", ");
                    let is_pk = tag == "pk";
                    // Size value: when an ideal estimate is available
                    // pair them as "actual / ideal" (e.g. "320 KB /
                    // 352 KB"). Otherwise just the actual size.
                    let size_display = if !idx.size_ideal.is_empty() {
                        format!("{} / {}", idx.size, idx.size_ideal)
                    } else {
                        idx.size.clone()
                    };
                    // Storage cell — colored class + tooltip when the
                    // plane emitted a factor. Empty cell otherwise (so
                    // the column stays aligned for tables where some
                    // indexes are btree and others aren't).
                    let storage_cell = if show_storage {
                        match (idx.storage_factor.as_deref(), idx.storage_severity) {
                            (Some(val), Some(sev)) if !val.is_empty() => {
                                let sev_class = match sev {
                                    Severity::Good    => "metric-good",
                                    Severity::Watch   => "metric-watch",
                                    Severity::Bad     => "metric-bad",
                                    // Not emitted by index storage factor
                                    // classifier today; defensive fallback
                                    // to no colour class if it ever is.
                                    Severity::Neutral => "",
                                };
                                let cls = format!("schema-td-size {}", sev_class);
                                let title = idx.storage_tooltip.clone().unwrap_or_default();
                                let v = val.to_string();
                                view! { <td class=cls title=title>{v}</td> }.into_view()
                            }
                            _ => view! { <td class="schema-td-size"></td> }.into_view(),
                        }
                    } else { view! { <span/> }.into_view() };
                    // Uses cell: scan count, coloured red on zero (drop
                    // candidate). Tooltip explains the underlying signal
                    // so a DBA reads the cell without having to recall
                    // what `pg_stat_user_indexes.idx_scan` means.
                    let uses_cell = if show_uses {
                        if idx.scan_count.is_empty() {
                            view! { <td class="schema-td-size"></td> }.into_view()
                        } else {
                            let (cls, title) = match idx.scan_severity {
                                Some(Severity::Bad) => (
                                    "schema-td-size metric-bad",
                                    "Never scanned since the last stat reset. Drop candidate.".to_string(),
                                ),
                                _ => (
                                    "schema-td-size",
                                    "Scans since the last pg_stat_user_indexes reset; the \
                                     parenthesised value is the average entries returned per \
                                     scan (idx_tup_read / idx_scan) — a diagnostic for \
                                     selectivity, not a quality verdict on its own.".to_string(),
                                ),
                            };
                            let v = idx.scan_count.clone();
                            view! { <td class=cls title=title>{v}</td> }.into_view()
                        }
                    } else { view! { <span/> }.into_view() };
                    view! {
                        <tr>
                            <td class="schema-td-itag">
                                <span class="schema-col-tag" class:schema-col-tag-pk=is_pk>
                                    "("{tag}")"
                                </span>
                            </td>
                            <td class="schema-td-name">{idx.name}</td>
                            <td class="schema-td-type">{kind}</td>
                            <td class="schema-td-size">{size_display}</td>
                            {storage_cell}
                            {uses_cell}
                            <td class="schema-td-unique">
                                {if idx.unique { "x" } else { "" }}
                            </td>
                            <td class="schema-td-default">{cols}</td>
                        </tr>
                    }
                }).collect_view()}
            </tbody>
        </table>
    }.into_view()
}

// ── ERD tab ─────────────────────────────────────────────────────────────────
//
// Per-table ERD with HTML cards and an SVG edge overlay. Layout follows the
// spec at vault/Architecture/ERD_PANEL_SPEC.md:
//
// - Up to five generation bands left to right: grandparent | parent | TARGET
//   | child | grandchild. Empty bands collapse — if a target has no
//   grandparents the parent band sits at the left edge, etc.
// - Parents are placed so their PK row aligns with the FK row of their child
//   in the next band (single straight horizontal where possible).
// - Children distribute their FK rows evenly across target's right-edge
//   inner range; grandchildren average their parent-PK ideal-y.
// - Edge anchor placement (§7b): an edge whose ideal y falls within the
//   parent card's vertical extent pins at perfection (no compromise).
//   Floating edges (ideal-y outside the parent's range) take the largest
//   available gap between pinned anchors with min spacing.
// - Edges render orthogonal H-V-H with sharp 90° turns; same-row pairs
//   collapse to a single horizontal segment (no zigzag).
// - Crow's foot at the child (FK) end opens TOWARD the entity. Single
//   perpendicular tick at the parent (PK) end. Target-touching edges
//   stroke brighter/thicker than generation-link edges.

// Card widths sized for typical long PG identifiers — names like
// `maintenance_study_manifest_backfill` (34 chars) or
// `imagingservicerequestattendingphysician` (39 chars) are common in
// medical / clinical schemas. At 13px monospace (`.erd-card-name`)
// each char is ~7.8px, so 34 chars needs ~265px of name area; with
// 14px L+R padding the card needs ~293px. 320/340px gives headroom
// for 37-40 char names without truncation while still leaving enough
// canvas width for a 5-band layout at default zoom.
const ERD_CARD_W:        i32 = 320;
const ERD_TARGET_CARD_W: i32 = 340;
const ERD_HEADER_H:      i32 = 36;
const ERD_ROW_H:         i32 = 22;
const ERD_PAD_TOP:       i32 = 4;
const ERD_PAD_BOTTOM:    i32 = 6;
const ERD_BAND_GAP:      i32 = 110;
const ERD_PART_DIV_H:    i32 = 6;
const ERD_PART_LABEL_H:  i32 = 18;
const ERD_PART_ROW_H:    i32 = 18;
const ERD_PART_VISIBLE_CAP: usize = 8;
/// Default cap on cards-per-band before the rest collapse into a
/// "+N more" pill. `u8::MAX` means uncapped. Mirrors the toolbar
/// dropdown's middle option.
pub(crate) const ERD_DEFAULT_BAND_CAP: u8 = 10;
const ERD_MIN_CARD_GAP:  i32 = 32;
const ERD_INNER_PAD:     i32 = 14;
const ERD_FLOAT_SPACING: i32 = 28;
const ERD_TOP_MARGIN:    i32 = 80;
const ERD_SIDE_MARGIN:   i32 = 40;
const ERD_BOTTOM_PAD:    i32 = 60;
const ERD_RIGHT_PAD:     i32 = 60;
/// Approximate width of one monospace 12px char — used to clip overlong
/// names with an ellipsis. Matches `--font-mono` in style.css.
const ERD_CHAR_W:        f32 = 7.0;

#[derive(Clone, Copy, PartialEq)]
enum ErdTag { Pk, Fk, PkFk }

impl ErdTag {
    fn label(self) -> &'static str {
        match self { ErdTag::Pk => "pk", ErdTag::Fk => "fk", ErdTag::PkFk => "pk·fk" }
    }
}

/// Generation slot: -2 grandparent, -1 parent, 0 target, +1 child, +2 grandchild.
type ErdGen = i8;

#[derive(Clone)]
struct ErdCard {
    schema:    String,
    table:     String,
    /// Currently informational — kept on the struct so callers can identify
    /// a card's generation without extra bookkeeping. Future use: per-band
    /// styling, debugging, or median-sweep ordering.
    #[allow(dead_code)]
    gen:       ErdGen,
    is_target: bool,
    /// Column rows: `Some(tag)` for PK/FK rows, `None` for plain rows
    /// (only ever shown in "All Fields" mode). The renderer drops the
    /// tag annotation and the row-tint when the tag is None.
    columns:   Vec<(String, Option<ErdTag>)>,
    /// Partition (schema, name) pairs. Populated only on the target card
    /// when the focal table is a partitioned root. The renderer shows up
    /// to ERD_PART_VISIBLE_CAP rows plus an "and N more" overflow line
    /// — partitioned tables with hundreds of partitions don't blow the
    /// card vertically. Empty for non-target cards.
    partitions: Vec<(String, String)>,
}

impl ErdCard {
    fn width(&self) -> i32 {
        if self.is_target { ERD_TARGET_CARD_W } else { ERD_CARD_W }
    }
    fn height(&self) -> i32 {
        let cols_h = (self.columns.len() as i32).max(1) * ERD_ROW_H;
        let parts_h = if self.partitions.is_empty() { 0 } else {
            let visible = self.partitions.len().min(ERD_PART_VISIBLE_CAP) as i32;
            let overflow = if self.partitions.len() > ERD_PART_VISIBLE_CAP { 1 } else { 0 };
            ERD_PART_DIV_H + ERD_PART_LABEL_H + (visible + overflow) * ERD_PART_ROW_H
        };
        ERD_HEADER_H + ERD_PAD_TOP + cols_h + ERD_PAD_BOTTOM + parts_h
    }
}

/// Y-offset of a column row's vertical centre relative to the card's top-left.
/// Falls back to row 0 when the column name isn't on the card — happens for
/// depth-2 edges that reference columns the bridge card hasn't surfaced.
fn erd_col_y_offset(card: &ErdCard, column: &str) -> i32 {
    let row = card.columns.iter().position(|(n, _)| n == column).unwrap_or(0);
    ERD_HEADER_H + ERD_PAD_TOP + (row as i32) * ERD_ROW_H + ERD_ROW_H / 2
}

/// Trim `s` to fit `max_px` of horizontal space at ~ERD_CHAR_W per char,
/// appending an ellipsis when truncation occurs.
fn erd_clip(s: &str, max_px: i32) -> String {
    let max_chars = ((max_px as f32) / ERD_CHAR_W).floor() as usize;
    if s.chars().count() <= max_chars { return s.to_string(); }
    let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", truncated)
}

/// Build a keys-only card from raw PK/FK column-name vecs, deduping and
/// tagging columns that appear in both sides as `pk·fk`. The merged column
/// list is alpha-sorted so anchor lookup gets stable row indices across
/// renders. Every row is `Some(tag)` — plain rows only appear in
/// `erd_build_card_full` (all-fields mode).
fn erd_build_card(
    schema:    &str,
    table:     &str,
    gen:       ErdGen,
    is_target: bool,
    mut pk:    Vec<String>,
    mut fk:    Vec<String>,
) -> ErdCard {
    use std::collections::HashSet;
    pk.sort(); pk.dedup();
    fk.sort(); fk.dedup();
    let pk_set: HashSet<&String> = pk.iter().collect();
    let fk_set: HashSet<&String> = fk.iter().collect();
    let mut all: Vec<String> = pk.iter().chain(fk.iter()).cloned().collect();
    all.sort();
    all.dedup();
    let mut cols = Vec::new();
    for c in &all {
        let in_pk = pk_set.contains(c);
        let in_fk = fk_set.contains(c);
        let tag = match (in_pk, in_fk) {
            (true,  true)  => ErdTag::PkFk,
            (true,  false) => ErdTag::Pk,
            (false, true)  => ErdTag::Fk,
            _              => continue,
        };
        cols.push((c.clone(), Some(tag)));
    }
    ErdCard {
        schema: schema.to_string(), table: table.to_string(),
        gen, is_target, columns: cols,
        partitions: Vec::new(),
    }
}

/// Build an all-fields card. `all_columns` is the full column list in
/// ordinal order (with PK flag from the bulk-columns query for neighbours
/// or from `IndexInfo` for the target). `fk_cols` comes from edges as
/// before. Each column row is tagged Pk / Fk / PkFk based on those two
/// sources; columns that are neither render with a `None` tag (plain row).
fn erd_build_card_full(
    schema:      &str,
    table:       &str,
    gen:         ErdGen,
    is_target:   bool,
    all_columns: &[(String, bool /*is_pk*/)],
    fk_cols:     &[String],
) -> ErdCard {
    use std::collections::HashSet;
    let fk_set: HashSet<&String> = fk_cols.iter().collect();
    let cols: Vec<(String, Option<ErdTag>)> = all_columns.iter().map(|(name, is_pk)| {
        let in_fk = fk_set.contains(name);
        let tag = match (*is_pk, in_fk) {
            (true,  true)  => Some(ErdTag::PkFk),
            (true,  false) => Some(ErdTag::Pk),
            (false, true)  => Some(ErdTag::Fk),
            (false, false) => None,
        };
        (name.clone(), tag)
    }).collect();
    ErdCard {
        schema: schema.to_string(), table: table.to_string(),
        gen, is_target, columns: cols,
        partitions: Vec::new(),
    }
}

fn erd_target_card(
    schema:     &str,
    name:       &str,
    indexes:    &[IndexInfo],
    ancestors:  &[RelEdge],
    partitions: &[PartitionInfo],
) -> ErdCard {
    let pk_cols: Vec<String> = indexes.iter()
        .find(|i| i.name.eq_ignore_ascii_case("PRIMARY"))
        .map(|i| i.columns.clone())
        .unwrap_or_default();
    let mut fk_cols: Vec<String> = Vec::new();
    for e in ancestors.iter().filter(|e| e.depth == 1) {
        for c in &e.from_columns { fk_cols.push(c.clone()); }
    }
    let mut card = erd_build_card(schema, name, 0, true, pk_cols, fk_cols);
    // Natural-sort so `part_2` precedes `part_10` in the focal card's
    // partition list (matches the outer-list and Schema-tab Partitions
    // section ordering).
    let mut sorted: Vec<&PartitionInfo> = partitions.iter().collect();
    sorted.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    card.partitions = sorted.into_iter()
        .map(|p| (p.schema.clone(), p.name.clone()))
        .collect();
    card
}

/// A band's "+N more" pill: its band, x, y and width, how many cards it
/// stands for, and those cards (as `ErdOverflow::hidden` lists them).
type ErdPill = (ErdGen, i32, i32, i32, usize, Vec<(String, String, usize)>);

/// Overflow tail captured by a band builder when the per-band cap clips
/// it. Drives the "+N more" pill that renders at the bottom of the
/// affected band, plus the popover that lists the hidden tables.
///
/// `hidden` preserves the band's natural alphabetical order so the
/// popover reads predictably. The trailing `usize` is the number of FK
/// columns the hidden table contributed to the band's edges — useful as
/// a secondary signal in the popover (a hub's biggest fan-outs surface
/// at a glance) without changing the sort.
#[derive(Clone)]
pub(crate) struct ErdOverflow {
    pub(crate) band:   ErdGen,
    pub(crate) hidden: Vec<(String, String, usize)>,
}

/// Split a fully-built band into a visible head + an optional overflow
/// tail according to the user's per-band cap. Sentinel `u8::MAX` skips
/// the cap entirely (used by the toolbar's "Show all" option).
fn split_band_at_cap<T, K, F>(
    band: ErdGen,
    cap: u8,
    entries: Vec<(K, T)>,
    mut build_card: F,
    fk_count: impl Fn(&T) -> usize,
    name_of:  impl Fn(&K) -> (String, String),
) -> (Vec<ErdCard>, Option<ErdOverflow>)
where
    F: FnMut(K, T) -> ErdCard,
{
    let cap_usize = if cap == u8::MAX { entries.len() } else { (cap as usize).min(entries.len()) };
    if cap_usize >= entries.len() {
        let cards = entries.into_iter().map(|(k, v)| build_card(k, v)).collect();
        return (cards, None);
    }
    let mut iter = entries.into_iter();
    let cards: Vec<ErdCard> = (&mut iter).take(cap_usize).map(|(k, v)| build_card(k, v)).collect();
    let hidden: Vec<(String, String, usize)> = iter
        .map(|(k, v)| {
            let n = fk_count(&v);
            let (s, t) = name_of(&k);
            (s, t, n)
        })
        .collect();
    (cards, Some(ErdOverflow { band, hidden }))
}

/// Cards for the parent band. Parents sit on the **PK** side of depth-1
/// ancestor edges (target → parent) and on the **FK** side of depth-2
/// ancestor edges they bridge (parent → grandparent). Both column sets land
/// on the card so anchor lookups for either direction succeed.
fn erd_parent_cards(
    focal_s: &str, focal_t: &str,
    ancestors: &[RelEdge], cap: u8,
) -> (Vec<ErdCard>, Option<ErdOverflow>) {
    let mut keys: BTreeMap<(String, String), (Vec<String>, Vec<String>)> = BTreeMap::new();
    for e in ancestors {
        if e.depth == 1 && !(e.schema == focal_s && e.table == focal_t) {
            let entry = keys.entry((e.schema.clone(), e.table.clone())).or_default();
            for c in &e.to_columns { entry.0.push(c.clone()); }
        }
        if e.depth == 2 && !(e.via_schema == focal_s && e.via_table == focal_t)
                        && !e.via_table.is_empty() {
            let entry = keys.entry((e.via_schema.clone(), e.via_table.clone())).or_default();
            for c in &e.from_columns { entry.1.push(c.clone()); }
        }
    }
    let entries: Vec<_> = keys.into_iter().collect();
    split_band_at_cap(
        -1, cap, entries,
        |(s, t), (pk, fk)| erd_build_card(&s, &t, -1, false, pk, fk),
        |(_pk, fk)| fk.len(),
        |(s, t)| (s.clone(), t.clone()),
    )
}

fn erd_grandparent_cards(
    focal_s: &str, focal_t: &str,
    ancestors: &[RelEdge], cap: u8,
) -> (Vec<ErdCard>, Option<ErdOverflow>) {
    let mut keys: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for e in ancestors.iter().filter(|e| e.depth == 2) {
        if e.schema == focal_s && e.table == focal_t { continue; }
        let entry = keys.entry((e.schema.clone(), e.table.clone())).or_default();
        for c in &e.to_columns { entry.push(c.clone()); }
    }
    let entries: Vec<_> = keys.into_iter().collect();
    split_band_at_cap(
        -2, cap, entries,
        |(s, t), pk| erd_build_card(&s, &t, -2, false, pk, Vec::new()),
        |pk| pk.len(),
        |(s, t)| (s.clone(), t.clone()),
    )
}

fn erd_child_cards(
    focal_s: &str, focal_t: &str,
    descendants: &[RelEdge], cap: u8,
) -> (Vec<ErdCard>, Option<ErdOverflow>) {
    let mut keys: BTreeMap<(String, String), (Vec<String>, Vec<String>)> = BTreeMap::new();
    for e in descendants {
        if e.depth == 1 && !(e.schema == focal_s && e.table == focal_t) {
            let entry = keys.entry((e.schema.clone(), e.table.clone())).or_default();
            for c in &e.from_columns { entry.1.push(c.clone()); }
        }
        if e.depth == 2 && !(e.via_schema == focal_s && e.via_table == focal_t)
                        && !e.via_table.is_empty() {
            let entry = keys.entry((e.via_schema.clone(), e.via_table.clone())).or_default();
            for c in &e.to_columns { entry.0.push(c.clone()); }
        }
    }
    let entries: Vec<_> = keys.into_iter().collect();
    split_band_at_cap(
        1, cap, entries,
        |(s, t), (pk, fk)| erd_build_card(&s, &t, 1, false, pk, fk),
        |(_pk, fk)| fk.len(),
        |(s, t)| (s.clone(), t.clone()),
    )
}

fn erd_grandchild_cards(
    focal_s: &str, focal_t: &str,
    descendants: &[RelEdge], cap: u8,
) -> (Vec<ErdCard>, Option<ErdOverflow>) {
    let mut keys: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for e in descendants.iter().filter(|e| e.depth == 2) {
        if e.schema == focal_s && e.table == focal_t { continue; }
        let entry = keys.entry((e.schema.clone(), e.table.clone())).or_default();
        for c in &e.from_columns { entry.push(c.clone()); }
    }
    let entries: Vec<_> = keys.into_iter().collect();
    split_band_at_cap(
        2, cap, entries,
        |(s, t), fk| erd_build_card(&s, &t, 2, false, Vec::new(), fk),
        |fk| fk.len(),
        |(s, t)| (s.clone(), t.clone()),
    )
}

/// One edge to render. Coordinates are absolute on the canvas. The parent
/// (PK) end is at (px, py) and emits a perpendicular tick; the child (FK)
/// end is at (cx, cy) and emits a 3-prong crow's foot opening toward the
/// child card. The path between the two endpoints is a cubic Bézier with
/// horizontal-tangent control points — both controls share their endpoint's
/// y, so the curve enters and exits horizontally regardless of the y delta.
///
/// `from_key` and `to_key` carry the (schema, table) of each endpoint so
/// the focus-mode dimming logic can decide whether this edge stays bright
/// when a card is hovered.
///
/// `lane_shift` (default 0) nudges the control points asymmetrically — a
/// positive shift bulges the curve closer to the source, negative closer
/// to the destination. Used to spread multiple edges sharing the same
/// (px, cx) band gap into distinct horizontal lanes.
struct ErdEdge {
    px: i32, py: i32,
    cx: i32, cy: i32,
    is_target: bool,
    from_key:   (String, String),
    to_key:     (String, String),
    lane_shift: i32,
}

#[derive(Clone, Copy, PartialEq)]
enum ErdSide { Left, Right }

/// Resolve overlaps: sort by y, push the lower one down whenever the previous
/// card's bottom plus min-gap exceeds the current y. Idempotent.
fn erd_resolve_overlaps(cards: &mut [(ErdCard, i32 /*x*/, i32 /*y*/)]) {
    cards.sort_by_key(|(_, _, y)| *y);
    for i in 1..cards.len() {
        let prev_bottom = cards[i - 1].2 + cards[i - 1].0.height();
        let min_y = prev_bottom + ERD_MIN_CARD_GAP;
        if cards[i].2 < min_y { cards[i].2 = min_y; }
    }
}

/// Find a placed card by (schema, table). Returns (card_ref, x, y).
fn erd_find_placed<'a>(
    placed: &'a [(ErdCard, i32, i32)],
    s: &str,
    t: &str,
) -> Option<&'a (ErdCard, i32, i32)> {
    placed.iter().find(|(c, _, _)| c.schema == s && c.table == t)
}

/// Render one card as an absolutely-positioned HTML div. The SVG edge layer
/// sits underneath, so the card's solid background terminates any line ends
/// that ran behind it.
///
/// `neighbors` is the set of (schema, table) keys this card has any edge to
/// — used by focus mode to decide whether to stay bright when a different
/// card is hovered. The card's own mouseenter/leave handlers drive `hovered`.
///
/// Non-target cards' table-name headers are click targets that re-centre
/// the inspector on the clicked table with the ERD tab still active. The
/// focal card stays unclickable — the user is already there.
#[allow(clippy::too_many_arguments)]
fn erd_render_card(
    card:        &ErdCard,
    x: i32, y: i32,
    hovered:     leptos::RwSignal<Option<(String, String)>>,
    neighbors:   std::collections::HashSet<(String, String)>,
    plane:       &'static dyn DbmsPlane,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    selected:    RwSignal<Option<(String, String)>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
) -> leptos::View {
    let w = card.width();
    let title_full = format!("{}.{}", card.schema, card.table);
    let title_clipped = erd_clip(&card.table, w - 32);
    let column_max_px = w - 28 - 44;
    let cols = card.columns.clone();
    let card_key = (card.schema.clone(), card.table.clone());
    let key_for_dim    = card_key.clone();
    let key_for_enter  = card_key.clone();
    let key_for_click  = card_key.clone();
    let is_target      = card.is_target;
    let is_dimmed = move || {
        match hovered.get() {
            None => false,
            Some(hk) => hk != key_for_dim && !neighbors.contains(&hk),
        }
    };
    let on_name_click = move |ev: web_sys::MouseEvent| {
        if is_target { return; }
        // Stop propagation so the canvas dblclick-zoom guard sees the click
        // landed in a card, not on empty space (it already gates on .closest
        // ".erd-card", but stopping bubble keeps any future canvas-level
        // single-click handlers off-limit too).
        ev.stop_propagation();
        let (s, t) = key_for_click.clone();
        let detail = ensure_table_detail_loaded(plane, &s, &t, table_cache, ws, session);
        // Pin the new detail's tab to ERD before swapping `selected`, so the
        // first reactive read after re-selection sees the intended tab.
        detail.active_tab.set(DetailTab::Erd);
        selected.set(Some((s, t)));
    };
    view! {
        <div class="erd-card"
             class:erd-card-target=card.is_target
             class:erd-dimmed=is_dimmed
             style=format!("position: absolute; left: {}px; top: {}px; width: {}px;", x, y, w)
             on:mouseenter=move |_| hovered.set(Some(key_for_enter.clone()))
             on:mouseleave=move |_| hovered.set(None)>
            <div class="erd-card-name"
                 class:erd-card-name-clickable=!is_target
                 title=title_full
                 on:click=on_name_click>
                <span class="erd-schema-prefix">{format!("{}.", card.schema)}</span>
                <span class="erd-table-name">{title_clipped}</span>
            </div>
            <div class="erd-card-cols">
                {cols.into_iter().map(|(name, tag)| {
                    match tag {
                        Some(t) => {
                            let tag_class = match t {
                                ErdTag::Pk   => "erd-tag-pk",
                                ErdTag::Fk   => "erd-tag-fk",
                                ErdTag::PkFk => "erd-tag-pkfk",
                            };
                            view! {
                                <div class="erd-col erd-col-relevant" title=name.clone()>
                                    <span class="erd-col-name">{erd_clip(&name, column_max_px)}</span>
                                    <span class={format!("erd-col-anno {}", tag_class)}>
                                        {format!("({})", t.label())}
                                    </span>
                                </div>
                            }
                        }
                        None => view! {
                            <div class="erd-col" title=name.clone()>
                                <span class="erd-col-name">{erd_clip(&name, column_max_px)}</span>
                                <span class="erd-col-anno"></span>
                            </div>
                        }
                    }
                }).collect_view()}
            </div>
            {erd_render_partitions(card, plane, table_cache, selected, ws, session, column_max_px)}
        </div>
    }.into_view()
}

/// Render the partition list at the bottom of the focal card. Returns an
/// empty view for cards with no partitions, so non-target cards (and
/// non-partitioned target cards) skip the section entirely.
///
/// Plane.partitions_are_navigable() decides whether each partition row
/// gets a click handler — PG declarative partitions are separately
/// addressable tables (each is its own pg_class entry), so navigating to
/// one is meaningful. MySQL partitions are storage divisions inside one
/// logical table, not separately addressable, so the rows render as
/// plain text. Click navigates to the partition's own detail view on
/// the **Schema** tab, not ERD — its FK topology is identical to the
/// parent's, so opening Schema gives more information.
#[allow(clippy::too_many_arguments)]
fn erd_render_partitions(
    card:        &ErdCard,
    plane:       &'static dyn DbmsPlane,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    selected:    RwSignal<Option<(String, String)>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
    max_px:      i32,
) -> leptos::View {
    if card.partitions.is_empty() {
        return view! { <div/> }.into_view();
    }
    let total = card.partitions.len();
    let visible: Vec<(String, String)> = card.partitions.iter()
        .take(ERD_PART_VISIBLE_CAP)
        .cloned()
        .collect();
    let overflow = total.saturating_sub(ERD_PART_VISIBLE_CAP);
    let navigable = plane.partitions_are_navigable();
    view! {
        <div class="erd-partitions">
            <div class="erd-partitions-label">"Partitions"</div>
            {visible.into_iter().map(|(s, t)| {
                let key = (s.clone(), t.clone());
                let display = erd_clip(&t, max_px);
                if navigable {
                    let s_for = s.clone();
                    let t_for = t.clone();
                    view! {
                        <div class="erd-partition erd-partition-clickable"
                             title=t.clone()
                             on:click=move |ev: web_sys::MouseEvent| {
                                 ev.stop_propagation();
                                 let detail = ensure_table_detail_loaded(plane, &s_for, &t_for, table_cache, ws, session);
                                 // Schema tab — partition's ERD would mirror
                                 // the parent's, no new info there.
                                 detail.active_tab.set(DetailTab::Schema);
                                 selected.set(Some(key.clone()));
                             }>
                            {display}
                        </div>
                    }.into_view()
                } else {
                    view! {
                        <div class="erd-partition" title=t.clone()>
                            {display}
                        </div>
                    }.into_view()
                }
            }).collect_view()}
            {if overflow > 0 {
                view! {
                    <div class="erd-partition erd-partition-overflow">
                        {format!("…and {} more", overflow)}
                    </div>
                }.into_view()
            } else {
                view! { <div/> }.into_view()
            }}
        </div>
    }.into_view()
}

/// Render one edge: the path itself, the crow's foot at the child end, and
/// the perpendicular tick at the parent end. Crow's foot apex sits ~10px
/// back from the entity edge with three prongs splaying out to the entity.
///
/// `hovered` drives focus-mode dimming: when a card is hovered, edges
/// neither incident to the hovered card stay dim.
fn erd_render_edge(edge: &ErdEdge, hovered: leptos::RwSignal<Option<(String, String)>>) -> leptos::View {
    let from_key = edge.from_key.clone();
    let to_key   = edge.to_key.clone();
    let is_dimmed = move || {
        match hovered.get() {
            None => false,
            Some(hk) => hk != from_key && hk != to_key,
        }
    };
    const FAN_LEN:   i32 = 10;
    const FAN_SPLAY: i32 = 5;
    const TICK_BACK: i32 = 8;
    const TICK_HALF: i32 = 5;

    // Cubic Bézier with horizontal-tangent control points. The control points
    // share their endpoint's y so the curve enters/exits horizontally at both
    // ends. Multiple edges leaving the same parent row diverge naturally
    // because their child endpoints differ — no lane-assignment pass needed
    // (the orthogonal H-V-H mode required one because identical mid_x values
    // produced overlapping vertical segments).
    //
    // Control-point offset `k` scales with the chord length (capped at 240px)
    // so short cross-card edges curve gently and long ones curve more
    // generously. The min of 20 keeps the curvature visible even when the
    // band gap is small. Cap raised from 160 to 240 for all-fields mode
    // where some curves span 300+ px of vertical distance and need more
    // horizontal breathing room to clear adjacent curves.
    let dx = (edge.cx - edge.px).abs() as f32;
    let dy = (edge.cy - edge.py).abs() as f32;
    let dist = (dx * dx + dy * dy).sqrt();
    let k = (dist * 0.5).clamp(20.0, 240.0) as i32;
    let dir: i32 = if edge.cx > edge.px { 1 } else { -1 };
    // Lane shift gets ADDED to k on the source side and SUBTRACTED from k on
    // the destination side. Asymmetric offsets shift the curve's midpoint
    // sideways, producing distinct horizontal paths for edges that share a
    // band gap. Same-row edges leave lane_shift=0, so they render as a flat
    // horizontal line regardless of the lane pass.
    let c1x = edge.px + (k + edge.lane_shift) * dir;
    let c2x = edge.cx - (k - edge.lane_shift) * dir;
    let path = format!(
        "M {} {} C {} {}, {} {}, {} {}",
        edge.px, edge.py,
        c1x, edge.py,
        c2x, edge.cy,
        edge.cx, edge.cy
    );
    let edge_class  = if edge.is_target { "erd-edge erd-edge-target" } else { "erd-edge" };
    let mark_class  = if edge.is_target { "erd-edge-marker erd-edge-marker-target" }
                      else              { "erd-edge-marker" };

    // Crow's foot at child end. The Bézier's tangent at (cx, cy) points from
    // c2 to (cx, cy) — purely horizontal since c2y = cy. Use c2 as the
    // "from" point so the fan apex sits along the curve, not on a phantom
    // line through the parent anchor.
    let from_x = c2x;
    let from_y = edge.cy;
    let dx_c = (edge.cx - from_x) as f32;
    let dy_c = (edge.cy - from_y) as f32;
    let len = (dx_c * dx_c + dy_c * dy_c).sqrt().max(1.0);
    let ux = dx_c / len;
    let uy = dy_c / len;
    let perp_x = -uy;
    let perp_y =  ux;
    let apex_x = (edge.cx as f32) - ux * (FAN_LEN as f32);
    let apex_y = (edge.cy as f32) - uy * (FAN_LEN as f32);
    let tip_a = (edge.cx as f32, edge.cy as f32);
    let tip_b = (edge.cx as f32 + perp_x * FAN_SPLAY as f32, edge.cy as f32 + perp_y * FAN_SPLAY as f32);
    let tip_c = (edge.cx as f32 - perp_x * FAN_SPLAY as f32, edge.cy as f32 - perp_y * FAN_SPLAY as f32);

    // Tick at parent end. Tangent at (px, py) points from (px, py) toward c1
    // — also horizontal since c1y = py. Use c1 as the "toward" point.
    let toward_x = c1x;
    let toward_y = edge.py;
    let dx2 = (toward_x - edge.px) as f32;
    let dy2 = (toward_y - edge.py) as f32;
    let len2 = (dx2 * dx2 + dy2 * dy2).sqrt().max(1.0);
    let ux2 = dx2 / len2;
    let uy2 = dy2 / len2;
    let perp2_x = -uy2;
    let perp2_y =  ux2;
    let tick_cx = (edge.px as f32) + ux2 * (TICK_BACK as f32);
    let tick_cy = (edge.py as f32) + uy2 * (TICK_BACK as f32);
    let tick_a_x = tick_cx + perp2_x * TICK_HALF as f32;
    let tick_a_y = tick_cy + perp2_y * TICK_HALF as f32;
    let tick_b_x = tick_cx - perp2_x * TICK_HALF as f32;
    let tick_b_y = tick_cy - perp2_y * TICK_HALF as f32;

    view! {
        <g class:erd-edge-dimmed=is_dimmed>
            <path class=edge_class d=path fill="none" />
            <line class=mark_class
                  x1={apex_x} y1={apex_y}
                  x2={tip_a.0} y2={tip_a.1} />
            <line class=mark_class
                  x1={apex_x} y1={apex_y}
                  x2={tip_b.0} y2={tip_b.1} />
            <line class=mark_class
                  x1={apex_x} y1={apex_y}
                  x2={tip_c.0} y2={tip_c.1} />
            <line class=mark_class
                  x1={tick_a_x} y1={tick_a_y}
                  x2={tick_b_x} y2={tick_b_y} />
        </g>
    }.into_view()
}

/// Cursor-anchored zoom helper. Updates the zoom signal AND adjusts the
/// scroll container's scroll position so the world-point under (mx, my)
/// stays under the same screen-position after the zoom. Without this, any
/// non-trivial zoom centres on the top-left and feels disorienting.
fn erd_apply_zoom_anchored(
    scroll_el: &web_sys::HtmlElement,
    zoom_sig: leptos::RwSignal<f64>,
    new_zoom: f64,
    mx: f64,
    my: f64,
) {
    let z1 = zoom_sig.get_untracked();
    let z2 = new_zoom.clamp(0.25, 2.0);
    if (z2 - z1).abs() < 0.001 { return; }
    let sl = scroll_el.scroll_left() as f64;
    let st = scroll_el.scroll_top() as f64;
    let world_x = (sl + mx) / z1;
    let world_y = (st + my) / z1;
    let new_sl = world_x * z2 - mx;
    let new_st = world_y * z2 - my;
    zoom_sig.set(z2);
    // Scroll size updates synchronously when Leptos flushes the style change
    // for the zoom-wrap div; setting scroll AFTER zoom.set() lands in the
    // same microtask, so the wrap is already at its new dimensions when the
    // browser clamps these values.
    scroll_el.set_scroll_left(new_sl.round() as i32);
    scroll_el.set_scroll_top(new_st.round() as i32);
}

/// Phase-4 ERD tab. HTML cards layered over an SVG edge canvas. Bands
/// collapse left when empty, edges anchor with the Pinned-Perfect rule,
/// target-touching edges render brighter than generation-link edges, and
/// the canvas supports cursor-anchored zoom (Cmd/Ctrl+scroll, double-click
/// step-in, or three toolbar buttons).
///
/// `plane`, `ws`, `session` are forwarded so the "All Fields" toggle
/// can fire the bulk-columns introspect query the first time it's flipped
/// on. The tab can render usefully without firing that query — every
/// card's PK/FK column tags come from edges + indexes already loaded.
///
/// `table_cache` and `selected` are forwarded for click-to-navigate: a
/// click on a non-target card's table name calls `ensure_table_detail_
/// loaded` and updates `selected`, re-centering the inspector on the
/// clicked table with the ERD tab still active.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_erd_tab(
    detail:      TableDetail,
    this_schema: String,
    this_name:   String,
    plane:       &'static dyn DbmsPlane,
    table_cache: RwSignal<HashMap<(String, String), TableDetail>>,
    selected:    RwSignal<Option<(String, String)>>,
    ws:          RwSignal<Option<WebSocket>>,
    session:  ExplorerSession,
) -> leptos::View {
    let ancestors   = detail.ancestors;
    let descendants = detail.descendants;
    let columns_sig = detail.columns;
    let indexes_sig = detail.indexes;
    let partitions_sig = detail.partitions;
    let zoom        = detail.erd_zoom;
    let generations = detail.erd_generations;
    let keys_only   = detail.erd_keys_only;
    let cards_per_band = detail.erd_cards_per_band;
    let neighbour_cols_sig = detail.erd_neighbour_columns;
    let scroll_ref: leptos::NodeRef<leptos::html::Div> = leptos::create_node_ref();
    let this_schema_for_fetch = this_schema.clone();
    let this_name_for_fetch   = this_name.clone();
    // Focus-mode hover state. Transient (cleared on mouse leave), so a local
    // signal is right — no need to persist on TableDetail across tab switches.
    let hovered: leptos::RwSignal<Option<(String, String)>> =
        leptos::create_rw_signal(None);

    // Maximum depth this specific table actually has data for. A leaf table
    // with no FK relationships clamps to 0; a table with parents/children
    // but nothing further out clamps to 1. While data is loading we report 2
    // (the data-cap) so the dropdown doesn't briefly collapse to "0
    // generations" before the edges arrive.
    let max_depth_available = leptos::create_memo(move |_| -> u8 {
        let a = match ancestors.get()   { LoadState::Loaded(v) => v, _ => return 2 };
        let d = match descendants.get() { LoadState::Loaded(v) => v, _ => return 2 };
        let has_d2 = a.iter().any(|e| e.depth == 2) || d.iter().any(|e| e.depth == 2);
        let has_d1 = a.iter().any(|e| e.depth == 1) || d.iter().any(|e| e.depth == 1);
        if has_d2 { 2 } else if has_d1 { 1 } else { 0 }
    });
    // Clamp the user's selected depth down if the table can't reach it.
    // Without this, switching from a 2-deep table to a leaf would leave the
    // dropdown showing "2 generations" with no matching option in the list.
    leptos::create_effect(move |_| {
        let cap = max_depth_available.get();
        if generations.get_untracked() > cap {
            generations.set(cap);
        }
    });

    // Lazy bulk-columns fetch. Fires only when the user toggles Keys-Only
    // OFF and the data hasn't been loaded yet — the default keys-only path
    // never pays for this query. Triggers on every (keys_only, ancestors,
    // descendants) change but the NotLoaded check makes the actual fetch
    // a one-shot.
    let plane_for_fetch = plane;
    leptos::create_effect(move |_| {
        if keys_only.get() { return; }
        if !matches!(neighbour_cols_sig.get(), LoadState::NotLoaded) { return; }
        let a = match ancestors.get()   { LoadState::Loaded(v) => v, _ => return };
        let d = match descendants.get() { LoadState::Loaded(v) => v, _ => return };
        let Some(sid) = session.id.get_untracked() else { return };
        // Collect every neighbour key (depth-1 + depth-2 endpoints + bridges).
        // Excludes the focal — its columns load via the standard table-detail
        // pipeline (`detail.columns`) on the focal's own ensure_table_detail
        // round.
        use std::collections::BTreeSet;
        let mut keys: BTreeSet<(String, String)> = BTreeSet::new();
        for e in a.iter().chain(d.iter()) {
            if e.schema != this_schema_for_fetch || e.table != this_name_for_fetch {
                keys.insert((e.schema.clone(), e.table.clone()));
            }
            if e.depth == 2 && !e.via_table.is_empty()
               && (e.via_schema != this_schema_for_fetch || e.via_table != this_name_for_fetch) {
                keys.insert((e.via_schema.clone(), e.via_table.clone()));
            }
        }
        let key_list: Vec<(String, String)> = keys.into_iter().collect();
        let sql = plane_for_fetch.table_columns_bulk_sql(&key_list);
        let tag = format!("erdcols:{}.{}", this_schema_for_fetch, this_name_for_fetch);
        neighbour_cols_sig.set(LoadState::Loading);
        ws_introspect(ws, &sid, sql, tag);
    });

    let on_wheel = move |ev: web_sys::WheelEvent| {
        // ctrl_key fires for trackpad pinch-zoom on Mac and for Ctrl-scroll
        // everywhere; meta_key covers Cmd-scroll on Mac when the user
        // doesn't use the trackpad. Either gesture means "zoom, not scroll."
        if !ev.ctrl_key() && !ev.meta_key() { return; }
        ev.prevent_default();
        let Some(scroll_el) = scroll_ref.get_untracked() else { return };
        let rect = scroll_el.get_bounding_client_rect();
        let mx = ev.client_x() as f64 - rect.left();
        let my = ev.client_y() as f64 - rect.top();
        let factor = if ev.delta_y() > 0.0 { 1.0 / 1.1 } else { 1.1 };
        let new_zoom = zoom.get_untracked() * factor;
        let scroll_el: &web_sys::HtmlElement = &scroll_el;
        erd_apply_zoom_anchored(scroll_el, zoom, new_zoom, mx, my);
    };

    let on_dblclick = move |ev: web_sys::MouseEvent| {
        // Step zoom in 1.25× toward the cursor — but only on EMPTY canvas.
        // Cards have their own single-click navigation; double-clicks on a
        // card shouldn't accidentally zoom the canvas. Walk the event
        // target's ancestor chain looking for `.erd-card`; if we find one,
        // suppress.
        if let Some(target) = ev.target() {
            use wasm_bindgen::JsCast;
            if let Ok(elem) = target.dyn_into::<web_sys::Element>() {
                if elem.closest(".erd-card").ok().flatten().is_some() {
                    return;
                }
            }
        }
        let Some(scroll_el) = scroll_ref.get_untracked() else { return };
        let rect = scroll_el.get_bounding_client_rect();
        let mx = ev.client_x() as f64 - rect.left();
        let my = ev.client_y() as f64 - rect.top();
        let new_zoom = zoom.get_untracked() * 1.25;
        let scroll_el: &web_sys::HtmlElement = &scroll_el;
        erd_apply_zoom_anchored(scroll_el, zoom, new_zoom, mx, my);
    };

    let zoom_btn = move |delta: f64| {
        // Toolbar buttons anchor the zoom on the viewport centre, so the
        // visible content stays roughly in place when the user clicks.
        let Some(scroll_el) = scroll_ref.get_untracked() else { return };
        let scroll_el: &web_sys::HtmlElement = &scroll_el;
        let mx = scroll_el.client_width() as f64 / 2.0;
        let my = scroll_el.client_height() as f64 / 2.0;
        let new_zoom = if delta == 0.0 { 1.0 } else { zoom.get_untracked() * delta };
        erd_apply_zoom_anchored(scroll_el, zoom, new_zoom, mx, my);
    };
    let zoom_out = move |_| zoom_btn(1.0 / 1.25);
    let zoom_reset = move |_| zoom_btn(0.0);
    let zoom_in  = move |_| zoom_btn(1.25);

    view! {
        <div class="erd-tab-root">
            <div class="erd-toolbar">
                <label class="erd-keys-toggle"
                    title="Show only PK / FK columns when checked; show every column when unchecked.">
                    <input type="checkbox"
                        prop:checked=move || keys_only.get()
                        on:change=move |e| {
                            use wasm_bindgen::JsCast;
                            let checked = e.target()
                                .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                                .map(|i| i.checked())
                                .unwrap_or(true);
                            keys_only.set(checked);
                        } />
                    <span>"Keys only"</span>
                </label>
                <span class="erd-toolbar-divider"></span>
                <select class="erd-gen-select"
                    title="How many generations of relationships to show (0 = target only)"
                    on:change=move |e| {
                        let v = event_target_value(&e).parse::<u8>().unwrap_or(2);
                        generations.set(v);
                    }>
                    {move || {
                        // Reactive option list — reruns when max_depth_available
                        // changes (e.g. on table switch). Caps at the actual
                        // depth the table reaches so a leaf doesn't offer a
                        // dropdown choice that produces an empty diagram.
                        let cap = max_depth_available.get();
                        (0..=cap).map(|d| {
                            let label = if d == 1 { "1 generation".to_string() }
                                        else      { format!("{} generations", d) };
                            view! {
                                <option value={d.to_string()}
                                        selected=move || generations.get() == d>
                                    {label}
                                </option>
                            }
                        }).collect_view()
                    }}
                </select>
                // Per-band cap — caps each generation band at N cards
                // (rest collapse behind a "+N more" pill). `u8::MAX` is
                // serialized as the sentinel "all" so an unconstrained
                // band reverts to the pre-cap behaviour.
                <select class="erd-gen-select"
                    title="Maximum tables shown per generation band; the rest collapse into a +N more pill."
                    on:change=move |e| {
                        let raw = event_target_value(&e);
                        let v = if raw == "all" { u8::MAX } else { raw.parse::<u8>().unwrap_or(ERD_DEFAULT_BAND_CAP) };
                        cards_per_band.set(v);
                    }>
                    {move || {
                        let cur = cards_per_band.get();
                        [5u8, 10, 15].iter().map(|&n| {
                            view! {
                                <option value={n.to_string()}
                                        selected=move || cur == n>
                                    {format!("Show {}", n)}
                                </option>
                            }
                        }).chain(std::iter::once(view! {
                            <option value="all"
                                    selected=move || cur == u8::MAX>
                                "Show all"
                            </option>
                        })).collect_view()
                    }}
                </select>
                <span class="erd-toolbar-divider"></span>
                // Magnifier-glass icons disambiguate the ERD toolbar's
                // zoom controls from the Data tab's bare +/- (Insert /
                // Delete) on the same inspector pane. SVG paths stroke
                // with `currentColor` so they pick up the explorer
                // theme's text color, and the icon sizes itself in
                // `em` so it tracks any future font-size scale.
                <button class="erd-zoom-btn" title="Zoom out" on:click=zoom_out>
                    <svg class="erd-zoom-icon" viewBox="0 0 16 16"
                         xmlns="http://www.w3.org/2000/svg"
                         fill="none" stroke="currentColor"
                         stroke-width="1.5" stroke-linecap="round">
                        <circle cx="7" cy="7" r="4.5"/>
                        <line x1="10.4" y1="10.4" x2="14" y2="14"/>
                        <line x1="4.5" y1="7" x2="9.5" y2="7"/>
                    </svg>
                </button>
                <button class="erd-zoom-btn erd-zoom-pct" title="Reset zoom" on:click=zoom_reset>
                    {move || format!("{}%", (zoom.get() * 100.0).round() as i32)}
                </button>
                <button class="erd-zoom-btn" title="Zoom in" on:click=zoom_in>
                    <svg class="erd-zoom-icon" viewBox="0 0 16 16"
                         xmlns="http://www.w3.org/2000/svg"
                         fill="none" stroke="currentColor"
                         stroke-width="1.5" stroke-linecap="round">
                        <circle cx="7" cy="7" r="4.5"/>
                        <line x1="10.4" y1="10.4" x2="14" y2="14"/>
                        <line x1="4.5" y1="7" x2="9.5" y2="7"/>
                        <line x1="7" y1="4.5" x2="7" y2="9.5"/>
                    </svg>
                </button>
            </div>
            <div class="erd-scroll"
                 node_ref=scroll_ref
                 on:wheel=on_wheel
                 on:dblclick=on_dblclick>
                {move || {
                let anc = ancestors.get();
                let des = descendants.get();
                let cols = columns_sig.get();
                let idxs = indexes_sig.get();
                if let LoadState::Error(e) = &anc {
                    return view! { <div class="detail-error">{e.clone()}</div> }.into_view();
                }
                if let LoadState::Error(e) = &des {
                    return view! { <div class="detail-error">{e.clone()}</div> }.into_view();
                }
                if let LoadState::Error(e) = &cols {
                    return view! { <div class="detail-error">{e.clone()}</div> }.into_view();
                }
                if let LoadState::Error(e) = &idxs {
                    return view! { <div class="detail-error">{e.clone()}</div> }.into_view();
                }
                let (a_rows, d_rows, t_cols, t_idx) = match (anc, des, cols, idxs) {
                    (LoadState::Loaded(a), LoadState::Loaded(d),
                     LoadState::Loaded(c), LoadState::Loaded(i)) => (a, d, c, i),
                    _ => return view! { <div class="detail-msg">"Loading topology…"</div> }.into_view(),
                };

                // Generations filter — applied BEFORE dedup so a depth-1 row
                // dropped here doesn't keep its depth-2 cousin alive. The
                // value is the maximum depth to KEEP: 0 drops all edges
                // (target alone), 1 keeps depth=1, 2 keeps both.
                let max_depth = generations.get();
                let a_rows: Vec<RelEdge> = a_rows.into_iter()
                    .filter(|e| e.depth <= max_depth)
                    .collect();
                let d_rows: Vec<RelEdge> = d_rows.into_iter()
                    .filter(|e| e.depth <= max_depth)
                    .collect();

                // Dedup: a table that's both depth-1 and depth-2 renders only
                // at its closer position.
                use std::collections::BTreeSet;
                let mut parents_set:  BTreeSet<(String, String)> = BTreeSet::new();
                let mut children_set: BTreeSet<(String, String)> = BTreeSet::new();
                for e in a_rows.iter().filter(|e| e.depth == 1) {
                    if e.schema == this_schema && e.table == this_name { continue; }
                    parents_set.insert((e.schema.clone(), e.table.clone()));
                }
                for e in d_rows.iter().filter(|e| e.depth == 1) {
                    if e.schema == this_schema && e.table == this_name { continue; }
                    children_set.insert((e.schema.clone(), e.table.clone()));
                }
                let a_rows: Vec<RelEdge> = a_rows.into_iter().filter(|e| {
                    if e.schema == this_schema && e.table == this_name { return false; }
                    if e.depth == 2 && parents_set.contains(&(e.schema.clone(), e.table.clone())) { return false; }
                    true
                }).collect();
                let d_rows: Vec<RelEdge> = d_rows.into_iter().filter(|e| {
                    if e.schema == this_schema && e.table == this_name { return false; }
                    if e.depth == 2 && children_set.contains(&(e.schema.clone(), e.table.clone())) { return false; }
                    true
                }).collect();

                // Partitions are loaded asynchronously alongside columns/
                // indexes; while loading we just render an empty list — the
                // card will reflow once the data arrives.
                let parts_for_target: Vec<PartitionInfo> = match partitions_sig.get() {
                    LoadState::Loaded(v) => v,
                    _ => Vec::new(),
                };
                let target_card  = erd_target_card(&this_schema, &this_name, &t_idx, &a_rows, &parts_for_target);
                let band_cap     = cards_per_band.get();
                let (parent_cards, parent_overflow) = erd_parent_cards(&this_schema, &this_name, &a_rows, band_cap);
                let (gp_cards,     gp_overflow)     = erd_grandparent_cards(&this_schema, &this_name, &a_rows, band_cap);
                let (child_cards,  child_overflow)  = erd_child_cards(&this_schema, &this_name, &d_rows, band_cap);
                let (gc_cards,     gc_overflow)     = erd_grandchild_cards(&this_schema, &this_name, &d_rows, band_cap);
                // Silence unused warnings until Phase 3 wires the pill render.
                let _ = (&parent_overflow, &gp_overflow, &child_overflow, &gc_overflow);

                // All-fields swap. When keys_only is OFF and the bulk-columns
                // data is loaded, replace each card's key-row list with its
                // full-column list (rows tinted only when PK or FK; plain
                // otherwise). Target uses its own already-loaded `t_cols` +
                // PRIMARY index; neighbours use the cached map. Cards whose
                // table isn't in the map keep their key-only column list as
                // a graceful fallback.
                let (target_card, parent_cards, gp_cards, child_cards, gc_cards) = if !keys_only.get() {
                    if let LoadState::Loaded(map) = neighbour_cols_sig.get() {
                        let target_pk: Vec<String> = t_idx.iter()
                            .find(|i| i.name.eq_ignore_ascii_case("PRIMARY"))
                            .map(|i| i.columns.clone())
                            .unwrap_or_default();
                        let target_all: Vec<(String, bool)> = t_cols.iter()
                            .map(|c| (c.name.clone(), target_pk.iter().any(|p| p == &c.name)))
                            .collect();
                        let mut target_fk: Vec<String> = Vec::new();
                        for e in a_rows.iter().filter(|e| e.depth == 1) {
                            for c in &e.from_columns { target_fk.push(c.clone()); }
                        }
                        let mut target_full = erd_build_card_full(
                            &this_schema, &this_name, 0, true, &target_all, &target_fk);
                        // Preserve partitions across the keys-only → all-fields
                        // rebuild so the focal card keeps its partition list.
                        target_full.partitions = parts_for_target.iter()
                            .map(|p| (p.schema.clone(), p.name.clone()))
                            .collect();

                        let inflate = |card: ErdCard| -> ErdCard {
                            let key = (card.schema.clone(), card.table.clone());
                            let Some(all) = map.get(&key) else { return card };
                            // FK columns: every column on this card already
                            // tagged Fk or PkFk in the keys-only build, plus
                            // any column appearing in `from_columns` of an
                            // edge originating here (depth-2 bridges).
                            let fk: Vec<String> = card.columns.iter()
                                .filter_map(|(n, t)| match t {
                                    Some(ErdTag::Fk) | Some(ErdTag::PkFk) => Some(n.clone()),
                                    _ => None,
                                })
                                .collect();
                            let mut rebuilt = erd_build_card_full(&card.schema, &card.table, card.gen, card.is_target, all, &fk);
                            // Carry partitions over — only target ever has any
                            // populated, but the assignment is harmless for
                            // non-target cards (their list stays empty).
                            rebuilt.partitions = card.partitions;
                            rebuilt
                        };
                        let inflate_vec = |v: Vec<ErdCard>| -> Vec<ErdCard> {
                            v.into_iter().map(inflate).collect()
                        };
                        (target_full, inflate_vec(parent_cards), inflate_vec(gp_cards),
                         inflate_vec(child_cards), inflate_vec(gc_cards))
                    } else {
                        (target_card, parent_cards, gp_cards, child_cards, gc_cards)
                    }
                } else {
                    (target_card, parent_cards, gp_cards, child_cards, gc_cards)
                };

                // Determine which generations are populated; assign x positions
                // left to right with band collapse.
                let mut populated: Vec<ErdGen> = Vec::new();
                if !gp_cards.is_empty()      { populated.push(-2); }
                if !parent_cards.is_empty()  { populated.push(-1); }
                populated.push(0);
                if !child_cards.is_empty()   { populated.push(1); }
                if !gc_cards.is_empty()      { populated.push(2); }

                // Band gap scales with whichever is bigger: the tallest card
                // on the canvas (so curves spanning that height have room),
                // or the maximum lane spread needed to give every source
                // card its own vertical runway. The keys-only constant
                // 110px holds for compact cards in narrow bands; wider
                // cards or larger neighbour stacks blow past that.
                let max_card_h = std::iter::once(target_card.height())
                    .chain(parent_cards.iter().map(|c| c.height()))
                    .chain(gp_cards.iter().map(|c| c.height()))
                    .chain(child_cards.iter().map(|c| c.height()))
                    .chain(gc_cards.iter().map(|c| c.height()))
                    .max()
                    .unwrap_or(0);
                let max_band_size = parent_cards.len()
                    .max(gp_cards.len())
                    .max(child_cards.len())
                    .max(gc_cards.len()) as i32;
                // Lane STEP=40 in the lane-assignment pass; (N-1)*40 is the
                // total spread, plus base curve room (~80) so the curves
                // don't degenerate into straight chords.
                let lane_band_min = (max_band_size - 1).max(0) * 40 + 80;
                let band_gap = ERD_BAND_GAP
                    .max((max_card_h as f32 * 0.55) as i32)
                    .max(lane_band_min);

                let mut x_by_gen: HashMap<ErdGen, i32> = HashMap::new();
                let mut cursor = ERD_SIDE_MARGIN;
                for g in &populated {
                    x_by_gen.insert(*g, cursor);
                    let w = if *g == 0 { ERD_TARGET_CARD_W } else { ERD_CARD_W };
                    cursor += w + band_gap;
                }

                // Visible-card key set, plus the focal table. Edge specs
                // whose either endpoint isn't visible are dropped — keeps
                // the canvas tidy when a band is capped (hidden tables don't
                // render, so their edges have nowhere to land). The pill
                // signals the existence of those connections without
                // drawing dangling stubs.
                let mut visible_keys: std::collections::HashSet<(String, String)> =
                    std::collections::HashSet::new();
                visible_keys.insert((this_schema.clone(), this_name.clone()));
                for band in [&parent_cards, &gp_cards, &child_cards, &gc_cards] {
                    for c in band {
                        visible_keys.insert((c.schema.clone(), c.table.clone()));
                    }
                }
                // Collect all edges in the spec's (from, to) form (parent →
                // child). For ancestors: from = ancestor, to = focal (depth 1)
                // or from = ancestor, to = bridge (depth 2). For descendants:
                // from = focal/bridge, to = descendant. Specs targeting a
                // hidden card (see `visible_keys` above) are skipped.
                let mut edge_specs: Vec<(String, String, String, String, String, String)> = Vec::new();
                for e in &a_rows {
                    let from_col = e.to_columns.first().cloned().unwrap_or_default();
                    let to_col   = e.from_columns.first().cloned().unwrap_or_default();
                    if e.depth == 1 {
                        let from = (e.schema.clone(), e.table.clone());
                        if !visible_keys.contains(&from) { continue; }
                        edge_specs.push((
                            e.schema.clone(), e.table.clone(), from_col,
                            this_schema.clone(), this_name.clone(), to_col,
                        ));
                    } else if e.depth == 2 && !e.via_table.is_empty() {
                        let from = (e.schema.clone(), e.table.clone());
                        let via  = (e.via_schema.clone(), e.via_table.clone());
                        if !visible_keys.contains(&from) || !visible_keys.contains(&via) { continue; }
                        edge_specs.push((
                            e.schema.clone(), e.table.clone(), from_col,
                            e.via_schema.clone(), e.via_table.clone(), to_col,
                        ));
                    }
                }
                for e in &d_rows {
                    let from_col = e.to_columns.first().cloned().unwrap_or_default();
                    let to_col   = e.from_columns.first().cloned().unwrap_or_default();
                    if e.depth == 1 {
                        let to = (e.schema.clone(), e.table.clone());
                        if !visible_keys.contains(&to) { continue; }
                        edge_specs.push((
                            this_schema.clone(), this_name.clone(), from_col,
                            e.schema.clone(), e.table.clone(), to_col,
                        ));
                    } else if e.depth == 2 && !e.via_table.is_empty() {
                        let via = (e.via_schema.clone(), e.via_table.clone());
                        let to  = (e.schema.clone(), e.table.clone());
                        if !visible_keys.contains(&via) || !visible_keys.contains(&to) { continue; }
                        edge_specs.push((
                            e.via_schema.clone(), e.via_table.clone(), from_col,
                            e.schema.clone(), e.table.clone(), to_col,
                        ));
                    }
                }

                // Layout: place target first, then walk parent chain (gen -1,
                // -2) aligning each parent's PK row to the FK row of its child
                // in the next band. Then place children/grandchildren.
                let target_x = *x_by_gen.get(&0).unwrap();
                // Headroom of 200px gives the parent chain room to grow above
                // target without the final-normalization pass having to shift
                // everything. Final normalization still kicks in on schemas
                // with many parents stacked above, but for typical shapes the
                // initial placement lands inside the visible canvas.
                let target_y = 200;
                let placed_target = vec![(target_card, target_x, target_y)];

                let mut placed_parents = parent_cards.into_iter()
                    .map(|c| { let x = *x_by_gen.get(&-1).unwrap_or(&0); (c, x, target_y) })
                    .collect::<Vec<_>>();
                erd_place_outward_layer(&mut placed_parents, &placed_target, &edge_specs);

                let mut placed_gp = gp_cards.into_iter()
                    .map(|c| { let x = *x_by_gen.get(&-2).unwrap_or(&0); (c, x, target_y) })
                    .collect::<Vec<_>>();
                erd_place_outward_layer(&mut placed_gp, &placed_parents, &edge_specs);

                let mut placed_children = child_cards.into_iter()
                    .map(|c| { let x = *x_by_gen.get(&1).unwrap_or(&0); (c, x, target_y) })
                    .collect::<Vec<_>>();
                erd_place_children(&mut placed_children, &placed_target, &edge_specs);

                let mut placed_gc = gc_cards.into_iter()
                    .map(|c| { let x = *x_by_gen.get(&2).unwrap_or(&0); (c, x, target_y) })
                    .collect::<Vec<_>>();
                erd_place_grandchildren(&mut placed_gc, &placed_children, &edge_specs);

                // Final normalization: shift everything down so the topmost
                // card sits at >= ERD_TOP_MARGIN.
                let mut min_y = i32::MAX;
                for v in [&placed_gp, &placed_parents, &placed_target, &placed_children, &placed_gc] {
                    for (_, _, y) in v { if *y < min_y { min_y = *y; } }
                }
                let shift = if min_y < ERD_TOP_MARGIN { ERD_TOP_MARGIN - min_y } else { 0 };
                let apply_shift = |v: Vec<(ErdCard, i32, i32)>| -> Vec<(ErdCard, i32, i32)> {
                    v.into_iter().map(|(c, x, y)| (c, x, y + shift)).collect()
                };
                let placed_gp       = apply_shift(placed_gp);
                let placed_parents  = apply_shift(placed_parents);
                let placed_target   = apply_shift(placed_target);
                let placed_children = apply_shift(placed_children);
                let placed_gc       = apply_shift(placed_gc);

                // ── "+N more" overflow pills ───────────────────────────────────
                // Each capped band gets one pill at the bottom of the band,
                // x-aligned with the band's cards. Computed AFTER apply_shift
                // so the pill's y picks up the centering shift. The pill
                // participates in the stage bbox below so the canvas grows
                // tall enough to enclose it without scrolling past.
                //
                // ERD_OVERFLOW_PILL_H is intentionally short (one row) — the
                // pill is a visual annotation, not a real card. ERD_MIN_CARD_GAP
                // separates it from the last real card so the user reads them
                // as distinct objects.
                const ERD_OVERFLOW_PILL_H: i32 = 26;
                let pill_for_band = |overflow: &Option<ErdOverflow>, band_cards: &[(ErdCard, i32, i32)]|
                    -> Option<ErdPill>
                {
                    let ovf = overflow.as_ref()?;
                    let (last_card, x, y) = band_cards.last()?;
                    let pill_x = *x;
                    let pill_y = y + last_card.height() + ERD_MIN_CARD_GAP;
                    let pill_w = last_card.width();
                    Some((ovf.band, pill_x, pill_y, pill_w, ovf.hidden.len(), ovf.hidden.clone()))
                };
                let pills: Vec<ErdPill> = [
                    pill_for_band(&gp_overflow,     &placed_gp),
                    pill_for_band(&parent_overflow, &placed_parents),
                    pill_for_band(&child_overflow,  &placed_children),
                    pill_for_band(&gc_overflow,     &placed_gc),
                ].into_iter().flatten().collect();

                let all_placed: Vec<(ErdCard, i32, i32)> = placed_gp.iter()
                    .chain(placed_parents.iter())
                    .chain(placed_target.iter())
                    .chain(placed_children.iter())
                    .chain(placed_gc.iter())
                    .cloned()
                    .collect();

                // Edge anchor computation. Pinned-Perfect rule: pin edges
                // whose ideal y falls inside the parent card's vertical
                // extent; place the rest in the largest gap between pinned
                // anchors.
                let mut edges = erd_compute_edges(&edge_specs, &all_placed, &this_schema, &this_name);

                // Per-source-card lane shift. Every card in a band gets a
                // vertical "runway" through the gap to its right, ranked by
                // its position in the band (top → bend late / right; bottom
                // → bend early / left). All edges leaving the same source
                // card share the same shift, so a single card's outgoing
                // curves stay parallel in the lane while different source
                // cards' curves don't pile up on the same vertical sweep.
                const LANE_STEP: f32 = 40.0;
                let mut band_rank: HashMap<(String, String), (i32, i32)> = HashMap::new();
                let band_vecs: [&Vec<(ErdCard, i32, i32)>; 4] =
                    [&placed_gp, &placed_parents, &placed_children, &placed_gc];
                for band in &band_vecs {
                    let n = band.len() as i32;
                    for (i, (card, _, _)) in band.iter().enumerate() {
                        band_rank.insert((card.schema.clone(), card.table.clone()), (i as i32, n));
                    }
                }
                for e in edges.iter_mut() {
                    if e.py == e.cy { continue; }  // same-row Pinned-Perfect stays flat
                    let Some(&(rank, total)) = band_rank.get(&e.from_key) else { continue };
                    if total < 2 { continue; }
                    let center = (total as f32 - 1.0) / 2.0;
                    e.lane_shift = ((center - rank as f32) * LANE_STEP) as i32;
                }

                // Stage size = bounding box of all cards + overflow pills +
                // bottom/right pad. Pills are excluded from card lookups but
                // included in the bbox so the canvas tall enough to enclose
                // every pill (which sits below its band's last card).
                let mut max_x = 0;
                let mut max_y = 0;
                for (_band, px, py, pw, _n, _hidden) in &pills {
                    let r = px + pw;
                    let b = py + ERD_OVERFLOW_PILL_H;
                    if r > max_x { max_x = r; }
                    if b > max_y { max_y = b; }
                }
                for (c, x, y) in &all_placed {
                    let r = x + c.width();
                    let b = y + c.height();
                    if r > max_x { max_x = r; }
                    if b > max_y { max_y = b; }
                }
                let stage_w = max_x + ERD_RIGHT_PAD;
                let stage_h = max_y + ERD_BOTTOM_PAD;

                // Adjacency map for focus mode. For each (schema, table)
                // key, the set of keys it shares an edge with — covers both
                // directions of every edge so hovering either end lights
                // the other.
                let mut adjacency: HashMap<(String, String), std::collections::HashSet<(String, String)>> =
                    HashMap::new();
                for e in &edges {
                    adjacency.entry(e.from_key.clone()).or_default().insert(e.to_key.clone());
                    adjacency.entry(e.to_key.clone()).or_default().insert(e.from_key.clone());
                }

                let z = zoom.get();
                let scaled_w = (stage_w as f64 * z).ceil() as i32;
                let scaled_h = (stage_h as f64 * z).ceil() as i32;

                // Empty-state hint: when the table genuinely has no FK
                // relationships (max depth = 0), show a small caption under
                // the focal card so the lone-card canvas reads as
                // intentional rather than broken. Scoping to
                // `max_depth_available == 0` (not just `populated.len()
                // == 1`) avoids the false hit when the user has dialed
                // generations down to 0 on a table that DOES have
                // relationships.
                let show_empty_hint = max_depth_available.get() == 0;
                let (target_x_pos, target_y_pos, target_w_val, target_h_val) = {
                    let (c, x, y) = placed_target.first().expect("target always placed");
                    (*x, *y, c.width(), c.height())
                };

                view! {
                    // The wrap takes the SCALED dimensions so the scroll
                    // container's scroll bars track the visual canvas size;
                    // the stage inside is at the natural unscaled size and
                    // gets visually shrunk/grown via transform: scale.
                    <div class="erd-zoom-wrap"
                         style=format!("position: relative; width: {}px; height: {}px;", scaled_w, scaled_h)>
                        <div class="erd-stage"
                             style=format!("position: absolute; left: 0; top: 0; width: {}px; height: {}px; transform: scale({}); transform-origin: 0 0;", stage_w, stage_h, z)>
                            <svg class="erd-edges-svg"
                                 style=format!("position: absolute; inset: 0; width: {}px; height: {}px; pointer-events: none;", stage_w, stage_h)
                                 width={stage_w} height={stage_h}
                                 viewBox={format!("0 0 {} {}", stage_w, stage_h)}>
                                {edges.iter().map(|e| erd_render_edge(e, hovered)).collect_view()}
                            </svg>
                            <div class="erd-cards-layer"
                                 style="position: absolute; inset: 0;">
                                {all_placed.iter().map(|(c, x, y)| {
                                    let key = (c.schema.clone(), c.table.clone());
                                    let neighbors = adjacency.get(&key).cloned().unwrap_or_default();
                                    erd_render_card(c, *x, *y, hovered, neighbors,
                                                    plane, table_cache, selected, ws, session)
                                }).collect_view()}
                                {pills.iter().map(|(_band, x, y, w, n, hidden)| {
                                    let style = format!(
                                        "left:{}px;top:{}px;width:{}px;height:{}px",
                                        x, y, w, ERD_OVERFLOW_PILL_H
                                    );
                                    let label = format!("+{} more", n);
                                    let hidden_for_click = hidden.clone();
                                    let popover_sig = detail.erd_overflow_popover;
                                    view! {
                                        <div class="erd-overflow-pill" style=style
                                             title="Click to list the hidden tables"
                                             on:click=move |ev: web_sys::MouseEvent| {
                                                 // Anchor to the pill's viewport
                                                 // rect rather than the click point —
                                                 // the popover lands beneath the pill
                                                 // even when the user clicks the
                                                 // pill's edge or arrives via keyboard.
                                                 use wasm_bindgen::JsCast;
                                                 let (px, py) = ev.current_target()
                                                     .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                                                     .map(|el| {
                                                         let r = el.get_bounding_client_rect();
                                                         (r.left(), r.bottom())
                                                     })
                                                     .unwrap_or((ev.client_x() as f64, ev.client_y() as f64));
                                                 popover_sig.set(Some(ErdOverflowPopover {
                                                     x: px,
                                                     y: py,
                                                     hidden: hidden_for_click.clone(),
                                                 }));
                                             }>
                                            {label}
                                        </div>
                                    }.into_view()
                                }).collect_view()}
                                {if show_empty_hint {
                                    view! {
                                        <div class="erd-empty-hint"
                                             style=format!("position: absolute; left: {}px; top: {}px; width: {}px;",
                                                           target_x_pos, target_y_pos + target_h_val + 32, target_w_val)>
                                            "No foreign-key relationships."
                                        </div>
                                    }.into_view()
                                } else {
                                    view! { <div/> }.into_view()
                                }}
                            </div>
                        </div>
                    </div>
                }.into_view()
            }}
            </div>
            // ── "+N more" overflow popover ────────────────────────────────
            // Rendered at the .erd-tab-root scope so the popover floats
            // above the canvas (escape the transformed/scaled stage).
            // Backdrop click and Esc close; row click navigates the
            // inspector to that table via the same path the card-name
            // click takes.
            {
                let popover_sig = detail.erd_overflow_popover;
                let table_cache_inner = table_cache;
                move || popover_sig.get().map(|state| {
                    let pos_style = format!("left:{}px; top:{}px", state.x, state.y);
                    let hidden_rows = state.hidden.clone();
                    view! {
                        <div class="erd-overflow-popover-backdrop"
                             on:mousedown=move |_| popover_sig.set(None)
                             on:contextmenu=move |e: web_sys::MouseEvent| {
                                 e.prevent_default();
                                 popover_sig.set(None);
                             } />
                        <div class="erd-overflow-popover"
                             style=pos_style
                             on:mousedown=move |e: web_sys::MouseEvent| {
                                 use wasm_bindgen::JsCast;
                                 let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                             }>
                            <div class="erd-overflow-popover-header">
                                {format!("{} hidden table{}", hidden_rows.len(),
                                    if hidden_rows.len() == 1 { "" } else { "s" })}
                            </div>
                            {hidden_rows.into_iter().map(|(s, t, fk_n)| {
                                let label = format!("{}.{}", s, t);
                                let fk_label = if fk_n == 1 { "1 fk col".to_string() }
                                               else        { format!("{} fk cols", fk_n) };
                                let s_for_click = s.clone();
                                let t_for_click = t.clone();
                                view! {
                                    <div class="erd-overflow-popover-row"
                                         on:click=move |_| {
                                             let s2 = s_for_click.clone();
                                             let t2 = t_for_click.clone();
                                             let new_detail = ensure_table_detail_loaded(
                                                 plane, &s2, &t2, table_cache_inner, ws, session);
                                             new_detail.active_tab.set(DetailTab::Erd);
                                             selected.set(Some((s2, t2)));
                                             popover_sig.set(None);
                                         }>
                                        <span class="erd-overflow-popover-name">{label}</span>
                                        <span class="erd-overflow-popover-meta">{fk_label}</span>
                                    </div>
                                }.into_view()
                            }).collect_view()}
                        </div>
                    }.into_view()
                })
            }
        </div>
    }.into_view()
}

/// Place a layer that lies OUTWARD from its already-placed neighbour layer
/// (parent gen -1 with target as inner; gen -2 with gen -1 as inner). For
/// each card, compute the desired y as the average of: inner_card.y +
/// inner_fk_offset - this_pk_offset across all edges to the inner layer.
fn erd_place_outward_layer(
    layer: &mut [(ErdCard, i32, i32)],
    inner: &[(ErdCard, i32, i32)],
    edges: &[(String, String, String, String, String, String)],
) {
    if layer.is_empty() { return; }
    // Compute desired y for each card.
    for (card, _x, y) in layer.iter_mut() {
        let mut sum: i64 = 0;
        let mut n: i64 = 0;
        for (fs, ft, fc, ts, tt, tc) in edges {
            if fs == &card.schema && ft == &card.table {
                if let Some((inner_card, _ix, iy)) = erd_find_placed(inner, ts, tt) {
                    let inner_fk_y = iy + erd_col_y_offset(inner_card, tc);
                    let pk_off     = erd_col_y_offset(card, fc);
                    sum += (inner_fk_y - pk_off) as i64;
                    n += 1;
                }
            }
        }
        if n > 0 { *y = (sum / n) as i32; }
    }
    erd_resolve_overlaps(layer);
}

/// Place children: distribute their FK rows evenly across target's right-
/// edge inner range, then resolve overlaps with min vertical gap.
fn erd_place_children(
    layer: &mut [(ErdCard, i32, i32)],
    target_placed: &[(ErdCard, i32, i32)],
    edges: &[(String, String, String, String, String, String)],
) {
    if layer.is_empty() { return; }
    let Some((tcard, _tx, ty)) = target_placed.first() else { return };
    let inner_pad = 18;
    let top = ty + inner_pad;
    let bot = ty + tcard.height() - inner_pad;
    let n = layer.len() as i32;
    for (i, (card, _x, y)) in layer.iter_mut().enumerate() {
        // Desired anchor y on target's right edge.
        let anchor_y = if n == 1 {
            (top + bot) / 2
        } else {
            top + (bot - top) * (i as i32) / (n - 1)
        };
        // FK column on this child that points to target.
        let fk_col = edges.iter()
            .find(|(fs, ft, _, ts, tt, _)| fs == &tcard.schema && ft == &tcard.table
                                        && ts == &card.schema && tt == &card.table)
            .map(|(_, _, _, _, _, tc)| tc.clone())
            .unwrap_or_default();
        let fk_off = erd_col_y_offset(card, &fk_col);
        *y = anchor_y - fk_off;
    }
    erd_resolve_overlaps(layer);
}

/// Place grandchildren: average ideal y across each parent (gen +1) of this
/// grandchild, then resolve overlaps.
fn erd_place_grandchildren(
    layer: &mut [(ErdCard, i32, i32)],
    children_placed: &[(ErdCard, i32, i32)],
    edges: &[(String, String, String, String, String, String)],
) {
    if layer.is_empty() { return; }
    for (card, _x, y) in layer.iter_mut() {
        let mut sum: i64 = 0;
        let mut n: i64 = 0;
        for (fs, ft, fc, ts, tt, tc) in edges {
            if ts == &card.schema && tt == &card.table {
                if let Some((p_card, _px, py)) = erd_find_placed(children_placed, fs, ft) {
                    let p_pk_y = py + erd_col_y_offset(p_card, fc);
                    let fk_off = erd_col_y_offset(card, tc);
                    sum += (p_pk_y - fk_off) as i64;
                    n += 1;
                }
            }
        }
        if n > 0 { *y = (sum / n) as i32; }
    }
    erd_resolve_overlaps(layer);
}

/// Build the renderable edge list with anchors placed via the Pinned-Perfect
/// / Largest-Gap rule (§7b). The CHILD anchor is precise (FK column row);
/// the PARENT anchor is forgiving — it pins at ideal y when in range, else
/// floats into the largest free gap on that side.
fn erd_compute_edges(
    edge_specs: &[(String, String, String, String, String, String)],
    placed:     &[(ErdCard, i32, i32)],
    focal_s:    &str,
    focal_t:    &str,
) -> Vec<ErdEdge> {
    // For each (parent_table, side) group, batch edges and place anchors.
    // Group key: (from_schema, from_table, side).
    #[derive(Clone)]
    struct Pending<'a> {
        e: &'a (String, String, String, String, String, String),
        side: ErdSide,
        ideal_y: i32,
        cx: i32, cy: i32,
    }
    let mut groups: BTreeMap<(String, String, i32), Vec<Pending>> = BTreeMap::new();
    for e in edge_specs {
        let (fs, ft, _fc, ts, tt, tc) = e;
        let Some((p_card, p_x, _p_y)) = erd_find_placed(placed, fs, ft) else { continue };
        let Some((c_card, c_x, c_y)) = erd_find_placed(placed, ts, tt) else { continue };
        let parent_cx = p_x + p_card.width() / 2;
        let child_cx  = c_x + c_card.width() / 2;
        let p_side = if child_cx > parent_cx { ErdSide::Right } else { ErdSide::Left };
        let c_side = if parent_cx < child_cx { ErdSide::Left } else { ErdSide::Right };
        // Child anchor: precise at FK column row.
        let cx = match c_side { ErdSide::Right => c_x + c_card.width(), ErdSide::Left => *c_x };
        let cy = c_y + erd_col_y_offset(c_card, tc);
        let ideal_y = cy;
        let key = (fs.clone(), ft.clone(), match p_side { ErdSide::Right => 1, ErdSide::Left => 0 });
        groups.entry(key).or_default().push(Pending {
            e, side: p_side, ideal_y, cx, cy,
        });
    }

    let mut out: Vec<ErdEdge> = Vec::new();
    for (key, items) in &groups {
        let (fs, ft, _) = key;
        let Some((p_card, p_x, p_y)) = erd_find_placed(placed, fs, ft) else { continue };
        let side = items[0].side;
        let anchor_x = match side { ErdSide::Right => p_x + p_card.width(), ErdSide::Left => *p_x };
        let min_y = p_y + ERD_INNER_PAD;
        let max_y = p_y + p_card.height() - ERD_INNER_PAD;

        // Pinned: ideal_y in range.
        let mut placed_y: Vec<Option<i32>> = vec![None; items.len()];
        for (i, it) in items.iter().enumerate() {
            if it.ideal_y >= min_y && it.ideal_y <= max_y {
                placed_y[i] = Some(it.ideal_y);
            }
        }
        // Floating: largest-gap placement, processed in ideal-y order.
        let mut floating_idxs: Vec<usize> = (0..items.len()).filter(|i| placed_y[*i].is_none()).collect();
        floating_idxs.sort_by_key(|i| items[*i].ideal_y);
        for fi in floating_idxs {
            let mut occupied: Vec<i32> = placed_y.iter().filter_map(|v| *v).collect();
            occupied.sort();
            let mut boundaries: Vec<i32> = Vec::with_capacity(occupied.len() + 2);
            boundaries.push(min_y);
            boundaries.extend(occupied);
            boundaries.push(max_y);
            let mut best: Option<(i32, i32)> = None; // (mid, size)
            for w in boundaries.windows(2) {
                let (a, b) = (w[0], w[1]);
                let pad_a = if a == min_y { 0 } else { ERD_FLOAT_SPACING };
                let pad_b = if b == max_y { 0 } else { ERD_FLOAT_SPACING };
                let start = a + pad_a;
                let end   = b - pad_b;
                let size  = end - start;
                if size > 0 {
                    let mid = (start + end) / 2;
                    if best.is_none() || size > best.unwrap().1 { best = Some((mid, size)); }
                }
            }
            let placed_at = match best {
                Some((m, _)) => m,
                None => if items[fi].ideal_y > max_y { max_y } else { min_y },
            };
            placed_y[fi] = Some(placed_at);
        }

        // Emit the edge. The Bézier renderer derives its control-point x
        // from (px, cx, py, cy) directly, so no per-edge routing data is
        // needed beyond the four anchor coordinates.
        for (i, it) in items.iter().enumerate() {
            let py = placed_y[i].unwrap_or(it.ideal_y);
            let is_target = it.e.0 == focal_s && it.e.1 == focal_t
                          || it.e.3 == focal_s && it.e.4 == focal_t;
            let _ = side;  // p_side kept implicit (anchor_x already encodes it)
            out.push(ErdEdge {
                px: anchor_x, py,
                cx: it.cx, cy: it.cy,
                is_target,
                from_key:   (it.e.0.clone(), it.e.1.clone()),
                to_key:     (it.e.3.clone(), it.e.4.clone()),
                lane_shift: 0,
            });
        }
    }
    // No lane-shift pass for curves: shifting upgoing and downgoing curves
    // sideways in a shared gap creates more crossings than it eliminates.
    // Curves diverge naturally via different (cx, cy) endpoints, and the
    // wider dynamic band-gap (computed by the caller) gives them room.
    out
}

pub(crate) fn render_ddl_tab(ddl_signal: RwSignal<LoadState<String>>) -> leptos::View {
    view! {
        {move || match ddl_signal.get() {
            LoadState::NotLoaded | LoadState::Loading => view! {
                <div class="detail-msg">"Loading…"</div>
            }.into_view(),
            LoadState::Error(e) => view! {
                <div class="detail-error">{e}</div>
            }.into_view(),
            LoadState::Loaded(ddl) => view! {
                <pre class="detail-ddl">{ddl}</pre>
            }.into_view(),
        }}
    }.into_view()
}

/// Render the Storage tab — the table's physical shape.
///
/// Sections, top to bottom:
///   Geometry        → Storage factor / Pages / Ideal / Avg row / Dead tuples
///                     (from the `status:*` overview)
///   Autovacuum      → trigger table + settings table + Run action
///                     (from `autovac:*`, see `autovac.rs`)
///   Visibility map  → all-visible / all-frozen (from `vm:*`)
///
/// Each section is reactive on its own signal, so one refetch doesn't
/// blank the others. A section with nothing to show renders nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_storage_tab(
    detail:         TableDetail,
    schema:         String,
    name:           String,
    explorer_pane_id: String,
    shell_panes:    RwSignal<HashMap<String, ShellTab>>,
    pane_ops:       PaneOps,
) -> leptos::View {
    let overview_sig = detail.overview;
    let vm_sig       = detail.vm;
    let autovac_sig  = detail.autovac;
    view! {
        <div class="schema-scroll">
            {move || match overview_sig.get() {
                LoadState::NotLoaded | LoadState::Loading => view! {
                    <div class="detail-msg">"Loading storage detail…"</div>
                }.into_view(),
                LoadState::Error(e) => view! {
                    <div class="detail-error">{e}</div>
                }.into_view(),
                LoadState::Loaded(ov) => {
                    // Assemble geometry-band cells with the same
                    // MetaCell pattern the header uses.
                    let mut geometry: Vec<MetaCell> = Vec::new();
                    if let (Some(value), Some(sev), Some(title)) = (
                        ov.storage_factor.clone(),
                        ov.storage_severity,
                        ov.storage_tooltip.clone(),
                    ) {
                        if !value.is_empty() {
                            geometry.push(MetaCell::Severity {
                                key: "Storage factor", value, sev, title,
                            });
                        }
                    }
                    if !ov.data_pages.is_empty() {
                        let (key, value) = if !ov.data_pages_ideal.is_empty() {
                            ("Pages / Ideal",
                             format!("~{} / ~{}", ov.data_pages, ov.data_pages_ideal))
                        } else {
                            ("Pages", format!("~{}", ov.data_pages))
                        };
                        geometry.push(MetaCell::Simple { key, value });
                    }
                    if !ov.avg_row_len.is_empty() {
                        geometry.push(MetaCell::Simple {
                            key: "Avg row",
                            value: ov.avg_row_len.clone(),
                        });
                    }

                    // Dead tuples measure garbage, not trigger distance —
                    // bloat-adjacent, so it sits with the geometry cells.
                    if let (Some(value), Some(sev), Some(title)) = (
                        ov.dead_tup_pct.clone(),
                        ov.dead_tup_severity,
                        ov.dead_tup_tooltip.clone(),
                    ) {
                        if !value.is_empty() {
                            geometry.push(MetaCell::Severity {
                                key: "Dead tuples", value, sev, title,
                            });
                        }
                    }

                    if geometry.is_empty() {
                        return view! {
                            <div class="detail-msg">
                                "No storage detail available for this table."
                            </div>
                        }.into_view();
                    }

                    view! {
                        <div class="schema-section-header">"Geometry"</div>
                        {render_meta_table(geometry)}
                    }.into_view()
                }
            }}

            /* Visibility map — its own `vm:*` signal, so it refreshes
               independently. Nothing while the first load is in flight
               or when the plane returned no row (non-PG / relation
               gone); the server's message when the query failed. */
            {move || match vm_sig.get() {
                LoadState::Loaded(Some(vm)) => render_vm_section(vm),
                LoadState::Error(e) => render_section_error("Visibility map", e),
                _ => view! { <div/> }.into_view(),
            }}

            /* Autovacuum section — knobs and counters arrive together
               in the `autovac:*` payload, so this block depends on
               nothing else. Renders nothing while loading, or when the
               payload is empty (non-PG connection / relation not
               found). */
            {
                let schema_ac = schema.clone();
                let name_ac   = name.clone();
                let explorer_ac = explorer_pane_id.clone();
                move || {
                    let av = match autovac_sig.get() {
                        LoadState::Loaded(Some(av)) => av,
                        LoadState::Error(e) => return render_section_error("Autovacuum", e),
                        _ => return view! { <div/> }.into_view(),
                    };
                    render_autovac_section(
                        av, detail,
                        schema_ac.clone(),
                        name_ac.clone(),
                        explorer_ac.clone(),
                        shell_panes, pane_ops,
                    )
                }
            }

        </div>
    }.into_view()
}

/// Render the Storage tab's Visibility map section: a small table with
/// All-visible and All-frozen as rows (pages of total, bar, share), the
/// source as a badge, and a caption saying what the numbers mean and
/// how fresh they are. A row the source doesn't report is left out.
fn render_vm_section(vm: VmSummary) -> leptos::View {
    let live = vm.source == "pg_visibility";
    let (badge_text, badge_class, badge_title) = if live {
        ("via pg_visibility", "storage-vm-source-fresh",
         "Live per-page read of the visibility map (pg_visibility extension).")
    } else if vm.live_denied {
        ("via pg_class", "storage-vm-source-fallback",
         "Snapshot from the last VACUUM / ANALYZE (pg_class). pg_visibility \
          is installed, but this role isn't allowed to run \
          pg_visibility_map_summary().")
    } else {
        ("via pg_class", "storage-vm-source-fallback",
         "Snapshot from the last VACUUM / ANALYZE (pg_class). \
          Install the pg_visibility extension for a live read.")
    };
    let header = view! {
        <div class="schema-section-divider"></div>
        <div class="schema-section-header">
            <span>"Visibility map"</span>
            <span class=format!("storage-vm-source {badge_class}") title=badge_title>
                {badge_text}
            </span>
        </div>
    };

    let Some(total) = vm.total_pages.filter(|n| *n > 0) else {
        let why = if live {
            "The table has no pages yet."
        } else {
            "No pages recorded yet: the table is empty, or hasn't been \
             vacuumed or analyzed since it was loaded."
        };
        return view! { {header} <p class="av-prose">{why}</p> }.into_view();
    };

    let fmt = |n: u64| crate::format::format_int_with_commas_i64(n as i64);
    let row = |label: &'static str, pages: u64, sev: Severity| {
        let pct = 100.0 * pages as f64 / total as f64;
        let title = crate::storage_factor::build_vm_tooltip(label, pct, pages, total, &vm.source);
        view! {
            <tr title=title>
                <td class="av-key">{label}</td>
                <td class="av-num">{format!("{} of {}", fmt(pages), fmt(total))}</td>
                <td class="av-progress">{progress_view(pct, sev)}</td>
            </tr>
        }
    };
    let visible = vm.all_visible_pages.map(|p| {
        let pct = 100.0 * p as f64 / total as f64;
        row("All-visible", p, crate::storage_factor::classify_vm_visible_pct(pct))
    });
    // No severity on all-frozen: whether it's low enough to matter
    // depends on freeze age, which the header already shows.
    let frozen = vm.all_frozen_pages.map(|p| row("All-frozen", p, Severity::Neutral));

    let mut prose = String::from(
        "All-visible pages let index-only scans skip the heap; all-frozen \
         pages let anti-wraparound vacuums skip them.",
    );
    prose.push_str(if live {
        " Read live from the visibility map."
    } else if vm.live_denied {
        " Snapshot from the last VACUUM or ANALYZE, so writes since then \
         aren't reflected. pg_visibility is installed, but this role isn't \
         allowed to run pg_visibility_map_summary(), which reads it live \
         (a superuser, or a role granted EXECUTE on it, can)."
    } else {
        " Snapshot from the last VACUUM or ANALYZE, so writes since then \
         aren't reflected; the pg_visibility extension reads it live."
    });
    if vm.all_frozen_pages.is_none() {
        prose.push_str(" pg_class records all-frozen pages from PostgreSQL 18 on.");
    }
    if let Some(n) = vm.partitions {
        prose.push_str(&format!(
            " Summed across {n} partition{}.", if n == 1 { "" } else { "s" },
        ));
    }

    view! {
        {header}
        <table class="schema-table av-table">
            <thead>
                <tr>
                    <th>"State"</th>
                    <th class="av-num">"Pages"</th>
                    <th>"Share"</th>
                </tr>
            </thead>
            <tbody>{visible}{frozen}</tbody>
        </table>
        <p class="av-prose">{prose}</p>
    }.into_view()
}

/// A Storage-tab section whose query failed: header plus the server's
/// error, so the section says what went wrong instead of vanishing.
fn render_section_error(title: &'static str, message: String) -> leptos::View {
    view! {
        <div class="schema-section-divider"></div>
        <div class="schema-section-header">{title}</div>
        <div class="detail-error">{message}</div>
    }.into_view()
}

/// Thin severity-coloured bar with the percentage beside it — the
/// Storage tab's one way of showing "how far along". The bar caps at
/// 100%; the number doesn't. A sliver stays visible for tiny non-zero
/// values so "barely started" reads differently from "nothing yet".
fn progress_view(pct: f64, sev: Severity) -> leptos::View {
    let class = metric_class(sev);
    let width = if pct.is_finite() { pct.clamp(0.0, 100.0) } else { 100.0 };
    let width = if pct > 0.0 { width.max(1.5) } else { 0.0 };
    view! {
        <span class="av-bar">
            <span class=format!("av-bar-fill {class}") style=format!("width: {width:.1}%")></span>
        </span>
        <span class=format!("av-pct {class}")>{crate::autovac::progress_text(pct)}</span>
    }.into_view()
}

/// Key/value table on the `.schema-table` look — the Storage tab's
/// vertical alternative to the header's meta-grid, so a value sits next
/// to its label instead of across the panel. Severity cells colour their
/// value and carry their tooltip on the row.
fn render_meta_table(cells: Vec<MetaCell>) -> leptos::View {
    let rows = cells.into_iter().map(|cell| match cell {
        MetaCell::Simple { key, value } => view! {
            <tr>
                <td class="av-key">{key}</td>
                <td class="av-num">{value}</td>
            </tr>
        },
        MetaCell::Severity { key, value, sev, title } => view! {
            <tr title=title>
                <td class="av-key">{key}</td>
                <td class=format!("av-num {}", metric_class(sev))>{value}</td>
            </tr>
        },
    }).collect_view();
    view! {
        <table class="schema-table av-table">
            <tbody>{rows}</tbody>
        </table>
    }.into_view()
}

/// Render the Storage tab's Autovacuum section: a Triggers table ("how
/// close is autovacuum to firing?"), a Settings table ("why that
/// threshold?"), then the Run action panel. The maths and wording live
/// in `autovac.rs`; this function only lays them out. Partitioned
/// parents get a note instead of the tables, since autovacuum never
/// processes them.
#[allow(clippy::too_many_arguments)]
fn render_autovac_section(
    av: AutovacSettings,
    detail:         TableDetail,
    schema:         String,
    name:           String,
    explorer_pane_id: String,
    shell_panes:    RwSignal<HashMap<String, ShellTab>>,
    pane_ops:       PaneOps,
) -> leptos::View {
    use crate::autovac::AutovacView;
    let body = match crate::autovac::build(&av) {
        AutovacView::PartitionedParent => view! {
            <p class="av-prose">
                "Partitioned tables are not processed by autovacuum: each partition is \
                 vacuumed and analyzed on its own schedule. Statistics on the parent \
                 itself only refresh with a manual ANALYZE, so run one after loading \
                 data or when the distribution across partitions shifts."
            </p>
        }.into_view(),
        AutovacView::Table { banner, triggers, settings } => view! {
            {banner.map(|b| view! { <div class="av-banner">{b}</div> })}
            {render_autovac_triggers(triggers)}
            {render_autovac_settings(settings)}
        }.into_view(),
    };

    // Action panel — 4 checkboxes, live SQL preview, Run button.
    // Runs the composed statement on a Shell pane's session so its
    // output streams into the shell scrollback. The panel finds a
    // matching Shell pane by conn_id at Run time; if none exists it
    // surfaces an inline error and does nothing.
    let vac_ck   = detail.autovac_action_vacuum;
    let ana_ck   = detail.autovac_action_analyze;
    let frz_ck   = detail.autovac_action_freeze;
    let vbs_ck   = detail.autovac_action_verbose;
    let action_err = detail.autovac_action_error;
    let last_run   = detail.autovac_run;
    // The status reads the action shell's scrollback, which grows with
    // every line that shell prints (for any table). The memo recomputes
    // on each line but only notifies when the status itself changes.
    let run_status = create_memo(move |_| last_run.get()
        .map(|run| { let status = action_run_status(&run, shell_panes); (run, status) }));

    // A checkbox change clears a stale "couldn't start" message.
    let clear_err = move || action_err.set(None);

    let schema_run   = schema.clone();
    let name_run     = name.clone();
    let explorer_run = explorer_pane_id.clone();

    let action_panel = view! {
        <div class="storage-autovac-subheader">"Run action"</div>
        <div class="storage-autovac-action">
            <label class="storage-autovac-checkbox">
                <input type="checkbox"
                    prop:checked=move || vac_ck.get()
                    on:change=move |ev| {
                        use wasm_bindgen::JsCast;
                        let checked = ev.target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                            .map(|el| el.checked()).unwrap_or(false);
                        vac_ck.set(checked);
                        // Unchecking Vacuum implicitly unchecks Freeze
                        // (Freeze is a VACUUM modifier — makes no sense
                        // on its own).
                        if !checked { frz_ck.set(false); }
                        clear_err();
                    } />
                "Vacuum"
            </label>
            <label class="storage-autovac-checkbox">
                <input type="checkbox"
                    prop:checked=move || ana_ck.get()
                    on:change=move |ev| {
                        use wasm_bindgen::JsCast;
                        let checked = ev.target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                            .map(|el| el.checked()).unwrap_or(false);
                        ana_ck.set(checked);
                        clear_err();
                    } />
                "Analyze"
            </label>
            <label class="storage-autovac-checkbox"
                class:storage-autovac-checkbox-disabled=move || !vac_ck.get()>
                <input type="checkbox"
                    prop:checked=move || frz_ck.get() && vac_ck.get()
                    prop:disabled=move || !vac_ck.get()
                    on:change=move |ev| {
                        use wasm_bindgen::JsCast;
                        let checked = ev.target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                            .map(|el| el.checked()).unwrap_or(false);
                        frz_ck.set(checked);
                        clear_err();
                    } />
                "Freeze"
            </label>
            <label class="storage-autovac-checkbox">
                <input type="checkbox"
                    prop:checked=move || vbs_ck.get()
                    on:change=move |ev| {
                        use wasm_bindgen::JsCast;
                        let checked = ev.target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                            .map(|el| el.checked()).unwrap_or(false);
                        vbs_ck.set(checked);
                        clear_err();
                    } />
                "Verbose"
            </label>
        </div>
        {
            // Reactive SQL preview + Run button. Preview shows the
            // composed statement live as the DBA toggles boxes; the
            // Run button reads the same composition and dispatches.
            let schema_p = schema_run.clone();
            let name_p   = name_run.clone();
            move || {
                let sql = compose_vacuum_sql(
                    &schema_p, &name_p,
                    vac_ck.get(), ana_ck.get(), frz_ck.get(), vbs_ck.get(),
                );
                let run_disabled = sql.is_none();
                let preview_text = sql.clone().unwrap_or_else(||
                    "(pick Vacuum or Analyze — Freeze alone isn't valid without Vacuum)".to_string()
                );
                let sql_for_run = sql;
                let (explorer_c, schema_c, name_c) =
                    (explorer_run.clone(), schema_p.clone(), name_p.clone());
                // One run per table at a time: Run waits while the last
                // one is still opening its shell or executing.
                let in_flight = move || run_status.with(|r| r.as_ref().is_some_and(|(_, status)|
                    matches!(status, RunStatus::Opening | RunStatus::Running)));
                view! {
                    <div class="storage-autovac-preview">{preview_text}</div>
                    <div class="storage-autovac-run-row">
                        <button class="storage-autovac-run"
                            prop:disabled=move || run_disabled || in_flight()
                            on:click=move |_| {
                                let Some(sql) = sql_for_run.clone() else { return };
                                let started = pane_ops.run_in_action_shell.call(ActionShellRun {
                                    explorer_pane_id: explorer_c.clone(),
                                    schema:           schema_c.clone(),
                                    name:             name_c.clone(),
                                    sql:              sql.clone(),
                                });
                                match started {
                                    Some(pane_id) => {
                                        last_run.set(Some(ActionRun { pane_id, sql }));
                                        action_err.set(None);
                                    }
                                    None => action_err.set(Some(
                                        "Couldn't open a shell for this connection.".into())),
                                }
                            }>
                            "Run"
                        </button>
                        {move || run_status.get()
                            .map(|(run, status)| render_action_status(run, status, shell_panes, pane_ops))}
                        {move || action_err.get().map(|msg| view! {
                            <span class="storage-autovac-error">{msg}</span>
                        })}
                    </div>
                }.into_view()
            }
        }
    };

    view! {
        <div class="schema-section-divider"></div>
        <div class="schema-section-header">"Autovacuum"</div>
        <div class="av-section">
            {body}
            {action_panel}
        </div>
    }.into_view()
}

/// Where the last Run command stands, read from the action shell's own
/// state and scrollback — so it's right even if the Storage tab wasn't
/// on screen when the command finished.
#[derive(Clone, Debug, PartialEq)]
enum RunStatus {
    /// The shell is still connecting; the command runs when it opens.
    Opening,
    Running,
    Done { exec_time_us: u64 },
    Failed(String),
    /// The shell tab was closed. Nothing to link to: a reopened tab is
    /// a new pane with no history.
    Closed,
    /// The command's line is no longer in scrollback (buffer cleared).
    Unknown,
}

fn action_run_status(run: &ActionRun, shell_panes: RwSignal<HashMap<String, ShellTab>>) -> RunStatus {
    let Some(shell) = shell_panes.with(|m| m.get(&run.pane_id).cloned()) else {
        return RunStatus::Closed;
    };
    let history = shell.shell_history.get();
    let Some(at) = history.iter().rposition(|e|
        matches!(e, ShellEntry::Query { sql, .. } if *sql == run.sql))
    else {
        if shell.connecting.get() || shell.startup_sql.with(Option::is_some) {
            return RunStatus::Opening;
        }
        return match history.last() {
            // The shell couldn't connect (SessionError lands here).
            Some(ShellEntry::Error(msg) | ShellEntry::CliError { message: msg, .. }) => RunStatus::Failed(msg.clone()),
            _ => RunStatus::Unknown,
        };
    };
    for entry in &history[at + 1..] {
        match entry {
            ShellEntry::Notice { .. } => continue,
            ShellEntry::Info     { exec_time_us, .. }
            | ShellEntry::Table  { exec_time_us, .. }
            | ShellEntry::Vertical { exec_time_us, .. } =>
                return RunStatus::Done { exec_time_us: *exec_time_us },
            ShellEntry::Error(msg) | ShellEntry::CliError { message: msg, .. } => return RunStatus::Failed(msg.clone()),
            _ => break,
        }
    }
    RunStatus::Running
}

/// Status line beside Run: what the last command is doing, with a link
/// that brings its shell tab to the front.
fn render_action_status(
    run:         ActionRun,
    status:      RunStatus,
    shell_panes: RwSignal<HashMap<String, ShellTab>>,
    pane_ops:    PaneOps,
) -> leptos::View {
    if status == RunStatus::Closed {
        return view! { <span class="av-run-status av-run-closed">"shell tab closed"</span> }.into_view();
    }
    let shell_label = shell_panes.with_untracked(|m| m.get(&run.pane_id)
        .and_then(|t| t.custom_label.get_untracked())
        .unwrap_or_else(|| "shell".into()));
    let (class, text) = match &status {
        RunStatus::Opening => ("av-run-status", format!("\u{27F3} opening {shell_label}…")),
        RunStatus::Running => ("av-run-status", format!("\u{27F3} running in {shell_label}")),
        RunStatus::Done { exec_time_us } =>
            ("av-run-status av-run-done", format!("\u{2713} done in {}", format_elapsed(*exec_time_us / 1000))),
        RunStatus::Failed(msg) => ("av-run-status av-run-failed",
            format!("\u{2717} failed: {}", msg.lines().next().unwrap_or(msg))),
        RunStatus::Unknown | RunStatus::Closed => ("av-run-status", String::new()),
    };
    let link_text = if matches!(status, RunStatus::Opening | RunStatus::Running) {
        "view shell"
    } else {
        "view output"
    };
    let pane_id = run.pane_id;
    view! {
        <span class=class>{text}</span>
        <button class="av-run-link" title=format!("Bring {shell_label} to the front")
            on:click=move |_| pane_ops.focus.call(pane_id.clone())>
            {link_text}
        </button>
    }.into_view()
}

/// Elapsed time the way a DBA reads it: `850 ms`, `4.2 s`, `3 min 12 s`.
fn format_elapsed(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1_000.0)
    } else {
        format!("{} min {} s", ms / 60_000, (ms % 60_000) / 1_000)
    }
}

/// CSS class for a severity-coloured value. Neutral gets none.
fn metric_class(sev: Severity) -> &'static str {
    match sev {
        Severity::Good    => "metric-good",
        Severity::Watch   => "metric-watch",
        Severity::Bad     => "metric-bad",
        Severity::Neutral => "",
    }
}

/// Triggers table — one row per autovacuum trigger, columns reading
/// left to right as PG's own formula: what it measures, where it is
/// now, where it fires, how far along, and the rule that sets the
/// threshold. Values set on this table carry the `av-ovr` marker.
fn render_autovac_triggers(rows: Vec<crate::autovac::TriggerRow>) -> leptos::View {
    use crate::autovac::Segment;
    use crate::format::format_int_condensed;
    let any_override = rows.iter().any(|r| r.rule.iter().any(|s| s.overridden));
    let body = rows.into_iter().map(|t| {
        let progress = match (t.inactive, t.progress) {
            (Some(reason), _) => view! {
                <span class="av-inactive-reason">{reason}</span>
            }.into_view(),
            (None, Some(p)) => progress_view(p, t.severity),
            (None, None) => view! { <span class="av-dim">"—"</span> }.into_view(),
        };
        let rule = t.rule.into_iter().map(|Segment { text, overridden }| {
            if overridden {
                view! { <span class="av-ovr">{text}</span> }.into_view()
            } else {
                text.into_view()
            }
        }).collect_view();
        let current  = t.current.map(format_int_condensed).unwrap_or_else(|| "—".into());
        let fires_at = t.fires_at.map(|f| format_int_condensed(f.round() as i64))
            .unwrap_or_else(|| "—".into());
        view! {
            <tr class:av-row-inactive=t.inactive.is_some() title=t.tooltip>
                <td class="av-trigger">{t.label}</td>
                <td class="av-measures">{t.measures}</td>
                <td class="av-num">{current}</td>
                <td class="av-num">{fires_at}</td>
                <td class="av-progress">{progress}</td>
                <td class="av-rule">{rule}</td>
            </tr>
        }
    }).collect_view();
    view! {
        <table class="schema-table av-table">
            <thead>
                <tr>
                    <th>"Trigger"</th>
                    <th>"Measures"</th>
                    <th class="av-num">"Current"</th>
                    <th class="av-num">"Fires at"</th>
                    <th>"Progress"</th>
                    <th>"Rule"</th>
                </tr>
            </thead>
            <tbody>{body}</tbody>
        </table>
        {any_override.then(|| view! {
            <div class="av-legend"><span class="av-ovr"></span>" set on this table"</div>
        })}
    }.into_view()
}

/// Settings table — every knob by its real `ALTER TABLE … SET (…)`
/// name, grouped to mirror the trigger rows. Hover shows the table and
/// cluster values side by side. The Note column appears only when a
/// value is resolved, limited, or ignored by PG's own rules.
fn render_autovac_settings(groups: Vec<Vec<crate::autovac::SettingRow>>) -> leptos::View {
    use crate::autovac::source_label;
    let has_notes = groups.iter().flatten().any(|r| r.note.is_some());
    let body = groups.into_iter().enumerate().flat_map(|(gi, group)| {
        group.into_iter().enumerate().map(move |(ri, r)| {
            let label = source_label(r.source);
            let group_start = gi > 0 && ri == 0;
            let value_class = if r.source == AutovacSource::Reloption {
                "av-num av-value-table"
            } else {
                "av-num"
            };
            view! {
                <tr class:av-group-start=group_start title=r.tooltip>
                    <td class="av-setting">{r.name}</td>
                    <td class=value_class>{r.value}</td>
                    <td class=format!("av-source av-source-{label}")>{label}</td>
                    {has_notes.then(|| view! {
                        <td class="av-note">{r.note.unwrap_or_default()}</td>
                    })}
                </tr>
            }
        }).collect::<Vec<_>>()
    }).collect_view();
    view! {
        <table class="schema-table av-table av-settings">
            <thead>
                <tr>
                    <th>"Setting"</th>
                    <th class="av-num">"Value"</th>
                    <th>"Source"</th>
                    {has_notes.then(|| view! { <th>"Note"</th> })}
                </tr>
            </thead>
            <tbody>{body}</tbody>
        </table>
    }.into_view()
}

/// Compose a VACUUM / ANALYZE statement from the four checkbox
/// states on the Autovacuum section's action panel. Returns `None`
/// when the combination doesn't produce a valid statement (nothing
/// checked, or Freeze without Vacuum).
///
/// Rules:
///   - `!vacuum && !analyze`      → None (nothing to run)
///   - `freeze && !vacuum`        → None (FREEZE is a VACUUM modifier)
///   - `analyze && !vacuum`       → `ANALYZE [VERBOSE] "schema"."name"`
///   - `vacuum` (with any others) → `VACUUM ({opts}) "schema"."name"`
///
/// Options ordering inside VACUUM (…) matches psql conventions:
/// FREEZE, VERBOSE, ANALYZE. Bare `VACUUM` (no options at all) skips
/// the parenthesized list for a cleaner statement.
///
/// Identifier quoting uses double-quotes and doubles any embedded
/// quotes — matches PG's standard identifier escape.
pub(crate) fn compose_vacuum_sql(
    schema:  &str,
    name:    &str,
    vacuum:  bool,
    analyze: bool,
    freeze:  bool,
    verbose: bool,
) -> Option<String> {
    if !vacuum && !analyze { return None; }
    if freeze && !vacuum   { return None; }
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let qualified = format!("{}.{}", q(schema), q(name));
    if !vacuum {
        // ANALYZE only. VERBOSE is the only option that applies.
        if verbose {
            Some(format!("ANALYZE (VERBOSE) {qualified}"))
        } else {
            Some(format!("ANALYZE {qualified}"))
        }
    } else {
        let mut opts: Vec<&'static str> = Vec::new();
        if freeze  { opts.push("FREEZE");  }
        if verbose { opts.push("VERBOSE"); }
        if analyze { opts.push("ANALYZE"); }
        if opts.is_empty() {
            Some(format!("VACUUM {qualified}"))
        } else {
            Some(format!("VACUUM ({}) {qualified}", opts.join(", ")))
        }
    }
}

/// True when the named column is covered by at least one index — either
/// as the leading column of a single-column index or any position in a
/// composite. Used by the Data tab's sort gating: indexed columns sort
/// freely, unindexed columns gate behind a confirmation when the row
/// estimate exceeds the threshold.
fn column_is_indexed(col: &str, indexes: &[IndexInfo]) -> bool {
    indexes.iter().any(|ix|
        ix.columns.iter().any(|c| c.eq_ignore_ascii_case(col)))
}

/// True when the named column participates in the table's PRIMARY KEY.
/// Both planes alias the PK index as "PRIMARY" via the column-alias
/// contract, so we case-insensitive-match on the index name.
fn column_is_pk(col: &str, indexes: &[IndexInfo]) -> bool {
    indexes.iter()
        .filter(|ix| ix.name.eq_ignore_ascii_case("PRIMARY"))
        .any(|ix| ix.columns.iter().any(|c| c.eq_ignore_ascii_case(col)))
}

/// If the named column is the FK side of an outgoing foreign-key
/// relationship from this table, return a "schema.table.column"
/// descriptor of the referenced (parent) side; None otherwise. We pick
/// the closest depth-1 ancestor when multiple match. Read from the
/// existing `ancestors` cache (populated by the ERD tab); marker may
/// take a moment to appear after first opening the Data tab.
fn fk_target_for(col: &str, ancestors: &[RelEdge]) -> Option<String> {
    ancestors.iter()
        // Only direct (depth-1) parents — grandparents would be
        // misleading as a "this column points to X" label.
        .filter(|e| e.depth == 1)
        .filter_map(|e| {
            // Match the column to its index inside from_columns and use
            // the same index in to_columns for the parent column name.
            let pos = e.from_columns.iter().position(|c| c.eq_ignore_ascii_case(col))?;
            let to_col = e.to_columns.get(pos).cloned().unwrap_or_default();
            Some(format!("{}.{}.{}", e.schema, e.table, to_col))
        })
        .next()
}

/// Parse the human-formatted row count from a TableOverview into a u64
/// for threshold checks. The raw string carries thousand separators and
/// possibly a leading sign or trailing notation; strip everything that
/// isn't a digit before parsing. Returns 0 when the overview hasn't
/// loaded yet OR the string fails to parse — both treat-as-small for
/// gating purposes (preferring to allow the operation rather than
/// blocking the user during a transient load).
fn parse_row_estimate(state: LoadState<TableOverview>) -> u64 {
    if let LoadState::Loaded(o) = state {
        let digits: String = o.row_count.chars().filter(|c| c.is_ascii_digit()).collect();
        digits.parse::<u64>().unwrap_or(0)
    } else { 0 }
}

/// Threshold above which sorting an unindexed column on an unfiltered
/// table requires an explicit user confirmation (because PG / MySQL
/// would scan + sort to disk). 50K rows is a defensible default; a
/// future filter UI will narrow the visible set so most "I want to
/// sort by created_at" workflows fall under this.
const DATA_FREE_SORT_LIMIT: u64 = 50_000;
/// Above this row estimate the Data tab refuses to auto-fetch on first
/// view and instead shows a "this table is huge — filter first" hint.
/// The user can override with a "Load First Page Anyway" button.
const DATA_FILTER_FIRST_THRESHOLD: u64 = 1_000_000;

/// Tiny PartialEq-able shadow of LoadState used as the prev-value param
/// for effects that need to detect transitions but don't care about the
/// inner T. LoadState<T> isn't generically comparable since T may not be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LoadStateMarker { NotLoaded, Loading, Loaded, Error }

/// Render the Data tab — paginated, sortable read-only browse.
/// One page in memory at a time (page size 200 by default); navigation
/// refetches the new page and discards the previous to keep memory
/// Render the Object → Privileges matrix tab. Lazy-fetches once
/// when first activated; pivots the plane's `(grantee, privilege,
/// source, chain)` rows into a (Grantee × Privilege) grid where
/// each cell is `●` direct, `○` inherited (with the chain in the
/// title attribute for a tooltip), or blank.
///
/// The toolbar above the matrix has two controls:
///   * Show-inherited toggle (default on) — filters out rows whose
///     `source == "inherited"` so the user can see direct grantees
///     only.
///   * Role-name filter — case-insensitive substring match on the
///     grantee column.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_privileges_tab(
    detail:        TableDetail,
    kind:          ObjectKind,
    schema:        String,
    name:          String,
    plane:         &'static dyn DbmsPlane,
    ws:            RwSignal<Option<WebSocket>>,
    session:    ExplorerSession,
    cell_status:   RwSignal<std::collections::HashMap<(String, String), EditStatus>>,
    pending_revoke: RwSignal<Option<(String, String)>>,
) -> leptos::View {
    let grants_sig = detail.object_grants;
    let show_inh   = detail.privileges_show_inherited;
    let show_none  = detail.privileges_show_no_grant;
    let filter_sig = detail.privileges_filter;
    let priv_kinds = plane.object_privilege_kinds(kind);

    // Lazy-fetch when the cache is NotLoaded. Tracks grants_sig
    // reactively so a post-GRANT/REVOKE reset to NotLoaded (from
    // the DmlResult handler in main.rs) re-fires this effect and
    // refetches the matrix. The early-return on non-NotLoaded
    // (which fires when the effect's own write flips status to
    // Loading) prevents a re-entry loop.
    let s_eff = schema.clone();
    let n_eff = name.clone();
    create_effect(move |_| {
        if !matches!(grants_sig.get(), LoadState::NotLoaded) { return; }
        let Some(sid) = session.id.get_untracked() else { return; };
        let Some(sql) = plane.object_grants_sql(kind, &s_eff, &n_eff) else {
            grants_sig.set(LoadState::Error(
                "This plane doesn't expose object grants for this kind.".to_string()));
            return;
        };
        grants_sig.set(LoadState::Loading);
        let tag = format!("objgrants:{}.{}", s_eff, n_eff);
        crate::dom::ws_introspect(ws, &sid, sql, tag);
    });

    view! {
        <div class="privileges-scroll">
            <div class="privileges-toolbar">
                <label class="privileges-toggle"
                    title="Show roles whose access is via membership in another role">
                    <input type="checkbox"
                        prop:checked=move || show_inh.get()
                        on:change=move |e| show_inh.set(event_target_checked(&e))/>
                    "Show inherited"
                </label>
                <label class="privileges-toggle"
                    title="Show every role in the cluster, even those with no relationship to this object">
                    <input type="checkbox"
                        prop:checked=move || show_none.get()
                        on:change=move |e| show_none.set(event_target_checked(&e))/>
                    "Show no-grant roles"
                </label>
                <input type="text"
                    class="privileges-filter"
                    placeholder="filter by role…"
                    prop:value=move || filter_sig.get()
                    on:input=move |e| filter_sig.set(event_target_value(&e))/>
            </div>
            {move || match grants_sig.get() {
                LoadState::NotLoaded | LoadState::Loading => view! {
                    <div class="detail-msg">"Loading privileges…"</div>
                }.into_view(),
                LoadState::Error(e) => view! {
                    <div class="detail-error">{e}</div>
                }.into_view(),
                LoadState::Loaded(rows) if rows.is_empty() => view! {
                    <div class="detail-msg">"No grants on this object."</div>
                }.into_view(),
                LoadState::Loaded(rows) => {
                    // Build the per-call edit context. Closures
                    // capture schema/name/kind by clone (cheap) so
                    // each cell's click handler stays self-contained.
                    let edit = {
                        let s_for_grant = schema.clone();
                        let n_for_grant = name.clone();
                        let s_for_revoke = schema.clone();
                        let n_for_revoke = name.clone();
                        let s_for_tag_g = schema.clone();
                        let n_for_tag_g = name.clone();
                        let s_for_tag_r = schema.clone();
                        let n_for_tag_r = name.clone();
                        let grant_fire = std::rc::Rc::new(move |grantee: &str, priv_name: &str| {
                            let Some(sid) = session.id.get_untracked() else { return };
                            let Some(sql) = plane.grant_sql(kind, &s_for_grant, &n_for_grant, grantee, priv_name)
                                else { return };
                            cell_status.update(|m| { m.insert((grantee.to_string(), priv_name.to_string()),
                                EditStatus::Saving); });
                            let tag = format!("obj_grant:{}.{}:{}:{}",
                                s_for_tag_g, n_for_tag_g, grantee, priv_name);
                            crate::dom::ws_dml(ws, &sid, sql, tag);
                        }) as std::rc::Rc<dyn Fn(&str, &str)>;
                        let revoke_fire = std::rc::Rc::new(move |grantee: &str, priv_name: &str| {
                            let Some(sid) = session.id.get_untracked() else { return };
                            let Some(sql) = plane.revoke_sql(kind, &s_for_revoke, &n_for_revoke, grantee, priv_name)
                                else { return };
                            cell_status.update(|m| { m.insert((grantee.to_string(), priv_name.to_string()),
                                EditStatus::Saving); });
                            let tag = format!("obj_revoke:{}.{}:{}:{}",
                                s_for_tag_r, n_for_tag_r, grantee, priv_name);
                            crate::dom::ws_dml(ws, &sid, sql, tag);
                        }) as std::rc::Rc<dyn Fn(&str, &str)>;
                        Some(PrivilegeEdit {
                            cell_status,
                            pending_revoke,
                            object_label: format!("{}.{}", schema, name),
                            grant_fire,
                            revoke_fire,
                        })
                    };
                    render_privileges_matrix(
                        rows, priv_kinds,
                        show_inh.get(), show_none.get(), filter_sig.get(),
                        edit,
                    )
                }
            }}
        </div>
    }.into_view()
}

/// Grants or revokes one privilege: (grantee, privilege).
type PrivilegeAction = std::rc::Rc<dyn Fn(&str, &str)>;

/// Pivot the plane's flat `Vec<ObjectGrantRow>` into a (Grantee ×
/// Privilege) matrix, sectioned by role category. Sections render
/// in priority order so the user reads top-to-bottom for "who has
/// access and how":
///
///   1. Owner (PG only — single row, all cells `●`).
///   2. Superusers (cells `★`, all privileges).
///   3. Direct grantees (cells `●` / blank).
///   4. Inherited grantees (cells `○` / blank, granting role in
///      the row label, full chain in cell tooltip).
///   5. No-grant roles (all cells blank — opt-in via toggle).
///
/// Within each section, rows are sorted alphabetically. Cells that
/// don't apply to a role get the blank glyph regardless of section
/// (e.g. owner row will always be `●` for every privilege; direct
/// row may have `●` for SELECT and blank for INSERT).
/// Click context for the editable matrix. None = read-only.
/// Constructed at each call site so the renderer doesn't need to
/// know whether it's editing an object or a schema — only how to
/// fire a GRANT/REVOKE for (grantee, privilege) and where to
/// stash the per-cell status.
pub(crate) struct PrivilegeEdit {
    pub cell_status:    leptos::RwSignal<std::collections::HashMap<(String, String), EditStatus>>,
    pub pending_revoke: leptos::RwSignal<Option<(String, String)>>,
    /// Human-readable label for the confirmation banner — e.g.
    /// `"public.film"` or `"schema public"`.
    pub object_label:   String,
    pub grant_fire:     PrivilegeAction,
    pub revoke_fire:    PrivilegeAction,
}

fn render_privileges_matrix(
    rows:               Vec<ObjectGrantRow>,
    priv_kinds:         &'static [&'static str],
    show_inherited:     bool,
    show_no_grant:      bool,
    filter_text:        String,
    edit:               Option<PrivilegeEdit>,
) -> leptos::View {
    use std::collections::BTreeMap;

    /// Stable cell representation: source + chain + grantable.
    /// Used for both lookup-by-(grantee,priv) and rendering.
    type Cell = (String, String, String);

    // Per-grantee bucket: (role_category, BTreeMap<priv, Cell>).
    let mut cells: BTreeMap<String, (String, BTreeMap<String, Cell>)> = BTreeMap::new();
    for r in rows.into_iter() {
        let bucket = cells.entry(r.grantee.clone())
            .or_insert_with(|| (r.role_category.clone(), BTreeMap::new()));
        // role_category is stable across a role's rows, but if we
        // haven't seen it yet (rare race), record now.
        if bucket.0.is_empty() { bucket.0 = r.role_category.clone(); }
        let entry = bucket.1.entry(r.privilege.clone());
        match entry {
            std::collections::btree_map::Entry::Vacant(v) => {
                v.insert((r.source, r.chain, r.grantable));
            }
            std::collections::btree_map::Entry::Occupied(mut o) => {
                // Source priority for cell display: owner > superuser
                // > direct > inherited > none.
                let new_rank = source_rank(&r.source);
                let cur_rank = source_rank(&o.get().0);
                if new_rank < cur_rank {
                    o.insert((r.source, r.chain, r.grantable));
                }
            }
        }
    }

    // Section ordering: owner=0, superuser=1, direct=2, inherited=3, none=4.
    let section_for = |cat: &str| -> u8 {
        match cat {
            "owner"     => 0,
            "superuser" => 1,
            "direct"    => 2,
            "inherited" => 3,
            _           => 4,
        }
    };

    let needle = filter_text.trim().to_ascii_lowercase();
    let mut grantees: Vec<(String, String)> = cells.iter()
        .filter(|(g, _)| needle.is_empty() || g.to_ascii_lowercase().contains(&needle))
        .filter(|(_, (cat, _))| {
            // Toggle gating: hide inherited rows when toggle off,
            // hide no-grant rows when toggle off. Owner / super /
            // direct always render.
            if cat == "inherited" && !show_inherited { return false; }
            if cat == "none"      && !show_no_grant  { return false; }
            true
        })
        .map(|(g, (cat, _))| (g.clone(), cat.clone()))
        .collect();
    grantees.sort_by(|a, b| section_for(&a.1).cmp(&section_for(&b.1))
        .then_with(|| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase())));

    if grantees.is_empty() {
        return view! {
            <div class="detail-msg">"No grantees match the filter."</div>
        }.into_view();
    }

    // Confirmation banner. Renders above the table when a REVOKE is
    // pending. Mirrors the Data-tab Delete-row banner — same Cancel
    // / Confirm buttons, same in-flight disable, same inline error
    // display. The matrix below stays interactive (other cells can
    // still flash through, e.g. an earlier GRANT settling green
    // while a REVOKE banner is open) — but the cell that's the
    // subject of the banner is the only one the buttons act on.
    let banner_view = match &edit {
        Some(ec) => {
            let pending_revoke = ec.pending_revoke;
            let cell_status    = ec.cell_status;
            let object_label   = ec.object_label.clone();
            let revoke_fire    = ec.revoke_fire.clone();
            view! {
                {move || match pending_revoke.get() {
                    None => view! { <span/> }.into_view(),
                    Some((grantee, priv_name)) => {
                        let key_a = (grantee.clone(), priv_name.clone());
                        let key_b = (grantee.clone(), priv_name.clone());
                        let key_c = (grantee.clone(), priv_name.clone());
                        let key_err = (grantee.clone(), priv_name.clone());
                        // Three independent closures over cell_status
                        // because Leptos `prop:disabled` + `class:` +
                        // text-`move ||` each consume their own Fn —
                        // closures aren't Copy and the alternative
                        // (Rc<dyn Fn>) is heavier syntactically.
                        let in_flight_cancel = move || matches!(
                            cell_status.with(|m| m.get(&key_a).cloned()),
                            Some(EditStatus::Saving));
                        let in_flight_btn = move || matches!(
                            cell_status.with(|m| m.get(&key_b).cloned()),
                            Some(EditStatus::Saving));
                        let in_flight_label = move || matches!(
                            cell_status.with(|m| m.get(&key_c).cloned()),
                            Some(EditStatus::Saving));
                        let err_msg = move || match cell_status.with(|m| m.get(&key_err).cloned()) {
                            Some(EditStatus::Failed(msg)) => Some(msg),
                            _ => None,
                        };
                        let label = format!("Revoke {} on {} from {}?",
                            priv_name, object_label, grantee);
                        let on_revoke = {
                            let fire = revoke_fire.clone();
                            let g = grantee.clone();
                            let p = priv_name.clone();
                            move |_| fire(&g, &p)
                        };
                        view! {
                            <div class="privileges-revoke-banner">
                                <span class="privileges-revoke-banner-text">{label}</span>
                                {move || err_msg().map(|m| {
                                    let title = m.clone();
                                    view! {
                                        <span class="privileges-revoke-banner-error" title=title>{m}</span>
                                    }
                                })}
                                <button class="data-pg-btn"
                                    disabled=in_flight_cancel
                                    on:click=move |_| pending_revoke.set(None)>
                                    "Cancel"
                                </button>
                                <button class="data-pg-btn data-delete-confirm-btn"
                                    disabled=in_flight_btn
                                    on:click=on_revoke>
                                    {move || if in_flight_label() { "Revoking…" } else { "Revoke" }}
                                </button>
                            </div>
                        }.into_view()
                    }
                }}
            }.into_view()
        }
        None => view! { <span/> }.into_view(),
    };

    // Snapshot the edit context's signals + closures so each cell's
    // click handler can capture cheap Copy/Rc clones. `edit_ctx` is
    // None when the matrix is read-only.
    // Cells only need `grant_fire` directly (one-click GRANT) and
    // `pending_revoke` (to arm the banner). The actual REVOKE call
    // happens inside the banner above; the closure for that was
    // captured by clone before this struct's construction.
    #[derive(Clone)]
    struct CellCtx {
        cell_status:    leptos::RwSignal<std::collections::HashMap<(String, String), EditStatus>>,
        pending_revoke: leptos::RwSignal<Option<(String, String)>>,
        grant_fire:     PrivilegeAction,
    }
    let edit_ctx: Option<CellCtx> = edit.map(|e| CellCtx {
        cell_status:    e.cell_status,
        pending_revoke: e.pending_revoke,
        grant_fire:     e.grant_fire,
    });

    view! {
        <div class="privileges-grid-wrap">
            {banner_view}
            <table class="privileges-grid">
                <thead>
                    <tr>
                        <th class="privileges-th-grantee">"Grantee"</th>
                        {priv_kinds.iter().map(|p| view! {
                            <th class="privileges-th-priv" title=p.to_string()>{*p}</th>
                        }).collect_view()}
                    </tr>
                </thead>
                <tbody>
                    {grantees.into_iter().map(|(grantee, category)| {
                        let row = cells.get(&grantee).map(|(_, m)| m.clone()).unwrap_or_default();
                        let row_cls = format!("privileges-row-{}", category);
                        // Row-label suffix per section. The granting
                        // role for inherited rows is the immediate
                        // ancestor — pulled out of any inherited cell
                        // we have for this grantee (they all share it
                        // by construction of the plane SQL).
                        // Inherited row's "via X" label is an
                        // approximation: a role can inherit different
                        // privileges via different ancestor roles
                        // (SELECT via role_b, UPDATE via role_c). The
                        // BTreeMap iterates in alphabetical privilege
                        // order, so the label shows whichever
                        // granting role's privilege sorts first. The
                        // per-cell tooltip carries the precise chain
                        // for that intersection — which is the load-
                        // bearing answer to "where does THIS access
                        // come from."
                        let suffix = match category.as_str() {
                            "owner"     => Some(String::from("(owner)")),
                            "superuser" => Some(String::from("(super-user)")),
                            "inherited" => row.values()
                                .find(|(_, ch, _)| !ch.is_empty())
                                .map(|(_, ch, _)| format!("(via {})", ch)),
                            _ => None,
                        };
                        view! {
                            <tr class=row_cls>
                                <td class="privileges-td-grantee" title=grantee.clone()>
                                    <span class="privileges-grantee-name">{grantee.clone()}</span>
                                    {suffix.map(|s| view! {
                                        <span class="privileges-grantee-suffix">{s}</span>
                                    })}
                                </td>
                                {priv_kinds.iter().map(|p| {
                                    let cell = row.get(*p).cloned();
                                    // CellAction: which click flow,
                                    // if any, this cell binds.
                                    //   Grant  — empty cell; one-click GRANT
                                    //   Revoke — direct cell; arms the banner
                                    //   None   — owner/super/inherited or
                                    //            wider-scope direct (chain
                                    //            non-empty: see MySQL note
                                    //            below)
                                    #[derive(Clone, Copy, PartialEq)]
                                    enum CellAction { Grant, Revoke, None }
                                    let (glyph, cls, title, action) = match cell {
                                        Some((src, chain, grantable)) => {
                                            let suffix = if grantable.eq_ignore_ascii_case("YES") {
                                                " (with grant option)"
                                            } else { "" };
                                            match src.as_str() {
                                                "owner" => ("●", "privileges-cell-owner",
                                                            format!("Owner — implicit ALL{}", suffix),
                                                            CellAction::None),
                                                "superuser" => ("★", "privileges-cell-super",
                                                                String::from("Superuser bypass"),
                                                                CellAction::None),
                                                "direct" => {
                                                    // Chain non-empty on a
                                                    // direct row = MySQL
                                                    // wider-scope grant (db.*
                                                    // / *.*) projected into
                                                    // this object's row. We
                                                    // can't REVOKE that from
                                                    // this cell — the user
                                                    // has to revoke at the
                                                    // wider scope. So the
                                                    // click is a no-op.
                                                    let scope = if chain.is_empty() {
                                                        String::new()
                                                    } else {
                                                        format!(" via {}", chain)
                                                    };
                                                    let act = if chain.is_empty() {
                                                        CellAction::Revoke
                                                    } else {
                                                        CellAction::None
                                                    };
                                                    ("●", "privileges-cell-direct",
                                                     format!("Direct grant{}{}", scope, suffix),
                                                     act)
                                                }
                                                "inherited" => ("○", "privileges-cell-inherited",
                                                                if chain.is_empty() {
                                                                    String::from("Inherited")
                                                                } else {
                                                                    format!("Inherited via {}", chain)
                                                                },
                                                                CellAction::None),
                                                // Unknown source string — a grant
                                                // EXISTS, just under an unrecognized
                                                // category. Don't fire a duplicate
                                                // GRANT on click; leave it inert
                                                // until the source is taught here.
                                                _ => ("", "privileges-cell-empty", String::new(),
                                                      CellAction::None),
                                            }
                                        }
                                        None => ("", "privileges-cell-empty", String::new(),
                                                 CellAction::Grant),
                                    };
                                    let grantee_for_cell = grantee.clone();
                                    let priv_name = p.to_string();
                                    let ctx = edit_ctx.clone();
                                    // Cell is clickable iff we have edit
                                    // context AND the action isn't None.
                                    let is_clickable = ctx.is_some() && action != CellAction::None;
                                    let click_action = action;
                                    let final_cls = if is_clickable {
                                        format!("privileges-td-cell privileges-clickable {}", cls)
                                    } else {
                                        format!("privileges-td-cell {}", cls)
                                    };
                                    // Per-cell status overlay. Saving →
                                    // greyed; Saved → 1.5s green flash;
                                    // Failed → red tint + err in title.
                                    let key_for_status = (grantee_for_cell.clone(), priv_name.clone());
                                    let status_cls_sig = move || match ctx.as_ref() {
                                        Some(c) => match c.cell_status.with(|m| m.get(&key_for_status).cloned()) {
                                            Some(EditStatus::Saving) => "privileges-cell-saving",
                                            Some(EditStatus::Saved)  => "privileges-cell-flash-saved",
                                            Some(EditStatus::Failed(_)) => "privileges-cell-flash-failed",
                                            None => "",
                                        }
                                        None => "",
                                    };
                                    let key_for_title = (grantee_for_cell.clone(), priv_name.clone());
                                    let title_sig = {
                                        let base = title.clone();
                                        let ctx2 = edit_ctx.clone();
                                        move || match ctx2.as_ref() {
                                            Some(c) => match c.cell_status.with(|m| m.get(&key_for_title).cloned()) {
                                                Some(EditStatus::Failed(msg)) => msg,
                                                _ => base.clone(),
                                            }
                                            None => base.clone(),
                                        }
                                    };
                                    // on:click: empty cell fires GRANT;
                                    // direct cell arms the REVOKE banner.
                                    // ctx2 keeps the closure self-contained.
                                    let ctx_for_click = edit_ctx.clone();
                                    let click_grantee = grantee_for_cell.clone();
                                    let click_priv = priv_name.clone();
                                    let on_click = move |_| {
                                        let Some(c) = ctx_for_click.as_ref() else { return };
                                        match click_action {
                                            CellAction::Grant => (c.grant_fire)(&click_grantee, &click_priv),
                                            CellAction::Revoke => {
                                                c.pending_revoke.set(Some((click_grantee.clone(), click_priv.clone())));
                                            }
                                            CellAction::None => {}
                                        }
                                    };
                                    view! {
                                        <td class=move || format!("{} {}", final_cls, status_cls_sig())
                                            title=title_sig
                                            on:click=on_click>{glyph}</td>
                                    }
                                }).collect_view()}
                            </tr>
                        }
                    }).collect_view()}
                </tbody>
            </table>
        </div>
    }.into_view()
}

/// Source priority — lower wins when collapsing multiple rows for
/// the same (grantee, privilege) cell. Owner is the strongest claim
/// (object-ownership), superuser next, then direct, inherited,
/// none. None is the weakest (placeholder for "show all roles").
fn source_rank(s: &str) -> u8 {
    match s {
        "owner"     => 0,
        "superuser" => 1,
        "direct"    => 2,
        "inherited" => 3,
        _           => 4,
    }
}

/// bounded. Sort by column header click; gated behind a confirmation
/// for unindexed columns on tables larger than DATA_FREE_SORT_LIMIT.
pub(crate) fn render_data_tab(
    detail:     TableDetail,
    schema:     String,
    name:       String,
    plane:      &'static dyn DbmsPlane,
    ws:         RwSignal<Option<WebSocket>>,
    session: ExplorerSession,
) -> leptos::View {
    let page_idx    = detail.data_page_index;
    let page_size   = detail.data_page_size;
    let sort_col    = detail.data_sort_column;
    let sort_desc   = detail.data_sort_desc;
    let page_signal = detail.data_page;
    let proceed_huge = detail.data_proceed_huge;
    let overview     = detail.overview;
    let indexes_sig  = detail.indexes;
    let ancestors_sig = detail.ancestors;
    let filter_sig   = detail.data_filter;
    let search_sig  = detail.data_search;
    let scroll_left = detail.data_scroll_left;
    let scroll_top  = detail.data_scroll_top;
    let view_mode   = detail.data_view_mode;
    let selected_row = detail.data_selected_row;
    let selected_col = detail.data_selected_col;
    let exact_count  = detail.data_exact_count;

    // Reset selected row + col + flip back to Grid mode on every
    // refetch — the row indices in the new page don't correspond
    // to the old, and a stale `selected_col` would silently force
    // DataBody mode on the new page at the wrong column.
    create_effect(move |prev: Option<LoadStateMarker>| {
        let cur = match page_signal.get() {
            LoadState::Loaded(_) => LoadStateMarker::Loaded,
            LoadState::Loading   => LoadStateMarker::Loading,
            LoadState::Error(_)  => LoadStateMarker::Error,
            LoadState::NotLoaded => LoadStateMarker::NotLoaded,
        };
        // Only reset on a transition INTO Loading (i.e. a new fetch
        // started). Loading→Loaded shouldn't disturb the user's
        // selection during a no-op refresh.
        if cur == LoadStateMarker::Loading && prev != Some(LoadStateMarker::Loading) {
            selected_row.set(None);
            selected_col.set(None);
            view_mode.set(DataViewMode::Grid);
            // Clear delete state too — the row indices are about to
            // shift under a refetch and a stale banner would be
            // pointing at the wrong row.
            detail.data_delete_pending.set(None);
            detail.data_delete_status.update(|m| m.clear());
        }
        cur
    });
    // Local-to-this-render refresh counter. The lazy-fetch effect
    // dedupes by SQL string; bumping this counter changes the dedup
    // key so the manual refresh button forces a re-fetch even when
    // the underlying SELECT hasn't changed.
    // Refresh tick now lives on TableDetail so the INSERT-response
    // handler in main.rs can bump it to force a page refetch after a
    // successful insert (the new row's PK / defaults won't be in the
    // pending DML — refetching is the simplest source of truth).
    let refresh_tick = detail.data_refresh_tick;

    // Restore the user's scroll position after the body re-renders.
    // The Loaded→Loaded transition (sort flip, page nav, filter apply)
    // tears down the .data-grid-scroll div and rebuilds it, so without
    // intervention scrollLeft/scrollTop reset to 0 and a wide-table
    // user gets bounced back to column 0 on every sort. Re-applying
    // via requestAnimationFrame after the new DOM is mounted keeps
    // their column in view.
    let scroll_key_eff = format!("{}.{}", schema, name);
    create_effect(move |_| {
        // Track data_page so this fires on every grid mount.
        let page_state = page_signal.get();
        if !matches!(page_state, LoadState::Loaded(_)) { return; }
        let l = scroll_left.get_untracked();
        let t = scroll_top.get_untracked();
        if l == 0 && t == 0 { return; }
        let selector = format!(".data-grid-scroll[data-key=\"{}\"]", scroll_key_eff);
        if let Some(win) = web_sys::window() {
            let cb = Closure::once(move || {
                if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                    if let Some(el) = doc.query_selector(&selector).ok().flatten() {
                        el.set_scroll_left(l);
                        el.set_scroll_top(t);
                    }
                }
            });
            let _ = win.request_animation_frame(cb.as_ref().unchecked_ref());
            cb.forget();
        }
    });

    // Reset to first page whenever the server filter changes — the new
    // dataset may have very different shape and "page 47 of the old"
    // doesn't map to anything sensible. Same prev-value guard as the
    // admin effects: skip the initial pulse on mount.
    create_effect(move |prev: Option<String>| {
        let cur = filter_sig.get();
        if let Some(prev) = prev {
            if prev != cur {
                page_idx.set(0);
                // Force a refetch even if we were on page 0 already.
                page_signal.set(LoadState::NotLoaded);
                // Exact count is filter-scoped — invalidate.
                exact_count.set(None);
            }
        }
        cur
    });

    // Lazy-fetch effect: triggers whenever any of (page_idx, page_size,
    // sort_col, sort_desc, proceed_huge) changes AND we have permission
    // to fetch (small table OR user dismissed the huge-table warning).
    // Sets data_page to Loading then issues the SELECT.
    let schema_eff = schema.clone();
    let name_eff   = name.clone();
    create_effect(move |prev: Option<String>| -> String {
        let row_est = parse_row_estimate(overview.get());
        let filter  = filter_sig.get();
        let has_filter = !filter.trim().is_empty();
        // Refuse auto-fetch on huge tables until either (a) the user has
        // explicitly dismissed the warning OR (b) they've typed a filter
        // — the filter is itself an opt-in to "I know what I want."
        if row_est > DATA_FILTER_FIRST_THRESHOLD && !proceed_huge.get() && !has_filter {
            return prev.unwrap_or_default();
        }
        let idx  = page_idx.get();
        let size = page_size.get();
        let col  = sort_col.get();
        let desc = sort_desc.get();
        let Some(sid) = session.id.get_untracked() else { return prev.unwrap_or_default() };
        let order_by = col.as_deref().map(|c| (c, desc));
        let where_opt = if has_filter { Some(filter.as_str()) } else { None };
        let sql = plane.select_paged_sql(&schema_eff, &name_eff, size, idx * size, order_by, where_opt);
        // Dedup key = sql + refresh tick. Pure SQL dedup would swallow
        // the manual ↻ button; including the tick lets us bump the key
        // to force a re-fetch even when the SELECT is unchanged.
        let tick = refresh_tick.get();
        let key  = format!("{}#{}", sql, tick);
        if prev.as_deref() == Some(key.as_str()) { return key; }
        let tag = format!("data_page:{}.{}", schema_eff, name_eff);
        page_signal.set(LoadState::Loading);
        ws_introspect(ws, &sid, sql, tag);
        key
    });

    let schema_for_view  = schema.clone();
    let name_for_view    = name.clone();
    let schema_for_count = schema.clone();
    let name_for_count   = name.clone();

    view! {
        <div class="data-tab">
            // ── Filter row: server WHERE + client visible-row search
            //
            // Two distinct surfaces. Server filter rewrites the SELECT
            // (raw SQL fragment, no parsing — DBA tool); client search
            // hides rows in the rendered page only, no round-trip.
            // Visually distinct backgrounds so the user can't confuse them.
            <div class="data-filter-row">
                <span class="data-filter-label" title="Server-side WHERE clause — rewrites the SELECT, affects all pages and counts">
                    "WHERE"
                </span>
                <input class="data-filter-input data-filter-server"
                    type="text"
                    placeholder="raw SQL fragment, e.g. created_at > '2024-01-01' AND active"
                    prop:value=move || filter_sig.get()
                    on:keydown=move |e: web_sys::KeyboardEvent| {
                        if e.key() == "Enter" {
                            e.prevent_default();
                            filter_sig.set(event_target_value(&e));
                        } else if e.key() == "Escape" {
                            e.stop_propagation();
                            filter_sig.set(String::new());
                        }
                    }
                    on:blur=move |e| {
                        // Commit on blur too so clicking away applies.
                        let v = event_target_value(&e);
                        if v != filter_sig.get_untracked() {
                            filter_sig.set(v);
                        }
                    }/>
                <span class="data-filter-divider"/>
                <span class="data-filter-label" title="Client-side filter — hides rows in the current page only, no query re-issued">
                    "Find"
                </span>
                <input class="data-filter-input data-filter-client"
                    type="text"
                    placeholder="filter visible rows…"
                    prop:value=move || search_sig.get()
                    on:input=move |e| search_sig.set(event_target_value(&e))/>
            </div>

            // ── Toolbar: pagination + sort indicator + refresh
            <div class="data-toolbar">
                {move || {
                    let row_est = parse_row_estimate(overview.get());
                    let has_filter = !filter_sig.get().trim().is_empty();
                    let size = page_size.get();
                    let idx  = page_idx.get();
                    // Approximate page count from estimate. Floor-divide
                    // and round up (so 201 estimated rows → 2 pages).
                    // When a filter is active the underlying count is
                    // unknown (we'd need a COUNT(*) round-trip), so just
                    // hide the "of ~N" suffix.
                    let total_pages = if row_est == 0 { 1 }
                                      else { (row_est as usize).div_ceil(size).max(1) };
                    let page_label = if has_filter {
                        format!("Page {}", idx + 1)
                    } else if row_est > 0 {
                        format!("Page {} of ~{}", idx + 1, total_pages)
                    } else {
                        format!("Page {}", idx + 1)
                    };
                    let at_first = idx == 0;
                    // Clone the table identity here so the inner reactive
                    // closures (Exact button, etc.) can move-capture
                    // fresh copies without making this outer closure
                    // FnOnce. Each toolbar render gets a new pair.
                    let schema_for_count = schema_for_count.clone();
                    let name_for_count   = name_for_count.clone();
                    view! {
                        <button class="data-pg-btn" disabled=move || at_first
                            on:click=move |_| if !at_first { page_idx.set(0); }>"«"</button>
                        <button class="data-pg-btn" disabled=move || at_first
                            on:click=move |_| if !at_first { page_idx.update(|i| *i = i.saturating_sub(1)); }>"‹"</button>
                        <span class="data-pg-label">{page_label}</span>
                        <button class="data-pg-btn"
                            on:click=move |_| page_idx.update(|i| *i += 1)>"›"</button>
                        <span class="data-pg-spacer"/>
                        // Page size selector. Changing it bumps the page
                        // back to 0 since the OFFSET math no longer maps.
                        <span class="data-pg-label">"Rows/page"</span>
                        <select class="data-pg-select"
                            on:change=move |e| {
                                if let Ok(n) = event_target_value(&e).parse::<usize>() {
                                    page_size.set(n);
                                    page_idx.set(0);
                                }
                            }>
                            {[100usize, 200, 500, 1000].iter().map(|&n| {
                                view! {
                                    <option value={n.to_string()}
                                        selected=move || page_size.get() == n>
                                        {n.to_string()}
                                    </option>
                                }
                            }).collect_view()}
                        </select>
                        // "Show all" — only enabled for tables small
                        // enough that fetching the full set is cheap.
                        // Reads the row estimate from the overview cache;
                        // setting page_size beyond the table's row count
                        // is harmless (LIMIT just over-allocates slightly).
                        {move || {
                            let row_est = parse_row_estimate(overview.get());
                            let allowed = row_est > 0 && row_est <= 5_000;
                            if !allowed { return view! { <span/> }.into_view(); }
                            view! {
                                <button class="data-pg-btn"
                                    title="Load the entire table in one page (small tables only)"
                                    on:click=move |_| {
                                        page_size.set(row_est as usize);
                                        page_idx.set(0);
                                    }>
                                    "Show all"
                                </button>
                            }.into_view()
                        }}
                        <span class="data-pg-meta">
                            {move || {
                                if let Some(n) = exact_count.get() {
                                    return format!("{} rows", n);
                                }
                                if !filter_sig.get().trim().is_empty() {
                                    return "filtered".to_string();
                                }
                                match overview.get() {
                                    LoadState::Loaded(o) => {
                                        if o.row_count.trim().is_empty() {
                                            "row count unknown".to_string()
                                        } else {
                                            format!("~{} total rows", o.row_count)
                                        }
                                    }
                                    _ => "loading…".to_string(),
                                }
                            }}
                        </span>
                        // Exact-count button — issues SELECT COUNT(*).
                        // Hidden once a count has been fetched (no point
                        // re-fetching) until something invalidates it
                        // (filter change clears).
                        {move || {
                            if exact_count.get().is_some() { return view! { <span/> }.into_view(); }
                            // Clone inside the reactive closure so each
                            // re-render hands a fresh pair to the click
                            // handler. Otherwise the closure becomes
                            // FnOnce when it moves the strings out.
                            let s = schema_for_count.clone();
                            let n = name_for_count.clone();
                            view! {
                                <button class="data-pg-btn"
                                    title="Run SELECT COUNT(*) for an exact row count"
                                    on:click=move |_| {
                                        let filter = filter_sig.get_untracked();
                                        let where_opt = if filter.trim().is_empty() { None } else { Some(filter.as_str()) };
                                        let sql = plane.count_rows_sql(&s, &n, where_opt);
                                        if let Some(sid) = session.id.get_untracked() {
                                            let tag = format!("data_count:{}.{}", s, n);
                                            ws_introspect(ws, &sid, sql, tag);
                                        }
                                    }>
                                    "Exact"
                                </button>
                            }.into_view()
                        }}
                        <button class="data-pg-btn"
                            disabled=move || {
                                let m = view_mode.get();
                                matches!(m, DataViewMode::Insert)
                                    || (m == DataViewMode::Grid && selected_row.get().is_none())
                            }
                            title=move || match view_mode.get() {
                                DataViewMode::Grid => "Show selected row vertically (click a row first)",
                                DataViewMode::Vertical => "Back to grid view",
                                DataViewMode::Insert => "Cancel the insert form first",
                            }
                            on:click=move |_| {
                                view_mode.update(|m| *m = match *m {
                                    DataViewMode::Grid => DataViewMode::Vertical,
                                    DataViewMode::Vertical => DataViewMode::Grid,
                                    DataViewMode::Insert => DataViewMode::Insert,  // disabled
                                });
                            }>
                            {move || match view_mode.get() {
                                DataViewMode::Grid => "≡",
                                DataViewMode::Vertical => "⊞",
                                DataViewMode::Insert => "≡",
                            }}
                        </button>
                        // + Insert: switch to the per-column vertical
                        // form to add a new row. Disabled while a save
                        // is already in flight so a double-click can't
                        // duplicate the request.
                        <button class="data-pg-btn"
                            title="Insert a new row"
                            disabled=move || matches!(view_mode.get(), DataViewMode::Insert)
                            on:click=move |_| {
                                detail.data_insert_buffer.update(|m| m.clear());
                                detail.data_insert_status.set(None);
                                detail.data_insert_upsert.set(false);
                                view_mode.set(DataViewMode::Insert);
                            }>"+"</button>
                        // − Delete row: arms the confirmation banner
                        // for the currently-selected row. Disabled
                        // when no row is selected — avoids a "delete"
                        // that has no target. Actual DELETE only fires
                        // after the banner's Confirm button.
                        <button class="data-pg-btn"
                            title="Delete the selected row (click a row first)"
                            disabled=move || selected_row.get().is_none()
                                || matches!(view_mode.get(), DataViewMode::Insert)
                            on:click=move |_| {
                                if let Some(idx) = selected_row.get_untracked() {
                                    detail.data_delete_pending.set(Some(idx));
                                }
                            }>"−"</button>
                        // Export current page — CSV + JSON, mirroring the
                        // shell's result export. Only the rows in memory
                        // (one page) are exported; for the full table the
                        // user paginates and exports each, or pulls via
                        // the shell.
                        {move || match page_signal.get() {
                            LoadState::Loaded(p) => {
                                let p_csv  = p.clone();
                                let p_json = p;
                                view! {
                                    <button class="data-pg-btn" title="Export current page as CSV"
                                        on:click=move |_| {
                                            crate::export::trigger_download(
                                                "page.csv",
                                                &crate::export::to_csv(&p_csv.columns, &p_csv.cells()),
                                                "text/csv");
                                        }>"CSV"</button>
                                    <button class="data-pg-btn" title="Export current page as JSON"
                                        on:click=move |_| {
                                            crate::export::trigger_download(
                                                "page.json",
                                                &crate::export::to_json(&p_json.columns, &p_json.cells()),
                                                "application/json");
                                        }>"JSON"</button>
                                }.into_view()
                            }
                            _ => view! { <span/> }.into_view(),
                        }}
                        <button class="data-pg-btn data-refresh-btn" title="Refresh page"
                            on:click=move |_| refresh_tick.update(|t| *t += 1)>"↻"</button>
                    }.into_view()
                }}
            </div>

            // ── Body: filter-first warning, error, loading, or grid
            {move || {
                // Insert mode is rendered before any page-state checks
                // so the form keeps working through Loading / Error
                // states (the form pulls columns from `detail.columns`,
                // not from the data page).
                if matches!(view_mode.get(), DataViewMode::Insert) {
                    return render_data_insert(
                        detail, plane,
                        schema_for_view.clone(), name_for_view.clone(),
                        ws, session,
                    );
                }
                let row_est = parse_row_estimate(overview.get());
                if row_est > DATA_FILTER_FIRST_THRESHOLD && !proceed_huge.get() {
                    return view! {
                        <div class="data-filter-first">
                            <div class="data-filter-first-title">"Large table"</div>
                            <div class="data-filter-first-body">
                                {format!(
                                    "This table has ~{} estimated rows. Loading without a filter \
                                     fetches one page (200 rows) at a time, but sorting and \
                                     scrolling across the dataset is expensive. A filter UI is \
                                     coming next session — for now you can load the first page \
                                     and paginate manually.",
                                    row_est)}
                            </div>
                            <button class="data-filter-first-btn"
                                on:click=move |_| proceed_huge.set(true)>
                                "Load First Page Anyway"
                            </button>
                        </div>
                    }.into_view();
                }
                match page_signal.get() {
                    LoadState::NotLoaded | LoadState::Loading => view! {
                        <div class="detail-msg">"Loading…"</div>
                    }.into_view(),
                    LoadState::Error(e) => view! {
                        <div class="detail-error">{e}</div>
                    }.into_view(),
                    LoadState::Loaded(page) => match view_mode.get() {
                        DataViewMode::Grid => render_data_grid(
                            page, sort_col, sort_desc, page_idx,
                            indexes_sig, overview, filter_sig, search_sig,
                            scroll_left, scroll_top, selected_row, ancestors_sig,
                            detail, plane, ws, session,
                            schema_for_view.clone(), name_for_view.clone(),
                        ),
                        DataViewMode::Vertical => render_data_vertical(
                            page, selected_row, view_mode,
                        ),
                        // Insert is dispatched before this match (see
                        // the early-return above) so the form survives
                        // any Loading state. This arm is unreachable
                        // but kept exhaustive for the compiler.
                        DataViewMode::Insert => view! { <span/> }.into_view(),
                    },
                }
            }}
        </div>
    }.into_view()
}

/// Render the data grid — column headers (clickable for sort) + rows
/// filtered by the client-side `search_sig` if non-empty.
#[allow(clippy::too_many_arguments)]
fn render_data_grid(
    page:        DataPage,
    sort_col:    RwSignal<Option<String>>,
    sort_desc:   RwSignal<bool>,
    page_idx:    RwSignal<usize>,
    indexes_sig: RwSignal<LoadState<Vec<IndexInfo>>>,
    overview:    RwSignal<LoadState<TableOverview>>,
    filter_sig:  RwSignal<String>,
    search_sig:  RwSignal<String>,
    scroll_left:   RwSignal<i32>,
    scroll_top:    RwSignal<i32>,
    selected_row:  RwSignal<Option<usize>>,
    ancestors_sig: RwSignal<LoadState<Vec<RelEdge>>>,
    detail:        TableDetail,
    plane:         &'static dyn DbmsPlane,
    ws:            RwSignal<Option<WebSocket>>,
    session:    ExplorerSession,
    schema:        String,
    name:          String,
) -> leptos::View {
    let scroll_key = format!("{}.{}", schema, name);
    let active_edit = detail.data_active_edit;
    let edit_input  = detail.data_edit_input;
    let edit_status = detail.data_edit_status;
    let edit_pending = detail.data_edit_pending;
    let edit_aborting = detail.data_edit_aborting;
    let selected_col  = detail.data_selected_col;
    let delete_pending = detail.data_delete_pending;
    let delete_status  = detail.data_delete_status;
    let columns = page.columns;
    let rows    = page.rows;
    if rows.is_empty() {
        return view! {
            <div class="detail-msg">"No rows on this page."</div>
        }.into_view();
    }
    // Snapshot indexes + ancestors once for the header render. Both
    // signals are populated by the Schema / ERD tabs and are stable
    // for the lifetime of this render — re-rendering on every
    // header click would just churn the same lookups.
    let pk_set: std::collections::HashSet<String> = match indexes_sig.get_untracked() {
        LoadState::Loaded(ix) => columns.iter()
            .filter(|c| column_is_pk(c, &ix))
            .map(|c| c.to_ascii_lowercase())
            .collect(),
        _ => Default::default(),
    };
    // Ordered (col_name, col_index) pairs for the table's PK so the
    // editor can construct a WHERE clause from a row's stored values.
    // Empty when the table has no detectable PK (no editing allowed).
    let pk_columns: Vec<(String, usize)> = columns.iter().enumerate()
        .filter(|(_, c)| pk_set.contains(&c.to_ascii_lowercase()))
        .map(|(i, c)| (c.clone(), i))
        .collect();
    let editable = !pk_columns.is_empty();
    // Map column → "schema.table.column" target descriptor for FKs.
    // Used for the tooltip on the 🔗 marker so the user knows what
    // foreign object this column points to without flipping to ERD.
    let fk_targets: std::collections::HashMap<String, String> =
        match ancestors_sig.get_untracked() {
            LoadState::Loaded(an) => columns.iter()
                .filter_map(|c| fk_target_for(c, &an).map(|t| (c.to_ascii_lowercase(), t)))
                .collect(),
            _ => Default::default(),
        };

    let header_views = columns.iter().map(|col| {
        let col_name      = col.clone();
        let col_for_click = col.clone();
        let col_for_state = col.clone();
        let col_for_arrow = col.clone();
        let key_lc = col.to_ascii_lowercase();
        let key_marker: Option<(&'static str, &'static str, String)> =
            if pk_set.contains(&key_lc) {
                Some(("data-col-pk", "🔑", "Primary key".to_string()))
            } else if let Some(target) = fk_targets.get(&key_lc) {
                Some(("data-col-fk", "🔗", format!("Foreign key → {}", target)))
            } else { None };
        view! {
            <th class="data-col-header"
                on:click=move |_| {
                    // Sort gating: free for indexed columns, for tables
                    // estimated under DATA_FREE_SORT_LIMIT rows, OR when
                    // a server-side WHERE filter is active (the user has
                    // narrowed the set themselves). Otherwise skip the
                    // sort with a console warning — a confirmation
                    // dialog with EXPLAIN cost is the next polish item.
                    let row_est = parse_row_estimate(overview.get_untracked());
                    let is_indexed = match indexes_sig.get_untracked() {
                        LoadState::Loaded(ix) => column_is_indexed(&col_for_click, &ix),
                        _ => false,
                    };
                    let has_filter = !filter_sig.get_untracked().trim().is_empty();
                    let allowed = is_indexed
                        || row_est <= DATA_FREE_SORT_LIMIT
                        || has_filter;
                    if !allowed {
                        leptos::logging::warn!(
                            "Skipping sort: column '{}' is unindexed and table has ~{} rows. \
                             Apply a WHERE filter or sort by an indexed column.",
                            col_for_click, row_est);
                        return;
                    }
                    // Toggle: same column → flip direction; new column →
                    // ASC. Reset to page 1 since sort changes the rowset.
                    let cur = sort_col.get_untracked();
                    if cur.as_deref() == Some(col_for_click.as_str()) {
                        sort_desc.update(|d| *d = !*d);
                    } else {
                        sort_col.set(Some(col_for_click.clone()));
                        sort_desc.set(false);
                    }
                    page_idx.set(0);
                }>
                {key_marker.map(|(class, glyph, title)| view! {
                    <span class={class} title={title}>{glyph}</span>
                })}
                {col_name}
                {move || {
                    let active = sort_col.get().as_deref() == Some(col_for_state.as_str());
                    if active {
                        let arrow = if sort_desc.get() { " ↓" } else { " ↑" };
                        let _ = &col_for_arrow;
                        view! { <span class="data-sort-arrow">{arrow}</span> }.into_view()
                    } else {
                        view! { <span/> }.into_view()
                    }
                }}
            </th>
        }
    }).collect_view();

    // Closure that produces the delete confirmation banner from the
    // currently-pending row index. Keeps the banner's PK lookup
    // alongside the rest of the grid's PK plumbing instead of
    // duplicating it at the toolbar layer.
    let pk_columns_for_banner = pk_columns.clone();
    let rows_for_banner       = rows.clone();
    let schema_for_banner     = schema.clone();
    let name_for_banner       = name.clone();
    let banner_view = move || {
        let Some(idx) = delete_pending.get() else {
            return view! { <span/> }.into_view();
        };
        // No PK → can't issue a safe DELETE. Show an explanatory
        // banner with only a Cancel button.
        if pk_columns_for_banner.is_empty() {
            return view! {
                <div class="data-delete-banner data-delete-banner-disabled">
                    <span class="data-delete-banner-text">
                        "This table has no primary key — single-row delete isn't supported."
                    </span>
                    <button class="data-pg-btn"
                        on:click=move |_| delete_pending.set(None)>"Cancel"</button>
                </div>
            }.into_view();
        }
        let row = match rows_for_banner.get(idx) {
            Some(r) => r,
            None => return view! { <span/> }.into_view(),
        };
        let pk_pairs: Vec<(String, String)> = pk_columns_for_banner.iter()
            .map(|(col, ci)| (col.clone(), row.get(*ci).cloned().unwrap_or_default()))
            .collect();
        let pk_summary = pk_pairs.iter()
            .map(|(c, v)| format!("{} = {}", c, v))
            .collect::<Vec<_>>()
            .join(", ");

        // Reactive read of this row's status — used by all three
        // status-driven bits below. Wrapping it in a closure ensures
        // each consumer re-runs on `delete_status` updates instead of
        // capturing a stale snapshot at banner-render time.
        let row_status = move || delete_status.with(|m| m.get(&idx).cloned());
        let in_flight  = move || matches!(row_status(), Some(EditStatus::Saving));

        let pk_cols_v: Vec<String> = pk_pairs.iter().map(|(c, _)| c.clone()).collect();
        let pk_vals_v: Vec<String> = pk_pairs.iter().map(|(_, v)| v.clone()).collect();
        let schema_c = schema_for_banner.clone();
        let name_c   = name_for_banner.clone();
        let confirm = move || {
            // Synchronous re-entrancy guard against a fast double-click
            // — the reactive `disabled` only flips on the next render.
            if matches!(delete_status.with_untracked(|m| m.get(&idx).cloned()), Some(EditStatus::Saving)) {
                return;
            }
            let sql = plane.delete_row_sql(&schema_c, &name_c, &pk_cols_v, &pk_vals_v);
            let tag = format!("data_delete:{}.{}:{}", schema_c, name_c, idx);
            delete_status.update(|m| { m.insert(idx, EditStatus::Saving); });
            match session.id.get_untracked() {
                Some(sid) => crate::dom::ws_dml(ws, &sid, sql, tag),
                None => {
                    delete_status.update(|m| {
                        m.insert(idx, EditStatus::Failed(
                            "No active session — try reconnecting.".to_string()));
                    });
                }
            }
        };
        view! {
            <div class="data-delete-banner">
                <span class="data-delete-banner-text">
                    "Delete row where "
                    <span class="data-delete-banner-pk">{pk_summary}</span>
                    " ?"
                </span>
                {move || match row_status() {
                    Some(EditStatus::Failed(msg)) => view! {
                        <span class="data-delete-banner-error" title=msg.clone()>{msg}</span>
                    }.into_view(),
                    _ => view! { <span/> }.into_view(),
                }}
                <button class="data-pg-btn"
                    disabled=in_flight
                    on:click=move |_| delete_pending.set(None)>"Cancel"</button>
                <button class="data-pg-btn data-delete-confirm-btn"
                    disabled=in_flight
                    on:click=move |_| confirm()>
                    {move || if in_flight() { "Deleting…" } else { "Delete" }}
                </button>
            </div>
        }.into_view()
    };

    // Render with reactive client filter so changing search_sig refines
    // the visible rows without refetching. Empty needle = render all.
    view! {
        <div class="data-grid-container">
        {banner_view}
        <div class="data-grid-scroll" attr:data-key=scroll_key
             on:scroll=move |ev| {
                 use wasm_bindgen::JsCast;
                 if let Some(el) = ev.target()
                     .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                 {
                     scroll_left.set(el.scroll_left());
                     scroll_top.set(el.scroll_top());
                 }
             }>
            <table class="data-grid">
                <thead>
                    <tr>{header_views}</tr>
                </thead>
                <tbody>
                    {move || {
                        let needle = search_sig.get().trim().to_ascii_lowercase();
                        // Carry the original row index through the filter so
                        // a click sets selected_row to the right slot AND the
                        // active class lights up the right tr after sort
                        // toggles, page nav, or filter narrows.
                        let filtered: Vec<(usize, &Vec<String>)> = if needle.is_empty() {
                            rows.iter().enumerate().collect()
                        } else {
                            rows.iter().enumerate()
                                .filter(|(_, r)| r.iter().any(|c| c.to_ascii_lowercase().contains(&needle)))
                                .collect()
                        };
                        if filtered.is_empty() {
                            return view! {
                                <tr><td colspan="99" class="data-cell-empty">"No rows match the visible filter."</td></tr>
                            }.into_view();
                        }
                        filtered.into_iter().map(|(idx, row)| {
                            let idx_for_active = idx;
                            let idx_for_click  = idx;
                            let row_owned = row.clone();
                            let row_for_edit = std::rc::Rc::new(row_owned);
                            let columns_for_row = columns.clone();
                            let pk_columns_for_row = pk_columns.clone();
                            let schema_row = schema.clone();
                            let name_row   = name.clone();
                            view! {
                                <tr class="data-row"
                                    attr:data-row-idx=idx.to_string()
                                    class:data-row-selected=move || selected_row.get() == Some(idx_for_active)
                                    on:click=move |ev| {
                                        use wasm_bindgen::JsCast;
                                        selected_row.set(Some(idx_for_click));
                                        // Disambiguate cell-click vs gutter-click
                                        // by walking up from `event.target`: if
                                        // the click landed inside a `.data-cell`,
                                        // that cell's own `on:click` has already
                                        // set `selected_col` to its index — leave
                                        // it alone. If the click missed every
                                        // cell (row gutter / padding) we default
                                        // to col 0 so the user lands on a visible
                                        // focus indicator regardless of how they
                                        // clicked the row.
                                        let on_cell = ev.target()
                                            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                                            .and_then(|el| el.closest(".data-cell").ok().flatten())
                                            .is_some();
                                        if !on_cell {
                                            selected_col.set(Some(0));
                                        }
                                    }>
                                    {columns_for_row.iter().enumerate().map(|(col_idx, col_name_ref)| {
                                        let cell = row_for_edit.get(col_idx).cloned().unwrap_or_default();
                                        let col_name = col_name_ref.clone();
                                        let col_is_pk = pk_set.contains(&col_name.to_ascii_lowercase());
                                        let editable_cell = editable && !col_is_pk;
                                        let row_for_edit_click = row_for_edit.clone();
                                        let pk_cols_for_click = pk_columns_for_row.clone();
                                        let schema_for_click = schema_row.clone();
                                        let name_for_click   = name_row.clone();
                                        let cell_for_click   = cell.clone();

                                        // Per-closure clones so each `move ||` owns its own copy.
                                        let cn_null   = col_name.clone();
                                        let cn_saving = col_name.clone();
                                        let cn_saved  = col_name.clone();
                                        let cn_failed = col_name.clone();
                                        let cn_title  = col_name.clone();
                                        let cn_dbl    = col_name.clone();
                                        let cn_body   = col_name.clone();

                                        view! {
                                            <td class="data-cell"
                                                attr:data-row-idx=idx.to_string()
                                                attr:data-col-idx=col_idx.to_string()
                                                class:data-cell-null=move || {
                                                    let _ = &cn_null;
                                                    cell == "NULL"
                                                }
                                                class:data-cell-editable=move || editable_cell
                                                // Cell-level focus (row+col) — drives the
                                                // 1px accent border that marks the active
                                                // grid cell. Mouse and keyboard both write
                                                // through `selected_row`/`selected_col`,
                                                // so the indicator is always coherent.
                                                class:data-cell-focus=move || {
                                                    selected_row.get() == Some(idx)
                                                        && selected_col.get() == Some(col_idx)
                                                }
                                                class:data-cell-saving=move ||
                                                    matches!(edit_status.with(|m| m.get(&(idx, cn_saving.clone())).cloned()),
                                                             Some(EditStatus::Saving))
                                                class:data-cell-saved=move ||
                                                    matches!(edit_status.with(|m| m.get(&(idx, cn_saved.clone())).cloned()),
                                                             Some(EditStatus::Saved))
                                                class:data-cell-failed=move ||
                                                    matches!(edit_status.with(|m| m.get(&(idx, cn_failed.clone())).cloned()),
                                                             Some(EditStatus::Failed(_)))
                                                title=move || match edit_status.with(|m| m.get(&(idx, cn_title.clone())).cloned()) {
                                                    Some(EditStatus::Failed(msg)) => msg,
                                                    _ => String::new(),
                                                }
                                                on:click=move |_| {
                                                    // Click on a cell sets the column too;
                                                    // the row-level on:click on `<tr>` handles
                                                    // setting the row. Both run on click
                                                    // (the cell handler doesn't stop
                                                    // propagation) so mouse-driven focus
                                                    // matches keyboard-driven focus.
                                                    selected_col.set(Some(col_idx));
                                                }
                                                on:dblclick=move |ev| {
                                                    use wasm_bindgen::JsCast;
                                                    let _ = ev.dyn_ref::<web_sys::Event>().map(|e| e.stop_propagation());
                                                    if !editable_cell { return; }
                                                    edit_input.set(cell_for_click.clone());
                                                    active_edit.set(Some((idx, cn_dbl.clone())));
                                                }>
                                                {move || {
                                                    let is_editing = active_edit.get()
                                                        .map(|(r, c)| r == idx && c == cn_body)
                                                        .unwrap_or(false);
                                                    if is_editing {
                                                        let row_clone = row_for_edit_click.clone();
                                                        let pk_clone  = pk_cols_for_click.clone();
                                                        let schema_e  = schema_for_click.clone();
                                                        let name_e    = name_for_click.clone();
                                                        let col_e     = cn_body.clone();
                                                        let commit = move || {
                                                            // Escape sets `aborting` and then clears
                                                            // active_edit, which unmounts the input and
                                                            // fires on:blur → here. Honor that signal so
                                                            // the typed-but-cancelled value doesn't reach
                                                            // the DB.
                                                            if edit_aborting.get_untracked() {
                                                                edit_aborting.set(false);
                                                                return;
                                                            }
                                                            let new_val = edit_input.get_untracked();
                                                            // Build pk_vals from the row data using the
                                                            // captured PK column indices.
                                                            let pk_cols_v: Vec<String> = pk_clone.iter()
                                                                .map(|(c, _)| c.clone()).collect();
                                                            let pk_vals_v: Vec<String> = pk_clone.iter()
                                                                .map(|(_, i)| row_clone.get(*i).cloned().unwrap_or_default())
                                                                .collect();
                                                            let sql = plane.update_row_sql(
                                                                &schema_e, &name_e, &col_e, &new_val,
                                                                &pk_cols_v, &pk_vals_v);
                                                            let edit_id = uuid::Uuid::new_v4().to_string();
                                                            let tag = format!("data_update:{}.{}:{}",
                                                                              schema_e, name_e, edit_id);
                                                            edit_pending.update(|m| {
                                                                m.insert(edit_id.clone(),
                                                                    (idx, col_e.clone(), new_val.clone()));
                                                            });
                                                            edit_status.update(|m| {
                                                                m.insert((idx, col_e.clone()), EditStatus::Saving);
                                                            });
                                                            if let Some(sid) = session.id.get_untracked() {
                                                                crate::dom::ws_dml(ws, &sid, sql, tag);
                                                            }
                                                            active_edit.set(None);
                                                        };
                                                        // Auto-focus + select-all on mount so a single
                                                        // dblclick (mouse) or Enter (keyboard) drops you
                                                        // straight into "type to replace" mode — Excel /
                                                        // Sheets convention. To extend instead of replace,
                                                        // press End (or →) once after entering edit mode.
                                                        //
                                                        // We defer the focus through a rAF because the
                                                        // input is mounted while the original Enter
                                                        // keydown is still on the call stack — calling
                                                        // focus() synchronously inside that tick gets
                                                        // ignored on Chromium.
                                                        let input_ref = create_node_ref::<leptos::html::Input>();
                                                        input_ref.on_load(move |el| {
                                                            let el_for_cb = el.clone();
                                                            if let Some(win) = web_sys::window() {
                                                                let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
                                                                    let _ = el_for_cb.focus();
                                                                    el_for_cb.select();
                                                                });
                                                                use wasm_bindgen::JsCast;
                                                                let _ = win.request_animation_frame(cb.unchecked_ref());
                                                            }
                                                        });
                                                        view! {
                                                            <input class="data-cell-input"
                                                                node_ref=input_ref
                                                                prop:value=move || edit_input.get()
                                                                on:input=move |e| edit_input.set(event_target_value(&e))
                                                                on:click=move |e| {
                                                                    use wasm_bindgen::JsCast;
                                                                    let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                                                                }
                                                                on:keydown={
                                                                    let commit_k = commit.clone();
                                                                    move |e: web_sys::KeyboardEvent| {
                                                                        match e.key().as_str() {
                                                                            "Enter"  => { e.prevent_default(); commit_k(); }
                                                                            "Escape" => {
                                                                                e.prevent_default();
                                                                                e.stop_propagation();
                                                                                edit_aborting.set(true);
                                                                                active_edit.set(None);
                                                                            }
                                                                            _ => {}
                                                                        }
                                                                    }
                                                                }
                                                                on:blur=move |_| commit()
                                                            />
                                                        }.into_view()
                                                    } else {
                                                        // Not editing — render the cell value
                                                        // (which may have been patched by a recent
                                                        // successful UPDATE).
                                                        view! { <span>{row_for_edit_click.get(col_idx).cloned().unwrap_or_default()}</span> }.into_view()
                                                    }
                                                }}
                                            </td>
                                        }
                                    }).collect_view()}
                                </tr>
                            }
                        }).collect_view()
                    }}
                </tbody>
            </table>
        </div>
        </div>
    }.into_view()
}

/// Render a single row as a vertical key/value form. One column per
/// line so wide values render in full instead of ellipsis-truncating.
/// Toolbar lets the user step Prev/Next through the page's rows
/// without flipping back to the grid. The same rendering will host
/// the future Insert form (phase 5) — a fresh row with empty values.
fn render_data_vertical(
    page:         DataPage,
    selected_row: RwSignal<Option<usize>>,
    view_mode:    RwSignal<DataViewMode>,
) -> leptos::View {
    let total = page.rows.len();
    if total == 0 {
        return view! {
            <div class="detail-msg">"No rows on this page to project."</div>
        }.into_view();
    }
    let columns = page.columns;
    let rows    = page.rows;

    view! {
        <div class="data-vertical">
            // Mini-toolbar: prev / next / position / back-to-grid
            <div class="data-vertical-toolbar">
                {move || {
                    let cur = selected_row.get().unwrap_or(0).min(total.saturating_sub(1));
                    let at_first = cur == 0;
                    let at_last  = cur + 1 >= total;
                    view! {
                        <button class="data-pg-btn" disabled=move || at_first
                            on:click=move |_| {
                                if !at_first {
                                    selected_row.set(Some(cur.saturating_sub(1)));
                                }
                            }>"‹ Prev row"</button>
                        <span class="data-vertical-pos">{format!("Row {} of {}", cur + 1, total)}</span>
                        <button class="data-pg-btn" disabled=move || at_last
                            on:click=move |_| {
                                if !at_last {
                                    selected_row.set(Some(cur + 1));
                                }
                            }>"Next row ›"</button>
                        <span class="data-pg-spacer"/>
                        <button class="data-pg-btn"
                            title="Back to grid"
                            on:click=move |_| view_mode.set(DataViewMode::Grid)>"⊞ Grid"</button>
                    }.into_view()
                }}
            </div>

            // Body: key/value list
            {move || {
                let cur = selected_row.get().unwrap_or(0).min(total.saturating_sub(1));
                let row = rows.get(cur).cloned().unwrap_or_default();
                let pairs: Vec<(String, String)> = columns.iter()
                    .zip(row.iter())
                    .map(|(c, v)| (c.clone(), v.clone()))
                    .collect();
                view! {
                    <div class="data-vertical-scroll">
                        <table class="data-vertical-table">
                            <tbody>
                                {pairs.into_iter().map(|(k, v)| {
                                    let is_null = v == "NULL";
                                    view! {
                                        <tr>
                                            <th class="data-vertical-key">{k}</th>
                                            <td class="data-vertical-val"
                                                class:data-cell-null=move || is_null>
                                                {v}
                                            </td>
                                        </tr>
                                    }
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                }
            }}
        </div>
    }.into_view()
}

/// Vertical form for inserting a new row. One input per column; each
/// input writes into `data_insert_buffer` keyed by column name. On
/// Save we omit any column the user left empty so the DB-side default
/// fires (and AUTO_INCREMENT / SERIAL keeps doing its job); typing
/// "NULL" (uppercase) emits a bare SQL NULL keyword. On success the
/// response handler in main.rs clears the buffer, flips back to
/// Grid mode, and bumps `data_refresh_tick` to refetch the page.
#[allow(clippy::too_many_arguments)]
fn render_data_insert(
    detail:        TableDetail,
    plane:         &'static dyn DbmsPlane,
    schema:        String,
    name:          String,
    ws:            RwSignal<Option<WebSocket>>,
    session:    ExplorerSession,
) -> leptos::View {
    let columns_sig   = detail.columns;
    let indexes_sig   = detail.indexes;
    let view_mode     = detail.data_view_mode;
    let insert_buffer = detail.data_insert_buffer;
    let insert_status = detail.data_insert_status;
    let insert_upsert = detail.data_insert_upsert;

    // Reactive `has_pk` derived from the indexes signal so the
    // Upsert checkbox auto-disables on tables without a detectable
    // primary key (no conflict target → no upsert in PG).
    let has_pk = move || {
        match indexes_sig.get() {
            LoadState::Loaded(ix) => ix.iter()
                .any(|i| i.name.eq_ignore_ascii_case("PRIMARY")),
            _ => false,
        }
    };

    let cancel = move || {
        insert_buffer.update(|m| m.clear());
        insert_status.set(None);
        insert_upsert.set(false);
        view_mode.set(DataViewMode::Grid);
    };

    let schema_save = schema.clone();
    let name_save   = name.clone();
    let save = move || {
        // Synchronous re-entrancy guard — the Save button's reactive
        // `disabled` only flips on the next render frame, so a fast
        // double-click could otherwise dispatch two INSERTs.
        if matches!(insert_status.get_untracked(), Some(EditStatus::Saving) | Some(EditStatus::Saved)) {
            return;
        }
        let cols_meta = match columns_sig.get_untracked() {
            LoadState::Loaded(c) => c,
            _ => return,
        };
        // Preserve the table's natural column order for the INSERT
        // — readability helps when a DBA squints at server logs.
        let buf = insert_buffer.get_untracked();
        let mut cols: Vec<String> = Vec::new();
        let mut vals: Vec<String> = Vec::new();
        for c in &cols_meta {
            if let Some(v) = buf.get(&c.name) {
                if !v.is_empty() {
                    cols.push(c.name.clone());
                    vals.push(v.clone());
                }
            }
        }
        // Upsert path: only when the toggle is on AND the table has
        // a PK we can use as the conflict target. Without a PK the
        // checkbox is disabled, but guard here too in case the index
        // signal was Loading at toggle time and PK info arrived
        // before Save.
        let pk_cols: Vec<String> = match indexes_sig.get_untracked() {
            LoadState::Loaded(ix) => ix.iter()
                .filter(|i| i.name.eq_ignore_ascii_case("PRIMARY"))
                .flat_map(|i| i.columns.iter().cloned())
                .collect(),
            _ => Vec::new(),
        };
        let sql = if insert_upsert.get_untracked() && !pk_cols.is_empty() {
            plane.upsert_row_sql(&schema_save, &name_save, &cols, &vals, &pk_cols)
        } else {
            plane.insert_row_sql(&schema_save, &name_save, &cols, &vals)
        };
        let tag = format!("data_insert:{}.{}", schema_save, name_save);
        insert_status.set(Some(EditStatus::Saving));
        match session.id.get_untracked() {
            Some(sid) => crate::dom::ws_dml(ws, &sid, sql, tag),
            None => {
                // Don't leave the user stuck on an indefinite "Saving…"
                // pill if the session dropped out before they hit Save.
                insert_status.set(Some(EditStatus::Failed(
                    "No active session — try reconnecting.".to_string(),
                )));
            }
        }
    };

    view! {
        <div class="data-vertical">
            <div class="data-vertical-toolbar">
                <button class="data-pg-btn"
                    title="Insert this row"
                    disabled=move || matches!(
                        insert_status.get(),
                        Some(EditStatus::Saving) | Some(EditStatus::Saved)
                    )
                    on:click=move |_| save()>"Save"</button>
                <button class="data-pg-btn"
                    title="Discard this insert"
                    on:click=move |_| cancel()>"Cancel"</button>
                <label class="data-insert-upsert-label"
                    title=move || if has_pk() {
                        "On PK conflict, update the existing row instead of erroring"
                    } else {
                        "Upsert needs a primary key — this table has none"
                    }>
                    <input type="checkbox"
                        prop:checked=move || insert_upsert.get()
                        prop:disabled=move || !has_pk()
                        on:change=move |e| insert_upsert.set(event_target_checked(&e))/>
                    "Upsert"
                </label>
                <span class="data-pg-spacer"/>
                {move || match insert_status.get() {
                    Some(EditStatus::Saving) =>
                        view! { <span class="data-insert-status">"Saving…"</span> }.into_view(),
                    Some(EditStatus::Saved) =>
                        view! { <span class="data-insert-status data-insert-status-saved">"Saved"</span> }.into_view(),
                    Some(EditStatus::Failed(msg)) => {
                        let msg_for_title = msg.clone();
                        view! { <span class="data-insert-status data-insert-status-failed" title=msg_for_title>{msg}</span> }.into_view()
                    }
                    None => view! { <span/> }.into_view(),
                }}
            </div>
            {move || match columns_sig.get() {
                LoadState::NotLoaded | LoadState::Loading => view! {
                    <div class="detail-msg">"Loading columns…"</div>
                }.into_view(),
                LoadState::Error(e) => view! {
                    <div class="detail-error">{e}</div>
                }.into_view(),
                LoadState::Loaded(cols) => {
                    view! {
                        <div class="data-vertical-scroll">
                            <table class="data-vertical-table">
                                <tbody>
                                    {cols.into_iter().map(|col| {
                                        let col_name           = col.name.clone();
                                        let col_name_for_input = col.name.clone();
                                        let col_name_for_value = col.name.clone();
                                        let col_type = col.col_type.clone();
                                        view! {
                                            <tr>
                                                <th class="data-vertical-key">
                                                    {col_name.clone()}
                                                    <span class="data-insert-type-hint">{col_type}</span>
                                                </th>
                                                <td class="data-vertical-val">
                                                    <input
                                                        class="data-cell-input data-insert-input"
                                                        prop:value=move || insert_buffer
                                                            .with(|m| m.get(&col_name_for_value).cloned().unwrap_or_default())
                                                        on:input=move |e| {
                                                            let v = event_target_value(&e);
                                                            insert_buffer.update(|m| {
                                                                m.insert(col_name_for_input.clone(), v);
                                                            });
                                                        }
                                                        placeholder="(default)"
                                                    />
                                                </td>
                                            </tr>
                                        }
                                    }).collect_view()}
                                </tbody>
                            </table>
                        </div>
                    }.into_view()
                }
            }}
        </div>
    }.into_view()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_pg_visibility_read_is_read_again_from_pg_class() {
        let refused = shared::DbError { sqlstate: Some("42501".into()), ..shared::DbError::from_message("permission denied for function pg_visibility_map_summary") };
        let other = shared::DbError { sqlstate: Some("42P01".into()), ..shared::DbError::from_message("relation does not exist") };
        // Every read offered pg_visibility that's refused — the second of
        // two in flight too — is read again.
        assert!(vm_read_refused(Some(&refused), true));
        // A pg_class read's failure is shown, whatever it is: no loop.
        assert!(!vm_read_refused(Some(&refused), false));
        // Another error, or none: the server's error stands.
        assert!(!vm_read_refused(Some(&other), true));
        assert!(!vm_read_refused(None, true));
    }

    /// A connected action shell plus the pane map the status reads.
    fn setup() -> (ShellTab, RwSignal<HashMap<String, ShellTab>>) {
        let shell = ShellTab::new("conn".into(), "rp_vna_db".into(), DbType::Postgres, String::new());
        shell.connecting.set(false);
        let panes = create_rw_signal(HashMap::from([(shell.id.clone(), shell.clone())]));
        (shell, panes)
    }

    fn run(shell: &ShellTab) -> ActionRun {
        ActionRun { pane_id: shell.id.clone(), sql: "VACUUM \"public\".\"exam\"".into() }
    }

    fn push(shell: &ShellTab, e: ShellEntry) {
        shell.shell_history.update(|h| h.push(e));
    }

    fn query(sql: &str) -> ShellEntry {
        ShellEntry::Query { num: 1, sql: sql.into(), terminator: ";".into(), meta: false, errored: false }
    }

    #[test]
    fn opening_until_the_shell_runs_its_startup_command() {
        let _rt = create_runtime();
        let (shell, panes) = setup();
        shell.connecting.set(true);
        shell.startup_sql.set(Some(run(&shell).sql));
        assert_eq!(action_run_status(&run(&shell), panes), RunStatus::Opening);
    }

    #[test]
    fn running_once_submitted_then_done_past_its_notices() {
        let _rt = create_runtime();
        let (shell, panes) = setup();
        let r = run(&shell);
        push(&shell, query(&r.sql));
        assert_eq!(action_run_status(&r, panes), RunStatus::Running);
        push(&shell, ShellEntry::Notice { severity: "INFO".into(), message: "vacuuming".into() });
        assert_eq!(action_run_status(&r, panes), RunStatus::Running);
        push(&shell, ShellEntry::Info { exec_time_us: 4_200_000, said: Vec::new(), timed: false });
        assert_eq!(action_run_status(&r, panes), RunStatus::Done { exec_time_us: 4_200_000 });
    }

    #[test]
    fn failed_when_the_command_errors() {
        let _rt = create_runtime();
        let (shell, panes) = setup();
        let r = run(&shell);
        push(&shell, query(&r.sql));
        push(&shell, ShellEntry::CliError {
            lines:   vec!["ERROR:  permission denied for table exam".into()],
            message: "permission denied for table exam".into(),
        });
        assert_eq!(action_run_status(&r, panes),
                   RunStatus::Failed("permission denied for table exam".into()));
    }

    #[test]
    fn failed_when_the_shell_never_connected() {
        let _rt = create_runtime();
        let (shell, panes) = setup();
        push(&shell, ShellEntry::Error("password authentication failed".into()));
        assert_eq!(action_run_status(&run(&shell), panes),
                   RunStatus::Failed("password authentication failed".into()));
    }

    #[test]
    fn reads_the_latest_run_of_a_repeated_command() {
        let _rt = create_runtime();
        let (shell, panes) = setup();
        let r = run(&shell);
        push(&shell, query(&r.sql));
        push(&shell, ShellEntry::Info { exec_time_us: 10, said: Vec::new(), timed: false });
        push(&shell, query(&r.sql));
        assert_eq!(action_run_status(&r, panes), RunStatus::Running);
    }

    #[test]
    fn closed_once_the_tab_is_gone() {
        let _rt = create_runtime();
        let (shell, panes) = setup();
        let r = run(&shell);
        panes.update(|m| { m.remove(&shell.id); });
        assert_eq!(action_run_status(&r, panes), RunStatus::Closed);
    }

    #[test]
    fn elapsed_reads_like_a_dba_would_say_it() {
        assert_eq!(format_elapsed(850), "850 ms");
        assert_eq!(format_elapsed(4_200), "4.2 s");
        assert_eq!(format_elapsed(192_000), "3 min 12 s");
    }
}
