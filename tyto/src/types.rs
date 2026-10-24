//! Frontend-shared types.
//!
//! Houses every struct / enum that more than one tyto module needs
//! at once — the `ShellTab` reactive struct (the unit of pane-level
//! state), `PaneTree` (the splittable workspace layout),
//! `TableDetail` (Schema Explorer per-table cache), `AdminRow`
//! (Admin pane row representation), and notification + dialog enums.
//!
//! Files that need only their own state should keep it local;
//! anything two modules look at lives here.

use leptos::*;
use shared::{DbType, FormatStyle};
use std::collections::HashMap;
use uuid::Uuid;
use wasm_bindgen::prelude::*;

// ── Shell history ─────────────────────────────────────────────────────────────

/// A `\gexec` whose generated statements are running. psql's
/// `PrintQueryResult` runs them before its own statement is done: then
/// its status prints, and the result variables are set from it — failed,
/// should one of them have failed (`ExecQueryAndProcessResults`). Held
/// until the queue is down to the statements that followed them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GexecPending {
    /// How many statements followed the generated ones in the queue.
    pub rest:      usize,
    /// Its own statement's status: a RETURNING's tag, the time.
    pub trailer:   Option<ShellEntry>,
    /// Its own statement's rows, for `:ROW_COUNT`.
    pub row_count: u64,
    /// A statement it generated failed.
    pub failed:    bool,
}

/// A statement as the user typed it: `sql` is what goes to the server;
/// `terminator` is what ended it — `;`, a `DELIMITER` string, MySQL's
/// `\G`, a psql `\g`-family line (`"\n\\gx"`), or empty when none
/// was typed. The shell shows and remembers both, the way psql and the
/// mysql CLI echo and recall a statement; the server only gets `sql`
/// (it can't parse a custom delimiter or `\G`). `recalled`: `sql` is
/// the previous query, not something typed — `\g` on an empty buffer.
/// `vertical` and `post`: what the terminator asks of the result —
/// shown vertically (`\gx`, `\G`), or handed to `\gexec`, `\gset` or
/// `\crosstabview`; each statement of an input keeps its own.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TypedStatement {
    pub sql:        String,
    pub terminator: String,
    pub recalled:   bool,
    pub vertical:   bool,
    pub post:       Option<PostAction>,
    /// What the CLI said reading its lines (`DbmsPlane::line_notes`:
    /// mysql's `Unknown command '\i'.`), shown when it's sent — after
    /// what the statements before it printed, as the CLI says each line's
    /// as it reads it.
    pub notes:      Vec<ShellEntry>,
}

impl TypedStatement {
    pub(crate) fn new(sql: impl Into<String>, terminator: impl Into<String>) -> Self {
        TypedStatement { sql: sql.into(), terminator: terminator.into(), recalled: false, vertical: false, post: None, notes: Vec::new() }
    }

    /// Its result shown vertically, or handed to `post`.
    pub(crate) fn shown(self, vertical: bool, post: Option<PostAction>) -> Self {
        TypedStatement { vertical, post, ..self }
    }

    /// The previous query, run again by the `terminator` line alone.
    pub(crate) fn recalled(sql: impl Into<String>, terminator: impl Into<String>) -> Self {
        TypedStatement { recalled: true, ..TypedStatement::new(sql, terminator) }
    }

    /// A terminator that ends nothing — `;` alone, the second `;` of
    /// `select 1;;`, mysql's `\G;` — which the mysql CLI answers itself
    /// (`DbmsPlane::empty_statement`, `shell::cli_answer`) and psql sends.
    /// A query `\gexec` generated has no terminator: it goes to the
    /// server even empty, as psql's `ExecQueryTuples` sends it.
    pub(crate) fn ends_nothing(&self) -> bool {
        self.sql.is_empty() && !self.terminator.is_empty()
    }

    /// The statement as typed — shown in the scrollback, kept in
    /// history, copied by "copy block".
    pub(crate) fn as_typed(&self) -> String {
        format!("{}{}", self.sql, self.terminator)
    }

    /// What the persisted history keeps: the text the user typed. For a
    /// recalled statement that's only the `\g` line, as in psql — the
    /// previous query has its `:var`s substituted, and a variable may
    /// hold a secret.
    pub(crate) fn history_text(&self) -> String {
        if self.recalled { self.terminator.trim_start().to_string() } else { self.as_typed() }
    }
}

/// Statements that run together (a multi-statement Enter), substituted
/// once: `run` has each one's `:var`s expanded — what safe-mode
/// inspects, the confirm dialog lists, the scrollback shows and the
/// server gets, so `\set` can't change a batch while it awaits
/// confirmation — and `typed` keeps each as typed, for the persisted
/// history (a variable may hold a secret).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StatementBatch {
    pub typed: Vec<TypedStatement>,
    pub run:   Vec<TypedStatement>,
}

impl StatementBatch {
    /// `interpolate`: what the shell's CLI does to typed SQL
    /// ([`ShellTab::interpolate`]).
    pub(crate) fn substitute(typed: Vec<TypedStatement>, interpolate: impl Fn(&str) -> String) -> Self {
        let run = typed.iter()
            .map(|st| TypedStatement { sql: interpolate(&st.sql), ..st.clone() })
            .collect();
        StatementBatch { typed, run }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub(crate) enum ShellEntry {
    /// `sql` was sent; `terminator` ended it as typed (see
    /// [`TypedStatement`]). The block shows both. `meta`: a
    /// meta-command as typed (`\d film`, `\x`) — shown and recallable
    /// like a statement, but never "the previous query" that `\g`,
    /// `\watch` or `\e` reach back for.
    Query { num: u32, sql: String, terminator: String, meta: bool, errored: bool },
    /// Rows in the shell's grid, SQL NULL as `None`. `aligns` lines each column up as the CLI
    /// does (a missing one is left); `style` is what it printed with.
    /// `warnings`: the server's warning count (MySQL), which the CLI
    /// prints with the row count (`DbmsPlane::rows_footer`). `timed`: the
    /// time is shown — fixed when the result arrives, as the CLI prints
    /// it once (psql's `\timing`); a later toggle leaves it be.
    Table { columns: Vec<String>, aligns: Vec<Align>, rows: Vec<Vec<Option<String>>>, exec_time_us: u64, rows_affected: u64, warnings: u16, timed: bool, style: PrintedWith },
    /// Rows as the CLI prints them vertically (`DbmsPlane::print_vertical`),
    /// each column's `aligns` kept for psql's `\pset numericlocale`.
    Vertical { columns: Vec<String>, aligns: Vec<Align>, rows: Vec<Vec<Option<String>>>, exec_time_us: u64, warnings: u16, timed: bool, style: PrintedWith },
    /// A statement that returned no rows: what its CLI said about it,
    /// worded when it arrived (`DbmsPlane::command_status` — nothing
    /// under psql's `QUIET`), and its time when `timed`.
    Info { exec_time_us: u64, said: Vec<String>, timed: bool },
    Error(String),
    /// A server notice raised while a query ran (`RAISE NOTICE`,
    /// `VACUUM VERBOSE` progress, "…does not exist, skipping"). Arrives
    /// before the query's result; rendered psql-style as
    /// `SEVERITY:  message`.
    Notice { severity: String, message: String },
    MetaOutput(Vec<String>),
    KeyValue(Vec<(String, String)>),
    /// An error as the native CLI prints it, line by line — the
    /// server's, formatted by the plane (`DbmsPlane::format_error`), or
    /// one the CLI raises itself (psql's `"s.v" is not a view`) — and
    /// its message.
    CliError { lines: Vec<String>, message: String },
    /// Text a meta-command composed the way its native CLI prints it
    /// (psql's `\d <table>`), line by line, with spans that open the
    /// Schema Explorer.
    Printed { lines: Vec<String>, links: Vec<PrintedLink> },
}

/// What a result printed with: psql's `\pset null`, `\t`, `\pset footer`,
/// the title over it (`\pset title`, or a list's own, which stands in
/// for it as psql's does), a list's own footers, the lines its expanded
/// output is drawn with (`\pset border`, `linestyle`) and, under `\pset
/// numericlocale`, the client's locale its numbers are written in. A
/// result keeps them, as psql's printed output does — a later `\pset`
/// changes what prints next, not what has; shown and copied alike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrintedWith {
    pub null_display:   String,
    pub tuples_only:    bool,
    pub footer:         bool,
    pub title:          Option<String>,
    pub rules:          Rules,
    pub numeric_locale: Option<shared::NumericLocale>,
    /// The width it printed to — `\pset columns`, or the pane's —
    /// where its record lines need it (`xheader_width page`).
    pub columns:        Option<usize>,
    /// psql's `myopt.footers`: lines a list prints under its rows in
    /// place of the row count (`\dn`'s `Publications:`) — under `\pset
    /// footer off` too, as `footers_with_default` reads only the count
    /// from it; never under `\t`.
    pub footers:        Vec<String>,
}

impl Default for PrintedWith {
    /// The shell's defaults (its null display reads `NULL`).
    fn default() -> Self {
        PrintedWith {
            null_display: "NULL".into(), tuples_only: false, footer: true, title: None, rules: Rules::default(),
            numeric_locale: None, columns: None, footers: Vec::new(),
        }
    }
}

impl PrintedWith {
    /// A cell as it shows in a column lined up `align`: SQL NULL as the
    /// null display; a number (right-aligned) in the client's locale
    /// when the result printed under psql's `\pset numericlocale`
    /// (`DbmsPlane::localized_number`).
    pub(crate) fn cell(&self, plane: &dyn crate::plane::DbmsPlane, value: &Option<String>, align: Align) -> String {
        match (value, &self.numeric_locale) {
            (None, _) => self.null_display.clone(),
            (Some(value), Some(locale)) if align == Align::Right => plane.localized_number(value, locale),
            (Some(value), _) => value.clone(),
        }
    }

    /// A row's cells as they show, each column lined up as `aligns` says
    /// (a missing one left).
    pub(crate) fn row(&self, plane: &dyn crate::plane::DbmsPlane, row: &[Option<String>], aligns: &[Align]) -> Vec<String> {
        row.iter().enumerate()
            .map(|(c, value)| self.cell(plane, value, aligns.get(c).copied().unwrap_or(Align::Left)))
            .collect()
    }

    /// The title over the result — none under `\t`, as psql's printer
    /// leaves it out there.
    pub(crate) fn title(&self) -> Option<&str> {
        self.title.as_deref().filter(|_| !self.tuples_only)
    }

    /// Whether the row count under the rows shows: not under `\t` or
    /// `\pset footer off`, nor under a list's own footers.
    pub(crate) fn shows_footer(&self) -> bool {
        !self.tuples_only && self.footer && self.footers.is_empty()
    }

    /// A list's own footers as they show: none under `\t`.
    pub(crate) fn footers(&self) -> &[String] {
        if self.tuples_only { &[] } else { &self.footers }
    }
}

/// The last error a shell's command got, for psql's `\errverbose`: the
/// server's fields and the statement its position counts into.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LastError {
    pub error: shared::DbError,
    pub query: Option<String>,
}

/// A staged meta-command in flight: what was typed, the shell state it
/// was typed in, and every batch's result so far. `nonce` tags its
/// batches, so a reply that lands after the shell moved on (another
/// command, a reconnect) is dropped.
#[derive(Clone, Debug)]
pub(crate) struct PendingMeta {
    pub nonce:   u32,
    pub command: String,
    pub ctx:     crate::plane::MetaContext,
    pub replies: Vec<shared::BatchResult>,
    /// The batch last sent — an error's position counts into its query
    /// that failed.
    pub queries: Vec<String>,
    /// Whether that was a fast-path call (`MetaStep::Call`): its failure
    /// isn't kept for `\errverbose`, as psql's `PQfn` makes no result.
    pub fast_path: bool,
}

/// A span of a [`ShellEntry::Printed`] line — `lines[line][start..end]`,
/// byte offsets — that opens the Schema Explorer on a relation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PrintedLink {
    pub line:  usize,
    pub start: usize,
    pub end:   usize,
    pub open:  ExplorerInit,
}

// ── Schema explorer state types ───────────────────────────────────────────────

#[derive(Clone)]
pub(crate) enum LoadState<T: Clone> {
    NotLoaded,
    Loading,
    Loaded(T),
    Error(String),
}

// ── WebSocket lifecycle ───────────────────────────────────────────────────────

/// Connection state of the single global WebSocket between tyto and noctua.
/// Surfaced as a top-of-app banner when not `Connected` so the user sees a
/// silent backend death rather than a frozen UI. `Disconnected` means the
/// browser observed a close/error after a successful open; `Connecting` is
/// the cold-start state before the first onopen fires.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WsStatus {
    Connecting,
    Connected,
    Disconnected,
}

/// Object kinds the explorer can browse. Order matches the segmented strip
/// the MySQL plane exposes; Postgres plane drops `Events`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum ObjectKind { Tables, Views, Procedures, Functions, Triggers, Events }

impl ObjectKind {
    pub(crate) const ALL: [ObjectKind; 6] = [
        ObjectKind::Tables, ObjectKind::Views, ObjectKind::Procedures,
        ObjectKind::Functions, ObjectKind::Triggers, ObjectKind::Events,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            ObjectKind::Tables     => "Tables",
            ObjectKind::Views      => "Views",
            ObjectKind::Procedures => "Procedures",
            ObjectKind::Functions  => "Functions",
            ObjectKind::Triggers   => "Triggers",
            ObjectKind::Events     => "Events",
        }
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            ObjectKind::Tables     => "▦",
            ObjectKind::Views      => "◫",
            ObjectKind::Procedures => "⚙",
            ObjectKind::Functions  => "ƒ",
            ObjectKind::Triggers   => "⚡",
            ObjectKind::Events     => "◴",
        }
    }

    /// Headers for the flat list, excluding the always-present Name column
    /// and the conditional Schema column.
    pub(crate) fn extra_columns(self) -> &'static [&'static str] {
        match self {
            ObjectKind::Tables     => &["Engine", "Rows", "Updated", "Comment"],
            ObjectKind::Views      => &["Definer", "Updatable", "Security"],
            ObjectKind::Procedures => &["Definer", "Created", "Modified"],
            ObjectKind::Functions  => &["Definer", "Returns", "Created"],
            ObjectKind::Triggers   => &["Table", "Event", "Timing", "Definer"],
            ObjectKind::Events     => &["Definer", "Status", "Type", "Interval"],
        }
    }
}

/// Admin areas surfaced under the Schema Explorer's Admin tab. Each area
/// has its own scope (Instance / Database / Schema) and may not be
/// supported by every plane — e.g. MySQL has no first-class Tablespaces
/// and merges database with schema. The `plane.admin_areas()` method
/// returns only those the plane actually supports.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum AdminArea {
    Roles,         // PG: roles (users + groups). MySQL: users.
    Tablespaces,   // PG only.
    Databases,     // List of databases on the instance.
    Schemas,       // PG only. Database-scoped namespaces with USAGE/CREATE grants.
    Extensions,    // PG only.
    ServerConfig,  // postgresql.conf / my.cnf settings (read-only first).
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum AdminScope { Instance, Database, Schema }

impl AdminArea {
    pub(crate) fn label(self) -> &'static str {
        match self {
            AdminArea::Roles        => "Roles",
            AdminArea::Tablespaces  => "Tablespaces",
            AdminArea::Databases    => "Databases",
            AdminArea::Schemas      => "Schemas",
            AdminArea::Extensions   => "Extensions",
            AdminArea::ServerConfig => "Server Config",
        }
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            AdminArea::Roles        => "◉",
            AdminArea::Tablespaces  => "◐",
            AdminArea::Databases    => "▦",
            AdminArea::Schemas      => "⌬",
            AdminArea::Extensions   => "✦",
            AdminArea::ServerConfig => "⚙",
        }
    }

    pub(crate) fn scope(self) -> AdminScope {
        match self {
            AdminArea::Roles        => AdminScope::Instance,
            AdminArea::Tablespaces  => AdminScope::Instance,
            AdminArea::Databases    => AdminScope::Instance,
            AdminArea::Schemas      => AdminScope::Database,
            AdminArea::ServerConfig => AdminScope::Instance,
            AdminArea::Extensions   => AdminScope::Database,
        }
    }
}

impl AdminScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            AdminScope::Instance => "Instance",
            AdminScope::Database => "Database",
            AdminScope::Schema   => "Schema",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ExplorerColumn {
    pub(crate) name:        String,
    pub(crate) col_type:    String,
    pub(crate) nullable:    String,
    pub(crate) default_val: String,
    pub(crate) extra:       String,
    pub(crate) comment:     String,
    pub(crate) source:      String,
}

/// Open-state for the ERD's "+N more" overflow popover. Anchored to the
/// pill's viewport coordinates so the popover lands directly beneath the
/// pill (escapes the ERD's transformed/scaled stage). `hidden` carries
/// each clipped table's (schema, name, fk_count) — clicking a row
/// re-centers the inspector on that table via the same path the card-
/// name click takes.
#[derive(Clone)]
pub(crate) struct ErdOverflowPopover {
    pub(crate) x:      f64,
    pub(crate) y:      f64,
    pub(crate) hidden: Vec<(String, String, usize)>,
}

/// One row in the flat object list.
///
/// `parent_schema` / `parent_name` are `Some(...)` when this row is a PG
/// declarative partition child (`relispartition = true`); the outer list
/// uses them to render the row indented under its parent. `None` for
/// top-level tables and on MySQL (which doesn't expose partitions as
/// separate rows in `information_schema.tables`).
#[derive(Clone)]
pub(crate) struct ObjectRow {
    pub(crate) schema:        String,
    pub(crate) name:          String,
    pub(crate) parent_schema: Option<String>,
    pub(crate) parent_name:   Option<String>,
    pub(crate) extras:        Vec<String>,
}

/// One row in a flat Admin-area list (Roles, Tablespaces, Databases, etc.).
/// Identity is the `name` plus all `extras` — for MySQL roles the row's
/// "name" is the user account but the first extra is host, so each
/// (user, host) pair is its own visible row in keeping with Parliament's
/// flat-list design language. The renderer uses `plane.role_list_extra_columns()`
/// (or the analogous accessor per area) to label the extras.
#[derive(Clone)]
pub(crate) struct AdminRow {
    pub(crate) name:   String,
    pub(crate) extras: Vec<String>,
}

#[derive(Clone, Default, PartialEq)]
pub(crate) struct TableOverview {
    pub(crate) engine:          String,
    pub(crate) collation:       String,
    pub(crate) row_count:       String,
    pub(crate) data_size:       String,
    pub(crate) index_size:      String,
    /// Actual page count, sourced directly from the plane's
    /// `Page_count` column (PG: `c.relpages`, MySQL: pages computed
    /// from `(DATA + IDX + DATA_FREE) / 16384`). Pre-formatted with
    /// thousand separators for display. Aligned with the storage
    /// factor's NUMERATOR — see also [`Self::data_pages_ideal`].
    pub(crate) data_pages:       String,
    /// Theoretical-minimum page count derived from the same widths
    /// the storage factor uses (PG: `pg_stats` widths × live rows;
    /// MySQL: `(DATA + IDX) / 16384`). Empty when the plane couldn't
    /// estimate (partitioned parent, never-analyzed PG table,
    /// non-InnoDB MySQL engine). When present, the Overview cell
    /// renders as `~actual / ~ideal`; when empty, as `~actual` alone.
    pub(crate) data_pages_ideal: String,
    pub(crate) avg_row_len:     String,
    pub(crate) auto_inc:        String,
    pub(crate) create_time:     String,
    pub(crate) update_time:     String,
    pub(crate) comment:         String,
    pub(crate) partition_count: String,
    /// Set when THIS table is itself a partition — the parent's table name.
    ///
    /// `None` means the plane didn't emit a `Parent_table` column at all
    /// (MySQL — partition lineage is modeled differently and isn't
    /// surfaced as a parent reference in `table_status_sql`). `Some("")`
    /// means the plane emitted the column but THIS table isn't a
    /// partition. `Some("orders")` means THIS table is a partition of
    /// the named parent. The renderer hides the row for both `None`
    /// and `Some("")`; the type-level distinction is preserved so a
    /// future UX layer can choose to surface "(not applicable on
    /// MySQL)" explicitly. See issue #21 item 3 / audit cluster D
    /// finding 3.3 for the architectural rationale.
    pub(crate) parent_table:    Option<String>,
    /// Schema of the partitioned parent. Usually equals THIS table's
    /// schema — partitions in another schema are legal but unusual.
    /// `None` / `Some("")` / `Some(name)` semantics match
    /// `parent_table`.
    pub(crate) parent_schema:   Option<String>,
    /// The partition rule (`FOR VALUES FROM (...) TO (...)`) for THIS
    /// table when it is a partition. PG-only — MySQL's partition rule
    /// has different syntax (`PARTITION BY RANGE` etc.) and isn't
    /// surfaced here. `None` / `Some("")` / `Some(rule)` semantics
    /// match `parent_table`.
    pub(crate) partition_bound: Option<String>,
    /// Object owner. Postgres: `pg_get_userbyid(c.relowner)`. MySQL has
    /// no table-ownership concept.
    ///
    /// `None` = plane doesn't emit Owner (MySQL — concept not
    /// applicable). `Some("")` = plane emits it but the row's owner
    /// resolved to empty (rare; allowed). `Some(name)` = render the
    /// name.
    pub(crate) owner:           Option<String>,
    /// Storage factor: ratio of on-disk pages to a theoretical-minimum
    /// page count for the live row population. Formatted for display
    /// as e.g. `"1.8×"`. Captures dead tuples + intra-page free space
    /// + fillfactor + alignment overhead in one DBA-actionable number.
    ///
    /// PG: estimated from `pg_stats.avg_width` widths + tuple header /
    /// null-bitmap overhead vs `c.relpages` (canonical PG-wiki bloat
    /// estimate). MySQL: `(data+idx+data_free)/(data+idx)` from
    /// `information_schema.TABLES`. None when the plane couldn't emit
    /// a value (table never analyzed, empty table, partitioned parent
    /// on PG, non-InnoDB engine on MySQL).
    pub(crate) storage_factor:   Option<String>,
    /// Severity band for the storage factor — drives the colored
    /// `.metric-*` CSS class on the rendered cell. None when
    /// `storage_factor` is None.
    pub(crate) storage_severity: Option<Severity>,
    /// Tooltip text describing the storage-factor breakdown. None
    /// when `storage_factor` is None.
    pub(crate) storage_tooltip:  Option<String>,
    /// Latest of `last_vacuum` / `last_autovacuum` from
    /// `pg_stat_user_tables`. PG-only — MySQL InnoDB has no
    /// autovacuum metadata. `None` on MySQL or when no vacuum has
    /// run; `Some(timestamp)` for the most-recent vacuum.
    pub(crate) last_vacuum:        Option<String>,
    /// Buffer cache hit ratio from `pg_statio_user_tables`
    /// (heap_blks_hit / (heap_blks_hit + heap_blks_read)). Formatted
    /// for display as a one-decimal percentage like `"99.7%"`.
    /// `None` when no blocks have been accessed yet (fresh table)
    /// or on MySQL (no clean per-table equivalent in core InnoDB).
    pub(crate) cache_hit_ratio:    Option<String>,
    /// Severity band for [`Self::cache_hit_ratio`]. `None` when
    /// the ratio is `None`.
    pub(crate) cache_hit_severity: Option<Severity>,
    /// Hover tooltip text for the cache-hit cell. `None` when
    /// the ratio is `None`.
    pub(crate) cache_hit_tooltip:  Option<String>,
    /// Block IO display — condensed-magnitude summary of buffer
    /// cache hits vs disk reads across heap + index blocks from
    /// `pg_statio_user_tables`. Format: `"1.2G hit / 4.5M read"`.
    /// Answers "scale" while [`Self::cache_hit_ratio`] answers
    /// "efficiency"; the pair reads as one thought. Suppressed
    /// when both totals are 0 (fresh table or
    /// post-`pg_stat_reset()`). PG-only.
    pub(crate) block_io_display:   Option<String>,
    /// Hover tooltip for the Block IO cell — breaks out heap vs
    /// index hits / reads separately and reminds the reader that
    /// read = fetched from OS/disk, hit = served from
    /// shared_buffers. `None` when the display is `None`.
    pub(crate) block_io_tooltip:   Option<String>,
    /// Latest of `last_analyze` / `last_autoanalyze` from
    /// `pg_stat_user_tables`. PG-only — MySQL has no ANALYZE
    /// metadata. `None` on MySQL or when ANALYZE has never run.
    pub(crate) last_analyze:       Option<String>,
    /// Dead-tuple percentage — `n_dead_tup / (n_live_tup +
    /// n_dead_tup)` × 100 from `pg_stat_user_tables`. Forward-
    /// looking companion to [`Self::last_vacuum`]: answers "is
    /// vacuum keeping up?" PG's default `autovacuum_vacuum_scale_
    /// factor` is 0.2, so the severity bands mirror it — ≤5%
    /// green, 5-20% yellow, >20% red. `None` on MySQL or when
    /// the table has no rows (nothing to be dead about).
    pub(crate) dead_tup_pct:       Option<String>,
    /// Severity band for [`Self::dead_tup_pct`].
    pub(crate) dead_tup_severity:  Option<Severity>,
    /// Hover tooltip for the dead-tuples cell — breaks down raw
    /// live / dead counts and reminds the reader of PG's default
    /// autovacuum trigger.
    pub(crate) dead_tup_tooltip:   Option<String>,
    /// Freeze-age percentage — `age(relfrozenxid) * 100 /
    /// autovacuum_freeze_max_age`. Answers "how close is this table
    /// to triggering an anti-wraparound vacuum?" Pure relation-level
    /// catalog math (no page scans), safe on TB-sized tables.
    /// Formatted as `"68%"`. `None` when the table has no
    /// relfrozenxid (temp / unlogged / brand-new) or on MySQL.
    pub(crate) freeze_age_pct:      Option<String>,
    /// Severity band for [`Self::freeze_age_pct`].
    /// <50% Good / 50-100% Watch / >100% Bad.
    pub(crate) freeze_age_severity: Option<Severity>,
    /// Hover tooltip for the freeze-age cell — raw age magnitude,
    /// the GUC being compared against, and the "check for a long-
    /// running txn" hint when the value is >100%.
    pub(crate) freeze_age_tooltip:  Option<String>,
    /// Percentage of index scans vs sequential scans since the
    /// last stats reset — `idx_scan / (idx_scan + seq_scan)`.
    /// Formatted for display as `"95.3% indexed"`. `None` when
    /// the table has never been accessed (both counters 0) or on
    /// MySQL (no per-table read stats).
    pub(crate) idx_scan_pct:       Option<String>,
    /// Severity band for [`Self::idx_scan_pct`]. `None` when the
    /// percentage is `None`.
    pub(crate) idx_scan_severity:  Option<Severity>,
    /// Hover tooltip text for the idx-scans cell — breaks down
    /// raw idx vs seq counts and hints at the "missing indexes"
    /// remediation when the percentage is low.
    pub(crate) idx_scan_tooltip:   Option<String>,
    /// Lifetime read volume — `seq_tup_read + idx_tup_fetch` from
    /// CRUD volume display — thousands-comma-formatted
    /// `"C / R / U / D"` sourced from `pg_stat_user_tables`:
    /// C = `n_tup_ins`, R = `seq_tup_read + idx_tup_fetch`,
    /// U = `n_tup_upd`, D = `n_tup_del`. R is tuple volume, NOT
    /// query count — a `SELECT * FROM foo` returns N tuples in
    /// one call. Grows monotonically until `pg_stat_reset()`.
    /// Suppressed when all four counters are 0 (fresh table or
    /// just-reset stats). `None` on MySQL.
    pub(crate) crud_display:       Option<String>,
    /// Hover tooltip for the CRUD cell — spells out each letter
    /// and reminds the reader that R is tuples-returned, not
    /// query count. `None` when [`Self::crud_display`] is `None`.
    pub(crate) crud_tooltip:       Option<String>,
    /// TOAST storage footprint + fraction of row-storage — a
    /// pre-composed `"1.8 GB (42%)"` string that the parser
    /// stitches from the plane's `Toast_size` (bytes) and
    /// `Toast_pct`. Answers "where does this table's row-storage
    /// live" — invisible until a bytea/text-heavy table needs it.
    /// PG-only; `None` on MySQL (no TOAST concept), on tables
    /// with no TOAST relation, on tables whose TOAST is empty,
    /// and on partitioned parents (which have `reltoastrelid = 0`
    /// since they own no data).
    pub(crate) toast_display:      Option<String>,
}

/// Severity band for a metric — drives the `.metric-good` /
/// `.metric-watch` / `.metric-bad` CSS class on the rendered cell.
/// Set by [`crate::main`]'s status:* parser via
/// [`crate::storage_factor::classify`]. `Neutral` renders the cell
/// as a plain tooltip-only text — used when the value carries no
/// direction (e.g. CRUD volume, where more/less is table-purpose-
/// dependent, not good-vs-bad).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Severity { Good, Watch, Bad, Neutral }

#[derive(Clone)]
pub(crate) struct IndexInfo {
    pub(crate) name:       String,
    pub(crate) unique:     bool,
    pub(crate) index_type: String,
    pub(crate) size:       String,
    /// Theoretical-minimum on-disk size if the index were perfectly
    /// packed (same per-row math as the storage factor's denominator).
    /// Empty when the plane couldn't estimate (PG non-btree, MySQL,
    /// never-analyzed tables). When non-empty the renderer pairs it
    /// with `size` as `Size / Ideal: <actual> / <ideal>`.
    pub(crate) size_ideal: String,
    pub(crate) columns:    Vec<String>,
    /// True when the index was propagated from a partitioned-parent index
    /// (PG declarative partitioning). MySQL has no equivalent and always
    /// reports false. Renderer appends " (Inherited)" to the Kind column
    /// so DBAs can spot parent-driven indexes alongside partition-local
    /// ones.
    pub(crate) inherited:  bool,
    /// Per-index storage factor: ratio of on-disk pages to the
    /// theoretical-minimum page count for the live row population.
    /// Same multiplier framing as the table-level `storage_factor` on
    /// [`TableOverview`]. PG-btree-only — non-btree index types and
    /// MySQL InnoDB indexes emit empty. Formatted for display as e.g.
    /// `"1.4×"`; the renderer attaches the colored `.metric-*` class
    /// based on the paired severity below.
    pub(crate) storage_factor:   Option<String>,
    /// Severity band for [`Self::storage_factor`]. None when factor
    /// is None.
    pub(crate) storage_severity: Option<Severity>,
    /// Hover tooltip text for the per-index Storage cell. None when
    /// factor is None.
    pub(crate) storage_tooltip:  Option<String>,
    /// Number of times this index has been scanned since the last
    /// stat reset (PG `pg_stat_user_indexes.idx_scan`), pre-formatted
    /// with thousand separators (e.g. `"1,234"`). Empty when the
    /// plane doesn't emit it (MySQL — no equivalent without
    /// an extension).
    pub(crate) scan_count:       String,
    /// Severity band for [`Self::scan_count`]. Binary: `Some(Bad)`
    /// when the count is 0 (drop candidate; the renderer colours
    /// the cell red), `None` otherwise. Non-zero counts get no
    /// special colouring — "scanned at least once" isn't
    /// actionable; "never scanned" is the signal.
    pub(crate) scan_severity:    Option<Severity>,
}

/// One row of the table-relationships graph. `depth` is 1 (immediate
/// parent/child) or 2 (grand-parent/grand-child). `schema` is the
/// schema of the related table — render qualified when it differs
/// from the current detail's schema.
///
/// `from_columns` / `to_columns` are the ordered FK column pairs for
/// THIS edge — `from` is the dependent ("many") side, `to` is the
/// referenced ("one") side. The ERD tab uses these to anchor crow's-foot
/// endpoints to the correct rows on each card. Empty when the underlying
/// SQL didn't supply them (older test fixtures).
///
/// `via_schema` / `via_table` are populated for depth=2 rows only, naming
/// the bridging table the depth-2 hop goes through. Empty for depth=1.
/// The ERD uses this to route the line through the correct depth-1 card.
#[derive(Clone)]
pub(crate) struct RelEdge {
    pub(crate) depth:        u8,
    pub(crate) schema:       String,
    pub(crate) table:        String,
    pub(crate) from_columns: Vec<String>,
    pub(crate) to_columns:   Vec<String>,
    pub(crate) via_schema:   String,
    pub(crate) via_table:    String,
}

/// One direct partition of a partitioned table. `bound` is the
/// dialect-specific bound expression (e.g. PG's `FOR VALUES FROM
/// ('2022-01-01') TO ('2022-02-01')`). `rows` and `data_size` are
/// already display-formatted (commas / human-readable bytes) — kept
/// as strings so the renderer doesn't need to know about formatting.
/// Empty when the plane doesn't supply them (older test SQL etc.).
#[derive(Clone)]
pub(crate) struct PartitionInfo {
    pub(crate) schema:    String,
    pub(crate) name:      String,
    pub(crate) bound:     String,
    pub(crate) rows:      String,
    pub(crate) data_size: String,
}

/// TOAST section payload — three layouts depending on what kind of
/// table we're looking at. See [`ToastKind`] for the discriminator.
/// `columns` lists the TOASTABLE (non-PLAIN storage) columns; empty
/// for `IsToastOf` since a TOAST relation has internal columns only.
#[derive(Clone)]
pub(crate) struct ToastDetail {
    pub(crate) kind:    ToastKind,
    pub(crate) columns: Vec<ToastColumn>,
}

#[derive(Clone)]
pub(crate) enum ToastKind {
    /// Normal table with its own TOAST relation. The four size /
    /// count fields are pre-formatted for display (`human_bytes`,
    /// `format_int_condensed`, etc.).
    Single {
        toast_schema:  String,   // usually "pg_toast"
        toast_name:    String,   // e.g. "pg_toast_18432"
        heap_size:     String,   // "1.8 GB"
        index_size:    String,   // "64 MB"
        rows_est:      String,   // "2.4M"
        storage_ratio: String,   // "42%"
    },
    /// Partitioned parent — aggregate summary across leaves.
    /// The `largest_*` fields point at the leaf with the biggest
    /// TOAST footprint, clickable to drill in.
    PartitionAggregate {
        partition_count: usize,
        total_toast:     String, // "410 GB"
        largest_schema:  String, // usually "public"
        largest_name:    String, // e.g. "payment_p2022_02"
        largest_size:    String, // "38 GB"
    },
    /// This table IS a TOAST relation (schema = pg_toast). The
    /// section is replaced with a `TOAST OF <parent>` back-link at
    /// the top of the Schema tab.
    IsToastOf {
        parent_schema: String,
        parent_name:   String,
    },
}

#[derive(Clone)]
pub(crate) struct ToastColumn {
    pub(crate) name:    String,
    /// Storage class from `pg_attribute.attstorage`, already decoded
    /// server-side into `"EXTENDED"` / `"EXTERNAL"` / `"MAIN"`
    /// (PLAIN columns are filtered out at the plane).
    pub(crate) storage: String,
}

/// Visibility Map summary for a PG table, from the `vm:*` introspect.
/// The plane reads it live through the `pg_visibility` extension when
/// the session has it, else from `pg_class` (the snapshot the last
/// VACUUM / ANALYZE left). Partitioned parents are summed over their
/// leaf partitions.
///
/// Page counts are `Option<u64>`: `None` = the source doesn't report
/// it (all-frozen on `pg_class` before PG 18), `Some(0)` = reported as
/// zero. The render hides a `None` row and shows `Some(0)` as 0%.
#[derive(Clone)]
pub(crate) struct VmSummary {
    /// `"pg_visibility"` (live per-page read) or `"pg_class"`
    /// (snapshot). Anything else → the parser returns `None`.
    pub(crate) source:            String,
    pub(crate) total_pages:       Option<u64>,
    pub(crate) all_visible_pages: Option<u64>,
    pub(crate) all_frozen_pages:  Option<u64>,
    /// Leaf partitions summed, when the relation is a partitioned parent.
    pub(crate) partitions:        Option<u64>,
    /// Read from `pg_class` because the role was refused
    /// `pg_visibility_map_summary()`, not because the extension is missing.
    pub(crate) live_denied:       bool,
}

/// Where a resolved autovacuum setting value came from. The plane's
/// SQL populates the `Source` column with a string discriminator;
/// the parser maps it into this enum; `autovac::source_label` turns it
/// into the plain word the UI shows (table / cluster / default).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum AutovacSource {
    /// Value comes from `pg_class.reloptions` — table-specific override.
    Reloption,
    /// Value comes from a server setting changed from its built-in
    /// default (`pg_settings.source <> 'default'`).
    Cluster,
    /// Built-in default: a server setting left at its boot value, or a
    /// per-table-only knob (`autovacuum_enabled`) that isn't set.
    Default,
}

/// One autovacuum knob — its resolved value plus the raw inputs used
/// to derive it. All values arrive as text (the plane doesn't try to
/// parse int-vs-float-vs-bool); `autovac.rs` formats them.
#[derive(Clone, Debug)]
pub(crate) struct AutovacEntry {
    /// Reloption name (canonical form) — e.g. `autovacuum_vacuum_scale_factor`
    /// — or the GUC name for the server-wide switches (`autovacuum`,
    /// `track_counts`).
    pub(crate) setting:         String,
    /// The value that actually applies to this table, before the
    /// override / clamping rules `autovac.rs` applies.
    pub(crate) effective_value: String,
    pub(crate) source:          AutovacSource,
    /// Raw reloption text (empty if not overridden).
    pub(crate) reloption_value: Option<String>,
    /// The cluster GUC value (empty for reloption-only knobs).
    pub(crate) cluster_value:   Option<String>,
    /// The `vacuum_cost_*` GUC value a `-1` cost setting defers to.
    pub(crate) fallback_value:  Option<String>,
    /// `pg_settings.unit` — `"ms"` for the time-valued knobs.
    pub(crate) unit:            Option<String>,
}

/// Relation-level counters the trigger rows compare against — read in
/// the same `autovac:*` query as the knobs so both come from one
/// snapshot. `None` means the server didn't report the value (no
/// statistics yet, or a column the server version predates).
#[derive(Clone, Default, Debug)]
pub(crate) struct AutovacCounters {
    /// `pg_class.relkind`: `r` table, `p` partitioned parent, `t` TOAST,
    /// `m` materialized view.
    pub(crate) relkind:             String,
    /// `pg_class.reltuples`; `-1` means never vacuumed or analyzed.
    pub(crate) reltuples:           Option<i64>,
    pub(crate) relpages:            Option<i64>,
    /// `pg_class.relallfrozen` — PG 18+.
    pub(crate) relallfrozen:        Option<i64>,
    pub(crate) n_dead_tup:          Option<i64>,
    /// `pg_stat_all_tables.n_ins_since_vacuum` — PG 13+.
    pub(crate) n_ins_since_vacuum:  Option<i64>,
    pub(crate) n_mod_since_analyze: Option<i64>,
    /// `age(relfrozenxid)`.
    pub(crate) xid_age:             Option<i64>,
    /// `mxid_age(relminmxid)`.
    pub(crate) mxid_age:            Option<i64>,
}

/// Full autovacuum dump for a table — powers the Storage tab's
/// Autovacuum section. `entries` is one row per knob in the plane's
/// order. Parser returns `None` when the plane returned no knob rows
/// (relation not found, MySQL) so the render can suppress the section.
#[derive(Clone, Debug)]
pub(crate) struct AutovacSettings {
    pub(crate) entries:  Vec<AutovacEntry>,
    pub(crate) counters: AutovacCounters,
}

#[derive(Clone, Copy, PartialEq, Debug)]
/// Detail-inspector tabs. Order determines both the visual tab strip
/// (left-to-right) and the keyboard nav walk order (←/→). Storage sits
/// between Schema and ERD — Schema answers "what is this table logically
/// (columns / indexes)" while Storage answers "what is this table
/// physically (pages / bloat / VM)". Reading them adjacent makes the
/// logical-vs-physical distinction click.
pub(crate) enum DetailTab { Schema, Storage, Erd, Ddl, Data, Privileges }

/// One pivot-able row in the Object Privileges matrix. Rows are
/// streamed back from the plane's `object_grants_sql`; the explorer
/// pivots them into a (Grantee × Privilege) grid where each cell is
/// either blank (no access), a direct grant, or an inherited grant
/// gained through one or more role memberships.
#[derive(Clone, Debug)]
pub(crate) struct ObjectGrantRow {
    pub(crate) grantee:   String,
    pub(crate) privilege: String,
    /// Per-cell source. One of:
    ///   - `owner`     — the role owns the object (PG only).
    ///   - `superuser` — the role bypasses grants via rolsuper.
    ///   - `direct`    — explicit GRANT row in the ACL.
    ///   - `inherited` — gained via membership in a directly-granted role.
    ///   - `none`      — no access at all (placeholder for the
    ///     "show all roles" mode).
    pub(crate) source:    String,
    /// For inherited grants, the immediate granting role (the role
    /// in the membership chain that holds the actual GRANT). Empty
    /// for every other source.
    pub(crate) chain:     String,
    /// "YES" when the grant carries `WITH GRANT OPTION`. Empty
    /// otherwise.
    pub(crate) grantable: String,
    /// Per-role best-source classification (matches `source` for
    /// homogeneous-source rows, but stable across all of a role's
    /// cells). Drives the section grouping in the matrix UI: rows
    /// with the same `role_category` cluster together (owner first,
    /// then superuser, then direct, then inherited, then none).
    pub(crate) role_category: String,
}

/// One materialised page of a table's data: columns + rows as strings.
/// String values come from the same `mysql_value` / `pg_value`
/// formatters the shell uses (binaries as "<binary N>" on every
/// surface); an introspect result reads SQL NULL as the text "NULL".
#[derive(Clone)]
pub(crate) struct DataPage {
    pub(crate) columns: Vec<String>,
    pub(crate) rows:    Vec<Vec<String>>,
}

impl DataPage {
    /// Its rows with SQL NULL as `None`, for the exporters.
    pub(crate) fn cells(&self) -> Vec<Vec<Option<String>>> {
        self.rows.iter().map(|row| row.iter().map(|v| (v != "NULL").then(|| v.clone())).collect()).collect()
    }
}

/// How the Data tab body is laid out: Grid is the horizontal sortable
/// table; Vertical projects a single row as a key/value form (one
/// field per line) for inspecting wide columns or staging an INSERT
/// in a future phase. Toggled via the "≡ Vertical" toolbar button.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DataViewMode { Grid, Vertical, Insert }

/// Per-cell DML status for the Data tab's inline editor. Saving = the
/// UPDATE has been dispatched and we're waiting on the response;
/// Saved = success (briefly shown, then auto-cleared by a setTimeout);
/// Failed carries the error message until the user retries the cell.
#[derive(Clone)]
pub(crate) enum EditStatus { Saving, Saved, Failed(String) }

/// Each ERD neighbour's columns in "all fields" mode — (column name,
/// whether it's in the primary key) — by (schema, table).
pub(crate) type ErdNeighbourColumns = HashMap<(String, String), Vec<(String, bool)>>;

/// Per-(schema, table) detail state cached on the explorer pane.
#[derive(Clone, Copy)]
pub(crate) struct TableDetail {
    pub(crate) overview:    RwSignal<LoadState<TableOverview>>,
    pub(crate) columns:     RwSignal<LoadState<Vec<ExplorerColumn>>>,
    pub(crate) indexes:     RwSignal<LoadState<Vec<IndexInfo>>>,
    pub(crate) ddl:         RwSignal<LoadState<String>>,
    pub(crate) ancestors:   RwSignal<LoadState<Vec<RelEdge>>>,
    pub(crate) descendants: RwSignal<LoadState<Vec<RelEdge>>>,
    pub(crate) partitions:  RwSignal<LoadState<Vec<PartitionInfo>>>,
    /// TOAST detail — populated only when the table has TOAST (or is
    /// itself a TOAST relation, or is a partitioned parent whose
    /// leaves carry TOAST). Suppressed as a whole section otherwise.
    /// `Option` inside the `LoadState` models "loaded but this table
    /// has no TOAST" cleanly — a real load with an explicit no-signal
    /// verdict, distinct from "still loading."
    pub(crate) toast:       RwSignal<LoadState<Option<ToastDetail>>>,
    /// Visibility Map summary — populated on PG when `pg_visibility`
    /// contrib is installed or on PG 17+ via the `pg_stat_all_tables`
    /// fallback. `None` means the plane reported `Source = "none"`
    /// (neither path available); the render suppresses the section.
    pub(crate) vm:          RwSignal<LoadState<Option<VmSummary>>>,
    /// Autovacuum knob dump — populated by the `autovac:*` introspect.
    /// `None` when the plane returned zero rows (relation not found,
    /// non-PG connection). Section suppresses when None or Loading.
    pub(crate) autovac:     RwSignal<LoadState<Option<AutovacSettings>>>,
    /// True from a ↻ (or stale-triggered) refetch until the `status:*`
    /// reply lands. Values stay on screen meanwhile; the ↻ icon spins.
    pub(crate) refreshing:  RwSignal<bool>,
    /// Set when DDL on this connection may have changed the table while
    /// its detail wasn't on screen. The next open refetches in place.
    pub(crate) stale:       RwSignal<bool>,
    /// Autovacuum-section action panel state. Each checkbox is a
    /// bool; the composed SQL preview + Run button read all four.
    /// Reset when the TableDetail instance is dropped (per-table
    /// ephemeral). Freeze is a modifier on Vacuum, so the UI
    /// disables the Freeze checkbox when Vacuum is off.
    pub(crate) autovac_action_vacuum:  RwSignal<bool>,
    pub(crate) autovac_action_analyze: RwSignal<bool>,
    pub(crate) autovac_action_freeze:  RwSignal<bool>,
    pub(crate) autovac_action_verbose: RwSignal<bool>,
    /// Why Run couldn't start (the connection is gone, or the explorer
    /// has no session yet). Cleared when a checkbox changes.
    pub(crate) autovac_action_error:   RwSignal<Option<String>>,
    /// The last command Run sent to the explorer's action shell. The
    /// status line derives running / done / failed from that shell's
    /// scrollback, so it's right even if the Storage tab wasn't on
    /// screen when the command finished.
    pub(crate) autovac_run:            RwSignal<Option<ActionRun>>,
    pub(crate) active_tab:  RwSignal<DetailTab>,
    /// Schema-tab Relationships row: when at least one band has more than
    /// `RELATIONSHIPS_OVERFLOW_THRESHOLD` entries an Expand toggle appears
    /// in the section header. False = default clipped (`.rel-col` keeps
    /// its 160px max-height); true = grown (`.rel-col-expanded` lifts the
    /// cap to 480px). Per-table; ephemeral.
    pub(crate) relationships_expanded: RwSignal<bool>,
    /// ERD-tab zoom level (1.0 = 100%). Per-table so each diagram remembers
    /// its own zoom across tab switches. Clamped 0.25..=2.0 by the renderer.
    pub(crate) erd_zoom:    RwSignal<f64>,
    /// Maximum FK-relationship depth to display. 0 shows the target alone,
    /// 1 shows ±1 hop (parents + children), 2 shows ±2 hops (full diagram).
    /// Default is 2 — full topology. The renderer clamps the live value
    /// to the table's actual maximum depth, so a leaf table never offers
    /// "2 generations" as an option in the dropdown. Changing the value
    /// re-filters the already-loaded depth-1/depth-2 edges client-side,
    /// so no extra introspect round-trip is needed.
    pub(crate) erd_generations: RwSignal<u8>,
    /// When true, ERD cards show only PK and FK columns (default). When
    /// false, all columns of every visible card render with FK/PK rows
    /// tinted. Toggling false the first time triggers a single bulk-
    /// columns introspect query for every visible neighbour; the result
    /// is cached in `erd_neighbour_columns` so subsequent toggles are
    /// free.
    pub(crate) erd_keys_only: RwSignal<bool>,
    /// Maximum number of cards rendered in each generation band before the
    /// rest collapse behind a "+N more" overflow pill. `u8::MAX` means
    /// "no cap" — the band renders every related table (matches the
    /// pre-cap behaviour). Default is 10; toolbar dropdown lets the user
    /// pick 5 / 10 / 15 / All. Per-table so each diagram remembers its
    /// own choice across tab switches.
    pub(crate) erd_cards_per_band: RwSignal<u8>,
    /// When `Some`, the ERD's overflow popover is open: lists the tables
    /// hidden by the per-band cap so the user can drill into one. Set
    /// by clicking a "+N more" pill; cleared on backdrop click, Esc, or
    /// selecting a row.
    pub(crate) erd_overflow_popover: RwSignal<Option<ErdOverflowPopover>>,
    /// Cached column-name + is-pk lookup for the visible-neighbour set in
    /// "all fields" mode. Keyed by (schema, table). Loaded lazily when
    /// the user first turns "Keys Only" off; refetched only on DDL.
    pub(crate) erd_neighbour_columns: RwSignal<LoadState<ErdNeighbourColumns>>,

    // ── Privileges tab state ──────────────────────────────────────────
    /// Per-(grantee × privilege) effective access on this object,
    /// streamed back from `plane.object_grants_sql`. Pivoted client-
    /// side into the matrix view. Lazy-loaded when the Privileges tab
    /// is first activated, like the other detail tabs.
    pub(crate) object_grants: RwSignal<LoadState<Vec<ObjectGrantRow>>>,
    /// Toggle: include rows whose `source == "inherited"`. Default
    /// true so the user sees the full transitive picture (the whole
    /// motivating "why does this user have access?" question). Setting
    /// false collapses the matrix to direct grantees only.
    pub(crate) privileges_show_inherited: RwSignal<bool>,
    /// Toggle: include the "no grant" section — every role in the
    /// cluster that has no relationship at all to this object. Off
    /// by default because for a typical PG instance with all the
    /// `pg_*` built-in roles the section easily doubles the matrix
    /// height with all-blank rows; useful for the editable v8b
    /// matrix (need to see grant-able targets) but noisy for v8a's
    /// read-only "who has access" answer.
    pub(crate) privileges_show_no_grant: RwSignal<bool>,
    /// Case-insensitive substring filter on the grantee column.
    /// Empty string = no filter.
    pub(crate) privileges_filter: RwSignal<String>,

    // ── Data tab state ────────────────────────────────────────────────
    //
    // The Data tab paginates with a single-page-in-memory model: one
    // page lives in `data_page` at a time; navigating Prev/Next refetches
    // the new page and discards the previous. Sort + page-size + page-index
    // signals drive the SELECT; changing any of them refetches.
    /// 0-based page index. Multiplied by data_page_size for OFFSET.
    pub(crate) data_page_index: RwSignal<usize>,
    /// Rows per page. Default 200; user-configurable via toolbar in a
    /// later phase.
    pub(crate) data_page_size:  RwSignal<usize>,
    /// Column to ORDER BY. None = natural order (PG: physical, MySQL:
    /// PK if any). Setting requires the column be safe to sort by — see
    /// the gating logic in the explorer.
    pub(crate) data_sort_column: RwSignal<Option<String>>,
    /// True for DESC, false for ASC. Ignored when data_sort_column is None.
    pub(crate) data_sort_desc:   RwSignal<bool>,
    /// One page of fetched rows. Reset to NotLoaded on every page/sort
    /// change (which then triggers the lazy-fetch effect).
    pub(crate) data_page:        RwSignal<LoadState<DataPage>>,
    /// User has explicitly dismissed the "this table is huge — filter
    /// first" warning for this table. Lets them proceed to load the
    /// first page anyway. Resets per-table.
    pub(crate) data_proceed_huge: RwSignal<bool>,
    /// Server-side WHERE fragment — applied as `WHERE <fragment>` in the
    /// paged SELECT. Empty string means "no filter". Raw SQL by design
    /// (this is a DBA tool); the placeholder copy makes the contract
    /// explicit. Changing this resets page_idx to 0 and refetches.
    pub(crate) data_filter:       RwSignal<String>,
    /// Client-side filter applied in the renderer — case-insensitive
    /// substring match across all columns of the currently-loaded
    /// page. Local to memory; no round-trip. Persists across pagination
    /// so a "filter visible rows" intent survives Next/Prev.
    pub(crate) data_search:       RwSignal<String>,
    /// Last-known horizontal scroll offset of the data grid in pixels.
    /// Captured in the on:scroll handler and restored via rAF after a
    /// re-render so sort flips / page navigation don't bounce the user
    /// back to column 0 of a wide table.
    pub(crate) data_scroll_left:  RwSignal<i32>,
    /// Same idea, vertical axis. Less common (each page is a fresh
    /// dataset so vertical position isn't usually meaningful) but
    /// preserved symmetrically — the same re-render that resets
    /// scrollLeft also resets scrollTop.
    pub(crate) data_scroll_top:   RwSignal<i32>,
    /// Which body layout the Data tab is rendering — grid by default,
    /// vertical when the user has clicked the "≡ Vertical" button
    /// with a row selected. Resets to Grid on tab switch and on every
    /// data refetch (since row indices are no longer meaningful).
    pub(crate) data_view_mode:    RwSignal<DataViewMode>,
    /// Index into the current page's rows for vertical-mode display.
    /// None until the user clicks a row in grid mode. Reset to None
    /// on data refetch so the vertical view doesn't display a stale
    /// row from a different sort/filter.
    pub(crate) data_selected_row: RwSignal<Option<usize>>,
    /// Companion to `data_selected_row` — column index for the focused
    /// cell. Set by clicking a cell or by ←/→ keyboard navigation while
    /// in the Data body. Drives the per-cell `data-cell-focus` border.
    /// Cleared (set to None) when the user Esc's out of the Data body.
    pub(crate) data_selected_col: RwSignal<Option<usize>>,
    /// Row cursor inside the Schema-tab columns list. `Some(i)` means
    /// the user has descended from the tab strip into the Schema body
    /// and is at column row `i`. `None` means SchemaBody mode is not
    /// active. Mutually exclusive with `data_selected_col` because the
    /// user can only be inside one tab body at a time.
    pub(crate) schema_selected_row: RwSignal<Option<usize>>,
    /// On-demand exact row count from `SELECT COUNT(*)`. Populated only
    /// when the user clicks the "Exact" button next to the estimate.
    /// Cleared on every server-filter change since the count belongs to
    /// the prior filter's rowset.
    pub(crate) data_exact_count:  RwSignal<Option<u64>>,
    /// (row_idx, column_name) currently being edited inline. None when
    /// no cell is in edit mode. Set by double-click on a cell; cleared
    /// when the editor commits (Enter / blur) or cancels (Escape).
    pub(crate) data_active_edit:  RwSignal<Option<(usize, String)>>,
    /// Buffer for the active editor's text content. Populated with the
    /// cell's current value when edit mode opens; the user types into
    /// it; on commit the value is sent as the SET clause's literal.
    pub(crate) data_edit_input:   RwSignal<String>,
    /// Per-(row_idx, column) status for cells with in-flight or
    /// recently-completed edits. Saving = pending DML, Saved = green
    /// pulse for ~1.5s, Failed = red border + error tooltip until the
    /// user retries the same cell.
    pub(crate) data_edit_status:  RwSignal<HashMap<(usize, String), EditStatus>>,
    /// Maps DML tag (sent in ExecuteDml) → (row_idx, column, new_val)
    /// so the response handler can route back to the right cell AND
    /// patch the in-memory page on success without refetching the
    /// whole page from the DB.
    pub(crate) data_edit_pending: RwSignal<HashMap<String, (usize, String, String)>>,
    /// Set briefly when Escape cancels an in-flight edit, so the
    /// follow-up `on:blur` (fired as the input unmounts) knows to
    /// skip the commit instead of writing the typed-but-abandoned
    /// value to the DB. Reset to false by the blur handler.
    pub(crate) data_edit_aborting: RwSignal<bool>,
    /// Per-column input values for an in-progress Insert. Missing
    /// keys (or empty values) are omitted from the INSERT so the DB
    /// default fires; typing the literal "NULL" (uppercase) emits a
    /// bare SQL NULL, matching the Phase 4 UPDATE convention. Cleared
    /// on Save success or Cancel.
    pub(crate) data_insert_buffer: RwSignal<HashMap<String, String>>,
    /// In-flight status for the active Insert (Saving / Saved / Failed
    /// with error message). None when no Insert is being saved.
    pub(crate) data_insert_status: RwSignal<Option<EditStatus>>,
    /// Whether the user has toggled "Upsert" on the active Insert form.
    /// When true and the table has a detectable PK, Save dispatches
    /// `upsert_row_sql` instead of `insert_row_sql` — PG `ON CONFLICT
    /// (pk) DO UPDATE` / MySQL `ON DUPLICATE KEY UPDATE`. Reset to
    /// false on every fresh + click so the user opts in deliberately.
    pub(crate) data_insert_upsert: RwSignal<bool>,
    /// Bumped to force a page re-fetch even when the SELECT SQL hasn't
    /// changed. Used by the manual refresh button AND by the Insert
    /// response handler in main.rs (which doesn't know the new row's
    /// auto-generated values, so a refetch is the right way to surface
    /// them in the grid).
    pub(crate) data_refresh_tick: RwSignal<u32>,
    /// Row index armed for a Delete confirmation. `Some(idx)` means
    /// the user clicked "Delete row" while row `idx` was selected and
    /// the inline confirmation banner is showing. None = no pending
    /// delete. Cleared on Cancel, Esc, or after the DELETE response
    /// (success or failure).
    pub(crate) data_delete_pending: RwSignal<Option<usize>>,
    /// Per-row delete status. Keyed by original row index from the
    /// page (the same idx stamped on `<tr data-row-idx>`). Cleared
    /// when the page refetches.
    pub(crate) data_delete_status: RwSignal<HashMap<usize, EditStatus>>,
}

impl TableDetail {
    pub(crate) fn new() -> Self {
        TableDetail {
            overview:    create_rw_signal(LoadState::Loading),
            columns:     create_rw_signal(LoadState::Loading),
            indexes:     create_rw_signal(LoadState::Loading),
            ddl:         create_rw_signal(LoadState::Loading),
            ancestors:   create_rw_signal(LoadState::Loading),
            descendants: create_rw_signal(LoadState::Loading),
            partitions:  create_rw_signal(LoadState::Loading),
            toast:       create_rw_signal(LoadState::Loading),
            vm:          create_rw_signal(LoadState::Loading),
            autovac:     create_rw_signal(LoadState::Loading),
            refreshing:  create_rw_signal(false),
            stale:       create_rw_signal(false),
            autovac_action_vacuum:  create_rw_signal(false),
            autovac_action_analyze: create_rw_signal(false),
            autovac_action_freeze:  create_rw_signal(false),
            autovac_action_verbose: create_rw_signal(false),
            autovac_action_error:   create_rw_signal(None),
            autovac_run:            create_rw_signal(None),
            active_tab:  create_rw_signal(DetailTab::Schema),
            relationships_expanded: create_rw_signal(false),
            erd_zoom:    create_rw_signal(1.0),
            erd_generations: create_rw_signal(2),
            erd_keys_only: create_rw_signal(true),
            erd_cards_per_band: create_rw_signal(crate::explorer::ERD_DEFAULT_BAND_CAP),
            erd_overflow_popover: create_rw_signal(None),
            erd_neighbour_columns: create_rw_signal(LoadState::NotLoaded),
            object_grants:         create_rw_signal(LoadState::NotLoaded),
            privileges_show_inherited: create_rw_signal(true),
            privileges_show_no_grant:  create_rw_signal(false),
            privileges_filter:     create_rw_signal(String::new()),
            data_page_index:    create_rw_signal(0),
            data_page_size:     create_rw_signal(200),
            data_sort_column:   create_rw_signal(None),
            data_sort_desc:     create_rw_signal(false),
            data_page:          create_rw_signal(LoadState::NotLoaded),
            data_proceed_huge:  create_rw_signal(false),
            data_filter:        create_rw_signal(String::new()),
            data_search:        create_rw_signal(String::new()),
            data_scroll_left:   create_rw_signal(0),
            data_scroll_top:    create_rw_signal(0),
            data_view_mode:     create_rw_signal(DataViewMode::Grid),
            data_selected_row:  create_rw_signal(None),
            data_selected_col:  create_rw_signal(None),
            schema_selected_row: create_rw_signal(None),
            data_exact_count:   create_rw_signal(None),
            data_active_edit:   create_rw_signal(None),
            data_edit_input:    create_rw_signal(String::new()),
            data_edit_status:   create_rw_signal(HashMap::new()),
            data_edit_pending:  create_rw_signal(HashMap::new()),
            data_edit_aborting: create_rw_signal(false),
            data_insert_buffer: create_rw_signal(HashMap::new()),
            data_insert_status: create_rw_signal(None),
            data_insert_upsert: create_rw_signal(false),
            data_refresh_tick:  create_rw_signal(0),
            data_delete_pending: create_rw_signal(None),
            data_delete_status:  create_rw_signal(HashMap::new()),
        }
    }
}

// ── Pane layout tree ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SplitDir { H, V }

/// A pane leaf: holds an ordered list of shell-session tabs and an active index.
/// Both fields are RwSignal (Copy), so PaneLeaf is Copy — usable in closures freely.
/// Equal and hashed by identity (its signals'), not by its tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PaneLeaf {
    pub(crate) tabs:   RwSignal<Vec<String>>,
    pub(crate) active: RwSignal<usize>,
}

impl PaneLeaf {
    pub(crate) fn new(pane_id: String) -> Self {
        PaneLeaf {
            tabs:   create_rw_signal(vec![pane_id]),
            active: create_rw_signal(0),
        }
    }

    pub(crate) fn active_pane_id(&self) -> Option<String> {
        // Reactive reads on BOTH fields so callers in a Leptos render
        // closure re-run when either tabs or active changes. The tab
        // bar's action buttons (render_leaf_tab_bar) used to subscribe
        // only to tabs, so the brief intermediate state during
        // close_pane (tabs updated, active not yet) leaked through as a
        // stale-indexed out-of-bounds and the New-shell button captured
        // an empty pane id.
        let tabs = self.tabs.get();
        let idx  = self.active.get();
        tabs.into_iter().nth(idx)
    }
}

/// What a pane leaf displays.
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum PaneKind { Shell, SchemaExplorer, Activity }

impl PaneKind {
    /// Client-role label sent on `ClientMessage::Connect` so noctua can
    /// set PG's `application_name` to `"Parliament - <role>"`. Shows up
    /// in `pg_stat_activity.application_name` for DBA observability —
    /// they can see which pane a session belongs to at a glance.
    pub(crate) fn application_role(self) -> &'static str {
        match self {
            PaneKind::Shell          => "SQL Shell",
            PaneKind::SchemaExplorer => "Schema Explorer",
            PaneKind::Activity       => "Activity Monitor",
        }
    }
}

/// Which sub-tab is selected inside the role-detail inspector.
/// General is the always-available key/value grid; Privileges shows the
/// role's effective object access — explicit grants AND ownership-implied
/// access in one unified table, with a Source column distinguishing the
/// two. Memberships / Settings are placeholders for the next round.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RoleDetailTab { General, Privileges }

/// Sub-view inside the Privileges tab. `Grants` is the default
/// scope-leveled list (the existing role-privileges output, now
/// with a checkbox column for selection). `AffectedObjects` is
/// the resolved view — each selected grant's scope expanded out
/// to the concrete tables it touches.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RolePrivsView { Grants, AffectedObjects }

// ── Activity pane data types ──────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct SessionRow {
    pub pid:    String,
    pub user:   String,
    pub client: String,
    pub db:     String,
    pub state:  String,
    pub time:   String,
    pub query:  String,
}

#[derive(Clone)]
pub(crate) struct LockRow {
    pub pid:      String,
    pub mode:     String,
    pub object:   String,
    pub granted:  bool,
    pub duration: String,
}

#[derive(Clone)]
pub(crate) struct BlockEdge {
    pub waiter_pid:  String,
    pub blocker_pid: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ActivitySort { User, Client, Oldest, Newest }

impl ActivitySort {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ActivitySort::User    => "User",
            ActivitySort::Client  => "Client",
            ActivitySort::Oldest  => "Oldest",
            ActivitySort::Newest  => "Newest",
        }
    }
    pub(crate) const ALL: [ActivitySort; 4] = [
        ActivitySort::Oldest, ActivitySort::Newest,
        ActivitySort::User, ActivitySort::Client,
    ];
}

/// One entry in the ⊡ pane-type dropdown.
#[derive(Clone, Copy)]
pub(crate) struct PaneMenuItem {
    pub kind:  PaneKind,
    pub label: &'static str,
    pub icon:  &'static str,
}

/// Cell holding the Activity pane's refresh-timer state.
pub(crate) type ActivityTimerSlot = Option<(i32, Closure<dyn FnMut()>)>;

/// Recursive pane layout.
#[derive(Clone, Debug)]
pub(crate) enum PaneTree {
    Leaf(PaneLeaf),
    Split { dir: SplitDir, ratio: RwSignal<f64>, a: Box<PaneTree>, b: Box<PaneTree> },
}

impl PaneTree {
    /// ID of the first visible pane.
    pub(crate) fn first_pane_id(&self) -> String {
        match self {
            PaneTree::Leaf(leaf) => leaf.active_pane_id().unwrap_or_default(),
            PaneTree::Split { a, .. } => a.first_pane_id(),
        }
    }

    /// All pane IDs across all leaves.
    #[allow(dead_code)]
    pub(crate) fn all_pane_ids(&self) -> Vec<String> {
        match self {
            PaneTree::Leaf(leaf) => leaf.tabs.get_untracked(),
            PaneTree::Split { a, b, .. } => {
                let mut ids = a.all_pane_ids();
                ids.extend(b.all_pane_ids());
                ids
            }
        }
    }

    /// Find the PaneLeaf containing pane_id, if any.
    pub(crate) fn find_leaf(&self, pane_id: &str) -> Option<PaneLeaf> {
        match self {
            PaneTree::Leaf(leaf) => {
                if leaf.tabs.get_untracked().iter().any(|id| id == pane_id) {
                    Some(*leaf)
                } else {
                    None
                }
            }
            PaneTree::Split { a, b, .. } => {
                a.find_leaf(pane_id).or_else(|| b.find_leaf(pane_id))
            }
        }
    }

    /// Split the leaf containing `target` into two panes side by side / stacked.
    pub(crate) fn split_at(self, target: &str, new_id: &str, dir: SplitDir) -> Self {
        match self {
            PaneTree::Leaf(leaf) => {
                if leaf.tabs.get_untracked().iter().any(|id| id == target) {
                    PaneTree::Split {
                        dir,
                        ratio: create_rw_signal(0.5),
                        a: Box::new(PaneTree::Leaf(leaf)),
                        b: Box::new(PaneTree::Leaf(PaneLeaf::new(new_id.to_string()))),
                    }
                } else {
                    PaneTree::Leaf(leaf)
                }
            }
            PaneTree::Split { dir: d, ratio: r, a, b } => PaneTree::Split {
                dir: d,
                ratio: r,
                a: Box::new(a.split_at(target, new_id, dir)),
                b: Box::new(b.split_at(target, new_id, dir)),
            },
        }
    }

    /// Remove a leaf from the tree only if its last tab is being closed.
    pub(crate) fn remove_empty_leaf(self, pane_id: &str) -> Option<Self> {
        match self {
            PaneTree::Leaf(leaf) => {
                let tabs = leaf.tabs.get_untracked();
                if tabs.len() == 1 && tabs.first().map(|s| s.as_str()) == Some(pane_id) {
                    None
                } else {
                    Some(PaneTree::Leaf(leaf))
                }
            }
            PaneTree::Split { dir, ratio, a, b } => {
                match (a.remove_empty_leaf(pane_id), b.remove_empty_leaf(pane_id)) {
                    (None, None)             => None,
                    (Some(t), None) | (None, Some(t)) => Some(t),
                    (Some(a2), Some(b2))     => Some(PaneTree::Split { dir, ratio, a: Box::new(a2), b: Box::new(b2) }),
                }
            }
        }
    }
}

// ── Workspace tab ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct WorkspaceTab {
    pub(crate) id:     String,
    #[allow(dead_code)]
    pub(crate) label:  String,
    pub(crate) layout: RwSignal<PaneTree>,
    /// When `Some(pane_id)`, only the leaf containing that pane renders —
    /// other leaves are hidden. Double-clicking a pane tab toggles this.
    /// The underlying `layout` tree is never mutated, so restoring the
    /// previous geometry is instant.
    pub(crate) zoomed_pane_id: RwSignal<Option<String>>,
}

impl WorkspaceTab {
    pub(crate) fn new(label: String, layout: PaneTree) -> Self {
        WorkspaceTab {
            id:     Uuid::new_v4().to_string(),
            label,
            layout: create_rw_signal(layout),
            zoomed_pane_id: create_rw_signal(None),
        }
    }
}

// ── Meta-command action type ────────────────────────────────────────────────

pub(crate) enum MetaAction {
    Disconnect,
    RunSql(String, bool),
    /// One of psql's list commands (`\dt`, `\l`, `\dp` …): its query,
    /// shown vertically or not, under psql's title (`List of relations`),
    /// or — when no rows come back and psql says so — `none` (`Did not
    /// find any tables named "x".`); `footers`, a second query psql
    /// runs after it for the lines under its rows.
    List { sql: String, vertical: bool, title: String, none: Option<String>, counts_rows: bool, footers: Option<FooterQuery> },
    Output(Vec<String>),
    OutputKV(Vec<(String, String)>),
    /// psql's `\r`, the mysql CLI's `\c`: the query buffer — the input
    /// — is cleared, and the CLI says so (psql) or not.
    ClearInput(Option<&'static str>),
    /// psql's `\if` / `\elif` / `\else` / `\endif`: the shell's conditional
    /// stack moves where the command stands (`psql_if::apply`).
    Branch(crate::psql_if::Branch),
    /// A command that fails as psql's does (`HandleSlashCmds`): one it
    /// doesn't have (`invalid command \foo`), or one this shell can't
    /// run yet. The error is printed under the command, the command and
    /// the rest of its line are dropped, and the query buffer typed
    /// before it stays in the input.
    Fail(Vec<String>),
    /// psql's `\watch [[i=]SEC] [c=N] [m=MIN]` — run the query buffer
    /// (or the previous query) every `interval_ms`, each run printed.
    /// `count = Some(N)` stops after N runs; `min_rows = Some(N)` stops
    /// after a result with fewer than N rows. Both None = until stopped.
    Watch { interval_ms: u32, count: Option<u32>, min_rows: Option<u32> },
    /// psql's `\c`: a new session on this database (None: the current
    /// one), the previous kept until it's open (`ShellTab.switching`).
    ReconnectDb(Option<String>),
    /// The mysql CLI's `connect` (`\r`): the session closed and a new one
    /// opened on `database` (None: the current one), the buffer cleared
    /// (`com_connect`, `shell::connect_anew`). `host`, when named, must be
    /// the connection's: another needs credentials of its own.
    Connect { database: Option<String>, host: Option<String> },
    SetDelimiter(String),
    /// psql's `\x [on|off|auto]` / `\pset expanded`: set, or with no
    /// value toggle (`auto` toggles off), and say so as psql does.
    SetExpanded(Option<Expanded>),
    /// psql's `\timing [on|off]`: None toggles.
    SetTiming(Option<bool>),
    SafeMode(bool),         // \safe-mode on / off
    /// The mysql CLI's `rehash` (`\#`): completion's names fetched afresh
    /// — the shell's cached tables and columns dropped — silently.
    RebuildCompletion,
    /// The mysql CLI's `warnings` / `nowarning` (`\W` / `\w`): the
    /// server's warnings shown after each statement that has them, or
    /// fails (`ShellTab.show_warnings`) — or not — and what the CLI says.
    ShowWarnings { on: bool, said: String },
    Introspect { sql: String, tag: String },  // fire IntrospectQuery (result routes by tag)
    /// Run these queries as one batch (`IntrospectBatch`), then ask the
    /// plane what's next (`DbmsPlane::meta_continue`) — how psql's
    /// `\d <table>` runs its catalog queries.
    Staged(Vec<String>),
    /// `\pset` of a printer's setting (`border`, `linestyle`,
    /// `fieldsep`, `pager` … `\f`, `\T`): the settings as they now are,
    /// and what psql says about them (nothing, for some).
    SetTextOptions { options: TextOptions, said: Vec<String> },
    /// `\pset null [value]`: show SQL NULL as `value` from now on — or,
    /// none given, just say what it is.
    SetNullDisplay(Option<String>),
    SetVariable(String, String),  // \set name value
    UnsetVariable(String),         // \unset name
    ListVariables,                 // \set (no args)
    /// `\pset format` (`\a`, `\H`, Parliament's `\output`): print
    /// results in `format` — and set `\x` too when `expanded` says
    /// (`\output vertical` / `table`).
    SetFormat { format: OutputFormat, expanded: Option<Expanded> },
    /// `\e [line]` — open the query buffer (the previous query when
    /// it's empty, as psql does) in the external editor, at `line`
    /// when given. `seed` replaces the buffer instead — psql's blank
    /// `\ef` / `\ev` templates. noctua picks the editor. The edited
    /// text comes back into the input and waits for Enter.
    EditExternal { seed: Option<String>, line: Option<u32> },
    /// `\g` / `\gx` — execute the current input buffer (with `\g`
    /// stripped) as a query. `expanded` mirrors psql's `\gx`: forces
    /// vertical output for this one execution regardless of the
    /// session's sticky `\x` setting.
    Go { expanded: bool },
    /// `\errverbose` — re-render the last query error in the shell
    /// history. (Tyto's wire shape carries only the message string
    /// today, so the "verbose" expansion is limited to re-printing
    /// the same text — fuller SQLSTATE / DETAIL / HINT propagation
    /// is a follow-up.)
    ErrVerbose,
    /// psql's `\s`: the shell's history — what Up-arrow walks — as
    /// typed, oldest first, this command last, and a blank line after
    /// (`printHistory`).
    ShowHistory,
    /// `\p` — the query buffer as the CLI holds it, printed as its
    /// `\p` prints it (`DbmsPlane::print_buffer`).
    PrintBuffer,
    /// `\pset tuples_only` (`\t`) — on, results print without their
    /// header and row count. A value sets it, silently as psql's does;
    /// none toggles it and says so.
    SetTuplesOnly(Option<bool>),
    /// `\pset footer` — off, results print without their row count
    /// (but with their header). A value sets it silently; none toggles
    /// it and says so.
    SetFooter(Option<bool>),
    /// `\pset title` (`\C`) — the title over results from now on;
    /// none unsets it (an empty one is a title, as in psql).
    SetTitle(Option<String>),
    /// `\prompt [text] name` — interactive variable read. Sets the
    /// shell into prompt mode: the next Enter takes whatever is in
    /// the input buffer and assigns it to the named variable. Esc
    /// cancels without writing. The optional `text` is rendered
    /// once when the action fires so the user sees what they're
    /// being asked for.
    Prompt { text: String, name: String },
    /// `\gexec` — execute the current input buffer (with `\gexec`
    /// stripped) as a query, then re-execute each non-NULL cell of
    /// the result as its own SQL. Shell strips the suffix; main.rs
    /// QueryResult routing wires up the per-cell submission via
    /// `pending_post_action = Some(Gexec)`.
    Gexec,
    /// `\gdesc` — describe the columns the current buffer would
    /// produce, WITHOUT executing it. Shell strips the suffix and
    /// sends `ClientMessage::DescribeQuery`; main.rs renders the
    /// returned (name, type) pairs as a two-column Table. Pure
    /// catalog-introspection on the backend (sqlx's `describe()`);
    /// no side effects (no triggers, no functions, no row scans).
    Gdesc,
    /// `\gset [prefix]` — execute the current input buffer (with
    /// the meta stripped) as a query; on a single-row result, bind
    /// each column into a `\set` variable named `<prefix><col>`.
    /// Empty prefix uses the column name verbatim.
    Gset { prefix: String },
    /// `\crosstabview [colV [colH [colD [sortcolH]]]]` — execute
    /// the current buffer, then pivot the result into a 2-D grid
    /// via `psql_crosstab::pivot_table` before rendering.
    Crosstabview {
        col_v:     Option<String>,
        col_h:     Option<String>,
        col_d:     Option<String>,
        sortcol_h: Option<String>,
    },
    /// `\password [user]` — interactive client-side SCRAM-SHA-256
    /// password change. The shell arms a two-stage masked prompt;
    /// when both inputs match, the browser computes the hash and
    /// submits `ALTER USER … PASSWORD '<hash>'`. The plaintext
    /// password never crosses the WebSocket. `user = None` means
    /// "current session user" (resolved at submit time from
    /// `:USER`).
    Password { user: Option<String> },
}

/// psql's expanded display (`popt.topt.expanded`): off, on, or `auto` —
/// each result vertical only when its aligned table is wider than the
/// pane (`DbmsPlane::expands_automatically`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Expanded {
    #[default]
    Off,
    On,
    Auto,
}

impl Expanded {
    /// `\x` with no value: on ↔ off — and from `auto`, off, as psql's
    /// `!expanded` makes 2 into 0.
    pub(crate) fn toggled(self) -> Expanded {
        if self == Expanded::Off { Expanded::On } else { Expanded::Off }
    }

    /// What psql says once it's set (`printPsetInfo`).
    pub(crate) fn said(self) -> &'static str {
        match self {
            Expanded::Off  => "Expanded display is off.",
            Expanded::On   => "Expanded display is on.",
            Expanded::Auto => "Expanded display is used automatically.",
        }
    }
}

/// How a column's cells line up: text left, numbers right — as the CLI
/// decides by the column's type (`DbmsPlane::column_align`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Align {
    #[default]
    Left,
    Right,
}

/// How results print — psql's `\pset format` (`printFormat`), and
/// Parliament's own `json` / `markdown` (`\output`). `Aligned` results
/// show as the shell's grid; the others print as their CLI prints them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    #[default]
    Aligned,
    Asciidoc,
    Csv,
    Html,
    Latex,
    LatexLongtable,
    TroffMs,
    Unaligned,
    Wrapped,
    Json,
    Markdown,
}

impl OutputFormat {
    /// Its name, as psql says it (`Output format is unaligned.`).
    pub(crate) fn name(self) -> &'static str {
        match self {
            OutputFormat::Aligned        => "aligned",
            OutputFormat::Asciidoc       => "asciidoc",
            OutputFormat::Csv            => "csv",
            OutputFormat::Html           => "html",
            OutputFormat::Latex          => "latex",
            OutputFormat::LatexLongtable => "latex-longtable",
            OutputFormat::TroffMs        => "troff-ms",
            OutputFormat::Unaligned      => "unaligned",
            OutputFormat::Wrapped        => "wrapped",
            OutputFormat::Json           => "json",
            OutputFormat::Markdown       => "markdown",
        }
    }
}

/// How the shell prints a table — psql's `printTableOpt`: the format,
/// `\x`, `\t`, `\pset footer`, the text printers' separators and the like
/// (`text`), the width to fit — `\pset columns`, or else the pane's
/// in characters, psql's terminal width for `\x auto`, `wrapped` and
/// `xheader_width page` (None: not measured — nothing expands, wraps or
/// stops at the page) — and the
/// client's locale, which `\pset numericlocale` writes numbers in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrintOptions {
    pub format:      OutputFormat,
    pub expanded:    Expanded,
    pub tuples_only: bool,
    /// psql's `default_footer`: a table's row count, `(2 rows)`.
    pub footer:      bool,
    pub columns:     Option<usize>,
    pub text:        TextOptions,
    pub locale:      shared::NumericLocale,
}

impl Default for PrintOptions {
    /// psql's: aligned, not expanded, the row count shown.
    fn default() -> Self {
        PrintOptions {
            format: OutputFormat::Aligned, expanded: Expanded::Off, tuples_only: false, footer: true,
            columns: None, text: TextOptions::default(), locale: shared::NumericLocale::default(),
        }
    }
}

/// A separator psql's unaligned printer writes (`\pset fieldsep` /
/// `recordsep`): text, or a zero byte (`fieldsep_zero` /
/// `recordsep_zero`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Separator {
    Text(String),
    Zero,
}

impl Separator {
    /// What it writes.
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Separator::Text(text) => text,
            Separator::Zero => "\0",
        }
    }
}

/// The settings psql's printers read besides the format, `\x`, `\t` and
/// the footer: `\pset fieldsep` and `recordsep` (unaligned),
/// `csv_fieldsep`, `tableattr` (html, latex-longtable), `columns` (the
/// width to fit; 0: the pane's), the lines tables are drawn with
/// (`rules`), `numericlocale` (numbers in the client's locale:
/// `PrintOptions.locale`) — and those kept, as psql keeps them, without
/// effect here: `pager` (a pane has none) and `pager_min_lines`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextOptions {
    pub fieldsep:        Separator,
    pub recordsep:       Separator,
    pub csv_fieldsep:    char,
    pub tableattr:       Option<String>,
    pub columns:         i32,
    pub rules:           Rules,
    /// psql's `topt.pager`: 0 off, 1 for long output, 2 always.
    pub pager:           u8,
    pub pager_min_lines: i32,
    pub numeric_locale:  bool,
}

impl Default for TextOptions {
    /// psql's: `|`, a newline, `,`, none, unset, border 1 in ascii, the
    /// pager for long output, no locale grouping.
    fn default() -> Self {
        TextOptions {
            fieldsep:        Separator::Text("|".into()),
            recordsep:       Separator::Text("\n".into()),
            csv_fieldsep:    ',',
            tableattr:       None,
            columns:         0,
            rules:           Rules::default(),
            pager:           1,
            pager_min_lines: 0,
            numeric_locale:  false,
        }
    }
}

/// psql's `\pset xheader_width`: how far an expanded record's line runs
/// — the full width, to the header column's end, to the page's width
/// (`\pset columns`, or the pane's), or so many characters (a width of
/// 0 or less: the full width).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum XHeader {
    #[default]
    Full,
    Column,
    Page,
    Exact(i32),
}

/// psql's `\pset linestyle`: the characters its aligned and wrapped
/// printers draw lines and marks with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum LineStyle {
    #[default]
    Ascii,
    OldAscii,
    Unicode,
}

impl LineStyle {
    /// Its name, as `\pset linestyle` takes and says it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            LineStyle::Ascii    => "ascii",
            LineStyle::OldAscii => "old-ascii",
            LineStyle::Unicode  => "unicode",
        }
    }
}

/// A line of the unicode style: `\pset unicode_border_linestyle`,
/// `unicode_column_linestyle`, `unicode_header_linestyle`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum UnicodeLine {
    #[default]
    Single,
    Double,
}

impl UnicodeLine {
    /// Its name, as `\pset` takes and says it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            UnicodeLine::Single => "single",
            UnicodeLine::Double => "double",
        }
    }
}

/// How psql's printers draw a table's lines: `\pset border` — aligned
/// and wrapped: 0 none, 1 between columns, 2 a frame as well (any more
/// reads as 2); html writes it as it is, latex reads up to 3 — and, for
/// aligned and wrapped, `\pset linestyle`, the unicode style's border,
/// column and header lines, and how far an expanded record's line runs
/// (`\pset xheader_width`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rules {
    pub border:         u16,
    pub style:          LineStyle,
    pub unicode_border: UnicodeLine,
    pub unicode_column: UnicodeLine,
    pub unicode_header: UnicodeLine,
    pub xheader:        XHeader,
}

impl Default for Rules {
    /// psql's: border 1, ascii, single lines, full-width record lines.
    fn default() -> Self {
        Rules {
            border:         1,
            style:          LineStyle::Ascii,
            unicode_border: UnicodeLine::Single,
            unicode_column: UnicodeLine::Single,
            unicode_header: UnicodeLine::Single,
            xheader:        XHeader::Full,
        }
    }
}

impl MetaAction {
    /// The command works on the query buffer typed before it: runs it
    /// (the `\g` family, `\watch`), describes it (`\gdesc`), edits it
    /// (`\e`, `\ef`, `\ev`) or clears it (`\r`, mysql's `\c`). Any other
    /// leaves the buffer where it is, as psql does (`select 1 \x`).
    pub(crate) fn works_on_buffer(&self) -> bool {
        matches!(self,
            MetaAction::ClearInput(_) | MetaAction::EditExternal { .. } | MetaAction::Go { .. }
            | MetaAction::Gexec | MetaAction::Gdesc | MetaAction::Gset { .. }
            | MetaAction::Crosstabview { .. } | MetaAction::Watch { .. } | MetaAction::Connect { .. })
    }
}

/// State of an active `\prompt`. While `Some(...)` is set on the
/// tab, the on_keydown handler intercepts Enter / Esc to drive the
/// variable assignment instead of the normal submit-query path.
/// `text` is stored alongside `name` for future re-display features
/// (cancel notice, escape-to-history). Currently only `name` is
/// read by the keydown intercept.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct PromptState {
    pub text: String,
    pub name: String,
}

/// What a psql list command's reply shows besides its rows
/// (`MetaAction::List`): psql's title over them, or what psql says
/// instead when there are none.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ListReply {
    pub title: String,
    pub none:  Option<String>,
    /// psql's `default_footer` for it: `(2 rows)` under the list — all
    /// but `\du`'s, which psql prints without.
    pub counts_rows: bool,
    /// The query psql runs after the list's for its footers, if any…
    pub footers: Option<FooterQuery>,
    /// …and, while that runs, the list's own result, waiting for it.
    pub held: Option<HeldList>,
}

/// A second query psql runs after a list's, whose rows become the lines
/// under it (`listSchemas`' publications, `\dn <pattern>` on PG 15+):
/// `heading`, then each row's first value as `    "value"` — in place of
/// the row count; when it finds none, the count shows as ever.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FooterQuery {
    pub sql:     String,
    pub heading: String,
}

impl FooterQuery {
    /// The footers its rows make, as psql writes them (`PQgetvalue`: a
    /// NULL is empty).
    pub(crate) fn footers(&self, rows: &[Vec<Option<String>>]) -> Vec<String> {
        if rows.is_empty() {
            return Vec::new();
        }
        std::iter::once(self.heading.clone())
            .chain(rows.iter().map(|row| format!("    \"{}\"", row.first().cloned().flatten().unwrap_or_default())))
            .collect()
    }
}

/// A list's result, held while its footer query runs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeldList {
    pub columns:      Vec<String>,
    pub types:        Vec<String>,
    pub rows:         Vec<Vec<Option<String>>>,
    pub status:       shared::CommandStatus,
    pub exec_time_us: u64,
    pub vertical:     bool,
}

/// An external edit (`\e` / `\ef` / `\ev`) in progress. While set,
/// the input is read-only and Esc stops waiting. `nonce` rides in the
/// request's tag, so a reply for an edit nobody waits on any more is
/// dropped.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExternalEdit {
    pub nonce:        u32,
    /// What the input goes back to when no text returns (an error, a
    /// discarded edit, Esc): the buffer without the `\e` line.
    pub restore:      String,
    /// The text the editor opened on.
    pub seed:         String,
    /// psql's `discard_on_quit`: when the edit comes back unchanged,
    /// put `restore` back (normally empty) instead. Set when `seed` is
    /// the previous query (the buffer was empty) and for `\ef` / `\ev`.
    pub discard_if_unchanged: bool,
    /// What was typed after the command — the mysql CLI's `select 1 \e
    /// from dual`, a paste's lines below: it follows whatever the input
    /// gets back, as the CLI appends it to the edited buffer.
    pub then: String,
}

impl ExternalEdit {
    /// `text` back in the input, followed by what was typed after.
    pub(crate) fn followed(&self, text: &str) -> String {
        format!("{text}{}", self.then)
    }
}

/// State of an active `\password` prompt. Two stages: first the
/// user types a new password, then they retype it. If the two match,
/// the shell SCRAM-hashes the value browser-side and submits the
/// `ALTER USER … PASSWORD` SQL. Mismatch aborts with a notice.
///
/// The plaintext is kept inside this struct (in `first`) only as
/// long as the prompt is active — cleared the instant the second
/// Enter fires or Esc cancels.
#[derive(Clone, Debug)]
pub(crate) struct PasswordPromptState {
    /// Target user — `None` means "current session user" (resolved
    /// from `:USER` at submit time).
    pub user:  Option<String>,
    /// `None` → still on stage 1 (first entry). `Some(s)` → on
    /// stage 2 (retype), with `s` held for comparison.
    pub first: Option<String>,
}

/// A statement wrapped as psql's `SendQuery` wraps it: what the plane
/// sends around it (`DbmsPlane::statement_opening` /
/// `statement_closing` — psql's `BEGIN` under `AUTOCOMMIT off`, its
/// savepoint under `ON_ERROR_ROLLBACK`) runs on the session as batches
/// of their own (tag `txn:<nonce>`), so their replies are never taken for
/// the statement's, and the shell goes on once the last has answered.
#[derive(Clone, Debug)]
pub(crate) struct Wrap {
    pub nonce: u32,
    pub stage: WrapStage,
}

#[derive(Clone, Debug)]
pub(crate) enum WrapStage {
    /// `BEGIN` / the savepoint running; then `send` — and with
    /// `savepoint`, a savepoint to close once it has answered.
    Opening { send: WrappedSend, savepoint: bool },
    /// The statement running inside the savepoint; `tag`, its command tag
    /// once it has answered (none for an error).
    Running { tag: Option<String> },
    /// The savepoint's `RELEASE` / `ROLLBACK TO` running.
    Closing,
}

/// A statement the user runs, as the shell sends it: a query, or one to
/// describe (`\gdesc`).
#[derive(Clone, Debug)]
pub(crate) enum WrappedSend {
    Query(String),
    Describe(String),
}

impl WrappedSend {
    pub(crate) fn sql(&self) -> &str {
        match self {
            WrappedSend::Query(sql) | WrappedSend::Describe(sql) => sql,
        }
    }
}

/// What to do with the NEXT QueryResult that lands on the tab.
/// Latched from the statement's `post` when it's sent (`\gexec`,
/// `\gset`, `\crosstabview`); consumed and cleared by the QueryResult
/// handler. None = normal flow (push entry into
/// shell_history). QueryError clears it too — an errored query can't
/// produce gexec statements or gset values.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PostAction {
    /// `\gexec` — take each non-NULL cell of the result as its own
    /// SQL statement and queue them through the existing multi-
    /// statement `pending_stmts` machinery.
    Gexec,
    /// `\gset [prefix]` — bind a single-row result's columns into
    /// `\set` variables. `prefix` is prepended to every column name.
    /// Empty prefix = use the column name verbatim.
    Gset { prefix: String },
    /// `\crosstabview` — pivot the next result via
    /// `psql_crosstab::pivot_table` before rendering. Specs (col_v
    /// / col_h / col_d / sortcol_h) are raw strings here; resolved
    /// against the actual result columns in the QueryResult handler.
    Crosstabview {
        col_v:     Option<String>,
        col_h:     Option<String>,
        col_d:     Option<String>,
        sortcol_h: Option<String>,
    },
    /// psql's `\gdesc`, its second step: the described columns' names
    /// and types (`DbmsPlane::described_columns_sql`), printed as any
    /// result, its time the describe's (`describe_us`) and this query's.
    Gdesc { describe_us: u64 },
}

/// Initial focus for a freshly-created `PaneKind::SchemaExplorer`
/// pane. When `add_pane` creates the explorer with `Some(init)`,
/// the new tab's `explorer_selected` is pre-populated with
/// `(schema, name)` so the pane opens already focused on the
/// requested relation. Driven by `\d <name>`: its lookup reply (tag
/// `describe_open:`) opens the pane through main.rs's
/// `open_explorer_slot`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExplorerInit {
    /// The database the shell is on (after `\c`, not necessarily the
    /// connection's default) — the explorer opens there.
    pub database: Option<String>,
    /// Tables or Views — which list the relation belongs in.
    pub kind:     ObjectKind,
    pub schema:   String,
    pub name:     String,
}

// ── Explorer action shell ─────────────────────────────────────────────────────
//
// Administrative commands the explorer runs (Storage tab → Run) execute
// in the explorer's own background shell tab: a separate session, so
// they can never touch a transaction in one of the user's shells, and
// a shell, so the SQL, timing, errors and notices stay observable.

/// A command for an explorer's action shell.
#[derive(Clone, Debug)]
pub(crate) struct ActionShellRun {
    pub(crate) explorer_pane_id: String,
    pub(crate) schema:           String,
    pub(crate) name:             String,
    pub(crate) sql:              String,
}

/// Set on an action shell while a command the explorer sent is in
/// flight; when its result arrives, that explorer's table refreshes.
#[derive(Clone, Debug)]
pub(crate) struct ActionOrigin {
    pub(crate) explorer_pane_id: String,
    pub(crate) schema:           String,
    pub(crate) name:             String,
}

/// What a table's Run panel remembers about its last command.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActionRun {
    pub(crate) pane_id: String,
    pub(crate) sql:     String,
}

/// Pane operations the explorer needs from the app, which owns the
/// workspace layout. Copy callbacks, so they thread into click handlers
/// without context lookups (those return None there).
#[derive(Clone, Copy)]
pub(crate) struct PaneOps {
    /// Run a command in the explorer's action shell, opening that
    /// shell as a background tab next to the explorer if it doesn't
    /// exist or is busy. Returns the shell's pane id, or `None` when
    /// the explorer's connection can't be found.
    pub(crate) run_in_action_shell: Callback<ActionShellRun, Option<String>>,
    /// Bring a pane to the front: its workspace tab, its tab within
    /// the leaf, and keyboard focus.
    pub(crate) focus:               Callback<String>,
    /// Open a Schema Explorer tab next to a pane, on a relation (the
    /// links in psql's `\d` output).
    pub(crate) open_explorer:       Callback<(String, ExplorerInit)>,
}

// ── Transaction state tracking ────────────────────────────────────────────────

pub(crate) use shared::TxnState;

/// Carries the SQL waiting on a safe-mode confirm dialog. Pre-fix this
/// was a bare `Option<String>` because only single-statement input was
/// ever gated. Audit finding 1.1 showed that multi-statement pastes
/// bypassed safe-mode entirely; the dialog has to cover batches too.
///
/// Single → one statement, optional `\G` vertical-output marker.
/// Batch  → the full ordered list of statements to run, plus the
///          indexes of the destructive ones (so the dialog can quote
///          them back to the DBA before they sign off on the whole
///          batch).
#[derive(Clone, Debug)]
pub(crate) enum PendingConfirm {
    Single {
        stmt: TypedStatement,
        /// Repeated by `\watch` once confirmed, rather than run once.
        watch: Option<WatchBounds>,
        /// Reason the inspector flagged this as destructive. Surfaced
        /// in the dialog so the DBA understands why it's gated (e.g.,
        /// "UPDATE without WHERE" vs "DROP" vs "parse failed —
        /// confirm if intentional").
        reason: crate::sql_inspect::DestructiveReason,
    },
    Batch {
        /// All statements in the batch, in execution order. The
        /// non-destructive ones flow through unchanged after confirm.
        batch: StatementBatch,
        /// Indexes (into `batch.run`) of the destructive statements.
        /// The dialog renders previews of these.
        destructive_indexes: Vec<usize>,
    },
}

// ── Right pane ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Pane { Welcome, NewConnection, Shell }

/// How often `\watch` repeats its statement and when it stops
/// (`MetaAction::Watch`): psql's interval in milliseconds, run count
/// and fewest rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WatchBounds {
    pub interval_ms:   u32,
    pub count:         Option<u32>,
    pub min_rows:      Option<u32>,
}

/// A `\c` under way (`ShellTab.switching`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Switching {
    /// The session it replaces, if there was one (none after a failed
    /// reset): closed once the new one opens, kept if that fails.
    pub(crate) previous: Option<String>,
}

/// A lost session being replaced (`ShellTab.reopening`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Reopening {
    /// The mysql CLI's reconnect: this statement, already in the
    /// scrollback, goes to the new session.
    Resend(String),
    /// psql's reset: the rest of the input (`pending_stmts`) goes on in
    /// the new session.
    Reset,
    /// The mysql CLI's reconnect for a staged command (its `help topic`,
    /// the server's `HELP`): run again in the new session.
    Meta { command: String, queries: Vec<String> },
    /// The mysql CLI's `connect` (`\r`): its new session, and whether a
    /// database was named — then the CLI reads its names for completion,
    /// and says so.
    Connect { named: bool },
}

// ── Per-pane state ────────────────────────────────────────────────────────────

/// Status shown in the sidebar for a connected pane.
#[derive(Clone, Default, PartialEq)]
pub(crate) enum PaneStatus {
    #[default]
    NotStarted,
    Ok(String),
    Err(String),
}

// ── Closed-pane reopen stack ─────────────────────────────────────────────────

/// Minimal snapshot pushed onto the reopen stack when a pane is closed, so
/// Cmd+Shift+Z can recreate it. We don't try to preserve DB session state —
/// a reopen is always a fresh connection to the same connection definition.
#[derive(Clone)]
pub(crate) struct ClosedPaneInfo {
    pub(crate) conn_id:      String,
    pub(crate) kind:         PaneKind,
    pub(crate) custom_label: Option<String>,
    /// The database it was on, so reopening returns there.
    pub(crate) database:     Option<String>,
}

/// Where the tab context menu was anchored, plus which pane it refers to.
#[derive(Clone)]
pub(crate) struct TabContextMenu {
    pub(crate) pane_id: String,
    pub(crate) x:       f64,
    pub(crate) y:       f64,
}

/// Right-click context menu on the shell input textarea. Carries the
/// owning pane's id so the menu's "Format" verbs know which `ShellTab`'s
/// input signal + plane to apply against. Position is the right-click
/// viewport coordinates.
#[derive(Clone)]
pub(crate) struct ShellInputContextMenu {
    pub(crate) pane_id: String,
    pub(crate) x:       f64,
    pub(crate) y:       f64,
    /// `Some(n)` when the right-click landed on a query block (the
    /// rendered group of one `ShellEntry::Query` plus its trailing
    /// result entries, where `n` is the Query's sequence number).
    /// The menu render prepends block-scoped items (Copy command /
    /// Copy output / Copy whole block) above the existing
    /// shell-level items. `None` for right-clicks outside any block
    /// (empty scroll area, textarea, gutter) — those see only the
    /// shell-level items.
    pub(crate) block_num: Option<u32>,
}

/// Tab-completion popup state. Anchored to the shell input's bounding
/// rect (the popup sits below the textarea — pixel-exact cursor
/// positioning is deferred). `replace_start` is the byte offset in the
/// input where the prefix begins; accepting an item replaces
/// `input[replace_start..cursor]` with the chosen text.
#[derive(Clone)]
pub(crate) struct CompletionPopup {
    pub(crate) pane_id:       String,
    pub(crate) x:             f64,
    pub(crate) y:             f64,
    pub(crate) items:         Vec<String>,
    pub(crate) selected:      usize,
    pub(crate) replace_start: usize,
}

/// What the sidebar's right-click context menu was anchored on. Distinguishes
/// a connection row (which has cut/copy/edit/delete verbs) from a folder
/// header (which has new-folder/new-connection/paste/rename/delete verbs).
#[derive(Clone)]
pub(crate) enum SidebarMenuTarget {
    /// `source_path` is the folder the row was rendered under — empty when
    /// the row was in the synthetic "All Connections" view. Cut / Copy /
    /// Remove-from-folder all key off this so a linked connection's other
    /// placements remain untouched.
    Connection { conn_id: String, source_path: String },
    /// `path` is the folder's slash-separated path; empty path means the
    /// root "All Connections" header. `is_root` short-circuits rename /
    /// delete (those don't apply to the synthetic root).
    Folder { path: String, is_root: bool },
}

#[derive(Clone)]
pub(crate) struct SidebarContextMenu {
    pub(crate) x:      f64,
    pub(crate) y:      f64,
    pub(crate) target: SidebarMenuTarget,
}

/// What action a Cut/Copy/Copy-as-Link gesture stamps onto the clipboard.
/// The verb is consulted when the user picks Paste (vs Paste-as-Link) at
/// a destination — Paste honors the verb, Paste-as-Link always forces
/// Link semantics regardless.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardVerb {
    Cut,
    Copy,
    Link,
}

/// What's currently pending on the connection-management clipboard.
/// Cleared after a Paste (when verb was Cut, the move can only happen
/// once); preserved after Copy / Link / Paste-as-Link so the user can
/// drop the same payload into multiple folders.
#[derive(Clone)]
pub(crate) struct ClipboardEntry {
    pub(crate) conn_id:     String,
    pub(crate) verb:        ClipboardVerb,
    /// Folder the clipboard entry was Cut from, if any. Empty for entries
    /// stamped from the "All Connections" synthetic view (where no real
    /// source folder applies).
    pub(crate) source_path: String,
}

/// In-app modal dialog. Used in place of `window.prompt` / `window.confirm`,
/// which surface the browser chrome's `"localhost:8080 says"` framing —
/// jarring in an otherwise polished app. Two shapes: a text prompt with an
/// input field, and a yes/no confirm. Both close on backdrop click and Esc.
#[derive(Clone)]
pub(crate) enum DialogKind {
    Prompt {
        title:         String,
        message:       String,
        default_value: String,
        confirm_label: String,
        /// Fires with the trimmed input when the user clicks Confirm or
        /// presses Enter. Cancel / Esc / backdrop click skip it.
        on_submit:     leptos::Callback<String>,
    },
    Confirm {
        title:         String,
        message:       String,
        confirm_label: String,
        /// True for destructive verbs (Delete connection, Delete folder) —
        /// the confirm button renders in danger styling.
        danger:        bool,
        /// Fires with `()` when the user confirms.
        on_confirm:    leptos::Callback<()>,
    },
}

// ── Notifications ─────────────────────────────────────────────────────────────

/// One entry in the app-header notification bell's activity queue. A
/// notification is emitted when a query completes or errors in a pane that
/// is not currently active (and not in `\watch` mode — watch has its own
/// reactive display and would otherwise peg the badge).
///
/// The queue is the single source of truth for unread activity: both the
/// bell-badge total and each sidebar connection's unread badge derive from
/// it. Focus-gaining a pane removes that pane's notifications.
#[derive(Clone)]
pub(crate) struct Notification {
    pub(crate) id:            String,
    pub(crate) pane_id:       String,
    pub(crate) pane_label:    String,
    pub(crate) conn_id:       String,
    pub(crate) conn_name:     String,
    pub(crate) db_type:       DbType,
    pub(crate) event:         NotificationEvent,
    pub(crate) sql_snippet:   Option<String>,
    pub(crate) created_at_ms: f64,
}

#[derive(Clone)]
pub(crate) enum NotificationEvent {
    Ok  { rows_affected: u64, exec_time_us: u64, has_cols: bool },
    Err { message: String },
}

/// Tab completion's column cache: `(schema, table) → LoadState<[columns]>`.
pub(crate) type CompletionColumns = HashMap<(String, String), LoadState<Vec<String>>>;

/// Where the right-click menu on a partitioned table's partition count
/// opened (x, y), the parent's (schema, name), and whether its partitions
/// were expanded then — which names the menu's one verb.
pub(crate) type PartitionMenuAnchor = (f64, f64, (String, String), bool);

/// An admin area's drill-in detail rows, cached by (area, row name,
/// section name).
pub(crate) type AdminRowDetails = HashMap<(AdminArea, String, String), LoadState<Vec<Vec<String>>>>;

/// All reactive state for one shell pane.
#[derive(Clone)]
pub(crate) struct ShellTab {
    pub(crate) id:               String,
    pub(crate) conn_id:          String,
    pub(crate) label:            String,
    pub(crate) kind:             PaneKind,
    pub(crate) db_type:          DbType,
    pub(crate) session_id:       RwSignal<Option<String>>,
    pub(crate) session_version:  RwSignal<String>,
    /// Names of extensions installed on this session's PG connection
    /// (empty vec on MySQL). Populated once from the SessionOpened wire
    /// message that fires on connect; consumed by plane methods that
    /// need to gate SQL on extension availability (e.g. table_vm_sql
    /// picks its pg_visibility vs pg_class path from this list). If the
    /// DBA installs a new extension via the shell, this doesn't refresh
    /// until they reconnect — acceptable trade-off given how rarely
    /// extensions are added mid-session.
    pub(crate) session_extensions: RwSignal<Vec<String>>,
    /// The role this session connects as was refused
    /// `pg_visibility_map_summary()` (SQLSTATE 42501), though the
    /// extension is installed: the explorer's Visibility map reads
    /// `pg_class` instead for the rest of the session, and says why.
    /// Reset when a session opens — its privileges may differ.
    pub(crate) vm_live_denied: RwSignal<bool>,
    /// What the client knows of the session, from `SessionOpened`:
    /// whether the server asked for a password, and noctua's TLS library
    /// (psql's `\conninfo`).
    pub(crate) session_password_used: RwSignal<bool>,
    pub(crate) session_ssl_library: RwSignal<String>,
    /// The client's numeric locale (noctua's environment, as psql's
    /// own), from `SessionOpened`: `\pset numericlocale` writes numbers
    /// in it.
    pub(crate) session_numeric_locale: RwSignal<shared::NumericLocale>,
    pub(crate) db_connection_id: RwSignal<Option<String>>,
    /// True when this pane's session_id was minted before the most recent
    /// WS Disconnected event — i.e. the noctua-side session is gone and any
    /// query against it will silently fail. Set en-masse on Disconnected;
    /// cleared per-pane when a fresh `SessionOpened` arrives. Drives the ✕
    /// in the connection-id pill and the ⚠ on the pane tab title.
    pub(crate) session_stale:    RwSignal<bool>,
    /// User-assigned tab name via right-click → Rename. `None` falls back
    /// to the current-db / connection-name logic in the tab renderer.
    pub(crate) custom_label:     RwSignal<Option<String>>,
    pub(crate) connecting:       RwSignal<bool>,
    pub(crate) shell_history:    RwSignal<Vec<ShellEntry>>,
    /// Block-numbers (Query.num) the user has collapsed via the
    /// `⊖` toggle on a Table/Vertical entry. Membership = collapsed
    /// (wrapped in a fixed-height scroll container); absence =
    /// expanded (flow layout). Per-pane so different shells can
    /// have different collapsed states. Survives across re-renders;
    /// cleared automatically when shell_history is wiped because
    /// the affected query numbers no longer exist.
    pub(crate) collapsed_blocks: RwSignal<std::collections::HashSet<u32>>,
    pub(crate) current_input:    RwSignal<String>,
    pub(crate) pending_vertical: RwSignal<bool>,
    pub(crate) query_count:      RwSignal<u32>,
    pub(crate) hist_pos:         RwSignal<Option<usize>>,
    pub(crate) saved_input:      RwSignal<String>,
    pub(crate) txn_state:        RwSignal<TxnState>,
    pub(crate) history_key:      String,
    /// This connection's saved history, read when the tab's first
    /// session opens — once, as psql reads its history file at startup:
    /// read again after a reconnect or `\c` it would hold this session's
    /// commands twice. None until then.
    pub(crate) persisted_cmds:   RwSignal<Option<Vec<String>>>,
    pub(crate) watch_sql:        RwSignal<Option<String>>,
    /// `\watch`'s interval in milliseconds (psql's `sleep_ms`).
    pub(crate) watch_interval:   RwSignal<Option<u32>>,
    pub(crate) watch_tick:       RwSignal<u32>,
    /// The `\watch` a run in flight belongs to (`watch_generation` when
    /// it was sent); None: no run in flight. A run of a watch since
    /// stopped or started over answers to nothing.
    pub(crate) watch_pending:    RwSignal<Option<u32>>,
    /// Which `\watch` this is: one more each time one starts.
    pub(crate) watch_generation: RwSignal<u32>,
    /// The title of the `\watch` run in flight (`shell::watch_title`).
    pub(crate) watch_title:      RwSignal<String>,
    /// `\watch count=N`. None = unlimited; Some(N) decrements after
    /// each run, stops after the last.
    pub(crate) watch_count:      RwSignal<Option<u32>>,
    /// `\watch min_rows=N`. None = no min; Some(N) stops the watch
    /// when the most recent result returned fewer than N rows.
    pub(crate) watch_min_rows:   RwSignal<Option<u32>>,
    pub(crate) watch_timer:      StoredValue<ActivityTimerSlot>,
    /// Per-pane format style. Initialized from `ConnectionDef.format_style`
    /// on tab creation; the shell-input right-click "Format" menu uses
    /// this as the "default" choice when the user picks it. Manual style
    /// picks from the same menu override per-keystroke (one-shot).
    pub(crate) format_style:     RwSignal<FormatStyle>,
    // Ctrl+R reverse search state (Shell panes only)
    pub(crate) search_active:   RwSignal<bool>,
    pub(crate) search_term:     RwSignal<String>,
    pub(crate) search_pos:      RwSignal<usize>,
    pub(crate) querying:         RwSignal<bool>,
    pub(crate) last_status:      RwSignal<PaneStatus>,
    pub(crate) explorer_schemas:       RwSignal<LoadState<Vec<String>>>,
    pub(crate) explorer_schema:        RwSignal<Option<String>>,
    /// Set by a psql list command (`MetaAction::List`) for its query's
    /// reply: the title its result shows under, and what's said instead
    /// when no rows come back — and, for a list psql runs a second query
    /// after, its result while that runs. Consumed by the next
    /// QueryResult — and dropped by a QueryError — so a statement after
    /// it doesn't inherit it.
    pub(crate) pending_list:           RwSignal<Option<ListReply>>,
    /// A staged meta-command (`MetaAction::Staged`, psql's `\d
    /// <table>`) waiting for its next batch.
    pub(crate) pending_meta:           RwSignal<Option<PendingMeta>>,
    /// The last error a command here got (`\errverbose`).
    pub(crate) last_error:             RwSignal<Option<LastError>>,
    /// The SQL last sent to run here, exactly — an error's position
    /// counts into it (`shell::send_statement`).
    pub(crate) sent_sql:               RwSignal<Option<String>>,
    /// Database list for the explorer's Database dropdown (PG only).
    /// Fetched lazily the first time the dropdown opens.
    pub(crate) explorer_databases:     RwSignal<LoadState<Vec<String>>>,
    /// Set true between sending ListDatabases and receiving DatabaseList
    /// for this pane, so the response handler routes the list to
    /// `explorer_databases` instead of the connection-form consumer.
    pub(crate) explorer_db_pending:    RwSignal<bool>,
    pub(crate) explorer_kind:          RwSignal<ObjectKind>,
    pub(crate) explorer_search:        RwSignal<String>,
    pub(crate) explorer_all_schemas:   RwSignal<bool>,
    pub(crate) explorer_listing:       RwSignal<LoadState<Vec<ObjectRow>>>,
    pub(crate) explorer_selected:        RwSignal<Option<(String, String)>>,
    /// Keyboard cursor position in the object list. Drives the row
    /// highlight (same visual as the `-active` class) so there's only
    /// ever one highlight on screen. Click sets both `cursor_key` and
    /// `selected`; arrow keys set only `cursor_key`; Enter clicks the
    /// row at `cursor_key`, which then matches `selected` and loads.
    pub(crate) explorer_cursor_key:      RwSignal<Option<(String, String)>>,
    pub(crate) explorer_detail_visible:  RwSignal<bool>,
    pub(crate) explorer_detail_expanded: RwSignal<bool>,
    pub(crate) explorer_last_fetch:      RwSignal<Option<(ObjectKind, bool, Option<String>)>>,
    pub(crate) explorer_table_details:   RwSignal<HashMap<(String, String), TableDetail>>,
    /// Tab-completion table cache: `schema → [tables]`. Loaded lazily on
    /// the first Tab press in a context that asks for tables, refreshed
    /// after a DDL hit by the same hook that invalidates the explorer
    /// caches. Per-Shell-pane rather than per-connection so each pane
    /// gets its own fetch deadline (cheap query; not worth the cross-
    /// pane sharing refactor in v1).
    pub(crate) completion_tables:        RwSignal<LoadState<HashMap<String, Vec<String>>>>,
    /// Tab-completion column cache: `(schema, table) → LoadState<[columns]>`.
    /// Prefetched when the user types `.` after a known table name; read
    /// at Tab time to populate the column-completion popup. Per-entry
    /// LoadState so a second `.` on the same table doesn't re-fire the
    /// fetch and so an in-flight fetch can be distinguished from a never-
    /// requested one. DDL invalidation clears the whole map.
    pub(crate) completion_columns:       RwSignal<CompletionColumns>,
    pub(crate) explorer_object_ddls:     RwSignal<HashMap<(ObjectKind, String, String), LoadState<String>>>,
    /// True when the user has switched the typebar from object kinds
    /// (Tables/Views/...) to Admin. Hides the schema picker and replaces
    /// the object listing with a list of admin areas.
    pub(crate) explorer_admin_active: RwSignal<bool>,
    /// Currently-selected admin area when admin_active is true; None when
    /// the user has just clicked Admin and hasn't drilled into an area
    /// yet (we render the area-list inspector).
    pub(crate) explorer_admin_area:   RwSignal<Option<AdminArea>>,
    /// Per-ObjectKind memory of the last (schema, name) the user clicked
    /// in that kind's listing. Lets the typebar switch Tables ↔ Views ↔
    /// Triggers preserve "where you were" inside each kind. Updated on
    /// every selection change; read on kind change to restore the prior
    /// selection in that namespace.
    pub(crate) explorer_selections_by_kind: RwSignal<HashMap<ObjectKind, (String, String)>>,
    /// Per-ObjectKind sort state for the outer object list.
    /// `(column_label, is_desc)` — column label is the visible header
    /// text (e.g. "Name" / "Rows" / "Updated"). Absence from the map
    /// means "no client-side sort" and the SQL's ORDER BY governs.
    /// Sort state persists across kind switches so each kind
    /// remembers its own ordering.
    pub(crate) explorer_sort_by_kind: RwSignal<HashMap<ObjectKind, (String, bool)>>,
    /// `(schema, parent_name)` keys for partition-parent rows the user has
    /// explicitly expanded in the outer object list. Parents with ≤ 6
    /// partition children render expanded by default regardless of this
    /// set; parents with > 6 default to collapsed and require an entry
    /// here to show their children. Cleared on schema/kind switch is
    /// unnecessary — stale entries naturally do nothing when the parent
    /// isn't in the current listing.
    pub(crate) explorer_expanded_parents: RwSignal<std::collections::HashSet<(String, String)>>,
    /// Anchor for the partition-row right-click context menu. `Some((x, y,
    /// parent_key, is_expanded))` while open; `None` when closed. The fourth
    /// field captures the parent's expansion state at the moment the menu
    /// was anchored, so the menu can label its single verb correctly
    /// ("Collapse partitions" when expanded, "Expand partitions" when
    /// collapsed) without re-deriving the threshold logic. Explicit per-
    /// project rule that right-click never silently fires an action.
    pub(crate) explorer_partition_menu: RwSignal<Option<PartitionMenuAnchor>>,
    /// Admin Roles list. Loaded lazily the first time the user enters
    /// the Roles admin area in this pane; cached for the session.
    pub(crate) explorer_admin_roles:    RwSignal<LoadState<Vec<AdminRow>>>,
    /// Currently-clicked admin entity (role / tablespace / etc.). Cleared
    /// when the admin area changes since the previous selection's
    /// namespace would no longer apply.
    pub(crate) explorer_admin_selected: RwSignal<Option<AdminRow>>,
    /// Selected sub-tab inside the role-detail inspector. Resets to
    /// General whenever `explorer_admin_selected` changes.
    pub(crate) explorer_admin_role_tab: RwSignal<RoleDetailTab>,
    /// Per-role privileges, lazy-fetched on first view of the Privileges
    /// tab. Reset to NotLoaded whenever `explorer_admin_selected` changes
    /// so the next role's privileges fetch fresh. Each row is a raw
    /// column tuple — the renderer pairs columns to plane-supplied
    /// headers via `role_privileges_columns`.
    pub(crate) explorer_admin_role_privileges: RwSignal<LoadState<Vec<Vec<String>>>>,
    /// Active sub-view inside the Privileges tab — `Grants` shows the
    /// scope-leveled list with selection checkboxes; `AffectedObjects`
    /// shows the resolver output (selected grants expanded to per-
    /// table object rows). Resets to `Grants` on role switch.
    pub(crate) explorer_admin_role_privs_view: RwSignal<RolePrivsView>,
    /// Indices into `explorer_admin_role_privileges` for grants the
    /// user has selected. Populated to "all rows" once when the
    /// privileges data first loads for this role; user can deselect
    /// rows to narrow the affected-objects resolution. Cleared on
    /// role switch. The companion `_auto_selected` flag guards
    /// against re-selecting all rows on subsequent reloads (e.g.
    /// after DDL invalidation) — once the user has interacted with
    /// the set, the auto-select doesn't fire again.
    pub(crate) explorer_admin_role_privs_selected: RwSignal<std::collections::HashSet<usize>>,
    /// Latch: true once the "select all" auto-select has run for the
    /// current role. Reset to false on role switch. Used to suppress
    /// re-applying the auto-select when DDL forces a privs reload
    /// (which would otherwise silently override the user's manual
    /// deselection of every row).
    pub(crate) explorer_admin_role_privs_auto_selected: RwSignal<bool>,
    /// Resolver output: the union of tables touched by the currently-
    /// selected grants. Re-fetched whenever the user switches to the
    /// AffectedObjects sub-view (selection may have changed). Reset
    /// to NotLoaded on role switch.
    pub(crate) explorer_admin_role_affected_objects: RwSignal<LoadState<Vec<Vec<String>>>>,
    /// Kind-filter chips for the Privileges Grants list. `None` =
    /// no filter (every Kind visible). `Some(set)` = show only rows
    /// whose Kind is in the set. Rows hidden by the filter don't
    /// feed the affected-objects resolver either — the resolver
    /// sees the effective (selected ∩ visible) row set so the badge
    /// count matches what a user can see and click.
    pub(crate) explorer_admin_role_privs_kinds: RwSignal<Option<std::collections::HashSet<String>>>,
    /// Kind-filter chips for the Show-affected-objects list.
    /// Separate from explorer_admin_role_privs_kinds because the
    /// two sub-views show different Kind vocabularies — the Grants
    /// view talks about grant-scope kinds (database, schema, table,
    /// view, function...) while the resolved list talks about pure
    /// object kinds (table, view, sequence, function, procedure,
    /// matview...). Filtering them independently keeps the two
    /// mental models separate.
    pub(crate) explorer_admin_role_affected_kinds: RwSignal<Option<std::collections::HashSet<String>>>,
    /// Per-cell status for editable privilege matrices. Keyed by
    /// (grantee, privilege). `Saving` → cell is mid-GRANT/REVOKE,
    /// disabled in the UI; `Saved` flashes green for ~1.5s then
    /// auto-clears; `Failed(msg)` paints the cell red with the
    /// DB error in the title attribute. Shared across the object
    /// matrix and the Schemas admin matrix — only one matrix is
    /// visible at a time per tab, and cleanup hooks clear this
    /// map on detail-context switches.
    pub(crate) explorer_priv_cell_status: RwSignal<std::collections::HashMap<(String, String), EditStatus>>,
    /// "Are you sure you want to revoke?" confirmation banner state.
    /// Some((grantee, privilege)) → banner visible for that cell;
    /// the REVOKE fires when the user clicks the banner's Revoke
    /// button. Cleared on Cancel, on detail-context switch, and
    /// after a successful REVOKE.
    pub(crate) explorer_priv_pending_revoke: RwSignal<Option<(String, String)>>,
    /// Cache of flat-list data for non-Roles admin areas (Tablespaces,
    /// Databases, Extensions, ServerConfig). Keyed by AdminArea so each
    /// area is independently loaded; the renderer pulls the matching
    /// entry on demand. Roles uses its own typed signal because rows are
    /// clickable and need name/extras separation.
    pub(crate) explorer_admin_area_data: RwSignal<HashMap<AdminArea, LoadState<Vec<Vec<String>>>>>,
    /// (area, row-name) of the currently clicked row in a non-Roles area.
    /// None = no row clicked. Roles uses its own typed `admin_selected`.
    pub(crate) explorer_admin_row_selected: RwSignal<Option<(AdminArea, String)>>,
    /// Per-(area, row-name, section-name) cached detail rows. Populated
    /// lazily when the user opens a drill-in inspector and the section
    /// hasn't been fetched yet for this row. Cleared on row change.
    pub(crate) explorer_admin_row_detail: RwSignal<AdminRowDetails>,
    pub(crate) activity_sessions:     RwSignal<LoadState<Vec<SessionRow>>>,
    pub(crate) activity_locks:        RwSignal<LoadState<Vec<LockRow>>>,
    pub(crate) activity_blocks:       RwSignal<LoadState<Vec<BlockEdge>>>,
    pub(crate) activity_interval:     RwSignal<Option<u32>>,
    pub(crate) activity_hide_idle:    RwSignal<bool>,
    pub(crate) activity_sort:         RwSignal<ActivitySort>,
    pub(crate) activity_last_updated: RwSignal<Option<f64>>,
    pub(crate) activity_tick:         RwSignal<u32>,
    pub(crate) activity_confirm_kill: RwSignal<Option<String>>,
    pub(crate) activity_timer:        StoredValue<ActivityTimerSlot>,
    /// Sessions section header collapse state (false = expanded).
    /// In-memory only — resets when the tab is re-created (page reload,
    /// pane re-open). Matches how `expanded` / `relationships_expanded`
    /// scope their state.
    pub(crate) activity_sessions_collapsed: RwSignal<bool>,
    /// Locks section header collapse state (false = expanded).
    pub(crate) activity_locks_collapsed:    RwSignal<bool>,
    /// Blocking section header collapse state (false = expanded). Only
    /// meaningful when there are blocking edges to display — the outer
    /// `move ||` still hides the whole section when `blocks_sig` is
    /// empty, so this toggle only kicks in on a table with real edges.
    pub(crate) activity_blocks_collapsed:   RwSignal<bool>,
    /// PIDs whose query cells the DBA has clicked to expand. Membership
    /// = "this cell shows full-wrapped query text instead of one-line
    /// truncated." Toggles on left-click; persists across polling ticks
    /// so an expanded cell stays expanded when the sessions list
    /// refreshes. In-memory per-tab.
    pub(crate) activity_expanded_queries:   RwSignal<std::collections::HashSet<String>>,
    pub(crate) current_db:             RwSignal<String>,
    pub(crate) pane_menu_open:        RwSignal<bool>,
    pub(crate) delimiter:             RwSignal<String>,
    pub(crate) expanded:              RwSignal<Expanded>,
    pub(crate) show_timing:           RwSignal<bool>,
    pub(crate) safe_mode:             RwSignal<bool>,
    pub(crate) null_display:          RwSignal<String>,
    pub(crate) output_format:         RwSignal<OutputFormat>,
    /// psql's `\pset fieldsep` / `recordsep` / `csv_fieldsep` /
    /// `tableattr` / `columns`.
    pub(crate) pset_text:             RwSignal<TextOptions>,
    /// psql `\pset tuples_only` (`\t`) — when true, hides the
    /// table header row and the row-count footer on Table results.
    pub(crate) pset_tuples_only:      RwSignal<bool>,
    /// psql `\pset footer` — when false, hides the row-count
    /// footer (but keeps the header). Distinct from `tuples_only`.
    pub(crate) pset_footer:           RwSignal<bool>,
    /// psql `\pset title` (`\C`) — the title over results printed from
    /// now on; an empty one is a title (a blank line), None none.
    pub(crate) pset_title:            RwSignal<Option<String>>,
    /// psql `\set ON_ERROR_STOP`. Off by default, as in psql (and the
    /// mysql CLI, which always is): the QueryError handler continues
    /// with the next queued statement. On: it drops the rest.
    pub(crate) on_error_stop:         RwSignal<bool>,
    /// Active `\prompt` state. `Some(...)` switches the on_keydown
    /// handler into prompt mode; `None` is the normal shell.
    pub(crate) prompt_state:          RwSignal<Option<PromptState>>,
    /// Active `\password` state. `Some(...)` switches the
    /// on_keydown handler into masked two-stage password entry; the
    /// highlight overlay renders `•` chars instead of tokens while
    /// active so on-screen the password reads as bullets.
    pub(crate) password_prompt:       RwSignal<Option<PasswordPromptState>>,
    /// External edit in progress (see [`ExternalEdit`]).
    pub(crate) external_edit:         RwSignal<Option<ExternalEdit>>,
    /// What to do with the NEXT successful QueryResult — `\gexec`
    /// queues each cell as further SQL; `\gset` binds the single
    /// row into psql variables. Cleared on consume or on
    /// QueryError. None = ordinary result rendering.
    pub(crate) pending_post_action:   RwSignal<Option<PostAction>>,
    /// The statement psql's `SendQuery` wraps, while what it sends around
    /// it runs (`shell::send_user_statement`, `continue_wrap`).
    pub(crate) wrap:                  RwSignal<Option<Wrap>>,
    /// psql `PROMPT1` / `PROMPT2` / `PROMPT3` templates. Default
    /// to `psql_prompt::DEFAULT_PROMPT*`. `\set PROMPT1 'tpl'` is
    /// routed to the corresponding signal by the SetVariable
    /// handler so prompt rendering updates reactively.
    pub(crate) prompt1:               RwSignal<String>,
    /// PROMPT2 / PROMPT3 are reserved fields — initialised to the
    /// defaults, but currently never read by any render site.
    /// PROMPT2 is psql's multi-line continuation prompt (tyto
    /// submits the whole buffer per Enter, so no continuation
    /// state exists yet); PROMPT3 is shown during `\copy` (Wave 3
    /// follow-up). `\set PROMPT2 / PROMPT3` routes via
    /// AcceptedNoOp until those render sites exist.
    #[allow(dead_code)]
    pub(crate) prompt2:               RwSignal<String>,
    #[allow(dead_code)]
    pub(crate) prompt3:               RwSignal<String>,
    pub(crate) pending_confirm:       RwSignal<Option<PendingConfirm>>,
    /// The rest of the input, run one statement at a time as each
    /// answers (`shell::run_next_queued`) — `:var`s already substituted.
    pub(crate) pending_stmts:         RwSignal<Vec<TypedStatement>>,
    /// A `\gexec` whose generated statements are running: what its own
    /// statement says once they have (`shell::finish_gexec`).
    pub(crate) gexec:                 RwSignal<Option<GexecPending>>,
    /// The unended statement typed so far whose lines the CLI has said
    /// its notes about (`DbmsPlane::line_notes`) — left in the input — so
    /// a line isn't noted twice when it's entered again with more.
    pub(crate) noted:                 RwSignal<String>,
    /// Notes about text left in the input, held until what was queued
    /// before it has answered — the CLI reads that text after it.
    pub(crate) held_notes:            RwSignal<Vec<ShellEntry>>,
    /// The mysql CLI's `warnings` (`\W`): after each statement with
    /// warnings, or one that failed, the server's `SHOW WARNINGS` printed
    /// under its output (`shell::fetch_warnings`). Off to start.
    pub(crate) show_warnings:         RwSignal<bool>,
    /// What's left of an input, read on once the shell is free — after
    /// the statements before a command, a command's reply, a prompt
    /// (`shell::process_input`, `shell_busy`). Input typed meanwhile
    /// joins it, as type-ahead joins the CLI's.
    pub(crate) resume_input:          RwSignal<Option<String>>,
    /// What the CLI says once the command it's about is done, if it
    /// didn't fail — psql's `extra argument … ignored` after a command
    /// that waits for its reply or its user; dropped when it fails.
    pub(crate) pending_warnings:      RwSignal<Vec<ShellEntry>>,
    /// psql's conditional stack: the `\if` blocks open, innermost last,
    /// across Enters (`psql_if`).
    pub(crate) conditionals:          RwSignal<Vec<crate::psql_if::IfFrame>>,
    pub(crate) variables:             RwSignal<HashMap<String, String>>,
    /// SQL to run once this shell's session opens — how a new action
    /// shell receives the command it was opened for.
    pub(crate) startup_sql:           RwSignal<Option<String>>,
    /// A lost session being replaced, as the plane's CLI does
    /// (`DbmsPlane::lost_connection`); settled by SessionOpened or
    /// SessionError.
    pub(crate) reopening:             RwSignal<Option<Reopening>>,
    /// A lost session couldn't be replaced, and the CLI knows it has no
    /// connection: psql answers every statement "You are currently not
    /// connected to a database." (and its prompt says `!`), mysql skips
    /// its "Lost connection" error and just tries again. Cleared by the
    /// next SessionOpened (`\c`, or a reconnect that works).
    pub(crate) disconnected:          RwSignal<bool>,
    /// A `\c` under way: its new session is opening while the previous
    /// one stays — psql keeps it if the new one can't be opened.
    pub(crate) switching:             RwSignal<Option<Switching>>,
    /// How many times the running statement found its session dead and
    /// went to a new one — the mysql CLI gives up after two.
    pub(crate) reopen_retries:        RwSignal<u8>,
    /// Set while a command sent by an explorer runs here (see
    /// `ActionOrigin`).
    pub(crate) action_origin:         RwSignal<Option<ActionOrigin>>,
    /// Explorer panes only: this explorer's action shell, reused for
    /// later commands while it's idle.
    pub(crate) explorer_action_pane:  RwSignal<Option<String>>,
}

impl ShellTab {
    /// The database to (re)connect this pane to: the one it was on —
    /// after `\c`, `USE` or the explorer's database switcher, not
    /// necessarily the connection's default. None before its first
    /// session, meaning the connection's default.
    pub(crate) fn reconnect_database(&self) -> Option<String> {
        Some(self.current_db.get_untracked()).filter(|db| !db.is_empty())
    }

    /// What a meta-command typed here may need to know.
    /// Typed SQL as the shell's CLI sends it: with its variables
    /// expanded as psql expands them (`parser::substitute_vars`), or as
    /// it is — the mysql CLI has no `:var`s.
    pub(crate) fn interpolate(&self, sql: &str) -> String {
        if crate::plane::plane_for(self.db_type).interpolates_variables() {
            self.variables.with_untracked(|vars| crate::parser::substitute_vars(sql, vars))
        } else {
            sql.to_string()
        }
    }

    /// How a table prints in this shell now — expanded, whatever `\x`
    /// says, when `vertical` (`\gx`).
    pub(crate) fn print_options(&self, vertical: bool) -> PrintOptions {
        let format = self.output_format.get_untracked();
        let expanded = if vertical { Expanded::On } else { self.expanded.get_untracked() };
        let text = self.pset_text.get_untracked();
        // `\pset columns`, or the pane's width — which matters only to
        // `\x auto`, `wrapped` and `xheader_width page`.
        let columns = match usize::try_from(text.columns) {
            Ok(width) if width > 0 => Some(width),
            _ => (expanded == Expanded::Auto || format == OutputFormat::Wrapped || text.rules.xheader == XHeader::Page)
                .then(|| crate::dom::shell_columns(&self.id)).flatten(),
        };
        PrintOptions {
            format,
            expanded,
            tuples_only: self.pset_tuples_only.get_untracked(),
            footer:      self.pset_footer.get_untracked(),
            columns,
            text,
            locale:      self.session_numeric_locale.get_untracked(),
        }
    }

    /// What a result printed now prints with, under `title`.
    pub(crate) fn printed_with(&self, title: Option<String>) -> PrintedWith {
        PrintedWith {
            null_display: self.null_display.get_untracked(),
            tuples_only:  self.pset_tuples_only.get_untracked(),
            footer:       self.pset_footer.get_untracked(),
            title,
            rules:        self.pset_text.with_untracked(|text| text.rules),
            numeric_locale: self.pset_text.with_untracked(|text| text.numeric_locale)
                .then(|| self.session_numeric_locale.get_untracked()),
            columns:      self.print_options(false).columns,
            footers:      Vec::new(),
        }
    }

    /// psql's `QUIET`: what it leaves unsaid stays so.
    pub(crate) fn quiet(&self) -> bool {
        self.variables.with_untracked(crate::psql_vars::quiet)
    }

    pub(crate) fn meta_context(&self) -> crate::plane::MetaContext {
        crate::plane::MetaContext {
            database:    self.current_db.get_untracked(),
            print:       self.print_options(false),
            error_style: self.variables.with_untracked(crate::psql_vars::error_style),
            null_display: self.null_display.get_untracked(),
            title:       self.pset_title.get_untracked(),
            server_version: self.session_version.get_untracked(),
            user: self.variables.with_untracked(|v| v.get("USER").cloned().unwrap_or_default()),
            host: self.variables.with_untracked(|v| v.get("HOST").cloned().unwrap_or_default()),
            port: self.variables.with_untracked(|v| v.get("PORT").cloned().unwrap_or_default()),
            password_used: self.session_password_used.get_untracked(),
            ssl_library: self.session_ssl_library.get_untracked(),
            variables: self.variables.get_untracked(),
            txn: self.txn_state.get_untracked(),
        }
    }

    pub(crate) fn new(conn_id: String, label: String, db_type: DbType, history_key: String) -> Self {
        ShellTab {
            id:               Uuid::new_v4().to_string(),
            conn_id,
            label,
            kind:             PaneKind::Shell,
            db_type,
            session_id:       create_rw_signal(None),
            session_version:  create_rw_signal(String::new()),
            session_extensions: create_rw_signal(Vec::new()),
            vm_live_denied: create_rw_signal(false),
            session_password_used: create_rw_signal(false),
            session_ssl_library: create_rw_signal(String::new()),
            session_numeric_locale: create_rw_signal(shared::NumericLocale::default()),
            db_connection_id: create_rw_signal(None),
            session_stale:    create_rw_signal(false),
            custom_label:     create_rw_signal(None),
            connecting:       create_rw_signal(true),
            shell_history:    create_rw_signal(vec![]),
            collapsed_blocks: create_rw_signal(std::collections::HashSet::new()),
            current_input:    create_rw_signal(String::new()),
            pending_vertical: create_rw_signal(false),
            query_count:      create_rw_signal(1u32),
            hist_pos:         create_rw_signal(None),
            saved_input:      create_rw_signal(String::new()),
            history_key,
            persisted_cmds:   create_rw_signal(None),
            txn_state:        create_rw_signal(TxnState::Idle),
            watch_sql:        create_rw_signal(None),
            watch_interval:   create_rw_signal(None),
            watch_tick:       create_rw_signal(0u32),
            watch_pending:    create_rw_signal(None),
            watch_generation: create_rw_signal(0),
            watch_title:      create_rw_signal(String::new()),
            watch_count:      create_rw_signal(None),
            watch_min_rows:   create_rw_signal(None),
            watch_timer:      StoredValue::new(None),
            format_style:     create_rw_signal(FormatStyle::Unformatted),
            search_active:    create_rw_signal(false),
            search_term:      create_rw_signal(String::new()),
            search_pos:       create_rw_signal(0usize),
            querying:         create_rw_signal(false),
            last_status:      create_rw_signal(PaneStatus::NotStarted),
            explorer_schemas:       create_rw_signal(LoadState::NotLoaded),
            explorer_schema:        create_rw_signal(None),
            pending_list:           create_rw_signal(None),
            pending_meta:           create_rw_signal(None),
            last_error:             create_rw_signal(None),
            sent_sql:               create_rw_signal(None),
            explorer_databases:     create_rw_signal(LoadState::NotLoaded),
            explorer_db_pending:    create_rw_signal(false),
            explorer_kind:          create_rw_signal(ObjectKind::Tables),
            explorer_search:        create_rw_signal(String::new()),
            explorer_all_schemas:   create_rw_signal(false),
            explorer_listing:       create_rw_signal(LoadState::NotLoaded),
            explorer_selected:        create_rw_signal(None),
            explorer_cursor_key:      create_rw_signal(None),
            explorer_detail_visible:  create_rw_signal(true),
            explorer_detail_expanded: create_rw_signal(false),
            explorer_last_fetch:      create_rw_signal(None),
            explorer_table_details:   create_rw_signal(HashMap::new()),
            completion_tables:        create_rw_signal(LoadState::NotLoaded),
            completion_columns:       create_rw_signal(HashMap::new()),
            explorer_object_ddls:     create_rw_signal(HashMap::new()),
            explorer_admin_active: create_rw_signal(false),
            explorer_admin_area:   create_rw_signal(None),
            explorer_selections_by_kind: create_rw_signal(HashMap::new()),
            explorer_sort_by_kind:       create_rw_signal(HashMap::new()),
            explorer_expanded_parents: create_rw_signal(std::collections::HashSet::new()),
            explorer_partition_menu:   create_rw_signal(None),
            explorer_admin_roles:    create_rw_signal(LoadState::NotLoaded),
            explorer_admin_selected: create_rw_signal(None),
            explorer_admin_role_tab: create_rw_signal(RoleDetailTab::General),
            explorer_admin_role_privileges: create_rw_signal(LoadState::NotLoaded),
            explorer_admin_role_privs_view: create_rw_signal(RolePrivsView::Grants),
            explorer_admin_role_privs_selected: create_rw_signal(std::collections::HashSet::new()),
            explorer_admin_role_affected_objects: create_rw_signal(LoadState::NotLoaded),
            explorer_admin_role_privs_auto_selected: create_rw_signal(false),
            explorer_admin_role_privs_kinds: create_rw_signal(None),
            explorer_admin_role_affected_kinds: create_rw_signal(None),
            explorer_priv_cell_status: create_rw_signal(HashMap::new()),
            explorer_priv_pending_revoke: create_rw_signal(None),
            explorer_admin_area_data: create_rw_signal(HashMap::new()),
            explorer_admin_row_selected: create_rw_signal(None),
            explorer_admin_row_detail: create_rw_signal(HashMap::new()),
            activity_sessions:      create_rw_signal(LoadState::NotLoaded),
            activity_locks:         create_rw_signal(LoadState::NotLoaded),
            activity_blocks:        create_rw_signal(LoadState::NotLoaded),
            activity_interval:      create_rw_signal(Some(5)),
            activity_hide_idle:     create_rw_signal(true),
            activity_sort:          create_rw_signal(ActivitySort::Oldest),
            activity_last_updated:  create_rw_signal(None),
            activity_tick:          create_rw_signal(0u32),
            activity_confirm_kill:  create_rw_signal(None),
            activity_timer:         StoredValue::new(None),
            activity_sessions_collapsed: create_rw_signal(false),
            activity_locks_collapsed:    create_rw_signal(false),
            activity_blocks_collapsed:   create_rw_signal(false),
            activity_expanded_queries:   create_rw_signal(std::collections::HashSet::new()),
            current_db:             create_rw_signal(String::new()),
            pane_menu_open:         create_rw_signal(false),
            delimiter:              create_rw_signal(";".to_string()),
            expanded:               create_rw_signal(Expanded::Off),
            show_timing:            create_rw_signal(crate::plane::plane_for(db_type).timed_by_default()),
            safe_mode:              create_rw_signal(false),
            null_display:           create_rw_signal(crate::plane::plane_for(db_type).null_display().to_string()),
            output_format:          create_rw_signal(OutputFormat::Aligned),
            pset_text:              create_rw_signal(TextOptions::default()),
            pset_tuples_only:       create_rw_signal(false),
            pset_footer:            create_rw_signal(true),
            pset_title:             create_rw_signal(None),
            on_error_stop:          create_rw_signal(false),
            prompt_state:           create_rw_signal(None),
            password_prompt:        create_rw_signal(None),
            external_edit:          create_rw_signal(None),
            pending_post_action:    create_rw_signal(None),
            wrap:                   create_rw_signal(None),
            prompt1:                create_rw_signal(crate::psql_prompt::DEFAULT_PROMPT1.into()),
            prompt2:                create_rw_signal(crate::psql_prompt::DEFAULT_PROMPT2.into()),
            prompt3:                create_rw_signal(crate::psql_prompt::DEFAULT_PROMPT3.into()),
            pending_confirm:        create_rw_signal(None),
            pending_stmts:          create_rw_signal(vec![]),
            gexec:                  create_rw_signal(None),
            noted:                  create_rw_signal(String::new()),
            held_notes:             create_rw_signal(Vec::new()),
            show_warnings:          create_rw_signal(false),
            resume_input:           create_rw_signal(None),
            pending_warnings:       create_rw_signal(Vec::new()),
            conditionals:           create_rw_signal(Vec::new()),
            variables:              create_rw_signal(HashMap::new()),
            startup_sql:            create_rw_signal(None),
            reopening:              create_rw_signal(None),
            disconnected:           create_rw_signal(false),
            switching:              create_rw_signal(None),
            reopen_retries:         create_rw_signal(0),
            action_origin:          create_rw_signal(None),
            explorer_action_pane:   create_rw_signal(None),
        }
    }

    pub(crate) fn new_explorer(conn_id: String, label: String, db_type: DbType) -> Self {
        ShellTab {
            id:               Uuid::new_v4().to_string(),
            conn_id,
            label,
            kind:             PaneKind::SchemaExplorer,
            db_type,
            session_id:       create_rw_signal(None),
            session_version:  create_rw_signal(String::new()),
            session_extensions: create_rw_signal(Vec::new()),
            vm_live_denied: create_rw_signal(false),
            session_password_used: create_rw_signal(false),
            session_ssl_library: create_rw_signal(String::new()),
            session_numeric_locale: create_rw_signal(shared::NumericLocale::default()),
            db_connection_id: create_rw_signal(None),
            session_stale:    create_rw_signal(false),
            custom_label:     create_rw_signal(None),
            connecting:       create_rw_signal(true),
            shell_history:    create_rw_signal(vec![]),
            collapsed_blocks: create_rw_signal(std::collections::HashSet::new()),
            current_input:    create_rw_signal(String::new()),
            pending_vertical: create_rw_signal(false),
            query_count:      create_rw_signal(1u32),
            hist_pos:         create_rw_signal(None),
            saved_input:      create_rw_signal(String::new()),
            history_key:      String::new(),
            persisted_cmds:   create_rw_signal(None),
            txn_state:        create_rw_signal(TxnState::Idle),
            watch_sql:        create_rw_signal(None),
            watch_interval:   create_rw_signal(None),
            watch_tick:       create_rw_signal(0u32),
            watch_pending:    create_rw_signal(None),
            watch_generation: create_rw_signal(0),
            watch_title:      create_rw_signal(String::new()),
            watch_count:      create_rw_signal(None),
            watch_min_rows:   create_rw_signal(None),
            watch_timer:      StoredValue::new(None),
            format_style:     create_rw_signal(FormatStyle::Unformatted),
            search_active:    create_rw_signal(false),
            search_term:      create_rw_signal(String::new()),
            search_pos:       create_rw_signal(0usize),
            querying:         create_rw_signal(false),
            last_status:      create_rw_signal(PaneStatus::NotStarted),
            explorer_schemas:       create_rw_signal(LoadState::Loading),
            explorer_schema:        create_rw_signal(None),
            pending_list:           create_rw_signal(None),
            pending_meta:           create_rw_signal(None),
            last_error:             create_rw_signal(None),
            sent_sql:               create_rw_signal(None),
            explorer_databases:     create_rw_signal(LoadState::NotLoaded),
            explorer_db_pending:    create_rw_signal(false),
            explorer_kind:          create_rw_signal(ObjectKind::Tables),
            explorer_search:        create_rw_signal(String::new()),
            explorer_all_schemas:   create_rw_signal(false),
            explorer_listing:       create_rw_signal(LoadState::NotLoaded),
            explorer_selected:        create_rw_signal(None),
            explorer_cursor_key:      create_rw_signal(None),
            explorer_detail_visible:  create_rw_signal(true),
            explorer_detail_expanded: create_rw_signal(false),
            explorer_last_fetch:      create_rw_signal(None),
            explorer_table_details:   create_rw_signal(HashMap::new()),
            completion_tables:        create_rw_signal(LoadState::NotLoaded),
            completion_columns:       create_rw_signal(HashMap::new()),
            explorer_object_ddls:     create_rw_signal(HashMap::new()),
            explorer_admin_active: create_rw_signal(false),
            explorer_admin_area:   create_rw_signal(None),
            explorer_selections_by_kind: create_rw_signal(HashMap::new()),
            explorer_sort_by_kind:       create_rw_signal(HashMap::new()),
            explorer_expanded_parents: create_rw_signal(std::collections::HashSet::new()),
            explorer_partition_menu:   create_rw_signal(None),
            explorer_admin_roles:    create_rw_signal(LoadState::NotLoaded),
            explorer_admin_selected: create_rw_signal(None),
            explorer_admin_role_tab: create_rw_signal(RoleDetailTab::General),
            explorer_admin_role_privileges: create_rw_signal(LoadState::NotLoaded),
            explorer_admin_role_privs_view: create_rw_signal(RolePrivsView::Grants),
            explorer_admin_role_privs_selected: create_rw_signal(std::collections::HashSet::new()),
            explorer_admin_role_affected_objects: create_rw_signal(LoadState::NotLoaded),
            explorer_admin_role_privs_auto_selected: create_rw_signal(false),
            explorer_admin_role_privs_kinds: create_rw_signal(None),
            explorer_admin_role_affected_kinds: create_rw_signal(None),
            explorer_priv_cell_status: create_rw_signal(HashMap::new()),
            explorer_priv_pending_revoke: create_rw_signal(None),
            explorer_admin_area_data: create_rw_signal(HashMap::new()),
            explorer_admin_row_selected: create_rw_signal(None),
            explorer_admin_row_detail: create_rw_signal(HashMap::new()),
            activity_sessions:      create_rw_signal(LoadState::NotLoaded),
            activity_locks:         create_rw_signal(LoadState::NotLoaded),
            activity_blocks:        create_rw_signal(LoadState::NotLoaded),
            activity_interval:      create_rw_signal(Some(5)),
            activity_hide_idle:     create_rw_signal(true),
            activity_sort:          create_rw_signal(ActivitySort::Oldest),
            activity_last_updated:  create_rw_signal(None),
            activity_tick:          create_rw_signal(0u32),
            activity_confirm_kill:  create_rw_signal(None),
            activity_timer:         StoredValue::new(None),
            activity_sessions_collapsed: create_rw_signal(false),
            activity_locks_collapsed:    create_rw_signal(false),
            activity_blocks_collapsed:   create_rw_signal(false),
            activity_expanded_queries:   create_rw_signal(std::collections::HashSet::new()),
            current_db:             create_rw_signal(String::new()),
            pane_menu_open:         create_rw_signal(false),
            delimiter:              create_rw_signal(";".to_string()),
            expanded:               create_rw_signal(Expanded::Off),
            show_timing:            create_rw_signal(crate::plane::plane_for(db_type).timed_by_default()),
            safe_mode:              create_rw_signal(false),
            null_display:           create_rw_signal(crate::plane::plane_for(db_type).null_display().to_string()),
            output_format:          create_rw_signal(OutputFormat::Aligned),
            pset_text:              create_rw_signal(TextOptions::default()),
            pset_tuples_only:       create_rw_signal(false),
            pset_footer:            create_rw_signal(true),
            pset_title:             create_rw_signal(None),
            on_error_stop:          create_rw_signal(false),
            prompt_state:           create_rw_signal(None),
            password_prompt:        create_rw_signal(None),
            external_edit:          create_rw_signal(None),
            pending_post_action:    create_rw_signal(None),
            wrap:                   create_rw_signal(None),
            prompt1:                create_rw_signal(crate::psql_prompt::DEFAULT_PROMPT1.into()),
            prompt2:                create_rw_signal(crate::psql_prompt::DEFAULT_PROMPT2.into()),
            prompt3:                create_rw_signal(crate::psql_prompt::DEFAULT_PROMPT3.into()),
            pending_confirm:        create_rw_signal(None),
            pending_stmts:          create_rw_signal(vec![]),
            gexec:                  create_rw_signal(None),
            noted:                  create_rw_signal(String::new()),
            held_notes:             create_rw_signal(Vec::new()),
            show_warnings:          create_rw_signal(false),
            resume_input:           create_rw_signal(None),
            pending_warnings:       create_rw_signal(Vec::new()),
            conditionals:           create_rw_signal(Vec::new()),
            variables:              create_rw_signal(HashMap::new()),
            startup_sql:            create_rw_signal(None),
            reopening:              create_rw_signal(None),
            disconnected:           create_rw_signal(false),
            switching:              create_rw_signal(None),
            reopen_retries:         create_rw_signal(0),
            action_origin:          create_rw_signal(None),
            explorer_action_pane:   create_rw_signal(None),
        }
    }

    pub(crate) fn new_activity(conn_id: String, label: String, db_type: DbType) -> Self {
        ShellTab {
            id:               Uuid::new_v4().to_string(),
            conn_id,
            label,
            kind:             PaneKind::Activity,
            db_type,
            session_id:       create_rw_signal(None),
            session_version:  create_rw_signal(String::new()),
            session_extensions: create_rw_signal(Vec::new()),
            vm_live_denied: create_rw_signal(false),
            session_password_used: create_rw_signal(false),
            session_ssl_library: create_rw_signal(String::new()),
            session_numeric_locale: create_rw_signal(shared::NumericLocale::default()),
            db_connection_id: create_rw_signal(None),
            session_stale:    create_rw_signal(false),
            custom_label:     create_rw_signal(None),
            connecting:       create_rw_signal(true),
            shell_history:    create_rw_signal(vec![]),
            collapsed_blocks: create_rw_signal(std::collections::HashSet::new()),
            current_input:    create_rw_signal(String::new()),
            pending_vertical: create_rw_signal(false),
            query_count:      create_rw_signal(1u32),
            hist_pos:         create_rw_signal(None),
            saved_input:      create_rw_signal(String::new()),
            history_key:      String::new(),
            persisted_cmds:   create_rw_signal(None),
            txn_state:        create_rw_signal(TxnState::Idle),
            watch_sql:        create_rw_signal(None),
            watch_interval:   create_rw_signal(None),
            watch_tick:       create_rw_signal(0u32),
            watch_pending:    create_rw_signal(None),
            watch_generation: create_rw_signal(0),
            watch_title:      create_rw_signal(String::new()),
            watch_count:      create_rw_signal(None),
            watch_min_rows:   create_rw_signal(None),
            watch_timer:      StoredValue::new(None),
            format_style:     create_rw_signal(FormatStyle::Unformatted),
            search_active:    create_rw_signal(false),
            search_term:      create_rw_signal(String::new()),
            search_pos:       create_rw_signal(0usize),
            querying:         create_rw_signal(false),
            last_status:      create_rw_signal(PaneStatus::NotStarted),
            explorer_schemas:       create_rw_signal(LoadState::NotLoaded),
            explorer_schema:        create_rw_signal(None),
            pending_list:           create_rw_signal(None),
            pending_meta:           create_rw_signal(None),
            last_error:             create_rw_signal(None),
            sent_sql:               create_rw_signal(None),
            explorer_databases:     create_rw_signal(LoadState::NotLoaded),
            explorer_db_pending:    create_rw_signal(false),
            explorer_kind:          create_rw_signal(ObjectKind::Tables),
            explorer_search:        create_rw_signal(String::new()),
            explorer_all_schemas:   create_rw_signal(false),
            explorer_listing:       create_rw_signal(LoadState::NotLoaded),
            explorer_selected:        create_rw_signal(None),
            explorer_cursor_key:      create_rw_signal(None),
            explorer_detail_visible:  create_rw_signal(true),
            explorer_detail_expanded: create_rw_signal(false),
            explorer_last_fetch:      create_rw_signal(None),
            explorer_table_details:   create_rw_signal(HashMap::new()),
            completion_tables:        create_rw_signal(LoadState::NotLoaded),
            completion_columns:       create_rw_signal(HashMap::new()),
            explorer_object_ddls:     create_rw_signal(HashMap::new()),
            explorer_admin_active: create_rw_signal(false),
            explorer_admin_area:   create_rw_signal(None),
            explorer_selections_by_kind: create_rw_signal(HashMap::new()),
            explorer_sort_by_kind:       create_rw_signal(HashMap::new()),
            explorer_expanded_parents: create_rw_signal(std::collections::HashSet::new()),
            explorer_partition_menu:   create_rw_signal(None),
            explorer_admin_roles:    create_rw_signal(LoadState::NotLoaded),
            explorer_admin_selected: create_rw_signal(None),
            explorer_admin_role_tab: create_rw_signal(RoleDetailTab::General),
            explorer_admin_role_privileges: create_rw_signal(LoadState::NotLoaded),
            explorer_admin_role_privs_view: create_rw_signal(RolePrivsView::Grants),
            explorer_admin_role_privs_selected: create_rw_signal(std::collections::HashSet::new()),
            explorer_admin_role_affected_objects: create_rw_signal(LoadState::NotLoaded),
            explorer_admin_role_privs_auto_selected: create_rw_signal(false),
            explorer_admin_role_privs_kinds: create_rw_signal(None),
            explorer_admin_role_affected_kinds: create_rw_signal(None),
            explorer_priv_cell_status: create_rw_signal(HashMap::new()),
            explorer_priv_pending_revoke: create_rw_signal(None),
            explorer_admin_area_data: create_rw_signal(HashMap::new()),
            explorer_admin_row_selected: create_rw_signal(None),
            explorer_admin_row_detail: create_rw_signal(HashMap::new()),
            activity_sessions:      create_rw_signal(LoadState::Loading),
            activity_locks:         create_rw_signal(LoadState::Loading),
            activity_blocks:        create_rw_signal(LoadState::Loaded(Vec::new())),
            activity_interval:      create_rw_signal(Some(5)),
            activity_hide_idle:     create_rw_signal(true),
            activity_sort:          create_rw_signal(ActivitySort::Oldest),
            activity_last_updated:  create_rw_signal(None),
            activity_tick:          create_rw_signal(0u32),
            activity_confirm_kill:  create_rw_signal(None),
            activity_timer:         StoredValue::new(None),
            activity_sessions_collapsed: create_rw_signal(false),
            activity_locks_collapsed:    create_rw_signal(false),
            activity_blocks_collapsed:   create_rw_signal(false),
            activity_expanded_queries:   create_rw_signal(std::collections::HashSet::new()),
            current_db:             create_rw_signal(String::new()),
            pane_menu_open:         create_rw_signal(false),
            delimiter:              create_rw_signal(";".to_string()),
            expanded:               create_rw_signal(Expanded::Off),
            show_timing:            create_rw_signal(crate::plane::plane_for(db_type).timed_by_default()),
            safe_mode:              create_rw_signal(false),
            null_display:           create_rw_signal(crate::plane::plane_for(db_type).null_display().to_string()),
            output_format:          create_rw_signal(OutputFormat::Aligned),
            pset_text:              create_rw_signal(TextOptions::default()),
            pset_tuples_only:       create_rw_signal(false),
            pset_footer:            create_rw_signal(true),
            pset_title:             create_rw_signal(None),
            on_error_stop:          create_rw_signal(false),
            prompt_state:           create_rw_signal(None),
            password_prompt:        create_rw_signal(None),
            external_edit:          create_rw_signal(None),
            pending_post_action:    create_rw_signal(None),
            wrap:                   create_rw_signal(None),
            prompt1:                create_rw_signal(crate::psql_prompt::DEFAULT_PROMPT1.into()),
            prompt2:                create_rw_signal(crate::psql_prompt::DEFAULT_PROMPT2.into()),
            prompt3:                create_rw_signal(crate::psql_prompt::DEFAULT_PROMPT3.into()),
            pending_confirm:        create_rw_signal(None),
            pending_stmts:          create_rw_signal(vec![]),
            gexec:                  create_rw_signal(None),
            noted:                  create_rw_signal(String::new()),
            held_notes:             create_rw_signal(Vec::new()),
            show_warnings:          create_rw_signal(false),
            resume_input:           create_rw_signal(None),
            pending_warnings:       create_rw_signal(Vec::new()),
            conditionals:           create_rw_signal(Vec::new()),
            variables:              create_rw_signal(HashMap::new()),
            startup_sql:            create_rw_signal(None),
            reopening:              create_rw_signal(None),
            disconnected:           create_rw_signal(false),
            switching:              create_rw_signal(None),
            reopen_retries:         create_rw_signal(0),
            action_origin:          create_rw_signal(None),
            explorer_action_pane:   create_rw_signal(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_runs_substituted_and_is_remembered_as_typed() {
        let vars = HashMap::from([("pw".to_string(), "hunter2".to_string())]);
        let typed = vec![TypedStatement::new("SELECT :'pw'", ";"), TypedStatement::new("SELECT 2", "")];
        let batch = StatementBatch::substitute(typed.clone(), |sql| crate::parser::substitute_vars(sql, &vars));
        assert_eq!(batch.typed, typed);
        assert_eq!(batch.run.iter().map(TypedStatement::as_typed).collect::<Vec<_>>(), ["SELECT 'hunter2';", "SELECT 2"]);
    }
}
