//! DBMS planes — one implementation per supported database engine.
//!
//! A plane owns everything that varies per dialect: meta-command parsing,
//! status/help SQL, schema-explorer queries, identifier quoting, and the
//! syntax-highlighting keyword/function lists. The backend stays
//! DBMS-agnostic; the frontend translates meta-commands to SQL *before*
//! sending them over the wire.

use shared::{DbType, EditorVar, LineFlagVar};
use crate::{AdminArea, AdminRow, MetaAction, ObjectKind, PaneMenuItem};
use crate::parser::Lexicon;

pub mod mysql;
pub mod postgres;

/// What a meta-command may need to know about the shell it was typed
/// in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MetaContext {
    /// The database the shell's session is on.
    pub database:    String,
    /// How a table the plane prints shows (psql's `\pset`: format, `\x`,
    /// `\t`, footer, and the pane's width).
    pub print: crate::types::PrintOptions,
    /// psql's `\pset null` and `\pset title` (unset: None).
    pub null_display: String,
    pub title:       Option<String>,
    /// How much of a server error to show (`VERBOSITY`, `SHOW_CONTEXT`).
    pub error_style: ErrorStyle,
    /// The server's `version()` text, as the session reported it: a
    /// plane branches on it where its CLI does (psql's `\l`).
    pub server_version: String,
    /// Who and where the session is (`:USER`, `:HOST`, `:PORT`): psql's
    /// `\c` keeps what it isn't told to change.
    pub user: String,
    pub host: String,
    pub port: String,
    /// What libpq knows of the session and the server doesn't say:
    /// whether it asked for a password, and the client's TLS library
    /// (psql's `\conninfo`).
    pub password_used: bool,
    pub ssl_library: String,
    /// The shell's variables: psql expands them in a command's arguments.
    pub variables: crate::parser::Variables,
    /// The session's transaction state as the server last reported it
    /// (libpq's `PQtransactionStatus`): psql's large-object commands open
    /// a transaction of their own outside one.
    pub txn: shared::TxnState,
}

/// What a CLI sends before a statement the user runs
/// (`DbmsPlane::statement_opening`): `queries`, in order, and whether
/// they set a savepoint to close once the statement has answered
/// (`DbmsPlane::statement_closing`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Opening {
    pub queries:   Vec<String>,
    pub savepoint: bool,
}

/// Where a CLI shows a command's time (`DbmsPlane::timing`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Timing {
    /// At the end of the footer or status line: mysql's "(0.002 sec)".
    Inline(String),
    /// A line of its own under the result: psql's "Time: 0.205 ms".
    OwnLine(String),
}

/// How a native CLI carries on when its connection to the server is
/// lost (`DbmsPlane::lost_connection`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LostConnection {
    /// No CLI behaviour ported: the shell's "Session ended" banner.
    Banner,
    /// The mysql CLI (`--reconnect`, on by default): nothing until the
    /// next statement, which prints `error` — unless a reconnect has
    /// already failed, so the CLI knows — and `notice`, opens a new
    /// session and runs there. A statement that finds the connection
    /// dead runs again in the new one.
    OnNextStatement { error: &'static str, notice: &'static str },
    /// psql: the command that found it lost prints `lost` under its
    /// error, and the connection is reset at once; the rest of the input
    /// runs on the new one. While there's none, every statement prints
    /// `not_connected`.
    ResetAtOnce { lost: &'static [&'static str], not_connected: &'static str },
}

/// How much of a server error to show — psql's `VERBOSITY` and
/// `SHOW_CONTEXT`. Planes without those knobs ignore it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ErrorStyle {
    pub verbosity:    Verbosity,
    pub show_context: ShowContext,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Verbosity {
    #[default] Default,
    Verbose,
    Terse,
    Sqlstate,
    /// Severity and primary message only, whatever `VERBOSITY` says —
    /// psql's `minimal_error_message`, for `\ef` / `\ev` lookups.
    Minimal,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ShowContext { Never, #[default] Errors, Always }

/// The next step of a staged meta-command ([`crate::MetaAction::Staged`]):
/// another batch of queries to run, or what to show.
pub(crate) enum MetaStep {
    Run(Vec<String>),
    /// A server function the CLI calls through libpq's fast path
    /// (`PQfn`: psql's `lo_unlink`), run as a query — which `ECHO_HIDDEN`
    /// neither shows nor, under `noexec`, holds back, as psql's doesn't.
    Call(String),
    Done(Vec<crate::ShellEntry>),
    /// The command failed — a server error, or nothing found where psql's
    /// function returns false (`Did not find any relation named "x".`):
    /// what it says. Nothing more follows it (`extra_arguments`).
    Failed(Vec<crate::ShellEntry>),
}

pub use mysql::MySqlPlane;
pub use postgres::PostgresPlane;

/// Per-DBMS behavior. See `vault/Architecture/DBMS Planes.md`.
pub trait DbmsPlane: Send + Sync {
    /// The `DbType` this plane serves. Currently unused by callers (they
    /// already know the type when they resolve the plane), but kept so a
    /// plane is always self-identifying for future diagnostics / debug.
    #[allow(dead_code)]
    fn db_type(&self) -> DbType;
    /// Human-readable engine name, e.g. "MySQL" / "PostgreSQL".
    fn name(&self) -> &'static str;

    // ── Connection form defaults / requirements ─────────────────────────

    fn default_port(&self) -> u16;
    fn default_user(&self) -> &'static str;
    /// True iff a target database must be specified on the connection.
    fn database_required(&self) -> bool;

    // ── Shell meta-command handling ────────────────────────────────────

    fn meta_cmd(&self, input: &str) -> Option<MetaAction>;
    /// [`DbmsPlane::meta_cmd`] for a command typed in a shell, knowing
    /// that shell's state (psql checks a pattern's database part
    /// against the one it's on).
    fn meta_cmd_in(&self, input: &str, _ctx: &MetaContext) -> Option<MetaAction> { self.meta_cmd(input) }
    /// A line typed while no statement is pending that the CLI reads as
    /// one of its named commands, under statement `delimiter` — the
    /// mysql CLI's `status`, `use db`, `delimiter //` (`find_command`),
    /// psql's `quit`. Default: none.
    fn named_command(&self, _line: &str, _delimiter: &str) -> Option<MetaAction> { None }
    /// A statement its delimiter ended that the CLI runs as one of its
    /// named commands instead of sending it — the mysql CLI's `status;`,
    /// `select 1; use db;` (`add_line`). Default: none; psql sends it.
    fn statement_command(&self, _statement: &str) -> Option<MetaAction> { None }
    /// The step after a staged meta-command's last batch: `replies` holds
    /// every batch's result so far, in order. Planes keep no state
    /// between steps — they read the command again.
    fn meta_continue(&self, _input: &str, _ctx: &MetaContext, _replies: &[shared::BatchResult]) -> MetaStep {
        MetaStep::Done(Vec::new())
    }
    fn help_lines(&self) -> Vec<String>;
    /// A server error as this dialect's CLI prints it, line by line.
    /// `query` is the statement the error's position counts into.
    fn format_error(&self, error: &shared::DbError, query: Option<&str>, style: ErrorStyle) -> Vec<String>;
    /// Environment variables the native CLI reads to pick an editor,
    /// in order — noctua tries them when neither the connection nor
    /// Settings names one.
    fn editor_env_chain(&self) -> &'static [EditorVar];
    /// Variable holding the native CLI's "open at line N" flag for an
    /// editor taken from the environment, if the CLI has one.
    fn editor_line_flag_var(&self) -> Option<LineFlagVar> { None }
    // ── Schema explorer ────────────────────────────────────────────────

    /// Object kinds this plane supports in the schema explorer, in the
    /// order they should appear in the type strip. MySQL exposes Events;
    /// Postgres doesn't, so it drops that kind.
    fn object_kinds(&self) -> &'static [ObjectKind];

    /// Admin areas this plane supports under the "Admin" type-strip tab.
    /// Order matches the per-scope grouping the explorer renders. Each
    /// plane returns only the areas it actually has — e.g. PG includes
    /// Tablespaces and Extensions; MySQL leaves those out.
    fn admin_areas(&self) -> &'static [AdminArea] { &[] }

    /// SQL listing roles for the Admin → Roles area. Each row's first
    /// column is the role name; remaining columns are listed by
    /// `role_list_extra_columns()` and shown as flat-list extras.
    /// Returns None for planes that don't surface a Roles area.
    fn list_roles_sql(&self) -> Option<&'static str> { None }

    /// Header labels for the Roles list's extra columns (everything past
    /// the always-present Name column). Order MUST match the SQL's column
    /// order from `list_roles_sql`.
    fn role_list_extra_columns(&self) -> &'static [&'static str] { &[] }

    /// SQL listing the privileges granted to a single role / user. The
    /// `name` is the role's primary identifier (PG: rolname, MySQL: User);
    /// `extras` mirrors `role_list_extra_columns()` so a plane can pull
    /// auxiliary identity bits (e.g. MySQL's Host as `extras[0]`). Returns
    /// None for planes that don't surface privileges.
    fn role_privileges_sql(&self, _name: &str, _extras: &[String]) -> Option<String> { None }

    /// Header labels for the Privileges-tab table. Order MUST match the
    /// `role_privileges_sql` column order. Empty slice means "this plane
    /// doesn't render privileges."
    fn role_privileges_columns(&self) -> &'static [&'static str] { &[] }

    /// Resolver query for the Privileges → "Show affected objects"
    /// sub-view. Takes the `Object` qualifiers of the currently-
    /// selected grant rows (extracted from the `role_privileges_sql`
    /// output) and returns one row per (schema, table) that any of
    /// those scopes expand to:
    ///
    ///   - `*.*` → every (schema, table) the connecting user can see
    ///   - `<db>.*` → every (schema, table) where `table_schema = db`
    ///   - `<db>.<tbl>` → that one row
    ///   - `<schema>` (no dot, PG-only) → every table in that schema
    ///
    /// Returned column aliases: `Schema`, `Object`. Returns None for
    /// planes that don't surface privileges (matching
    /// `role_privileges_sql`).
    fn role_affected_objects_sql(
        &self,
        _name: &str,
        _extras: &[String],
        _selected_objects: &[String],
    ) -> Option<String> { None }

    /// True iff `row` represents a superuser whose privileges are
    /// effectively unlimited regardless of what the grants table says.
    /// Drives the "Superuser bypasses privilege checks" banner on the
    /// Privileges tab. Default false for planes without the concept.
    fn role_is_superuser(&self, _row: &AdminRow) -> bool { false }

    /// SQL for the Object → Privileges matrix tab. Returns rows shaped
    /// to populate `ObjectGrantRow` (case-insensitive aliases:
    /// `Grantee`, `Privilege`, `Source`, `Chain`, `Grantable`):
    ///
    /// - `Grantee` — role / user name (the matrix's row label).
    /// - `Privilege` — single privilege type (the matrix's column).
    /// - `Source` — `direct` for an explicit grant on this object,
    ///   `inherited` for access gained via membership chains.
    /// - `Chain` — empty for direct grants; for inherited, the
    ///   member→ancestor chain joined by " → ".
    /// - `Grantable` — `YES` when the grant carries WITH GRANT OPTION.
    ///
    /// `kind` lets the plane gate: only Tables and Views are wired
    /// in v1 — other kinds return None. Returns None for planes that
    /// don't expose object grants.
    fn object_grants_sql(
        &self,
        _kind: ObjectKind,
        _schema: &str,
        _name: &str,
    ) -> Option<String> { None }

    /// Privilege types that label the matrix's columns for the given
    /// object kind. Order is the column order. Empty slice means
    /// "this plane doesn't render the privileges matrix for this
    /// kind" — the tab gates off.
    fn object_privilege_kinds(&self, _kind: ObjectKind) -> &'static [&'static str] { &[] }

    /// Privilege types for the schema-level matrix (USAGE / CREATE on
    /// Postgres; empty on MySQL where schema == database). Used by the
    /// AdminArea::Schemas "Privileges" section renderer to label matrix
    /// columns. Order is the column order.
    fn schema_privilege_kinds(&self) -> &'static [&'static str] { &[] }

    /// One-shot GRANT statement for a single (grantee, privilege) on a
    /// single object. Returns None if the plane doesn't support GRANT
    /// for that object kind. Caller wraps the result in `ws_dml` and
    /// tags the response so the matrix flashes green + refetches on
    /// success.
    fn grant_sql(
        &self,
        _kind: ObjectKind,
        _schema: &str,
        _name: &str,
        _grantee: &str,
        _privilege: &str,
    ) -> Option<String> { None }

    /// REVOKE counterpart to `grant_sql`. Same shape, same return
    /// semantics. Direct grants only — the renderer never wires this
    /// up for `owner` / `superuser` / `inherited` cells.
    fn revoke_sql(
        &self,
        _kind: ObjectKind,
        _schema: &str,
        _name: &str,
        _grantee: &str,
        _privilege: &str,
    ) -> Option<String> { None }

    /// Schema-level GRANT/REVOKE for the AdminArea::Schemas matrix.
    /// Separate from object-level because schemas aren't ObjectKind.
    /// MySQL leaves both at None — its database-level grant is a
    /// different surface (future phase).
    fn schema_grant_sql(
        &self,
        _schema: &str,
        _grantee: &str,
        _privilege: &str,
    ) -> Option<String> { None }
    fn schema_revoke_sql(
        &self,
        _schema: &str,
        _grantee: &str,
        _privilege: &str,
    ) -> Option<String> { None }

    /// SQL listing rows for a non-Roles admin area (Tablespaces /
    /// Databases / Extensions / ServerConfig). Returns None for areas
    /// the plane doesn't support — the renderer falls through to a
    /// "coming soon" placeholder. Roles is handled separately because
    /// its rows are click-to-detail.
    fn area_list_sql(&self, _area: AdminArea) -> Option<String> { None }

    /// Header labels for an admin area's flat-list table. Order MUST
    /// match the `area_list_sql` column order.
    fn area_list_columns(&self, _area: AdminArea) -> &'static [&'static str] { &[] }

    /// True iff a row in the given area is clickable for drill-in
    /// detail. When false the row is decorative-only and the inspector
    /// stays closed; when true, clicking sets the selected row and the
    /// inspector renders sections from `area_row_detail_*`.
    fn area_row_clickable(&self, _area: AdminArea) -> bool { false }

    /// Section names rendered inside the area-row detail inspector,
    /// in order. Each section is a sub-table with its own SQL +
    /// column headers (see `area_row_detail_sql` and
    /// `area_row_detail_columns`). Empty slice = no sections, no detail.
    fn area_row_detail_sections(&self, _area: AdminArea) -> &'static [&'static str] { &[] }

    /// SQL for one section of an area-row's detail view. `row_id` is
    /// the row's primary identifier (typically the Name column the
    /// list rendered). `section` is one of the strings returned by
    /// `area_row_detail_sections`.
    fn area_row_detail_sql(
        &self,
        _area:    AdminArea,
        _row_id:  &str,
        _section: &str,
    ) -> Option<String> { None }

    /// Header labels for a section's sub-table. Order MUST match the
    /// `area_row_detail_sql` column order.
    fn area_row_detail_columns(
        &self,
        _area:    AdminArea,
        _section: &str,
    ) -> &'static [&'static str] { &[] }

    /// SQL that returns one row per schema, with the schema name in the
    /// first column.
    fn list_schemas_sql(&self) -> &'static str;

    /// Schemas the auto-selector should skip over when picking the first
    /// schema to show. Used purely for the "pick a sane default schema"
    /// heuristic; the user can still select them manually.
    fn system_schemas(&self) -> &'static [&'static str];

    /// SQL to list objects of `kind`. `schema = None` means "all schemas".
    fn list_sql(&self, kind: ObjectKind, schema: Option<&str>) -> String;

    // Table-detail queries. All must return rows whose column names match
    // what the introspect handlers look up case-insensitively (see the
    // `status:` / `columns:` / `indexes:` / `ddl:` /
    // `relancestors:` / `reldescendants:` handlers in main.rs).
    fn table_status_sql(&self, schema: &str, name: &str) -> String;
    fn table_columns_sql(&self, schema: &str, name: &str) -> String;
    fn table_indexes_sql(&self, schema: &str, name: &str) -> String;
    fn table_ddl_sql(&self, schema: &str, name: &str) -> String;

    /// FK relationships pointing OUT of this table — i.e. parents (PK side)
    /// at depth 1 and grand-parents at depth 2. Aliases: `Depth`,
    /// `Ref_schema`, `Ref_table`. One row per (depth, referenced table).
    fn table_relationship_ancestors_sql(&self, schema: &str, name: &str) -> String;
    /// FK relationships pointing IN to this table — i.e. children (FK side)
    /// at depth 1 and grand-children at depth 2. Same alias contract as
    /// the ancestors query.
    fn table_relationship_descendants_sql(&self, schema: &str, name: &str) -> String;

    /// Direct partitions of this table. Aliases: `Partition_schema`,
    /// `Partition_name`, `Partition_bound`. Returns zero rows for non-
    /// partitioned tables. The bound is dialect-specific text (e.g. PG's
    /// `FOR VALUES FROM (...) TO (...)`).
    fn table_partitions_sql(&self, schema: &str, name: &str) -> String;

    /// TOAST section detail — one summary row + N TOASTABLE-column rows.
    /// Aliases: `Row_kind` (`"summary"` | `"column"`), plus discriminator
    /// `Kind` on the summary row (`"single"` | `"partition_aggregate"` |
    /// `"is_toast_of"` | `"none"`) and its per-kind fields. See
    /// `.claude/rules/plane.md` for the full alias table. MySQL returns
    /// a zero-row query since TOAST doesn't exist there.
    fn table_toast_sql(&self, schema: &str, name: &str) -> String;

    /// Visibility Map summary — one row per table with per-page all-
    /// visible / all-frozen counts and the source used to compute them.
    /// Aliases: `All_visible_pages`, `All_frozen_pages`, `Total_pages`,
    /// `Source` (`"pg_visibility"` | `"pg_class"`).
    ///
    /// `extensions` is the list from `SessionOpened.extensions` (probed
    /// once at connection open on noctua). PG plane consults it and
    /// picks its SQL path: emits `pg_visibility_map_summary()`-based
    /// SQL when `"pg_visibility"` is present (fresh per-page read,
    /// includes all-frozen), else falls back to `pg_class.relallvisible`
    /// (snapshot from last VACUUM, all-visible only, works PG 9.2+).
    ///
    /// **Do not** inline-CASE-gate a reference to
    /// `pg_visibility_map_summary()` — PG resolves function names at
    /// parse time, not runtime. The whole point of the caller-supplied
    /// `extensions` list is to make the SQL a plane-side branch, so
    /// the function name never appears in the query text on servers
    /// that don't have the extension.
    fn table_vm_sql(&self, schema: &str, name: &str, extensions: &[String]) -> String;

    /// Per-table autovacuum knob dump — one row per relevant
    /// autovacuum reloption. Powers the Storage tab's Autovacuum
    /// section. Aliases: `Setting`, `Effective_value`, `Source`
    /// (`"reloption"` | `"cluster"` | `"default"`), `Reloption_value`,
    /// `Cluster_value`.
    ///
    /// PG resolves each knob as: reloption if set, else cluster GUC,
    /// else a hardcoded default (for reloption-only knobs like
    /// `autovacuum_enabled` that have no GUC counterpart). Note the
    /// reloption↔GUC name mismatch: `autovacuum_freeze_min_age` (the
    /// reloption) corresponds to `vacuum_freeze_min_age` (the GUC,
    /// no `autovacuum_` prefix). Same for `_freeze_table_age` and
    /// the `_multixact_*` variants.
    ///
    /// MySQL emits a zero-row stub — no equivalent per-table
    /// autovacuum knob surface.
    fn table_autovac_sql(&self, schema: &str, name: &str) -> String;

    /// Bulk-fetch ALL columns of N tables in a single round trip. Used by
    /// the ERD tab's "All Fields" mode to render every column with PK/FK
    /// rows tinted, without firing N separate introspect queries.
    /// Aliases (case-insensitive lookup, same contract as the other detail
    /// queries): `Schema_`, `Table_`, `Field`, `Ord`, `Is_pk`. Returns one
    /// row per column, ordered by table then ordinal so the parser can
    /// stream-group without an extra sort. `Is_pk` is 1 when the column
    /// participates in the table's primary key, 0 otherwise.
    fn table_columns_bulk_sql(&self, tables: &[(String, String)]) -> String;

    /// True if partitions returned by `table_partitions_sql` are separately
    /// addressable tables (PG declarative partitioning — you can `SELECT
    /// FROM payment_p2022_01` directly). False when partitions are storage
    /// divisions inside one logical table (MySQL); the renderer emits plain
    /// text instead of a navigation link in that case. Default false so any
    /// new plane is safe by default.
    fn partitions_are_navigable(&self) -> bool { false }

    /// Single-cell UPDATE for the Data tab's inline editor. Quotes
    /// identifiers with the plane's native syntax and emits the value
    /// as a single-quoted SQL literal (with embedded quotes doubled),
    /// or the bare keyword NULL when `set_val` is the literal string
    /// "NULL". `pk_cols` and `pk_vals` are paired to build the WHERE
    /// clause; the caller MUST pass exactly the row's primary key
    /// columns and their string-encoded values.
    fn update_row_sql(
        &self,
        schema:   &str,
        table:    &str,
        set_col:  &str,
        set_val:  &str,
        pk_cols:  &[String],
        pk_vals:  &[String],
    ) -> String;

    /// INSERT for the Data tab's vertical insert form. `cols` and
    /// `vals` are paired (same length, same order). Caller MUST omit
    /// any column the user left blank so the DB-side default fires;
    /// the literal string "NULL" emits a bare SQL NULL keyword (same
    /// convention as `update_row_sql`). Quoting matches the plane's
    /// native style.
    fn insert_row_sql(
        &self,
        schema: &str,
        table:  &str,
        cols:   &[String],
        vals:   &[String],
    ) -> String;

    /// Single-row DELETE for the Data tab's row-delete action. Same
    /// pk-pair WHERE-clause contract as `update_row_sql` — caller
    /// passes the row's full primary key. The plane returns
    /// `DELETE FROM ... WHERE pk_cols... = pk_vals...` with native
    /// quoting; values follow the same SQL-literal escaping as the
    /// other DML methods (single quotes doubled; "NULL" → bare NULL).
    fn delete_row_sql(
        &self,
        schema:  &str,
        table:   &str,
        pk_cols: &[String],
        pk_vals: &[String],
    ) -> String;

    /// UPSERT for the Data tab's Insert form when the user has the
    /// "Upsert" toggle on. Same INSERT prefix as `insert_row_sql`
    /// followed by a plane-specific conflict clause:
    ///
    ///   PG:    `ON CONFLICT (pk_cols) DO UPDATE SET non_pk_col =
    ///           EXCLUDED.non_pk_col, …` (or `DO NOTHING` when the
    ///           caller filled only PK columns).
    ///   MySQL: `ON DUPLICATE KEY UPDATE non_pk_col = VALUES(non_pk_col), …`
    ///           (or the caller substitutes `INSERT IGNORE` when only
    ///           PK columns were filled — encoded plane-side).
    ///
    /// `pk_cols` is the table's primary key (the conflict target on
    /// PG, the SET-clause exclusion list on both planes). `cols` /
    /// `vals` are the same paired arrays as `insert_row_sql`. Caller
    /// MUST omit blank inputs (they fall back to DB defaults) before
    /// passing them in. Caller should only invoke this when
    /// `pk_cols` is non-empty — without a PK there is no conflict
    /// target.
    fn upsert_row_sql(
        &self,
        schema:  &str,
        table:   &str,
        cols:    &[String],
        vals:    &[String],
        pk_cols: &[String],
    ) -> String;

    /// Exact `SELECT COUNT(*)` for the Data tab's "Exact count" button.
    /// Honours the same `where_clause` semantics as `select_paged_sql`
    /// so the count reflects what the user is actually browsing.
    fn count_rows_sql(
        &self,
        schema:       &str,
        table:        &str,
        where_clause: Option<&str>,
    ) -> String;

    /// Paged SELECT for the Data tab. `order_by` is `(column, desc)`;
    /// None means natural order. `where_clause` is a raw SQL fragment
    /// to drop in after `WHERE` (e.g. "created_at > '2024-01-01' AND
    /// active") — pasted verbatim, by design, since this is a DBA-
    /// facing tool. Caller passes None or an empty fragment for no
    /// filter. Implementations must quote schema / table / column
    /// with the plane's native quoting since they come from arbitrary
    /// catalog data.
    fn select_paged_sql(
        &self,
        schema:       &str,
        table:        &str,
        limit:        usize,
        offset:       usize,
        order_by:     Option<(&str, bool)>,
        where_clause: Option<&str>,
    ) -> String;

    /// View columns — defaults to table_columns_sql (works in both MySQL and
    /// Postgres). Postgres overrides to add a Source column via view_column_usage.
    fn view_columns_sql(&self, schema: &str, name: &str) -> String {
        self.table_columns_sql(schema, name)
    }

    /// DDL query for a non-table object (view, procedure, etc.).
    fn object_ddl_sql(&self, kind: ObjectKind, schema: &str, name: &str) -> String;

    // ── Syntax highlighting ────────────────────────────────────────────

    /// Sorted slice of uppercase keywords — used by `highlight::tokenize_sql`
    /// for `binary_search` lookups, so **must stay sorted**.
    fn keywords(&self) -> &'static [&'static str];
    /// Subset of `keywords()` that introduce a top-level clause (SELECT,
    /// FROM, WHERE, JOIN, etc.). The tokenizer checks this slice first and
    /// emits `KeywordMajor` for hits so the query skeleton can render bolder
    /// than its decoration. Same sorted-slice invariant.
    fn major_keywords(&self) -> &'static [&'static str] { MAJOR_KEYWORDS_UNIVERSAL }

    /// Label rendered above the Schema Explorer's schema-picker
    /// dropdown. PG's natural concept is the schema (one DB has many
    /// schemas); MySQL collapses the two — `mysql.user` connects to a
    /// database which IS the schema. Pre-fix the label was hard-coded
    /// to "Schema" everywhere, which read wrong on MySQL where the
    /// dropdown is populated from `SHOW DATABASES`. Audit finding 5
    /// of issue #10.
    fn schema_picker_label(&self) -> &'static str { "Schema" }

    /// Word-shaped operators (UNION, INTERSECT, EXCEPT). Spelled like
    /// keywords but semantically operators between result sets — emitted as
    /// `Operator` so they share the operator color. Same sorted-slice
    /// invariant.
    fn set_operators(&self) -> &'static [&'static str] { SET_OPERATORS_UNIVERSAL }
    /// Sorted slice of uppercase built-in function names (same invariant).
    fn functions(&self) -> &'static [&'static str];
    /// Sorted slice of meta-command names the plane recognizes —
    /// backslash-prefixed verbs typed in the shell (e.g. `\dt`, `\dnS`).
    /// Used by tab completion to enumerate candidates when the cursor
    /// sits after a leading `\`. Default is `&[]` so planes without a
    /// meta-command vocabulary opt in silently.
    fn meta_command_names(&self) -> &'static [&'static str] { &[] }
    /// SQL that lists every table / view / materialized view visible to
    /// the current session, as 2 columns aliased `Schema_` and `Table_`
    /// (matching the column-alias contract). Used by the shell's tab
    /// completion to populate the per-pane table cache. Excluding the
    /// system catalogs that aren't useful interactively (pg_toast on PG;
    /// nothing on MySQL since `information_schema` etc. are useful for
    /// completion) is the plane's call.
    fn completion_tables_sql(&self) -> String;
    /// Schema name to scope unqualified `FROM <Tab>` completion to.
    /// MySQL conflates schema and database, so the default just returns
    /// `current_db`. PG keeps the search-path concept separate from the
    /// connected database — its impl returns `"public"` regardless of
    /// `current_db`. Empty return → caller flattens every cached schema.
    fn default_completion_schema(&self, current_db: &str) -> String {
        current_db.to_string()
    }
    /// SQL that returns the column names of one specific table, as a
    /// single column aliased `Column_` in physical (ordinal) order.
    /// Used by tab completion's column popup. Identifier values come
    /// from the plane's existing table cache (PG `pg_class` / MySQL
    /// `information_schema.TABLES`) so they're already trusted as
    /// identifier shape — no extra quoting beyond what the impl does.
    fn completion_columns_sql(&self, schema: &str, table: &str) -> String;
    /// How the native CLI's lexer reads SQL: its quotes and comments,
    /// where its statements end and its backslash commands start.
    fn lexicon(&self) -> Lexicon;
    /// sqlparser dialect used by the on-the-fly beautifier. Returned as
    /// `&'static dyn` so it slots straight into `sqlparser::parser::Parser::parse_sql`.
    fn sql_dialect(&self) -> &'static dyn sqlparser::dialect::Dialect;
    /// What the native CLI prints for a statement that returns rows
    /// but returned none, instead of the column header: the mysql CLI's
    /// "Empty set". None: psql prints the header over "(0 rows)" — the
    /// result has its columns.
    fn empty_set(&self) -> Option<&'static str>;

    /// What the native CLI prints itself for a terminator that ends
    /// nothing — `;` alone, `select 1;;`, mysql's `\g` or `\G;`: the
    /// mysql CLI's `ERROR: ` / `No query specified`, in order with the
    /// input's other statements. None: the CLI sends it, as psql sends
    /// what its buffer holds, even nothing — the server answers an empty
    /// query with nothing.
    fn empty_statement(&self) -> Option<&'static [&'static str]> { None }

    /// The CLI expands `:name` variables in what's typed — statements
    /// and its commands' arguments (psql). The mysql CLI has none.
    fn interpolates_variables(&self) -> bool { false }

    /// How much of `command` — the input from a command on — the
    /// command takes, under statement `delimiter`; what follows is typed
    /// text again, left in the input after the buffer the command
    /// leaves. psql's commands take their line (the default); the mysql
    /// CLI's without arguments take `\` and their letter (`\s from dual;`,
    /// `\s;`), those with arguments run to the delimiter (`\u db; select
    /// 1;`).
    fn command_extent(&self, command: &str, _delimiter: &str) -> usize {
        command.find('\n').unwrap_or(command.len())
    }

    /// Under `\x auto`, whether a result shows vertically printed with
    /// `options` — in a pane `options.columns` characters wide (or
    /// `\pset columns`): psql's printer does when its aligned table,
    /// drawn as `\pset border` says, would be wider. Default: never (no
    /// `auto`).
    fn expands_automatically(&self, _columns: &[String], _rows: &[Vec<String>], _options: &crate::types::PrintOptions) -> bool { false }

    /// A result's rows as the CLI prints them vertically (psql's `\x` /
    /// `\gx` records, the mysql CLI's `\G` rows), a line each — without
    /// the footer the shell adds under them (`rows_footer`), as `style`
    /// says where the CLI reads it: the title over them (psql's `\pset
    /// title`, a list's), psql's `\t` (the records unlabelled), its
    /// `\pset border` and `linestyle`.
    fn print_vertical(&self, columns: &[String], rows: &[Vec<String>], style: &crate::types::PrintedWith) -> Vec<String>;

    /// What the CLI sends before a statement the user runs — given the
    /// session's transaction state and the shell's variables — psql's
    /// `SendQuery`: `BEGIN` under `AUTOCOMMIT off` outside a transaction
    /// (not before one that can't run in one), then, in a transaction,
    /// its savepoint under `ON_ERROR_ROLLBACK`. Default: nothing.
    fn statement_opening(&self, _sql: &str, _txn: shared::TxnState, _vars: &crate::parser::Variables) -> Option<Opening> { None }

    /// What the CLI sends once a statement it set a savepoint for has
    /// answered — given the transaction state then and the statement's
    /// command tag (psql: `ROLLBACK TO` its savepoint in a failed
    /// transaction; `RELEASE` in one still open, unless the statement
    /// ended or replaced it). Default: nothing.
    fn statement_closing(&self, _txn: shared::TxnState, _tag: Option<&str>) -> Option<String> { None }

    /// The query that names a described statement's result columns'
    /// types, as the CLI asks it (psql's `\gdesc`: `format_type` over
    /// each column's type and modifier); None where the CLI has no
    /// `\gdesc`.
    fn described_columns_sql(&self, _columns: &[shared::DescribedColumn]) -> Option<String> { None }

    /// A number in a right-aligned column as the CLI writes it in the
    /// client's `locale` (psql's `\pset numericlocale`); only psql's
    /// shells ask. Default: as it is.
    fn localized_number(&self, value: &str, _locale: &shared::NumericLocale) -> String { value.to_string() }

    /// What the CLI shows for SQL NULL until told otherwise: psql's
    /// `\pset null` starts empty; the mysql CLI prints `NULL`.
    fn null_display(&self) -> &'static str { "NULL" }

    /// The encoding the session's client speaks, as the CLI names it
    /// (psql's `:ENCODING`). Default: none said.
    fn client_encoding(&self) -> Option<&'static str> { None }

    /// How the CLI lines up a column of this type (`QueryResult.types`:
    /// the type as the server identifies it) in its tables — psql's
    /// `column_type_alignment`, the mysql CLI's `IS_NUM`: numbers right.
    /// Default: left.
    fn column_align(&self, _type_id: &str) -> crate::types::Align { crate::types::Align::Left }

    /// Whether the CLI centres a table's headers (psql) rather than
    /// starting them at the left (the mysql CLI). Default: no.
    fn centres_headers(&self) -> bool { false }

    /// A result as the CLI prints it in `options.format` when that isn't
    /// the shell's grid (psql's `\pset format unaligned`, `csv`, `html`
    /// …): its lines, `title` where the format puts one and `footers` (a
    /// list's own, psql's `myopt.footers`) in place of the row count.
    /// `types` is each column's type as the server identifies it
    /// (`QueryResult.types`); NULLs already read as the null display.
    /// None: the grid shows it. Default: always the grid.
    fn print_result(&self, _columns: &[String], _types: &[String], _rows: &[Vec<String>], _title: Option<&str>, _footers: &[String], _options: &crate::types::PrintOptions) -> Option<Vec<String>> { None }

    /// `\e` on an empty buffer edits the previous query; whether it
    /// stays in the input when it comes back unchanged — the mysql CLI's
    /// does; psql drops it (`discard_on_quit`).
    fn keeps_unchanged_recall(&self) -> bool { false }

    /// What the CLI prints as it reads the line just entered — the
    /// input's last — under statement `delimiter`, about text that stays
    /// in the statement: the mysql CLI's `ERROR: ` / `Unknown command
    /// '\i'.` for each backslash command it doesn't know, psql's `Use \q
    /// to quit.` for a `quit` typed while a statement is pending. Default:
    /// nothing.
    fn line_notes(&self, _input: &str, _delimiter: &str) -> Vec<crate::ShellEntry> { Vec::new() }

    /// What the CLI says, once one of its commands has run without
    /// failing, about each argument it didn't read: psql's `\x: extra
    /// argument "a" ignored`. Default: nothing (the mysql CLI drops them).
    fn extra_arguments(&self, _command: &str) -> Vec<String> { Vec::new() }

    /// What `\p` prints (`MetaAction::PrintBuffer`), line by line: the
    /// query buffer — `typed`, the text before the command as typed since
    /// the last statement ended (a named command run as a statement:
    /// that statement, mysql's `print;`) — or, where the CLI falls back
    /// to it, the `previous` query as it keeps it (`previous_buffer`).
    fn print_buffer(&self, typed: &str, previous: Option<&str>, ctx: &MetaContext) -> Vec<String>;

    /// The previous query as the CLI keeps it — what `\e` edits and `\p`
    /// prints when the buffer is empty — from the last statement sent:
    /// its SQL and its terminator as typed. Default: the statement alone,
    /// as the mysql CLI's `old_buffer` holds it.
    fn previous_buffer(&self, sql: &str, _terminator: &str) -> String { sql.to_string() }

    /// What the previous query becomes after `command` — one of the CLI's
    /// own, as the scrollback records it, `terminator` the delimiter of
    /// one typed as a statement; an empty one is an empty statement the
    /// CLI answered itself — when that changes it. Default: nothing does.
    fn previous_buffer_after(&self, _command: &str, _terminator: &str) -> Option<String> { None }

    /// What the native CLI prints for a statement that returns no rows,
    /// line by line: psql the server's command tag (`UPDATE 3`, `CREATE
    /// TABLE`), the mysql CLI "Query OK, N rows affected[, W warnings]"
    /// and the server's info line under it (`Rows matched: 1  Changed: 0
    /// Warnings: 0`) — in the output format the shell is in (psql's
    /// `PrintQueryStatus`: a paragraph under `\pset format html`). The
    /// shell adds the time to the first line.
    fn command_status(&self, status: &shared::CommandStatus, format: crate::types::OutputFormat) -> Vec<String>;

    /// The query the CLI asks the server for a statement's warnings
    /// with, when it shows them (mysql's `print_warnings`: `show
    /// warnings`). Default: none — the CLI doesn't.
    fn warnings_query(&self) -> Option<&'static str> { None }

    /// The lines the CLI prints of that query's rows after a statement
    /// that failed with `error` (or none). Default: none.
    fn warning_lines(&self, _set: &shared::ResultSet, _error: Option<u16>) -> Vec<String> { Vec::new() }

    /// Whether the CLI also prints a statement's status after the rows
    /// it returned (psql's `PrintQueryResult`: an INSERT / UPDATE /
    /// DELETE / MERGE … RETURNING's tag, under its rows). Default: no.
    fn status_after_rows(&self, _status: &shared::CommandStatus) -> bool { false }

    /// The line under a result's rows — how many, and any warnings — as
    /// the CLI words it (mysql: "2 rows in set, 1 warning", "Empty set";
    /// psql: "(2 rows)"); `vertical`: the rows were shown one field per
    /// line. None: the CLI prints none (psql's expanded output). Default:
    /// Parliament's "2 rows".
    fn rows_footer(&self, rows: usize, _warnings: u16, _vertical: bool) -> Option<String> {
        Some(format!("{rows} row{}", if rows == 1 { "" } else { "s" }))
    }

    /// A command's elapsed time as the CLI shows it, and where. Default:
    /// "(2 ms)" after the footer.
    fn timing(&self, exec_time_us: u64) -> Timing { Timing::Inline(format!("({} ms)", exec_time_us / 1000)) }

    /// Whether the CLI shows times before anything asks it to (mysql
    /// always does; psql only after `\timing`).
    fn timed_by_default(&self) -> bool { true }

    /// How the native CLI carries on when its connection to the server
    /// is lost; a shell does the same (`shell::send_to`, and main.rs's
    /// QueryError / SessionClosed / SessionOpened / SessionError).
    fn lost_connection(&self) -> LostConnection { LostConnection::Banner }

    /// What the CLI prints once it has replaced a lost session (`ok`), or
    /// failed to.
    fn reconnected_notice(&self, _ok: bool, _connection_id: &str, _database: &str) -> Vec<String> { Vec::new() }

    /// What the CLI prints as it reads a database's names for completion
    /// on connecting to it — the mysql CLI's `connect db`
    /// (`build_completion_hash`). Default: nothing.
    fn completion_notice(&self) -> Vec<String> { Vec::new() }

    /// A statement that switches the session's database, as the CLI
    /// treats it: the database, and what the CLI prints in place of the
    /// server's status (mysql's `use` — "Database changed"). None for
    /// anything else (psql switches with `\c`, a new session).
    fn switches_database(&self, _sql: &str) -> Option<(String, &'static str)> { None }

    /// What the CLI prints when a switch to another database (psql's
    /// `\c`) has opened its session: `database` as `user`, and — when
    /// there was no session before — on `host` at `port`.
    fn connected_notice(&self, _database: &str, _user: &str, _host: &str, _port: &str, _had_session: bool) -> Vec<String> { Vec::new() }

    /// What the CLI prints when that switch couldn't connect (`error`:
    /// the server's fields, if it answered), and whether it kept the
    /// session it had.
    fn connect_failed(&self, _host: &str, _port: &str, _error: Option<&shared::DbError>, message: &str, _kept: bool) -> Vec<String> {
        vec![message.to_string()]
    }

    // ── Activity pane (live sessions / blockers / locks) ──────────────

    /// Non-shell pane types this DBMS supports, in the order they appear
    /// in the ⊡ dropdown menu. Shell is always available and not listed
    /// here — it's spawned from the connection sidebar, not the dropdown.
    fn supported_panes(&self) -> &'static [PaneMenuItem];

    /// SQL that returns one row per active session. Column aliases must
    /// match what the `activity_sessions` handler looks up case-insensitively:
    /// "Pid", "User", "Client", "DB", "State", "Time", "Query".
    fn sessions_sql(&self, hide_idle: bool) -> String;

    /// SQL that returns one (waiter_pid, blocker_pid) edge per row.
    /// Column aliases: "Waiter_pid", "Blocker_pid".
    fn blockers_sql(&self) -> String;

    /// SQL that returns one row per held lock. Column aliases:
    /// "Pid", "Mode", "Object", "Granted", "Duration".
    ///
    /// `server_version` is the raw string returned by the server (PG's
    /// `current_setting('server_version')`, MySQL's `VERSION()`).
    /// Planes parse it to pick the right system view: MySQL 8.0+ uses
    /// `performance_schema.data_locks`, 5.x falls back to
    /// `information_schema.INNODB_LOCKS`. PG ignores the parameter
    /// because `pg_locks` is stable across supported PG versions.
    /// Audit cluster D finding 3.6 / issue #21 item 6.
    fn locks_sql(&self, server_version: &str) -> String;

    /// Whether this plane supports killing queries via a soft-kill action.
    /// Default: true. Override to return false for read-only or restricted
    /// planes.
    fn supports_session_kill(&self) -> bool { true }

    /// SQL to soft-cancel a specific session's current query. Not called
    /// when `supports_session_kill()` returns false. The parameter is the
    /// string form of the session's server-side identifier (MySQL thread
    /// id / Postgres backend pid).
    fn kill_query_sql(&self, pid: &str) -> String;
}

// ── Singleton planes ──────────────────────────────────────────────────

static MYSQL_PLANE: MySqlPlane = MySqlPlane;
static POSTGRES_PLANE: PostgresPlane = PostgresPlane;

/// Resolve the plane for a given `DbType`.
pub fn plane_for(t: DbType) -> &'static dyn DbmsPlane {
    match t {
        DbType::MySql => &MYSQL_PLANE,
        DbType::Postgres => &POSTGRES_PLANE,
    }
}

// Universal clause-introducing keywords shared by every dialect. Sorted —
// the tokenizer does `binary_search` on this slice. Planes can override
// `major_keywords()` to add dialect-specific clause verbs (SHOW, EXPLAIN,
// COPY, VACUUM, …); until they do, this default applies everywhere.
static MAJOR_KEYWORDS_UNIVERSAL: &[&str] = &[
    "ALTER", "BEGIN", "BY", "COMMIT", "CREATE", "CROSS", "DELETE", "DROP",
    "FROM", "FULL", "GRANT", "GROUP", "HAVING", "INNER", "INSERT",
    "INTO", "JOIN", "LATERAL", "LEFT", "LIMIT", "NATURAL",
    "OFFSET", "ORDER", "OUTER", "RECURSIVE", "RETURNING", "REVOKE", "RIGHT",
    "ROLLBACK", "SAVEPOINT", "SELECT", "SET", "START", "TRUNCATE",
    "UPDATE", "VALUES", "WHERE", "WITH",
];

// Word-shaped operators — see `DbmsPlane::set_operators`. Tokenized as
// `Operator` (operator-colored) rather than as keywords so they sit
// visually between the two SELECTs they connect.
static SET_OPERATORS_UNIVERSAL: &[&str] = &[
    "EXCEPT", "INTERSECT", "UNION",
];
