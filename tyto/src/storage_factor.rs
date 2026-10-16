//! Storage factor classification + display formatting.
//!
//! The factor is the ratio of actual on-disk pages to a theoretical
//! minimum (live rows × ideal tuple size / page size). It captures
//! dead tuples + intra-page free space + alignment overhead +
//! fillfactor in one DBA-actionable number.
//!
//! - 1.0× = perfectly packed (every page full of live tuples)
//! - 1.5× = 50% over ideal — common steady-state for an OLTP table
//! - 2.0× = twice the ideal — rebuild candidate
//!
//! Thresholds chosen with the DBA in mind:
//! - ≤1.2× green: healthy, nothing to do
//! - 1.2–1.5× yellow: monitor; routine autovacuum should keep it here
//! - >1.5× red: consider VACUUM FULL (PG) / OPTIMIZE TABLE (MySQL)
//!
//! The plane SQL returns the factor as a decimal string (e.g.
//! `"1.82"`); parsing happens here so the SQL stays dialect-pure.

use crate::types::Severity;

/// Classify a storage factor into a severity band. NaN / infinite /
/// negative values map to [`Severity::Good`] (defensive — we don't
/// want a parse glitch to scream "your table is bloated").
pub(crate) fn classify(factor: f64) -> Severity {
    if !factor.is_finite() || factor <= 0.0 {
        return Severity::Good;
    }
    if factor <= 1.2      { Severity::Good }
    else if factor <= 1.5 { Severity::Watch }
    else                  { Severity::Bad }
}

/// Format the factor for display in the Overview cell.
/// `1.82` → `"1.8×"`. One decimal place; multiplication-sign glyph.
pub(crate) fn format_display(factor: f64) -> String {
    if !factor.is_finite() || factor <= 0.0 {
        return String::new();
    }
    format!("{:.1}\u{00d7}", factor)
}

/// Build the hover tooltip describing the breakdown. `actual_bytes`
/// is the on-disk size of the storage that the factor refers to;
/// `ideal_bytes` is `actual_bytes / factor`. The trailing action hint
/// names the right rebuild command for the layer being described —
/// `Tables` (VACUUM FULL / OPTIMIZE TABLE) vs `Indexes` (REINDEX).
/// Output reads like:
/// `"Storage factor: 1.8× of theoretical minimum (8.4 GB actual vs
/// 4.7 GB ideal). Consider REINDEX."`
pub(crate) fn build_tooltip(factor: f64, actual_bytes: u64, target: TooltipTarget) -> String {
    let ideal_bytes = if factor > 0.0 {
        (actual_bytes as f64 / factor) as u64
    } else {
        actual_bytes
    };
    let actual = crate::format::human_bytes(&actual_bytes.to_string());
    let ideal  = crate::format::human_bytes(&ideal_bytes.to_string());
    let action = match target {
        TooltipTarget::Table => "Consider VACUUM FULL (PG) or OPTIMIZE TABLE (MySQL).",
        TooltipTarget::Index => "Consider REINDEX.",
    };
    format!(
        "Storage factor: {:.1}\u{00d7} of theoretical minimum \
         ({} actual vs {} ideal). {}",
        factor, actual, ideal, action,
    )
}

/// What layer is being described — selects the action hint in the
/// tooltip ("VACUUM FULL / OPTIMIZE TABLE" vs "REINDEX").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TooltipTarget { Table, Index }

/// Classify a buffer-cache hit ratio (0-100 percent) into a severity
/// band. Higher = better (more accesses served from RAM). Thresholds
/// follow common OLTP-DBA wisdom:
///
/// - ≥99% green: cache is doing its job
/// - 95-99% yellow: mostly cached, some disk pressure
/// - <95% red: significant disk reads — investigate (more RAM /
///   smaller tables / missing indexes)
///
/// NaN / infinite / negative / >100 values default to Good
/// (defensive — same posture as `classify`).
pub(crate) fn classify_cache_hit(pct: f64) -> Severity {
    if !pct.is_finite() || pct < 0.0 || pct > 100.0 {
        return Severity::Good;
    }
    if pct >= 99.0      { Severity::Good }
    else if pct >= 95.0 { Severity::Watch }
    else                { Severity::Bad }
}

/// Build the cache-hit tooltip with the raw hit/read counts:
/// `"Buffer cache hit ratio: 99.7% (1,234,567 hits / 4,321 reads).
/// Higher = better; consider more RAM or smaller tables if low."`.
pub(crate) fn build_cache_hit_tooltip(pct: f64, hits: u64, reads: u64) -> String {
    let hits_fmt  = crate::format::format_int_with_commas(&hits.to_string());
    let reads_fmt = crate::format::format_int_with_commas(&reads.to_string());
    format!(
        "Buffer cache hit ratio: {:.1}% ({} hits / {} reads). \
         Higher = better; consider more RAM or smaller tables if low.",
        pct, hits_fmt, reads_fmt,
    )
}

/// Classify the idx-scan-percentage (index scans / total scans) into
/// a severity band. Mirrors [`classify_cache_hit`]'s shape:
///
/// - ≥80% green: mostly indexed access, OLTP-shaped
/// - 20-80% yellow: mixed — some seq scans, may want an index audit
/// - <20% red: overwhelmingly sequential; likely a missing index or
///   a query the planner can't route (or an intentional OLAP table)
///
/// NaN / infinite / negative / >100 values default to Good (defensive).
pub(crate) fn classify_idx_scan_pct(pct: f64) -> Severity {
    if !pct.is_finite() || pct < 0.0 || pct > 100.0 {
        return Severity::Good;
    }
    if pct >= 80.0      { Severity::Good }
    else if pct >= 20.0 { Severity::Watch }
    else                { Severity::Bad }
}

/// Classify the dead-tuple percentage into a severity band. Mirrors
/// PG's own autovacuum defaults:
///
/// - ≤5% green: autovacuum is comfortably keeping pace with churn
/// - 5-20% yellow: routine — autovacuum triggers around 20% by default
/// - >20% red: autovacuum should be running but hasn't, or is being
///   blocked by a long-running transaction holding back the xmin
///   horizon
///
/// NaN / infinite / negative / >100 values default to Good (defensive
/// — same posture as [`classify_cache_hit`]).
pub(crate) fn classify_dead_tup_pct(pct: f64) -> Severity {
    if !pct.is_finite() || pct < 0.0 || pct > 100.0 {
        return Severity::Good;
    }
    if pct <= 5.0        { Severity::Good }
    else if pct <= 20.0  { Severity::Watch }
    else                 { Severity::Bad }
}

/// Build the dead-tuples tooltip with the raw live / dead counts and
/// a reminder of PG's default autovacuum trigger.
pub(crate) fn build_dead_tup_tooltip(pct: f64, live: i64, dead: i64) -> String {
    let live_fmt = crate::format::format_int_with_commas_i64(live);
    let dead_fmt = crate::format::format_int_with_commas_i64(dead);
    format!(
        "Dead tuples: {:.1}% of live+dead rows ({} dead of {} live). \
         PG's default autovacuum triggers around 20% \
         (autovacuum_vacuum_scale_factor). Higher values mean vacuum \
         is falling behind — check for long-running transactions \
         holding back the xmin horizon.",
        pct, dead_fmt, live_fmt,
    )
}

/// Freeze-age severity — how close the table is to triggering an
/// anti-wraparound vacuum, expressed as `age(relfrozenxid) * 100 /
/// autovacuum_freeze_max_age`. Same shape for MultiXact age (relminmxid
/// / autovacuum_multixact_freeze_max_age); the ratio semantics are
/// identical so both use this classifier.
///
/// - <50% green: healthy headroom, autovacuum won't trigger on freeze
/// - 50-100% yellow: autovacuum should be handling this soon or now
/// - >100% red: anti-wraparound vacuum should already be running; if
///   it isn't, something's blocking it (typically a long-running txn
///   holding back the xmin horizon)
///
/// NaN / infinite / negative values default to Good.
pub(crate) fn classify_freeze_pct(pct: f64) -> Severity {
    if !pct.is_finite() || pct < 0.0 {
        return Severity::Good;
    }
    if pct < 50.0        { Severity::Good }
    else if pct <= 100.0 { Severity::Watch }
    else                 { Severity::Bad }
}

/// All-visible % severity for the Visibility Map section on the Storage
/// tab. Interprets what fraction of the table's pages are marked
/// all-visible in the visibility map.
///
/// - ≥95% green: index-only scans skip the heap on almost every page
/// - 70-95% yellow: recent writes have dirtied a chunk of pages; VACUUM
///   will restore
/// - <70% red: VM is significantly stale; index-only scans are doing a
///   lot of heap fetches, or the table is being written faster than
///   autovacuum can keep up
///
/// NaN / infinite / negative values default to Good (defensive).
pub(crate) fn classify_vm_visible_pct(pct: f64) -> Severity {
    if !pct.is_finite() || pct < 0.0 {
        return Severity::Good;
    }
    if pct >= 95.0     { Severity::Good }
    else if pct >= 70.0 { Severity::Watch }
    else               { Severity::Bad }
}

/// Visibility-map row tooltip: exact page counts plus how fresh the
/// number is, named by source. `kind` is "All-visible" or "All-frozen";
/// the pg_class column differs per kind (relallvisible / relallfrozen).
pub(crate) fn build_vm_tooltip(
    kind:       &str,
    pct:        f64,
    pages:      u64,
    total:      u64,
    source:     &str,
) -> String {
    let column = if kind == "All-frozen" { "relallfrozen" } else { "relallvisible" };
    let source_note = match source {
        "pg_visibility" => "live per-page read via pg_visibility".to_string(),
        "pg_class"      => format!("snapshot from the last VACUUM / ANALYZE (pg_class.{column})"),
        other           => format!("source: {other}"),
    };
    format!(
        "{kind}: {} of {} pages ({pct:.1}%)\n{source_note}",
        crate::format::format_int_with_commas_i64(pages as i64),
        crate::format::format_int_with_commas_i64(total as i64),
    )
}

/// Freeze-age tooltip. `label` differentiates XID ("transactions") from
/// MultiXact ("MultiXacts") in the wording so the DBA sees which
/// counter is driving the number. `max_setting_name` names the GUC
/// (`autovacuum_freeze_max_age` or `autovacuum_multixact_freeze_max_age`)
/// so the DBA can go tune it if needed.
pub(crate) fn build_freeze_tooltip(
    pct: f64,
    raw_age: i64,
    label: &str,
    max_setting_name: &str,
) -> String {
    let raw_fmt = crate::format::format_int_condensed(raw_age);
    format!(
        "{raw_fmt} {label} since freeze horizon ({pct:.0}% of \
         {max_setting_name}). At 100% autovacuum triggers an anti- \
         wraparound freeze. Values above 100% mean autovacuum should \
         already be running — if it isn't, check for a long-running \
         transaction holding back the xmin horizon.",
    )
}

/// Build the Index access tooltip with the raw idx / seq counts and
/// a sharpened explanation of what the ratio actually measures.
///
/// The value is not "percentage of queries that used an index" — it's
/// the fraction of executor access-method choices on this table where
/// the planner picked an index (Index Scan, Index Only Scan, or
/// Bitmap Index Scan) over a full sequential scan. A single query
/// that hits two indexes on the same table via a bitmap OR bumps the
/// counter by 2, so the number counts planner picks, not queries.
pub(crate) fn build_idx_scan_tooltip(pct: f64, idx: i64, seq: i64) -> String {
    let idx_fmt = crate::format::format_int_with_commas_i64(idx);
    let seq_fmt = crate::format::format_int_with_commas_i64(seq);
    format!(
        "Index access: {:.1}% of executor access-method choices on this \
         table used an index ({} idx scans vs {} seq scans since last \
         stats reset). Counts planner picks, not queries — one query \
         that hits two indexes on the same table (bitmap OR) bumps the \
         counter by 2. Low values point to missing indexes or \
         predicates the planner can't route.",
        pct, idx_fmt, seq_fmt,
    )
}

/// Parse a decimal-string factor (e.g. `"1.82"` or `"  1.82  "`).
/// Returns `None` on empty / unparseable input. Trims whitespace.
pub(crate) fn parse(raw: &str) -> Option<f64> {
    let t = raw.trim();
    if t.is_empty() { return None; }
    t.parse::<f64>().ok().filter(|f| f.is_finite() && *f > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify ──────────────────────────────────────────────────────

    #[test]
    fn classify_at_or_below_1_2_is_good() {
        assert_eq!(classify(1.00), Severity::Good);
        assert_eq!(classify(1.10), Severity::Good);
        assert_eq!(classify(1.20), Severity::Good);  // boundary
    }

    #[test]
    fn classify_between_1_2_and_1_5_is_watch() {
        // 1.20 is the boundary — classify_at_or_below_1_2_is_good
        // pins that. Use strictly-greater for Watch.
        assert_eq!(classify(1.21), Severity::Watch);
        assert_eq!(classify(1.30), Severity::Watch);
        assert_eq!(classify(1.50), Severity::Watch); // boundary
    }

    #[test]
    fn classify_above_1_5_is_bad() {
        assert_eq!(classify(1.51), Severity::Bad);
        assert_eq!(classify(2.00), Severity::Bad);
        assert_eq!(classify(10.0), Severity::Bad);
    }

    #[test]
    fn classify_pathological_inputs_defaults_to_good() {
        assert_eq!(classify(f64::NAN),      Severity::Good);
        assert_eq!(classify(f64::INFINITY), Severity::Good);
        assert_eq!(classify(0.0),           Severity::Good);
        assert_eq!(classify(-1.0),          Severity::Good);
    }

    // ── format_display ─────────────────────────────────────────────────

    #[test]
    fn format_display_rounds_to_one_decimal_with_multiplication_glyph() {
        assert_eq!(format_display(1.00),  "1.0\u{00d7}");
        assert_eq!(format_display(1.82),  "1.8\u{00d7}");
        assert_eq!(format_display(2.5),   "2.5\u{00d7}");
        assert_eq!(format_display(10.49), "10.5\u{00d7}");
    }

    #[test]
    fn format_display_pathological_is_empty() {
        assert_eq!(format_display(f64::NAN),      "");
        assert_eq!(format_display(f64::INFINITY), "");
        assert_eq!(format_display(0.0),           "");
        assert_eq!(format_display(-1.0),          "");
    }

    // ── parse ──────────────────────────────────────────────────────────

    #[test]
    fn parse_accepts_decimal_strings() {
        assert_eq!(parse("1.00"), Some(1.0));
        assert_eq!(parse("1.82"), Some(1.82));
        assert_eq!(parse("  2.5  "), Some(2.5));
    }

    #[test]
    fn parse_rejects_empty_or_unparseable() {
        assert_eq!(parse(""),    None);
        assert_eq!(parse("   "), None);
        assert_eq!(parse("abc"), None);
        assert_eq!(parse("-1.0"), None);  // negative rejected
        assert_eq!(parse("0.0"),  None);  // zero rejected
    }

    // ── build_tooltip ──────────────────────────────────────────────────

    #[test]
    fn build_tooltip_mentions_factor_and_both_sizes() {
        // 8 GiB actual / 1.6× = 5 GiB ideal. Use exact powers of
        // 1024 so human_bytes thresholds are easy to predict.
        let tip = build_tooltip(1.6, 8 * 1024 * 1024 * 1024, TooltipTarget::Table);
        assert!(tip.contains("1.6\u{00d7}"), "factor: {}", tip);
        assert!(tip.contains("GB"),          "should mention GB: {}", tip);
        assert!(tip.contains("VACUUM FULL"), "should mention VACUUM FULL: {}", tip);
    }

    #[test]
    fn build_tooltip_index_recommends_reindex_not_vacuum() {
        let tip = build_tooltip(1.6, 8 * 1024 * 1024, TooltipTarget::Index);
        assert!(tip.contains("REINDEX"),     "should mention REINDEX: {}", tip);
        assert!(!tip.contains("VACUUM"),     "should NOT mention VACUUM: {}", tip);
        assert!(!tip.contains("OPTIMIZE"),   "should NOT mention OPTIMIZE: {}", tip);
    }

    // ── classify_cache_hit ─────────────────────────────────────────────

    #[test]
    fn classify_cache_hit_at_or_above_99_is_good() {
        assert_eq!(classify_cache_hit(100.0), Severity::Good);
        assert_eq!(classify_cache_hit(99.5),  Severity::Good);
        assert_eq!(classify_cache_hit(99.0),  Severity::Good); // boundary
    }

    #[test]
    fn classify_cache_hit_95_to_99_is_watch() {
        assert_eq!(classify_cache_hit(98.9), Severity::Watch);
        assert_eq!(classify_cache_hit(97.0), Severity::Watch);
        assert_eq!(classify_cache_hit(95.0), Severity::Watch); // boundary
    }

    #[test]
    fn classify_cache_hit_below_95_is_bad() {
        assert_eq!(classify_cache_hit(94.9), Severity::Bad);
        assert_eq!(classify_cache_hit(50.0), Severity::Bad);
        assert_eq!(classify_cache_hit(0.0),  Severity::Bad);
    }

    #[test]
    fn classify_cache_hit_pathological_defaults_to_good() {
        // NaN / infinite / negative / > 100 — defensive; we don't want
        // a parse glitch to scream red.
        assert_eq!(classify_cache_hit(f64::NAN),      Severity::Good);
        assert_eq!(classify_cache_hit(f64::INFINITY), Severity::Good);
        assert_eq!(classify_cache_hit(-1.0),          Severity::Good);
        assert_eq!(classify_cache_hit(101.0),         Severity::Good);
    }

    // ── build_cache_hit_tooltip ────────────────────────────────────────

    #[test]
    fn build_cache_hit_tooltip_includes_pct_and_counts() {
        let tip = build_cache_hit_tooltip(99.7, 1_234_567, 4_321);
        assert!(tip.contains("99.7%"),       "factor: {}", tip);
        assert!(tip.contains("1,234,567"),   "should comma-format hits: {}", tip);
        assert!(tip.contains("4,321"),       "should comma-format reads: {}", tip);
        assert!(tip.contains("Higher = better"), "should explain direction: {}", tip);
    }

    // ── classify_idx_scan_pct ──────────────────────────────────────────

    #[test]
    fn classify_idx_scan_pct_at_or_above_80_is_good() {
        assert_eq!(classify_idx_scan_pct(100.0), Severity::Good);
        assert_eq!(classify_idx_scan_pct(95.3),  Severity::Good);
        assert_eq!(classify_idx_scan_pct(80.0),  Severity::Good); // boundary
    }

    #[test]
    fn classify_idx_scan_pct_20_to_80_is_watch() {
        assert_eq!(classify_idx_scan_pct(79.9), Severity::Watch);
        assert_eq!(classify_idx_scan_pct(50.0), Severity::Watch);
        assert_eq!(classify_idx_scan_pct(20.0), Severity::Watch); // boundary
    }

    #[test]
    fn classify_idx_scan_pct_below_20_is_bad() {
        assert_eq!(classify_idx_scan_pct(19.9), Severity::Bad);
        assert_eq!(classify_idx_scan_pct(5.0),  Severity::Bad);
        assert_eq!(classify_idx_scan_pct(0.0),  Severity::Bad);
    }

    #[test]
    fn classify_idx_scan_pct_pathological_defaults_to_good() {
        // Same defensive posture as classify_cache_hit — a parse glitch
        // should never scream red.
        assert_eq!(classify_idx_scan_pct(f64::NAN),      Severity::Good);
        assert_eq!(classify_idx_scan_pct(f64::INFINITY), Severity::Good);
        assert_eq!(classify_idx_scan_pct(-1.0),          Severity::Good);
        assert_eq!(classify_idx_scan_pct(101.0),         Severity::Good);
    }

    // ── build_idx_scan_tooltip ─────────────────────────────────────────

    #[test]
    fn build_idx_scan_tooltip_includes_pct_and_counts() {
        let tip = build_idx_scan_tooltip(95.3, 1_234_567, 61);
        assert!(tip.contains("95.3%"),              "pct: {}", tip);
        assert!(tip.contains("1,234,567"),          "should comma-format idx: {}", tip);
        assert!(tip.contains("61 seq"),             "should mention seq count: {}", tip);
        assert!(tip.contains("missing indexes"),    "should hint at diagnosis: {}", tip);
    }

    #[test]
    fn build_idx_scan_tooltip_handles_zero_counts() {
        // Not a real reachable state (parser guards against 0 total)
        // but the tooltip fn shouldn't panic if callers pass zeroes.
        let tip = build_idx_scan_tooltip(0.0, 0, 0);
        assert!(tip.contains("0.0%"));
        assert!(tip.contains("0 idx"));
        assert!(tip.contains("0 seq"));
    }

    // ── classify_dead_tup_pct ──────────────────────────────────────────

    #[test]
    fn classify_dead_tup_pct_at_or_below_5_is_good() {
        assert_eq!(classify_dead_tup_pct(0.0), Severity::Good);
        assert_eq!(classify_dead_tup_pct(4.9), Severity::Good);
        assert_eq!(classify_dead_tup_pct(5.0), Severity::Good); // boundary
    }

    #[test]
    fn classify_dead_tup_pct_5_to_20_is_watch() {
        assert_eq!(classify_dead_tup_pct(5.1),  Severity::Watch);
        assert_eq!(classify_dead_tup_pct(15.0), Severity::Watch);
        assert_eq!(classify_dead_tup_pct(20.0), Severity::Watch); // boundary
    }

    #[test]
    fn classify_dead_tup_pct_above_20_is_bad() {
        assert_eq!(classify_dead_tup_pct(20.1), Severity::Bad);
        assert_eq!(classify_dead_tup_pct(50.0), Severity::Bad);
        assert_eq!(classify_dead_tup_pct(99.9), Severity::Bad);
    }

    #[test]
    fn classify_dead_tup_pct_pathological_defaults_to_good() {
        assert_eq!(classify_dead_tup_pct(f64::NAN),      Severity::Good);
        assert_eq!(classify_dead_tup_pct(f64::INFINITY), Severity::Good);
        assert_eq!(classify_dead_tup_pct(-1.0),          Severity::Good);
        assert_eq!(classify_dead_tup_pct(101.0),         Severity::Good);
    }

    // ── build_dead_tup_tooltip ─────────────────────────────────────────

    #[test]
    fn build_dead_tup_tooltip_mentions_counts_and_threshold() {
        let tip = build_dead_tup_tooltip(12.3, 1_000_000, 140_000);
        assert!(tip.contains("12.3%"),               "pct: {}", tip);
        assert!(tip.contains("140,000"),             "dead comma: {}", tip);
        assert!(tip.contains("1,000,000"),           "live comma: {}", tip);
        assert!(tip.contains("autovacuum"),          "should mention autovacuum: {}", tip);
    }

    // ── visibility map ─────────────────────────────────────────────────

    #[test]
    fn classify_vm_visible_pct_bands() {
        assert_eq!(classify_vm_visible_pct(99.0), Severity::Good);
        assert_eq!(classify_vm_visible_pct(95.0), Severity::Good);  // boundary
        assert_eq!(classify_vm_visible_pct(80.0), Severity::Watch);
        assert_eq!(classify_vm_visible_pct(69.9), Severity::Bad);
    }

    #[test]
    fn vm_tooltip_names_count_and_source_column() {
        let tip = build_vm_tooltip("All-frozen", 96.7, 9_885_717, 10_226_355, "pg_class");
        assert!(tip.contains("9,885,717 of 10,226,355 pages (96.7%)"), "{tip}");
        assert!(tip.contains("pg_class.relallfrozen"), "{tip}");
        let tip = build_vm_tooltip("All-visible", 98.8, 1, 1, "pg_visibility");
        assert!(tip.contains("live per-page read"), "{tip}");
    }

    // ── classify_freeze_pct ────────────────────────────────────────────
    //
    // Bands: <50 Good / 50-100 Watch / >100 Bad. Unlike dead_tup_pct,
    // freeze percentages ABOVE 100 are meaningful — they
    // mean autovacuum should already be running and something is blocking
    // it — so the >100 case must land in Bad, not fall through to Good.

    #[test]
    fn classify_freeze_pct_below_50_is_good() {
        assert_eq!(classify_freeze_pct(0.0),  Severity::Good);
        assert_eq!(classify_freeze_pct(25.0), Severity::Good);
        assert_eq!(classify_freeze_pct(49.9), Severity::Good);
    }

    #[test]
    fn classify_freeze_pct_50_to_100_is_watch() {
        assert_eq!(classify_freeze_pct(50.0),  Severity::Watch); // boundary
        assert_eq!(classify_freeze_pct(75.0),  Severity::Watch);
        assert_eq!(classify_freeze_pct(99.9),  Severity::Watch);
        assert_eq!(classify_freeze_pct(100.0), Severity::Watch); // boundary
    }

    #[test]
    fn classify_freeze_pct_above_100_is_bad() {
        // Values above 100% ARE meaningful for freeze — autovacuum
        // hasn't kept up. Must NOT clamp back to Good.
        assert_eq!(classify_freeze_pct(100.1), Severity::Bad);
        assert_eq!(classify_freeze_pct(150.0), Severity::Bad);
        assert_eq!(classify_freeze_pct(999.9), Severity::Bad);
    }

    #[test]
    fn classify_freeze_pct_pathological_defaults_to_good() {
        assert_eq!(classify_freeze_pct(f64::NAN),          Severity::Good);
        assert_eq!(classify_freeze_pct(f64::INFINITY),     Severity::Good);
        assert_eq!(classify_freeze_pct(f64::NEG_INFINITY), Severity::Good);
        assert_eq!(classify_freeze_pct(-1.0),              Severity::Good);
    }

    // ── build_freeze_tooltip ───────────────────────────────────────────

    #[test]
    fn build_freeze_tooltip_mentions_raw_pct_setting_and_hint() {
        let tip = build_freeze_tooltip(
            60.0,
            120_000_000,
            "transactions",
            "autovacuum_freeze_max_age",
        );
        assert!(tip.contains("60%"),                        "pct: {}", tip);
        assert!(tip.contains("120M"),                       "condensed raw: {}", tip);
        assert!(tip.contains("transactions"),               "label: {}", tip);
        assert!(tip.contains("autovacuum_freeze_max_age"),  "GUC name: {}", tip);
        assert!(tip.contains("long-running transaction"),   "hint: {}", tip);
    }

    #[test]
    fn build_freeze_tooltip_swaps_label_for_multixact() {
        let tip = build_freeze_tooltip(
            15.0,
            60_000_000,
            "MultiXacts",
            "autovacuum_multixact_freeze_max_age",
        );
        assert!(tip.contains("MultiXacts"),                            "label: {}", tip);
        assert!(tip.contains("autovacuum_multixact_freeze_max_age"),   "GUC: {}", tip);
    }
}
