//! Autovacuum presentation for the Storage tab.
//!
//! Turns the `autovac:*` payload (knobs + counters, one snapshot) into
//! the two tables the section renders: a Triggers table answering "how
//! close is autovacuum to firing?" and a Settings table answering "why
//! that threshold?". Pure functions over plain data, so the formulas are
//! unit-tested natively.
//!
//! Trigger formulas follow the PG 18 docs (§24.1.6, The Autovacuum
//! Daemon). `reltuples` is `pg_class.reltuples`, taken as 0 when it is
//! -1 (never vacuumed / analyzed), exactly as autovacuum does:
//!
//! ```text
//! Vacuum            dead tuples          > min(max_threshold, threshold + scale × reltuples)
//! Insert            inserts since vacuum > threshold + scale × reltuples × unfrozen
//! Analyze           changes since analyze > threshold + scale × reltuples
//! Freeze            age(relfrozenxid)    > autovacuum_freeze_max_age
//! MultiXact freeze  mxid_age(relminmxid) > autovacuum_multixact_freeze_max_age
//! ```
//!
//! `max_threshold` is PG 18+ (-1 disables the cap). The insert trigger
//! is PG 13+ (threshold -1 disables it); `unfrozen` is
//! `1 − relallfrozen / relpages` on PG 18+ and 1 before. TOAST
//! relations are never analyzed.
//!
//! Override rules from the CREATE TABLE storage-parameter docs: a
//! per-table `*_freeze_max_age` larger than the cluster value is
//! ignored; `*_freeze_min_age` is limited to half the cluster max age
//! and `*_freeze_table_age` to 95% of it. `-1` cost settings defer to
//! the `vacuum_cost_*` GUCs. Partitioned tables are never processed.

use std::collections::HashMap;

use crate::format::{format_int_condensed, format_int_with_commas_i64};
use crate::types::{AutovacCounters, AutovacEntry, AutovacSettings, AutovacSource, Severity};

/// What the Autovacuum section shows for one relation.
pub(crate) enum AutovacView {
    /// Partitioned parent — autovacuum never processes it, so the
    /// trigger and settings tables would describe nothing that happens.
    PartitionedParent,
    Table {
        /// Set when autovacuum is switched off for this table (reloption)
        /// or the whole cluster; names which switch and what still runs.
        banner:   Option<String>,
        triggers: Vec<TriggerRow>,
        /// Settings grouped to mirror the trigger rows; the renderer
        /// adds spacing between groups.
        settings: Vec<Vec<SettingRow>>,
    },
}

/// One run of text in a trigger's rule column. `overridden` marks a
/// value that comes from this table's own storage parameter.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Segment {
    pub(crate) text:       String,
    pub(crate) overridden: bool,
}

pub(crate) struct TriggerRow {
    pub(crate) label:    &'static str,
    pub(crate) measures: &'static str,
    pub(crate) current:  Option<i64>,
    pub(crate) fires_at: Option<f64>,
    pub(crate) rule:     Vec<Segment>,
    /// Percent of the way to firing. `INFINITY` when the threshold is 0
    /// and the counter is already past it.
    pub(crate) progress: Option<f64>,
    pub(crate) severity: Severity,
    /// Why this trigger can't fire right now (autovacuum off, insert
    /// vacuums disabled). The row renders muted with this in place of
    /// the progress bar.
    pub(crate) inactive: Option<&'static str>,
    pub(crate) tooltip:  String,
}

pub(crate) struct SettingRow {
    pub(crate) name:    String,
    pub(crate) value:   String,
    pub(crate) source:  AutovacSource,
    pub(crate) note:    Option<String>,
    pub(crate) tooltip: String,
}

/// Plain-word label for a setting's source.
pub(crate) fn source_label(source: AutovacSource) -> &'static str {
    match source {
        AutovacSource::Reloption => "table",
        AutovacSource::Cluster   => "cluster",
        AutovacSource::Default   => "default",
    }
}

fn source_explanation(source: AutovacSource) -> &'static str {
    match source {
        AutovacSource::Reloption => "table: storage parameter (reloption) set on this table with ALTER TABLE … SET (…)",
        AutovacSource::Cluster   => "cluster: server setting changed from its built-in default",
        AutovacSource::Default   => "default: built-in default, not changed on this server",
    }
}

/// Progress severity: under 100% autovacuum isn't due yet; up to 150%
/// it is due or just past; beyond that something is holding it back.
pub(crate) fn progress_severity(pct: f64) -> Severity {
    if pct.is_nan() || pct < 100.0 { Severity::Good }
    else if pct <= 150.0            { Severity::Watch }
    else                            { Severity::Bad }
}

/// Display text for a progress value: one decimal, or "due" when the
/// threshold is zero and already crossed.
pub(crate) fn progress_text(pct: f64) -> String {
    if pct.is_infinite() { "due".into() } else { format!("{pct:.1}%") }
}

pub(crate) fn build(av: &AutovacSettings) -> AutovacView {
    if av.counters.relkind == "p" {
        return AutovacView::PartitionedParent;
    }
    let knobs = Knobs::new(&av.entries);
    let c = &av.counters;

    // Each switch that stops activity-driven autovacuum: the short
    // label muted trigger rows show, and the banner sentence.
    let switches: [(&str, &'static str, &'static str); 3] = [
        ("autovacuum", "autovacuum off",
         "Autovacuum is off for the whole cluster (autovacuum = off)."),
        ("track_counts", "track_counts off",
         "track_counts is off, so autovacuum can't see table activity."),
        ("autovacuum_enabled", "off for this table",
         "Autovacuum is disabled for this table (autovacuum_enabled = off)."),
    ];
    let off: Vec<_> = switches.iter()
        .filter(|(name, _, _)| knobs.flag(name) == Some(false))
        .collect();
    let banner = (!off.is_empty()).then(|| format!(
        "{} Only anti-wraparound (freeze) vacuums will run.",
        off.iter().map(|(_, _, sentence)| *sentence).collect::<Vec<_>>().join(" "),
    ));
    let activity_off = off.first().map(|(_, label, _)| *label);

    let rows = Rows::from(c.reltuples);
    let mut triggers = Vec::new();
    triggers.push(vacuum_trigger(&knobs, c, rows, activity_off));
    if let Some(t) = insert_trigger(&knobs, c, rows, activity_off) { triggers.push(t); }
    if c.relkind != "t" {
        triggers.push(analyze_trigger(&knobs, c, rows, activity_off));
    }
    triggers.push(freeze_trigger(
        &knobs, "Freeze", "XID age", "autovacuum_freeze_max_age", c.xid_age,
    ));
    triggers.push(freeze_trigger(
        &knobs, "MultiXact freeze", "MXID age", "autovacuum_multixact_freeze_max_age", c.mxid_age,
    ));

    AutovacView::Table { banner, triggers, settings: setting_groups(&knobs) }
}

// ── Knob lookup ─────────────────────────────────────────────────────

struct Knobs<'a> {
    by_name: HashMap<&'a str, &'a AutovacEntry>,
    order:   Vec<&'a AutovacEntry>,
}

/// An effective value after PG's override and clamping rules.
struct Resolved {
    value:      f64,
    from_table: bool,
}

impl<'a> Knobs<'a> {
    fn new(entries: &'a [AutovacEntry]) -> Self {
        Knobs {
            by_name: entries.iter().map(|e| (e.setting.as_str(), e)).collect(),
            order:   entries.iter().collect(),
        }
    }

    fn get(&self, name: &str) -> Option<&'a AutovacEntry> {
        self.by_name.get(name).copied()
    }

    fn flag(&self, name: &str) -> Option<bool> {
        self.get(name).and_then(|e| parse_bool(&e.effective_value))
    }

    fn cluster_num(&self, name: &str) -> Option<f64> {
        self.get(name)?.cluster_value.as_deref()?.trim().parse().ok()
    }

    /// Effective numeric value with the per-table override rules
    /// applied: a `*_freeze_max_age` reloption larger than the cluster
    /// value is ignored.
    fn resolve(&self, name: &str) -> Option<Resolved> {
        let e = self.get(name)?;
        let value: f64 = e.effective_value.trim().parse().ok()?;
        let from_table = e.source == AutovacSource::Reloption;
        if from_table && is_max_age(name) {
            if let Some(cluster) = self.cluster_num(name) {
                if value > cluster {
                    return Some(Resolved { value: cluster, from_table: false });
                }
            }
        }
        Some(Resolved { value, from_table })
    }
}

fn is_max_age(name: &str) -> bool {
    matches!(name, "autovacuum_freeze_max_age" | "autovacuum_multixact_freeze_max_age")
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" | "t" | "y" => Some(true),
        "off" | "false" | "no" | "0" | "f" | "n" => Some(false),
        _ => None,
    }
}

// ── Trigger rows ────────────────────────────────────────────────────

/// `pg_class.reltuples` as autovacuum uses it: -1 counts as 0, but the
/// display keeps saying "never analyzed" so the signal isn't lost.
#[derive(Clone, Copy)]
struct Rows {
    count:          f64,
    never_analyzed: bool,
    known:          bool,
}

impl Rows {
    fn from(reltuples: Option<i64>) -> Self {
        match reltuples {
            Some(n) if n < 0 => Rows { count: 0.0, never_analyzed: true, known: true },
            Some(n)          => Rows { count: n as f64, never_analyzed: false, known: true },
            None             => Rows { count: 0.0, never_analyzed: false, known: false },
        }
    }

    fn text(self) -> String {
        if self.never_analyzed { "0 rows (never analyzed)".into() }
        else { format!("{} rows", format_int_condensed(self.count as i64)) }
    }
}

fn seg(text: impl Into<String>) -> Segment {
    Segment { text: text.into(), overridden: false }
}

fn knob_seg(r: &Resolved) -> Segment {
    Segment { text: fmt_number(r.value, true), overridden: r.from_table }
}

/// Numbers in rules and fires-at columns: condensed when large
/// (`500K`), otherwise as written (`0.02`, `50`).
fn fmt_number(v: f64, condense: bool) -> String {
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        let n = v as i64;
        if condense { format_int_condensed(n) } else { format_int_with_commas_i64(n) }
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn percent_of(current: Option<i64>, fires_at: f64) -> Option<f64> {
    let cur = current? as f64;
    Some(if fires_at > 0.0 {
        100.0 * cur / fires_at
    } else if cur > 0.0 {
        f64::INFINITY
    } else {
        0.0
    })
}

fn severity_of(progress: Option<f64>, inactive: Option<&str>) -> Severity {
    match (progress, inactive) {
        (Some(p), None) => progress_severity(p),
        _               => Severity::Neutral,
    }
}

fn scaled_rule(base: &Resolved, scale: &Resolved, rows: Rows) -> Vec<Segment> {
    vec![knob_seg(base), seg(" + "), knob_seg(scale), seg(format!(" × {}", rows.text()))]
}

fn current_line(label: &str, current: Option<i64>, progress: Option<f64>) -> String {
    match (current, progress) {
        (Some(n), Some(p)) => format!("{label} now: {} ({})", format_int_with_commas_i64(n), progress_text(p)),
        (Some(n), None)    => format!("{label} now: {}", format_int_with_commas_i64(n)),
        (None, _)          => format!("{label}: no statistics yet"),
    }
}

fn vacuum_trigger(k: &Knobs, c: &AutovacCounters, rows: Rows, off: Option<&'static str>) -> TriggerRow {
    let (Some(base), Some(scale)) = (
        k.resolve("autovacuum_vacuum_threshold"),
        k.resolve("autovacuum_vacuum_scale_factor"),
    ) else {
        return unknown_trigger("Vacuum", "dead tuples", c.n_dead_tup);
    };
    // -1 disables the cap; absent before PG 18.
    let cap = k.resolve("autovacuum_vacuum_max_threshold").filter(|m| m.value >= 0.0);
    let uncapped = base.value + scale.value * rows.count;
    let (fires_at, rule) = match &cap {
        Some(m) if m.value < uncapped => {
            let mut r = vec![seg("min("), knob_seg(m), seg(", ")];
            r.extend(scaled_rule(&base, &scale, rows));
            r.push(seg(")"));
            (m.value, r)
        }
        _ => (uncapped, scaled_rule(&base, &scale, rows)),
    };
    let progress = rows.known.then(|| percent_of(c.n_dead_tup, fires_at)).flatten();
    let cap_line = match &cap {
        Some(m) => format!("Capped at autovacuum_vacuum_max_threshold = {}", fmt_number(m.value, false)),
        None    => "No cap (autovacuum_vacuum_max_threshold absent or -1)".into(),
    };
    let tooltip = format!(
        "Vacuum fires when dead tuples exceed\n\
         autovacuum_vacuum_threshold + autovacuum_vacuum_scale_factor × reltuples\n\
         = {} + {} × {} = {}\n{}\nFires at: {}\n{}",
        fmt_number(base.value, false), fmt_number(scale.value, false),
        rows_exact(rows), fmt_number(uncapped.round(), false),
        cap_line, fmt_number(fires_at.round(), false),
        current_line("Dead tuples", c.n_dead_tup, progress),
    );
    TriggerRow {
        label: "Vacuum", measures: "dead tuples",
        current: c.n_dead_tup, fires_at: Some(fires_at), rule,
        severity: severity_of(progress, off), progress, inactive: off, tooltip,
    }
}

fn insert_trigger(k: &Knobs, c: &AutovacCounters, rows: Rows, off: Option<&'static str>) -> Option<TriggerRow> {
    // Knob absent → server predates insert-driven vacuums (PG 12).
    let base = k.resolve("autovacuum_vacuum_insert_threshold")?;
    let Some(scale) = k.resolve("autovacuum_vacuum_insert_scale_factor") else {
        return Some(unknown_trigger("Insert", "inserts since vacuum", c.n_ins_since_vacuum));
    };
    if base.value < 0.0 {
        return Some(TriggerRow {
            label: "Insert", measures: "inserts since vacuum",
            current: c.n_ins_since_vacuum, fires_at: None,
            rule: vec![knob_seg(&base)],
            progress: None, severity: Severity::Neutral,
            inactive: Some("disabled"),
            tooltip: "Insert-driven vacuums are disabled for this table \
                      (autovacuum_vacuum_insert_threshold = -1).".into(),
        });
    }
    // PG 18 scales by the unfrozen fraction of the table; earlier
    // servers don't report relallfrozen and use the whole table.
    let unfrozen = match (c.relallfrozen, c.relpages) {
        (Some(af), Some(pages)) if pages > 0 && af > 0 => {
            1.0 - (af.min(pages) as f64 / pages as f64)
        }
        _ => 1.0,
    };
    let fires_at = base.value + scale.value * rows.count * unfrozen;
    let mut rule = scaled_rule(&base, &scale, rows);
    if unfrozen < 1.0 {
        rule.push(seg(format!(" × {:.0}% unfrozen", unfrozen * 100.0)));
    }
    let progress = rows.known.then(|| percent_of(c.n_ins_since_vacuum, fires_at)).flatten();
    let unfrozen_line = if c.relallfrozen.is_some() {
        format!("\n× unfrozen fraction (1 − relallfrozen / relpages) = {:.1}%", unfrozen * 100.0)
    } else {
        String::new()
    };
    let tooltip = format!(
        "Insert vacuum fires when inserts since the last vacuum exceed\n\
         autovacuum_vacuum_insert_threshold + autovacuum_vacuum_insert_scale_factor × reltuples\n\
         = {} + {} × {}{}\nFires at: {}\n{}",
        fmt_number(base.value, false), fmt_number(scale.value, false),
        rows_exact(rows), unfrozen_line, fmt_number(fires_at.round(), false),
        current_line("Inserts since vacuum", c.n_ins_since_vacuum, progress),
    );
    Some(TriggerRow {
        label: "Insert", measures: "inserts since vacuum",
        current: c.n_ins_since_vacuum, fires_at: Some(fires_at), rule,
        severity: severity_of(progress, off), progress, inactive: off, tooltip,
    })
}

fn analyze_trigger(k: &Knobs, c: &AutovacCounters, rows: Rows, off: Option<&'static str>) -> TriggerRow {
    let (Some(base), Some(scale)) = (
        k.resolve("autovacuum_analyze_threshold"),
        k.resolve("autovacuum_analyze_scale_factor"),
    ) else {
        return unknown_trigger("Analyze", "changes since analyze", c.n_mod_since_analyze);
    };
    let fires_at = base.value + scale.value * rows.count;
    let progress = rows.known.then(|| percent_of(c.n_mod_since_analyze, fires_at)).flatten();
    let tooltip = format!(
        "Analyze fires when rows inserted, updated or deleted since the last ANALYZE exceed\n\
         autovacuum_analyze_threshold + autovacuum_analyze_scale_factor × reltuples\n\
         = {} + {} × {} = {}\n{}",
        fmt_number(base.value, false), fmt_number(scale.value, false),
        rows_exact(rows), fmt_number(fires_at.round(), false),
        current_line("Changes since analyze", c.n_mod_since_analyze, progress),
    );
    TriggerRow {
        label: "Analyze", measures: "changes since analyze",
        current: c.n_mod_since_analyze, fires_at: Some(fires_at),
        rule: scaled_rule(&base, &scale, rows),
        severity: severity_of(progress, off), progress, inactive: off, tooltip,
    }
}

/// Anti-wraparound triggers run even when autovacuum is switched off,
/// so these rows are never marked inactive.
fn freeze_trigger(
    k: &Knobs,
    label: &'static str,
    measures: &'static str,
    setting: &'static str,
    age: Option<i64>,
) -> TriggerRow {
    let Some(max) = k.resolve(setting) else {
        return unknown_trigger(label, measures, age);
    };
    let progress = percent_of(age, max.value);
    let ignored = k.get(setting)
        .filter(|e| e.source == AutovacSource::Reloption && !max.from_table)
        .map(|e| format!(
            "\nThe table's own value ({}) is ignored: it is larger than the cluster setting.",
            fmt_number(e.effective_value.trim().parse().unwrap_or(0.0), false),
        ))
        .unwrap_or_default();
    let tooltip = format!(
        "An anti-wraparound vacuum is forced when {} exceeds {}.\n\
         Runs even when autovacuum is switched off.\nFires at: {}{}\n{}",
        measures, setting, fmt_number(max.value, false), ignored,
        current_line(measures, age, progress),
    );
    TriggerRow {
        label, measures, current: age, fires_at: Some(max.value),
        rule: vec![Segment { text: setting.into(), overridden: max.from_table }],
        severity: severity_of(progress, None), progress, inactive: None, tooltip,
    }
}

/// Row for a trigger whose settings didn't come back — keeps the
/// counter visible rather than dropping the row.
fn unknown_trigger(label: &'static str, measures: &'static str, current: Option<i64>) -> TriggerRow {
    TriggerRow {
        label, measures, current, fires_at: None, rule: vec![seg("settings unavailable")],
        progress: None, severity: Severity::Neutral, inactive: None,
        tooltip: format!("The server didn't report the settings for the {} trigger.", label.to_lowercase()),
    }
}

fn rows_exact(rows: Rows) -> String {
    if !rows.known { "reltuples (unknown)".into() }
    else if rows.never_analyzed { "0 (reltuples = -1, never analyzed)".into() }
    else { format_int_with_commas_i64(rows.count as i64) }
}

// ── Settings rows ───────────────────────────────────────────────────

/// Groups mirror the trigger rows top to bottom. Settings a server
/// doesn't know are simply absent; anything unlisted lands in a final
/// group so a new knob from the plane is never silently dropped.
const GROUPS: &[&[&str]] = &[
    &["autovacuum", "track_counts", "autovacuum_enabled"],
    &["autovacuum_vacuum_threshold", "autovacuum_vacuum_scale_factor", "autovacuum_vacuum_max_threshold"],
    &["autovacuum_vacuum_insert_threshold", "autovacuum_vacuum_insert_scale_factor"],
    &["autovacuum_analyze_threshold", "autovacuum_analyze_scale_factor"],
    &["autovacuum_freeze_max_age", "autovacuum_freeze_min_age", "autovacuum_freeze_table_age"],
    &["autovacuum_multixact_freeze_max_age", "autovacuum_multixact_freeze_min_age",
      "autovacuum_multixact_freeze_table_age"],
    &["autovacuum_vacuum_cost_delay", "autovacuum_vacuum_cost_limit", "log_autovacuum_min_duration"],
];

/// Server-wide switches with no per-table storage parameter.
const CLUSTER_ONLY: &[&str] = &["autovacuum", "track_counts"];

fn setting_groups(k: &Knobs) -> Vec<Vec<SettingRow>> {
    let mut groups: Vec<Vec<SettingRow>> = GROUPS.iter()
        .map(|names| names.iter().filter_map(|n| k.get(n)).map(|e| setting_row(k, e)).collect::<Vec<_>>())
        .filter(|g| !g.is_empty())
        .collect();
    let extras: Vec<SettingRow> = k.order.iter()
        .filter(|e| !GROUPS.iter().any(|g| g.contains(&e.setting.as_str())))
        .map(|e| setting_row(k, e))
        .collect();
    if !extras.is_empty() { groups.push(extras); }
    groups
}

fn setting_row(k: &Knobs, e: &AutovacEntry) -> SettingRow {
    let name = e.setting.as_str();
    let raw = e.effective_value.trim();
    let num: Option<f64> = raw.parse().ok();
    let mut source = e.source;
    let mut note: Option<String> = None;

    let value = if let Some(b) = parse_bool(raw).filter(|_| num.is_none() || is_flag(name)) {
        if b { "on".into() } else { "off".into() }
    } else if let Some(v) = num {
        match name {
            "autovacuum_vacuum_cost_delay" | "autovacuum_vacuum_cost_limit" if v < 0.0 => {
                let fallback_name = name.trim_start_matches("autovacuum_");
                note = Some(format!("-1 → {fallback_name}"));
                match e.fallback_value.as_deref() {
                    Some(fb) => fmt_setting_value(fb.trim().parse().unwrap_or(v), fb, e.unit.as_deref()),
                    None     => "-1".into(),
                }
            }
            "autovacuum_vacuum_insert_threshold" if v < 0.0 => "disabled".into(),
            "autovacuum_vacuum_max_threshold" if v < 0.0 => "no cap".into(),
            "log_autovacuum_min_duration" if v < 0.0 => "off".into(),
            "log_autovacuum_min_duration" if v == 0.0 => "every run".into(),
            _ if is_max_age(name) && source == AutovacSource::Reloption => {
                match k.cluster_num(name).filter(|cl| v > *cl) {
                    Some(cl) => {
                        note = Some(format!("table value {} ignored: larger than cluster", fmt_number(v, false)));
                        source = AutovacSource::Cluster;
                        fmt_setting_value(cl, &cl.to_string(), e.unit.as_deref())
                    }
                    None => fmt_setting_value(v, raw, e.unit.as_deref()),
                }
            }
            _ => {
                if let Some((limit, why)) = freeze_age_limit(k, name) {
                    if v > limit {
                        note = Some(format!("limited to {} ({why})", fmt_number(limit.floor(), false)));
                    }
                }
                fmt_setting_value(v, raw, e.unit.as_deref())
            }
        }
    } else if raw.is_empty() {
        "—".into()
    } else {
        raw.to_string()
    };

    let mut tooltip = String::from(name);
    tooltip.push_str(&format!("\nEffective: {value}"));
    if let Some(n) = &note { tooltip.push_str(&format!(" ({n})")); }
    if !CLUSTER_ONLY.contains(&name) {
        tooltip.push_str(&format!(
            "\nThis table: {}",
            e.reloption_value.as_deref().unwrap_or("not set"),
        ));
    }
    tooltip.push_str(&format!(
        "\nCluster: {}",
        e.cluster_value.as_deref().unwrap_or("no server setting (per-table only)"),
    ));
    tooltip.push_str(&format!("\n{}", source_explanation(source)));

    SettingRow { name: e.setting.clone(), value, source, note, tooltip }
}

fn is_flag(name: &str) -> bool {
    matches!(name, "autovacuum" | "track_counts" | "autovacuum_enabled")
}

/// VACUUM silently limits the min / table freeze ages to a fraction of
/// the cluster max age: min_age to half, table_age to 95%.
fn freeze_age_limit(k: &Knobs, name: &str) -> Option<(f64, &'static str)> {
    let (max_name, factor, why) = match name {
        "autovacuum_freeze_min_age" =>
            ("autovacuum_freeze_max_age", 0.5, "half of autovacuum_freeze_max_age"),
        "autovacuum_freeze_table_age" =>
            ("autovacuum_freeze_max_age", 0.95, "95% of autovacuum_freeze_max_age"),
        "autovacuum_multixact_freeze_min_age" =>
            ("autovacuum_multixact_freeze_max_age", 0.5, "half of autovacuum_multixact_freeze_max_age"),
        "autovacuum_multixact_freeze_table_age" =>
            ("autovacuum_multixact_freeze_max_age", 0.95, "95% of autovacuum_multixact_freeze_max_age"),
        _ => return None,
    };
    Some((k.cluster_num(max_name)? * factor, why))
}

/// Exact value with thousands separators and the server's unit. Time
/// settings (unit `ms`) read as `2 ms`, `10 s`, `10 min`.
fn fmt_setting_value(v: f64, raw: &str, unit: Option<&str>) -> String {
    match unit.map(str::trim).filter(|u| !u.is_empty()) {
        Some("ms") => format_duration_ms(v),
        Some("s")  => format_duration_ms(v * 1000.0),
        Some(u)    => format!("{} {u}", fmt_plain(v, raw)),
        None       => fmt_plain(v, raw),
    }
}

fn fmt_plain(v: f64, raw: &str) -> String {
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        format_int_with_commas_i64(v as i64)
    } else {
        raw.to_string()
    }
}

pub(crate) fn format_duration_ms(ms: f64) -> String {
    let whole = ms.fract() == 0.0;
    if whole && ms >= 60_000.0 && ms % 60_000.0 == 0.0 {
        format!("{} min", ms / 60_000.0)
    } else if whole && ms >= 1_000.0 && ms % 1_000.0 == 0.0 {
        format!("{} s", ms / 1_000.0)
    } else {
        format!("{} ms", fmt_number(ms, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(setting: &str, effective: &str, source: AutovacSource, cluster: Option<&str>) -> AutovacEntry {
        AutovacEntry {
            setting: setting.into(),
            effective_value: effective.into(),
            source,
            reloption_value: (source == AutovacSource::Reloption).then(|| effective.to_string()),
            cluster_value: cluster.map(String::from),
            fallback_value: None,
            unit: None,
        }
    }

    fn cluster(setting: &str, v: &str) -> AutovacEntry {
        entry(setting, v, AutovacSource::Default, Some(v))
    }

    /// PG 18 defaults, the shape the plane returns for an untuned table.
    fn defaults() -> Vec<AutovacEntry> {
        vec![
            cluster("autovacuum", "on"),
            cluster("track_counts", "on"),
            entry("autovacuum_enabled", "on", AutovacSource::Default, None),
            cluster("autovacuum_vacuum_threshold", "50"),
            cluster("autovacuum_vacuum_scale_factor", "0.2"),
            cluster("autovacuum_vacuum_max_threshold", "100000000"),
            cluster("autovacuum_vacuum_insert_threshold", "1000"),
            cluster("autovacuum_vacuum_insert_scale_factor", "0.2"),
            cluster("autovacuum_analyze_threshold", "50"),
            cluster("autovacuum_analyze_scale_factor", "0.1"),
            cluster("autovacuum_freeze_max_age", "200000000"),
            cluster("autovacuum_freeze_min_age", "50000000"),
            cluster("autovacuum_freeze_table_age", "150000000"),
            cluster("autovacuum_multixact_freeze_max_age", "400000000"),
            cluster("autovacuum_multixact_freeze_min_age", "5000000"),
            cluster("autovacuum_multixact_freeze_table_age", "150000000"),
            cluster("autovacuum_vacuum_cost_delay", "2"),
            cluster("autovacuum_vacuum_cost_limit", "-1"),
            cluster("log_autovacuum_min_duration", "600000"),
        ]
    }

    fn set(entries: &mut [AutovacEntry], name: &str, value: &str) {
        let e = entries.iter_mut().find(|e| e.setting == name).unwrap();
        e.effective_value = value.into();
        e.reloption_value = Some(value.into());
        e.source = AutovacSource::Reloption;
    }

    fn counters(reltuples: i64) -> AutovacCounters {
        AutovacCounters {
            relkind: "r".into(),
            reltuples: Some(reltuples),
            relpages: Some(1000),
            relallfrozen: None,
            n_dead_tup: Some(0),
            n_ins_since_vacuum: Some(0),
            n_mod_since_analyze: Some(0),
            xid_age: Some(0),
            mxid_age: Some(0),
        }
    }

    fn table(av: &AutovacSettings) -> (Option<String>, Vec<TriggerRow>, Vec<Vec<SettingRow>>) {
        match build(av) {
            AutovacView::Table { banner, triggers, settings } => (banner, triggers, settings),
            AutovacView::PartitionedParent => panic!("expected a table view"),
        }
    }

    fn trigger<'a>(rows: &'a [TriggerRow], label: &str) -> &'a TriggerRow {
        rows.iter().find(|t| t.label == label).unwrap()
    }

    fn setting<'a>(groups: &'a [Vec<SettingRow>], name: &str) -> &'a SettingRow {
        groups.iter().flatten().find(|s| s.name == name).unwrap()
    }

    fn rule_text(t: &TriggerRow) -> String {
        t.rule.iter().map(|s| s.text.as_str()).collect()
    }

    /// The screenshot table: 460M rows, several per-table overrides.
    #[test]
    fn vacuum_trigger_follows_threshold_plus_scale() {
        let mut e = defaults();
        set(&mut e, "autovacuum_vacuum_scale_factor", "0.02");
        let mut c = counters(460_000_000);
        c.n_dead_tup = Some(4_700);
        let (_, t, _) = table(&AutovacSettings { entries: e, counters: c });
        let v = trigger(&t, "Vacuum");
        assert_eq!(v.fires_at, Some(50.0 + 0.02 * 460_000_000.0));
        assert_eq!(rule_text(v), "50 + 0.02 × 460M rows");
        assert!(v.rule[2].overridden && !v.rule[0].overridden);
        assert!((v.progress.unwrap() - 0.051).abs() < 0.001);
        assert_eq!(v.severity, Severity::Good);
    }

    #[test]
    fn vacuum_trigger_is_capped_by_max_threshold() {
        let mut c = counters(6_000_000_000);
        c.n_dead_tup = Some(150_000_000);
        let (_, t, _) = table(&AutovacSettings { entries: defaults(), counters: c });
        let v = trigger(&t, "Vacuum");
        assert_eq!(v.fires_at, Some(100_000_000.0));
        assert_eq!(rule_text(v), "min(100M, 50 + 0.2 × 6.0G rows)");
        assert_eq!(v.severity, Severity::Watch);
    }

    #[test]
    fn max_threshold_minus_one_means_no_cap() {
        let mut e = defaults();
        set(&mut e, "autovacuum_vacuum_max_threshold", "-1");
        let (_, t, s) = table(&AutovacSettings { entries: e, counters: counters(6_000_000_000) });
        assert_eq!(trigger(&t, "Vacuum").fires_at, Some(50.0 + 0.2 * 6_000_000_000.0));
        assert_eq!(setting(&s, "autovacuum_vacuum_max_threshold").value, "no cap");
    }

    #[test]
    fn insert_trigger_uses_inserts_since_vacuum() {
        let mut e = defaults();
        set(&mut e, "autovacuum_vacuum_insert_threshold", "500000");
        set(&mut e, "autovacuum_vacuum_insert_scale_factor", "0");
        let mut c = counters(460_000_000);
        c.n_ins_since_vacuum = Some(125_000);
        let (_, t, _) = table(&AutovacSettings { entries: e, counters: c });
        let i = trigger(&t, "Insert");
        assert_eq!(i.fires_at, Some(500_000.0));
        assert_eq!(i.progress, Some(25.0));
        assert_eq!(rule_text(i), "500K + 0 × 460M rows");
    }

    #[test]
    fn insert_trigger_scales_by_unfrozen_fraction_on_pg18() {
        let mut c = counters(1_000_000);
        c.relpages = Some(1000);
        c.relallfrozen = Some(750);
        let (_, t, _) = table(&AutovacSettings { entries: defaults(), counters: c });
        let i = trigger(&t, "Insert");
        assert_eq!(i.fires_at, Some(1000.0 + 0.2 * 1_000_000.0 * 0.25));
        assert!(rule_text(i).ends_with("× 25% unfrozen"));
    }

    #[test]
    fn insert_threshold_minus_one_disables_the_trigger() {
        let mut e = defaults();
        set(&mut e, "autovacuum_vacuum_insert_threshold", "-1");
        let (_, t, s) = table(&AutovacSettings { entries: e, counters: counters(1000) });
        let i = trigger(&t, "Insert");
        assert_eq!(i.inactive, Some("disabled"));
        assert_eq!(i.progress, None);
        assert_eq!(setting(&s, "autovacuum_vacuum_insert_threshold").value, "disabled");
    }

    #[test]
    fn pg12_has_no_insert_trigger() {
        let e: Vec<_> = defaults().into_iter()
            .filter(|e| !e.setting.contains("insert") && !e.setting.contains("max_threshold"))
            .collect();
        let (_, t, _) = table(&AutovacSettings { entries: e, counters: counters(1000) });
        assert!(t.iter().all(|t| t.label != "Insert"));
    }

    #[test]
    fn never_analyzed_counts_as_zero_rows_but_says_so() {
        let mut c = counters(-1);
        c.n_dead_tup = Some(25);
        let (_, t, _) = table(&AutovacSettings { entries: defaults(), counters: c });
        let v = trigger(&t, "Vacuum");
        assert_eq!(v.fires_at, Some(50.0));
        assert_eq!(v.progress, Some(50.0));
        assert!(rule_text(v).ends_with("0 rows (never analyzed)"));
    }

    #[test]
    fn zero_threshold_crossed_reads_as_due() {
        let mut e = defaults();
        set(&mut e, "autovacuum_analyze_threshold", "0");
        set(&mut e, "autovacuum_analyze_scale_factor", "0");
        let mut c = counters(1000);
        c.n_mod_since_analyze = Some(3);
        let (_, t, _) = table(&AutovacSettings { entries: e, counters: c });
        let a = trigger(&t, "Analyze");
        assert!(a.progress.unwrap().is_infinite());
        assert_eq!(progress_text(a.progress.unwrap()), "due");
        assert_eq!(a.severity, Severity::Bad);
    }

    #[test]
    fn freeze_max_age_larger_than_cluster_is_ignored() {
        let mut e = defaults();
        set(&mut e, "autovacuum_freeze_max_age", "500000000");
        let mut c = counters(1000);
        c.xid_age = Some(100_000_000);
        let (_, t, s) = table(&AutovacSettings { entries: e, counters: c });
        let f = trigger(&t, "Freeze");
        assert_eq!(f.fires_at, Some(200_000_000.0));
        assert!(!f.rule[0].overridden);
        let row = setting(&s, "autovacuum_freeze_max_age");
        assert_eq!(row.value, "200,000,000");
        assert_eq!(row.source, AutovacSource::Cluster);
        assert!(row.note.as_deref().unwrap().contains("ignored"));
    }

    #[test]
    fn freeze_max_age_smaller_than_cluster_applies() {
        let mut e = defaults();
        set(&mut e, "autovacuum_freeze_max_age", "100000000");
        let mut c = counters(1000);
        c.xid_age = Some(41_000_000);
        let (_, t, _) = table(&AutovacSettings { entries: e, counters: c });
        let f = trigger(&t, "Freeze");
        assert_eq!(f.fires_at, Some(100_000_000.0));
        assert!(f.rule[0].overridden);
        assert_eq!(f.progress, Some(41.0));
    }

    #[test]
    fn freeze_min_age_over_half_max_gets_a_limit_note() {
        let mut e = defaults();
        set(&mut e, "autovacuum_freeze_min_age", "150000000");
        let (_, _, s) = table(&AutovacSettings { entries: e, counters: counters(1000) });
        let note = setting(&s, "autovacuum_freeze_min_age").note.clone().unwrap();
        assert_eq!(note, "limited to 100,000,000 (half of autovacuum_freeze_max_age)");
    }

    #[test]
    fn cost_limit_minus_one_shows_the_vacuum_cost_limit_fallback() {
        let mut e = defaults();
        e.iter_mut().find(|e| e.setting == "autovacuum_vacuum_cost_limit").unwrap()
            .fallback_value = Some("200".into());
        let (_, _, s) = table(&AutovacSettings { entries: e, counters: counters(1000) });
        let row = setting(&s, "autovacuum_vacuum_cost_limit");
        assert_eq!(row.value, "200");
        assert_eq!(row.note.as_deref(), Some("-1 → vacuum_cost_limit"));
    }

    #[test]
    fn time_settings_carry_units() {
        assert_eq!(format_duration_ms(2.0), "2 ms");
        assert_eq!(format_duration_ms(0.5), "0.5 ms");
        assert_eq!(format_duration_ms(10_000.0), "10 s");
        assert_eq!(format_duration_ms(600_000.0), "10 min");
        let mut e = defaults();
        e.iter_mut().find(|e| e.setting == "log_autovacuum_min_duration").unwrap().unit = Some("ms".into());
        let (_, _, s) = table(&AutovacSettings { entries: e, counters: counters(1000) });
        assert_eq!(setting(&s, "log_autovacuum_min_duration").value, "10 min");
    }

    #[test]
    fn disabled_table_mutes_activity_triggers_but_not_freeze() {
        let mut e = defaults();
        set(&mut e, "autovacuum_enabled", "false");
        let (banner, t, s) = table(&AutovacSettings { entries: e, counters: counters(1000) });
        assert!(banner.unwrap().contains("autovacuum_enabled = off"));
        assert_eq!(trigger(&t, "Vacuum").inactive, Some("off for this table"));
        assert_eq!(trigger(&t, "Analyze").inactive, Some("off for this table"));
        assert_eq!(trigger(&t, "Freeze").inactive, None);
        assert_eq!(setting(&s, "autovacuum_enabled").value, "off");
    }

    #[test]
    fn muted_rows_name_the_switch_that_stopped_them() {
        let mut e = defaults();
        e.iter_mut().find(|e| e.setting == "track_counts").unwrap().effective_value = "off".into();
        let (banner, t, _) = table(&AutovacSettings { entries: e, counters: counters(1000) });
        assert!(banner.unwrap().contains("track_counts is off"));
        assert_eq!(trigger(&t, "Vacuum").inactive, Some("track_counts off"));
        assert_eq!(trigger(&t, "MultiXact freeze").inactive, None);
    }

    #[test]
    fn toast_relations_have_no_analyze_trigger() {
        let mut c = counters(1000);
        c.relkind = "t".into();
        let (_, t, _) = table(&AutovacSettings { entries: defaults(), counters: c });
        assert!(t.iter().all(|t| t.label != "Analyze"));
    }

    #[test]
    fn partitioned_parent_gets_its_own_view() {
        let mut c = counters(1000);
        c.relkind = "p".into();
        assert!(matches!(
            build(&AutovacSettings { entries: defaults(), counters: c }),
            AutovacView::PartitionedParent,
        ));
    }

    #[test]
    fn settings_group_in_trigger_order_and_label_sources_plainly() {
        let mut e = defaults();
        set(&mut e, "autovacuum_vacuum_cost_limit", "10000");
        let (_, _, s) = table(&AutovacSettings { entries: e, counters: counters(1000) });
        assert_eq!(s.len(), GROUPS.len());
        assert_eq!(s[0][0].name, "autovacuum");
        let limit = setting(&s, "autovacuum_vacuum_cost_limit");
        assert_eq!((limit.value.as_str(), source_label(limit.source)), ("10,000", "table"));
        assert_eq!(source_label(setting(&s, "autovacuum_vacuum_threshold").source), "default");
    }
}
