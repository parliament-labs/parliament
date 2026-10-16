//! River-style SQL beautifier. Walks a sqlparser AST and emits text with
//! right-aligned clause keywords and one column per line in SELECT / GROUP BY
//! lists. Driven by the per-connection `FormatStyle` setting.
//!
//! v1 scope: SELECT statements (and the things they contain — WITH / CTEs,
//! UNION / INTERSECT / EXCEPT, FROM / JOIN, WHERE, GROUP BY, HAVING,
//! ORDER BY, LIMIT, OFFSET). Non-SELECT statements (INSERT / UPDATE /
//! DELETE / DDL) pass through unchanged — `sqlparser::ast::Statement`'s
//! `Display` impl emits the original SQL on a single line.
//!
//! ## River column
//!
//! The "river" is the column where every clause keyword's first word
//! right-aligns. Default 6 (matching `SELECT` / `HAVING`); widens when a
//! present keyword is longer — `NATURAL JOIN` pushes to 7, `INTERSECT`
//! pushes to 9, `WITH RECURSIVE` stays at 4 because we right-align only the
//! first word. Each query scope (top-level Query, CTE body, subquery)
//! computes its own river so a CTE with `INTERSECT` doesn't force its outer
//! query to also indent.

use sqlparser::ast::{
    Cte, Expr, Fetch, Function, GroupByExpr, Join, JoinConstraint, JoinOperator,
    LockClause, NamedWindowDefinition, Query, Select, SelectItem, SetExpr,
    SetOperator, Statement, TableWithJoins, WindowSpec, WindowType, With,
};
use sqlparser::dialect::Dialect;
use sqlparser::parser::Parser;

/// Minimum river column — matches `SELECT` / `HAVING` (6 chars).
const MIN_RIVER: usize = 6;
/// Maximum width for a WHERE / HAVING leaf to render inline. Above this,
/// nested AND/OR chains are exploded into a multi-line block so the
/// structure stays scannable.
const MAX_LEAF_INLINE: usize = 60;

/// Right-pad `kw` so its FIRST word ends at column `river` (1-indexed).
/// Multi-word keywords (`GROUP BY`, `LEFT JOIN`) get the first word aligned;
/// subsequent words trail inline. If the first word exceeds the river,
/// emits a single leading space.
fn pad_kw(kw: &str, river: usize) -> String {
    let first = kw.split_whitespace().next().unwrap_or("");
    if first.len() > river {
        format!(" {}", kw)
    } else {
        let pad = river - first.len();
        format!("{}{}", " ".repeat(pad), kw)
    }
}

fn first_word_len(s: &str) -> usize {
    s.split_whitespace().next().map(|w| w.len()).unwrap_or(0)
}

/// Parse and reformat. Returns `Some(new_sql)` only when the parse succeeds.
/// Non-SELECT statements pass through their default `Display`; multi-statement
/// input is joined with `;\n`.
pub fn format_river(sql: &str, dialect: &dyn Dialect) -> Option<String> {
    let stmts = Parser::parse_sql(dialect, sql).ok()?;
    if stmts.is_empty() { return None; }
    let mut parts: Vec<String> = Vec::with_capacity(stmts.len());
    for stmt in &stmts {
        match stmt {
            Statement::Query(q) => parts.push(render_query(q)),
            // v1 scope: SELECT statements only. Anything else (INSERT,
            // UPDATE, DELETE, CREATE, ALTER, DROP, BEGIN, COMMIT, …) bails
            // the whole reformat so the user's original text stays
            // untouched. Even a mixed `SELECT 1; INSERT ...` input bails —
            // we don't want surprise normalization of DML the user just
            // typed.
            _ => return None,
        }
    }
    // Preserve the user's terminator + whatever trailing whitespace they
    // typed *verbatim* — the parser eats `;` and trailing whitespace, so
    // the formatter has to put them back. Without this, `;\n ` collapses
    // to just `;` and the cursor gets yanked back onto the previous line.
    let middle = parts.join(";\n");
    let trimmed = sql.trim_end();
    let trailing_ws = &sql[trimmed.len()..];
    let trailing_semi = if trimmed.ends_with(';') { ";" } else { "" };
    Some(format!("{}{}{}", middle, trailing_semi, trailing_ws))
}

// ── River computation ────────────────────────────────────────────────────

/// Determine the river column for one Query scope by walking every clause
/// keyword that will appear in the rendered output and taking the max
/// first-word length (floored at MIN_RIVER).
fn compute_query_river(q: &Query) -> usize {
    let mut max = MIN_RIVER;
    if q.with.is_some() {
        max = max.max(first_word_len("WITH"));
    }
    if q.order_by.is_some() {
        max = max.max(first_word_len("ORDER BY"));
    }
    if let Some(lc) = &q.limit_clause {
        use sqlparser::ast::LimitClause;
        match lc {
            LimitClause::LimitOffset { limit, offset, .. } => {
                if limit.is_some()  { max = max.max(first_word_len("LIMIT"));  }
                if offset.is_some() { max = max.max(first_word_len("OFFSET")); }
            }
            LimitClause::OffsetCommaLimit { .. } => {
                max = max.max(first_word_len("LIMIT"));
                max = max.max(first_word_len("OFFSET"));
            }
        }
    }
    if q.fetch.is_some() {
        max = max.max(first_word_len("FETCH"));
    }
    if !q.locks.is_empty() {
        max = max.max(first_word_len("FOR"));
    }
    walk_set_expr_for_river(&q.body, &mut max);
    max
}

fn walk_set_expr_for_river(e: &SetExpr, max: &mut usize) {
    match e {
        SetExpr::Select(s) => walk_select_for_river(s, max),
        // Nested subqueries get their OWN river; don't drag their keywords
        // into the outer scope's computation.
        SetExpr::Query(_) => {}
        SetExpr::SetOperation { op, left, right, .. } => {
            let op_name = match op {
                SetOperator::Union     => "UNION",
                SetOperator::Intersect => "INTERSECT",
                SetOperator::Except    => "EXCEPT",
                SetOperator::Minus     => "MINUS",
            };
            *max = (*max).max(first_word_len(op_name));
            walk_set_expr_for_river(left, max);
            walk_set_expr_for_river(right, max);
        }
        _ => {}
    }
}

fn walk_select_for_river(s: &Select, max: &mut usize) {
    *max = (*max).max(first_word_len("SELECT"));
    if !s.from.is_empty() {
        *max = (*max).max(first_word_len("FROM"));
        for tw in &s.from {
            for j in &tw.joins {
                let kw = join_keyword_str(j);
                *max = (*max).max(first_word_len(&kw));
            }
        }
    }
    if s.selection.is_some() {
        *max = (*max).max(first_word_len("WHERE"));
    }
    let has_group = matches!(&s.group_by, GroupByExpr::Expressions(exprs, _) if !exprs.is_empty())
        || matches!(&s.group_by, GroupByExpr::All(_));
    if has_group {
        *max = (*max).max(first_word_len("GROUP BY"));
    }
    if s.having.is_some() {
        *max = (*max).max(first_word_len("HAVING"));
    }
    if !s.named_window.is_empty() {
        *max = (*max).max(first_word_len("WINDOW"));
    }
    if s.qualify.is_some() {
        *max = (*max).max(first_word_len("QUALIFY"));
    }
}

/// Compute just the keyword string for a Join (including any NATURAL
/// prefix), used both by `render_join` and by the river-width walk.
fn join_keyword_str(j: &Join) -> String {
    use JoinOperator::*;
    let kw: &str = match &j.join_operator {
        Join(_)         => "JOIN",
        Inner(_)        => "INNER JOIN",
        Left(_)         => "LEFT JOIN",
        LeftOuter(_)    => "LEFT OUTER JOIN",
        Right(_)        => "RIGHT JOIN",
        RightOuter(_)   => "RIGHT OUTER JOIN",
        FullOuter(_)    => "FULL OUTER JOIN",
        CrossJoin(_)    => "CROSS JOIN",
        Semi(_)         => "SEMI JOIN",
        LeftSemi(_)     => "LEFT SEMI JOIN",
        RightSemi(_)    => "RIGHT SEMI JOIN",
        Anti(_)         => "ANTI JOIN",
        LeftAnti(_)     => "LEFT ANTI JOIN",
        RightAnti(_)    => "RIGHT ANTI JOIN",
        StraightJoin(_) => "STRAIGHT_JOIN",
        CrossApply      => "CROSS APPLY",
        OuterApply      => "OUTER APPLY",
        AsOf { .. } | ArrayJoin | LeftArrayJoin | InnerArrayJoin => "",
    };
    let constraint = match &j.join_operator {
        Join(c) | Inner(c) | Left(c) | LeftOuter(c) | Right(c) | RightOuter(c)
        | FullOuter(c) | CrossJoin(c) | Semi(c) | LeftSemi(c) | RightSemi(c)
        | Anti(c) | LeftAnti(c) | RightAnti(c) | StraightJoin(c) => Some(c),
        _ => None,
    };
    if matches!(constraint, Some(JoinConstraint::Natural)) {
        format!("NATURAL {}", kw)
    } else {
        kw.to_string()
    }
}

// ── Rendering ────────────────────────────────────────────────────────────

fn render_query(q: &Query) -> String {
    let river = compute_query_river(q);
    let mut out = String::new();
    if let Some(with) = &q.with {
        out.push_str(&render_with(with, river));
        out.push('\n');
    }
    out.push_str(&render_set_expr(&q.body, river));
    if let Some(order) = &q.order_by {
        out.push('\n');
        out.push_str(&render_order_by(order, river));
    }
    if let Some(lc) = &q.limit_clause {
        out.push_str(&render_limit_clause(lc, river));
    }
    if let Some(fetch) = &q.fetch {
        out.push('\n');
        out.push_str(&render_fetch(fetch, river));
    }
    for lock in &q.locks {
        out.push('\n');
        out.push_str(&render_lock_clause(lock, river));
    }
    out
}

fn render_fetch(fetch: &Fetch, river: usize) -> String {
    // SQL-standard `FETCH FIRST [<n> [PERCENT]] ROWS { ONLY | WITH TIES }`.
    let mut out = pad_kw("FETCH", river);
    out.push_str(" FIRST");
    if let Some(qty) = &fetch.quantity {
        out.push(' ');
        out.push_str(&qty.to_string());
        if fetch.percent {
            out.push_str(" PERCENT");
        }
    }
    out.push_str(" ROWS");
    out.push_str(if fetch.with_ties { " WITH TIES" } else { " ONLY" });
    out
}

fn render_lock_clause(lock: &LockClause, river: usize) -> String {
    let mut out = pad_kw("FOR", river);
    out.push(' ');
    // LockType::Update / Share render via Display as "UPDATE" / "SHARE".
    out.push_str(&lock.lock_type.to_string());
    if let Some(of) = &lock.of {
        out.push_str(" OF ");
        out.push_str(&of.to_string());
    }
    if let Some(nb) = &lock.nonblock {
        out.push(' ');
        // NonBlock::Nowait / SkipLocked render as "NOWAIT" / "SKIP LOCKED".
        out.push_str(&nb.to_string());
    }
    out
}

fn render_limit_clause(lc: &sqlparser::ast::LimitClause, river: usize) -> String {
    use sqlparser::ast::LimitClause;
    let mut out = String::new();
    match lc {
        LimitClause::LimitOffset { limit, offset, .. } => {
            if let Some(l) = limit {
                out.push('\n');
                out.push_str(&pad_kw("LIMIT", river));
                out.push(' ');
                out.push_str(&l.to_string());
            }
            if let Some(o) = offset {
                out.push('\n');
                out.push_str(&pad_kw("OFFSET", river));
                out.push(' ');
                out.push_str(&o.value.to_string());
            }
        }
        LimitClause::OffsetCommaLimit { offset, limit } => {
            out.push('\n');
            out.push_str(&pad_kw("LIMIT", river));
            out.push(' ');
            out.push_str(&limit.to_string());
            out.push('\n');
            out.push_str(&pad_kw("OFFSET", river));
            out.push(' ');
            out.push_str(&offset.to_string());
        }
    }
    out
}

fn render_with(with: &With, river: usize) -> String {
    // CTE list aligns at `river + 1` leading spaces (col river+2 — same
    // column where SELECT projections start).
    let cte_indent = " ".repeat(river + 1);
    let parts: Vec<String> = with.cte_tables.iter().map(|c| render_cte(c, river)).collect();
    let joined = parts.join(&format!(",\n{}", cte_indent));
    if with.recursive {
        format!("{}\n{}{}", pad_kw("WITH RECURSIVE", river), cte_indent, joined)
    } else {
        format!("{} {}", pad_kw("WITH", river), joined)
    }
}

fn render_cte(cte: &Cte, outer_river: usize) -> String {
    // The CTE body computes its OWN river (independent scope) but its
    // text gets indented inside the outer query by `outer_river + 1` spaces.
    let body = render_query(&cte.query);
    let indent = " ".repeat(outer_river + 1);
    let body_indented: String = body
        .lines()
        .map(|l| format!("{}{}", indent, l))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{} AS (\n{}\n{})", cte.alias, body_indented, indent)
}

fn render_set_expr(e: &SetExpr, river: usize) -> String {
    match e {
        SetExpr::Select(s) => render_select(s, river),
        SetExpr::Query(q) => render_query(q),
        SetExpr::SetOperation { op, set_quantifier, left, right } => {
            let l = render_set_expr(left, river);
            let r = render_set_expr(right, river);
            let op_name = match op {
                SetOperator::Union     => "UNION",
                SetOperator::Intersect => "INTERSECT",
                SetOperator::Except    => "EXCEPT",
                SetOperator::Minus     => "MINUS",
            };
            let q_str = set_quantifier.to_string();
            let q_trim = q_str.trim();
            let kw = if q_trim.is_empty() || q_trim.eq_ignore_ascii_case("NONE") {
                op_name.to_string()
            } else {
                format!("{} {}", op_name, q_trim)
            };
            format!("{}\n{}\n{}", l, pad_kw(&kw, river), r)
        }
        _ => e.to_string(),
    }
}

fn render_select(s: &Select, river: usize) -> String {
    let mut out = String::new();

    let select_kw = match &s.distinct {
        Some(d) => format!("SELECT {}", d),
        None => "SELECT".to_string(),
    };
    out.push_str(&pad_kw(&select_kw, river));

    let proj_strs: Vec<String> = s.projection.iter().map(|p| render_select_item(p, river)).collect();
    if !proj_strs.is_empty() {
        out.push(' ');
        out.push_str(&proj_strs[0]);
        let indent = " ".repeat(river + 1);
        for p in proj_strs.iter().skip(1) {
            out.push(',');
            out.push('\n');
            out.push_str(&indent);
            out.push_str(p);
        }
    }

    if !s.from.is_empty() {
        out.push('\n');
        out.push_str(&render_from(&s.from, river));
    }

    if let Some(w) = &s.selection {
        out.push('\n');
        out.push_str(&render_filter_chain("WHERE", w, river));
    }

    if let Some(gb) = render_group_by(&s.group_by, river) {
        out.push('\n');
        out.push_str(&gb);
    }

    if let Some(h) = &s.having {
        out.push('\n');
        out.push_str(&render_filter_chain("HAVING", h, river));
    }

    // WINDOW (named window definitions) and QUALIFY. Dialects differ on
    // order: PG / SQL-standard put WINDOW first; BigQuery puts QUALIFY
    // first. sqlparser records the parsed order in `window_before_qualify`.
    let emit_window = !s.named_window.is_empty();
    let emit_qualify = s.qualify.is_some();
    let window_first = s.window_before_qualify || !emit_qualify;
    if emit_window && window_first {
        out.push('\n');
        out.push_str(&render_window_clause(&s.named_window, river));
    }
    if let Some(q) = &s.qualify {
        out.push('\n');
        out.push_str(&render_filter_chain("QUALIFY", q, river));
    }
    if emit_window && !window_first {
        out.push('\n');
        out.push_str(&render_window_clause(&s.named_window, river));
    }

    out
}

fn render_window_clause(windows: &[NamedWindowDefinition], river: usize) -> String {
    // `WINDOW w1 AS (PARTITION BY x), w2 AS (...)` — like a WITH list,
    // subsequent definitions align at col river+2.
    let indent = " ".repeat(river + 1);
    let parts: Vec<String> = windows.iter().map(|nw| nw.to_string()).collect();
    let joined = parts.join(&format!(",\n{}", indent));
    format!("{} {}", pad_kw("WINDOW", river), joined)
}

fn render_select_item(item: &SelectItem, river: usize) -> String {
    let (expr, alias_suffix) = match item {
        SelectItem::UnnamedExpr(e) => (e, String::new()),
        SelectItem::ExprWithAlias { expr, alias } => (expr, format!(" AS {}", alias)),
        _ => return item.to_string(),
    };
    if let Expr::Function(f) = expr {
        if let Some(WindowType::WindowSpec(spec)) = &f.over {
            return render_window_function(f, spec, &alias_suffix, river);
        }
    }
    item.to_string()
}

/// Render a windowed function. The function call ends the projection line;
/// `OVER (…)` starts a fresh line right-aligned to the outer River.
/// Single-clause OVER stays inline; multi-clause splits with its own
/// dynamic internal river.
fn render_window_function(f: &Function, spec: &WindowSpec, alias_suffix: &str, river: usize) -> String {
    let mut head_fn = f.clone();
    head_fn.over = None;
    let head = head_fn.to_string();

    // OVER right-aligned to `river` (it's 4 chars).
    let over_kw = pad_kw("OVER", river);

    let mut clauses: Vec<(String, String)> = Vec::new();
    if !spec.partition_by.is_empty() {
        let body = spec.partition_by.iter().map(|e| e.to_string())
            .collect::<Vec<_>>().join(", ");
        clauses.push(("PARTITION BY".into(), body));
    }
    if !spec.order_by.is_empty() {
        let body = spec.order_by.iter().map(|e| e.to_string())
            .collect::<Vec<_>>().join(", ");
        clauses.push(("ORDER BY".into(), body));
    }
    if let Some(frame) = &spec.window_frame {
        let units = frame.units.to_string();
        let body  = match &frame.end_bound {
            Some(end) => format!("BETWEEN {} AND {}", frame.start_bound, end),
            None      => frame.start_bound.to_string(),
        };
        clauses.push((units, body));
    }

    if clauses.is_empty() {
        return format!("{}\n{} (){}", head, over_kw, alias_suffix);
    }

    if clauses.len() == 1 {
        let (kw, body) = &clauses[0];
        let inner = if body.is_empty() { kw.clone() } else { format!("{} {}", kw, body) };
        return format!("{}\n{} ({}){}", head, over_kw, inner, alias_suffix);
    }

    // Multi-clause: first clause inline after `OVER (`. The prefix is
    // `<river-4 spaces>OVER ` (river+1 chars total before `(`); the `(`
    // sits at col river+2, first sub-keyword starts at col river+3.
    let prefix_chars = river + 2; // length of "  OVER ("-equivalent for this river
    let first_first_word_len = first_word_len(&clauses[0].0);
    // First sub-keyword's first word ends at col `prefix_chars + first_word_len`.
    let first_word_end_col = prefix_chars + first_first_word_len;

    let mut out = String::new();
    out.push_str(&head);
    out.push('\n');
    out.push_str(&over_kw);
    out.push_str(" (");
    out.push_str(&clauses[0].0);
    if !clauses[0].1.is_empty() {
        out.push(' ');
        out.push_str(&clauses[0].1);
    }
    for (kw, body) in clauses.iter().skip(1) {
        let kw_first_word_len = first_word_len(kw);
        let lead = first_word_end_col.saturating_sub(kw_first_word_len);
        out.push('\n');
        out.push_str(&" ".repeat(lead));
        out.push_str(kw);
        if !body.is_empty() {
            out.push(' ');
            out.push_str(body);
        }
    }
    out.push('\n');
    // Closing `)` at the same indent as the function call (col river+2,
    // so river+1 leading spaces).
    out.push_str(&" ".repeat(river + 1));
    out.push(')');
    out.push_str(alias_suffix);
    out
}

fn render_from(from: &[TableWithJoins], river: usize) -> String {
    let mut out = String::new();
    for (i, tw) in from.iter().enumerate() {
        if i == 0 {
            out.push_str(&pad_kw("FROM", river));
            out.push(' ');
            out.push_str(&tw.relation.to_string());
        } else {
            out.push_str(",\n");
            out.push_str(&" ".repeat(river + 1));
            out.push_str(&tw.relation.to_string());
        }
        for j in &tw.joins {
            out.push('\n');
            out.push_str(&render_join(j, river));
        }
    }
    out
}

fn render_join(j: &Join, river: usize) -> String {
    use JoinOperator::*;
    let constraint = match &j.join_operator {
        Join(c) | Inner(c) | Left(c) | LeftOuter(c) | Right(c) | RightOuter(c)
        | FullOuter(c) | CrossJoin(c) | Semi(c) | LeftSemi(c) | RightSemi(c)
        | Anti(c) | LeftAnti(c) | RightAnti(c) | StraightJoin(c) => Some(c),
        _ => None,
    };
    // Niche dialect variants fall back to sqlparser's Display.
    if matches!(&j.join_operator, AsOf { .. } | ArrayJoin | LeftArrayJoin | InnerArrayJoin) {
        return j.to_string();
    }
    let kw = join_keyword_str(j);

    let mut out = pad_kw(&kw, river);
    out.push(' ');
    out.push_str(&j.relation.to_string());

    if let Some(c) = constraint {
        match c {
            JoinConstraint::On(expr) => {
                out.push('\n');
                out.push_str(&pad_kw("ON", river));
                out.push(' ');
                out.push_str(&expr.to_string());
            }
            JoinConstraint::Using(cols) => {
                out.push('\n');
                out.push_str(&pad_kw("USING", river));
                out.push_str(" (");
                out.push_str(
                    &cols.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", "),
                );
                out.push(')');
            }
            // NATURAL was already folded into the keyword.
            JoinConstraint::Natural | JoinConstraint::None => {}
        }
    }
    out
}

fn render_filter_chain(keyword: &str, expr: &Expr, river: usize) -> String {
    let mut leaves: Vec<(Option<&'static str>, &Expr)> = Vec::new();
    collect_and_or(expr, &mut leaves);
    let mut out = String::new();
    for (i, (op, leaf)) in leaves.iter().enumerate() {
        if i == 0 {
            out.push_str(&pad_kw(keyword, river));
            out.push(' ');
            out.push_str(&render_leaf(leaf, river));
        } else {
            out.push('\n');
            out.push_str(&pad_kw(op.unwrap_or("AND"), river));
            out.push(' ');
            out.push_str(&render_leaf(leaf, river));
        }
    }
    out
}

/// Render a single AND/OR leaf. Short leaves stay inline. Long leaves that
/// wrap an AND/OR chain in `(...)` explode with the open paren on the
/// caller's connector line and the close paren glued to the last sub-leaf:
/// ```text
///    AND (sub1
///     OR  sub2
///     OR  sub3)
/// ```
/// Sub-connector right-aligns to the same `river` as the outer chain; two
/// trailing spaces push content to col `river+3` (one past where the
/// open paren consumed).
fn render_leaf(expr: &Expr, river: usize) -> String {
    let inline = expr.to_string();
    if inline.len() <= MAX_LEAF_INLINE {
        return inline;
    }
    if let Expr::Nested(inner) = expr {
        let mut subs: Vec<(Option<&'static str>, &Expr)> = Vec::new();
        collect_and_or(inner, &mut subs);
        if subs.len() >= 2 {
            let mut out = String::from("(");
            for (i, (op, sub)) in subs.iter().enumerate() {
                if i == 0 {
                    out.push_str(&sub.to_string());
                } else {
                    out.push('\n');
                    out.push_str(&pad_kw(op.unwrap_or("AND"), river));
                    out.push_str("  ");
                    out.push_str(&sub.to_string());
                }
            }
            out.push(')');
            return out;
        }
    }
    inline
}

fn collect_and_or<'a>(expr: &'a Expr, out: &mut Vec<(Option<&'static str>, &'a Expr)>) {
    use sqlparser::ast::BinaryOperator;
    if let Expr::BinaryOp { left, op, right } = expr {
        let connector = match op {
            BinaryOperator::And => Some("AND"),
            BinaryOperator::Or => Some("OR"),
            _ => None,
        };
        if let Some(name) = connector {
            collect_and_or(left, out);
            let mut right_leaves: Vec<(Option<&'static str>, &'a Expr)> = Vec::new();
            collect_and_or(right, &mut right_leaves);
            if let Some(&(_, first_expr)) = right_leaves.first() {
                out.push((Some(name), first_expr));
                for (sub_op, sub_expr) in right_leaves.into_iter().skip(1) {
                    out.push((sub_op, sub_expr));
                }
            }
            return;
        }
    }
    out.push((None, expr));
}

fn render_group_by(gb: &GroupByExpr, river: usize) -> Option<String> {
    match gb {
        GroupByExpr::Expressions(exprs, modifiers) if !exprs.is_empty() => {
            let kw = if modifiers.is_empty() {
                "GROUP BY".to_string()
            } else {
                let mods = modifiers.iter().map(|m| m.to_string())
                    .collect::<Vec<_>>().join(" ");
                format!("GROUP BY {}", mods)
            };
            let mut out = pad_kw(&kw, river);
            let indent = " ".repeat(river + 1);
            for (i, e) in exprs.iter().enumerate() {
                out.push('\n');
                out.push_str(&indent);
                out.push_str(&e.to_string());
                if i < exprs.len() - 1 {
                    out.push(',');
                }
            }
            Some(out)
        }
        GroupByExpr::All(_) => Some(pad_kw("GROUP BY ALL", river)),
        _ => None,
    }
}

fn render_order_by(order: &sqlparser::ast::OrderBy, river: usize) -> String {
    let mut out = pad_kw("ORDER BY", river);
    let indent = " ".repeat(river + 1);
    let exprs = order_by_exprs(order);
    for (i, e) in exprs.iter().enumerate() {
        out.push('\n');
        out.push_str(&indent);
        out.push_str(e);
        if i < exprs.len() - 1 {
            out.push(',');
        }
    }
    out
}

/// Best-effort extraction of OrderBy expression strings — sqlparser's
/// OrderBy field shapes have shifted across versions, so we lean on its
/// Display and split on `, `.
fn order_by_exprs(order: &sqlparser::ast::OrderBy) -> Vec<String> {
    let s = order.to_string();
    let body = s.trim_start_matches("ORDER BY").trim_start();
    body.split(", ").map(|s| s.to_string()).collect()
}

// ═══════════════════════════════════════════════════════════════════════
// Block style — sibling to River. Keywords left-aligned at column 0, each
// clause on its own line, content indented `BLOCK_INDENT` spaces underneath.
// This is the de-facto standard SQL formatting (sqlfluff, pgFormatter,
// DBeaver, DataGrip, sqlformat.org). See the file header for v1 scope —
// Block mirrors River's (SELECT-only, multi-statement join, trailing-
// whitespace preservation).
// ═══════════════════════════════════════════════════════════════════════

/// Fixed indent for content under a clause keyword. 4 spaces matches
/// sqlfluff / Mozilla / GitLab style guides. Configurable per-connection
/// is a future enhancement.
const BLOCK_INDENT: &str = "    ";

/// Parse and reformat as Block style. Same dispatch pattern as
/// [`format_river`]: non-SELECT statements bail the whole input.
pub fn format_block(sql: &str, dialect: &dyn Dialect) -> Option<String> {
    let stmts = Parser::parse_sql(dialect, sql).ok()?;
    if stmts.is_empty() { return None; }
    let mut parts: Vec<String> = Vec::with_capacity(stmts.len());
    for stmt in &stmts {
        match stmt {
            Statement::Query(q) => parts.push(render_query_block(q)),
            _ => return None,
        }
    }
    let middle = parts.join(";\n");
    let trimmed = sql.trim_end();
    let trailing_ws = &sql[trimmed.len()..];
    let trailing_semi = if trimmed.ends_with(';') { ";" } else { "" };
    Some(format!("{}{}{}", middle, trailing_semi, trailing_ws))
}

fn render_query_block(q: &Query) -> String {
    let mut out = String::new();
    if let Some(with) = &q.with {
        out.push_str(&render_with_block(with));
        out.push('\n');
    }
    out.push_str(&render_set_expr_block(&q.body));
    if let Some(order) = &q.order_by {
        out.push('\n');
        out.push_str(&render_order_by_block(order));
    }
    if let Some(lc) = &q.limit_clause {
        out.push_str(&render_limit_clause_block(lc));
    }
    if let Some(fetch) = &q.fetch {
        out.push('\n');
        out.push_str(&render_fetch_block(fetch));
    }
    for lock in &q.locks {
        out.push('\n');
        out.push_str(&render_lock_clause_block(lock));
    }
    out
}

fn render_set_expr_block(e: &SetExpr) -> String {
    match e {
        SetExpr::Select(s) => render_select_block(s),
        SetExpr::Query(q) => render_query_block(q),
        SetExpr::SetOperation { op, set_quantifier, left, right } => {
            let l = render_set_expr_block(left);
            let r = render_set_expr_block(right);
            let op_name = match op {
                SetOperator::Union     => "UNION",
                SetOperator::Intersect => "INTERSECT",
                SetOperator::Except    => "EXCEPT",
                SetOperator::Minus     => "MINUS",
            };
            let q_str = set_quantifier.to_string();
            let q_trim = q_str.trim();
            let kw = if q_trim.is_empty() || q_trim.eq_ignore_ascii_case("NONE") {
                op_name.to_string()
            } else {
                format!("{} {}", op_name, q_trim)
            };
            format!("{}\n{}\n{}", l, kw, r)
        }
        _ => e.to_string(),
    }
}

fn render_with_block(with: &With) -> String {
    let kw = if with.recursive { "WITH RECURSIVE" } else { "WITH" };
    let parts: Vec<String> = with.cte_tables.iter().map(render_cte_block).collect();
    // CTEs separated by `,\n` — each definition starts at column 0 like
    // the WITH keyword above it. Matches sqlfluff's default.
    let joined = parts.join(",\n");
    format!("{} {}", kw, joined)
}

fn render_cte_block(cte: &Cte) -> String {
    let body = render_query_block(&cte.query);
    let body_indented: String = body
        .lines()
        .map(|l| format!("{}{}", BLOCK_INDENT, l))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{} AS (\n{}\n)", cte.alias, body_indented)
}

fn render_select_block(s: &Select) -> String {
    let mut out = String::new();
    let select_kw = match &s.distinct {
        Some(d) => format!("SELECT {}", d),
        None => "SELECT".to_string(),
    };
    out.push_str(&select_kw);
    let proj_strs: Vec<String> = s.projection.iter().map(|p| p.to_string()).collect();
    if !proj_strs.is_empty() {
        // Single projection inline; multi go one-per-line indented.
        if proj_strs.len() == 1 {
            out.push(' ');
            out.push_str(&proj_strs[0]);
        } else {
            for (i, p) in proj_strs.iter().enumerate() {
                out.push('\n');
                out.push_str(BLOCK_INDENT);
                out.push_str(p);
                if i < proj_strs.len() - 1 {
                    out.push(',');
                }
            }
        }
    }

    if !s.from.is_empty() {
        out.push('\n');
        out.push_str(&render_from_block(&s.from));
    }

    if let Some(w) = &s.selection {
        out.push('\n');
        out.push_str(&render_filter_chain_block("WHERE", w));
    }

    if let Some(gb) = render_group_by_block(&s.group_by) {
        out.push('\n');
        out.push_str(&gb);
    }

    if let Some(h) = &s.having {
        out.push('\n');
        out.push_str(&render_filter_chain_block("HAVING", h));
    }

    // WINDOW / QUALIFY ordering: same dialect-dependent logic as River.
    let emit_window = !s.named_window.is_empty();
    let emit_qualify = s.qualify.is_some();
    let window_first = s.window_before_qualify || !emit_qualify;
    if emit_window && window_first {
        out.push('\n');
        out.push_str(&render_window_clause_block(&s.named_window));
    }
    if let Some(q) = &s.qualify {
        out.push('\n');
        out.push_str(&render_filter_chain_block("QUALIFY", q));
    }
    if emit_window && !window_first {
        out.push('\n');
        out.push_str(&render_window_clause_block(&s.named_window));
    }

    out
}

fn render_window_clause_block(windows: &[NamedWindowDefinition]) -> String {
    let mut out = String::from("WINDOW");
    for (i, nw) in windows.iter().enumerate() {
        out.push('\n');
        out.push_str(BLOCK_INDENT);
        out.push_str(&nw.to_string());
        if i < windows.len() - 1 {
            out.push(',');
        }
    }
    out
}

fn render_from_block(from: &[TableWithJoins]) -> String {
    let mut out = String::new();
    for (i, tw) in from.iter().enumerate() {
        if i == 0 {
            out.push_str("FROM ");
            out.push_str(&tw.relation.to_string());
        } else {
            out.push_str(",\n");
            out.push_str(BLOCK_INDENT);
            out.push_str(&tw.relation.to_string());
        }
        for j in &tw.joins {
            out.push('\n');
            out.push_str(&render_join_block(j));
        }
    }
    out
}

fn render_join_block(j: &Join) -> String {
    use JoinOperator::*;
    let constraint = match &j.join_operator {
        Join(c) | Inner(c) | Left(c) | LeftOuter(c) | Right(c) | RightOuter(c)
        | FullOuter(c) | CrossJoin(c) | Semi(c) | LeftSemi(c) | RightSemi(c)
        | Anti(c) | LeftAnti(c) | RightAnti(c) | StraightJoin(c) => Some(c),
        _ => None,
    };
    if matches!(&j.join_operator, AsOf { .. } | ArrayJoin | LeftArrayJoin | InnerArrayJoin) {
        return j.to_string();
    }
    let kw = join_keyword_str(j);
    let mut out = String::new();
    out.push_str(&kw);
    out.push(' ');
    out.push_str(&j.relation.to_string());
    if let Some(c) = constraint {
        match c {
            JoinConstraint::On(expr) => {
                // ON inline keeps short joins on one line: `JOIN x ON x.id = y.id`
                out.push_str(" ON ");
                out.push_str(&expr.to_string());
            }
            JoinConstraint::Using(cols) => {
                out.push_str(" USING (");
                out.push_str(
                    &cols.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", "),
                );
                out.push(')');
            }
            JoinConstraint::Natural | JoinConstraint::None => {}
        }
    }
    out
}

/// WHERE / HAVING / QUALIFY chain. Keyword on its own line; first leaf
/// on the next line indented `BLOCK_INDENT`; subsequent AND / OR
/// connectors start their own line at the same indent, with the
/// connector keyword + space + leaf.
fn render_filter_chain_block(keyword: &str, expr: &Expr) -> String {
    let mut leaves: Vec<(Option<&'static str>, &Expr)> = Vec::new();
    collect_and_or(expr, &mut leaves);
    let mut out = String::from(keyword);
    for (i, (op, leaf)) in leaves.iter().enumerate() {
        out.push('\n');
        out.push_str(BLOCK_INDENT);
        if i > 0 {
            out.push_str(op.unwrap_or("AND"));
            out.push(' ');
        }
        out.push_str(&render_block_leaf(leaf));
    }
    out
}

/// Short leaves stay inline. Long parenthesized AND/OR chains explode
/// with the open paren on the connector line, sub-leaves indented one
/// additional `BLOCK_INDENT` level, close paren glued to the last sub-leaf.
fn render_block_leaf(expr: &Expr) -> String {
    let inline = expr.to_string();
    if inline.len() <= MAX_LEAF_INLINE {
        return inline;
    }
    if let Expr::Nested(inner) = expr {
        let mut subs: Vec<(Option<&'static str>, &Expr)> = Vec::new();
        collect_and_or(inner, &mut subs);
        if subs.len() >= 2 {
            let mut out = String::from("(");
            for (i, (op, sub)) in subs.iter().enumerate() {
                if i == 0 {
                    out.push_str(&sub.to_string());
                } else {
                    out.push('\n');
                    out.push_str(BLOCK_INDENT);
                    out.push_str(BLOCK_INDENT);
                    out.push_str(op.unwrap_or("AND"));
                    out.push(' ');
                    out.push_str(&sub.to_string());
                }
            }
            out.push(')');
            return out;
        }
    }
    inline
}

fn render_group_by_block(gb: &GroupByExpr) -> Option<String> {
    match gb {
        GroupByExpr::Expressions(exprs, modifiers) if !exprs.is_empty() => {
            let kw = if modifiers.is_empty() {
                "GROUP BY".to_string()
            } else {
                let mods = modifiers.iter().map(|m| m.to_string())
                    .collect::<Vec<_>>().join(" ");
                format!("GROUP BY {}", mods)
            };
            let mut out = kw;
            for (i, e) in exprs.iter().enumerate() {
                out.push('\n');
                out.push_str(BLOCK_INDENT);
                out.push_str(&e.to_string());
                if i < exprs.len() - 1 {
                    out.push(',');
                }
            }
            Some(out)
        }
        GroupByExpr::All(_) => Some("GROUP BY ALL".to_string()),
        _ => None,
    }
}

fn render_order_by_block(order: &sqlparser::ast::OrderBy) -> String {
    let exprs = order_by_exprs(order);
    let mut out = String::from("ORDER BY");
    for (i, e) in exprs.iter().enumerate() {
        out.push('\n');
        out.push_str(BLOCK_INDENT);
        out.push_str(e);
        if i < exprs.len() - 1 {
            out.push(',');
        }
    }
    out
}

fn render_limit_clause_block(lc: &sqlparser::ast::LimitClause) -> String {
    use sqlparser::ast::LimitClause;
    let mut out = String::new();
    match lc {
        LimitClause::LimitOffset { limit, offset, .. } => {
            if let Some(l) = limit {
                out.push_str("\nLIMIT ");
                out.push_str(&l.to_string());
            }
            if let Some(o) = offset {
                out.push_str("\nOFFSET ");
                out.push_str(&o.value.to_string());
            }
        }
        LimitClause::OffsetCommaLimit { offset, limit } => {
            out.push_str("\nLIMIT ");
            out.push_str(&limit.to_string());
            out.push_str("\nOFFSET ");
            out.push_str(&offset.to_string());
        }
    }
    out
}

fn render_fetch_block(fetch: &Fetch) -> String {
    let mut out = String::from("FETCH FIRST");
    if let Some(qty) = &fetch.quantity {
        out.push(' ');
        out.push_str(&qty.to_string());
        if fetch.percent {
            out.push_str(" PERCENT");
        }
    }
    out.push_str(" ROWS");
    out.push_str(if fetch.with_ties { " WITH TIES" } else { " ONLY" });
    out
}

fn render_lock_clause_block(lock: &LockClause) -> String {
    let mut out = String::from("FOR ");
    out.push_str(&lock.lock_type.to_string());
    if let Some(of) = &lock.of {
        out.push_str(" OF ");
        out.push_str(&of.to_string());
    }
    if let Some(nb) = &lock.nonblock {
        out.push(' ');
        out.push_str(&nb.to_string());
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════
// Inline style — collapse each statement to a single line via sqlparser's
// `Display` impl. Multi-statement inputs join with `; ` so the whole
// batch becomes one line. Works on any statement type (not SELECT-only)
// because Display already handles every node in the AST.
// ═══════════════════════════════════════════════════════════════════════

pub fn format_inline(sql: &str, dialect: &dyn Dialect) -> Option<String> {
    let stmts = Parser::parse_sql(dialect, sql).ok()?;
    if stmts.is_empty() { return None; }
    let parts: Vec<String> = stmts.iter().map(|s| s.to_string()).collect();
    let middle = parts.join("; ");
    // Preserve the user's terminator + trailing whitespace verbatim, same
    // as River / Block. Without this, an input ending in `;\n` would
    // round-trip to `<stmt>` (the parser eats both) and the cursor would
    // jump backwards.
    let trimmed = sql.trim_end();
    let trailing_ws = &sql[trimmed.len()..];
    let trailing_semi = if trimmed.ends_with(';') { ";" } else { "" };
    Some(format!("{}{}{}", middle, trailing_semi, trailing_ws))
}
