//! Tab-completion engine for the shell input.
//!
//! The shell calls [`analyze_context`] on Tab press to figure out what kind
//! of completion (if any) is appropriate at the cursor, then
//! [`gather_candidates`] to materialize the candidate list. The popup UI in
//! `main.rs` renders the result.
//!
//! v1 scope: meta-commands (`\d<tab>`), SQL keywords / functions
//! (`SEL<tab>`), and table names (after `FROM`/`JOIN`/`INSERT INTO`/
//! `UPDATE` or following a `<schema>.` qualifier). Column completion is
//! out of scope until an alias-aware parser lands.
//!
//! The context analyzer walks the input from the start to the cursor,
//! tracking quote / comment / dollar-quote state so it bails when the
//! cursor sits inside a string, line comment, block comment, or
//! `$tag$ … $tag$` body. That mirrors `tyto/src/parser::split_statements`.

use crate::plane::DbmsPlane;

/// What kind of completion applies at the cursor.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CompletionContext {
    /// Cursor sits in (or just after) a backslash-prefixed token. Candidates
    /// are the plane's meta-command names.
    MetaCommand { prefix: String },
    /// Cursor sits in (or just after) an identifier-shaped token that's not
    /// a meta-command and not following a `.`. Candidates are SQL keywords
    /// and functions.
    Keyword { prefix: String },
    /// Cursor sits in (or just after) an identifier-shaped token that
    /// follows a table-introducing keyword (FROM / JOIN / UPDATE / INTO).
    /// Candidates are unqualified tables from the connection's cache.
    Table { prefix: String },
    /// Cursor sits in (or just after) an identifier-shaped token that
    /// follows `SELECT`. Candidates blend tables (so the user can pick
    /// a table to qualify columns against next via `<table>.<col>`)
    /// with keywords / functions. Empty prefix surfaces tables only —
    /// the full keyword list with no filter is too noisy to be useful.
    TableOrKeyword { prefix: String },
    /// Cursor sits in (or just after) an identifier-shaped token that
    /// FOLLOWS a `<schema>.` qualifier. Candidates are tables/views in
    /// that schema.
    TableInSchema { schema: String, prefix: String },
}

/// What [`analyze_context`] returns when the cursor sits in a completable
/// position. `replace_start` is the byte offset where the prefix begins —
/// accepting a candidate replaces `input[replace_start..cursor]` with the
/// candidate text.
#[derive(Debug, Clone)]
pub(crate) struct CompletionAnalysis {
    pub(crate) context:       CompletionContext,
    pub(crate) replace_start: usize,
}

/// Walk `input[..cursor]` tracking quote / comment / dollar-quote state.
/// Returns `None` when the cursor sits inside a string-or-comment
/// (let the browser's default Tab behaviour fire there).
pub(crate) fn analyze_context(
    input:           &str,
    cursor:          usize,
    hash_comments:   bool,
) -> Option<CompletionAnalysis> {
    let cursor = cursor.min(input.len());
    let head = &input[..cursor];

    // Walk forward to figure out whether we're sitting in a string / comment.
    // Mirrors `parser::split_statements` minus the actual statement splitting.
    let mut in_single   = false;
    let mut in_double   = false;
    let mut in_backtick = false;
    let mut in_block    = false;
    let mut in_line     = false;
    let mut in_dollar: Option<String> = None;

    let bytes = head.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];

        if in_line {
            if c == b'\n' { in_line = false; }
            i += 1;
            continue;
        }
        if in_block {
            if c == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(tag) = in_dollar.clone() {
            let close = format!("${}$", tag);
            if head[i..].starts_with(&close) {
                in_dollar = None;
                i += close.len();
                continue;
            }
            i += 1;
            continue;
        }
        if in_single {
            // PG / MySQL both double single-quotes to escape.
            if c == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if c == b'"' { in_double = false; }
            i += 1;
            continue;
        }
        if in_backtick {
            if c == b'`' { in_backtick = false; }
            i += 1;
            continue;
        }

        match c {
            b'\'' => { in_single = true; i += 1; }
            b'"'  => { in_double = true; i += 1; }
            b'`'  => { in_backtick = true; i += 1; }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                in_line = true; i += 2;
            }
            b'#' if hash_comments => {
                in_line = true; i += 1;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                in_block = true; i += 2;
            }
            b'$' => {
                // Postgres dollar-quote: `$tag$` where tag is empty or
                // `[A-Za-z_0-9]*`. Anything else (e.g. `$1` placeholder)
                // falls through as a plain `$`.
                let mut end = i + 1;
                while end < bytes.len()
                    && (bytes[end] == b'_' || bytes[end].is_ascii_alphanumeric())
                {
                    end += 1;
                }
                if end < bytes.len() && bytes[end] == b'$' {
                    in_dollar = Some(head[i + 1..end].to_string());
                    i = end + 1;
                } else {
                    i += 1;
                }
            }
            _ => { i += 1; }
        }
    }

    if in_single || in_double || in_backtick || in_block || in_line || in_dollar.is_some() {
        return None;
    }

    // Cursor is outside any string/comment. Walk backwards from `cursor`
    // to find the start of the current identifier-shaped token. Backslash
    // commands are detected by the char immediately preceding the token.
    let bytes_full = input.as_bytes();
    let mut token_start = cursor;
    while token_start > 0 {
        let c = bytes_full[token_start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
            // `-` lets `safe-mode` complete as a single token. The leading
            // backslash check below still anchors it to a meta-command.
            token_start -= 1;
        } else {
            break;
        }
    }
    let token = &input[token_start..cursor];

    // Backslash-prefixed token → meta-command.
    if token_start > 0 && bytes_full[token_start - 1] == b'\\' {
        return Some(CompletionAnalysis {
            context:       CompletionContext::MetaCommand { prefix: format!("\\{}", token) },
            replace_start: token_start - 1,
        });
    }

    // Dot-qualified identifier → table in schema.
    if token_start > 0 && bytes_full[token_start - 1] == b'.' {
        let dot_at = token_start - 1;
        let mut schema_start = dot_at;
        while schema_start > 0 {
            let c = bytes_full[schema_start - 1];
            if c.is_ascii_alphanumeric() || c == b'_' {
                schema_start -= 1;
            } else {
                break;
            }
        }
        if schema_start < dot_at {
            return Some(CompletionAnalysis {
                context: CompletionContext::TableInSchema {
                    schema: input[schema_start..dot_at].to_string(),
                    prefix: token.to_string(),
                },
                replace_start: token_start,
            });
        }
    }

    // Plain identifier — disambiguate Keyword vs Table by looking at the
    // most recent SQL-keyword-shaped word before the token. FROM / JOIN /
    // UPDATE / INTO → Table context.
    if !token.is_empty() {
        // Walk backwards from token_start, skipping ASCII whitespace.
        let mut i = token_start;
        while i > 0 {
            let c = bytes_full[i - 1];
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                i -= 1;
            } else {
                break;
            }
        }
        let prev_end = i;
        while i > 0 {
            let c = bytes_full[i - 1];
            if c.is_ascii_alphanumeric() || c == b'_' {
                i -= 1;
            } else {
                break;
            }
        }
        let prev_upper = input[i..prev_end].to_ascii_uppercase();
        if matches!(prev_upper.as_str(), "FROM" | "JOIN" | "UPDATE" | "INTO") {
            return Some(CompletionAnalysis {
                context:       CompletionContext::Table { prefix: token.to_string() },
                replace_start: token_start,
            });
        }
        if prev_upper == "SELECT" {
            return Some(CompletionAnalysis {
                context:       CompletionContext::TableOrKeyword { prefix: token.to_string() },
                replace_start: token_start,
            });
        }
        return Some(CompletionAnalysis {
            context:       CompletionContext::Keyword { prefix: token.to_string() },
            replace_start: token_start,
        });
    }

    // Empty token, but maybe preceded by `FROM ` / `JOIN ` etc. directly.
    // Walk back over whitespace; if the previous word is a table-introducer,
    // return Table { prefix: "" } so the popup lists every table.
    let mut i = cursor;
    while i > 0 {
        let c = bytes_full[i - 1];
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            i -= 1;
        } else {
            break;
        }
    }
    let prev_end = i;
    while i > 0 {
        let c = bytes_full[i - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            i -= 1;
        } else {
            break;
        }
    }
    if i < prev_end {
        let prev_upper = input[i..prev_end].to_ascii_uppercase();
        if matches!(prev_upper.as_str(), "FROM" | "JOIN" | "UPDATE" | "INTO") {
            return Some(CompletionAnalysis {
                context:       CompletionContext::Table { prefix: String::new() },
                replace_start: cursor,
            });
        }
        if prev_upper == "SELECT" {
            return Some(CompletionAnalysis {
                context:       CompletionContext::TableOrKeyword { prefix: String::new() },
                replace_start: cursor,
            });
        }
    }

    None
}

/// Filter a column list (or projection) by a case-insensitive prefix,
/// sort, and dedup. Used by both Phase 1 (real-table columns) and
/// Phase 3 (CTE / inline-view projections). TODO(sort order): the
/// underlying fetch returns DDL-ordinal order; the sort here
/// alphabetizes. Worth a flag later — see the `gather_candidates` TODO.
fn filter_columns(cols: &[String], prefix: &str) -> Vec<String> {
    let prefix_lower = prefix.to_ascii_lowercase();
    let mut out: Vec<String> = cols.iter()
        .filter(|c| c.to_ascii_lowercase().starts_with(prefix_lower.as_str()))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// What `extract_aliases` returns: both the alias map for column
/// resolution AND the set of `(schema, table)` pairs that
/// star-expansion couldn't resolve because the table's columns
/// aren't loaded yet. Callers fire prefetches for the latter so the
/// next Tab gets a fully expanded projection.
#[derive(Debug, Clone, Default)]
pub(crate) struct ExtractedAliases {
    pub(crate) aliases:       std::collections::HashMap<String, AliasResolution>,
    pub(crate) stars_pending: std::collections::HashSet<(String, String)>,
}

/// How an alias resolves at completion time.
///
/// `Table` is the Phase 2 case — alias maps to a real `(schema, table)`
/// pair, columns come from the per-pane column cache once a fetch has
/// landed. `Projection` is the Phase 3 case — alias resolves to a
/// CTE body or inline-view's projected column list, computed
/// statically from the SQL itself (no fetch needed).
#[derive(Debug, Clone)]
pub(crate) enum AliasResolution {
    Table { schema: String, table: String },
    Projection { columns: Vec<String> },
}

/// Build a `{alias → resolution}` map from CTEs (WITH clause) and
/// FROM/JOIN clauses in `input`. Phase 2 / Phase 3 share this entry
/// point. Used by column completion so `SELECT u.<Tab> FROM users u`
/// and `WITH q AS (SELECT a FROM t) SELECT q.<Tab>` both produce the
/// right columns. Nested sub-queries inside FROM (top-level case for
/// inline views) get their projections extracted; deeper nesting is
/// out of scope for v1. When a table reference has no explicit alias,
/// the bare table name becomes its own key.
pub(crate) fn extract_aliases(
    input:          &str,
    default_schema: &str,
    columns:        &std::collections::HashMap<(String, String), Vec<String>>,
) -> ExtractedAliases {
    use std::collections::{HashMap, HashSet};
    const TERMINATORS: &[&str] = &[
        "WHERE", "GROUP", "ORDER", "HAVING", "LIMIT", "OFFSET",
        "UNION", "INTERSECT", "EXCEPT", "WINDOW", "QUALIFY", "FETCH",
        "RETURNING", "FOR",
    ];
    const JOIN_KWS: &[&str] = &[
        "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "OUTER",
        "NATURAL", "STRAIGHT_JOIN", "SEMI", "ANTI", "LATERAL",
    ];

    let toks = tokenize(input);
    let mut map: HashMap<String, AliasResolution> = HashMap::new();
    let mut pending: HashSet<(String, String)> = HashSet::new();

    // Phase 3: extract CTEs from a leading WITH clause.
    parse_with_clause(&toks, &mut map, default_schema, columns, &mut pending);

    // Phase 2 + inline views: walk every FROM clause.
    let mut i = 0;
    while i < toks.len() {
        if toks[i].kind == TokKind::Word && toks[i].text.eq_ignore_ascii_case("FROM") {
            i = parse_from_clause(
                &toks, i + 1, default_schema,
                &mut map, columns, &mut pending,
                TERMINATORS, JOIN_KWS,
            );
            continue;
        }
        i += 1;
    }
    ExtractedAliases { aliases: map, stars_pending: pending }
}

/// Recognize a leading `WITH [RECURSIVE] name [(col, ...)] AS (SELECT ...)`
/// possibly repeated by commas, and register each CTE name as a
/// `Projection` alias resolved to the inner SELECT's projection list.
/// Stars in the CTE body's projection are expanded against the body's
/// own FROM clause; tables needed but missing from the column cache
/// are added to `pending` for the caller to prefetch.
fn parse_with_clause(
    toks:           &[Tok<'_>],
    map:            &mut std::collections::HashMap<String, AliasResolution>,
    default_schema: &str,
    columns:        &std::collections::HashMap<(String, String), Vec<String>>,
    pending:        &mut std::collections::HashSet<(String, String)>,
) {
    // Find the first non-whitespace token. tokenize() drops whitespace,
    // so toks[0] is already the first real token (if any).
    let mut i = 0;
    if i >= toks.len() || toks[i].kind != TokKind::Word
        || !toks[i].text.eq_ignore_ascii_case("WITH") { return; }
    i += 1;
    // Optional RECURSIVE
    if i < toks.len() && toks[i].kind == TokKind::Word
        && toks[i].text.eq_ignore_ascii_case("RECURSIVE") { i += 1; }

    loop {
        // CTE name
        if i >= toks.len() || toks[i].kind != TokKind::Word { return; }
        let cte_name = toks[i].text.to_string();
        i += 1;
        // Optional column list: name (col1, col2, ...) AS (...)
        // Skip the parenthesized list — the projection comes from the
        // inner SELECT regardless. (A CTE that explicitly aliases its
        // projection via this list COULD use the list as the truth, but
        // v1 sticks with the SELECT-list extractor.)
        if i < toks.len() && toks[i].kind == TokKind::LParen {
            let mut depth = 1;
            i += 1;
            while i < toks.len() && depth > 0 {
                match toks[i].kind {
                    TokKind::LParen => depth += 1,
                    TokKind::RParen => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
        }
        // Expect AS
        if i >= toks.len() || toks[i].kind != TokKind::Word
            || !toks[i].text.eq_ignore_ascii_case("AS") { return; }
        i += 1;
        // Expect (
        if i >= toks.len() || toks[i].kind != TokKind::LParen { return; }
        // Find matching )
        let body_start = i + 1;
        let mut depth = 1;
        i += 1;
        let body_end;
        loop {
            if i >= toks.len() { return; }
            match toks[i].kind {
                TokKind::LParen => depth += 1,
                TokKind::RParen => {
                    depth -= 1;
                    if depth == 0 { body_end = i; i += 1; break; }
                }
                _ => {}
            }
            i += 1;
        }
        // Extract projection from the CTE body (with star expansion).
        let projection = extract_projection_resolved(
            &toks[body_start..body_end], default_schema, columns, pending,
        );
        map.insert(cte_name, AliasResolution::Projection { columns: projection });

        // Continue if comma — another CTE follows.
        if i < toks.len() && toks[i].kind == TokKind::Comma { i += 1; continue; }
        return;
    }
}

/// One raw projection item from a SELECT body, before star expansion.
/// `Star` means a bare `*`; `TableStar(name)` means `<name>.*`; `Named`
/// is the resolved column or AS-alias name.
#[derive(Debug, Clone)]
enum ProjectionItem {
    Named(String),
    Star,
    TableStar(String),
}

/// Resolve a sub-query body's projection into a flat list of column
/// names, expanding any `*` / `<alias>.*` against the body's own FROM
/// clause. Stars referencing tables whose columns haven't been fetched
/// yet add to `pending` so the caller can prefetch them. Bare `*` in a
/// SELECT with no FROM expands to nothing.
fn extract_projection_resolved(
    body_toks:      &[Tok<'_>],
    default_schema: &str,
    columns:        &std::collections::HashMap<(String, String), Vec<String>>,
    pending:        &mut std::collections::HashSet<(String, String)>,
) -> Vec<String> {
    // Find SELECT inside the body.
    let mut sel_at = None;
    for (i, t) in body_toks.iter().enumerate() {
        if t.kind == TokKind::Word && t.text.eq_ignore_ascii_case("SELECT") {
            sel_at = Some(i);
            break;
        }
    }
    let Some(sel_idx) = sel_at else { return Vec::new(); };
    let mut i = sel_idx + 1;
    // Optional DISTINCT / ALL
    if i < body_toks.len() && body_toks[i].kind == TokKind::Word
        && (body_toks[i].text.eq_ignore_ascii_case("DISTINCT")
            || body_toks[i].text.eq_ignore_ascii_case("ALL"))
    {
        i += 1;
    }
    let raw_items = parse_projection_items(&body_toks[i..]);

    // Fast path: no stars, no resolution needed.
    if !raw_items.iter().any(|it| matches!(it, ProjectionItem::Star | ProjectionItem::TableStar(_))) {
        return raw_items.into_iter().filter_map(|it| match it {
            ProjectionItem::Named(s) => Some(s),
            _ => None,
        }).collect();
    }

    // Star expansion: build a LOCAL alias map from the body's FROM
    // clauses. Recursive inline views inside the body don't get fully
    // resolved (their stars stay empty), but their NON-star projections
    // still surface via parse_from_clause.
    const TERMINATORS: &[&str] = &[
        "WHERE", "GROUP", "ORDER", "HAVING", "LIMIT", "OFFSET",
        "UNION", "INTERSECT", "EXCEPT", "WINDOW", "QUALIFY", "FETCH",
        "RETURNING", "FOR",
    ];
    const JOIN_KWS: &[&str] = &[
        "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "OUTER",
        "NATURAL", "STRAIGHT_JOIN", "SEMI", "ANTI", "LATERAL",
    ];
    let mut local_aliases = std::collections::HashMap::new();
    let mut j = 0;
    while j < body_toks.len() {
        if body_toks[j].kind == TokKind::Word && body_toks[j].text.eq_ignore_ascii_case("FROM") {
            j = parse_from_clause(
                body_toks, j + 1, default_schema,
                &mut local_aliases, columns, pending,
                TERMINATORS, JOIN_KWS,
            );
            continue;
        }
        j += 1;
    }

    let mut out: Vec<String> = Vec::new();
    for item in raw_items {
        match item {
            ProjectionItem::Named(s) => out.push(s),
            ProjectionItem::Star => {
                // Union of every local alias's resolved columns.
                for resolution in local_aliases.values() {
                    append_resolved_columns(resolution, columns, pending, &mut out);
                }
            }
            ProjectionItem::TableStar(qualifier) => {
                if let Some(resolution) = local_aliases.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(&qualifier))
                    .map(|(_, v)| v)
                {
                    append_resolved_columns(resolution, columns, pending, &mut out);
                }
            }
        }
    }
    out
}

fn append_resolved_columns(
    resolution: &AliasResolution,
    columns:    &std::collections::HashMap<(String, String), Vec<String>>,
    pending:    &mut std::collections::HashSet<(String, String)>,
    out:        &mut Vec<String>,
) {
    match resolution {
        AliasResolution::Table { schema, table } => {
            let key = (schema.clone(), table.clone());
            if let Some(cols) = columns.get(&key) {
                out.extend(cols.iter().cloned());
            } else {
                pending.insert(key);
            }
        }
        AliasResolution::Projection { columns: cols } => {
            out.extend(cols.iter().cloned());
        }
    }
}

fn parse_projection_items(toks: &[Tok<'_>]) -> Vec<ProjectionItem> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if toks[i].kind == TokKind::Word && toks[i].text.eq_ignore_ascii_case("FROM") {
            break;
        }
        let item_start = i;
        let mut paren_depth = 0;
        let mut item_end = item_start;
        while item_end < toks.len() {
            match toks[item_end].kind {
                TokKind::LParen => paren_depth += 1,
                TokKind::RParen if paren_depth > 0 => paren_depth -= 1,
                TokKind::RParen => break,
                TokKind::Comma if paren_depth == 0 => break,
                TokKind::Word if paren_depth == 0
                    && toks[item_end].text.eq_ignore_ascii_case("FROM") => break,
                _ => {}
            }
            item_end += 1;
        }
        if let Some(item) = parse_projection_item(&toks[item_start..item_end]) {
            out.push(item);
        }
        i = item_end;
        if i < toks.len() && toks[i].kind == TokKind::Comma { i += 1; } else { break; }
    }
    out
}

fn parse_projection_item(toks: &[Tok<'_>]) -> Option<ProjectionItem> {
    if toks.is_empty() { return None; }

    // Bare `*` projection.
    if toks.len() == 1 && toks[0].kind == TokKind::Star {
        return Some(ProjectionItem::Star);
    }
    // `<word>.*` — table-qualified star.
    if toks.len() == 3
        && toks[0].kind == TokKind::Word
        && toks[1].kind == TokKind::Dot
        && toks[2].kind == TokKind::Star
    {
        return Some(ProjectionItem::TableStar(toks[0].text.to_string()));
    }

    // Look for `AS` at top-level paren depth. If found, the next word is
    // the projection's alias and wins regardless of what came before.
    let mut paren_depth = 0;
    for (i, t) in toks.iter().enumerate() {
        match t.kind {
            TokKind::LParen => paren_depth += 1,
            TokKind::RParen => paren_depth -= 1,
            TokKind::Word if paren_depth == 0 && t.text.eq_ignore_ascii_case("AS") => {
                if i + 1 < toks.len() && toks[i + 1].kind == TokKind::Word {
                    return Some(ProjectionItem::Named(toks[i + 1].text.to_string()));
                }
                return None;
            }
            _ => {}
        }
    }
    // No AS — only accept bare column refs (`col`, `tbl.col`, `s.t.col`).
    // Anything with parens, operators, or stars (other than the two
    // patterns above) is too ambiguous to synthesize a name from; skip.
    let all_word_or_dot = toks.iter().all(|t| matches!(t.kind, TokKind::Word | TokKind::Dot));
    if !all_word_or_dot { return None; }
    for t in toks.iter().rev() {
        if t.kind == TokKind::Word { return Some(ProjectionItem::Named(t.text.to_string())); }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TokKind { Word, Dot, Comma, LParen, RParen, Star, Other }

#[derive(Debug, Clone)]
struct Tok<'a> { text: &'a str, kind: TokKind }

fn tokenize(input: &str) -> Vec<Tok<'_>> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            // -- line comment
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' { i += 1; }
            }
            // /* block */ comment
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') { i += 1; }
                if i + 1 < bytes.len() { i += 2; }
            }
            // 'string' — including doubled-quote escape
            b'\'' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\'' { i += 2; continue; }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            // "quoted_ident" / `quoted_ident` — skip contents
            b'"' | b'`' => {
                let q = c;
                i += 1;
                while i < bytes.len() && bytes[i] != q { i += 1; }
                if i < bytes.len() { i += 1; }
            }
            _ if c.is_ascii_alphanumeric() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                out.push(Tok { text: &input[start..i], kind: TokKind::Word });
            }
            b'.' => { out.push(Tok { text: ".", kind: TokKind::Dot }); i += 1; }
            b',' => { out.push(Tok { text: ",", kind: TokKind::Comma }); i += 1; }
            b'(' => { out.push(Tok { text: "(", kind: TokKind::LParen }); i += 1; }
            b')' => { out.push(Tok { text: ")", kind: TokKind::RParen }); i += 1; }
            b'*' => { out.push(Tok { text: "*", kind: TokKind::Star }); i += 1; }
            _ => { out.push(Tok { text: &input[i..i + 1], kind: TokKind::Other }); i += 1; }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn parse_from_clause(
    toks:           &[Tok<'_>],
    start:          usize,
    default_schema: &str,
    map:            &mut std::collections::HashMap<String, AliasResolution>,
    columns:        &std::collections::HashMap<(String, String), Vec<String>>,
    pending:        &mut std::collections::HashSet<(String, String)>,
    terminators:    &[&str],
    join_kws:       &[&str],
) -> usize {
    let mut i = start;
    while i < toks.len() {
        let t = &toks[i];

        // Stop at clause terminators or unmatched ')'.
        if t.kind == TokKind::Word && terminators.iter().any(|k| t.text.eq_ignore_ascii_case(k)) {
            return i;
        }
        if t.kind == TokKind::RParen { return i; }

        // Parenthesized sub-query in FROM (inline view). Extract the
        // inner SELECT's projection and stash the alias → Projection.
        // If the contents aren't a SELECT, we fall through to skipping
        // the parens entirely (a bare `(table)` reference, rare).
        if t.kind == TokKind::LParen {
            let body_start = i + 1;
            let mut depth = 1;
            i += 1;
            let body_end;
            loop {
                if i >= toks.len() { return i; }
                match toks[i].kind {
                    TokKind::LParen => depth += 1,
                    TokKind::RParen => {
                        depth -= 1;
                        if depth == 0 { body_end = i; i += 1; break; }
                    }
                    _ => {}
                }
                i += 1;
            }
            // Optional AS
            if i < toks.len() && toks[i].kind == TokKind::Word
                && toks[i].text.eq_ignore_ascii_case("AS")
            {
                i += 1;
            }
            // Pick up the alias if there is one.
            let alias = if i < toks.len() && toks[i].kind == TokKind::Word {
                let txt = toks[i].text;
                if !terminators.iter().any(|k| txt.eq_ignore_ascii_case(k))
                    && !join_kws.iter().any(|k|  txt.eq_ignore_ascii_case(k))
                    && !txt.eq_ignore_ascii_case("ON")
                    && !txt.eq_ignore_ascii_case("USING")
                {
                    let a = txt.to_string();
                    i += 1;
                    Some(a)
                } else { None }
            } else { None };
            // Extract projection from the parenthesized body if it
            // looks like a sub-select (has SELECT inside). Only register
            // the alias if both: alias was provided AND projection has
            // at least one column we could name. Empty projection (star
            // expansion deferred; function calls without AS) just
            // produces no map entry — falls through to "no popup" at
            // gather time.
            if let Some(alias) = alias {
                let cols = extract_projection_resolved(
                    &toks[body_start..body_end], default_schema, columns, pending,
                );
                if !cols.is_empty() {
                    map.insert(alias, AliasResolution::Projection { columns: cols });
                }
            }
            continue;
        }

        // JOIN, INNER, LEFT, etc. — skip; the table follows.
        if t.kind == TokKind::Word && join_kws.iter().any(|k| t.text.eq_ignore_ascii_case(k)) {
            i += 1;
            continue;
        }
        if t.kind == TokKind::Comma { i += 1; continue; }

        // ON ... condition — skip until next JOIN, comma, terminator, or ')'.
        if t.kind == TokKind::Word
            && (t.text.eq_ignore_ascii_case("ON") || t.text.eq_ignore_ascii_case("USING"))
        {
            i += 1;
            while i < toks.len() {
                match toks[i].kind {
                    TokKind::Word => {
                        let w = toks[i].text;
                        if join_kws.iter().any(|k| w.eq_ignore_ascii_case(k))
                           || terminators.iter().any(|k| w.eq_ignore_ascii_case(k))
                        { break; }
                        i += 1;
                    }
                    TokKind::Comma => break,
                    TokKind::RParen => return i,
                    TokKind::LParen => {
                        let mut depth = 1;
                        i += 1;
                        while i < toks.len() && depth > 0 {
                            match toks[i].kind {
                                TokKind::LParen => depth += 1,
                                TokKind::RParen => depth -= 1,
                                _ => {}
                            }
                            i += 1;
                        }
                    }
                    _ => i += 1,
                }
            }
            continue;
        }

        // Table reference: <word> [. <word>] [AS] [<word>]
        if t.kind == TokKind::Word {
            let head = t.text;
            i += 1;
            let (schema, table) = if i < toks.len() && toks[i].kind == TokKind::Dot {
                i += 1;
                if i < toks.len() && toks[i].kind == TokKind::Word {
                    let tbl = toks[i].text;
                    i += 1;
                    (head.to_string(), tbl.to_string())
                } else {
                    (default_schema.to_string(), head.to_string())
                }
            } else {
                (default_schema.to_string(), head.to_string())
            };
            // Optional AS
            if i < toks.len() && toks[i].kind == TokKind::Word && toks[i].text.eq_ignore_ascii_case("AS") {
                i += 1;
            }
            // Optional alias — must not be a JOIN keyword, terminator, ON, or USING
            let alias = if i < toks.len() && toks[i].kind == TokKind::Word {
                let w = toks[i].text;
                if !terminators.iter().any(|k| w.eq_ignore_ascii_case(k))
                   && !join_kws.iter().any(|k|  w.eq_ignore_ascii_case(k))
                   && !w.eq_ignore_ascii_case("ON")
                   && !w.eq_ignore_ascii_case("USING")
                {
                    let a = w.to_string();
                    i += 1;
                    Some(a)
                } else { None }
            } else { None };
            let key = alias.unwrap_or_else(|| table.clone());
            // Don't overwrite a CTE / inline-view Projection entry — a
            // bare `FROM q` reference to a CTE named `q` should still
            // resolve to the CTE's projected columns, not be re-bound
            // to a hypothetical real `q` table. Order independence:
            // works regardless of whether parse_with_clause ran first
            // (and FROM walk finds the CTE name later) or vice versa.
            if !matches!(map.get(&key), Some(AliasResolution::Projection { .. })) {
                map.insert(key, AliasResolution::Table { schema, table });
            }
            continue;
        }

        // Anything else — skip.
        i += 1;
    }
    i
}

/// Produce a candidate list for `ctx`. Filters by prefix and returns
/// sorted deduplicated strings. `tables` is the per-pane cache (schema
/// → table names); pass `None` when the cache is not yet loaded (Table
/// / TableInSchema contexts then return empty, which the caller treats
/// as "fire a fetch, retry on next Tab"). `current_db` scopes
/// unqualified Table completion to that database (MySQL) or schema
/// (PG search_path's primary). `aliases` carries the FROM/JOIN alias
/// map for the current input (Phase 2 column completion via alias).
///
/// TODO(sort order): the column branches currently re-sort
/// alphabetically; the underlying fetch returns ordinal (DDL declared)
/// order. Worth surfacing the ordinal order in the popup so users
/// see "the table's columns" as they were defined, not as the
/// alphabet wills.
pub(crate) fn gather_candidates(
    ctx:        &CompletionContext,
    plane:      &'static dyn DbmsPlane,
    tables:     Option<&std::collections::HashMap<String, Vec<String>>>,
    columns:    &std::collections::HashMap<(String, String), Vec<String>>,
    aliases:    &std::collections::HashMap<String, AliasResolution>,
    current_db: &str,
) -> Vec<String> {
    match ctx {
        CompletionContext::MetaCommand { prefix } => {
            plane.meta_command_names()
                .iter()
                .filter(|name| name.starts_with(prefix.as_str()))
                .map(|s| (*s).to_string())
                .collect()
        }
        CompletionContext::Keyword { prefix } => {
            // Match case-insensitively but emit the canonical case from the
            // plane's lists (already uppercase). Users typing `sel` get
            // `SELECT`; users typing `SEL` get `SELECT`. Either way the
            // canonical uppercase form lands in the input.
            let prefix_upper = prefix.to_ascii_uppercase();
            let mut out: Vec<String> = Vec::new();
            for kw in plane.keywords() {
                if kw.starts_with(prefix_upper.as_str()) {
                    out.push((*kw).to_string());
                }
            }
            for f in plane.functions() {
                if f.starts_with(prefix_upper.as_str()) {
                    out.push((*f).to_string());
                }
            }
            // Single-source-of-truth dedup: keywords and functions overlap
            // (e.g. `COUNT` is both in some dialect lists). Sorted -> dedup
            // works because both source lists are sorted.
            out.sort();
            out.dedup();
            out
        }
        CompletionContext::TableOrKeyword { prefix } => {
            // After SELECT — surface tables (so the user can pick one
            // and qualify columns next), plus keywords/functions when
            // there's a non-empty prefix to filter against. Empty prefix
            // skips keywords to keep the popup tractable; the user can
            // type a character to broaden.
            let mut out: Vec<String> = Vec::new();
            // Tables — scope to the default completion schema, same as
            // the plain Table branch.
            if let Some(tables) = tables {
                let default_schema = plane.default_completion_schema(current_db);
                let pool: Vec<&String> = if !default_schema.is_empty() {
                    tables.iter()
                        .filter(|(k, _)| k.eq_ignore_ascii_case(&default_schema))
                        .flat_map(|(_, v)| v.iter())
                        .collect()
                } else {
                    Vec::new()
                };
                let pool = if !pool.is_empty() {
                    pool
                } else {
                    tables.values().flat_map(|v| v.iter()).collect()
                };
                for t in pool {
                    if t.starts_with(prefix.as_str()) {
                        out.push(t.clone());
                    }
                }
            }
            // Keywords + functions only when the user has typed something
            // to filter against. With an empty prefix, every keyword in
            // the dialect would land in the popup; not useful.
            if !prefix.is_empty() {
                let prefix_upper = prefix.to_ascii_uppercase();
                for kw in plane.keywords() {
                    if kw.starts_with(prefix_upper.as_str()) {
                        out.push((*kw).to_string());
                    }
                }
                for f in plane.functions() {
                    if f.starts_with(prefix_upper.as_str()) {
                        out.push((*f).to_string());
                    }
                }
            }
            out.sort();
            out.dedup();
            out
        }
        CompletionContext::Table { prefix } => {
            let Some(tables) = tables else { return Vec::new(); };
            // Resolve "default schema for completion" via the plane:
            // MySQL → current_db (which IS the schema), PG → `public`
            // (since current_db is a database, not a schema). Fall back
            // to flattening every cached schema when the default one is
            // empty in the cache — keeps users with no `public` schema
            // (PG) or a fresh connection without a USE'd db (MySQL)
            // from getting an empty popup.
            let default_schema = plane.default_completion_schema(current_db);
            let from_default: Vec<&String> = if !default_schema.is_empty() {
                tables.iter()
                    .filter(|(k, _)| k.eq_ignore_ascii_case(&default_schema))
                    .flat_map(|(_, v)| v.iter())
                    .collect()
            } else {
                Vec::new()
            };
            let pool: Vec<&String> = if !from_default.is_empty() {
                from_default
            } else {
                tables.values().flat_map(|v| v.iter()).collect()
            };
            let mut out: Vec<String> = pool.into_iter()
                .filter(|t| t.starts_with(prefix.as_str()))
                .cloned()
                .collect();
            out.sort();
            out.dedup();
            out
        }
        CompletionContext::TableInSchema { schema, prefix } => {
            // Disambiguate the dot-qualified token:
            //   1. If `<schema>` is a real schema → list its tables
            //      (the v1 schema-qualified table-completion path).
            //   2. Else if `<schema>` is a TABLE in the default
            //      completion schema → list its columns (Phase 1
            //      column completion).
            //   3. Else → nothing (the user's "do nothing on `f.` when
            //      there's no table `f`" rule falls out here).
            // Tables are case-insensitive for the disambiguation since
            // PG identifiers are case-folded by default and MySQL is
            // case-insensitive on most platforms.
            let Some(tables) = tables else { return Vec::new(); };

            // Path 1: schema-qualified table list.
            if let Some(in_schema) = tables.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(schema))
                .map(|(_, v)| v)
            {
                let prefix_lower = prefix.to_ascii_lowercase();
                let mut out: Vec<String> = in_schema.iter()
                    .filter(|t| t.to_ascii_lowercase().starts_with(prefix_lower.as_str()))
                    .cloned()
                    .collect();
                out.sort();
                out.dedup();
                return out;
            }

            // Path 2: alias resolution (CTEs, inline views, FROM aliases).
            //
            // CTEs SHADOW real tables in actual SQL — if the user wrote
            // `WITH actor AS (...)`, then `actor.<col>` means the CTE's
            // projection, NOT the schema's actor table. So alias check
            // runs before the bare-table fallback. Aliases also resolve
            // explicit FROM aliases (`FROM users u` → `u.<col>` is users)
            // — same shape, just sourced from FROM/JOIN instead of WITH.
            if let Some((_, resolution)) = aliases.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(schema))
            {
                match resolution {
                    AliasResolution::Table { schema: s, table: t } => {
                        let key = (s.clone(), t.clone());
                        if let Some(cols) = columns.get(&key) {
                            return filter_columns(cols, prefix);
                        }
                        return Vec::new();
                    }
                    AliasResolution::Projection { columns: cols } => {
                        return filter_columns(cols, prefix);
                    }
                }
            }

            // Path 3: bare-table-qualified column list. The qualifier
            // matches a real table name in the default schema (e.g. the
            // user typed `SELECT film.<Tab>` with no FROM clause yet —
            // no alias map entry to find).
            let default_schema = plane.default_completion_schema(current_db);
            if let Some(canonical_table) = tables.get(default_schema.as_str())
                .and_then(|in_default| in_default.iter()
                    .find(|t| t.eq_ignore_ascii_case(schema))
                    .cloned())
            {
                let key = (default_schema.clone(), canonical_table);
                if let Some(cols) = columns.get(&key) {
                    return filter_columns(cols, prefix);
                }
            }
            Vec::new()
        }
    }
}
