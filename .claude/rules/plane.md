---
description: Rules for plane-layer SQL (column-alias contract, dialect quirks, DBMS-specific catalog idioms)
paths:
  - tyto/src/plane/**
---

# Plane-layer rules (`tyto/src/plane/**`)

This directory holds per-DBMS planes. The backend is a DBMS-agnostic SQL relay — **all dialect awareness lives here**. Don't propose backend-side branching on DBMS.

## The column-alias contract (non-negotiable)

The introspect parser in `tyto/src/main.rs` (under `ServerMessage::IntrospectResult`) does **case-insensitive name lookups**, not positional. Every plane query must AS-alias its output columns to match what the parser expects. The parser is single-sourced on purpose; planes differ in SQL but never in parsing.

| Handler branch | Required aliases |
|---|---|
| `schemas` | first column → schema name |
| `objects:*` | col 0 → schema name, col 1 → object name. Remaining columns dump into `extras` in their original column order — **except** `Parent_schema` / `Parent_name` (PG Tables only), peeled off by case-insensitive name lookup and routed to `ObjectRow.parent_schema` / `parent_name` for the outer-list partition-tree nesting. Planes that don't emit those columns simply get `None`. |
| `status:*` | `Engine`, `Collation`, `Rows`, `Data_length`, `Index_length`, `Avg_row_length`, `Auto_increment`, `Create_time`, `Update_time`, `Comment`, `Partition_count`, `Storage_factor` (decimal string e.g. `"1.82"`; both planes emit, empty when uncomputable), `Storage_actual_bytes` (numerator-bytes the factor refers to), `Page_count` (the same numerator divided by page size), `Page_count_ideal` (theoretical-minimum page count; empty when uncomputable). On PG partitioned parents (`c.relkind = 'p'`), `Storage_factor` / `Storage_actual_bytes` / `Page_count` / `Page_count_ideal` are all rolled up as sums across leaf partitions via `pg_partition_tree` — the parent's own `relpages` is 0 since it owns no data, so a rollup gives the DBA a real number instead of a wall of zeros. Non-partition-parent tables use their own `relpages` scalar as before. PG-only: `Owner`, `Parent_table`, `Parent_schema`, `Partition_bound`, `Last_vacuum`, `Cache_hit_ratio` (decimal percentage from `pg_statio_user_tables`), `Heap_blks_hit` / `Heap_blks_read` (raw counts for the cache-hit tooltip), `Idx_blks_hit` / `Idx_blks_read` (index-block counterparts; the parser sums heap + idx into a single Block IO magnitude cell with the split preserved in the tooltip), `Last_analyze` (timestamp of the most recent manual or auto ANALYZE from `pg_stat_user_tables`), `Idx_scan_pct` (decimal percentage of `idx_scan / (idx_scan + seq_scan)`; empty when both counters are 0), `Idx_scan` / `Seq_scan` (raw counts for the access-pattern tooltip), `Crud_c` / `Crud_r` / `Crud_u` / `Crud_d` (four separate raw integer counters — `n_tup_ins`, `seq_tup_read + idx_tup_fetch`, `n_tup_upd`, `n_tup_del` — that the parser collapses into a single `"C / R / U / D"` display cell with a spelled-out tooltip; R is tuple-return volume, not query count), `Dead_tup_pct` (percentage of dead vs live+dead tuples from `pg_stat_user_tables`; severity ≤5% green / 5-20% yellow / >20% red — mirrors PG's `autovacuum_vacuum_scale_factor` default), `N_live_tup` / `N_dead_tup` (raw counts for the dead-tuples tooltip), `Toast_size` (bytes; `pg_total_relation_size(reltoastrelid)` — includes TOAST heap + TOAST index; empty when the table has no TOAST relation or the TOAST is empty; on partitioned parents rolls up as SUM across leaf partitions via `pg_partition_tree`), `Toast_pct` (percentage `TOAST / (heap + TOAST)` — sum-of-sums on partitioned parents; the parser composes both into a single `"1.8 GB (42%)"` display cell), `Freeze_age_pct` (integer percentage `age(relfrozenxid) * 100 / LEAST(reloption, autovacuum_freeze_max_age)` — PG ignores a per-table freeze max age larger than the cluster value; how close the table is to triggering an anti-wraparound vacuum; relation-level catalog math only, no page scans; rolled up as MAX across leaves on partitioned parents since the worst leaf drives urgency; empty when relfrozenxid = 0), `Freeze_age_raw` (raw `age(relfrozenxid)` for the tooltip, MAX-rolled on parents). MySQL omits the PG-only fields and the header-grid skips empties. |
| `columns:*` | `Field`, `Type`, `Null`, `Default`, `Extra`, `Comment` |
| `indexes:*` | `Key_name`, `Non_unique`, `Index_type`, `Index_size`, `Column_name`, `Seq_in_index`. PG also emits `Inherited` (`0`/`1`), `Storage_factor` (btree-only; non-btree returns empty), `Storage_actual_bytes`, `Index_size_ideal` (btree-only theoretical-minimum bytes), `Scan_count` (from `pg_stat_user_indexes.idx_scan`; integer-as-text), `Tup_per_scan` (idx_tup_read / idx_scan; decimal-as-text, empty when scan_count is 0). MySQL omits all six (no per-index decomposition / no equivalent stats view in InnoDB). |
| `ddl:*` / `objddl:*` | any column name starting with `"Create "` (or `"SQL Original Statement"` for legacy MySQL triggers) |
| `relancestors:*` / `reldescendants:*` | `Depth`, `Ref_schema`, `Ref_table`, `From_columns`, `To_columns`, `Via_schema`, `Via_table` |
| `partitions:*` | `Partition_schema`, `Partition_name`, `Partition_bound`, optional `Partition_rows`, `Partition_data_length` |
| `autovac:*` | Self-contained autovacuum dump (knobs + counters in one snapshot), discriminated by `Row_kind`. **`setting` rows** (one per knob, ordered by `Ord`): `Setting` (reloption name, or GUC name for the cluster-only switches `autovacuum` / `track_counts`), `Effective_value` (reloption if set → cluster GUC → built-in default), `Source` (`"reloption"` \| `"cluster"` = GUC changed from boot default via `pg_settings.source` \| `"default"`), `Reloption_value`, `Cluster_value`, `Fallback_value` (the `vacuum_cost_*` GUC a `-1` cost setting defers to), `Unit` (`pg_settings.unit`). **`counters` row** (one): `Relkind`, `Reltuples` (-1 = never analyzed), `Relpages`, `Relallfrozen` (PG 18+), `N_dead_tup`, `N_ins_since_vacuum` (PG 13+), `N_mod_since_analyze`, `Xid_age`, `Mxid_age`. Knobs the server doesn't have (insert_* before PG 13, `vacuum_max_threshold` before PG 18) are dropped by the WHERE; version-dependent columns are read via `to_jsonb(row) ->> 'col'` so a missing column is NULL, not a parse error. Zero rows when the relation doesn't exist. Reloption↔GUC name mismatch is spelled out in a VALUES CTE (`autovacuum_freeze_min_age` reloption ↔ `vacuum_freeze_min_age` GUC; same for `_freeze_table_age` and the `_multixact_*` pair). Counters come from `pg_stat_all_tables` so TOAST relations are covered. Presentation rules (override / clamp / -1 semantics) live in `tyto/src/autovac.rs`, not the SQL. MySQL emits a zero-row stub. |
| `vm:*` | Visibility Map summary — one row: `Total_pages`, `All_visible_pages`, `All_frozen_pages` (empty when the source doesn't report it), `Source` (`"pg_visibility"` \| `"pg_class"`), `Relkind`, `Rel_count` (leaf partitions summed when `Relkind = 'p'`). Zero rows when the relation doesn't exist. Signature: `table_vm_sql(&self, schema, name, extensions: &[String]) -> String`; `extensions` is the session's `SessionOpened.extensions` list, handed to the explorer's loaders via the `ExplorerSession { id, extensions }` handle (not Leptos context — `use_context` returns None in click handlers). With `"pg_visibility"` in the list the plane emits `pg_visibility_map_summary()` per relation and uses the live `pg_relation_size / block_size` as the total; otherwise `pg_class.relpages` / `relallvisible` / `relallfrozen` (the last via `to_jsonb(c) ->> 'relallfrozen'`, PG 18+). Partitioned parents are summed over `pg_partition_tree` leaves on both paths (`pg_visibility_map_summary` rejects relkind `p`). **Do not** inline-CASE-gate an extension function reference — PG resolves function names at parse time, so an untaken branch still errors; branch in Rust so the name is absent from the SQL (unit-tested in `postgres.rs`). MySQL emits a zero-row stub. |
| `toast:*` | `Row_kind` (`"summary"` \| `"column"`) discriminates row shape. Summary row: `Kind` (`"single"` \| `"partition_aggregate"` \| `"is_toast_of"` \| `"none"`) plus per-kind fields: `Toast_schema`, `Toast_name`, `Heap_bytes`, `Index_bytes`, `Rows_est`, `Storage_ratio` (single) / `Partition_count`, `Total_toast_bytes`, `Largest_schema`, `Largest_name`, `Largest_bytes` (partition_aggregate) / `Parent_schema`, `Parent_name` (is_toast_of). Column rows: `Column_name`, `Attstorage` (already decoded server-side into `"EXTENDED"` \| `"EXTERNAL"` \| `"MAIN"` — PLAIN columns filtered out). Empty column list on `is_toast_of` and on tables with no TOASTABLE columns. MySQL emits a zero-row query. |
| `edit_to_input` (`\ef` / `\ev`, PG) | One row: `Src` (the definition to edit), `Line` (editor line, integer; NULL for none), `Error` (psql-style refusal such as `"s.n" is not a view`). Read by `shell::parse_edit_lookup`. noctua sends SQL NULL as the text `"NULL"`, so text columns tested for presence (`Src`, `Error`) must be `''` when absent — `coalesce(…, '')` — not NULL. |
| `erdcols:*` | `Schema_`, `Table_`, `Field`, `Ord`, `Is_pk` (bulk-fetch all columns of N tables for the ERD All-Fields mode; one row per column, ordered by `Schema_`, `Table_`, `Ord`) |

The `(pk)` tag lights up when `Key_name = "PRIMARY"` (case-insensitive). PG queries must emit `CASE WHEN indisprimary THEN 'PRIMARY' ELSE i.relname END AS "Key_name"`.

**If plane SQL can't fit the parser's shape, change the SQL — not the parser.**

The contract governs the shared parser in main.rs. Staged meta-commands (`MetaAction::Staged` → `IntrospectBatch` → `meta_continue`) are read by the plane that wrote them: psql's `\d` (`plane/postgres/describe.rs`) reads its result sets by position, as psql's `PQgetvalue` does, with SQL NULL as `None` — its parser sits next to its SQL.

## Dialect purity per plane (non-negotiable)

Each plane handles **only its own native dialect's verbs**. No cross-CLI shortcuts in either direction. A Postgres connection behaves exactly like `psql`; a MySQL connection behaves exactly like the `mysql` CLI.

Concrete examples (the audit at PR #62 cleaned up the violations that had drifted in):

| Verb | Native to | The other plane MUST |
|---|---|---|
| `\password` | psql | not recognize — not even as a "use ALTER USER … IDENTIFIED BY" refuse-arm |
| `\lo_list` / `\lol` / `\lo_unlink` / `\lo_import` / `\lo_export` | psql | not recognize — not even as a "use BLOB / LONGBLOB" pointer |
| `\crosstabview`, `\gdesc`, `\gexec`, `\gset`, `\if` / `\elif` / `\else` / `\endif`, `\prompt`, `\pset`, `\set` predefined vars, `\watch` named args, `\copyright`, `\errverbose`, `\encoding`, `\d*` family, `\a`/`\C`/`\f`/`\H`/`\t`/`\T` format toggles, `PROMPT1` `%`-codes | psql | not recognize |
| `\gx` (one-shot vertical), `\x` (sticky-vertical toggle) | psql | not recognize |
| `\ef` / `\ev`, `\e [line]` / `\e <file>` | psql | not recognize `\ef` / `\ev`; its own `\e` takes no arguments (mysql CLI) |
| `edit` (long form of `\e`) | mysql CLI | not recognize |
| `\G` (per-statement vertical terminator) | mysql CLI | not recognize — even at the shell-layer terminator strip |
| `\u <db>` (use database), `\d <delimiter>` (set delimiter), `\W` / `\w` (warnings), `\P` / `\p` (pager), `\.` (source), `\!` (system) | mysql CLI | not recognize |

**Why:** Cross-dialect courtesies (refuse-arms that say "this is psql-specific, use X on MySQL") pollute one plane's language surface with the other's vocabulary. They also hide the fact that the wrong verb was typed — silent fall-through forces the user to notice they're on the wrong dialect, which is the correct signal.

When implementing a psql feature, edit ONLY `tyto/src/plane/postgres.rs` and its modules under `tyto/src/plane/postgres/` (`describe.rs`, `lists.rs`, `pattern.rs`, `print.rs`, `error.rs`, `conninfo.rs`, `encoding.rs`, `large_object.rs`). When implementing a mysql-CLI feature, edit ONLY `tyto/src/plane/mysql.rs`. Do not bridge.

### Shell-layer dialect-specific behaviors

Some behaviors aren't pure meta-commands — they live partly in `tyto/src/shell.rs` and `tyto/src/parser.rs` (e.g. mysql's `\G` is a statement terminator, not a meta-command). For those: add a trait method on `DbmsPlane` and route the shell-layer behavior through it.

`lexicon()` names the native CLI's lexer (`parser::Lexicon::Psql` / `Mysql`): its quotes and comments (psql: `E'…'` escapes, `$tag$` bodies, nested `/* */`; mysql: backslash escapes, `#`, `-- `, `/*!` as code, a backslash outside quotes with the character it takes), where its statements end (mysql: `\g` / `\G` too) and where a backslash command starts — on the input's last line, outside quotes and comments, psql's at the first backslash, the mysql CLI's at the first that's one of its commands (`\` + one of its letters; `\N` and an unknown `\i` stay in the statement) (`parser::trailing_command`), so `select 1 \gx` works on the SQL before it. Planes dispatch the command found there (any of theirs; MySQL's `\g` / `\G` are the splitter's), taking what `command_extent(command, delimiter)` says (psql: its line; mysql: `\` and its letter, or for one with arguments its line up to the delimiter after them — text after it reads on, a `;` included: `\s;` is `\s` and an empty statement; the command reads its arguments from the whole line, `\u db;` asking for database `db;`), and the action says whether it works on the buffer before it (`MetaAction::works_on_buffer`: the `\g` family, `\watch`, `\gdesc`, the editor, a clear) or leaves it in the input, as psql does (`select 1 \x`); `shell.rs` splits the input the same way (`split_buffer`, `buffer_statements`). What the CLI says about text on a line that stays in the statement is `line_notes()` (the input's last line, read with the lines before it; the shell asks it for every line, as the CLI reads each — `shell::notes_for`) (mysql's `Unknown command '\i'.`, `parser::mysql_unknown_commands`; psql's `Use \q to quit.` for an assistance word typed while a statement is pending), said when the statement holding the line is sent, or at once while it waits (`TypedStatement.notes`, `ShellTab.held_notes`). A plane checks the line itself first and reads the lines before it only when it could note it. psql's `\if` family is the PG plane's `MetaAction::Branch` (the shell keeps the conditional stack, `psql_if`); on MySQL `\if` is that unknown `\i`.

Also: `editor_env_chain()` / `editor_line_flag_var()` — the environment variables the native CLI reads for `\e` (psql `PSQL_EDITOR` → `EDITOR` → `VISUAL` + `PSQL_EDITOR_LINENUMBER_ARG`; mysql `EDITOR` → `VISUAL`). `\e` exists in both CLIs with different argument rules, so each plane parses its own — that's two native verbs, not a cross-CLI shortcut.

Reference implementation: `centres_headers(&self) -> bool`. Default `false` (the mysql CLI starts a table's headers at the left); the PG plane overrides it to `true`, as psql centres them; the shell's grid adds a class from it and never asks which dialect it's in.

**Pattern**: for any new shell-layer behavior that branches on dialect, prefer a trait method (returning `Option<&'static str>` or `bool` or similar) over a dialect-name string match. The method name should describe the SEMANTIC (`centres_headers`, `lexicon`, not `is_mysql`) so the next plane that wants the behavior overrides for the right reason.

The mysql CLI reads `\` and one letter as its command (`\endif` is `\e`, `\sf f` is `\s` then SQL, `\usakila` is `use sakila`): the MySQL plane parses the letter, never a word, and `command_extent` says how much the command takes. Its named commands (`NAMED_COMMANDS`, a port of `find_command`'s table) run as their letters do; `named_command(line, delimiter)` reads a line typed with nothing pending, `statement_command(statement)` a statement its delimiter ended (mysql's `add_line` — `status;`, `select 1; use db;`). psql has none of the latter: its plane keeps the default. Parliament's own `\safe-mode` is the one word it reads (it has no mysql meaning). The psql-named `\sf` / `\sv` conveniences it once had are gone.

## MySQL plane quirks

- **`information_schema` text columns are VARBINARY.** `ENGINE`, `TABLE_COMMENT`, column `Type`, `Default` are VARBINARY with text collation. Backend's `mysql_value` UTF-8-decodes BINARY/VARBINARY/BLOB before emitting `<binary NB>`. If you see `<binary NB>` in output, either non-UTF8 bytes or decode path isn't hit.
- **`BIGINT UNSIGNED` isn't `"BIGINT"`.** sqlx reports the compound string. `TABLE_ROWS`, `DATA_LENGTH` are `BIGINT UNSIGNED`. Any new numeric type match needs UNSIGNED/ZEROFILL stripping.
- Identifier quoting: backticks (`` `schema`.`table` ``).
- `#` is a valid line comment (`lexicon()` is `Lexicon::Mysql`).
- Prefer `information_schema.TABLES` over `SHOW FULL TABLES FROM db` for Tables listing (gives extras columns).

## PostgreSQL plane quirks

- **Identifier quoting: double quotes.** Column aliases also in double quotes — PG folds unquoted identifiers to lowercase, so `"Engine"` must be quoted to preserve case for parser lookup.
- **No `#` line comment.** `--` and `/* */` only.
- **A command `meta_cmd_in` doesn't run fails as psql's does** (`MetaAction::Fail`): psql's `invalid command \foo`, or `\x isn't supported here yet.` when it's one of psql's (`PSQL_COMMANDS`, psql 18's `exec_command` list — add to it when psql adds a command); the shell drops it and keeps the buffer before it. Only the `\if` family returns None (the `psql_if` pass reads it). Implementing a psql command needs no change there.
- **No `SHOW CREATE TABLE`.** Use `pg_get_viewdef(oid, true)`, `pg_get_functiondef(oid)`, `pg_get_triggerdef(oid)`. `table_ddl_sql` reconstructs CREATE TABLE from `pg_attribute` + `pg_constraint` + `pg_index`, including partition-aware `PARTITION BY` (parents) and `PARTITION OF … FOR VALUES …` (partitions) via `pg_get_partkeydef(oid)` + `pg_get_expr(relpartbound, oid)` + `pg_inherits`.
- **FK relationships are stored per-partition.** A logical FK on a partitioned parent appears in `pg_constraint` as one row per partition (`conrelid = partition_oid`). Relationship queries normalise both `conrelid` and `confrelid` via `COALESCE(pg_partition_root(...), ...)` before joining, so the focal table matches its partitions' FK rows AND partition references collapse in the result.
- **No events.** `PostgresPlane::object_kinds()` drops `ObjectKind::Events`. Explorer iterates `plane.object_kinds()`, not `ObjectKind::ALL`.
- **Row estimates:** `pg_class.reltuples::bigint`. `information_schema` has no equivalent.
- **Sizes:** `pg_relation_size(c.oid)` and `pg_indexes_size(c.oid)`, aliased `Data_length` / `Index_length` so `human_bytes` formats them.
- **Composite indexes:** `LATERAL unnest(ix.indkey) WITH ORDINALITY`. Don't hand-roll array indexing.
- **Catalog comments:** `obj_description((schema||'.'||name)::regclass, 'pg_class')` for tables, `col_description(...)` for columns. Always `COALESCE(..., '')` — NULL comments are frequent.
- **`\c dbname` can't switch databases** on a live connection. Plane's meta_cmd returns `MetaAction::Output` explaining this.

## Trait surface

```rust
fn object_kinds(&self) -> &'static [ObjectKind];
fn list_schemas_sql(&self) -> &'static str;
fn list_sql(&self, kind: ObjectKind, schema: Option<&str>) -> String;
fn table_status_sql(&self, schema: &str, name: &str) -> String;
fn table_columns_sql(&self, schema: &str, name: &str) -> String;
fn table_indexes_sql(&self, schema: &str, name: &str) -> String;
fn table_ddl_sql(&self, schema: &str, name: &str) -> String;
fn table_relationship_ancestors_sql(&self, schema: &str, name: &str) -> String;
fn table_relationship_descendants_sql(&self, schema: &str, name: &str) -> String;
fn table_partitions_sql(&self, schema: &str, name: &str) -> String;
fn partitions_are_navigable(&self) -> bool;       // PG true, MySQL false
fn table_columns_bulk_sql(&self, tables: &[(String, String)]) -> String;
fn object_ddl_sql(&self, kind: ObjectKind, schema: &str, name: &str) -> String;
// Data-tab paged read + DML
fn count_rows_sql(&self, schema: &str, table: &str, where_clause: Option<&str>) -> String;
fn select_paged_sql(
    &self, schema: &str, table: &str,
    limit: usize, offset: usize,
    order_by: Option<(&str, bool)>,
    where_clause: Option<&str>,
) -> String;
fn update_row_sql(
    &self, schema: &str, table: &str,
    set_col: &str, set_val: &str,
    pk_cols: &[String], pk_vals: &[String],
) -> String;
fn insert_row_sql(
    &self, schema: &str, table: &str,
    cols: &[String], vals: &[String],
) -> String;
fn delete_row_sql(
    &self, schema: &str, table: &str,
    pk_cols: &[String], pk_vals: &[String],
) -> String;
fn upsert_row_sql(
    &self, schema: &str, table: &str,
    cols: &[String], vals: &[String],
    pk_cols: &[String],
) -> String;

// Meta-commands. `meta_cmd_in` gets the shell's state (`MetaContext`:
// database, `\t`, the server's `version()` text — psql's `\l` branches
// on it — the shell's variables and the session's transaction state);
// `meta_continue` takes a staged command's replies so far and returns
// the next batch, the output (`MetaStep`), or a call psql makes through
// libpq's fast path (`MetaStep::Call`: `\lo_unlink`'s — never shown by
// `ECHO_HIDDEN`). A staged command may start with no batch
// (`Staged(vec![])`): the shell goes straight on to its next step.
// The PG plane reads a command's arguments with
// `pattern::slash_args(rest, &ctx.variables)` — psql's OT_NORMAL lexer,
// `:var`s expanded — before dispatching; don't parse `rest` by hand
// (only psql's whole-line commands do: `READS_ITS_OWN_ARGUMENTS`).
// `interpolates_variables()`: whether typed SQL gets `:var`s (psql).
// `named_command(line, delimiter)`: a line typed with nothing pending that
// is one of the CLI's named commands (mysql's `status`, psql's `quit`);
// `statement_command(statement)`: a statement its delimiter ended that
// the CLI runs as one instead of sending it (mysql only).
// `line_notes(input, delimiter)`: what the CLI says about text on the line
// just entered that stays in the statement (mysql's "Unknown command",
// psql's "Use \q to quit."), as shell entries.
// `extra_arguments(command)`: what the CLI says, after a command that
// didn't fail, about each argument it didn't read (psql's `\x: extra
// argument "a" ignored`). A psql error is `MetaAction::Fail`, and a
// staged command that fails returns `MetaStep::Failed`, so nothing
// follows them.
// `command_extent(command, delimiter)`: how much of the input a command
// takes (psql: its line; mysql: its letter, or its arguments to the
// delimiter).
// `print_result(columns, types, rows, title, footers, options)`: a
// result in a format that isn't the shell's grid, as the CLI prints it
// (psql's `\pset format unaligned` …, `print::print_table`), a list's
// own footers in place of the row count; None: the grid.
// `client_encoding()`: the encoding the session's client speaks, as the
// CLI names it (psql's `:ENCODING`, `UTF8`).
// `print_vertical(columns, rows, style)` (required): rows as the CLI
// prints them vertically (psql's `-[ RECORD 1 ]`, mysql's `*** 1. row
// ***`), without the footer the shell adds — `style` (`PrintedWith`) is
// what the result printed with: its title, psql's `\t`, `\pset border`
// and `linestyle`.
// `expands_automatically(columns, rows, options)`: under `\x auto`,
// whether the result goes vertical in `options.columns` characters
// (psql: its aligned table, drawn as `\pset border` says, is wider).
// `column_align(type_id)` / `centres_headers()`: how the CLI lines up a
// column of a type in its tables, and its headers (the grid follows).
// `statement_opening(sql, txn, vars)` / `statement_closing(txn, tag)`:
// what the CLI sends around a statement the user runs (psql's
// `SendQuery`: `BEGIN` under `AUTOCOMMIT off`, its savepoint under
// `ON_ERROR_ROLLBACK`, then `RELEASE` / `ROLLBACK TO`); the shell sends
// them as batches of their own (`shell::send_user_statement`).
// `described_columns_sql(columns)`: psql's `\gdesc` second query —
// `format_type` over each described column's type and modifier.
// `localized_number(value, locale)`: a number in a right-aligned
// column as the CLI writes it in the client's locale (psql's `\pset
// numericlocale`, `print::format_numeric_locale`); default as it is.
// `null_display()`: what the CLI shows for SQL NULL until told
// otherwise (psql: nothing; mysql: `NULL`) — a shell's starting
// `\pset null`.
// `MetaContext.print` (`PrintOptions`: format, `\x`, `\t`, footer, the
// pane's width, the separators, border and line style) is psql's
// `printTableOpt` for tables the plane prints itself — print them
// through `print::print_table`, never by hand.
fn meta_cmd(&self, input: &str) -> Option<MetaAction>;
fn meta_cmd_in(&self, input: &str, ctx: &MetaContext) -> Option<MetaAction>;
fn meta_continue(&self, input: &str, ctx: &MetaContext, replies: &[BatchResult]) -> MetaStep;
// A server error as this dialect's CLI prints it (psql: VERBOSITY /
// SHOW_CONTEXT via `ErrorStyle`; mysql: `ERROR n (state): msg`).
fn format_error(&self, error: &DbError, query: Option<&str>, style: ErrorStyle) -> Vec<String>;
// What this CLI prints for an empty result instead of its header
// (mysql's "Empty set"; None: psql's header over "(0 rows)"), and for a
// statement without rows (psql: the server's command tag; mysql:
// "Query OK, N rows affected").
fn empty_set(&self) -> Option<&'static str>;
// (lines: psql's tag; mysql's "Query OK…" and the server's info line).
// `format`: the shell's output format when it arrived (psql's
// `PrintQueryStatus` writes `<p>BEGIN</p>` under html). The shell keeps
// the lines (`ShellEntry::Info.said`) — none under psql's `QUIET`.
fn command_status(&self, status: &shared::CommandStatus, format: OutputFormat) -> Vec<String>;
// Whether the CLI prints a statement's status after the rows it
// returned too (psql: a RETURNING's tag; `shell::after_rows`).
fn status_after_rows(&self, status: &shared::CommandStatus) -> bool;
// The query the CLI asks for a statement's warnings with, when it shows
// them (mysql's `warnings` / `\W`: `show warnings`), and how it prints
// their rows after a statement that failed with `error` (none: psql).
fn warnings_query(&self) -> Option<&'static str>;
fn warning_lines(&self, set: &shared::ResultSet, error: Option<u16>) -> Vec<String>;
// The line under a result's rows (mysql: "2 rows in set, 1 warning",
// "Empty set"; psql: "(2 rows)", None under expanded rows), where a
// command's time goes (`Timing::Inline` — mysql "(0.002 sec)" — or
// `Timing::OwnLine` — psql "Time: 0.205 ms"), and whether times show
// before `\timing` asks (psql: no).
fn rows_footer(&self, rows: usize, warnings: u16, vertical: bool) -> Option<String>;
fn timing(&self, exec_time_us: u64) -> Timing;
fn timed_by_default(&self) -> bool;
// A lost session: how the CLI carries on — `Banner` (default; the shell's
// "Session ended" banner), `OnNextStatement { error, notice }` (mysql:
// reconnect and run the statement), `ResetAtOnce { lost, not_connected }`
// (psql: reset now, go on with the rest of the input) — and what it
// prints once the new session is open (`ok`) or can't be.
fn lost_connection(&self) -> LostConnection;
fn reconnected_notice(&self, ok: bool, connection_id: &str, database: &str) -> Vec<String>;
// A statement that switches the session's database as the CLI sees it —
// the database, and what the CLI prints for it (mysql's `use` → "Database
// changed"). Default None.
fn switches_database(&self, sql: &str) -> Option<(String, &'static str)>;
// A switch to another database (psql's `\c`): what the CLI prints once
// the new session is open, and when it couldn't be (`kept`: the previous
// session stays). Defaults: nothing / the bare message.
fn connected_notice(&self, database: &str, user: &str, host: &str, port: &str, had_session: bool) -> Vec<String>;
fn connect_failed(&self, host: &str, port: &str, error: Option<&DbError>, message: &str, kept: bool) -> Vec<String>;

// Shell-layer dialect routing (see "Dialect purity" above): defaults
// keep every plane out of a CLI's behaviour unless it opts in.
fn centres_headers(&self) -> bool { false }
```

For Upsert with no non-PK columns filled (caller passed only the
PK), MySQL emits `INSERT IGNORE INTO …` and Postgres emits
`… ON CONFLICT (pk) DO NOTHING` — both express "insert if absent,
leave alone otherwise" without erroring on duplicate keys. Caller
gates on `pk_cols.is_empty()` (no PK = no conflict target = the
toggle is disabled in the UI).

## DML value-quoting contract

`update_row_sql`, `insert_row_sql`, and `delete_row_sql` share one
escaping convention so the Data-tab UI can pass user-typed strings
through every plane uniformly:

- The literal string `"NULL"` (uppercase, exact) emits a bare SQL
  `NULL` keyword. Anything else is a value literal.
- Values are wrapped in single quotes with embedded single quotes
  doubled: `that's` → `'that''s'`.
- Identifiers (schema / table / column) use the plane's native
  quoting (backticks for MySQL, double quotes for PG); embedded
  quote chars are doubled.

For Insert with an empty `cols` list (user left every form field
blank), MySQL emits `INSERT INTO ... () VALUES ()` and PG emits
`INSERT INTO ... DEFAULT VALUES`. Both ask the DB to take all
column defaults.

## Catalog choice

`information_schema` is cross-DBMS standard but often incomplete or slow. `pg_catalog` is Postgres-specific but faster and more complete. Pick deliberately, not by habit.

## DDL safety

Any DDL-altering SQL (even a placeholder) — flag the blast radius explicitly.
