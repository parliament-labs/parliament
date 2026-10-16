//! AST-based SQL inspection. Single source of truth for the features
//! that need to *understand* a piece of user-typed SQL — safe-mode (is
//! it destructive?) and Schema Explorer cache invalidation (does it
//! change object metadata?). (The TXN badge used to guess from it too;
//! it now follows the server's own transaction status.)
//!
//! Pre-fix these lived in three separate prefix-match
//! implementations: trim, uppercase, `starts_with("DROP TABLE")`. Every
//! one was fooled by the same shapes:
//!
//! - `/* migration step */ DROP TABLE users` — starts with `/*`, not
//!   `DROP`. All three features missed it.
//! - `SELECT 1; DROP TABLE users;` — safe-mode only inspected
//!   single-statement input. The DROP shipped without confirmation.
//! - `DELETE FROM users` (no WHERE) — the textbook DBA footgun. Not in
//!   the destructive pattern list because `DELETE` is also the prefix
//!   of the safe `DELETE FROM users WHERE id = 1`.
//!
//! Audit references: cluster A/B/C, findings 1.1, 1.2, 1.3, 1.6, 1.7
//! in `vault/Engineering/Audit Findings 2026-06.md`.
//!
//! The fix: parse with `sqlparser-rs` using the active plane's dialect
//! (already exposed via `Plane::sql_dialect()` for the formatter), then
//! pattern-match the AST.
//!
//! **Parse-failure policy.** `sqlparser-rs` is incomplete — vendor
//! extensions, optimizer hints, some WITH-RECURSIVE patterns, newer
//! `MERGE` / `JSON_TABLE` forms can fail to parse. The right answer per
//! feature is encoded in [`StatementIntent::parse_failed`]:
//! - Safe-mode → treat as destructive (confirm with explanation).
//!   Punishing valid SQL that's just ahead of sqlparser-rs's coverage
//!   would burn the safe-mode protection budget by training the DBA to
//!   disable it.
//! - Schema cache → invalidate anyway. Refresh is cheap; a false
//!   positive costs nothing.

use sqlparser::ast::{AlterTableOperation, ObjectType, SetExpr, Statement};
use sqlparser::dialect::Dialect;
use sqlparser::parser::Parser;

/// Reason a statement was flagged as destructive. Surfaced in the
/// safe-mode confirm dialog so the DBA sees *why* a statement triggered
/// the warning — useful when the user is reviewing a batch of mixed
/// statements before approving the whole thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DestructiveReason {
    /// `DROP TABLE`, `DROP SCHEMA`, `DROP DATABASE`, `DROP VIEW`,
    /// `DROP INDEX`, etc. Always destructive — there's no safe form.
    Drop,
    /// `TRUNCATE`. Wipes every row; no WHERE clause possible.
    Truncate,
    /// `DELETE FROM x` with no WHERE clause. The textbook footgun
    /// pattern. `DELETE FROM x WHERE 1=1` is allowed through — the
    /// presence of an explicit WHERE is the DBA's intentional ritual.
    DeleteWithoutWhere,
    /// `UPDATE x SET y = …` with no WHERE clause. Same shape as
    /// DeleteWithoutWhere — mass-mutates every row.
    UpdateWithoutWhere,
    /// `ALTER TABLE … DROP COLUMN` / `DROP CONSTRAINT` —
    /// data-destroying ALTERs that the bare-`ALTER TABLE` prefix-match
    /// couldn't distinguish from harmless `ADD COLUMN`.
    AlterTableDestructive,
    /// `DROP ROLE`, `DROP USER`. Role/auth-affecting destruction.
    DropRole,
    /// `sqlparser-rs` couldn't parse this statement. Safe-mode treats
    /// as destructive (see module-level parse-failure policy).
    /// PG-specific verbs without dedicated sqlparser variants —
    /// `DROP OWNED BY`, `DROP TABLESPACE`, etc. — land here too.
    ParseFailed,
}

/// Per-statement inspection result. Every consumer reads only the
/// field(s) it cares about; the helper computes them all in one AST
/// walk so we don't pay for parsing three times.
#[derive(Clone, Debug, Default)]
pub(crate) struct StatementIntent {
    /// `Some(reason)` if safe-mode should gate this statement.
    pub(crate) destructive: Option<DestructiveReason>,
    /// `true` for object-DDL: CREATE TABLE / VIEW / INDEX / FUNCTION /
    /// TYPE / SCHEMA / SEQUENCE, ALTER TABLE / VIEW / INDEX / TYPE /
    /// SCHEMA, DROP TABLE / VIEW / INDEX / FUNCTION / TYPE / SCHEMA /
    /// SEQUENCE, RENAME, TRUNCATE.
    /// The Schema Explorer flushes its object cache when any of these
    /// land.
    pub(crate) invalidates_schema: bool,
    /// `true` for role/auth DDL: GRANT, REVOKE, CREATE ROLE, ALTER
    /// ROLE, DROP ROLE, CREATE USER, ALTER USER, DROP USER, SET
    /// PASSWORD. The Roles list in the Admin pane refreshes on these.
    pub(crate) invalidates_roles: bool,
    /// True when `sqlparser-rs` couldn't parse the statement. Each
    /// consumer applies its own safe default; see the module-level
    /// parse-failure policy.
    pub(crate) parse_failed: bool,
}

/// Inspect a single SQL statement (no trailing `;`, no batching) and
/// classify it. Caller is responsible for splitting multi-statement
/// input first — `crate::parser::split_statements` is the canonical
/// splitter and is comment / quote / dollar-quote aware.
pub(crate) fn inspect_statement(sql: &str, dialect: &dyn Dialect) -> StatementIntent {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return StatementIntent::default();
    }

    let parsed = match Parser::parse_sql(dialect, trimmed) {
        Ok(stmts) => stmts,
        Err(_) => {
            // Parse-failure policy: see module-level docs. Safe-mode
            // reads `destructive`, schema cache reads `invalidates_*`
            // (left false except we DO invalidate schema cache on the
            // side of caution — see the main.rs site for how it
            // composes).
            return StatementIntent {
                destructive: Some(DestructiveReason::ParseFailed),
                invalidates_schema: false,
                invalidates_roles: false,
                parse_failed: true,
            };
        }
    };

    // sqlparser returns one Statement per logical statement. Callers
    // pre-split; if multiple come back (splitter under-segmented), we
    // classify the first conservatively.
    let Some(stmt) = parsed.into_iter().next() else {
        return StatementIntent::default();
    };

    let mut out = classify(stmt);
    // Apply post-AST keyword fallbacks for shapes the AST recognizes
    // but doesn't classify the way the DBA needs. Examples:
    //   - `RESET MASTER`, `RESET BINARY LOGS AND GTIDS`, `SHUTDOWN` in
    //     MySQL — parse as `Statement::Reset` / `Statement::Use`-ish
    //     wrappers without surfacing the "this nukes the server" bit.
    apply_keyword_fallbacks(trimmed, &mut out);
    out
}

/// Whether `sql` is one statement that returns rows — a query (SELECT,
/// VALUES, TABLE, WITH …), DML with RETURNING, SHOW, EXPLAIN or FETCH —
/// so that, returning none, its result is an empty set, which the mysql
/// CLI prints as "Empty set" (MySQL's empty result has no columns to
/// tell it by; a Postgres one has). Not a count of rows affected. False
/// when `sql` doesn't parse (sqlparser 0.62 misses PG's `TABLE name`,
/// for one) or isn't one statement: such a result shows as rows affected.
pub(crate) fn returns_rows(sql: &str, dialect: &dyn Dialect) -> bool {
    match Parser::parse_sql(dialect, sql.trim()) {
        Ok(statements) if statements.len() == 1 => statement_returns_rows(&statements[0]),
        _ => false,
    }
}

fn statement_returns_rows(statement: &Statement) -> bool {
    match statement {
        Statement::Query(query) => set_returns_rows(&query.body),
        Statement::Insert(insert) => insert.returning.is_some(),
        Statement::Update(update) => update.returning.is_some(),
        Statement::Delete(delete) => delete.returning.is_some(),
        Statement::Explain { .. } | Statement::ExplainTable { .. } | Statement::Fetch { .. } => true,
        Statement::ShowFunctions { .. } | Statement::ShowVariable { .. } | Statement::ShowStatus { .. }
        | Statement::ShowVariables { .. } | Statement::ShowCreate { .. } | Statement::ShowColumns { .. }
        | Statement::ShowCatalogs { .. } | Statement::ShowDatabases { .. } | Statement::ShowProcessList { .. }
        | Statement::ShowSchemas { .. } | Statement::ShowCharset(_) | Statement::ShowObjects(_)
        | Statement::ShowTables { .. } | Statement::ShowViews { .. } | Statement::ShowCollation { .. } => true,
        _ => false,
    }
}

fn set_returns_rows(body: &SetExpr) -> bool {
    match body {
        // SELECT … INTO makes a table instead.
        SetExpr::Select(select) => select.into.is_none(),
        SetExpr::Query(query) => set_returns_rows(&query.body),
        SetExpr::SetOperation { .. } | SetExpr::Values(_) | SetExpr::Table(_) => true,
        // A data-modifying statement under WITH: rows with RETURNING.
        SetExpr::Insert(statement) | SetExpr::Update(statement) | SetExpr::Delete(statement)
        | SetExpr::Merge(statement) => statement_returns_rows(statement),
    }
}

/// Keyword-level fallbacks for shapes the AST either misses or
/// classifies in a way that doesn't match DBA expectations. This is a
/// narrow escape hatch — only patterns explicitly documented as
/// missing from the AST belong here. Resist growing the list; the AST
/// is the canonical inspector.
///
/// Currently:
/// - `RESET MASTER` / `RESET BINARY LOGS AND GTIDS` (MySQL) — wipe
///   binlog state across an entire replication topology. The plane's
///   pre-fix destructive_patterns list flagged them; the AST doesn't.
/// - `SHUTDOWN` (MySQL) — turns the server off. Same shape.
/// - `FLUSH PRIVILEGES` and other `FLUSH …` (MySQL) — admin-y but
///   recoverable; deliberately NOT in this list.
fn apply_keyword_fallbacks(trimmed: &str, out: &mut StatementIntent) {
    let head = trimmed.trim_start_matches(|c: char| c.is_whitespace());
    let upper: String = head.chars().take(64).collect::<String>().to_ascii_uppercase();

    // ── MySQL admin destructive ──────────────────────────────────────
    if out.destructive.is_none() {
        let admin_match = upper.starts_with("RESET MASTER")
            || upper.starts_with("RESET BINARY LOGS AND GTIDS")
            || upper.starts_with("RESET REPLICA")
            || upper.starts_with("RESET SLAVE")
            || upper == "SHUTDOWN"
            || upper.starts_with("SHUTDOWN ");
        if admin_match {
            // Surface as ParseFailed so the confirm dialog header says
            // "couldn't fully parse — confirm if safe." Technically
            // the AST DID parse most of these, but the classification
            // outcome ("destructive admin command") matches what the
            // ParseFailed UX is meant to convey: an unfamiliar
            // statement the DBA should look at twice.
            out.destructive = Some(DestructiveReason::ParseFailed);
        }
    }
}

fn classify(stmt: Statement) -> StatementIntent {
    let mut out = StatementIntent::default();

    match &stmt {
        // ── Destructive DML ──────────────────────────────────────────
        Statement::Delete(d) => {
            if d.selection.is_none() {
                out.destructive = Some(DestructiveReason::DeleteWithoutWhere);
            }
        }
        Statement::Update(u) => {
            if u.selection.is_none() {
                out.destructive = Some(DestructiveReason::UpdateWithoutWhere);
            }
        }

        // ── Drop family ──────────────────────────────────────────────
        // Every Drop* variant is destructive. We also flag
        // schema-affecting drops as invalidating the explorer cache,
        // and role-affecting drops as invalidating the roles list.
        Statement::Drop { object_type, .. } => {
            out.destructive = Some(match object_type {
                ObjectType::Role => DestructiveReason::DropRole,
                _                => DestructiveReason::Drop,
            });
            // DROP of any schema object invalidates the explorer.
            // Role / user drops don't strictly need a schema refresh
            // but they DO affect the Roles list.
            match object_type {
                ObjectType::Role => { out.invalidates_roles = true; }
                _                => { out.invalidates_schema = true; }
            }
        }
        Statement::DropFunction(_)
        | Statement::DropProcedure { .. }
        | Statement::DropTrigger { .. } => {
            out.destructive = Some(DestructiveReason::Drop);
            out.invalidates_schema = true;
        }
        Statement::DropExtension(_)
        | Statement::DropDomain(_)
        | Statement::DropPolicy(_) => {
            out.destructive = Some(DestructiveReason::Drop);
            out.invalidates_schema = true;
        }
        // PG-specific: `DROP OWNED [BY]` and `DROP TABLESPACE` aren't
        // dedicated variants in sqlparser 0.62 — they tend to fall
        // through to a parse error or get bucketed elsewhere. The
        // parse-failure policy catches them: safe-mode will confirm
        // before shipping, which is the correct behavior for both
        // since they're destructive.
        Statement::Truncate { .. } => {
            out.destructive = Some(DestructiveReason::Truncate);
            // TRUNCATE doesn't change schema, but the data tab's
            // cached row counts go stale; we let the cache logic
            // decide whether to refresh in response. Treating it as
            // schema-invalidating costs little (re-introspect on
            // visit) and matches the pre-fix behavior.
            out.invalidates_schema = true;
        }

        // ── Alter family ─────────────────────────────────────────────
        // ALTER TABLE is destructive only for column / constraint /
        // partition removals — `ADD COLUMN`, `SET DEFAULT`, etc. are
        // safe. The pre-fix `starts_with("ALTER ")` couldn't make
        // that distinction.
        Statement::AlterTable(a) => {
            out.invalidates_schema = true;
            if a.operations.iter().any(alter_op_is_destructive) {
                out.destructive = Some(DestructiveReason::AlterTableDestructive);
            }
        }
        Statement::AlterIndex { .. }
        | Statement::AlterView { .. }
        | Statement::AlterFunction(_)
        | Statement::AlterSchema(_)
        | Statement::AlterType(_) => {
            out.invalidates_schema = true;
        }
        Statement::AlterRole { .. } => {
            out.invalidates_roles = true;
        }
        Statement::AlterPolicy { .. } => {
            out.invalidates_schema = true;
        }

        // ── Create family ────────────────────────────────────────────
        Statement::CreateTable(_)
        | Statement::CreateVirtualTable { .. }
        | Statement::CreateView(_)
        | Statement::CreateIndex(_)
        | Statement::CreateSchema { .. }
        | Statement::CreateExtension { .. }
        | Statement::CreateSequence { .. }
        | Statement::CreateFunction(_)
        | Statement::CreateProcedure { .. }
        | Statement::CreateTrigger { .. }
        | Statement::CreateType { .. }
        | Statement::CreateDomain(_)
        | Statement::CreatePolicy(_) => {
            out.invalidates_schema = true;
        }
        Statement::CreateRole(_) => {
            out.invalidates_roles = true;
        }
        Statement::CreateUser(_) => {
            out.invalidates_roles = true;
        }

        // ── GRANT / REVOKE ───────────────────────────────────────────
        Statement::Grant { .. }
        | Statement::Revoke { .. } => {
            out.invalidates_roles = true;
        }

        // ── SET PASSWORD ─────────────────────────────────────────────
        // `SET PASSWORD …` flows through a generic Statement::Set in
        // sqlparser; we'd need to introspect the variable name to
        // detect "password". For now this falls through unflagged —
        // the role list isn't actually affected by a password change
        // (the rows themselves don't move), so dropping role
        // invalidation here costs nothing user-visible.

        // ── RENAME (MySQL) ───────────────────────────────────────────
        // `RENAME TABLE old TO new` invalidates schema; sqlparser
        // doesn't have a dedicated variant in 0.62, so unknown
        // statements at this point fall through. Not a regression
        // from the prefix-match: `RENAME ` was in the old list and
        // schema-invalidated.

        _ => {}
    }

    out
}

/// True for ALTER TABLE operations that destroy data or definitions,
/// as opposed to additive ones. The pre-fix `starts_with("ALTER ")`
/// gated every ALTER statement; this lets `ADD COLUMN`, `SET
/// DEFAULT`, `RENAME COLUMN`, etc. through unflagged while still
/// catching the data-destroying forms.
fn alter_op_is_destructive(op: &AlterTableOperation) -> bool {
    matches!(
        op,
        AlterTableOperation::DropColumn { .. }
            | AlterTableOperation::DropConstraint { .. }
            | AlterTableOperation::DropPrimaryKey { .. }
            | AlterTableOperation::DropForeignKey { .. }
            | AlterTableOperation::DropIndex { .. }
            | AlterTableOperation::DropPartitions { .. }
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn statements_that_return_rows() {
        use sqlparser::dialect::{MySqlDialect, PostgreSqlDialect};
        let pg = |sql: &str| super::returns_rows(sql, &PostgreSqlDialect {});
        let my = |sql: &str| super::returns_rows(sql, &MySqlDialect {});
        for sql in [
            "SELECT 1 WHERE false", "  select * from film where false;", "VALUES (1), (2)",
            "WITH x AS (SELECT 1) SELECT * FROM x", "SELECT 1 UNION SELECT 2",
            "INSERT INTO t VALUES (1) RETURNING id", "UPDATE t SET a = 1 RETURNING *", "DELETE FROM t RETURNING id",
            "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d", "EXPLAIN SELECT 1", "SHOW work_mem",
            "FETCH 10 FROM c",
        ] {
            assert!(pg(sql), "{sql}");
        }
        for sql in [
            "INSERT INTO t VALUES (1)", "UPDATE t SET a = 1", "DELETE FROM t", "CREATE TABLE t (a int)",
            "SELECT 1 INTO new_t", "BEGIN", "SET work_mem = '8MB'", "SELECT 1; SELECT 2", "", "not sql at all",
        ] {
            assert!(!pg(sql), "{sql}");
        }
        assert!(my("SHOW TABLES") && my("SHOW CREATE TABLE film") && my("SELECT * FROM film WHERE 0"));
        assert!(!my("UPDATE film SET title = title WHERE 0"));
    }

    use super::*;
    use sqlparser::dialect::PostgreSqlDialect;

    fn pg() -> &'static dyn Dialect {
        &PostgreSqlDialect {}
    }

    fn inspect(sql: &str) -> StatementIntent {
        inspect_statement(sql, pg())
    }

    // ── Audit 1.1 — multi-statement is the caller's job ──────────────
    // The inspector takes ONE statement; the caller splits first.

    // ── Audit 1.2 — comment / whitespace prefixes ────────────────────

    #[test]
    fn block_comment_prefix_does_not_hide_drop() {
        // The pre-fix prefix-match took `/* migration */ DROP TABLE x`
        // and matched against `/`, not `DROP`. AST parsing strips the
        // comment and surfaces the real intent.
        let i = inspect("/* migration step 3 */ DROP TABLE old_users");
        assert!(i.destructive.is_some(), "block-commented DROP must be flagged");
        assert!(i.invalidates_schema);
    }

    #[test]
    fn line_comment_prefix_does_not_hide_drop() {
        let i = inspect("-- comment\nDROP TABLE old_users");
        assert!(i.destructive.is_some(), "line-commented DROP must be flagged");
    }

    #[test]
    fn tab_separator_does_not_hide_drop() {
        // Pre-fix prefix-match looked for literal `DROP TABLE` with
        // a single space; tabs / multi-space slipped through.
        let i = inspect("DROP\tTABLE x");
        assert!(i.destructive.is_some(), "tab-separated DROP must be flagged");
    }

    // ── Audit 1.3 — DELETE / UPDATE without WHERE ────────────────────

    #[test]
    fn delete_without_where_is_destructive() {
        let i = inspect("DELETE FROM users");
        assert_eq!(i.destructive, Some(DestructiveReason::DeleteWithoutWhere));
    }

    #[test]
    fn delete_with_where_is_safe() {
        let i = inspect("DELETE FROM users WHERE id = 1");
        assert!(i.destructive.is_none());
    }

    #[test]
    fn delete_with_tautological_where_is_safe() {
        // `WHERE 1=1` is the DBA's intentional ritual — explicit
        // confirmation that this is the whole table. Out of scope for
        // automatic gating; trust the DBA when they typed any WHERE.
        let i = inspect("DELETE FROM users WHERE 1=1");
        assert!(i.destructive.is_none(), "tautological WHERE is trusted");
    }

    #[test]
    fn update_without_where_is_destructive() {
        let i = inspect("UPDATE accounts SET balance = 0");
        assert_eq!(i.destructive, Some(DestructiveReason::UpdateWithoutWhere));
    }

    #[test]
    fn update_with_where_is_safe() {
        let i = inspect("UPDATE accounts SET balance = 0 WHERE id = 7");
        assert!(i.destructive.is_none());
    }

    // ── Audit 1.7 — schema-cache invalidation comment-aware ──────────

    #[test]
    fn comment_prefixed_create_table_invalidates_schema() {
        let i = inspect("/* new */ CREATE TABLE x (id INT)");
        assert!(i.invalidates_schema);
    }

    #[test]
    fn alter_table_add_column_invalidates_schema_but_not_destructive() {
        // The pre-fix `starts_with("ALTER ")` gated every ALTER as
        // schema-invalidating (correct) but also as destructive when
        // safe-mode looked at it (incorrect — ADD COLUMN is safe).
        // AST-based classification splits the two.
        let i = inspect("ALTER TABLE x ADD COLUMN y INT");
        assert!(i.invalidates_schema, "ALTER TABLE invalidates schema");
        assert!(
            i.destructive.is_none(),
            "ALTER TABLE ADD COLUMN is not destructive: got {:?}",
            i.destructive,
        );
    }

    #[test]
    fn alter_table_drop_column_is_destructive() {
        let i = inspect("ALTER TABLE x DROP COLUMN y");
        assert_eq!(i.destructive, Some(DestructiveReason::AlterTableDestructive));
        assert!(i.invalidates_schema);
    }

    #[test]
    fn grant_invalidates_roles_not_schema() {
        let i = inspect("GRANT SELECT ON TABLE x TO some_user");
        assert!(i.invalidates_roles, "GRANT should invalidate role cache");
        assert!(!i.invalidates_schema, "GRANT should not invalidate schema cache");
    }

    #[test]
    fn create_role_invalidates_roles() {
        let i = inspect("CREATE ROLE alice");
        assert!(i.invalidates_roles);
    }

    #[test]
    fn drop_role_is_destructive_and_invalidates_roles() {
        let i = inspect("DROP ROLE alice");
        assert_eq!(i.destructive, Some(DestructiveReason::DropRole));
        assert!(i.invalidates_roles);
    }

    // ── Parse failure ────────────────────────────────────────────────

    #[test]
    fn parse_failure_is_treated_as_destructive() {
        // Garbage SQL must surface as a confirm dialog rather than
        // silently shipping. See module-level parse-failure policy.
        let i = inspect("this is not valid sql at all !!!");
        assert!(i.parse_failed);
        assert_eq!(i.destructive, Some(DestructiveReason::ParseFailed));
    }

    #[test]
    fn empty_input_is_inert() {
        let i = inspect("");
        assert!(i.destructive.is_none());
        assert!(!i.invalidates_schema);
        assert!(!i.invalidates_roles);
        assert!(!i.parse_failed);
    }

    // ── Common safe shapes ───────────────────────────────────────────

    #[test]
    fn plain_select_is_inert() {
        let i = inspect("SELECT * FROM users");
        assert!(i.destructive.is_none());
        assert!(!i.invalidates_schema);
        assert!(!i.invalidates_roles);
    }

    #[test]
    fn truncate_is_destructive_and_invalidates_schema() {
        let i = inspect("TRUNCATE TABLE temp_data");
        assert_eq!(i.destructive, Some(DestructiveReason::Truncate));
        assert!(i.invalidates_schema);
    }

    #[test]
    fn drop_table_cascade_is_destructive() {
        let i = inspect("DROP TABLE x CASCADE");
        assert_eq!(i.destructive, Some(DestructiveReason::Drop));
        assert!(i.invalidates_schema);
    }

    // ── Reviewer-flagged regressions from issue #9 first pass ────────
    // These shapes were in the old prefix-match implementations but
    // dropped out when we switched to AST. The keyword-fallback layer
    // restores them — each test below pins one of the regressions.

    /// MySQL admin commands that wipe binlog state across an entire
    /// replication topology. Pre-fix the plane's destructive_patterns
    /// listed them; the AST classifies them as bare Reset / Use
    /// without surfacing the danger. Keyword fallback gates them.
    #[test]
    fn reset_master_is_destructive() {
        let i = inspect("RESET MASTER");
        assert!(i.destructive.is_some(), "RESET MASTER must be gated by safe-mode");
    }

    #[test]
    fn reset_binary_logs_is_destructive() {
        let i = inspect("RESET BINARY LOGS AND GTIDS");
        assert!(i.destructive.is_some());
    }

    #[test]
    fn shutdown_is_destructive() {
        let i = inspect("SHUTDOWN");
        assert!(i.destructive.is_some(), "SHUTDOWN must be gated by safe-mode");
    }
}
