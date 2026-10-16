//! PostgreSQL plane — skeleton.
//!
//! Phase 4 fills in the meta-commands + status SQL, Phase 5 fills in the
//! schema-explorer queries, and Phase 6 fills in the keyword/function
//! highlighting lists. The struct exists from Phase 2 so the trait dispatch
//! compiles and a user who saves a Postgres connection doesn't panic.

use shared::{DbType, EditorVar, LineFlagVar};
use crate::{AdminArea, AdminRow, MetaAction, ObjectKind, PaneKind, PaneMenuItem};
use crate::types::{Align, Expanded, LineStyle, OutputFormat, PrintOptions, PrintedWith, Separator, TextOptions, UnicodeLine, XHeader};
use crate::parser::{self, Lexicon, Variables};
use crate::psql_if::Branch;
use super::{DbmsPlane, ErrorStyle, LostConnection, MetaContext, MetaStep, Timing};

mod conninfo;
mod describe;
mod encoding;
mod error;
mod large_object;
mod lists;
#[cfg(test)]
mod live;
mod pattern;
mod print;

pub struct PostgresPlane;

/// Major-keyword list for the PG plane. Sorted (binary_search). The
/// universal SQL-92 base + PG-specific clause-introducer verbs:
///
/// - `ANALYZE` — statistics refresh (`ANALYZE table`).
/// - `CLUSTER` — physical re-order (`CLUSTER table USING index`).
/// - `COPY` — bulk import / export (`COPY table FROM ...`).
/// - `EXPLAIN` — query plan (`EXPLAIN ANALYZE SELECT ...`).
/// - `REINDEX` — index rebuild.
/// - `VACUUM` — bloat reclamation.
///
/// Pre-fix the trait default (`MAJOR_KEYWORDS_UNIVERSAL`) omitted all
/// of these — a DBA's `EXPLAIN ANALYZE SELECT ...` lit up its
/// SELECT/FROM/WHERE clause keywords but left EXPLAIN flat-colored
/// because it wasn't in the major-keyword set. The highlighter
/// pretended PG was SQL-92. Audit finding 4 of issue #10.
static PG_MAJOR_KEYWORDS: &[&str] = &[
    "ALTER", "ANALYZE", "BEGIN", "BY", "CLUSTER", "COMMIT", "COPY",
    "CREATE", "CROSS", "DELETE", "DROP", "EXPLAIN", "FROM", "FULL",
    "GRANT", "GROUP", "HAVING", "INNER", "INSERT", "INTO", "JOIN",
    "LATERAL", "LEFT", "LIMIT", "NATURAL", "OFFSET", "ORDER", "OUTER",
    "RECURSIVE", "REINDEX", "RETURNING", "REVOKE", "RIGHT", "ROLLBACK",
    "SAVEPOINT", "SELECT", "SET", "START", "TRUNCATE", "UPDATE",
    "VACUUM", "VALUES", "WHERE", "WITH",
];

/// psql's `QUIET` (`pset.quiet`): what it leaves unsaid — `printPsetInfo`,
/// `\r`'s, `lo_unlink N`, describe.c's "Did not find …".
pub(super) fn quiet(ctx: &MetaContext) -> bool {
    crate::psql_vars::quiet(&ctx.variables)
}

/// psql's `\pset [option [value]]` (`exec_command_pset` → `do_pset`):
/// no option lists every setting as psql does; an option — named
/// exactly as psql names it, case and single letters too (`t`, `T`, `C`,
/// `x`) — is set from its value or, without one, toggled or said, each
/// in psql's words, and refused in them.
fn handle_pset(args: &[String], ctx: &MetaContext) -> MetaAction {
    let Some(param) = args.first() else {
        return MetaAction::Output(pset_listing(ctx));
    };
    let value = args.get(1).cloned();
    match param.as_str() {
        // No value: what it is (psql's `printPsetInfo`).
        "format" => match value {
            None if quiet(ctx) => MetaAction::Output(Vec::new()),
            None => MetaAction::Output(vec![format!("Output format is {}.", ctx.print.format.name())]),
            Some(value) => match psql_format(&value) {
                Ok(format) => MetaAction::SetFormat { format, expanded: None },
                Err(message) => MetaAction::Fail(vec![message]),
            },
        },
        "x" | "expanded" | "vertical" => expanded_action(param, value.as_deref()),
        "t" | "tuples_only" => pset_bool(param, value.as_deref())
            .map_or_else(|refused| MetaAction::Fail(vec![refused]), MetaAction::SetTuplesOnly),
        "footer" => pset_bool(param, value.as_deref())
            .map_or_else(|refused| MetaAction::Fail(vec![refused]), MetaAction::SetFooter),
        "C" | "title" => MetaAction::SetTitle(value),
        // No value: what it is.
        "null" => MetaAction::SetNullDisplay(value),
        "border" | "columns" | "csv_fieldsep" | "fieldsep" | "fieldsep_zero" | "linestyle" | "numericlocale" | "pager"
        | "pager_min_lines" | "recordsep" | "recordsep_zero" | "T" | "tableattr" | "unicode_border_linestyle"
        | "unicode_column_linestyle" | "unicode_header_linestyle" | "xheader_width" =>
            text_option(param, value, &ctx.print.text),
        _ => MetaAction::Fail(vec![format!("\\pset: unknown option: {param}")]),
    }
}

/// A Boolean `\pset` value as psql's `do_pset` reads it
/// (`ParseVariableBool`): Some — set it, silently — or None, no value:
/// toggle it; refused in psql's words, naming the option as typed.
fn pset_bool(param: &str, value: Option<&str>) -> Result<Option<bool>, String> {
    match value {
        None => Ok(None),
        Some(value) => crate::psql_vars::parse_bool(value).map(Some)
            .ok_or_else(|| format!("unrecognized value \"{value}\" for \"{param}\": Boolean expected")),
    }
}

/// What bare `\pset` prints: each setting and its value as psql's
/// `pset_value_string` writes it — strings quoted, a newline `\n` and a
/// quote `\'` in them — the names padded to 24.
fn pset_listing(ctx: &MetaContext) -> Vec<String> {
    let text = &ctx.print.text;
    let quoted = |s: &str| format!("'{}'", s.replace('\n', "\\n").replace('\'', "\\'"));
    let on_off = |on: bool| if on { "on" } else { "off" }.to_string();
    let separator = |sep: &Separator| match sep {
        Separator::Text(sep) => quoted(sep),
        Separator::Zero => quoted(""),
    };
    let settings = [
        ("border", text.rules.border.to_string()),
        ("columns", text.columns.to_string()),
        ("csv_fieldsep", quoted(&text.csv_fieldsep.to_string())),
        ("expanded", match ctx.print.expanded {
            Expanded::Auto => "auto".into(),
            expanded => on_off(expanded == Expanded::On),
        }),
        ("fieldsep", separator(&text.fieldsep)),
        ("fieldsep_zero", on_off(text.fieldsep == Separator::Zero)),
        ("footer", on_off(ctx.print.footer)),
        ("format", ctx.print.format.name().into()),
        ("linestyle", text.rules.style.name().into()),
        ("null", quoted(&ctx.null_display)),
        ("numericlocale", on_off(text.numeric_locale)),
        ("pager", text.pager.to_string()),
        ("pager_min_lines", text.pager_min_lines.to_string()),
        ("recordsep", separator(&text.recordsep)),
        ("recordsep_zero", on_off(text.recordsep == Separator::Zero)),
        ("tableattr", text.tableattr.as_deref().map(quoted).unwrap_or_default()),
        ("title", ctx.title.as_deref().map(quoted).unwrap_or_default()),
        ("tuples_only", on_off(ctx.print.tuples_only)),
        ("unicode_border_linestyle", text.rules.unicode_border.name().into()),
        ("unicode_column_linestyle", text.rules.unicode_column.name().into()),
        ("unicode_header_linestyle", text.rules.unicode_header.name().into()),
        ("xheader_width", match text.rules.xheader {
            XHeader::Full => "full".into(),
            XHeader::Column => "column".into(),
            XHeader::Page => "page".into(),
            XHeader::Exact(width) => width.to_string(),
        }),
    ];
    settings.into_iter().map(|(name, value)| format!("{name:<24} {value}")).collect()
}

/// psql's `\pset` of a printer's setting (`do_pset`), said as its
/// `printPsetInfo` says it: a value changes it, none just says what it
/// is — but for `tableattr` (`T`), which none unsets, the `_zero` forms,
/// and `numericlocale` and `pager`, which none toggles — and a Boolean
/// value for `numericlocale` is set silently, as psql's returns before
/// saying it. `csv_fieldsep` takes one one-byte character, not a quote
/// or a line break; `columns`, `border` and an `xheader_width` number
/// read as C's `atoi` reads them (`border` kept as psql's `unsigned
/// short` keeps it: -1 is 65535), `pager_min_lines` as psql's
/// `ParseVariableNum`; `linestyle` and the unicode lines take any prefix
/// of their names, in any case. Refused in psql's words, naming the
/// option as typed.
fn text_option(param: &str, value: Option<String>, current: &TextOptions) -> MetaAction {
    let mut options = current.clone();
    match param {
        "fieldsep" => if let Some(value) = value { options.fieldsep = Separator::Text(value) },
        "fieldsep_zero" => options.fieldsep = Separator::Zero,
        "recordsep" => if let Some(value) = value { options.recordsep = Separator::Text(value) },
        "recordsep_zero" => options.recordsep = Separator::Zero,
        "csv_fieldsep" => if let Some(value) = value {
            if value.len() != 1 {
                return MetaAction::Fail(vec!["\\pset: csv_fieldsep must be a single one-byte character".into()]);
            }
            let sep = value.chars().next().unwrap_or(',');
            if matches!(sep, '"' | '\n' | '\r') {
                return MetaAction::Fail(vec!["\\pset: csv_fieldsep cannot be a double quote, a newline, or a carriage return".into()]);
            }
            options.csv_fieldsep = sep;
        },
        "T" | "tableattr" => options.tableattr = value,
        "border" => if let Some(value) = value { options.rules.border = c_atoi(&value) as u16 },
        "linestyle" => if let Some(value) = value {
            match [LineStyle::Ascii, LineStyle::OldAscii, LineStyle::Unicode].into_iter().find(|s| abbreviates(s.name(), &value)) {
                Some(style) => options.rules.style = style,
                None => return MetaAction::Fail(vec!["\\pset: allowed line styles are ascii, old-ascii, unicode".into()]),
            }
        },
        "unicode_border_linestyle" | "unicode_column_linestyle" | "unicode_header_linestyle" => if let Some(value) = value {
            let Some(line) = [UnicodeLine::Single, UnicodeLine::Double].into_iter().find(|l| abbreviates(l.name(), &value)) else {
                let lines = param.trim_start_matches("unicode_").trim_end_matches("_linestyle");
                return MetaAction::Fail(vec![format!("\\pset: allowed Unicode {lines} line styles are single, double")]);
            };
            match param {
                "unicode_border_linestyle" => options.rules.unicode_border = line,
                "unicode_column_linestyle" => options.rules.unicode_column = line,
                _ => options.rules.unicode_header = line,
            }
        },
        "numericlocale" => match pset_bool(param, value.as_deref()) {
            Ok(Some(on)) => {
                options.numeric_locale = on;
                return MetaAction::SetTextOptions { said: Vec::new(), options };
            }
            Ok(None) => options.numeric_locale = !options.numeric_locale,
            Err(refused) => return MetaAction::Fail(vec![refused]),
        },
        "pager" => match value {
            Some(value) if value.eq_ignore_ascii_case("always") => options.pager = 2,
            Some(value) => match crate::psql_vars::parse_bool(&value) {
                Some(on) => options.pager = u8::from(on),
                None => return MetaAction::Fail(vec![
                    format!("unrecognized value \"{value}\" for \"{param}\""),
                    "Available values are: on, off, always.".into(),
                ]),
            },
            None => options.pager = u8::from(options.pager != 1),
        },
        "pager_min_lines" => if let Some(value) = value {
            match parse_variable_num(&value) {
                Some(lines) => options.pager_min_lines = lines,
                None => return MetaAction::Fail(vec![format!("invalid value \"{value}\" for \"{param}\": integer expected")]),
            }
        },
        "xheader_width" => if let Some(value) = value {
            options.rules.xheader = match value.to_ascii_lowercase().as_str() {
                "full" => XHeader::Full,
                "column" => XHeader::Column,
                "page" => XHeader::Page,
                _ => match c_atoi(&value) {
                    0 => return MetaAction::Fail(vec![
                        "\\pset: allowed xheader_width values are \"full\" (default), \"column\", \"page\", or a number specifying the exact width".into(),
                    ]),
                    width => XHeader::Exact(width),
                },
            };
        },
        _ => if let Some(value) = value { options.columns = c_atoi(&value) },
    }
    let said = match param {
        "fieldsep" | "fieldsep_zero" => match &options.fieldsep {
            Separator::Zero => "Field separator is zero byte.".into(),
            Separator::Text(sep) => format!("Field separator is \"{sep}\"."),
        },
        "recordsep" | "recordsep_zero" => match &options.recordsep {
            Separator::Zero => "Record separator is zero byte.".into(),
            Separator::Text(sep) if sep == "\n" => "Record separator is <newline>.".into(),
            Separator::Text(sep) => format!("Record separator is \"{sep}\"."),
        },
        "csv_fieldsep" => format!("Field separator for CSV is \"{}\".", options.csv_fieldsep),
        "T" | "tableattr" => match &options.tableattr {
            Some(attrs) => format!("Table attributes are \"{attrs}\"."),
            None => "Table attributes unset.".into(),
        },
        "border" => format!("Border style is {}.", options.rules.border),
        "linestyle" => format!("Line style is {}.", options.rules.style.name()),
        "unicode_border_linestyle" => format!("Unicode border line style is \"{}\".", options.rules.unicode_border.name()),
        "unicode_column_linestyle" => format!("Unicode column line style is \"{}\".", options.rules.unicode_column.name()),
        "unicode_header_linestyle" => format!("Unicode header line style is \"{}\".", options.rules.unicode_header.name()),
        "numericlocale" => format!("Locale-adjusted numeric output is {}.", if options.numeric_locale { "on" } else { "off" }),
        "pager" => match options.pager {
            1 => "Pager is used for long output.".into(),
            2 => "Pager is always used.".into(),
            _ => "Pager usage is off.".into(),
        },
        "pager_min_lines" => match options.pager_min_lines {
            1 => "Pager won't be used for less than 1 line.".into(),
            lines => format!("Pager won't be used for less than {lines} lines."),
        },
        "xheader_width" => match options.rules.xheader {
            XHeader::Full => "Expanded header width is \"full\".".into(),
            XHeader::Column => "Expanded header width is \"column\".".into(),
            XHeader::Page => "Expanded header width is \"page\".".into(),
            XHeader::Exact(width) => format!("Expanded header width is {width}."),
        },
        _ => match options.columns {
            0 => "Target width is unset.".into(),
            width => format!("Target width is {width}."),
        },
    };
    MetaAction::SetTextOptions { options, said: vec![said] }
}

/// A number as psql's `ParseVariableNum` reads it (`strtol` base 0, the
/// whole value): leading spaces, a sign, `0x` hex or a leading-`0` octal
/// — nothing after it, and within an `int`.
fn parse_variable_num(text: &str) -> Option<i32> {
    let text = text.trim_start_matches([' ', '\t', '\n', '\r', '\u{b}', '\u{c}']);
    let (negative, text) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let hex = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")).filter(|h| h.starts_with(|c: char| c.is_ascii_hexdigit()));
    let (radix, digits) = match hex {
        Some(digits) => (16, digits),
        None if text.len() > 1 && text.starts_with('0') => (8, &text[1..]),
        None => (10, text),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let number = i64::from_str_radix(digits, radix).ok()?;
    i32::try_from(if negative { -number } else { number }).ok()
}

/// A number as C's `atoi` reads it: leading spaces, a sign, the digits
/// that follow — 0 when there are none.
fn c_atoi(text: &str) -> i32 {
    let text = text.trim_start();
    let (sign, digits) = match text.as_bytes().first() {
        Some(b'-') => (-1, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => (1, text),
    };
    let digits: String = digits.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<i64>().map_or(0, |n| (sign * n).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
}

/// The savepoint psql sets around a statement under `ON_ERROR_ROLLBACK`.
const PSQL_SAVEPOINT: &str = "pg_psql_temporary_savepoint";

/// Whether `sql` is a command psql won't open a transaction for under
/// `AUTOCOMMIT off` (`command_no_begin`): one that ends or begins one, or
/// can't run in one — `VACUUM`, a bare `CLUSTER`, `CREATE` / `DROP
/// DATABASE` and `TABLESPACE`, `CREATE [UNIQUE] INDEX CONCURRENTLY`,
/// `DROP INDEX CONCURRENTLY`, `REINDEX … CONCURRENTLY` / `DATABASE` /
/// `SYSTEM`, `ALTER SYSTEM`, `DISCARD ALL`, `PREPARE TRANSACTION`. Its
/// words read past whitespace and comments, letters only, in any case.
fn command_no_begin(sql: &str) -> bool {
    let mut rest = sql;
    let mut word = || -> String {
        rest = skip_white_space(rest);
        let len = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
        let word = rest[..len].to_ascii_lowercase();
        rest = &rest[len..];
        word
    };
    match word().as_str() {
        "abort" | "begin" | "start" | "commit" | "end" | "rollback" | "vacuum" => true,
        "prepare" => word() == "transaction",
        // CLUSTER with any arguments may run in a transaction.
        "cluster" => !skip_white_space(rest).starts_with(|c: char| c.is_ascii_alphabetic()),
        "create" => match word().as_str() {
            "database" | "tablespace" => true,
            "unique" => word() == "index" && word() == "concurrently",
            "index" => word() == "concurrently",
            _ => false,
        },
        "alter" => word() == "system",
        "drop" | "reindex" => match word().as_str() {
            "database" | "system" | "tablespace" => true,
            "index" | "table" => word() == "concurrently",
            _ => false,
        },
        "discard" => word() == "all",
        _ => false,
    }
}

/// `sql` past leading whitespace and comments (`skip_white_space`):
/// `/* */`, nested, and `--` to the line's end.
fn skip_white_space(sql: &str) -> &str {
    let mut rest = sql;
    let mut depth = 0;
    loop {
        if let Some(after) = rest.strip_prefix("/*") {
            depth += 1;
            rest = after;
        } else if depth > 0 {
            match rest.strip_prefix("*/") {
                Some(after) => { depth -= 1; rest = after; }
                None => match rest.chars().next() {
                    Some(c) => rest = &rest[c.len_utf8()..],
                    None => return rest,
                },
            }
        } else if let Some(after) = rest.strip_prefix("--") {
            rest = after.find('\n').map_or("", |at| &after[at + 1..]);
        } else {
            match rest.chars().next() {
                Some(c) if c.is_whitespace() => rest = &rest[c.len_utf8()..],
                _ => return rest,
            }
        }
    }
}

/// Whether `value` starts `name`, in any case, as psql's `do_pset` reads
/// a value (`pg_strncasecmp(name, value, strlen(value))`): `""` starts
/// every name.
fn abbreviates(name: &str, value: &str) -> bool {
    name.as_bytes().get(..value.len()).is_some_and(|start| start.eq_ignore_ascii_case(value.as_bytes()))
}

/// A `\pset format` value as psql reads it (`do_pset`): any unique
/// abbreviation of a format's name, in any case — `latex-longtable` only
/// past `latex`, which it starts with — refused in psql's words.
fn psql_format(value: &str) -> Result<OutputFormat, String> {
    const FORMATS: [(&str, OutputFormat); 8] = [
        ("aligned", OutputFormat::Aligned),
        ("asciidoc", OutputFormat::Asciidoc),
        ("csv", OutputFormat::Csv),
        ("html", OutputFormat::Html),
        ("latex", OutputFormat::Latex),
        ("troff-ms", OutputFormat::TroffMs),
        ("unaligned", OutputFormat::Unaligned),
        ("wrapped", OutputFormat::Wrapped),
    ];
    let mut found: Option<(&str, OutputFormat)> = None;
    for (name, format) in FORMATS {
        if abbreviates(name, value) {
            if let Some((first, _)) = found {
                return Err(format!("\\pset: ambiguous abbreviation \"{value}\" matches both \"{first}\" and \"{name}\""));
            }
            found = Some((name, format));
        }
    }
    match found {
        Some((_, format)) => Ok(format),
        None if abbreviates("latex-longtable", value) => Ok(OutputFormat::LatexLongtable),
        None => Err("\\pset: allowed formats are aligned, asciidoc, csv, html, latex, latex-longtable, troff-ms, unaligned, wrapped".into()),
    }
}

/// psql's `\watch [[i=]SEC] [c=N] [m=MIN]` (`exec_command_watch`): the
/// interval in seconds — a fraction allowed, kept in milliseconds as
/// psql keeps it, 2 by default — how many runs, and the fewest rows to
/// go on; each given once, and refused in psql's words.
fn parse_watch_args(rest: &str, vars: &Variables) -> MetaAction {
    let refuse = |message: String| MetaAction::Output(vec![format!("\\watch: {message}")]);
    let (args, _) = pattern::slash_args(rest, vars, true);
    let mut interval: Option<f64> = None;
    let mut count: Option<u32> = None;
    let mut min_rows: Option<u32> = None;
    // psql's strtoint, above zero.
    let positive = |value: &str| value.parse::<i32>().ok().filter(|&n| n > 0).map(|n| n as u32);
    for arg in &args {
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (Some(name), value),
            None => (None, arg.as_str()),
        };
        match name {
            None | Some("i" | "interval") => {
                if interval.is_some() {
                    return refuse("interval value is specified more than once".into());
                }
                match value.parse::<f64>() {
                    Ok(secs) if secs >= 0.0 && secs.is_finite() => interval = Some(secs),
                    _ => return refuse(format!("incorrect interval value \"{value}\"")),
                }
            }
            Some("c" | "count") => {
                if count.is_some() {
                    return refuse("iteration count is specified more than once".into());
                }
                count = positive(value);
                if count.is_none() {
                    return refuse(format!("incorrect iteration count \"{value}\""));
                }
            }
            Some("m" | "min_rows") => {
                if min_rows.is_some() {
                    return refuse("minimum row count specified more than once".into());
                }
                min_rows = positive(value);
                if min_rows.is_none() {
                    return refuse(format!("incorrect minimum row count \"{value}\""));
                }
            }
            Some(_) => return refuse(format!("unrecognized parameter \"{arg}\"")),
        }
    }
    MetaAction::Watch { interval_ms: (interval.unwrap_or(2.0) * 1000.0) as u32, count, min_rows }
}

/// Parse `\prompt [text] name` into a `MetaAction::Prompt`. Quoted
/// text (single or double) captures spaces; bare two-word form
/// splits on first whitespace. A single token is treated as the
/// variable name with empty prompt text. Empty input → usage hint.
/// Parse `\crosstabview [colV [colH [colD [sortcolH]]]]` into a
/// `MetaAction::Crosstabview`. Each spec is a whitespace-separated
/// token (numeric 1-based index or column name). psql reads four; any
/// more are its extra arguments (`extra_arguments`). Resolution against
/// the actual result columns happens later in `psql_crosstab::pivot_table`.
fn parse_crosstabview_args(toks: &[String]) -> MetaAction {
    let get = |i: usize| toks.get(i).cloned();
    MetaAction::Crosstabview {
        col_v:     get(0),
        col_h:     get(1),
        col_d:     get(2),
        sortcol_h: get(3),
    }
}

/// Parse `\password [user]` into a `MetaAction::Password`. The user
/// name is optional; quoted identifiers (single or double) capture
/// names with spaces, uppercase, or — for double-quotes — embedded
/// quote chars via the PG convention `""`. Empty / whitespace-only →
/// `None` (target = current session user, resolved at submit time
/// from `:USER`). psql reads one name; any more are its extra arguments
/// (`extra_arguments`).
fn parse_password_args(rest: &str) -> MetaAction {
    let rest = rest.trim();
    if rest.is_empty() {
        return MetaAction::Password { user: None };
    }
    let user = if let Some(inner) = rest.strip_prefix('\'') {
        match scan_quoted(inner, '\'') {
            Ok((name, _)) => name,
            Err(e) => return MetaAction::Fail(vec![e]),
        }
    } else if let Some(inner) = rest.strip_prefix('"') {
        match scan_quoted(inner, '"') {
            Ok((name, _)) => name,
            Err(e) => return MetaAction::Fail(vec![e]),
        }
    } else {
        let first = rest.split_whitespace().next().unwrap_or("").to_string();
        first
    };
    let user = if user.is_empty() { None } else { Some(user) };
    MetaAction::Password { user }
}

/// Walk a string that has already had its opening `quote` stripped,
/// honoring the SQL convention that a doubled quote inside the body
/// is a literal quote (PG identifiers: `"a""b"` = `a"b`; single
/// quotes: `'a''b'` = `a'b`). Returns `(unescaped_body, tail)` where
/// `tail` is everything after the closing quote (NOT yet trimmed).
fn scan_quoted(inner: &str, quote: char) -> Result<(String, &str), String> {
    let mut out = String::new();
    let mut chars = inner.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == quote {
            // Doubled-quote escape — consume the second one as a
            // literal, keep scanning.
            if chars.peek().map(|&(_, n)| n) == Some(quote) {
                chars.next();
                out.push(quote);
                continue;
            }
            // Closing quote — `i` is at the closing position; the
            // tail starts immediately after it.
            let tail_start = i + quote.len_utf8();
            return Ok((out, &inner[tail_start..]));
        }
        out.push(c);
    }
    Err("\\password: unterminated quoted string".into())
}

fn parse_prompt_args(rest: &str) -> MetaAction {
    let rest = rest.trim();
    if rest.is_empty() {
        return MetaAction::Output(vec![
            "Usage: \\prompt [text] name  (interactive variable read; Esc cancels)".into(),
        ]);
    }
    let (text, name_part) = if let Some(inner) = rest.strip_prefix('\'') {
        match inner.find('\'') {
            Some(end) => (inner[..end].to_string(), inner[end + 1..].trim().to_string()),
            None => return MetaAction::Output(vec![
                "\\prompt: unterminated quoted string".into(),
            ]),
        }
    } else if let Some(inner) = rest.strip_prefix('"') {
        match inner.find('"') {
            Some(end) => (inner[..end].to_string(), inner[end + 1..].trim().to_string()),
            None => return MetaAction::Output(vec![
                "\\prompt: unterminated quoted string".into(),
            ]),
        }
    } else {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let first  = parts.next().unwrap_or("").to_string();
        let second = parts.next().unwrap_or("").trim().to_string();
        if second.is_empty() {
            // Single-word form — that word is the variable name.
            (String::new(), first)
        } else {
            (first, second)
        }
    };
    if name_part.is_empty() {
        return MetaAction::Output(vec![
            "\\prompt: missing variable name".into(),
        ]);
    }
    // Multi-token name after a quoted text is a user error — psql
    // takes only the first token after the prompt text. Match that.
    let name = name_part.split_whitespace().next().unwrap_or("").to_string();
    MetaAction::Prompt { text, name }
}

/// psql's `\x [value]` / `\pset expanded [value]` (`do_pset`, also
/// `x` and `vertical`): `auto` in any case, a Boolean as
/// `ParseVariableBool` reads one, or nothing — toggle; anything else
/// psql's error, naming the option as typed.
fn expanded_action(param: &str, value: Option<&str>) -> MetaAction {
    match value {
        None => MetaAction::SetExpanded(None),
        Some(value) if value.eq_ignore_ascii_case("auto") => MetaAction::SetExpanded(Some(Expanded::Auto)),
        Some(value) => match crate::psql_vars::parse_bool(value) {
            Some(on) => MetaAction::SetExpanded(Some(if on { Expanded::On } else { Expanded::Off })),
            None => MetaAction::Fail(vec![
                format!("unrecognized value \"{value}\" for \"{param}\""),
                "Available values are: on, off, auto.".into(),
            ]),
        },
    }
}

/// A backslash command's name — up to whitespace, a `;` included, as
/// psql's lexer reads one (`\gx;` is no command; `\dt;` is `\dt`, its
/// letters read as psql reads them) — and its argument text as typed:
/// whether a trailing `;` belongs to the last argument is each command's
/// to say, as in psql (`slash_args`).
fn split_command(input: &str) -> Option<(&str, &str)> {
    let after = input.trim().strip_prefix('\\')?;
    let (cmd, rest) = after.split_once(char::is_whitespace)
        .map(|(c, r)| (c, r.trim()))
        .unwrap_or((after, ""));
    Some((cmd, rest))
}

/// psql's `\c [-reuse-previous=on|off] [dbname [username [host [port]]]]`
/// (`exec_command_connect`, `read_connect_arg`): a double-quoted argument
/// loses its quotes and keeps its case, an unquoted `-` (or nothing)
/// keeps the current value. `-reuse-previous=` first takes a Boolean as
/// psql reads one — refused in its words otherwise, nothing connected:
/// on is as without it; off takes libpq's defaults for what isn't given
/// instead of the current connection's. Only the database can change
/// here — the new session comes from the same connection; another user,
/// host or port, a connection string, or libpq's defaults, isn't
/// supported yet.
fn psql_connect(rest: &str, ctx: &MetaContext) -> MetaAction {
    let (args, unterminated) = pattern::slash_args(rest, &ctx.variables, true);
    if unterminated.is_some() {
        return MetaAction::Output(vec!["unterminated quoted string".into()]);
    }
    let mut args: Vec<Option<String>> = args.into_iter().map(connect_arg).collect();
    if let Some(value) = args.first().and_then(|a| a.as_deref()).and_then(|a| a.strip_prefix(REUSE_PREVIOUS)) {
        match crate::psql_vars::parse_bool(value) {
            None => return MetaAction::Fail(vec![format!("unrecognized value \"{value}\" for \"-reuse-previous\": Boolean expected")]),
            Some(false) => return MetaAction::Fail(vec![
                "\\c: -reuse-previous=off — libpq's defaults for what isn't given — isn't supported yet.".into(),
            ]),
            Some(true) => { args.remove(0); }
        }
    }
    let args: Vec<Option<String>> = args.into_iter().take(4).collect();
    let conninfo = args.iter().flatten().any(|a| a.contains('=') || a.contains("://"));
    let kept = [&ctx.user, &ctx.host, &ctx.port];
    let elsewhere = args.iter().skip(1).zip(kept).any(|(arg, now)| arg.as_ref().is_some_and(|a| a != now));
    if conninfo || elsewhere {
        return MetaAction::Fail(vec![
            "\\c: only the database can change here — another user, host or port, or a connection string, isn't supported yet.".into(),
        ]);
    }
    MetaAction::ReconnectDb(args.into_iter().next().flatten())
}

/// psql's `PrintTiming`: milliseconds to the microsecond, and past a
/// second the same as minutes and seconds (hours, days).
fn psql_time(exec_time_us: u64) -> String {
    let ms = exec_time_us as f64 / 1000.0;
    if ms < 1000.0 {
        return format!("Time: {ms:.3} ms");
    }
    let mut seconds = ms / 1000.0;
    let mut minutes = (seconds / 60.0).floor();
    seconds -= 60.0 * minutes;
    if minutes < 60.0 {
        return format!("Time: {ms:.3} ms ({:02}:{seconds:06.3})", minutes as u64);
    }
    let mut hours = (minutes / 60.0).floor();
    minutes -= 60.0 * hours;
    if hours < 24.0 {
        return format!("Time: {ms:.3} ms ({:02}:{:02}:{seconds:06.3})", hours as u64, minutes as u64);
    }
    let days = (hours / 24.0).floor();
    hours -= 24.0 * days;
    format!("Time: {ms:.3} ms ({days:.0} d {:02}:{:02}:{seconds:06.3})", hours as u64, minutes as u64)
}

/// psql's `\c` option, read only as its first argument.
const REUSE_PREVIOUS: &str = "-reuse-previous=";

/// One `\c` argument as psql reads it: None keeps the current value.
fn connect_arg(arg: String) -> Option<String> {
    match arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
        Some(quoted) => Some(quoted.replace("\"\"", "\"")),
        None if arg.is_empty() || arg == "-" => None,
        None => Some(arg),
    }
}

/// psql's `\d` / `\dS` / `\d+` with a pattern describes the relations
/// it matches (`describeTableDetails`); without one, and the rest of the
/// family, list (`lists.rs`). The pattern, when this is a description.
fn describe_pattern(cmd: &str, rest: &str, vars: &Variables) -> Option<String> {
    if !lists::list_tables_types(cmd)?.is_empty() { return None; }
    let (args, unterminated) = pattern::slash_args(rest, vars, true);
    // An unterminated quote is reported by the list.
    if unterminated.is_some() { return None; }
    args.into_iter().next()
}

/// What psql's `\ef` with no function opens.
const FUNCTION_TEMPLATE: &str = "CREATE FUNCTION ( )\n RETURNS \n LANGUAGE \n \
                                 -- common options:  IMMUTABLE  STABLE  STRICT  SECURITY DEFINER\n\
                                 AS $function$\n\n$function$\n";
/// What psql's `\ev` with no view opens.
const VIEW_TEMPLATE: &str = "CREATE VIEW  AS\n SELECT \n  -- something...\n";

/// psql's `strip_lineno_from_objdesc`: a trailing run of digits after
/// whitespace or `)` is a line number — `\ef f 3`, `\ef f(int)3` —
/// while `\ef f3` is a function named `f3`.
fn split_lineno(desc: &str) -> Result<(&str, Option<u32>), String> {
    let desc = desc.trim_end();
    let name = desc.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = &desc[name.len()..];
    match name.chars().last() {
        Some(c) if !digits.is_empty() && (c.is_whitespace() || c == ')') => match digits.parse::<u32>() {
            Ok(n) if n > 0 => Ok((name.trim_end(), Some(n))),
            _ => Err(format!("invalid line number: {digits}")),
        },
        _ => Ok((desc, None)),
    }
}

/// `\ef`: the function's definition (`Src`), the editor line (`Line`)
/// and an empty `Error` (an introspect result reads SQL NULL as the
/// text "NULL", so absent text columns are `''`). Like psql, a name with an argument list resolves via
/// `regprocedure`, a bare one via `regproc` — so a missing or
/// overloaded name fails with the server's own error — and line 1 is
/// the body's first line (the one starting `AS `, `BEGIN ` or
/// `RETURN `), not the definition's.
fn edit_function_sql(name: &str, line: Option<u32>) -> String {
    let cast = if name.contains('(') { "regprocedure" } else { "regproc" };
    let line = match line {
        Some(n) => format!(
            "{n} + coalesce((SELECT min(l.n) - 1 \
                             FROM regexp_split_to_table(d.def, E'\\n') WITH ORDINALITY AS l(t, n) \
                             WHERE left(l.t, 3) = 'AS ' OR left(l.t, 6) = 'BEGIN ' \
                                OR left(l.t, 7) = 'RETURN '), 0)",
        ),
        None => "NULL::int".into(),
    };
    format!(
        "SELECT d.def AS \"Src\", {line} AS \"Line\", ''::text AS \"Error\" \
         FROM (SELECT pg_catalog.pg_get_functiondef('{name}'::pg_catalog.{cast}) AS def) d",
        name = name.replace('\'', "''"),
    )
}

/// psql's `\sf[+]` / `\sv[+]` (`exec_command_sf_sv`), once the lookup
/// — `\ef` / `\ev`'s — is in: the definition as psql prints it, `+`
/// numbering its lines as `print_with_linenumbers` does (a function's
/// from its body: `AS`, `BEGIN`, `RETURN`); a lookup the server refused,
/// in psql's `minimal_error_message`; a relation that isn't a view,
/// psql's word for it.
fn show_definition(cmd: &str, replies: &[shared::BatchResult]) -> MetaStep {
    let Some(reply) = replies.first() else { return MetaStep::Done(Vec::new()) };
    if let Some(error) = &reply.error {
        let style = ErrorStyle { verbosity: crate::plane::Verbosity::Minimal, ..ErrorStyle::default() };
        return MetaStep::Failed(vec![crate::ShellEntry::CliError {
            lines:   error::psql_error(error, None, style),
            message: error.message.clone(),
        }]);
    }
    let row = reply.sets.first().and_then(|set| set.rows.first()).cloned().unwrap_or_default();
    let column = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
    let refusal = column(2);
    if !refusal.is_empty() {
        return MetaStep::Failed(vec![crate::ShellEntry::CliError { lines: vec![refusal.clone()], message: refusal }]);
    }
    let source = column(0);
    let source = source.strip_suffix('\n').unwrap_or(&source);
    let lines: Vec<String> = if cmd.ends_with('+') {
        with_line_numbers(source, cmd.starts_with("sf"))
    } else {
        source.split('\n').map(String::from).collect()
    };
    MetaStep::Done(vec![crate::ShellEntry::Printed { lines, links: Vec::new() }])
}

/// psql's `print_with_linenumbers`: each line after its number, padded
/// to seven — a function's header lines (before `AS`, `BEGIN` or
/// `RETURN` starts its body) unnumbered, indented to match.
fn with_line_numbers(source: &str, is_func: bool) -> Vec<String> {
    let mut in_header = is_func;
    let mut number = 0;
    source.split('\n').map(|line| {
        if in_header && (line.starts_with("AS ") || line.starts_with("BEGIN ") || line.starts_with("RETURN ")) {
            in_header = false;
        }
        if in_header {
            format!("        {line}")
        } else {
            number += 1;
            format!("{number:<7} {line}")
        }
    }).collect()
}

/// `\ev` / `\sv`: `CREATE OR REPLACE VIEW` built the way psql builds it
/// (`get_create_object_cmd`) — schema-qualified, reloptions on a line of
/// their own as `appendReloptionsArray` writes them (a value quoted
/// unless it's a plain identifier: `security_barrier='true'`), the
/// view's trailing semicolon dropped, its check option restored — or,
/// for anything that isn't a plain view (a materialized view, a table),
/// psql's "is not a view" as `Error`.
fn edit_view_sql(name: &str, line: Option<u32>) -> String {
    let line = line.map_or_else(|| "NULL".into(), |n| n.to_string());
    format!(
        "SELECT coalesce(CASE WHEN c.relkind = 'v' THEN \
                    'CREATE OR REPLACE VIEW ' || quote_ident(n.nspname) || '.' || quote_ident(c.relname) \
                    || coalesce(E'\\n WITH (' || (SELECT string_agg(quote_ident(split_part(r.o, '=', 1)) || '=' \
                           || CASE WHEN quote_ident(r.v) = r.v THEN r.v ELSE '''' || replace(r.v, '''', '''''') || '''' END, \
                           ', ' ORDER BY r.n) \
                         FROM (SELECT o, n, CASE WHEN strpos(o, '=') > 0 THEN substr(o, strpos(o, '=') + 1) ELSE '' END AS v \
                               FROM unnest(array_remove(array_remove(c.reloptions, 'check_option=local'), 'check_option=cascaded')) \
                                    WITH ORDINALITY AS u(o, n)) r) || ')', '') \
                    || E' AS\\n' || regexp_replace(pg_catalog.pg_get_viewdef(c.oid, true), ';$', '') \
                    || CASE WHEN 'check_option=local' = ANY (c.reloptions) THEN E'\\n WITH LOCAL CHECK OPTION' \
                            WHEN 'check_option=cascaded' = ANY (c.reloptions) THEN E'\\n WITH CASCADED CHECK OPTION' \
                            ELSE '' END \
                END, '') AS \"Src\", \
                {line}::int AS \"Line\", \
                coalesce(CASE WHEN c.relkind <> 'v' THEN format('\"%s.%s\" is not a view', n.nspname, c.relname) END, '') \
                    AS \"Error\" \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.oid = '{name}'::pg_catalog.regclass",
        name = name.replace('\'', "''"),
    )
}

/// What psql's `help` prints (`MainLoop`).
const PSQL_HELP: [&str; 6] = [
    "You are using psql, the command-line interface to PostgreSQL.",
    "Type:  \\copyright for distribution terms",
    "       \\h for help with SQL commands",
    "       \\? for help with psql commands",
    "       \\g or terminate with semicolon to execute query",
    "       \\q to quit",
];

/// psql's assistance words, for users of other SQL clients.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Assistance {
    Help,
    Quit,
}

/// The assistance word `line` is, as psql's `MainLoop` reads one: `help`,
/// or `quit` / `exit`, in any case, with nothing before it and only
/// whitespace after it, and maybe one `;` (`QUIT;`).
fn assistance_word(line: &str) -> Option<Assistance> {
    let word = line.get(..4)?;
    let assistance = if word.eq_ignore_ascii_case("help") {
        Assistance::Help
    } else if word.eq_ignore_ascii_case("quit") || word.eq_ignore_ascii_case("exit") {
        Assistance::Quit
    } else {
        return None;
    };
    let rest = line[4..].trim_start();
    rest.strip_prefix(';').unwrap_or(rest).trim().is_empty().then_some(assistance)
}

/// How many arguments psql's command `cmd` reads (its `exec_command_*`
/// in command.c), given those typed — some read a second only after a
/// first. None: all of them, or its whole line (`\echo`, `\set`, `\h`,
/// `\df`'s argument types), or one this plane doesn't model (`\g`'s
/// output options).
fn arguments_read(cmd: &str, args: &[String]) -> Option<usize> {
    // The `\d` family, whatever its suffixes: one pattern; `\df` / `\do`
    // read every one after it as an argument type; `\dA[cfop]` and
    // `\drds` a second after a first.
    if let Some(family) = cmd.strip_prefix('d') {
        let b = family.as_bytes();
        return match b.first() {
            Some(b'f' | b'o') => None,
            Some(b'A') if matches!(b.get(1), Some(b'c' | b'f' | b'o' | b'p')) => Some(2),
            _ if family.starts_with("rds") => Some(2),
            _ => Some(1),
        };
    }
    Some(match cmd {
        "a" | "conninfo" | "copyright" | "errverbose" | "gdesc" | "gexec" | "H" | "html"
        | "p" | "print" | "q" | "quit" | "r" | "reset" | "else" | "endif" => 0,
        "C" | "encoding" | "f" | "gset" | "s" | "t" | "T" | "timing" | "unset" | "x" | "?"
        | "password" | "z" => 1,
        _ if matches!(cmd.trim_end_matches(['+', 'x']), "l" | "list") => 1,
        "pset" | "prompt" | "e" | "edit" => 2,
        _ if cmd.starts_with("lo_") => 2,
        "crosstabview" => 4,
        "c" | "connect" => if args.first().cloned().and_then(connect_arg).is_some_and(|a| a.starts_with(REUSE_PREVIOUS)) { 5 } else { 4 },
        _ => return None,
    })
}

/// psql's query buffer once it has read `lines`, as typed: each line's
/// text up to the backslash command on it (`parser::trailing_command`),
/// the buffer emptied by a command that works on it (`\g`, `\r` …) and
/// by each statement its `;` ends, and kept past the line comments it
/// starts with (`parser::past_leading_line_comments`).
fn psql_query_buffer(lines: &str) -> String {
    let mut buffer = String::new();
    for line in lines.split('\n') {
        let read = if buffer.is_empty() { line.to_string() } else { format!("{buffer}\n{line}") };
        buffer = match parser::trailing_command(&read, Lexicon::Psql) {
            Some(at) if PostgresPlane.meta_cmd(&read[at..]).is_some_and(|action| action.works_on_buffer()) => String::new(),
            Some(at) => read[..at].trim_end().to_string(),
            None => read,
        };
        let pending = parser::split_statements(&buffer, ";", Lexicon::Psql).pending;
        buffer = parser::past_leading_line_comments(&buffer[pending..], Lexicon::Psql).to_string();
    }
    buffer
}

/// psql's query buffer for the text `typed` before a backslash command,
/// as `MainLoop` builds it line by line: whitespace and `--` comments
/// ahead of anything else aren't kept (the lexer's whitespace rule), nor
/// an empty line outside quotes and comments, and the line break before
/// the command's line goes when nothing precedes the command on it
/// (`added_nl_pos`) — `select 2\n  as b\n\p` holds `select 2\n  as b`,
/// `select 1 \p` keeps its space.
fn psql_buffer(typed: &str) -> String {
    let typed = parser::past_leading_line_comments(typed, Lexicon::Psql);
    let mut buffer = String::with_capacity(typed.len());
    for (at, piece) in parser::Pieces::new(typed, Lexicon::Psql) {
        let text = &typed[at];
        // A quote or comment never ends with a line break: one before
        // this is code, and this line is empty.
        if piece == parser::Piece::Code && text == "\n" && buffer.ends_with('\n') {
            continue;
        }
        buffer.push_str(text);
    }
    if buffer.ends_with('\n') {
        buffer.pop();
    }
    buffer
}

/// psql 18's backslash commands (`exec_command`), but for the `\d…` and
/// `\lo_…` families it reads by their prefix — whose every command this
/// plane runs. One of these it doesn't run isn't supported here yet;
/// anything else is psql's `invalid command`.
const PSQL_COMMANDS: &[&str] = &[
    "a", "bind", "bind_named", "C", "c", "connect", "cd", "close_prepared", "conninfo", "copy",
    "copyright", "crosstabview", "e", "edit", "ef", "ev", "echo", "qecho", "warn", "elif", "else",
    "endif", "endpipeline", "encoding", "errverbose", "f", "flush", "flushrequest", "g", "gx",
    "gdesc", "getenv", "getresults", "gexec", "gset", "h", "help", "H", "html", "i", "include",
    "ir", "include_relative", "if", "l", "list", "lx", "listx", "l+", "list+", "lx+", "listx+",
    "l+x", "list+x", "o", "out", "p", "print", "parse", "password", "prompt", "pset", "q", "quit",
    "r", "reset", "restrict", "s", "sendpipeline", "set", "setenv", "sf", "sf+", "sv", "sv+",
    "startpipeline", "syncpipeline", "t", "T", "timing", "unrestrict", "unset", "w", "write",
    "watch", "x", "z", "zS", "zx", "zSx", "zxS", "!", "?",
];

/// A backslash command that isn't psql's, failing as psql's
/// `HandleSlashCmds` fails it: its name — up to whitespace or the next
/// backslash, a `;` included (`invalid command \foo;`) — and the hint.
fn invalid_command(command: &str) -> MetaAction {
    let after = command.trim_start().strip_prefix('\\').unwrap_or(command);
    let name = &after[..after.find(|c: char| c.is_whitespace() || c == '\\').unwrap_or(after.len())];
    MetaAction::Fail(vec![format!("invalid command \\{name}"), "Try \\? for help.".into()])
}

/// The commands whose last argument keeps a trailing `;`, as psql's do
/// (`\echo a;` prints `a;`, `\set z 1;` sets `1;`); the others drop it
/// (`\d film;`, `\x on;`).
const KEEPS_SEMICOLONS: &[&str] = &[
    "echo", "qecho", "warn", "set", "unset", "pset", "gset", "timing", "crosstabview", "if", "elif", "f", "T",
];

/// The commands that read their arguments themselves — the whole line,
/// as psql reads it for these (`\h`, `\sf`, `\sv`, `\ef`, `\ev`: no
/// variables expanded), or with a parser of their own.
const READS_ITS_OWN_ARGUMENTS: &[&str] = &[
    "h", "help", "sf", "sf+", "sv", "sv+", "ef", "ev", "prompt", "password", "c", "connect", "watch",
];

/// psql `\e [filename] [line]`. A single all-digits argument is a line
/// number; anything else names a file, and editing files needs access
/// to the user's machine, which noctua doesn't have yet.
fn psql_edit(args: &str) -> MetaAction {
    let args = args.trim().trim_end_matches(';').trim();
    if args.is_empty() {
        return MetaAction::EditExternal { seed: None, line: None };
    }
    if args.bytes().all(|b| b.is_ascii_digit()) {
        return match args.parse::<u32>() {
            Ok(n) if n > 0 => MetaAction::EditExternal { seed: None, line: Some(n) },
            _ => MetaAction::Fail(vec![format!("\\e: invalid line number: {args}")]),
        };
    }
    MetaAction::Fail(vec![
        "\\e: editing a file isn't supported yet. \\e [line] edits the query buffer, \
         or the previous query when it's empty.".into(),
    ])
}

impl DbmsPlane for PostgresPlane {
    fn db_type(&self) -> DbType { DbType::Postgres }
    fn name(&self) -> &'static str { "PostgreSQL" }

    fn default_port(&self) -> u16 { 5432 }
    fn default_user(&self) -> &'static str { "postgres" }
    fn database_required(&self) -> bool { true }

    fn meta_cmd(&self, input: &str) -> Option<MetaAction> {
        self.meta_cmd_in(input, &MetaContext::default())
    }

    fn meta_cmd_in(&self, input: &str, ctx: &MetaContext) -> Option<MetaAction> {
        let t = input.trim();

        // A command typed after the query buffer — after SQL on the
        // same line (`select 1 \gx`) or on a line of its own below:
        // psql's lexer starts it at the line's first backslash outside
        // quotes and comments, the text before being the buffer, which
        // the command runs (`\gx`) or leaves where it is (`\x`) —
        // `MetaAction::works_on_buffer`. `shell.rs` splits the input the
        // same way.
        if let Some(at) = parser::trailing_command(t, Lexicon::Psql).filter(|&at| at > 0) {
            return self.meta_cmd_in(&t[at..], ctx);
        }
        // A command ends with its line, as psql reads one: the lines
        // after it are typed text again (`command_extent`).
        let t = &t[..self.command_extent(t, ";")];

        // ── Backslash commands — psql subset ────────────────────────────
        if let Some((cmd, rest)) = split_command(t) {
            if let Some(pattern) = describe_pattern(cmd, rest, &ctx.variables) {
                let describe = describe::Describe { pattern: &pattern, verbose: cmd.contains('+') };
                return Some(match describe.first(ctx) {
                    Ok(queries) => MetaAction::Staged(queries),
                    Err(refusal) => MetaAction::Fail(vec![refusal]),
                });
            }
            if let Some(list) = lists::command(cmd, rest, ctx) {
                return Some(list);
            }

            // The arguments as psql reads ordinary ones (OT_NORMAL):
            // quotes resolved, variables expanded, a trailing `;`
            // dropped unless the command keeps it. The commands psql
            // reads a whole line for (\h, \sf, \sv, \ef, \ev) and the
            // ones that parse their own (\prompt, \password, \c,
            // \watch) use the text as typed.
            let (args, unterminated) = pattern::slash_args(rest, &ctx.variables, !KEEPS_SEMICOLONS.contains(&cmd));
            let arg = |i: usize| args.get(i).map_or("", String::as_str);
            // The whole line, for the commands psql reads that way — a
            // trailing `;` dropped, as psql drops it there.
            let line = rest.trim_end_matches(';').trim_end();

            let action = match cmd {
                // Quit / help
                "q" | "quit" => Some(MetaAction::Disconnect),
                "?" => Some(MetaAction::Output(psql_help_subtopic(self, arg(0)))),
                "h" | "help" => {
                    if line.is_empty() || line == "commands" || line == "options" || line == "variables" {
                        Some(MetaAction::Output(psql_help_subtopic(self, line)))
                    } else {
                        Some(pg_sql_help(&line.to_ascii_uppercase()))
                    }
                }

                // \s — the history (psql's `printHistory`); writing it to
                // a file needs the host.
                "s" if arg(0).is_empty() => Some(MetaAction::ShowHistory),
                "s" => Some(MetaAction::Fail(vec![
                    "\\s: writing the history to a file isn't supported yet. \\s prints it.".into(),
                ])),

                // \conninfo — psql 18's table of the session (`conninfo`).
                "conninfo" => Some(MetaAction::Staged(vec![conninfo::QUERY.into()])),

                // \echo / \qecho / \warn — print the arguments, a space
                // between each, as psql does. Our shell has no separate
                // stderr, so \warn renders the same as \echo; the
                // warn-colour styling is a later polish item. psql's
                // leading `-n` (no newline) has nothing to join onto here.
                "echo" | "qecho" | "warn" => {
                    let printed = if arg(0) == "-n" { &args[1..] } else { &args[..] };
                    Some(MetaAction::Output(vec![printed.join(" ")]))
                }

                // \copyright — psql's copyright notice. Static text.
                "copyright" => Some(MetaAction::Output(vec![
                    "PostgreSQL Database Management System".into(),
                    "(formerly known as Postgres, then as Postgres95)".into(),
                    "".into(),
                    "Portions Copyright (c) 1996-2024, The PostgreSQL Global Development Group".into(),
                    "Portions Copyright (c) 1994, The Regents of the University of California".into(),
                    "".into(),
                    "Permission to use, copy, modify, and distribute this software and its".into(),
                    "documentation for any purpose, without fee, and without a written agreement".into(),
                    "is hereby granted, provided that the above copyright notice and this".into(),
                    "paragraph and the following two paragraphs appear in all copies.".into(),
                    "".into(),
                    "(See the full PostgreSQL license at https://www.postgresql.org/about/licence/ )".into(),
                ])),

                // \p — the query buffer, or the previous query
                // (`exec_command_print` → `print_buffer`).
                "p" | "print" => Some(MetaAction::PrintBuffer),

                // \r — reset the input buffer (psql native).
                "r" | "reset" => Some(MetaAction::ClearInput((!quiet(ctx)).then_some("Query buffer reset (cleared)."))),

                // \prompt [text] name — interactive variable read.
                // Quoted text supports spaces; bare two-word form
                // takes the first word as the prompt and the second
                // as the variable name. Single-word form treats the
                // arg as the variable name (no prompt text).
                "prompt" => Some(parse_prompt_args(rest)),

                // \password [user] — SCRAM-SHA-256 password change.
                // No-arg form targets the current session user;
                // quoted user names preserve case / spaces.
                "password" => Some(parse_password_args(line)),

                // \watch [[i=]SEC] [c=N] [m=MIN] — run the query buffer
                // (or the previous query) every SEC seconds, printing
                // each run as psql does (`shell::start_watch`).
                "watch" => Some(parse_watch_args(rest, &ctx.variables)),

                // \x [on|off|auto] — `\pset expanded` (`exec_command_x`).
                "x" => Some(expanded_action("expanded", args.first().map(String::as_str))),

                // \output format — Parliament's: `table` / `vertical`
                // for aligned with `\x` off / on, `json` / `markdown`,
                // and psql's formats by their names.
                "output" => {
                    let set = |format, expanded| Some(MetaAction::SetFormat { format, expanded });
                    const NAMES: &str = "table, vertical, json, markdown, aligned, asciidoc, csv, html, latex, latex-longtable, troff-ms, unaligned, wrapped";
                    match arg(0).to_ascii_lowercase().as_str() {
                        "table"    => set(OutputFormat::Aligned, Some(Expanded::Off)),
                        "vertical" => set(OutputFormat::Aligned, Some(Expanded::On)),
                        "json"     => set(OutputFormat::Json, None),
                        "markdown" => set(OutputFormat::Markdown, None),
                        "" => Some(MetaAction::Output(vec![format!("Usage: \\output {}", NAMES.replace(", ", " | "))])),
                        fmt => match psql_format(fmt) {
                            Ok(format) if format.name() == fmt => set(format, None),
                            _ => Some(MetaAction::Fail(vec![format!("Unknown format '{fmt}'. Valid: {NAMES}.")])),
                        },
                    }
                }

                // \pset — customize display options. Per-option
                // dispatcher modeled on psql's `\pset <option> [value]`
                // shape. Three categories:
                //   1. Honored — actually affects rendering: `null`,
                //      `format`, `tuples_only` / `t`, `footer`,
                //      `title` / `C`, `expanded` / `x`.
                //   2. Accept-and-no-op (browser-irrelevant or
                //      deferred): `pager*`, `linestyle`, `border`,
                //      `numericlocale`, `tableattr`/`T`,
                //      `unicode_*_linestyle`, `xheader_width`,
                //      `fieldsep*`, `recordsep*`, `columns`.
                //   3. Unknown — surfaced as a usage error.
                // Bare `\pset` (no args) lists every option's current
                // status. Bare `\pset <option>` (no value) toggles
                // boolean options or shows the value.
                "pset" => Some(handle_pset(&args, ctx)),

                // ── psql standalone format aliases ─────────────────────────
                // These are case-sensitive single-letter commands per psql.
                // Each is a shortcut for a specific \pset option.

                // \a — aligned, or from aligned, unaligned (psql's
                // `exec_command_a`).
                "a" => Some(MetaAction::SetFormat {
                    format: if ctx.print.format != OutputFormat::Aligned { OutputFormat::Aligned } else { OutputFormat::Unaligned },
                    expanded: None,
                }),

                // \H — html, or from html, aligned (`exec_command_html`).
                "H" | "html" => Some(MetaAction::SetFormat {
                    format: if ctx.print.format != OutputFormat::Html { OutputFormat::Html } else { OutputFormat::Aligned },
                    expanded: None,
                }),

                // \t [on|off] — `\pset tuples_only` (`exec_command_t`): a
                // value sets it silently, none toggles it.
                "t" => Some(pset_bool("tuples_only", args.first().map(String::as_str))
                    .map_or_else(|refused| MetaAction::Fail(vec![refused]), MetaAction::SetTuplesOnly)),

                // \C [title] — `\pset title` (`exec_command_C`): its one
                // argument (quote a title with spaces); none unsets it.
                "C" => Some(MetaAction::SetTitle(args.first().cloned())),

                // \T [attrs] — `\pset tableattr` (`exec_command_T`):
                // the html table's attributes; none unsets them.
                "T" => Some(text_option("tableattr", args.first().cloned(), &ctx.print.text)),

                // \f [sep] — `\pset fieldsep` (`exec_command_f`): the
                // unaligned field separator; none says what it is.
                "f" => Some(text_option("fieldsep", args.first().cloned(), &ctx.print.text)),

                // \safe-mode on|off — confirmation gate for destructive SQL
                "safe-mode" => {
                    match arg(0).to_ascii_lowercase().as_str() {
                        "on"  => Some(MetaAction::SafeMode(true)),
                        "off" => Some(MetaAction::SafeMode(false)),
                        _     => Some(MetaAction::Output(vec!["Usage: \\safe-mode on | off".into()])),
                    }
                }

                // \timing — toggle execution time display
                // \timing [on|off] — psql's exec_command_timing.
                "timing" => Some(match args.first() {
                    None => MetaAction::SetTiming(None),
                    Some(value) => match crate::psql_vars::parse_bool(value) {
                        Some(on) => MetaAction::SetTiming(Some(on)),
                        None => MetaAction::Fail(vec![format!("unrecognized value \"{value}\" for \"\\timing\": Boolean expected")]),
                    },
                }),

                // \encoding [ENCODING] — psql's: the client encoding,
                // or a switch to one (`encoding`).
                "encoding" => Some(encoding::command(&args)),

                // \errverbose — re-render the last query error in the
                // shell history. Today the wire shape carries only the
                // error message string, so this re-prints it verbatim;
                // SQLSTATE / DETAIL / HINT propagation needs a protocol
                // extension and lands in a follow-up.
                "errverbose" => Some(MetaAction::ErrVerbose),

                // \g — execute the query buffer (`shell.rs` runs what
                // comes before the command); psql semantics: an empty
                // buffer re-runs the previous query. An argument is
                // psql's output redirect (`\g /tmp/out.sql`), ignored
                // for now (no file output in the browser); future psql
                // parity could surface it as "redirect not supported."
                "g"  => Some(MetaAction::Go { expanded: false }),
                // \gx — same as \g but forces expanded (vertical)
                // output for this one execution.
                "gx" => Some(MetaAction::Go { expanded: true  }),
                // \gexec — run the buffer, then each cell of its
                // result as a statement of its own.
                "gexec" => Some(MetaAction::Gexec),
                // \gdesc — describe the buffer's result columns
                // without running it.
                "gdesc" => Some(MetaAction::Gdesc),
                // \gset [prefix] — run the buffer, its one row's
                // columns into variables.
                "gset" => Some(MetaAction::Gset { prefix: arg(0).to_string() }),
                // \crosstabview [colV [colH [colD [sortcolH]]]] — run
                // the buffer, its result pivoted.
                "crosstabview" => Some(parse_crosstabview_args(&args)),

                // \lo_unlink LOBOID — psql's `do_lo_unlink`, staged
                // (`large_object`).
                "lo_unlink" => Some(large_object::unlink(args.first().map(String::as_str), ctx)),

                // \lo_import / \lo_export — deferred. Both need a
                // streaming WS payload + browser File API; logged
                // here so the user gets a clear "not yet" rather
                // than a silent unknown-command fall-through.
                "lo_import" => Some(MetaAction::Output(vec![
                    "\\lo_import: not implemented — needs a streaming WebSocket payload + browser File API.".into(),
                    "Workaround: shell into the DB host and run `psql -c \"\\lo_import /path\"` directly.".into(),
                ])),
                "lo_export" => Some(MetaAction::Output(vec![
                    "\\lo_export: not implemented — needs a streaming WebSocket payload + browser File API.".into(),
                    "Workaround: shell into the DB host and run `psql -c \"\\lo_export <oid> /path\"` directly.".into(),
                ])),

                // \sf[+] function / \sv[+] view — the definition as psql
                // prints it (`exec_command_sf_sv`): looked up as `\ef` /
                // `\ev` look it up, staged for the reply (`show_definition`).
                "sf" | "sf+" | "sv" | "sv+" => {
                    let is_func = cmd.starts_with("sf");
                    Some(if line.is_empty() {
                        MetaAction::Fail(vec![if is_func { "function name is required" } else { "view name is required" }.into()])
                    } else {
                        MetaAction::Staged(vec![if is_func { edit_function_sql(line, None) } else { edit_view_sql(line, None) }])
                    })
                }

                // \ef [function[(argtypes)] [line]] / \ev [view [line]] —
                // edit a definition in the external editor, as psql does:
                // a trailing number is the line to open at; no name opens
                // psql's blank template. The result (tag "edit_to_input")
                // goes to the shell's editor flow, not the history.
                "ef" | "ev" => {
                    let is_func = cmd == "ef";
                    Some(match split_lineno(line) {
                        Err(e) => MetaAction::Output(vec![format!("\\{cmd}: {e}")]),
                        Ok(("", line)) => MetaAction::EditExternal {
                            seed: Some(if is_func { FUNCTION_TEMPLATE } else { VIEW_TEMPLATE }.into()),
                            line,
                        },
                        Ok((name, line)) => MetaAction::Introspect {
                            sql: if is_func { edit_function_sql(name, line) } else { edit_view_sql(name, line) },
                            tag: "edit_to_input".into(),
                        },
                    })
                }

                // \e [line] — normally caught as the buffer's last line
                // above; same parse here.
                "e" | "edit" => Some(psql_edit(&args.join(" "))),

                // \c [dbname [username [host [port]]]] — a new session, on
                // another database or the same; the shell keeps the
                // previous one until it's open, as psql does.
                "c" | "connect" => Some(psql_connect(rest, ctx)),

                // \set [name [value ...]] — the values run together, as
                // psql joins them (`\set z a b c` is `abc`).
                "set" => Some(match args.split_first() {
                    None => MetaAction::ListVariables,
                    Some((name, values)) => MetaAction::SetVariable(name.clone(), values.concat()),
                }),
                "unset" => Some(match args.first() {
                    None => MetaAction::Output(vec!["\\unset: missing required argument".into()]),
                    Some(name) => MetaAction::UnsetVariable(name.clone()),
                }),

                // psql's \if / \elif / \else / \endif: where they stand
                // in the input, they move the shell's conditional stack
                // (`psql_if`). The expression is the arguments joined.
                "if"    => Some(MetaAction::Branch(Branch::If(crate::psql_if::expression(&args, "if")))),
                "elif"  => Some(MetaAction::Branch(Branch::Elif(crate::psql_if::expression(&args, "elif")))),
                "else"  => Some(MetaAction::Branch(Branch::Else)),
                "endif" => Some(MetaAction::Branch(Branch::Endif)),
                _ if PSQL_COMMANDS.contains(&cmd) || cmd.eq_ignore_ascii_case("copy") =>
                    Some(MetaAction::Fail(vec![format!("\\{cmd} isn't supported here yet.")])),
                _ => Some(invalid_command(t)),
            };
            // An unterminated quote is psql's error for a command it reads
            // ordinary arguments for — not for one it doesn't run.
            return match unterminated {
                Some(error) if !matches!(action, None | Some(MetaAction::Fail(_))) && !READS_ITS_OWN_ARGUMENTS.contains(&cmd) =>
                    Some(MetaAction::Output(vec![error.into()])),
                _ => action,
            };
        }
        // Its assistance words (`quit`) are a line's: `named_command`.
        None
    }

    /// psql's assistance words, on a line typed while the query buffer
    /// is empty (`MainLoop`): `help` prints psql's pointers, `quit` /
    /// `exit` leave.
    fn named_command(&self, line: &str, _delimiter: &str) -> Option<MetaAction> {
        match assistance_word(line)? {
            Assistance::Help => Some(MetaAction::Output(PSQL_HELP.iter().map(|l| l.to_string()).collect())),
            Assistance::Quit => Some(MetaAction::Disconnect),
        }
    }

    /// psql's notes on the line just entered while a statement is
    /// pending (`MainLoop`): its assistance words aren't commands then and
    /// stay in the buffer — `help` gets `Use \? for help or press
    /// control-C to clear the input buffer.`, `quit` / `exit` `Use \q to
    /// quit.`, or inside a quote or comment `Use control-D to quit.`, as a
    /// `\q` there gets.
    fn line_notes(&self, input: &str, _delimiter: &str) -> Vec<crate::ShellEntry> {
        let (before, line) = input.rsplit_once('\n').unwrap_or(("", input));
        // Only an assistance word or `\q` is noted: the buffer before it
        // is read only then.
        if assistance_word(line).is_none() && !line.starts_with("\\q") {
            return Vec::new();
        }
        let buffer = psql_query_buffer(before);
        if buffer.is_empty() {
            return Vec::new();
        }
        let inside = parser::ends_inside_quote_or_comment(&buffer, Lexicon::Psql);
        let note = match assistance_word(line) {
            Some(Assistance::Help) => "Use \\? for help or press control-C to clear the input buffer.",
            Some(Assistance::Quit) if !inside => "Use \\q to quit.",
            Some(Assistance::Quit) => "Use control-D to quit.",
            None if inside && line.starts_with("\\q") => "Use control-D to quit.",
            None => return Vec::new(),
        };
        vec![crate::ShellEntry::MetaOutput(vec![note.into()])]
    }

    /// psql's `\p` (`exec_command_print`): the query buffer — its `:var`s
    /// expanded, as psql's lexer expands them into it — or, empty, the
    /// previous query (`previous_buffer`); with neither, `Query buffer is
    /// empty.` unless QUIET.
    fn print_buffer(&self, typed: &str, previous: Option<&str>, ctx: &MetaContext) -> Vec<String> {
        let buffer = parser::substitute_vars(&psql_buffer(typed), &ctx.variables);
        let shown = match previous {
            Some(previous) if buffer.is_empty() => previous.to_string(),
            _ => buffer,
        };
        match shown.is_empty() {
            false => shown.split('\n').map(String::from).collect(),
            true if quiet(ctx) => Vec::new(),
            true => vec!["Query buffer is empty.".into()],
        }
    }

    /// psql's `previous_buf`: the buffer as `MainLoop` sent it — with its
    /// `;` (`select 1;`), or up to its `\g` line (`select 7 as g `).
    fn previous_buffer(&self, sql: &str, terminator: &str) -> String {
        let sent = format!("{sql}{terminator}");
        psql_buffer(&sent[..parser::trailing_command(&sent, Lexicon::Psql).unwrap_or(sent.len())])
    }

    /// psql's `\x: extra argument "extra" ignored`, for each argument the
    /// command doesn't read (`HandleSlashCmds`, `arguments_read`). psql
    /// reads them as it reads any, quotes resolved, but in an ignored
    /// branch: no variables expanded, a `;` kept (`\x on a;` → `"a;"`).
    fn extra_arguments(&self, command: &str) -> Vec<String> {
        let Some((cmd, rest)) = split_command(command.trim_start()) else { return Vec::new() };
        let (args, unterminated) = pattern::slash_args(rest, &Variables::new(), false);
        match arguments_read(cmd, &args) {
            Some(read) if unterminated.is_none() => args.iter().skip(read)
                .map(|arg| format!("\\{cmd}: extra argument \"{arg}\" ignored"))
                .collect(),
            _ => Vec::new(),
        }
    }

    // psql: PSQL_EDITOR, then EDITOR, then VISUAL; `\e N` passes the
    // line as PSQL_EDITOR_LINENUMBER_ARG + N.
    fn editor_env_chain(&self) -> &'static [EditorVar] {
        &[EditorVar::PsqlEditor, EditorVar::Editor, EditorVar::Visual]
    }
    fn editor_line_flag_var(&self) -> Option<LineFlagVar> {
        Some(LineFlagVar::PsqlEditorLinenumberArg)
    }

    fn empty_set(&self) -> Option<&'static str> { None }
    fn interpolates_variables(&self) -> bool { true }
    fn expands_automatically(&self, columns: &[String], rows: &[Vec<String>], options: &PrintOptions) -> bool {
        // Never a single column, which would be wider still.
        columns.len() > 1
            && options.columns.is_some_and(|width| print::aligned_width(columns, rows, options.text.rules.border) > width)
    }
    /// psql's expanded printer (`print_aligned_vertical`): a `-[ RECORD
    /// n ]-` line over each row's `header | value` lines — drawn as
    /// `\pset border` and `linestyle` said — and a list's own footers
    /// after them.
    fn print_vertical(&self, columns: &[String], rows: &[Vec<String>], style: &PrintedWith) -> Vec<String> {
        let table = print::PrintTable {
            title:   style.title().map(String::from),
            headers: columns.to_vec(),
            rows:    rows.to_vec(),
            footers: style.footers.clone(),
            ..print::PrintTable::default()
        };
        let options = PrintOptions {
            expanded:    Expanded::On,
            tuples_only: style.tuples_only,
            columns:     style.columns,
            text:        TextOptions { rules: style.rules, ..TextOptions::default() },
            ..PrintOptions::default()
        };
        let mut lines = print::print_table(&table, &options);
        // psql's blank line after the records; the footer follows instead.
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }
    /// psql's `\pset null` starts empty.
    fn null_display(&self) -> &'static str { "" }
    /// noctua's sessions speak UTF8 (`encoding`).
    fn client_encoding(&self) -> Option<&'static str> { Some(encoding::CLIENT_ENCODING) }
    /// psql's `SendQuery`: `BEGIN` first when `AUTOCOMMIT` is off, the
    /// session idle and the statement one that may run in a transaction
    /// (`command_no_begin`); then, in a transaction, its savepoint when
    /// `ON_ERROR_ROLLBACK` is on — or `interactive`, as this shell is.
    fn statement_opening(&self, sql: &str, txn: shared::TxnState, vars: &Variables) -> Option<crate::plane::Opening> {
        use shared::TxnState;
        let mut queries = Vec::new();
        let mut txn = txn;
        let autocommit = vars.get("AUTOCOMMIT").and_then(|v| crate::psql_vars::parse_bool(v)).unwrap_or(true);
        if txn == TxnState::Idle && !autocommit && !command_no_begin(sql) {
            queries.push("BEGIN".to_string());
            txn = TxnState::Active;
        }
        let rollback = vars.get("ON_ERROR_ROLLBACK")
            .is_some_and(|v| v.eq_ignore_ascii_case("interactive") || crate::psql_vars::parse_bool(v) == Some(true));
        let savepoint = txn == TxnState::Active && rollback;
        if savepoint {
            queries.push(format!("SAVEPOINT {PSQL_SAVEPOINT}"));
        }
        (!queries.is_empty()).then_some(crate::plane::Opening { queries, savepoint })
    }
    /// psql's: `ROLLBACK TO` its savepoint after an error, `RELEASE` it
    /// otherwise — unless the statement's tag says it ended (`COMMIT`,
    /// `ROLLBACK`), released or set a savepoint of its own (psql's
    /// `svpt_gone`); nothing once the transaction's over.
    fn statement_closing(&self, txn: shared::TxnState, tag: Option<&str>) -> Option<String> {
        use shared::TxnState;
        match txn {
            TxnState::Failed => Some(format!("ROLLBACK TO {PSQL_SAVEPOINT}")),
            TxnState::Active if !matches!(tag, Some("COMMIT" | "SAVEPOINT" | "RELEASE" | "ROLLBACK")) =>
                Some(format!("RELEASE {PSQL_SAVEPOINT}")),
            _ => None,
        }
    }
    /// psql's `\gdesc` (`DescribeQuery`): each column's name, quoted as
    /// `PQescapeLiteral` quotes it, and its type's name as the server
    /// writes it from the OID and modifier the describe gave.
    fn described_columns_sql(&self, columns: &[shared::DescribedColumn]) -> Option<String> {
        let values: Vec<String> = columns.iter()
            .map(|c| format!("({}, '{}'::pg_catalog.oid, {})", crate::parser::escape_literal(&c.name), c.type_id, c.type_modifier))
            .collect();
        Some(format!(
            "SELECT name AS \"Column\", pg_catalog.format_type(tp, tpm) AS \"Type\"\nFROM (VALUES {}) s(name, tp, tpm)",
            values.join(","),
        ))
    }
    /// psql's `format_numeric_locale`, under its `setDecimalLocale`.
    fn localized_number(&self, value: &str, locale: &shared::NumericLocale) -> String {
        print::format_numeric_locale(value, locale)
    }
    /// psql's `column_type_alignment`, by the type's OID.
    fn column_align(&self, type_id: &str) -> Align { print::type_alignment(type_id) }
    /// psql's aligned printer centres every header.
    fn centres_headers(&self) -> bool { true }
    /// psql's printers for every format but aligned, which the grid
    /// shows; numbers line up right by their type, as psql's do.
    fn print_result(&self, columns: &[String], types: &[String], rows: &[Vec<String>], title: Option<&str>, footers: &[String], options: &PrintOptions) -> Option<Vec<String>> {
        if matches!(options.format, OutputFormat::Aligned | OutputFormat::Json | OutputFormat::Markdown) {
            return None;
        }
        let table = print::PrintTable {
            title:   title.map(String::from),
            headers: columns.to_vec(),
            aligns:  (0..columns.len()).map(|c| print::type_alignment(types.get(c).map_or("", String::as_str))).collect(),
            rows:    rows.to_vec(),
            footers: footers.to_vec(),
            count_rows: true,
        };
        Some(print::print_table(&table, options))
    }
    /// psql's footer, "(2 rows)" — which its expanded output leaves out
    /// unless there are none.
    fn rows_footer(&self, rows: usize, _warnings: u16, vertical: bool) -> Option<String> {
        (!vertical || rows == 0).then(|| format!("({rows} row{})", if rows == 1 { "" } else { "s" }))
    }
    fn timing(&self, exec_time_us: u64) -> Timing { Timing::OwnLine(psql_time(exec_time_us)) }
    /// psql shows times only after `\timing`.
    fn timed_by_default(&self) -> bool { false }
    /// psql's `CheckConnection`, with libpq's message for a connection
    /// that closed under a query.
    fn lost_connection(&self) -> LostConnection {
        LostConnection::ResetAtOnce {
            lost: &[
                "server closed the connection unexpectedly",
                "\tThis probably means the server terminated abnormally",
                "\tbefore or while processing the request.",
            ],
            not_connected: "You are currently not connected to a database.",
        }
    }
    /// psql's `do_connect`: where it connected only when that changed —
    /// or there was no connection before.
    fn connected_notice(&self, database: &str, user: &str, host: &str, port: &str, had_session: bool) -> Vec<String> {
        vec![if had_session {
            format!("You are now connected to database \"{database}\" as user \"{user}\".")
        } else {
            format!("You are now connected to database \"{database}\" as user \"{user}\" on host \"{host}\" at port \"{port}\".")
        }]
    }
    /// libpq's `connection to server at …` line, the server's refusal
    /// as psql prints it (`FATAL:  database "x" does not exist`), and
    /// psql's word that it kept the session it had.
    fn connect_failed(&self, host: &str, port: &str, error: Option<&shared::DbError>, message: &str, kept: bool) -> Vec<String> {
        let why = match error {
            Some(e) => format!("{}:  {}", e.severity.as_deref().unwrap_or("FATAL"), e.message),
            None => message.to_string(),
        };
        let mut lines = vec![format!("connection to server at \"{host}\", port {port} failed: {why}")];
        if kept { lines.push("Previous connection kept".into()); }
        lines
    }
    /// psql 18 tries the reset twice when it fails, and says so twice.
    fn reconnected_notice(&self, ok: bool, _connection_id: &str, _database: &str) -> Vec<String> {
        let line = |outcome| format!("The connection to the server was lost. Attempting reset: {outcome}");
        if ok { vec![line("Succeeded.")] } else { vec![line("Failed."), line("Failed.")] }
    }
    /// psql prints the tag as the server sent it, and nothing for an
    /// empty query (no tag).
    /// psql's `PrintQueryStatus`: the server's tag — in a paragraph
    /// under `\pset format html`, escaped as `html_escaped_print` does.
    fn command_status(&self, status: &shared::CommandStatus, format: OutputFormat) -> Vec<String> {
        status.tag.iter().map(|tag| match format {
            OutputFormat::Html => {
                let mut paragraph = String::from("<p>");
                print::html_escaped(tag, &mut paragraph);
                paragraph.push_str("</p>");
                paragraph
            }
            _ => tag.clone(),
        }).collect()
    }

    /// psql's `PrintQueryStatus` for a result with rows: only a tag
    /// starting `INSERT`, `UPDATE`, `DELETE` or `MERGE` — a RETURNING's.
    fn status_after_rows(&self, status: &shared::CommandStatus) -> bool {
        status.tag.as_deref().is_some_and(|tag| ["INSERT", "UPDATE", "DELETE", "MERGE"].iter().any(|verb| tag.starts_with(verb)))
    }

    fn format_error(&self, error: &shared::DbError, query: Option<&str>, style: ErrorStyle) -> Vec<String> {
        error::psql_error(error, query, style)
    }

    fn meta_continue(&self, input: &str, ctx: &MetaContext, replies: &[shared::BatchResult]) -> MetaStep {
        let Some((cmd, rest)) = split_command(input) else { return MetaStep::Done(Vec::new()) };
        if cmd == "conninfo" {
            return conninfo::next(ctx, replies);
        }
        if matches!(cmd, "sf" | "sf+" | "sv" | "sv+") {
            return show_definition(cmd, replies);
        }
        if cmd == "lo_unlink" {
            let (args, _) = pattern::slash_args(rest, &ctx.variables, true);
            return large_object::unlink_next(args.first().map_or("", String::as_str), ctx, replies);
        }
        if let Some(pattern) = describe_pattern(cmd, rest, &ctx.variables) {
            return describe::Describe { pattern: &pattern, verbose: cmd.contains('+') }.next(ctx, replies);
        }
        lists::next(cmd, rest, ctx, replies).unwrap_or(MetaStep::Done(Vec::new()))
    }

    fn help_lines(&self) -> Vec<String> {
        crate::format::format_help("PostgreSQL shell commands", &[
            ("\\q",               "Disconnect."),
            ("\\?, \\h",           "Display this help."),
            ("\\? topic",          "Sub-topics: commands | options | variables."),
            ("\\h KEYWORD",        "SQL syntax help (e.g. \\h SELECT, \\h *)."),
            ("\\s",               "Display the history."),
            ("\\conninfo",         "Show current connection identity."),
            ("\\copyright",        "Show PostgreSQL copyright notice."),
            ("\\echo [text]",      "Print a line (also: \\qecho, \\warn)."),
            ("\\l[x+] [PATTERN]",   "List databases (also: \\list)."),
            ("\\dn[Sx+] [PATTERN]", "List schemas."),
            ("\\du[Sx+] [PATTERN]", "List roles (also: \\dg)."),
            ("\\dt[S+] [PATTERN]", "List tables (combine: \\dtv lists tables and views)."),
            ("\\dv[S+] [PATTERN]", "List views."),
            ("\\df[anptw][Sx+] [FUNCPTRN [TYPEPTRN ...]]", "List [only agg/normal/procedure/trigger/window] functions."),
            ("\\di[S+] [PATTERN]", "List indexes."),
            ("\\ds[S+] [PATTERN]", "List sequences."),
            ("\\dm[S+] [PATTERN]", "List materialized views."),
            ("\\dx[x+] [PATTERN]",  "List extensions (+ lists each one's objects)."),
            ("\\dT[Sx+] [PATTERN]", "List data types."),
            ("\\da[Sx] [PATTERN]",  "List aggregate functions."),
            ("\\do[Sx+] [OPPTRN [TYPEPTRN [TYPEPTRN]]]", "List operators."),
            ("\\dp[Sx] [PATTERN]",  "List table, view and sequence access privileges (also: \\z)."),
            ("\\ddp[x] [PATTERN]",  "List default access privileges."),
            ("\\drg[Sx] [PATTERN]", "List role grants."),
            ("\\dE[S+] [PATTERN]", "List foreign tables."),
            ("\\des[x+] [PATTERN]", "List foreign servers."),
            ("\\det[x+] [PATTERN]", "List foreign tables (with their server)."),
            ("\\deu[x+] [PATTERN]", "List user mappings."),
            ("\\dew[x+] [PATTERN]", "List foreign-data wrappers."),
            ("\\dP[itn][x+] [PATTERN]", "List [only index/table] partitioned relations [n=nested]."),
            ("\\dF[x+] [PATTERN]",  "List text search configurations (+ shows each one's token → dictionary map)."),
            ("\\dFd[x+] [PATTERN]", "List text search dictionaries."),
            ("\\dFp[x+] [PATTERN]", "List text search parsers (+ shows each one's methods and token types)."),
            ("\\dFt[x+] [PATTERN]", "List text search templates."),
            ("\\dc[Sx+] [PATTERN]", "List conversions."),
            ("\\dC[x+] [PATTERN]",  "List casts."),
            ("\\dd[Sx] [PATTERN]",  "Show comments on constraints, operator classes and families, rules, triggers."),
            ("\\dD[Sx+] [PATTERN]", "List domains."),
            ("\\dL[Sx+] [PATTERN]", "List procedural languages."),
            ("\\dO[Sx+] [PATTERN]", "List collations."),
            ("\\dy[x+] [PATTERN]",  "List event triggers."),
            ("\\db[x+] [PATTERN]",  "List tablespaces."),
            ("\\dconfig[x+] [PATTERN]", "List configuration parameters (without a pattern: those not at their default)."),
            ("\\dA[x+] [PATTERN]",  "List access methods."),
            ("\\dAc[x+] [AMPTRN [TYPEPTRN]]", "List operator classes."),
            ("\\dAf[x+] [AMPTRN [TYPEPTRN]]", "List operator families."),
            ("\\dAo[x+] [AMPTRN [OPFPTRN]]",  "List operators of operator families."),
            ("\\dAp[x+] [AMPTRN [OPFPTRN]]",  "List support functions of operator families."),
            ("\\dX[x] [PATTERN]",   "List extended statistics."),
            ("\\drds[x] [ROLEPTRN [DBPTRN]]", "List per-role / per-database settings."),
            ("\\dRp[x+] [PATTERN]", "List replication publications (+ shows each one's tables and schemas)."),
            ("\\dRs[x+] [PATTERN]", "List replication subscriptions."),
            ("\\sf funcname",      "Show function/procedure source."),
            ("\\sv viewname",      "Show view definition."),
            ("\\ef [func [line]]", "Edit a function definition in the external editor."),
            ("\\ev [view [line]]", "Edit a view definition in the external editor."),
            ("\\d[S+] [PATTERN]",  "List relations, or describe those matching PATTERN as psql does (the name opens the Schema Explorer)."),
            ("\\c [dbname]",       "Connect to a new database (the same one when omitted)."),
            ("\\e [line]",         "Edit the query buffer (or previous query) in the external editor."),
            ("\\output format",    "Set output format (table|vertical|json|markdown, or psql's: aligned|unaligned|csv|html|asciidoc|latex|latex-longtable|troff-ms|wrapped)."),
            ("\\x",                "Toggle expanded (vertical) output mode."),
            ("\\timing",           "Toggle execution time display."),
            ("\\safe-mode on|off", "Confirm before destructive commands."),
            ("\\pset null 'str'",  "Set NULL display text."),
            ("\\pset tuples_only", "Toggle header + row-count footer (alias `\\pset t on|off`)."),
            ("\\pset footer",      "Toggle the row-count footer only."),
            ("\\pset title 'str'", "Caption above the next Table result (alias `\\pset C`)."),
            ("\\pset",             "List supported options (honored + accepted-no-op)."),
            ("\\a",                "Toggle between unaligned and aligned output mode."),
            ("\\H",                "Toggle HTML output mode."),
            ("\\t [on|off]",       "Tuples-only (alias for \\pset tuples_only)."),
            ("\\C [title]",        "Set table caption (alias for \\pset title)."),
            ("\\T [attrs]",        "Set HTML table attributes (no-op in browser)."),
            ("\\f [sep]",          "Field separator for unaligned (no-op — CSV is RFC 4180)."),
            ("\\r",                "Reset (clear) the input buffer."),
            ("\\watch [[i=]SEC] [c=N] [m=MIN]", "Execute query every SEC seconds, up to N times, stop if less than MIN rows are returned."),
            ("\\prompt [text] name", "Interactive variable read (Enter to set, Esc to cancel)."),
            ("\\password [user]",  "Change a user's password (SCRAM-SHA-256 hashed browser-side; plaintext never sent)."),
            ("\\dl[x+], \\lo_list[x+], \\lol", "List large objects."),
            ("\\lo_unlink <oid>",  "Drop a large object (SELECT lo_unlink(<oid>))."),
            ("\\lo_import / \\lo_export", "Not implemented (needs streaming WS payload + browser File API)."),
            ("\\set [name val]",   "Set a variable (no args: list all). Predefined names (ON_ERROR_STOP / PROMPT1 / ECHO / VERBOSITY / etc.) are routed via psql_vars::classify."),
            ("\\set PROMPT1 'tpl'", "Customize the live shell prompt (psql `%`-codes; default `%l SQL>`)."),
            ("\\unset name",       "Remove a variable (Honored vars restore their default)."),
            ("\\g",                "Execute the current buffer (no terminator required)."),
            ("\\gx",               "Like \\g but force vertical output for this run."),
            ("\\gexec",            "Execute buffer; then run each non-NULL result cell as further SQL."),
            ("\\gdesc",            "Describe the columns the buffer would return, without executing it (sqlx `describe()`; no side effects)."),
            ("\\gset [prefix]",    "Execute buffer; bind the single-row result into \\set variables (with optional prefix)."),
            ("\\crosstabview [colV [colH [colD [sortcolH]]]]", "Execute buffer; pivot the result into a 2-D grid (defaults: cols 1/2/3 by 1-based index)."),
            ("\\encoding",         "Show client encoding."),
            ("\\errverbose",       "Re-render the last error from this shell."),
            ("\\if EXPR",          "Conditional block start (psql semantics; nests; EXPR can be `:var`)."),
            ("\\elif EXPR",        "Alternative branch inside an \\if / \\endif block."),
            ("\\else",             "Final branch when all \\if / \\elif expressions were false."),
            ("\\endif",            "Close the innermost \\if block."),
        ])
    }

    // ── Schema explorer — stub ─────────────────────────────────────────

    fn admin_areas(&self) -> &'static [crate::AdminArea] {
        // PG full admin set: roles + cluster-level things (tablespaces,
        // databases, server config) plus database-scoped extensions
        // and schemas. Schemas live here (not on the object-kinds
        // type-bar) because they're namespaces, not first-class
        // schema-scoped objects — and a USAGE / CREATE matrix needs
        // a drill-in surface the type-bar doesn't provide.
        &[
            crate::AdminArea::Roles,
            crate::AdminArea::Tablespaces,
            crate::AdminArea::Databases,
            crate::AdminArea::Schemas,
            crate::AdminArea::Extensions,
            crate::AdminArea::ServerConfig,
        ]
    }

    fn major_keywords(&self) -> &'static [&'static str] { PG_MAJOR_KEYWORDS }

    fn list_roles_sql(&self) -> Option<&'static str> {
        // pg_roles gives one row per role across the cluster. Internal
        // pg_* roles (pg_read_all_data, etc.) are real roles too and a
        // DBA may want to inspect them, so they stay in the list — they
        // sort to the top alphabetically because of the leading 'p'.
        // ConnLimit -1 means unlimited; render as ∞ so the column doesn't
        // mislead someone into reading -1 as "negative one connections".
        Some(
            "SELECT rolname AS \"Name\", \
                    CASE WHEN rolcanlogin     THEN 'Yes' ELSE ''    END AS \"Login\", \
                    CASE WHEN rolsuper        THEN 'Yes' ELSE ''    END AS \"Superuser\", \
                    CASE WHEN rolcreatedb     THEN 'Yes' ELSE ''    END AS \"CreateDB\", \
                    CASE WHEN rolcreaterole   THEN 'Yes' ELSE ''    END AS \"CreateRole\", \
                    CASE WHEN rolreplication  THEN 'Yes' ELSE ''    END AS \"Replication\", \
                    CASE WHEN rolconnlimit = -1 THEN '∞' ELSE rolconnlimit::text END AS \"ConnLimit\" \
             FROM pg_roles \
             ORDER BY rolname"
        )
    }

    fn role_list_extra_columns(&self) -> &'static [&'static str] {
        &["Login", "Superuser", "CreateDB", "CreateRole", "Replication", "ConnLimit"]
    }

    fn role_privileges_sql(&self, name: &str, _extras: &[String]) -> Option<String> {
        // Effective access = explicit table grants + ownership-implied
        // ALL PRIVILEGES. We UNION both sources and tag each row with a
        // Source column so a DBA can see at a glance which path gives
        // the role its access. Owned databases/schemas/relations all get
        // 'ALL PRIVILEGES' since ownership carries unlimited rights on
        // the object. Schema-USAGE / routine grants are deferred to a
        // richer Phase C.5 pass.
        // CASE expressions reused across the UNION arms — derive
        // the human-readable kind label from pg_class.relkind.
        let relkind_to_kind = "CASE c.relkind \
            WHEN 'r' THEN 'table' \
            WHEN 'v' THEN 'view' \
            WHEN 'm' THEN 'matview' \
            WHEN 'S' THEN 'sequence' \
            WHEN 'p' THEN 'partitioned table' \
            WHEN 'f' THEN 'foreign table' \
            ELSE c.relkind::text END";
        let role_q = name.replace('\'', "''");
        Some(format!(
            "SELECT g.table_schema || '.' || g.table_name AS \"Object\", \
                    string_agg(g.privilege_type, ', ' ORDER BY g.privilege_type) AS \"Privileges\", \
                    CASE WHEN bool_or(g.is_grantable = 'YES') THEN 'Yes' ELSE '' END AS \"Grantable\", \
                    'Grant'::text AS \"Source\", \
                    COALESCE({relkind_to_kind}, 'table')::text AS \"Kind\" \
             FROM information_schema.role_table_grants g \
             LEFT JOIN pg_namespace n ON n.nspname = g.table_schema \
             LEFT JOIN pg_class c \
                    ON c.relname = g.table_name \
                   AND c.relnamespace = n.oid \
                   AND c.relkind IN ('r','v','m','S','p','f') \
             WHERE g.grantee = '{0}' \
             GROUP BY g.table_schema, g.table_name, c.relkind \
             UNION ALL \
             SELECT datname AS \"Object\", \
                    'ALL PRIVILEGES'::text AS \"Privileges\", \
                    'Yes'::text AS \"Grantable\", \
                    'Owner (database)'::text AS \"Source\", \
                    'database'::text AS \"Kind\" \
             FROM pg_database \
             WHERE datdba = (SELECT oid FROM pg_roles WHERE rolname = '{0}') \
             UNION ALL \
             SELECT nspname AS \"Object\", \
                    'ALL PRIVILEGES'::text AS \"Privileges\", \
                    'Yes'::text AS \"Grantable\", \
                    'Owner (schema)'::text AS \"Source\", \
                    'schema'::text AS \"Kind\" \
             FROM pg_namespace \
             WHERE nspowner = (SELECT oid FROM pg_roles WHERE rolname = '{0}') \
               AND nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
             UNION ALL \
             SELECT n.nspname || '.' || c.relname AS \"Object\", \
                    'ALL PRIVILEGES'::text AS \"Privileges\", \
                    'Yes'::text AS \"Grantable\", \
                    ('Owner (' || {relkind_to_kind} || ')')::text AS \"Source\", \
                    {relkind_to_kind}::text AS \"Kind\" \
             FROM pg_class c \
             JOIN pg_namespace n ON c.relnamespace = n.oid \
             WHERE c.relowner = (SELECT oid FROM pg_roles WHERE rolname = '{0}') \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
               AND c.relkind IN ('r','v','m','S','p','f') \
             UNION ALL \
             /* Routine EXECUTE grants from pg_proc.proacl. Overloaded
                names collapse to a single row per (schema, name) for
                consistency with the schema-explorer identity; future
                work could break them out by signature. */ \
             SELECT n.nspname || '.' || p.proname AS \"Object\", \
                    'EXECUTE'::text AS \"Privileges\", \
                    CASE WHEN bool_or(g.is_grantable) THEN 'Yes' ELSE '' END AS \"Grantable\", \
                    'Grant'::text AS \"Source\", \
                    CASE p.prokind \
                        WHEN 'f' THEN 'function' \
                        WHEN 'p' THEN 'procedure' \
                        WHEN 'a' THEN 'aggregate' \
                        WHEN 'w' THEN 'window function' \
                        ELSE p.prokind::text \
                    END::text AS \"Kind\" \
             FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             CROSS JOIN LATERAL aclexplode(p.proacl) g \
             JOIN pg_roles r ON r.oid = g.grantee \
             WHERE r.rolname = '{0}' \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
             GROUP BY n.nspname, p.proname, p.prokind \
             UNION ALL \
             /* Routine ownership (PG owners hold implicit EXECUTE on
                the routines they own). Same overload-collapsing rule. */ \
             SELECT n.nspname || '.' || p.proname AS \"Object\", \
                    'ALL PRIVILEGES'::text AS \"Privileges\", \
                    'Yes'::text AS \"Grantable\", \
                    ('Owner (' || CASE p.prokind \
                        WHEN 'f' THEN 'function' \
                        WHEN 'p' THEN 'procedure' \
                        WHEN 'a' THEN 'aggregate' \
                        WHEN 'w' THEN 'window function' \
                        ELSE p.prokind::text \
                    END || ')')::text AS \"Source\", \
                    CASE p.prokind \
                        WHEN 'f' THEN 'function' \
                        WHEN 'p' THEN 'procedure' \
                        WHEN 'a' THEN 'aggregate' \
                        WHEN 'w' THEN 'window function' \
                        ELSE p.prokind::text \
                    END::text AS \"Kind\" \
             FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE p.proowner = (SELECT oid FROM pg_roles WHERE rolname = '{0}') \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
             GROUP BY n.nspname, p.proname, p.prokind \
             ORDER BY 4, 1",
            role_q,
            relkind_to_kind = relkind_to_kind,
        ))
    }

    fn role_privileges_columns(&self) -> &'static [&'static str] {
        &["Object", "Privileges", "Grantable", "Source", "Kind"]
    }

    fn role_affected_objects_sql(
        &self,
        _name: &str,
        _extras: &[String],
        selected_objects: &[String],
    ) -> Option<String> {
        if selected_objects.is_empty() { return None; }
        // Per-scope WHERE clause. We OR them together so one query
        // covers every selected grant. Scope forms we handle:
        //
        //   `*.*`            → every table the user can see.
        //   `<schema>.<tbl>` → that one table.
        //   `<schema>.*`     → every table in <schema>.
        //   `<schema>`       — bare schema name (PG owned-schema rows)
        //                       → every table in <schema>.
        //
        // PG doesn't have MySQL-style `db.tbl` grants at the
        // information_schema level (we'd see them in role_table_grants
        // as `<schema>.<table>` already), so the bare-schema form is
        // the only ambiguity and we treat it as a schema.
        let esc = |s: &str| s.replace('\'', "''");
        // Build the OR clause for one arm of the UNION. `name_col` is
        // the qualified object-name column in that arm (c.relname for
        // pg_class, p.proname for pg_proc) — everything else is shared.
        let build_clauses = |name_col: &str| -> String {
            selected_objects.iter().map(|obj| {
                if obj == "*.*" {
                    "TRUE".to_string()
                } else if let Some(schema) = obj.strip_suffix(".*") {
                    format!("n.nspname = '{}'", esc(schema))
                } else if let Some((s, t)) = obj.split_once('.') {
                    format!("(n.nspname = '{}' AND {} = '{}')", esc(s), name_col, esc(t))
                } else {
                    // Bare name — treat as schema. Database-ownership
                    // rows are remapped to `*.*` by the frontend before
                    // reaching here (see the selected-objects mapping
                    // in render_explorer_pane's affected-objects
                    // lazy-fetch effect).
                    format!("n.nspname = '{}'", esc(obj))
                }
            }).collect::<Vec<_>>().join(" OR ")
        };
        let where_sql = build_clauses("c.relname");
        let where_sql_proc = build_clauses("p.proname");
        // pg_class beats information_schema.tables here — the latter
        // misses materialized views (relkind 'm') and sequences
        // (relkind 'S'), which a role can certainly own / be granted
        // on. We pull tables, views, matviews, sequences, foreign
        // tables, and partitioned tables, plus routines from pg_proc
        // so functions / procedures touched by EXECUTE grants surface
        // alongside the relations. System schemas are filtered.
        Some(format!(
            "SELECT DISTINCT \
                    n.nspname AS \"Schema\", \
                    CASE c.relkind \
                        WHEN 'r' THEN 'table' \
                        WHEN 'v' THEN 'view' \
                        WHEN 'm' THEN 'matview' \
                        WHEN 'S' THEN 'sequence' \
                        WHEN 'p' THEN 'partitioned table' \
                        WHEN 'f' THEN 'foreign table' \
                        ELSE c.relkind::text \
                    END AS \"Kind\", \
                    c.relname AS \"Object\" \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE ({where_sql}) \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
               AND c.relkind IN ('r','v','m','S','f','p') \
             UNION ALL \
             SELECT DISTINCT \
                    n.nspname, \
                    CASE p.prokind \
                        WHEN 'f' THEN 'function' \
                        WHEN 'p' THEN 'procedure' \
                        WHEN 'a' THEN 'aggregate' \
                        WHEN 'w' THEN 'window function' \
                        ELSE p.prokind::text \
                    END, \
                    p.proname \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE ({where_sql_proc}) \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
             ORDER BY 2, 1, 3",
        ))
    }

    fn role_is_superuser(&self, row: &AdminRow) -> bool {
        // role_list_extra_columns is ["Login", "Superuser", "CreateDB",
        // "CreateRole", "Replication", "ConnLimit"], so Superuser sits
        // at extras[1]. The list_roles_sql emits 'Yes' / '' values.
        row.extras.get(1).map(|s| s == "Yes").unwrap_or(false)
    }

    fn object_grants_sql(
        &self,
        kind: ObjectKind,
        schema: &str,
        name: &str,
    ) -> Option<String> {
        let s = schema.replace('\'', "''");
        let n = name.replace('\'', "''");
        // The matrix query is identical for any PG object that
        // stores an ACL — only the source table, ACL column, and
        // applicable privilege list differ. Tables / views /
        // matviews / sequences / partitioned / foreign tables all
        // live in pg_class.relacl; functions and procedures live
        // in pg_proc.proacl. We build a unifying `target` CTE
        // that aliases the source columns to `acl` and
        // `owner_oid`, then reuse one matrix shape downstream.
        //
        // PG routines can be overloaded — multiple pg_proc rows
        // share (schema, name) but differ on argument signature.
        // The schema explorer's identity is just (schema, name)
        // so we deliberately don't LIMIT 1 here: every overload
        // contributes its grants, the matrix collapses on role
        // (best-source-wins via role_class), and the result is the
        // aggregate access the role has to "this name". Per-
        // signature disambiguation is a future detail-page concern.
        let (target_cte_body, priv_kinds_array) = match kind {
            ObjectKind::Tables | ObjectKind::Views => (
                format!(
                    "SELECT c.oid, c.relacl AS acl, c.relowner AS owner_oid, \
                            (COALESCE(pg_get_userbyid(c.relowner), '[unknown owner]') \
                             COLLATE \"default\")::text AS owner_name \
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = '{s}' AND c.relname = '{n}'",
                    s = s, n = n,
                ),
                "ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']",
            ),
            ObjectKind::Procedures | ObjectKind::Functions => (
                format!(
                    "SELECT p.oid, p.proacl AS acl, p.proowner AS owner_oid, \
                            (COALESCE(pg_get_userbyid(p.proowner), '[unknown owner]') \
                             COLLATE \"default\")::text AS owner_name \
                     FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                     WHERE n.nspname = '{s}' AND p.proname = '{n}'",
                    s = s, n = n,
                ),
                "ARRAY['EXECUTE']",
            ),
            _ => return None,
        };
        // Recursive CTE walks pg_auth_members upward from each role
        // through every role it inherits from. Depth = 0 is the role
        // itself (a direct grantee match); depth > 0 carries the
        // chain of ancestor names. PUBLIC is folded in as its own
        // grantee row rather than expanded across every role —
        // expanding would balloon the matrix and dilute the signal.
        // Sectioned matrix output: every role is classified into one
        // of five sources: owner > superuser > direct > inherited >
        // none. Owner / super-user rows produce a row per privilege
        // (synthetic — they bypass the ACL); direct/inherited come
        // from `aclexplode(relacl)` joined with `pg_auth_members`;
        // none rows fill in roles with no relationship at all so
        // the user sees the full role universe and can identify
        // gaps. The frontend filters none-rows behind a toggle.
        Some(format!(
            "WITH RECURSIVE ancestor_chain(role_oid, ancestor_oid, depth, chain) AS ( \
                SELECT oid, oid, 0, ARRAY[]::text[] FROM pg_roles \
                UNION ALL \
                SELECT ac.role_oid, am.roleid, ac.depth + 1, \
                       ac.chain || (r.rolname COLLATE \"default\")::text \
                FROM ancestor_chain ac \
                JOIN pg_auth_members am ON am.member = ac.ancestor_oid \
                JOIN pg_roles r ON r.oid = am.roleid \
                WHERE ac.depth < 64 \
            ), \
            target AS ( \
                /* COALESCE the owner name in case the owner OID points at
                   a dropped role (a `DROP ROLE` without `REASSIGN OWNED
                   FIRST` orphans the OID). Without the guard,
                   pg_get_userbyid returns NULL and the synthesized owner
                   row carries a NULL grantee that the parser silently
                   filters out — leaving an empty-looking matrix on an
                   actually-orphaned object. */ \
                {target_cte_body} \
            ), \
            priv_kinds AS ( \
                SELECT unnest({priv_kinds_array}) AS pt \
            ), \
            direct_grants AS ( \
                SELECT g.grantee AS grantee_oid, g.privilege_type, g.is_grantable \
                FROM target t CROSS JOIN LATERAL aclexplode(t.acl) g \
                WHERE t.acl IS NOT NULL \
                UNION ALL \
                /* Freshly-created object (acl IS NULL) → synthesize
                   owner-implicit grants for every applicable privilege. */ \
                SELECT t.owner_oid, pk.pt, true \
                FROM target t CROSS JOIN priv_kinds pk WHERE t.acl IS NULL \
            ), \
            /* Per-role best-source classification. Drives the section
               grouping in the matrix UI. owner > superuser > direct
               > inherited > none. */ \
            role_class AS ( \
                SELECT r.oid, \
                       (r.rolname COLLATE \"default\")::text AS rolname, \
                       r.rolsuper, \
                       CASE \
                           /* Explicit COLLATE on the rolname side
                              matches owner_name's \"default\" collation
                              from the `target` CTE — same pattern as
                              the recursive arm. PG's implicit name→
                              text cast drops the C collation, so this
                              works either way, but staying explicit
                              keeps the SQL self-documenting. */ \
                           WHEN (r.rolname COLLATE \"default\")::text = (SELECT owner_name FROM target) THEN 'owner' \
                           WHEN r.rolsuper THEN 'superuser' \
                           WHEN EXISTS ( \
                               SELECT 1 FROM ancestor_chain ac \
                               JOIN direct_grants dg ON dg.grantee_oid = ac.ancestor_oid \
                               WHERE ac.role_oid = r.oid AND ac.depth = 0 \
                           ) THEN 'direct' \
                           WHEN EXISTS ( \
                               SELECT 1 FROM ancestor_chain ac \
                               JOIN direct_grants dg ON dg.grantee_oid = ac.ancestor_oid \
                               WHERE ac.role_oid = r.oid AND ac.depth > 0 \
                           ) THEN 'inherited' \
                           ELSE 'none' \
                       END AS role_category \
                FROM pg_roles r \
            ) \
            /* Owner rows: one per privilege, synthetic (the owner has
               implicit ALL privileges in PG regardless of relacl). */ \
            SELECT t.owner_name AS \"Grantee\", \
                   pk.pt AS \"Privilege\", \
                   'owner' AS \"Source\", \
                   '' AS \"Chain\", \
                   'YES' AS \"Grantable\", \
                   'owner' AS \"RoleCategory\" \
            FROM target t CROSS JOIN priv_kinds pk \
            UNION ALL \
            /* Superuser rows: one per (super-role, privilege), excluding
               the owner if they're also super (already covered above). */ \
            SELECT rc.rolname, pk.pt, 'superuser', '', 'YES', 'superuser' \
            FROM role_class rc CROSS JOIN priv_kinds pk \
            WHERE rc.role_category = 'superuser' \
            UNION ALL \
            /* Direct + inherited grant rows. Excludes super/owner so
               we don't double-render. The chain field carries only the
               final hop (the granting role) for the row label; the
               full chain goes elsewhere if needed. */ \
            SELECT rc.rolname, \
                   dg.privilege_type, \
                   CASE WHEN ac.depth = 0 THEN 'direct' ELSE 'inherited' END, \
                   CASE WHEN ac.depth = 0 THEN '' \
                        ELSE ac.chain[array_length(ac.chain, 1)] END, \
                   CASE WHEN dg.is_grantable THEN 'YES' ELSE '' END, \
                   rc.role_category \
            FROM ancestor_chain ac \
            JOIN role_class rc ON rc.oid = ac.role_oid \
            JOIN direct_grants dg ON dg.grantee_oid = ac.ancestor_oid \
            WHERE dg.grantee_oid <> 0 \
              AND rc.role_category IN ('direct','inherited') \
            UNION ALL \
            /* PUBLIC pseudo-role — only fires when the ACL grants to
               PUBLIC (grantee_oid = 0). Categorize as direct so it
               sorts into the direct grantees section. */ \
            SELECT 'PUBLIC'::text, dg.privilege_type, 'direct', '', \
                   CASE WHEN dg.is_grantable THEN 'YES' ELSE '' END, \
                   'direct' \
            FROM direct_grants dg WHERE dg.grantee_oid = 0 \
            UNION ALL \
            /* No-grant rows: every role with no direct or inherited
               access, one row per (role, privilege). The frontend
               toggles whether to render these. */ \
            SELECT rc.rolname, pk.pt, 'none', '', '', 'none' \
            FROM role_class rc CROSS JOIN priv_kinds pk \
            WHERE rc.role_category = 'none' \
            ORDER BY 1, 2",
            target_cte_body = target_cte_body,
            priv_kinds_array = priv_kinds_array,
        ))
    }

    fn schema_privilege_kinds(&self) -> &'static [&'static str] {
        // PG namespace privileges per pg_namespace.nspacl. USAGE
        // (can reference objects inside the schema) and CREATE
        // (can create new objects in the schema) are the two
        // PostgreSQL emits. Order is the matrix column order.
        &["USAGE", "CREATE"]
    }

    fn grant_sql(
        &self,
        kind: ObjectKind,
        schema: &str,
        name: &str,
        grantee: &str,
        privilege: &str,
    ) -> Option<String> {
        let (verb, prep) = ("GRANT", "TO");
        Some(pg_grant_or_revoke(verb, prep, kind, schema, name, grantee, privilege)?)
    }

    fn revoke_sql(
        &self,
        kind: ObjectKind,
        schema: &str,
        name: &str,
        grantee: &str,
        privilege: &str,
    ) -> Option<String> {
        let (verb, prep) = ("REVOKE", "FROM");
        Some(pg_grant_or_revoke(verb, prep, kind, schema, name, grantee, privilege)?)
    }

    fn schema_grant_sql(
        &self,
        schema: &str,
        grantee: &str,
        privilege: &str,
    ) -> Option<String> {
        let s = pg_quote_ident(schema);
        let g = pg_quote_grantee(grantee);
        // privilege passed from the matrix is the user-facing label
        // ("USAGE" / "CREATE") which is also the SQL keyword.
        Some(format!("GRANT {} ON SCHEMA {} TO {}", privilege, s, g))
    }

    fn schema_revoke_sql(
        &self,
        schema: &str,
        grantee: &str,
        privilege: &str,
    ) -> Option<String> {
        let s = pg_quote_ident(schema);
        let g = pg_quote_grantee(grantee);
        Some(format!("REVOKE {} ON SCHEMA {} FROM {}", privilege, s, g))
    }

    fn object_privilege_kinds(&self, kind: ObjectKind) -> &'static [&'static str] {
        match kind {
            // Postgres table-level privileges per pg_class.relacl.
            // Order is the matrix column order.
            ObjectKind::Tables => &[
                "SELECT", "INSERT", "UPDATE", "DELETE",
                "TRUNCATE", "REFERENCES", "TRIGGER",
            ],
            // Views drop TRUNCATE, REFERENCES, TRIGGER (no
            // underlying table semantics to truncate / fk / trig).
            ObjectKind::Views => &["SELECT", "INSERT", "UPDATE", "DELETE"],
            // Routines: PG only defines EXECUTE on a function or
            // procedure — there's no SELECT/UPDATE-style breakdown.
            // The matrix collapses to a single column.
            ObjectKind::Procedures | ObjectKind::Functions => &["EXECUTE"],
            _ => &[],
        }
    }

    fn area_list_sql(&self, area: AdminArea) -> Option<String> {
        match area {
            AdminArea::Tablespaces => Some(
                // Every PG cluster ships with two built-ins:
                //   pg_default — the default tablespace, backed by the
                //                cluster's $PGDATA/base directory. Used
                //                whenever a relation is created without
                //                an explicit TABLESPACE clause.
                //   pg_global  — holds shared system catalogs (pg_database,
                //                pg_authid, pg_tablespace itself). Both
                //                report empty strings from
                //                pg_tablespace_location() since they live
                //                inside the data directory; we synthesize
                //                a "(built-in)" label so the row isn't
                //                blank.
                // pg_tablespace_size() requires CREATE on the tablespace
                // OR superuser; non-privileged callers will get an error
                // back which the renderer surfaces as-is.
                // Built-in tablespaces have no override location; their
                // physical home is $PGDATA/base (pg_default) and
                // $PGDATA/global (pg_global). We pull data_directory from
                // pg_settings — readable by superusers; non-privileged
                // callers get NULL and the COALESCE falls back to a
                // relative-path label so the column is still informative.
                "SELECT t.spcname AS \"Name\", \
                        pg_catalog.pg_get_userbyid(t.spcowner) AS \"Owner\", \
                        CASE \
                          WHEN COALESCE(pg_catalog.pg_tablespace_location(t.oid), '') <> '' \
                            THEN pg_catalog.pg_tablespace_location(t.oid) \
                          WHEN t.spcname = 'pg_default' THEN \
                            COALESCE( \
                              (SELECT setting FROM pg_settings WHERE name = 'data_directory') \
                                || '/base', \
                              '$PGDATA/base') \
                          WHEN t.spcname = 'pg_global'  THEN \
                            COALESCE( \
                              (SELECT setting FROM pg_settings WHERE name = 'data_directory') \
                                || '/global', \
                              '$PGDATA/global') \
                          ELSE '' \
                        END AS \"Location\", \
                        pg_catalog.pg_size_pretty(pg_catalog.pg_tablespace_size(t.oid)) AS \"Size\", \
                        (SELECT count(*)::text FROM pg_database \
                          WHERE dattablespace = t.oid) AS \"Default DBs\", \
                        CASE WHEN t.spcname = 'pg_default' THEN 'cluster default' \
                             WHEN t.spcname = 'pg_global'  THEN 'shared catalogs' \
                             ELSE '' END AS \"Notes\" \
                 FROM pg_catalog.pg_tablespace t \
                 ORDER BY t.spcname".to_string()
            ),
            AdminArea::Databases => Some(
                "SELECT datname AS \"Name\", \
                        pg_catalog.pg_get_userbyid(datdba) AS \"Owner\", \
                        pg_catalog.pg_encoding_to_char(encoding) AS \"Encoding\", \
                        datcollate AS \"Collate\", \
                        datctype AS \"CType\", \
                        pg_size_pretty(pg_database_size(datname)) AS \"Size\" \
                 FROM pg_catalog.pg_database \
                 WHERE NOT datistemplate \
                 ORDER BY datname".to_string()
            ),
            AdminArea::Extensions => Some(
                "SELECT e.extname AS \"Name\", \
                        e.extversion AS \"Version\", \
                        n.nspname AS \"Schema\", \
                        COALESCE(c.description, '') AS \"Description\" \
                 FROM pg_extension e \
                 JOIN pg_namespace n ON e.extnamespace = n.oid \
                 LEFT JOIN pg_description c ON c.objoid = e.oid \
                                            AND c.classoid = 'pg_extension'::regclass \
                 ORDER BY e.extname".to_string()
            ),
            AdminArea::Schemas => Some(
                // System schemas (pg_catalog, information_schema, pg_toast,
                // pg_temp_*) hidden — they're not directly useful to the
                // user and crowd the list. Object counts give a quick
                // sense of which schemas are non-empty without needing
                // to drill in.
                "SELECT n.nspname AS \"Name\", \
                        pg_catalog.pg_get_userbyid(n.nspowner) AS \"Owner\", \
                        COALESCE(d.description, '') AS \"Comment\", \
                        ( SELECT COUNT(*) FROM pg_class c \
                          WHERE c.relnamespace = n.oid \
                            AND c.relkind IN ('r','v','m','S','p','f') )::bigint AS \"Objects\" \
                 FROM pg_namespace n \
                 LEFT JOIN pg_description d ON d.objoid = n.oid \
                                            AND d.classoid = 'pg_namespace'::regclass \
                 WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
                   AND n.nspname NOT LIKE 'pg_temp_%' \
                   AND n.nspname NOT LIKE 'pg_toast_temp_%' \
                 ORDER BY n.nspname".to_string()
            ),
            AdminArea::ServerConfig => Some(
                "SELECT name AS \"Name\", \
                        setting AS \"Value\", \
                        COALESCE(unit, '') AS \"Unit\", \
                        category AS \"Category\" \
                 FROM pg_catalog.pg_settings \
                 ORDER BY category, name".to_string()
            ),
            _ => None,
        }
    }

    fn area_list_columns(&self, area: AdminArea) -> &'static [&'static str] {
        match area {
            AdminArea::Tablespaces  => &["Name", "Owner", "Location", "Size", "Default DBs", "Notes"],
            AdminArea::Databases    => &["Name", "Owner", "Encoding", "Collate", "CType", "Size"],
            AdminArea::Extensions   => &["Name", "Version", "Schema", "Description"],
            AdminArea::Schemas      => &["Name", "Owner", "Comment", "Objects"],
            AdminArea::ServerConfig => &["Name", "Value", "Unit", "Category"],
            _ => &[],
        }
    }

    fn area_row_clickable(&self, area: AdminArea) -> bool {
        // Tablespaces + Schemas drill in for v1; other areas stay
        // decorative. Adding Databases / Extensions next is a matter
        // of filling in their detail-section SQL — the infrastructure
        // is shared.
        matches!(area, AdminArea::Tablespaces | AdminArea::Schemas)
    }

    fn area_row_detail_sections(&self, area: AdminArea) -> &'static [&'static str] {
        match area {
            AdminArea::Tablespaces => &["Default for Databases", "Relations Stored Here"],
            // Schemas' one section is the USAGE/CREATE matrix. The
            // section name "Privileges" is the signal the row-detail
            // renderer uses to dispatch to render_privileges_matrix
            // instead of the generic flat-table renderer.
            AdminArea::Schemas     => &["Privileges"],
            _ => &[],
        }
    }

    fn area_row_detail_sql(&self, area: AdminArea, row_id: &str, section: &str) -> Option<String> {
        let row_q = row_id.replace('\'', "''");
        match (area, section) {
            (AdminArea::Tablespaces, "Default for Databases") => Some(format!(
                "SELECT datname AS \"Database\", \
                        pg_catalog.pg_get_userbyid(datdba) AS \"Owner\", \
                        pg_catalog.pg_encoding_to_char(encoding) AS \"Encoding\", \
                        pg_catalog.pg_size_pretty(pg_catalog.pg_database_size(oid)) AS \"Size\" \
                 FROM pg_catalog.pg_database \
                 WHERE dattablespace = (SELECT oid FROM pg_catalog.pg_tablespace WHERE spcname = '{}') \
                 ORDER BY datname",
                row_q
            )),
            (AdminArea::Schemas, "Privileges") => Some({
                let s = row_id.replace('\'', "''");
                // Mirror shape of object_grants_sql but with target=
                // pg_namespace and priv_kinds=[USAGE, CREATE]. The
                // role-class CTE + section synthesis is byte-for-byte
                // identical — kept inline rather than factored because
                // the SQL is stable and the helper would have to take
                // a half-dozen knobs to express the differences.
                format!(
                    "WITH RECURSIVE ancestor_chain(role_oid, ancestor_oid, depth, chain) AS ( \
                        SELECT oid, oid, 0, ARRAY[]::text[] FROM pg_roles \
                        UNION ALL \
                        SELECT ac.role_oid, am.roleid, ac.depth + 1, \
                               ac.chain || (r.rolname COLLATE \"default\")::text \
                        FROM ancestor_chain ac \
                        JOIN pg_auth_members am ON am.member = ac.ancestor_oid \
                        JOIN pg_roles r ON r.oid = am.roleid \
                        WHERE ac.depth < 64 \
                    ), \
                    target AS ( \
                        SELECT n.oid, n.nspacl AS acl, n.nspowner AS owner_oid, \
                               (COALESCE(pg_get_userbyid(n.nspowner), '[unknown owner]') \
                                COLLATE \"default\")::text AS owner_name \
                        FROM pg_namespace n WHERE n.nspname = '{s}' \
                    ), \
                    /* Keep this list in sync with schema_privilege_kinds() — \
                       the renderer uses the trait's slice to label the \
                       matrix columns, and a mismatch silently drops a \
                       column or labels it wrong. */ \
                    priv_kinds AS ( \
                        SELECT unnest(ARRAY['USAGE','CREATE']) AS pt \
                    ), \
                    direct_grants AS ( \
                        SELECT g.grantee AS grantee_oid, g.privilege_type, g.is_grantable \
                        FROM target t CROSS JOIN LATERAL aclexplode(t.acl) g \
                        WHERE t.acl IS NOT NULL \
                        UNION ALL \
                        SELECT t.owner_oid, pk.pt, true \
                        FROM target t CROSS JOIN priv_kinds pk WHERE t.acl IS NULL \
                    ), \
                    role_class AS ( \
                        SELECT r.oid, \
                               (r.rolname COLLATE \"default\")::text AS rolname, \
                               r.rolsuper, \
                               CASE \
                                   WHEN (r.rolname COLLATE \"default\")::text = (SELECT owner_name FROM target) THEN 'owner' \
                                   WHEN r.rolsuper THEN 'superuser' \
                                   WHEN EXISTS ( \
                                       SELECT 1 FROM ancestor_chain ac \
                                       JOIN direct_grants dg ON dg.grantee_oid = ac.ancestor_oid \
                                       WHERE ac.role_oid = r.oid AND ac.depth = 0 \
                                   ) THEN 'direct' \
                                   WHEN EXISTS ( \
                                       SELECT 1 FROM ancestor_chain ac \
                                       JOIN direct_grants dg ON dg.grantee_oid = ac.ancestor_oid \
                                       WHERE ac.role_oid = r.oid AND ac.depth > 0 \
                                   ) THEN 'inherited' \
                                   ELSE 'none' \
                               END AS role_category \
                        FROM pg_roles r \
                    ) \
                    SELECT t.owner_name AS \"Grantee\", \
                           pk.pt AS \"Privilege\", \
                           'owner' AS \"Source\", \
                           '' AS \"Chain\", \
                           'YES' AS \"Grantable\", \
                           'owner' AS \"RoleCategory\" \
                    FROM target t CROSS JOIN priv_kinds pk \
                    UNION ALL \
                    SELECT rc.rolname, pk.pt, 'superuser', '', 'YES', 'superuser' \
                    FROM role_class rc CROSS JOIN priv_kinds pk \
                    WHERE rc.role_category = 'superuser' \
                    UNION ALL \
                    SELECT rc.rolname, \
                           dg.privilege_type, \
                           CASE WHEN ac.depth = 0 THEN 'direct' ELSE 'inherited' END, \
                           CASE WHEN ac.depth = 0 THEN '' \
                                ELSE ac.chain[array_length(ac.chain, 1)] END, \
                           CASE WHEN dg.is_grantable THEN 'YES' ELSE '' END, \
                           rc.role_category \
                    FROM ancestor_chain ac \
                    JOIN role_class rc ON rc.oid = ac.role_oid \
                    JOIN direct_grants dg ON dg.grantee_oid = ac.ancestor_oid \
                    WHERE dg.grantee_oid <> 0 \
                      AND rc.role_category IN ('direct','inherited') \
                    UNION ALL \
                    SELECT 'PUBLIC'::text, dg.privilege_type, 'direct', '', \
                           CASE WHEN dg.is_grantable THEN 'YES' ELSE '' END, \
                           'direct' \
                    FROM direct_grants dg WHERE dg.grantee_oid = 0 \
                    UNION ALL \
                    SELECT rc.rolname, pk.pt, 'none', '', '', 'none' \
                    FROM role_class rc CROSS JOIN priv_kinds pk \
                    WHERE rc.role_category = 'none' \
                    ORDER BY 1, 2",
                    s = s
                )
            }),
            (AdminArea::Tablespaces, "Relations Stored Here") => Some(format!(
                // Lists relations whose reltablespace EXPLICITLY equals
                // this tablespace's oid. Note this excludes relations
                // sitting in pg_default-by-default, since those have
                // reltablespace = 0 (PG's "use the database's default"
                // sentinel) rather than pg_default's actual oid. So
                // pg_default's section will read empty even though most
                // relations physically live there — see the empty-state
                // copy on the renderer side for the explanation.
                "SELECT n.nspname AS \"Schema\", \
                        c.relname AS \"Name\", \
                        CASE c.relkind \
                          WHEN 'r' THEN 'table' \
                          WHEN 'v' THEN 'view' \
                          WHEN 'm' THEN 'matview' \
                          WHEN 'S' THEN 'sequence' \
                          WHEN 'i' THEN 'index' \
                          WHEN 'p' THEN 'partitioned table' \
                          WHEN 'f' THEN 'foreign table' \
                          ELSE c.relkind::text \
                        END AS \"Kind\", \
                        pg_catalog.pg_size_pretty(pg_catalog.pg_relation_size(c.oid)) AS \"Size\" \
                 FROM pg_catalog.pg_class c \
                 JOIN pg_catalog.pg_namespace n ON c.relnamespace = n.oid \
                 WHERE c.reltablespace = (SELECT oid FROM pg_catalog.pg_tablespace WHERE spcname = '{}') \
                   AND c.relkind IN ('r','v','m','S','i','p','f') \
                 ORDER BY n.nspname, c.relname",
                row_q
            )),
            _ => None,
        }
    }

    fn area_row_detail_columns(&self, area: AdminArea, section: &str) -> &'static [&'static str] {
        match (area, section) {
            (AdminArea::Tablespaces, "Default for Databases") => &["Database", "Owner", "Encoding", "Size"],
            (AdminArea::Tablespaces, "Relations Stored Here") => &["Schema", "Name", "Kind", "Size"],
            // Schemas Privileges section returns the matrix's
            // six-column flat shape; the renderer special-cases
            // section == "Privileges" and pivots to the matrix.
            (AdminArea::Schemas, "Privileges") =>
                &["Grantee", "Privilege", "Source", "Chain", "Grantable", "RoleCategory"],
            _ => &[],
        }
    }

    fn object_kinds(&self) -> &'static [ObjectKind] {
        // Postgres has no native EVENT concept; drop that kind from the strip.
        &[
            ObjectKind::Tables,
            ObjectKind::Views,
            ObjectKind::Procedures,
            ObjectKind::Functions,
            ObjectKind::Triggers,
        ]
    }

    fn list_schemas_sql(&self) -> &'static str {
        "SELECT schema_name FROM information_schema.schemata \
         WHERE schema_name NOT LIKE 'pg\\_%' ESCAPE '\\' \
         ORDER BY schema_name"
    }

    fn system_schemas(&self) -> &'static [&'static str] {
        &["pg_catalog", "information_schema", "pg_toast"]
    }

    fn list_sql(&self, kind: ObjectKind, schema: Option<&str>) -> String {
        // Postgres query output uses the same column aliases as the MySQL
        // plane so `ObjectRow` parsing remains dialect-agnostic:
        //   column 0 -> schema, column 1 -> name, rest -> extras in order.
        let esc = |s: &str| s.replace('\'', "''");
        let where_eq = |col: &str| match schema {
            Some(s) => format!("WHERE {} = '{}'", col, esc(s)),
            None    => "WHERE 1=1".to_string(),
        };
        match kind {
            ObjectKind::Tables => format!(
                // Parent_schema / Parent_name are populated for partition
                // children only (relispartition = true) and used by the outer
                // list to nest partitions under their parent. Non-partition
                // rows get empty strings; the parser converts those to None.
                "SELECT t.table_schema, t.table_name, \
                        'heap' AS engine, \
                        COALESCE(c.reltuples::bigint::text, '') AS row_estimate, \
                        '' AS updated, \
                        COALESCE(obj_description(c.oid, 'pg_class'), '') AS comment, \
                        COALESCE(pn.nspname, '') AS \"Parent_schema\", \
                        COALESCE(pc.relname, '') AS \"Parent_name\" \
                 FROM information_schema.tables t \
                 JOIN pg_namespace n ON n.nspname = t.table_schema \
                 JOIN pg_class c ON c.relname = t.table_name AND c.relnamespace = n.oid \
                 LEFT JOIN pg_inherits inh ON inh.inhrelid = c.oid AND c.relispartition \
                 LEFT JOIN pg_class    pc  ON pc.oid = inh.inhparent \
                 LEFT JOIN pg_namespace pn ON pn.oid = pc.relnamespace \
                 {} AND t.table_type = 'BASE TABLE' \
                 ORDER BY t.table_schema, t.table_name",
                where_eq("t.table_schema"),
            ),
            ObjectKind::Views => format!(
                "SELECT table_schema, table_name, \
                        '' AS definer, \
                        is_updatable, \
                        '' AS security \
                 FROM information_schema.views \
                 {} \
                 ORDER BY table_schema, table_name",
                where_eq("table_schema"),
            ),
            ObjectKind::Procedures => format!(
                "SELECT routine_schema, routine_name, \
                        '' AS definer, \
                        '' AS created, \
                        '' AS modified \
                 FROM information_schema.routines \
                 {} AND routine_type = 'PROCEDURE' \
                 ORDER BY routine_schema, routine_name",
                where_eq("routine_schema"),
            ),
            ObjectKind::Functions => format!(
                "SELECT routine_schema, routine_name, \
                        '' AS definer, \
                        COALESCE(data_type, '') AS returns, \
                        '' AS created \
                 FROM information_schema.routines \
                 {} AND routine_type = 'FUNCTION' \
                 ORDER BY routine_schema, routine_name",
                where_eq("routine_schema"),
            ),
            ObjectKind::Triggers => format!(
                "SELECT DISTINCT trigger_schema, trigger_name, \
                        event_object_table AS tbl, \
                        event_manipulation AS event, \
                        action_timing AS timing, \
                        '' AS definer \
                 FROM information_schema.triggers \
                 {} \
                 ORDER BY trigger_schema, trigger_name",
                where_eq("trigger_schema"),
            ),
            // Should be unreachable — object_kinds() drops Events for Postgres.
            ObjectKind::Events => "SELECT NULL::text, NULL::text WHERE 1=0".into(),
        }
    }

    fn table_status_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // The `col_stats` CTE drives the Storage_factor column. It sums
        // per-column avg_width × (1-null_frac) from pg_stats (populated
        // by ANALYZE) and counts nullable columns for the null-bitmap
        // budget. The expected page count comes from a simplified
        // version of the canonical PG-wiki bloat estimate
        // (https://wiki.postgresql.org/wiki/Show_database_bloat):
        // CEIL(reltuples × (datawidth + 28) / 8168) where 28 ≈ tuple
        // header (24) + null-bitmap padding budget (4) and 8168 = 8192
        // (page size) − 24 (page header). Compare to c.relpages.
        //
        // Empty / never-analyzed / partitioned-parent rows return ''
        // and the renderer drops the cell — see TableOverview.
        format!(
            "WITH col_stats AS ( \
                SELECT \
                    SUM((1 - null_frac) * avg_width) AS datawidth, \
                    COUNT(*) FILTER (WHERE null_frac > 0) AS nullable_cnt \
                FROM pg_stats \
                WHERE schemaname = '{0}' AND tablename = '{1}' \
             ) \
             SELECT \
                COALESCE(am.amname, 'heap')               AS \"Engine\", \
                ''                                        AS \"Collation\", \
                c.reltuples::bigint                       AS \"Rows\", \
                (CASE WHEN c.relkind = 'p' \
                     THEN COALESCE((SELECT SUM(pg_relation_size(inhrelid)) FROM pg_inherits WHERE inhparent = c.oid), 0) \
                     ELSE pg_relation_size(c.oid) \
                END)::bigint                              AS \"Data_length\", \
                (CASE WHEN c.relkind = 'p' \
                     THEN COALESCE((SELECT SUM(pg_indexes_size(inhrelid)) FROM pg_inherits WHERE inhparent = c.oid), 0) \
                     ELSE pg_indexes_size(c.oid) \
                END)::bigint                              AS \"Index_length\", \
                (CASE WHEN c.reltuples > 0 \
                     THEN (CASE WHEN c.relkind = 'p' \
                           THEN COALESCE((SELECT SUM(pg_relation_size(inhrelid)) FROM pg_inherits WHERE inhparent = c.oid), 0) \
                           ELSE pg_relation_size(c.oid) END \
                          / c.reltuples::bigint) \
                     ELSE 0 \
                END)::bigint                              AS \"Avg_row_length\", \
                ''                                        AS \"Auto_increment\", \
                ''                                        AS \"Create_time\", \
                ''                                        AS \"Update_time\", \
                COALESCE(obj_description(c.oid, 'pg_class'), '') AS \"Comment\", \
                (SELECT COUNT(*) FROM pg_inherits i \
                    JOIN pg_class ch ON ch.oid = i.inhrelid \
                    WHERE i.inhparent = c.oid AND ch.relispartition)::bigint AS \"Partition_count\", \
                COALESCE(pc.relname, '') AS \"Parent_table\", \
                COALESCE(pn.nspname, '') AS \"Parent_schema\", \
                COALESCE(pg_get_expr(c.relpartbound, c.oid), '') AS \"Partition_bound\", \
                pg_get_userbyid(c.relowner)               AS \"Owner\", \
                /* On a partitioned parent (relkind = 'p') the parent's \
                   own relpages / reltuples are 0 — the parent owns no \
                   data. Roll up Page_count and Page_count_ideal by \
                   summing across leaf partitions via pg_partition_tree \
                   (PG 10+). Storage_factor and Storage_actual_bytes \
                   fall out of the same rollup as ratio-of-sums / sum. \
                   Ideal per-partition needs each partition's own \
                   datawidth via a correlated pg_stats subquery. \
                   Suppression semantics preserved: if the parent has \
                   no leaf partitions (rare — an empty partitioned \
                   root before any partitions attach), the sum returns \
                   0 and the existing WHEN relpages <= 0 arm kicks in. */ \
                CASE \
                    WHEN c.relkind = 'p' THEN ( \
                        SELECT \
                            CASE \
                                WHEN SUM(pt.pages) = 0 THEN '1.00' \
                                WHEN SUM(pt.ideal) = 0 THEN '' \
                                ELSE TO_CHAR( \
                                    SUM(pt.pages)::numeric \
                                        / SUM(pt.ideal)::numeric, \
                                    'FM999990.00' \
                                ) \
                            END \
                        FROM ( \
                            SELECT \
                                pc.relpages::bigint AS pages, \
                                CASE \
                                    WHEN pc.reltuples <= 0 THEN 0 \
                                    ELSE GREATEST( \
                                        CEIL( \
                                            pc.reltuples::numeric \
                                            * (COALESCE(( \
                                                SELECT SUM(s2.avg_width * (1 - s2.null_frac)) \
                                                FROM pg_stats s2 \
                                                WHERE s2.schemaname = pn2.nspname \
                                                  AND s2.tablename = pc.relname \
                                            ), 0) + 28) / 8168 \
                                        )::bigint, \
                                        1::bigint \
                                    ) \
                                END AS ideal \
                            FROM pg_partition_tree(c.oid) pt2 \
                            JOIN pg_class pc ON pc.oid = pt2.relid \
                            JOIN pg_namespace pn2 ON pn2.oid = pc.relnamespace \
                            WHERE pt2.isleaf \
                        ) pt \
                    ) \
                    /* reltuples = -1 means pg_class hasn't been updated \
                       since a bulk load (e.g. COPY, INSERT … SELECT into \
                       an empty table). pg_stats may still hold a prior \
                       ANALYZE's column widths but reltuples is stale, \
                       so the ELSE division would produce a wildly \
                       inflated factor. Suppress until the next ANALYZE. */ \
                    WHEN c.reltuples < 0 THEN '' \
                    WHEN c.reltuples <= 0 AND c.relpages <= 1 THEN '1.00' \
                    WHEN c.relpages <= 0 THEN '1.00' \
                    WHEN cs.datawidth IS NULL OR cs.datawidth = 0 THEN '' \
                    ELSE TO_CHAR( \
                        c.relpages::numeric / GREATEST( \
                            CEIL( \
                                c.reltuples::numeric * (cs.datawidth + 28) / 8168 \
                            ), \
                            1::numeric \
                        ), \
                        'FM999990.00' \
                    ) \
                END AS \"Storage_factor\", \
                CASE \
                    WHEN c.relkind = 'p' THEN ( \
                        SELECT COALESCE(SUM(pc.relpages)::bigint, 0) \
                                * current_setting('block_size')::bigint \
                        FROM pg_partition_tree(c.oid) pt2 \
                        JOIN pg_class pc ON pc.oid = pt2.relid \
                        WHERE pt2.isleaf \
                    ) \
                    ELSE c.relpages::bigint * current_setting('block_size')::bigint \
                END                                       AS \"Storage_actual_bytes\", \
                CASE \
                    WHEN c.relkind = 'p' THEN ( \
                        SELECT COALESCE(SUM(pc.relpages)::text, '0') \
                        FROM pg_partition_tree(c.oid) pt2 \
                        JOIN pg_class pc ON pc.oid = pt2.relid \
                        WHERE pt2.isleaf \
                    ) \
                    ELSE c.relpages::text \
                END                                       AS \"Page_count\", \
                CASE \
                    WHEN c.relkind = 'p' THEN ( \
                        SELECT COALESCE(SUM( \
                            CASE \
                                WHEN pc.reltuples <= 0 THEN 0 \
                                ELSE GREATEST( \
                                    CEIL( \
                                        pc.reltuples::numeric \
                                        * (COALESCE(( \
                                            SELECT SUM(s2.avg_width * (1 - s2.null_frac)) \
                                            FROM pg_stats s2 \
                                            WHERE s2.schemaname = pn2.nspname \
                                              AND s2.tablename = pc.relname \
                                        ), 0) + 28) / 8168 \
                                    )::bigint, \
                                    1::bigint \
                                ) \
                            END \
                        )::text, '') \
                        FROM pg_partition_tree(c.oid) pt2 \
                        JOIN pg_class pc ON pc.oid = pt2.relid \
                        JOIN pg_namespace pn2 ON pn2.oid = pc.relnamespace \
                        WHERE pt2.isleaf \
                    ) \
                    WHEN c.reltuples < 0 THEN '' \
                    WHEN c.reltuples <= 0 THEN '' \
                    WHEN cs.datawidth IS NULL OR cs.datawidth = 0 THEN '' \
                    ELSE GREATEST( \
                        CEIL( \
                            c.reltuples::numeric * (cs.datawidth + 28) / 8168 \
                        )::bigint, \
                        1::bigint \
                    )::text \
                END                                       AS \"Page_count_ideal\", \
                COALESCE( \
                    to_char(GREATEST(s.last_vacuum, s.last_autovacuum), 'YYYY-MM-DD HH24:MI'), \
                    '' \
                ) AS \"Last_vacuum\", \
                /* Cache hit ratio: heap_blks_hit / (heap_blks_hit + \
                   heap_blks_read) from pg_statio_user_tables. \
                   Returns '' when no blocks accessed yet (fresh table); \
                   else a one-decimal percentage like '99.7'. The raw \
                   hit/read counts ride along so main.rs can build a \
                   tooltip without a second query. */ \
                CASE \
                    WHEN COALESCE(io.heap_blks_hit + io.heap_blks_read, 0) <= 0 THEN '' \
                    ELSE TO_CHAR( \
                        100.0 * io.heap_blks_hit::numeric \
                            / (io.heap_blks_hit + io.heap_blks_read), \
                        'FM990.0' \
                    ) \
                END                                       AS \"Cache_hit_ratio\", \
                COALESCE(io.heap_blks_hit::text,  '0')    AS \"Heap_blks_hit\", \
                COALESCE(io.heap_blks_read::text, '0')    AS \"Heap_blks_read\", \
                /* Index block IO — same source (pg_statio_user_tables) \
                   as the heap counters. Parser combines heap + idx into \
                   a single Block IO cell displaying scale (magnitudes) \
                   alongside the Cache hit ratio which shows efficiency. \
                   Tooltip breaks out heap vs index for follow-up. */ \
                COALESCE(io.idx_blks_hit::text,   '0')    AS \"Idx_blks_hit\", \
                COALESCE(io.idx_blks_read::text,  '0')    AS \"Idx_blks_read\", \
                /* Last analyze — greatest of manual + auto ANALYZE run \
                   times from pg_stat_user_tables. Empty when neither \
                   has ever run (fresh table). Mirrors Last_vacuum. */ \
                COALESCE( \
                    to_char(GREATEST(s.last_analyze, s.last_autoanalyze), 'YYYY-MM-DD HH24:MI'), \
                    '' \
                )                                          AS \"Last_analyze\", \
                /* Idx_scan_pct — percentage of index vs sequential \
                   scans since last stats reset. NULL when both are 0 \
                   (never accessed) — the renderer skips the cell. \
                   Raw idx / seq counts ride along for the tooltip. */ \
                CASE \
                    WHEN COALESCE(s.idx_scan, 0) + COALESCE(s.seq_scan, 0) = 0 THEN '' \
                    ELSE TO_CHAR( \
                        100.0 * COALESCE(s.idx_scan, 0)::numeric \
                            / (COALESCE(s.idx_scan, 0) + COALESCE(s.seq_scan, 0)), \
                        'FM990.0' \
                    ) \
                END                                        AS \"Idx_scan_pct\", \
                COALESCE(s.idx_scan::text, '0')            AS \"Idx_scan\", \
                COALESCE(s.seq_scan::text, '0')            AS \"Seq_scan\", \
                /* CRUD counters — four separate lifetime tuple counts \
                   from pg_stat_user_tables. The parser stitches them \
                   into a single C / R / U / D display string. \
                   \
                   Reads (R) is seq_tup_read + idx_tup_fetch — tuples \
                   returned to callers across sequential and index \
                   access paths. NOT query count — a SELECT * FROM foo \
                   returns N tuples in one call. Tuple volume is what \
                   the DBA actually cares about for how-busy. \
                   \
                   All four grow monotonically until pg_stat_reset(). \
                   No severity band — volume is neutral. */ \
                COALESCE(s.n_tup_ins::text, '0')           AS \"Crud_c\", \
                (COALESCE(s.seq_tup_read, 0) + COALESCE(s.idx_tup_fetch, 0))::text \
                                                           AS \"Crud_r\", \
                COALESCE(s.n_tup_upd::text, '0')           AS \"Crud_u\", \
                COALESCE(s.n_tup_del::text, '0')           AS \"Crud_d\", \
                /* Vacuum-health signals — forward-looking companions \
                   to Last vacuum / Last analyze. Dead_tup_pct is dead \
                   tuples as a fraction of live + dead; PG's default \
                   autovacuum_vacuum_scale_factor is 0.2 so values \
                   approaching 20% mean vacuum is about to fire, and \
                   higher values mean it's falling behind. Empty when \
                   the table has no rows (nothing to be dead about). \
                   Raw counts ride along for the tooltip. */ \
                CASE \
                    WHEN COALESCE(s.n_live_tup, 0) + COALESCE(s.n_dead_tup, 0) = 0 THEN '' \
                    ELSE TO_CHAR( \
                        100.0 * COALESCE(s.n_dead_tup, 0)::numeric \
                            / (COALESCE(s.n_live_tup, 0) + COALESCE(s.n_dead_tup, 0)), \
                        'FM990.0' \
                    ) \
                END                                        AS \"Dead_tup_pct\", \
                COALESCE(s.n_live_tup::text, '0')          AS \"N_live_tup\", \
                COALESCE(s.n_dead_tup::text, '0')          AS \"N_dead_tup\", \
                /* TOAST size + percentage-of-row-storage. \
                   pg_total_relation_size(reltoastrelid) covers both \
                   the TOAST heap and its own index. reltoastrelid = 0 \
                   when the table has no TOAST relation (small tables, \
                   partitioned parents, tables with no toastable \
                   columns). Empty string in either case so the meta- \
                   grid's push-if-non-empty guard hides the cell. \
                   \
                   On partitioned parents (relkind = 'p'), the parent's \
                   own reltoastrelid is 0 so a plain scalar lookup \
                   returns empty. Roll up via pg_partition_tree — sum \
                   TOAST across leaf partitions — so a partitioned \
                   table with TOAST'd data on its children shows a \
                   real value on the parent instead of hiding entirely. \
                   Matches the same rollup treatment Page_count / \
                   Storage_factor received in PR #94. */ \
                CASE \
                    WHEN c.relkind = 'p' THEN ( \
                        SELECT \
                            CASE \
                                WHEN COALESCE(SUM(pg_total_relation_size(pc.reltoastrelid)), 0) = 0 THEN '' \
                                ELSE SUM(pg_total_relation_size(pc.reltoastrelid))::text \
                            END \
                        FROM pg_partition_tree(c.oid) pt2 \
                        JOIN pg_class pc ON pc.oid = pt2.relid \
                        WHERE pt2.isleaf \
                    ) \
                    WHEN c.reltoastrelid = 0 THEN '' \
                    WHEN pg_total_relation_size(c.reltoastrelid) = 0 THEN '' \
                    ELSE pg_total_relation_size(c.reltoastrelid)::text \
                END                                        AS \"Toast_size\", \
                /* Percentage denominator = heap + TOAST. Answers `how \
                   much of the table's row-storage lives out of line`. \
                   Indexes excluded so a big-index table doesn't skew \
                   the TOAST fraction low. NULLIF guards a zero-total \
                   edge case even though we've already checked for it \
                   above. On partitioned parents the sum-of-sums via \
                   pg_partition_tree gives the whole-table percentage. */ \
                CASE \
                    WHEN c.relkind = 'p' THEN ( \
                        SELECT \
                            CASE \
                                WHEN COALESCE(SUM(pg_total_relation_size(pc.reltoastrelid)), 0) = 0 THEN '' \
                                ELSE TO_CHAR( \
                                    100.0 * SUM(pg_total_relation_size(pc.reltoastrelid))::numeric \
                                        / NULLIF( \
                                            SUM(pg_relation_size(pc.oid)) \
                                            + SUM(pg_total_relation_size(pc.reltoastrelid)), \
                                            0), \
                                    'FM990.0' \
                                ) \
                            END \
                        FROM pg_partition_tree(c.oid) pt2 \
                        JOIN pg_class pc ON pc.oid = pt2.relid \
                        WHERE pt2.isleaf \
                    ) \
                    WHEN c.reltoastrelid = 0 THEN '' \
                    WHEN pg_total_relation_size(c.reltoastrelid) = 0 THEN '' \
                    ELSE TO_CHAR( \
                        100.0 * pg_total_relation_size(c.reltoastrelid)::numeric \
                            / NULLIF( \
                                pg_relation_size(c.oid) \
                                + pg_total_relation_size(c.reltoastrelid), \
                                0), \
                        'FM990.0' \
                    ) \
                END                                        AS \"Toast_pct\", \
                /* Freeze age % = age(relfrozenxid) * 100 / \
                   effective_autovacuum_freeze_max_age. Answers how \
                   close this table is to triggering an anti-wraparound \
                   vacuum. Relation-level only — no page scans, no VM \
                   probing — safe on TB-sized tables. \
                   \
                   Autovacuum honors a per-table reloption only when it \
                   is smaller than the cluster GUC (a larger one is \
                   ignored), so the ceiling is LEAST(reloption, GUC). \
                   `pg_options_to_table(c.reloptions)` returns zero rows \
                   when no reloption is set; LEAST skips the NULL and \
                   falls through to `current_setting()`. Reloptions do \
                   NOT inherit across partition boundaries — each leaf \
                   applies its own — so the leaf's own reloptions are \
                   checked inside the pg_partition_tree walk. \
                   \
                   Empty result on temp / unlogged / brand-new tables \
                   where relfrozenxid = 0. On partitioned parents \
                   (relkind = 'p') roll up as MAX across leaves — \
                   worst leaf drives urgency. The outer COALESCE(..,'') \
                   normalizes SQL NULL (all leaves empty, or zero \
                   leaves) to empty string, matching every other \
                   rollup in this query. */ \
                CASE \
                    WHEN c.relkind = 'p' THEN COALESCE(( \
                        SELECT MAX( \
                            CASE \
                                WHEN pc.relfrozenxid = '0'::xid THEN NULL \
                                ELSE ROUND( \
                                    age(pc.relfrozenxid) * 100.0 \
                                    / LEAST( \
                                        (SELECT option_value::numeric \
                                         FROM pg_options_to_table(pc.reloptions) \
                                         WHERE option_name = 'autovacuum_freeze_max_age'), \
                                        current_setting('autovacuum_freeze_max_age')::numeric \
                                    ), \
                                    0 \
                                )::bigint \
                            END \
                        )::text \
                        FROM pg_partition_tree(c.oid) pt2 \
                        JOIN pg_class pc ON pc.oid = pt2.relid \
                        WHERE pt2.isleaf \
                    ), '') \
                    WHEN c.relfrozenxid = '0'::xid THEN '' \
                    ELSE ROUND( \
                        age(c.relfrozenxid) * 100.0 \
                        / LEAST( \
                            (SELECT option_value::numeric \
                             FROM pg_options_to_table(c.reloptions) \
                             WHERE option_name = 'autovacuum_freeze_max_age'), \
                            current_setting('autovacuum_freeze_max_age')::numeric \
                        ), \
                        0 \
                    )::bigint::text \
                END                                        AS \"Freeze_age_pct\", \
                /* Raw age(relfrozenxid) magnitude for the tooltip. \
                   Rolled up as MAX on partitioned parents to match \
                   the percentage. Outer COALESCE(..,'') normalizes \
                   SQL NULL from the aggregate to empty string. */ \
                CASE \
                    WHEN c.relkind = 'p' THEN COALESCE(( \
                        SELECT MAX( \
                            CASE WHEN pc.relfrozenxid = '0'::xid THEN NULL \
                                 ELSE age(pc.relfrozenxid)::bigint END \
                        )::text \
                        FROM pg_partition_tree(c.oid) pt2 \
                        JOIN pg_class pc ON pc.oid = pt2.relid \
                        WHERE pt2.isleaf \
                    ), '') \
                    WHEN c.relfrozenxid = '0'::xid THEN '' \
                    ELSE age(c.relfrozenxid)::bigint::text \
                END                                        AS \"Freeze_age_raw\" \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_am am ON am.oid = c.relam \
             LEFT JOIN pg_inherits inh ON inh.inhrelid = c.oid AND c.relispartition \
             LEFT JOIN pg_class    pc ON pc.oid = inh.inhparent \
             LEFT JOIN pg_namespace pn ON pn.oid = pc.relnamespace \
             LEFT JOIN pg_stat_user_tables s ON s.relid = c.oid \
             LEFT JOIN pg_statio_user_tables io ON io.relid = c.oid \
             LEFT JOIN col_stats cs ON true \
             WHERE n.nspname = '{0}' AND c.relname = '{1}'",
            esc(schema), esc(name),
        )
    }

    fn table_columns_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // Aliases match what the existing `columns:` handler looks up
        // (Field / Type / Null / Default / Extra / Comment).
        format!(
            "SELECT \
                column_name AS \"Field\", \
                CASE \
                    WHEN character_maximum_length IS NOT NULL \
                        THEN data_type || '(' || character_maximum_length || ')' \
                    WHEN numeric_precision IS NOT NULL AND numeric_scale IS NOT NULL \
                        THEN data_type || '(' || numeric_precision || ',' || numeric_scale || ')' \
                    WHEN datetime_precision IS NOT NULL \
                        THEN data_type || '(' || datetime_precision || ')' \
                    ELSE data_type \
                END AS \"Type\", \
                is_nullable AS \"Null\", \
                COALESCE(column_default, '') AS \"Default\", \
                '' AS \"Extra\", \
                COALESCE( \
                    col_description( \
                        (quote_ident(table_schema) || '.' || quote_ident(table_name))::regclass, \
                        ordinal_position \
                    ), '' \
                ) AS \"Comment\" \
             FROM information_schema.columns \
             WHERE table_schema = '{}' AND table_name = '{}' \
             ORDER BY ordinal_position",
            esc(schema), esc(name),
        )
    }

    fn view_columns_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        format!(
            "SELECT \
                c.column_name AS \"Field\", \
                CASE \
                    WHEN c.character_maximum_length IS NOT NULL \
                        THEN c.data_type || '(' || c.character_maximum_length || ')' \
                    WHEN c.numeric_precision IS NOT NULL AND c.numeric_scale IS NOT NULL \
                        THEN c.data_type || '(' || c.numeric_precision || ',' || c.numeric_scale || ')' \
                    WHEN c.datetime_precision IS NOT NULL \
                        THEN c.data_type || '(' || c.datetime_precision || ')' \
                    ELSE c.data_type \
                END AS \"Type\", \
                c.is_nullable AS \"Null\", \
                COALESCE(c.column_default, '') AS \"Default\", \
                '' AS \"Extra\", \
                '' AS \"Comment\", \
                COALESCE(vcu.table_name || '.' || vcu.column_name, '') AS \"Source\" \
             FROM information_schema.columns c \
             LEFT JOIN LATERAL ( \
                 SELECT table_name, column_name \
                 FROM information_schema.view_column_usage v \
                 WHERE v.view_schema = c.table_schema \
                   AND v.view_name   = c.table_name \
                   AND v.column_name = c.column_name \
                 LIMIT 1 \
             ) vcu ON true \
             WHERE c.table_schema = '{}' AND c.table_name = '{}' \
             ORDER BY c.ordinal_position",
            esc(schema), esc(name),
        )
    }

    fn table_indexes_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // Produce one row per (index, column-position) with column aliases
        // matching MySQL's SHOW INDEX: Key_name / Non_unique / Index_type /
        // Column_name / Seq_in_index. The existing handler groups by Key_name.
        // indisprimary → emits name "PRIMARY" so the (pk) tag logic lights up.
        //
        // `Inherited` is PG-specific: when an index is created on a
        // partitioned parent, PG creates child index objects on each
        // partition and records the link in pg_inherits (inhrelid =
        // partition's index oid, inhparent = parent partitioned-index oid).
        // The renderer appends " (Inherited)" to the Kind column so a DBA
        // can distinguish parent-driven indexes from partition-local ones.
        //
        // The `ix_widths` CTE sums avg_width across the indexed columns
        // (from pg_stats — same source the table factor uses). The btree
        // leaf-page count is CEIL(reltuples × (key_width + 18) / 8152),
        // where 18 = IndexTuple header (8) + ItemPointerData (6) + LP_ITEMID
        // (4), and 8152 = 8192 (page) − 24 (page header) − 16 (btree
        // BTPageOpaqueData special area).
        //
        // Total ideal pages = leaf_pages + meta(1) + internal_root(1
        // when leaf_pages > 1). Every btree has a meta page on disk;
        // any btree with 2+ leaves needs at least one internal root.
        // Without these two pages, small indexes like a 3-leaf integer
        // PK on a 1K-row table got a false 1.7× factor (actual 5 pages,
        // leaf-only ideal 3). We don't model deeper internal levels —
        // for very large indexes they're ≤ 0.5% of total pages, well
        // within the heuristic's noise floor.
        //
        // factor = i.relpages / ideal_total_pages, clamped to 1.0× as a
        // floor — the heuristic is still approximate (btree dedup on
        // PG 13+ can compress duplicate keys, alignment varies by
        // column type) and anything below 1.0× means "more packed than
        // the estimate predicted," which we report as healthy 1.0×.
        // Non-btree index types (GIN, GiST, BRIN, hash, SP-GiST) have
        // different storage shapes and emit '' for the factor — same
        // v1 trade-off as the table factor for non-InnoDB MySQL engines.
        format!(
            "WITH ix_widths AS ( \
                SELECT \
                    ix2.indexrelid, \
                    SUM(s.avg_width) AS key_width \
                FROM pg_index ix2 \
                JOIN pg_class t2 ON t2.oid = ix2.indrelid \
                JOIN pg_namespace n2 ON n2.oid = t2.relnamespace \
                JOIN LATERAL unnest(ix2.indkey) WITH ORDINALITY AS kp(attnum, ord) ON TRUE \
                JOIN pg_attribute a2 ON a2.attrelid = t2.oid AND a2.attnum = kp.attnum \
                LEFT JOIN pg_stats s ON s.schemaname = n2.nspname \
                                    AND s.tablename  = t2.relname \
                                    AND s.attname    = a2.attname \
                WHERE n2.nspname = '{0}' AND t2.relname = '{1}' \
                GROUP BY ix2.indexrelid \
             ), \
             ix_ideal AS ( \
                /* leaf_pages = entries / (page_avail / per_entry). \
                   total_pages = leaf + meta(1) + internal_root(1 when \
                   leaf > 1). Both Storage_factor and Index_size_ideal \
                   reference total_pages so they stay in sync. */ \
                SELECT \
                    i.oid AS indexrelid, \
                    CEIL(GREATEST(i.reltuples, 0)::numeric \
                         * (COALESCE(widths.key_width, 0) + 18) / 8152) AS leaf_pages \
                FROM pg_class i \
                LEFT JOIN ix_widths widths ON widths.indexrelid = i.oid \
             ), \
             ix_total AS ( \
                SELECT \
                    indexrelid, \
                    leaf_pages, \
                    (leaf_pages + 1 \
                     + CASE WHEN leaf_pages > 1 THEN 1 ELSE 0 END) AS total_pages \
                FROM ix_ideal \
             ) \
             SELECT \
                CASE WHEN ix.indisprimary THEN 'PRIMARY' ELSE i.relname END AS \"Key_name\", \
                CASE WHEN ix.indisunique THEN '0' ELSE '1' END            AS \"Non_unique\", \
                UPPER(am.amname)                                          AS \"Index_type\", \
                pg_relation_size(i.oid)::bigint                           AS \"Index_size\", \
                a.attname                                                 AS \"Column_name\", \
                kpos.ord                                                  AS \"Seq_in_index\", \
                EXISTS (SELECT 1 FROM pg_inherits WHERE inhrelid = i.oid)::int AS \"Inherited\", \
                CASE \
                    WHEN UPPER(am.amname) <> 'BTREE' THEN '' \
                    WHEN i.reltuples < 0 THEN '' \
                    WHEN i.reltuples <= 0 AND i.relpages <= 1 THEN '1.00' \
                    WHEN i.relpages <= 0 THEN '1.00' \
                    WHEN widths.key_width IS NULL OR widths.key_width <= 0 THEN '' \
                    ELSE TO_CHAR( \
                        GREATEST( \
                            i.relpages::numeric / GREATEST(ix_total.total_pages, 1::numeric), \
                            1::numeric \
                        ), \
                        'FM999990.00' \
                    ) \
                END                                                       AS \"Storage_factor\", \
                (i.relpages::bigint * current_setting('block_size')::bigint) AS \"Storage_actual_bytes\", \
                /* Index_size_ideal: estimated bytes the index WOULD use \
                   if perfectly packed. Uses the SAME total_pages from \
                   ix_total so the Size / Ideal pair stays consistent \
                   with the Storage factor cell. */ \
                CASE \
                    WHEN UPPER(am.amname) <> 'BTREE' THEN '' \
                    WHEN i.reltuples < 0 THEN '' \
                    WHEN i.reltuples <= 0 AND i.relpages <= 1 THEN '' \
                    WHEN i.relpages <= 0 THEN '' \
                    WHEN widths.key_width IS NULL OR widths.key_width <= 0 THEN '' \
                    ELSE ( \
                        GREATEST(ix_total.total_pages, 1::numeric) \
                        * current_setting('block_size')::numeric \
                    )::bigint::text \
                END                                                       AS \"Index_size_ideal\", \
                /* Scan_count: number of times the index has been used \
                   since the last stat reset. 0 = drop candidate (the \
                   Indexes-tab renderer colours it red). NULL collapses \
                   to 0 — pg_stat_user_indexes only lists indexes on \
                   user tables, but the join is left-outer to be safe. */ \
                COALESCE(s.idx_scan, 0)::text                             AS \"Scan_count\", \
                /* Tup_per_scan: average entries returned per scan \
                   (idx_tup_read / idx_scan). Empty when scan count is \
                   zero (avg undefined). Diagnostic — high values can \
                   mean range scans or low selectivity. Shown inline \
                   in the Uses cell as \"1,234 (2.3 avg)\". */ \
                CASE \
                    WHEN COALESCE(s.idx_scan, 0) <= 0 THEN '' \
                    ELSE TO_CHAR( \
                        s.idx_tup_read::numeric / s.idx_scan, \
                        'FM999990.0' \
                    ) \
                END                                                       AS \"Tup_per_scan\" \
             FROM pg_index ix \
             JOIN pg_class i  ON i.oid = ix.indexrelid \
             JOIN pg_class t  ON t.oid = ix.indrelid \
             JOIN pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_am am ON am.oid = i.relam \
             JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS kpos(attnum, ord) ON TRUE \
             JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = kpos.attnum \
             LEFT JOIN ix_widths widths ON widths.indexrelid = i.oid \
             LEFT JOIN ix_total ON ix_total.indexrelid = i.oid \
             LEFT JOIN pg_stat_user_indexes s ON s.indexrelid = i.oid \
             WHERE n.nspname = '{0}' AND t.relname = '{1}' \
             ORDER BY (CASE WHEN ix.indisprimary THEN 0 ELSE 1 END), i.relname, kpos.ord",
            esc(schema), esc(name),
        )
    }

    fn table_ddl_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // Reconstructs CREATE TABLE from pg_attribute + pg_constraint +
        // pg_index. Partition-aware: the `parts` CTE detects partitioned
        // parents (relkind = 'p') and partitions (relispartition); the
        // final SELECT branches into one of three forms:
        //
        //   * Partition          → CREATE TABLE x PARTITION OF parent <bound>;
        //   * Partitioned parent → CREATE TABLE x (cols, cons) PARTITION BY <key>;
        //   * Plain table        → CREATE TABLE x (cols, cons);
        //
        // A table that is BOTH a partition AND a partitioned parent (sub-
        // partitioning) gets the PARTITION OF form *plus* a trailing
        // PARTITION BY <key>. Local indexes append in all cases.
        format!(
            "WITH \
             tbl AS ( \
                 SELECT c.oid, c.relname, n.nspname, c.relpersistence, c.relkind \
                 FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = '{}' AND c.relname = '{}' \
             ), \
             parts AS ( \
                 SELECT t.relispartition                       AS is_partition, \
                        t.relkind = 'p'                        AS is_partitioned_parent, \
                        pg_get_partkeydef(t.oid)               AS partkey, \
                        pg_get_expr(t.relpartbound, t.oid)     AS bound, \
                        parent_n.nspname                       AS parent_schema, \
                        parent_c.relname                       AS parent_name \
                 FROM pg_class t \
                 LEFT JOIN pg_inherits inh \
                        ON inh.inhrelid = t.oid AND t.relispartition \
                 LEFT JOIN pg_class    parent_c ON parent_c.oid = inh.inhparent \
                 LEFT JOIN pg_namespace parent_n ON parent_n.oid = parent_c.relnamespace \
                 WHERE t.oid = (SELECT oid FROM tbl) \
             ), \
             cols AS ( \
                 SELECT a.attname, \
                        pg_catalog.format_type(a.atttypid, a.atttypmod) AS col_type, \
                        CASE WHEN a.attnotnull THEN ' NOT NULL' ELSE '' END AS nn, \
                        CASE WHEN a.atthasdef \
                             THEN ' DEFAULT ' || pg_catalog.pg_get_expr(d.adbin, d.adrelid) \
                             ELSE '' END AS col_default, \
                        CASE WHEN a.attidentity = 'a' THEN ' GENERATED ALWAYS AS IDENTITY' \
                             WHEN a.attidentity = 'd' THEN ' GENERATED BY DEFAULT AS IDENTITY' \
                             ELSE '' END AS identity, \
                        CASE WHEN a.attgenerated = 's' \
                             THEN ' GENERATED ALWAYS AS (' || pg_catalog.pg_get_expr(d.adbin, d.adrelid) || ') STORED' \
                             ELSE '' END AS generated, \
                        a.attnum \
                 FROM pg_attribute a \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                 WHERE a.attrelid = (SELECT oid FROM tbl) \
                   AND a.attnum > 0 AND NOT a.attisdropped \
                 ORDER BY a.attnum \
             ), \
             cons AS ( \
                 SELECT '    CONSTRAINT ' || quote_ident(c.conname) || ' ' || \
                        pg_catalog.pg_get_constraintdef(c.oid, true) AS def, \
                        CASE c.contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 \
                             WHEN 'f' THEN 2 WHEN 'c' THEN 3 ELSE 4 END AS sort_order, \
                        c.conname \
                 FROM pg_constraint c \
                 WHERE c.conrelid = (SELECT oid FROM tbl) \
                   AND c.contype <> 'n' \
                 ORDER BY sort_order, c.conname \
             ), \
             idxs AS ( \
                 SELECT pg_catalog.pg_get_indexdef(i.indexrelid) || ';' AS def \
                 FROM pg_index i \
                 WHERE i.indrelid = (SELECT oid FROM tbl) \
                   AND NOT i.indisprimary \
                   AND NOT EXISTS ( \
                       SELECT 1 FROM pg_constraint \
                       WHERE conindid = i.indexrelid AND contype IN ('p','u') \
                   ) \
                 ORDER BY (SELECT relname FROM pg_class WHERE oid = i.indexrelid) \
             ), \
             col_lines AS ( \
                 SELECT string_agg( \
                     ('    ' || quote_ident(attname) || ' ' || col_type || \
                      CASE WHEN generated <> '' THEN generated \
                           WHEN identity <> '' THEN identity \
                           ELSE col_default END || \
                      nn), \
                     ',' || chr(10) ORDER BY attnum \
                 ) AS body FROM cols \
             ), \
             con_lines AS ( \
                 SELECT string_agg(def, ',' || chr(10) ORDER BY sort_order, conname) AS body FROM cons \
             ), \
             idx_lines AS ( \
                 SELECT string_agg(def, chr(10)) AS body FROM idxs \
             ) \
             SELECT (SELECT relname FROM tbl) AS \"Table\", \
                    CASE WHEN (SELECT relpersistence FROM tbl) = 'u' THEN 'CREATE UNLOGGED TABLE ' \
                         WHEN (SELECT relpersistence FROM tbl) = 't' THEN 'CREATE TEMPORARY TABLE ' \
                         ELSE 'CREATE TABLE ' END || \
                    quote_ident((SELECT nspname FROM tbl)) || '.' || \
                    quote_ident((SELECT relname FROM tbl)) || \
                    CASE WHEN (SELECT is_partition FROM parts) THEN \
                        ' PARTITION OF ' || quote_ident((SELECT parent_schema FROM parts)) || '.' || \
                        quote_ident((SELECT parent_name FROM parts)) || chr(10) || '    ' || \
                        COALESCE((SELECT bound FROM parts), '') || \
                        CASE WHEN (SELECT is_partitioned_parent FROM parts) \
                             THEN chr(10) || '    PARTITION BY ' || (SELECT partkey FROM parts) \
                             ELSE '' END || \
                        ';' \
                    ELSE \
                        ' (' || chr(10) || \
                        (SELECT body FROM col_lines) || \
                        CASE WHEN (SELECT body FROM con_lines) IS NOT NULL \
                             THEN ',' || chr(10) || (SELECT body FROM con_lines) \
                             ELSE '' END || \
                        chr(10) || ')' || \
                        CASE WHEN (SELECT is_partitioned_parent FROM parts) \
                             THEN chr(10) || '    PARTITION BY ' || (SELECT partkey FROM parts) \
                             ELSE '' END || \
                        ';' \
                    END || \
                    CASE WHEN (SELECT body FROM idx_lines) IS NOT NULL \
                         THEN chr(10) || chr(10) || (SELECT body FROM idx_lines) \
                         ELSE '' END \
                    AS \"Create Table\"",
            esc(schema), esc(name),
        )
    }

    fn update_row_sql(
        &self,
        schema:  &str,
        table:   &str,
        set_col: &str,
        set_val: &str,
        pk_cols: &[String],
        pk_vals: &[String],
    ) -> String {
        let qid = |s: &str| s.replace('"', "\"\"");
        let qval = |v: &str| {
            if v == "NULL" { "NULL".to_string() }
            else { format!("'{}'", v.replace('\'', "''")) }
        };
        let where_parts: Vec<String> = pk_cols.iter().zip(pk_vals.iter())
            .map(|(c, v)| format!("\"{}\" = {}", qid(c), qval(v)))
            .collect();
        format!(
            "UPDATE \"{}\".\"{}\" SET \"{}\" = {} WHERE {}",
            qid(schema), qid(table), qid(set_col), qval(set_val),
            where_parts.join(" AND "),
        )
    }

    fn insert_row_sql(
        &self,
        schema: &str,
        table:  &str,
        cols:   &[String],
        vals:   &[String],
    ) -> String {
        let qid = |s: &str| s.replace('"', "\"\"");
        let qval = |v: &str| {
            if v == "NULL" { "NULL".to_string() }
            else { format!("'{}'", v.replace('\'', "''")) }
        };
        if cols.is_empty() {
            // All columns left blank → DEFAULT VALUES picks every
            // default and is the closest PG analog to MySQL's
            // empty-column-list INSERT.
            return format!(
                "INSERT INTO \"{}\".\"{}\" DEFAULT VALUES",
                qid(schema), qid(table),
            );
        }
        let col_list = cols.iter()
            .map(|c| format!("\"{}\"", qid(c)))
            .collect::<Vec<_>>()
            .join(", ");
        let val_list = vals.iter()
            .map(|v| qval(v))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO \"{}\".\"{}\" ({}) VALUES ({})",
            qid(schema), qid(table), col_list, val_list,
        )
    }

    fn delete_row_sql(
        &self,
        schema:  &str,
        table:   &str,
        pk_cols: &[String],
        pk_vals: &[String],
    ) -> String {
        let qid = |s: &str| s.replace('"', "\"\"");
        let qval = |v: &str| {
            if v == "NULL" { "NULL".to_string() }
            else { format!("'{}'", v.replace('\'', "''")) }
        };
        let where_parts: Vec<String> = pk_cols.iter().zip(pk_vals.iter())
            .map(|(c, v)| format!("\"{}\" = {}", qid(c), qval(v)))
            .collect();
        format!(
            "DELETE FROM \"{}\".\"{}\" WHERE {}",
            qid(schema), qid(table),
            where_parts.join(" AND "),
        )
    }

    fn upsert_row_sql(
        &self,
        schema:  &str,
        table:   &str,
        cols:    &[String],
        vals:    &[String],
        pk_cols: &[String],
    ) -> String {
        let qid = |s: &str| s.replace('"', "\"\"");
        let qval = |v: &str| {
            if v == "NULL" { "NULL".to_string() }
            else { format!("'{}'", v.replace('\'', "''")) }
        };
        let pk_lower: std::collections::HashSet<String> = pk_cols.iter()
            .map(|c| c.to_ascii_lowercase()).collect();
        let set_cols: Vec<&String> = cols.iter()
            .filter(|c| !pk_lower.contains(&c.to_ascii_lowercase()))
            .collect();

        // Conflict target: the table's PK columns. Always present
        // here because the caller is gated on `pk_cols.is_empty()`.
        let conflict_target = pk_cols.iter()
            .map(|c| format!("\"{}\"", qid(c)))
            .collect::<Vec<_>>()
            .join(", ");

        let insert_prefix = if cols.is_empty() {
            format!("INSERT INTO \"{}\".\"{}\" DEFAULT VALUES",
                qid(schema), qid(table))
        } else {
            let col_list = cols.iter()
                .map(|c| format!("\"{}\"", qid(c)))
                .collect::<Vec<_>>()
                .join(", ");
            let val_list = vals.iter().map(|v| qval(v))
                .collect::<Vec<_>>().join(", ");
            format!("INSERT INTO \"{}\".\"{}\" ({}) VALUES ({})",
                qid(schema), qid(table), col_list, val_list)
        };

        if set_cols.is_empty() {
            // Only PK columns filled (or no columns) → DO NOTHING is
            // the natural intent: "insert if absent, otherwise leave
            // the existing row alone."
            return format!("{} ON CONFLICT ({}) DO NOTHING",
                insert_prefix, conflict_target);
        }
        let set_clause = set_cols.iter()
            .map(|c| format!("\"{c}\" = EXCLUDED.\"{c}\"", c = qid(c)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} ON CONFLICT ({}) DO UPDATE SET {}",
            insert_prefix, conflict_target, set_clause)
    }

    fn count_rows_sql(&self, schema: &str, table: &str, where_clause: Option<&str>) -> String {
        let q = |s: &str| s.replace('"', "\"\"");
        let where_part = where_clause
            .map(|w| w.trim())
            .filter(|w| !w.is_empty())
            .map(|w| {
                let trimmed = w.strip_prefix("WHERE ")
                    .or_else(|| w.strip_prefix("where "))
                    .unwrap_or(w);
                format!(" WHERE {}", trimmed)
            })
            .unwrap_or_default();
        format!("SELECT COUNT(*) AS \"count\" FROM \"{}\".\"{}\"{}", q(schema), q(table), where_part)
    }

    fn select_paged_sql(
        &self,
        schema:       &str,
        table:        &str,
        limit:        usize,
        offset:       usize,
        order_by:     Option<(&str, bool)>,
        where_clause: Option<&str>,
    ) -> String {
        // PG quotes identifiers with double quotes; embedded double
        // quotes are doubled. As with the MySQL plane, schema/table/col
        // come from catalog data but we escape defensively.
        let q = |s: &str| s.replace('"', "\"\"");
        let where_part = where_clause
            .map(|w| w.trim())
            .filter(|w| !w.is_empty())
            .map(|w| {
                let trimmed = w.strip_prefix("WHERE ")
                    .or_else(|| w.strip_prefix("where "))
                    .unwrap_or(w);
                format!(" WHERE {}", trimmed)
            })
            .unwrap_or_default();
        let order_clause = match order_by {
            Some((col, desc)) => format!(
                " ORDER BY \"{}\"{}",
                q(col),
                if desc { " DESC" } else { " ASC" }
            ),
            None => String::new(),
        };
        format!(
            "SELECT * FROM \"{}\".\"{}\"{}{} LIMIT {} OFFSET {}",
            q(schema), q(table), where_part, order_clause, limit, offset,
        )
    }

    fn table_partitions_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // Direct partition children (one level only — sub-partitions belong
        // to their immediate parent, not to the grand-parent). Returns zero
        // rows for non-partitioned tables.
        //
        // `pg_inherits` stores both declarative-partitioning children AND
        // classical `INHERITS` children. The `child_c.relispartition` guard
        // restricts to declarative partitions only — classical inheritance
        // children would surface here with a NULL bound otherwise.
        format!(
            "SELECT child_n.nspname                          AS \"Partition_schema\", \
                    child_c.relname                          AS \"Partition_name\", \
                    pg_get_expr(child_c.relpartbound, child_c.oid) AS \"Partition_bound\", \
                    child_c.reltuples::bigint                AS \"Partition_rows\", \
                    pg_relation_size(child_c.oid)::bigint    AS \"Partition_data_length\" \
             FROM pg_inherits inh \
             JOIN pg_class    parent_c ON parent_c.oid = inh.inhparent \
             JOIN pg_namespace parent_n ON parent_n.oid = parent_c.relnamespace \
             JOIN pg_class    child_c  ON child_c.oid  = inh.inhrelid \
             JOIN pg_namespace child_n ON child_n.oid  = child_c.relnamespace \
             WHERE parent_n.nspname = '{}' AND parent_c.relname = '{}' \
               AND child_c.relispartition \
             ORDER BY child_c.relname",
            esc(schema), esc(name),
        )
    }

    fn partitions_are_navigable(&self) -> bool { true }

    fn table_toast_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // The query returns 1 summary row + N TOASTABLE-column rows.
        //
        // Summary row: Row_kind='summary', Kind discriminates the four
        // layouts the renderer supports:
        //   'is_toast_of' — when this table lives in pg_toast; reverse-
        //                   lookup the parent via pg_class.reltoastrelid.
        //   'partition_aggregate' — c.relkind = 'p'; walk pg_partition_tree
        //                   and sum TOAST across leaves. Emit 'none' if
        //                   no leaf has TOAST'd bytes.
        //   'single' — regular table with c.reltoastrelid > 0 and
        //              non-empty TOAST.
        //   'none' — everything else; renderer hides the section.
        //
        // Column rows: Row_kind='column', one per non-PLAIN attribute
        // (attstorage IN ('e', 'x', 'm')). attstorage decoded server-side
        // into the display string. Empty for TOAST relations themselves
        // and for tables with no TOASTABLE columns.
        format!(
            "WITH tgt AS ( \
                 SELECT c.oid, c.relkind, c.reltoastrelid, c.relname, n.nspname \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = '{schema}' AND c.relname = '{name}' \
             ), \
             /* is_toast_of: reverse-lookup parent when this IS a TOAST rel */ \
             toast_parent AS ( \
                 SELECT pn.nspname AS parent_schema, pc.relname AS parent_name \
                 FROM tgt \
                 JOIN pg_class pc ON pc.reltoastrelid = tgt.oid \
                 JOIN pg_namespace pn ON pn.oid = pc.relnamespace \
                 WHERE tgt.nspname = 'pg_toast' \
             ), \
             /* partition_aggregate: sum TOAST across leaf partitions + \
                find the largest single leaf */ \
             leaves AS ( \
                 SELECT pc.oid, pc.relname, pn.nspname AS schema_name, \
                        pc.reltoastrelid \
                 FROM tgt \
                 CROSS JOIN LATERAL pg_partition_tree(tgt.oid) pt \
                 JOIN pg_class pc ON pc.oid = pt.relid \
                 JOIN pg_namespace pn ON pn.oid = pc.relnamespace \
                 WHERE tgt.relkind = 'p' AND pt.isleaf \
             ), \
             leaf_toasts AS ( \
                 SELECT schema_name, relname, \
                        pg_total_relation_size(reltoastrelid) AS toast_bytes \
                 FROM leaves \
                 WHERE reltoastrelid <> 0 \
             ), \
             agg AS ( \
                 SELECT COUNT(*)::bigint AS partition_count, \
                        COALESCE(SUM(toast_bytes), 0)::bigint AS total_toast \
                 FROM leaf_toasts \
             ), \
             largest AS ( \
                 SELECT schema_name, relname, toast_bytes \
                 FROM leaf_toasts \
                 ORDER BY toast_bytes DESC \
                 LIMIT 1 \
             ), \
             /* single: use the direct reltoastrelid for the summary */ \
             single_toast AS ( \
                 SELECT \
                     tc_ns.nspname AS toast_schema, \
                     tc.relname    AS toast_name, \
                     pg_relation_size(tgt.reltoastrelid)::bigint AS heap_bytes, \
                     COALESCE(pg_indexes_size(tgt.reltoastrelid), 0)::bigint AS index_bytes, \
                     tc.reltuples::bigint AS rows_est, \
                     CASE \
                         WHEN pg_relation_size(tgt.oid) \
                             + pg_total_relation_size(tgt.reltoastrelid) = 0 THEN 0 \
                         ELSE ROUND( \
                             100.0 * pg_total_relation_size(tgt.reltoastrelid)::numeric \
                             / (pg_relation_size(tgt.oid) \
                                + pg_total_relation_size(tgt.reltoastrelid)), \
                             0 \
                         ) \
                     END::bigint AS storage_ratio_pct \
                 FROM tgt \
                 JOIN pg_class tc ON tc.oid = tgt.reltoastrelid \
                 JOIN pg_namespace tc_ns ON tc_ns.oid = tc.relnamespace \
                 WHERE tgt.nspname <> 'pg_toast' \
                   AND tgt.relkind <> 'p' \
                   AND tgt.reltoastrelid <> 0 \
                   AND pg_total_relation_size(tgt.reltoastrelid) > 0 \
             ), \
             /* Summary row — assemble whichever Kind matches. Note the \
                Kind ordering: is_toast_of first (pg_toast schema), then \
                partition_aggregate (relkind='p' with TOAST-carrying \
                leaves), then single, else none. */ \
             summary AS ( \
                 SELECT \
                     'summary'::text AS \"Row_kind\", \
                     CASE \
                         WHEN (SELECT 1 FROM toast_parent) = 1 THEN 'is_toast_of' \
                         WHEN (SELECT total_toast FROM agg) > 0 THEN 'partition_aggregate' \
                         WHEN (SELECT 1 FROM single_toast) = 1 THEN 'single' \
                         ELSE 'none' \
                     END AS \"Kind\", \
                     COALESCE((SELECT toast_schema FROM single_toast), '') AS \"Toast_schema\", \
                     COALESCE((SELECT toast_name FROM single_toast), '')   AS \"Toast_name\", \
                     COALESCE((SELECT heap_bytes::text FROM single_toast), '')  AS \"Heap_bytes\", \
                     COALESCE((SELECT index_bytes::text FROM single_toast), '') AS \"Index_bytes\", \
                     COALESCE((SELECT rows_est::text FROM single_toast), '')    AS \"Rows_est\", \
                     COALESCE((SELECT storage_ratio_pct::text FROM single_toast), '') AS \"Storage_ratio\", \
                     COALESCE((SELECT partition_count::text FROM agg), '') AS \"Partition_count\", \
                     COALESCE((SELECT total_toast::text FROM agg), '')     AS \"Total_toast_bytes\", \
                     COALESCE((SELECT schema_name FROM largest), '') AS \"Largest_schema\", \
                     COALESCE((SELECT relname     FROM largest), '') AS \"Largest_name\", \
                     COALESCE((SELECT toast_bytes::text FROM largest), '') AS \"Largest_bytes\", \
                     COALESCE((SELECT parent_schema FROM toast_parent), '') AS \"Parent_schema\", \
                     COALESCE((SELECT parent_name   FROM toast_parent), '') AS \"Parent_name\", \
                     ''::text AS \"Column_name\", \
                     ''::text AS \"Attstorage\" \
             ), \
             /* Column rows — one per non-PLAIN attstorage. attstorage \
                is a `char` in pg_attribute; decode here into the \
                display string. Only emitted for tables that aren't \
                pg_toast relations themselves (skip on is_toast_of). \
                attnum > 0 filters out system columns. attisdropped \
                filters out dropped columns still lingering in the \
                catalog. */ \
             cols AS ( \
                 SELECT \
                     'column'::text AS \"Row_kind\", \
                     ''::text AS \"Kind\", \
                     ''::text AS \"Toast_schema\", ''::text AS \"Toast_name\", \
                     ''::text AS \"Heap_bytes\", ''::text AS \"Index_bytes\", \
                     ''::text AS \"Rows_est\", ''::text AS \"Storage_ratio\", \
                     ''::text AS \"Partition_count\", ''::text AS \"Total_toast_bytes\", \
                     ''::text AS \"Largest_schema\", ''::text AS \"Largest_name\", \
                     ''::text AS \"Largest_bytes\", \
                     ''::text AS \"Parent_schema\", ''::text AS \"Parent_name\", \
                     a.attname::text AS \"Column_name\", \
                     CASE a.attstorage \
                         WHEN 'e' THEN 'EXTERNAL' \
                         WHEN 'x' THEN 'EXTENDED' \
                         WHEN 'm' THEN 'MAIN' \
                         ELSE '' \
                     END AS \"Attstorage\" \
                 FROM tgt \
                 JOIN pg_attribute a ON a.attrelid = tgt.oid \
                 WHERE tgt.nspname <> 'pg_toast' \
                   AND a.attnum > 0 \
                   AND NOT a.attisdropped \
                   AND a.attstorage IN ('e', 'x', 'm') \
                 ORDER BY a.attnum \
             ) \
             SELECT * FROM summary UNION ALL SELECT * FROM cols",
            schema = esc(schema),
            name = esc(name),
        )
    }

    fn table_vm_sql(&self, schema: &str, name: &str, extensions: &[String]) -> String {
        // Visibility Map summary. One row: Total_pages,
        // All_visible_pages, All_frozen_pages, Source, Relkind,
        // Rel_count. Zero rows when the relation doesn't exist.
        //
        // Path is picked plane-side from the session's installed
        // extensions (probed once at session open):
        //   pg_visibility installed → pg_visibility_map_summary(), a
        //     live per-page read including all-frozen; total pages are
        //     the live relation size, so the ratio can't pass 100%.
        //   otherwise → pg_class.relallvisible (and relallfrozen on
        //     PG 18+, read via to_jsonb so older servers get NULL),
        //     a snapshot from the last VACUUM / ANALYZE.
        //
        // Why not one query with a CASE around the extension call? PG
        // resolves function names at parse time, so an untaken branch
        // still errors "function does not exist" without the extension
        // (PR #107). Branching here keeps the name out of the SQL.
        //
        // Partitioned parents (relkind 'p') own no pages and
        // pg_visibility_map_summary() rejects them, so both paths sum
        // over the parent's leaf partitions (`rels`); Rel_count says
        // how many were summed.
        let esc = |s: &str| s.replace('\'', "''");
        let rels = format!(
            "WITH tgt AS ( \
                 SELECT c.oid, c.relkind \
                 FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = '{schema}' AND c.relname = '{name}' \
                   AND c.relkind IN ('r', 'p', 't', 'm') \
             ), \
             rels AS ( \
                 SELECT tgt.oid FROM tgt WHERE tgt.relkind <> 'p' \
                 UNION ALL \
                 SELECT pt.relid \
                 FROM tgt, pg_partition_tree(tgt.oid) pt \
                 WHERE tgt.relkind = 'p' AND pt.isleaf \
             ) ",
            schema = esc(schema),
            name = esc(name),
        );
        let tail = "(SELECT relkind::text FROM tgt)          AS \"Relkind\", \
                    (SELECT count(*) FROM rels)::text         AS \"Rel_count\" ";
        if extensions.iter().any(|e| e == "pg_visibility") {
            format!(
                "{rels} \
                 SELECT \
                     COALESCE(SUM(pg_relation_size(r.oid) \
                                  / current_setting('block_size')::bigint), 0)::text AS \"Total_pages\", \
                     COALESCE(SUM(v.all_visible), 0)::text AS \"All_visible_pages\", \
                     COALESCE(SUM(v.all_frozen), 0)::text  AS \"All_frozen_pages\", \
                     'pg_visibility'::text                 AS \"Source\", \
                     {tail} \
                 FROM rels r, LATERAL pg_visibility_map_summary(r.oid) v \
                 HAVING EXISTS (SELECT 1 FROM tgt)"
            )
        } else {
            format!(
                "{rels} \
                 SELECT \
                     COALESCE(SUM(c.relpages), 0)::text      AS \"Total_pages\", \
                     COALESCE(SUM(c.relallvisible), 0)::text AS \"All_visible_pages\", \
                     COALESCE(SUM((to_jsonb(c) ->> 'relallfrozen')::bigint)::text, '') \
                                                             AS \"All_frozen_pages\", \
                     'pg_class'::text                        AS \"Source\", \
                     {tail} \
                 FROM rels r JOIN pg_class c ON c.oid = r.oid \
                 HAVING EXISTS (SELECT 1 FROM tgt)"
            )
        }
    }

    fn table_autovac_sql(&self, schema: &str, name: &str) -> String {
        // Autovacuum dump for the Storage tab — one `counters` row plus
        // one `setting` row per knob, discriminated by `Row_kind` (same
        // shape as `toast:*`). Self-contained so the Autovacuum section
        // reads knobs and counters from one snapshot.
        //
        // Setting rows:
        //   Setting          — reloption name, or GUC name for the two
        //                      cluster-only switches (autovacuum,
        //                      track_counts)
        //   Effective_value  — reloption if set, else cluster GUC, else
        //                      the built-in default (reloption-only knobs)
        //   Source           — "reloption" | "cluster" (GUC set away from
        //                      its boot default) | "default"
        //   Reloption_value / Cluster_value — raw inputs for the tooltip
        //   Fallback_value   — the vacuum_* GUC a -1 cost setting defers to
        //   Unit             — pg_settings.unit ("ms" for the time knobs)
        //
        // Counters row: Relkind, Reltuples (pg_class, -1 = never
        // analyzed), Relpages, Relallfrozen, N_dead_tup,
        // N_ins_since_vacuum, N_mod_since_analyze, Xid_age, Mxid_age.
        //
        // Version safety: knobs a server doesn't know (insert_* before
        // PG 13, vacuum_max_threshold before PG 18) have no pg_settings
        // row and can't carry a reloption, so the WHERE drops them.
        // Columns that only exist on newer servers (relallfrozen PG 18,
        // n_ins_since_vacuum PG 13) are read through to_jsonb(row) ->>
        // 'col' — a missing key yields NULL instead of a parse error.
        //
        // Reloption↔GUC name mismatch: the freeze min/table-age
        // reloptions map to vacuum_* GUCs (no autovacuum_ prefix). The
        // VALUES map spells each pair out.
        //
        // pg_stat_all_tables (not _user_) so TOAST relations and
        // catalogs, which autovacuum also processes, get counters.
        // Zero rows when the relation doesn't exist or isn't a
        // table-like relkind.
        let esc = |s: &str| s.replace('\'', "''");
        format!(
            "WITH tgt AS ( \
                 SELECT c.oid, c.relkind, c.reltuples, c.relpages, \
                        COALESCE(c.reloptions, '{{}}'::text[])        AS reloptions, \
                        to_jsonb(c) ->> 'relallfrozen'                AS relallfrozen, \
                        CASE WHEN c.relfrozenxid = '0'::xid THEN NULL \
                             ELSE age(c.relfrozenxid)::bigint END     AS xid_age, \
                        CASE WHEN c.relminmxid = '0'::xid THEN NULL \
                             ELSE mxid_age(c.relminmxid)::bigint END  AS mxid_age \
                 FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = '{schema}' AND c.relname = '{name}' \
                   AND c.relkind IN ('r','p','t','m') \
             ), \
             knobs(ord, setting, guc, fallback_guc, default_val) AS (VALUES \
                 ( 1, 'autovacuum',                            'autovacuum',                            NULL::text,          NULL::text), \
                 ( 2, 'track_counts',                          'track_counts',                          NULL,                NULL), \
                 ( 3, 'autovacuum_enabled',                    NULL,                                    NULL,                'on'), \
                 ( 4, 'autovacuum_vacuum_threshold',           'autovacuum_vacuum_threshold',           NULL,                NULL), \
                 ( 5, 'autovacuum_vacuum_scale_factor',        'autovacuum_vacuum_scale_factor',        NULL,                NULL), \
                 ( 6, 'autovacuum_vacuum_max_threshold',       'autovacuum_vacuum_max_threshold',       NULL,                NULL), \
                 ( 7, 'autovacuum_vacuum_insert_threshold',    'autovacuum_vacuum_insert_threshold',    NULL,                NULL), \
                 ( 8, 'autovacuum_vacuum_insert_scale_factor', 'autovacuum_vacuum_insert_scale_factor', NULL,                NULL), \
                 ( 9, 'autovacuum_analyze_threshold',          'autovacuum_analyze_threshold',          NULL,                NULL), \
                 (10, 'autovacuum_analyze_scale_factor',       'autovacuum_analyze_scale_factor',       NULL,                NULL), \
                 (11, 'autovacuum_freeze_max_age',             'autovacuum_freeze_max_age',             NULL,                NULL), \
                 (12, 'autovacuum_freeze_min_age',             'vacuum_freeze_min_age',                 NULL,                NULL), \
                 (13, 'autovacuum_freeze_table_age',           'vacuum_freeze_table_age',               NULL,                NULL), \
                 (14, 'autovacuum_multixact_freeze_max_age',   'autovacuum_multixact_freeze_max_age',   NULL,                NULL), \
                 (15, 'autovacuum_multixact_freeze_min_age',   'vacuum_multixact_freeze_min_age',       NULL,                NULL), \
                 (16, 'autovacuum_multixact_freeze_table_age', 'vacuum_multixact_freeze_table_age',     NULL,                NULL), \
                 (17, 'autovacuum_vacuum_cost_delay',          'autovacuum_vacuum_cost_delay',          'vacuum_cost_delay', NULL), \
                 (18, 'autovacuum_vacuum_cost_limit',          'autovacuum_vacuum_cost_limit',          'vacuum_cost_limit', NULL), \
                 (19, 'log_autovacuum_min_duration',           'log_autovacuum_min_duration',           NULL,                NULL) \
             ) \
             SELECT \
                 'setting'                                          AS \"Row_kind\", \
                 k.ord                                              AS \"Ord\", \
                 k.setting                                          AS \"Setting\", \
                 COALESCE(NULLIF(r.option_value, ''), s.setting, k.default_val, '') AS \"Effective_value\", \
                 CASE \
                     WHEN r.option_value IS NOT NULL THEN 'reloption' \
                     WHEN s.setting IS NOT NULL AND s.source <> 'default' THEN 'cluster' \
                     ELSE 'default' \
                 END                                                AS \"Source\", \
                 COALESCE(r.option_value, '')                       AS \"Reloption_value\", \
                 COALESCE(s.setting, '')                            AS \"Cluster_value\", \
                 COALESCE(f.setting, '')                            AS \"Fallback_value\", \
                 COALESCE(s.unit, '')                               AS \"Unit\", \
                 ''::text AS \"Relkind\",            ''::text AS \"Reltuples\", \
                 ''::text AS \"Relpages\",           ''::text AS \"Relallfrozen\", \
                 ''::text AS \"N_dead_tup\",         ''::text AS \"N_ins_since_vacuum\", \
                 ''::text AS \"N_mod_since_analyze\", ''::text AS \"Xid_age\", \
                 ''::text AS \"Mxid_age\" \
             FROM knobs k \
             LEFT JOIN LATERAL ( \
                 SELECT option_value \
                 FROM tgt, pg_options_to_table(tgt.reloptions) \
                 WHERE option_name = k.setting \
                 LIMIT 1 \
             ) r ON true \
             LEFT JOIN pg_settings s ON s.name = k.guc \
             LEFT JOIN pg_settings f ON f.name = k.fallback_guc \
             WHERE EXISTS (SELECT 1 FROM tgt) \
               AND (k.guc IS NULL OR s.name IS NOT NULL OR r.option_value IS NOT NULL) \
             UNION ALL \
             SELECT \
                 'counters', 0, '', '', '', '', '', '', '', \
                 tgt.relkind::text, \
                 tgt.reltuples::bigint::text, \
                 tgt.relpages::text, \
                 COALESCE(tgt.relallfrozen, ''), \
                 COALESCE(st.n_dead_tup::text, ''), \
                 COALESCE(to_jsonb(st) ->> 'n_ins_since_vacuum', ''), \
                 COALESCE(st.n_mod_since_analyze::text, ''), \
                 COALESCE(tgt.xid_age::text, ''), \
                 COALESCE(tgt.mxid_age::text, '') \
             FROM tgt \
             LEFT JOIN pg_stat_all_tables st ON st.relid = tgt.oid \
             ORDER BY 2",
            schema = esc(schema),
            name = esc(name),
        )
    }

    fn table_relationship_ancestors_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // depth 1 = tables THIS references; depth 2 = grand-parents.
        // pg_constraint.confrelid is the referenced (parent) side; conrelid
        // is the FK-owning (child) side.
        //
        // Partition-aware: PG stores FK constraints PER-PARTITION. A logical
        // FK declared on a partitioned parent appears in pg_constraint as N
        // rows, one per partition, each with conrelid = the partition. There
        // is no row with conrelid = the partitioned parent itself.
        //
        // The `fks` CTE normalises BOTH ends of every FK row to its partition
        // root, so the focal table matches its partitions' FK rows (when the
        // focal table is partitioned) AND a partition reference in the result
        // collapses to its root (when the neighbour is partitioned). DISTINCT
        // folds the per-partition duplicates. pg_partition_root() returns
        // NULL for non-partitioned, non-child relations; COALESCE keeps the
        // original OID in that case.
        //
        // FK column names: con.conkey / con.confkey are int2[] arrays of
        // column ordinals on conrelid / confrelid. Inline scalar subqueries
        // unnest them WITH ORDINALITY and string_agg the matched attnames in
        // declaration order, producing comma-joined lists. Declarative
        // partitions inherit their parent's column structure, so reading
        // attnames from the per-partition conrelid yields the same names as
        // the partition root and survives DISTINCT folding intact.
        //
        // Focal-table OID resolved via a CTE rather than a `::regclass` cast
        // on a quoted identifier — same safety pattern as table_ddl_sql,
        // robust against names with embedded double-quotes.
        format!(
            "WITH t AS ( \
                 SELECT c.oid FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relname = '{name}' AND n.nspname = '{schema}' \
             ), fks AS ( \
                 SELECT \
                     COALESCE(pg_partition_root(con.conrelid),  con.conrelid)  AS owner_root, \
                     COALESCE(pg_partition_root(con.confrelid), con.confrelid) AS ref_root, \
                     ( SELECT string_agg(a.attname, ',' ORDER BY ck.ord) \
                       FROM unnest(con.conkey) WITH ORDINALITY AS ck(attnum, ord) \
                       JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = ck.attnum \
                     ) AS from_cols, \
                     ( SELECT string_agg(a.attname, ',' ORDER BY ck.ord) \
                       FROM unnest(con.confkey) WITH ORDINALITY AS ck(attnum, ord) \
                       JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = ck.attnum \
                     ) AS to_cols \
                 FROM pg_constraint con \
                 WHERE con.contype = 'f' \
             ) \
             SELECT DISTINCT 1 AS \"Depth\", \
                    n.nspname AS \"Ref_schema\", \
                    c.relname AS \"Ref_table\", \
                    COALESCE(fks.from_cols, '') AS \"From_columns\", \
                    COALESCE(fks.to_cols, '')   AS \"To_columns\", \
                    ''::text                    AS \"Via_schema\", \
                    ''::text                    AS \"Via_table\" \
             FROM fks \
             JOIN pg_class     c ON c.oid = fks.ref_root \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE fks.owner_root = (SELECT oid FROM t) \
             UNION ALL \
             SELECT DISTINCT 2, \
                    n2.nspname, \
                    c2.relname, \
                    COALESCE(p2.from_cols, ''), \
                    COALESCE(p2.to_cols, ''), \
                    n_via.nspname, \
                    c_via.relname \
             FROM fks p1 \
             JOIN fks p2 ON p2.owner_root = p1.ref_root \
             JOIN pg_class     c2    ON c2.oid    = p2.ref_root \
             JOIN pg_namespace n2    ON n2.oid    = c2.relnamespace \
             JOIN pg_class     c_via ON c_via.oid = p1.ref_root \
             JOIN pg_namespace n_via ON n_via.oid = c_via.relnamespace \
             WHERE p1.owner_root = (SELECT oid FROM t)",
            schema = esc(schema), name = esc(name),
        )
    }

    fn table_relationship_descendants_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // depth 1 = tables that reference THIS; depth 2 = grand-children.
        // See table_relationship_ancestors_sql for the partition-handling,
        // CTE, column-aggregation, and DISTINCT rationale. This direction
        // filters on ref_root (and follows owner_root for the depth-2 hop).
        // From_columns is the FK on the descendant side; To_columns is the
        // referenced column on the bridge / focal table.
        format!(
            "WITH t AS ( \
                 SELECT c.oid FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relname = '{name}' AND n.nspname = '{schema}' \
             ), fks AS ( \
                 SELECT \
                     COALESCE(pg_partition_root(con.conrelid),  con.conrelid)  AS owner_root, \
                     COALESCE(pg_partition_root(con.confrelid), con.confrelid) AS ref_root, \
                     ( SELECT string_agg(a.attname, ',' ORDER BY ck.ord) \
                       FROM unnest(con.conkey) WITH ORDINALITY AS ck(attnum, ord) \
                       JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = ck.attnum \
                     ) AS from_cols, \
                     ( SELECT string_agg(a.attname, ',' ORDER BY ck.ord) \
                       FROM unnest(con.confkey) WITH ORDINALITY AS ck(attnum, ord) \
                       JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = ck.attnum \
                     ) AS to_cols \
                 FROM pg_constraint con \
                 WHERE con.contype = 'f' \
             ) \
             SELECT DISTINCT 1 AS \"Depth\", \
                    n.nspname AS \"Ref_schema\", \
                    c.relname AS \"Ref_table\", \
                    COALESCE(fks.from_cols, '') AS \"From_columns\", \
                    COALESCE(fks.to_cols, '')   AS \"To_columns\", \
                    ''::text                    AS \"Via_schema\", \
                    ''::text                    AS \"Via_table\" \
             FROM fks \
             JOIN pg_class     c ON c.oid = fks.owner_root \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE fks.ref_root = (SELECT oid FROM t) \
             UNION ALL \
             SELECT DISTINCT 2, \
                    n2.nspname, \
                    c2.relname, \
                    COALESCE(p2.from_cols, ''), \
                    COALESCE(p2.to_cols, ''), \
                    n_via.nspname, \
                    c_via.relname \
             FROM fks p1 \
             JOIN fks p2 ON p2.ref_root = p1.owner_root \
             JOIN pg_class     c2    ON c2.oid    = p2.owner_root \
             JOIN pg_namespace n2    ON n2.oid    = c2.relnamespace \
             JOIN pg_class     c_via ON c_via.oid = p1.owner_root \
             JOIN pg_namespace n_via ON n_via.oid = c_via.relnamespace \
             WHERE p1.ref_root = (SELECT oid FROM t)",
            schema = esc(schema), name = esc(name),
        )
    }

    fn table_columns_bulk_sql(&self, tables: &[(String, String)]) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        if tables.is_empty() {
            return "SELECT ''::text AS \"Schema_\", ''::text AS \"Table_\", \
                          ''::text AS \"Field\", 0 AS \"Ord\", 0 AS \"Is_pk\" \
                    WHERE FALSE".to_string();
        }
        let tuples = tables.iter()
            .map(|(s, t)| format!("('{}','{}')", esc(s), esc(t)))
            .collect::<Vec<_>>()
            .join(",");
        // Joins: pg_attribute → pg_class/pg_namespace for the table identity,
        // and a LEFT JOIN to pg_index on indrelid + the PRIMARY KEY index
        // (indisprimary). The CASE checks the column's attnum is in indkey
        // — pg_index.indkey is an int2vector of column ordinals participating
        // in the index. Non-PK columns get a NULL i.indkey from the LEFT
        // JOIN's lack of match → CASE returns 0.
        format!(
            "SELECT n.nspname AS \"Schema_\", \
                    c.relname AS \"Table_\", \
                    a.attname AS \"Field\", \
                    a.attnum  AS \"Ord\", \
                    CASE WHEN i.indisprimary AND a.attnum = ANY(i.indkey) THEN 1 ELSE 0 END AS \"Is_pk\" \
             FROM pg_attribute a \
             JOIN pg_class     c ON c.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_index i ON i.indrelid = c.oid AND i.indisprimary \
             WHERE NOT a.attisdropped AND a.attnum > 0 \
               AND (n.nspname, c.relname) IN ({tuples}) \
             ORDER BY n.nspname, c.relname, a.attnum",
            tuples = tuples,
        )
    }

    fn object_ddl_sql(&self, kind: ObjectKind, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        let qual = format!("'{}'::text, '{}'::text", esc(schema), esc(name));
        match kind {
            // Views: pg_get_viewdef accepts oid or text — use a subquery that
            // resolves the oid from pg_class/pg_namespace.
            ObjectKind::Views => format!(
                "SELECT pg_get_viewdef(c.oid, true) AS \"Create View\" \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = '{}' AND c.relname = '{}' AND c.relkind IN ('v','m')",
                esc(schema), esc(name),
            ),
            // Procedures and Functions share pg_proc; pg_get_functiondef
            // handles both (despite the name).
            ObjectKind::Procedures | ObjectKind::Functions => format!(
                "SELECT pg_get_functiondef(p.oid) AS \"Create Function\" \
                 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                 WHERE n.nspname = '{}' AND p.proname = '{}' \
                 LIMIT 1",
                esc(schema), esc(name),
            ),
            // Triggers: pg_get_triggerdef takes the trigger oid. Triggers in
            // Postgres attach to a relation, so we match by trigger name only.
            ObjectKind::Triggers => format!(
                "SELECT pg_get_triggerdef(tg.oid) AS \"Create Trigger\" \
                 FROM pg_trigger tg JOIN pg_class c ON c.oid = tg.tgrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = '{}' AND tg.tgname = '{}' AND NOT tg.tgisinternal \
                 LIMIT 1",
                esc(schema), esc(name),
            ),
            // Tables go through table_ddl_sql, not here.
            ObjectKind::Tables => format!(
                "SELECT '-- Use the Schema tab for PostgreSQL tables.' AS \"Create Table\" \
                 WHERE false OR ({qual}) = ({qual})"
            ),
            // Events unreachable (PG has none).
            ObjectKind::Events => "SELECT '-- Postgres has no events.' AS \"Create Event\"".into(),
        }
    }

    fn keywords(&self) -> &'static [&'static str] { POSTGRES_KEYWORDS }
    fn functions(&self) -> &'static [&'static str] { POSTGRES_FUNCTIONS }
    fn meta_command_names(&self) -> &'static [&'static str] { POSTGRES_META_COMMANDS }
    fn completion_tables_sql(&self) -> String {
        // Tables, views, mat-views, foreign tables, partitioned roots —
        // everything completion candidates after FROM/JOIN should
        // accept. Exclude `pg_toast` (uninteresting) but keep
        // `pg_catalog` and `information_schema` because they're useful
        // when the user knows the system table they want.
        "SELECT n.nspname AS \"Schema_\", c.relname AS \"Table_\" \
           FROM pg_class c \
           JOIN pg_namespace n ON c.relnamespace = n.oid \
          WHERE c.relkind IN ('r','v','m','f','p') \
            AND n.nspname <> 'pg_toast' \
          ORDER BY 1, 2".into()
    }
    fn completion_columns_sql(&self, schema: &str, table: &str) -> String {
        // pg_attribute is faster + more accurate than information_schema
        // (it surfaces dropped columns we filter via attisdropped, and
        // skips system columns via attnum > 0). Both identifiers are
        // sourced from the table cache so we trust them as identifier
        // shape, but still single-quote-escape for literal safety.
        let s = schema.replace('\'', "''");
        let t = table.replace('\'',  "''");
        format!(
            "SELECT a.attname AS \"Column_\" \
               FROM pg_attribute a \
               JOIN pg_class c ON a.attrelid = c.oid \
               JOIN pg_namespace n ON c.relnamespace = n.oid \
              WHERE n.nspname = '{}' AND c.relname = '{}' \
                AND a.attnum > 0 AND NOT a.attisdropped \
              ORDER BY a.attnum",
            s, t,
        )
    }
    fn default_completion_schema(&self, _current_db: &str) -> String {
        // PG's `current_db` is a database, not a schema — unrelated to
        // the schema-keyed completion cache. Default the unqualified
        // `FROM <Tab>` scope to `public` (the conventional user schema);
        // gather_candidates falls back to flattening every cached schema
        // when this one comes up empty, so PG users without a public
        // schema still get something. v2 could read `current_schemas()`
        // off the live session.
        "public".to_string()
    }
    fn lexicon(&self) -> Lexicon { Lexicon::Psql }
    fn sql_dialect(&self) -> &'static dyn sqlparser::dialect::Dialect {
        &sqlparser::dialect::PostgreSqlDialect {}
    }

    // ── Activity pane ──────────────────────────────────────────────────

    fn supported_panes(&self) -> &'static [PaneMenuItem] {
        &[
            PaneMenuItem { kind: PaneKind::SchemaExplorer, label: "Schema Explorer", icon: "⊞" },
            PaneMenuItem { kind: PaneKind::Activity,       label: "Activity",        icon: "⚡" },
        ]
    }

    fn sessions_sql(&self, hide_idle: bool) -> String {
        // "Hide idle" is focus mode — strip idle / idle-in-transaction
        // sessions (unless they're blocking, which makes them the root
        // cause of someone else's wait) AND hide the monitoring session
        // itself. Unchecked is full transparency: show everything,
        // including ourselves.
        let (self_filter, idle_filter) = if hide_idle {
            (
                "AND pid <> pg_backend_pid()",
                "AND (state NOT IN ('idle','idle in transaction','idle in transaction (aborted)') \
                      OR pid IN (SELECT unnest(pg_blocking_pids(w.pid)) \
                                 FROM pg_stat_activity w \
                                 WHERE cardinality(pg_blocking_pids(w.pid)) > 0))",
            )
        } else {
            ("", "")
        };
        format!(
            "SELECT \
                pid::text                                                      AS \"Pid\", \
                COALESCE(usename, '')                                          AS \"User\", \
                COALESCE(host(client_addr), '')                                AS \"Client\", \
                COALESCE(datname, '')                                          AS \"DB\", \
                COALESCE(state, '')                                            AS \"State\", \
                COALESCE(EXTRACT(EPOCH FROM (now() - query_start))::int::text, '0') AS \"Time\", \
                COALESCE(query, '')                                            AS \"Query\" \
             FROM pg_stat_activity \
             WHERE 1=1 {} {} \
             ORDER BY query_start ASC NULLS LAST",
            self_filter, idle_filter,
        )
    }

    fn blockers_sql(&self) -> String {
        // One row per (waiter, blocker) edge via unnest(pg_blocking_pids).
        // cardinality > 0 filters out sessions with no blockers so the edge
        // set is exactly what the tree builder wants.
        "SELECT \
            a.pid::text                                 AS \"Waiter_pid\", \
            (unnest(pg_blocking_pids(a.pid)))::text     AS \"Blocker_pid\" \
         FROM pg_stat_activity a \
         WHERE cardinality(pg_blocking_pids(a.pid)) > 0"
            .into()
    }

    fn locks_sql(&self, _server_version: &str) -> String {
        // `pg_locks` is stable across every supported PG version;
        // version doesn't affect the SQL. Parameter is part of the
        // trait so MySQL can branch (see `MySqlPlane::locks_sql`).
        "SELECT \
            l.pid::text                                  AS \"Pid\", \
            l.mode                                       AS \"Mode\", \
            CASE \
                WHEN l.relation IS NOT NULL THEN n.nspname || '.' || c.relname \
                WHEN l.transactionid IS NOT NULL THEN 'xid:' || l.transactionid::text \
                ELSE COALESCE(l.locktype, '') \
            END                                          AS \"Object\", \
            l.granted                                    AS \"Granted\", \
            ''                                           AS \"Duration\" \
         FROM pg_locks l \
         LEFT JOIN pg_class c     ON c.oid = l.relation \
         LEFT JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE l.pid <> pg_backend_pid()"
            .into()
    }

    fn kill_query_sql(&self, pid: &str) -> String {
        // pg_cancel_backend is the soft kill — cancels the running statement
        // but leaves the session alive. Hard terminate (pg_terminate_backend)
        // is deliberately not exposed in v1.
        format!("SELECT pg_cancel_backend({})", pid)
    }
}

/// Quote an identifier (role / schema / object name) for PG SQL: wrap
/// in double quotes with embedded `"` doubled. Used by GRANT / REVOKE
/// statement builders and by shell-side meta-commands that emit ALTER
/// / DDL on user-supplied names (e.g. `\password`).
pub(crate) fn pg_quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Quote a string for use as a PG SQL literal: wrap in single quotes
/// with embedded `'` doubled. Mirrors `pg_quote_ident`'s shape for
/// the literal channel. Used by the same DDL-emitting meta-commands.
pub(crate) fn pg_quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Quote a grantee, preserving the special `PUBLIC` pseudo-role as a
/// bare keyword. Anything else goes through identifier quoting.
fn pg_quote_grantee(g: &str) -> String {
    if g.eq_ignore_ascii_case("PUBLIC") {
        "PUBLIC".to_string()
    } else {
        pg_quote_ident(g)
    }
}

/// Shared GRANT/REVOKE builder. The two callers differ only in verb
/// (GRANT / REVOKE) and preposition (TO / FROM); the rest of the
/// statement is identical.
fn pg_grant_or_revoke(
    verb: &str,
    prep: &str,
    kind: crate::ObjectKind,
    schema: &str,
    name: &str,
    grantee: &str,
    privilege: &str,
) -> Option<String> {
    use crate::ObjectKind;
    // Pick the keyword that names the object in a GRANT statement:
    //   ON TABLE     — tables, views, matviews, partitioned, foreign
    //   ON SEQUENCE  — sequences (not currently navigable; future)
    //   ON ROUTINE   — functions + procedures (PG 11+), name without
    //                  arg-list disambiguation; matches the
    //                  (schema, name) identity the explorer uses.
    //                  On overloaded routines PG will error if the
    //                  name matches multiple signatures — the user
    //                  sees the DB error in the cell tooltip and
    //                  must disambiguate manually until per-
    //                  signature GRANT/REVOKE is built.
    //                  Older clusters (PG < 11) reject ON ROUTINE
    //                  entirely; this whole feature is gated on a
    //                  modern PG by the trait's None default for
    //                  unsupported planes.
    let on_clause = match kind {
        ObjectKind::Tables | ObjectKind::Views => "TABLE",
        ObjectKind::Procedures | ObjectKind::Functions => "ROUTINE",
        _ => return None,
    };
    let s = pg_quote_ident(schema);
    let n = pg_quote_ident(name);
    let g = pg_quote_grantee(grantee);
    Some(format!(
        "{verb} {priv} ON {on_clause} {s}.{n} {prep} {g}",
        verb = verb, priv = privilege, on_clause = on_clause,
        s = s, n = n, prep = prep, g = g,
    ))
}

/// Dispatch `\?` with an optional sub-topic. Bare `\?` returns the full
/// shell-command help; `\? commands` / `\? options` / `\? variables`
/// return topic-specific variants matching psql's native sub-topic help.
fn psql_help_subtopic(plane: &PostgresPlane, topic: &str) -> Vec<String> {
    match topic.to_ascii_lowercase().as_str() {
        "" | "commands" => plane.help_lines(),
        "options" => vec![
            "psql startup options are not applicable in a browser SQL client —".into(),
            "command-line flags like --host, --port, --file have no analog here.".into(),
            "Use the connection form on the left sidebar to configure connections,".into(),
            "and \\? (or \\? commands) for available shell commands.".into(),
        ],
        "variables" => vec![
            "Client-side variables:".into(),
            "".into(),
            "  \\set name value   Set a variable.".into(),
            "  \\set              List all variables.".into(),
            "  \\unset name       Remove a variable.".into(),
            "".into(),
            "Substitution forms — in SQL (not in quotes, comments or a :: cast)".into(),
            "and in backslash-command arguments (not in quotes):".into(),
            "  :name              Raw value (no quoting).".into(),
            "  :'name'            Value quoted as SQL string literal.".into(),
            "  :\"name\"            Value quoted as SQL identifier.".into(),
            "  :{?name}           TRUE if the variable is set, else FALSE.".into(),
            "".into(),
            "Undefined variables are left as literal text.".into(),
        ],
        other => vec![
            format!("\\?: unknown topic '{}'.  Valid topics: commands, options, variables.", other),
        ],
    }
}

fn pg_sql_help(topic: &str) -> MetaAction {
    let base = "https://www.postgresql.org/docs/current";
    let (syntax, url) = match topic {
        "SELECT" => ("SELECT [ALL | DISTINCT [ON (expr, ...)]] select_list FROM ... [WHERE ...] [GROUP BY ...] [HAVING ...] [ORDER BY ...] [LIMIT n] [OFFSET n]", "sql-select"),
        "INSERT" => ("INSERT INTO table [(columns)] {VALUES (...) | SELECT ...} [ON CONFLICT ... DO {NOTHING | UPDATE SET ...}] [RETURNING ...]", "sql-insert"),
        "UPDATE" => ("UPDATE table SET col = expr [, ...] [FROM ...] [WHERE ...] [RETURNING ...]", "sql-update"),
        "DELETE" => ("DELETE FROM table [USING ...] [WHERE ...] [RETURNING ...]", "sql-delete"),
        "CREATE TABLE" => ("CREATE [TEMP | UNLOGGED] TABLE [IF NOT EXISTS] name (columns, constraints) [INHERITS (...)] [PARTITION BY {RANGE|LIST|HASH} (...)]", "sql-createtable"),
        "ALTER TABLE" => ("ALTER TABLE name {ADD|DROP|ALTER COLUMN|RENAME|SET|ADD CONSTRAINT|DROP CONSTRAINT|...}", "sql-altertable"),
        "DROP TABLE" => ("DROP TABLE [IF EXISTS] name [, ...] [CASCADE | RESTRICT]", "sql-droptable"),
        "CREATE INDEX" => ("CREATE [UNIQUE] INDEX [CONCURRENTLY] [name] ON table [USING method] (columns) [INCLUDE (cols)] [WHERE pred]", "sql-createindex"),
        "DROP INDEX" => ("DROP INDEX [CONCURRENTLY] [IF EXISTS] name [, ...] [CASCADE | RESTRICT]", "sql-dropindex"),
        "CREATE VIEW" => ("CREATE [OR REPLACE] [TEMP] VIEW name [(columns)] AS query [WITH CHECK OPTION]", "sql-createview"),
        "CREATE FUNCTION" => ("CREATE [OR REPLACE] FUNCTION name (args) RETURNS type AS $$ body $$ LANGUAGE lang", "sql-createfunction"),
        "CREATE PROCEDURE" => ("CREATE [OR REPLACE] PROCEDURE name (args) LANGUAGE lang AS $$ body $$", "sql-createprocedure"),
        "CREATE TRIGGER" => ("CREATE [OR REPLACE] TRIGGER name {BEFORE|AFTER|INSTEAD OF} event ON table [FOR EACH {ROW|STATEMENT}] EXECUTE FUNCTION func()", "sql-createtrigger"),
        "GRANT" => ("GRANT privilege [, ...] ON object TO role [, ...] [WITH GRANT OPTION]", "sql-grant"),
        "REVOKE" => ("REVOKE privilege [, ...] ON object FROM role [, ...]", "sql-revoke"),
        "CREATE ROLE" => ("CREATE ROLE name [WITH option ...] — options: SUPERUSER|LOGIN|CREATEDB|CREATEROLE|PASSWORD|VALID UNTIL|...", "sql-createrole"),
        "DROP ROLE" => ("DROP ROLE [IF EXISTS] name [, ...]", "sql-droprole"),
        "BEGIN" => ("BEGIN [WORK | TRANSACTION] [ISOLATION LEVEL ...] [READ WRITE | READ ONLY] [NOT DEFERRABLE]", "sql-begin"),
        "COMMIT" => ("COMMIT [WORK | TRANSACTION] [AND [NO] CHAIN]", "sql-commit"),
        "ROLLBACK" => ("ROLLBACK [WORK | TRANSACTION] [AND [NO] CHAIN] [TO SAVEPOINT name]", "sql-rollback"),
        "EXPLAIN" => ("EXPLAIN [(option, ...)] statement — options: ANALYZE, VERBOSE, COSTS, BUFFERS, WAL, TIMING, SUMMARY, FORMAT {TEXT|JSON|XML|YAML}", "sql-explain"),
        "COPY" => ("COPY table [(columns)] {FROM|TO} {'file'|STDIN|STDOUT} [WITH (FORMAT csv|text|binary, HEADER, DELIMITER, ...)]", "sql-copy"),
        "TRUNCATE" => ("TRUNCATE [TABLE] name [, ...] [RESTART IDENTITY | CONTINUE IDENTITY] [CASCADE | RESTRICT]", "sql-truncate"),
        "VACUUM" => ("VACUUM [FULL] [FREEZE] [VERBOSE] [ANALYZE] [table [(columns)]]", "sql-vacuum"),
        "ANALYZE" => ("ANALYZE [VERBOSE] [table [(columns)]]", "sql-analyze"),
        "REINDEX" => ("REINDEX [{INDEX|TABLE|SCHEMA|DATABASE|SYSTEM}] [CONCURRENTLY] name", "sql-reindex"),
        "CREATE DATABASE" => ("CREATE DATABASE name [WITH option ...] — options: OWNER|TEMPLATE|ENCODING|LC_COLLATE|LC_CTYPE|TABLESPACE|CONNECTION LIMIT", "sql-createdatabase"),
        "DROP DATABASE" => ("DROP DATABASE [IF EXISTS] name [WITH (FORCE)]", "sql-dropdatabase"),
        "CREATE SCHEMA" => ("CREATE SCHEMA [IF NOT EXISTS] name [AUTHORIZATION role]", "sql-createschema"),
        "CREATE EXTENSION" => ("CREATE EXTENSION [IF NOT EXISTS] name [WITH SCHEMA schema] [VERSION version] [CASCADE]", "sql-createextension"),
        "CREATE TYPE" => ("CREATE TYPE name AS {ENUM (...) | (composite_cols) | RANGE (subtype=...)}", "sql-createtype"),
        "MERGE" => ("MERGE INTO target USING source ON condition WHEN MATCHED THEN {UPDATE|DELETE} WHEN NOT MATCHED THEN INSERT ...", "sql-merge"),
        "DO" => ("DO [LANGUAGE lang] $$ code_block $$", "sql-do"),
        "LISTEN" => ("LISTEN channel", "sql-listen"),
        "NOTIFY" => ("NOTIFY channel [, 'payload']", "sql-notify"),
        "*" => {
            return MetaAction::Output(vec![
                "Available SQL help topics:".into(),
                "".into(),
                "  SELECT, INSERT, UPDATE, DELETE, MERGE".into(),
                "  CREATE TABLE, ALTER TABLE, DROP TABLE, TRUNCATE".into(),
                "  CREATE INDEX, DROP INDEX, CREATE VIEW".into(),
                "  CREATE FUNCTION, CREATE PROCEDURE, CREATE TRIGGER, DO".into(),
                "  GRANT, REVOKE, CREATE ROLE, DROP ROLE".into(),
                "  BEGIN, COMMIT, ROLLBACK".into(),
                "  EXPLAIN, COPY, VACUUM, ANALYZE, REINDEX".into(),
                "  CREATE DATABASE, DROP DATABASE, CREATE SCHEMA".into(),
                "  CREATE EXTENSION, CREATE TYPE".into(),
                "  LISTEN, NOTIFY".into(),
                "".into(),
                "Usage: \\h KEYWORD  (e.g. \\h SELECT, \\h CREATE TABLE)".into(),
            ]);
        }
        _ => {
            return MetaAction::Output(vec![
                format!("No help available for '{}'. Try \\h * for a list of topics.", topic),
            ]);
        }
    };
    MetaAction::OutputKV(vec![
        ("Command".into(), topic.to_string()),
        ("Syntax".into(), syntax.to_string()),
        ("Docs".into(), format!("{}/{}.html", base, url)),
    ])
}

/// PostgreSQL keywords — must stay sorted (binary_search depends on it).
/// Covers the standard SQL-92 set plus Postgres extensions (ILIKE,
/// LATERAL, OVER, RETURNING, ON CONFLICT, …) and common PG-specific
/// type aliases (SERIAL, BIGSERIAL, BYTEA, JSONB, TEXT, TIMESTAMPTZ, …).
/// Meta-command names recognized by the PG plane's `meta_cmd`, sorted
/// lexicographically (ASCII) so the tab-completion engine can binary-
/// search or take a prefix-range slice cheaply. The leading `\` is
/// included so an exact string match against the user's token works
/// without extra concatenation.
///
/// MUST stay in sync with `meta_cmd` match arms — when adding a new
/// backslash verb there, add it here too.
static POSTGRES_META_COMMANDS: &[&str] = &[
    "\\?",
    "\\C",      "\\H",        "\\T",         "\\a",
    "\\c",      "\\conninfo", "\\copyright", "\\crosstabview",
    "\\d",      "\\dA",       "\\dAc",       "\\dAf",  "\\dAo",  "\\dAp",
    "\\dC",     "\\dD",       "\\dE",
    "\\dF",     "\\dFd",      "\\dFp",       "\\dFt",
    "\\dL",     "\\dO",       "\\dP",        "\\dPi",  "\\dPt",
    "\\dRp",    "\\dRs",
    "\\dS",     "\\dT",       "\\dX",
    "\\da",     "\\db",       "\\dc",        "\\dconfig",
    "\\dd",     "\\ddp",    "\\des",      "\\det",       "\\deu",  "\\dew",
    "\\df",     "\\dfa",      "\\dfn",       "\\dfp",  "\\dft",  "\\dfw",
    "\\dg",
    "\\di",     "\\dl",       "\\dm",        "\\dn",   "\\dnS",  "\\do",
    "\\drds",   "\\drg",      "\\ds",        "\\dt",   "\\dtS",  "\\du",
    "\\dv",     "\\dvS",
    "\\dx",     "\\dy",
    "\\e",      "\\echo",     "\\ef",        "\\elif",
    "\\else",   "\\encoding", "\\endif",
    "\\errverbose",
    "\\ev",
    "\\f",
    "\\g",      "\\gdesc",     "\\gexec",     "\\gset",      "\\gx",
    "\\h",      "\\help",
    "\\if",
    "\\l",      "\\list",     "\\lo_export","\\lo_import","\\lo_list","\\lo_unlink","\\lol",
    "\\output",
    "\\password", "\\prompt", "\\pset",
    "\\q",      "\\qecho",
    "\\r",
    "\\s",      "\\safe-mode","\\set",       "\\sf",   "\\sv",
    "\\t",      "\\timing",
    "\\unset",
    "\\warn",   "\\watch",
    "\\x",
    "\\z",
];

static POSTGRES_KEYWORDS: &[&str] = &[
    "ABORT", "ADD", "ALL", "ALTER", "ANALYZE", "AND", "ANY", "ARRAY",
    "AS", "ASC", "AT", "ATOMIC",
    "BEGIN", "BETWEEN", "BIGINT", "BIGSERIAL", "BIT", "BOOL", "BOOLEAN",
    "BOTH", "BY", "BYTEA",
    "CALL", "CASCADE", "CASE", "CAST", "CHAR", "CHARACTER", "CHECK",
    "CLUSTER", "COLLATE", "COLUMN", "COMMENT", "COMMIT", "CONCURRENTLY",
    "CONFLICT", "CONSTRAINT", "COPY", "CREATE", "CROSS", "CUBE",
    "CURRENT", "CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP",
    "CURRENT_USER", "CURSOR",
    "DATABASE", "DATE", "DECIMAL", "DECLARE", "DEFAULT", "DEFERRABLE",
    "DELETE", "DESC", "DISTINCT", "DO", "DOMAIN", "DOUBLE", "DROP",
    "EACH", "ELSE", "END", "ESCAPE", "EXCEPT", "EXCLUDE", "EXCLUDING",
    "EXECUTE", "EXISTS", "EXPLAIN", "EXTENSION", "EXTRACT",
    "FALSE", "FETCH", "FILTER", "FIRST", "FLOAT", "FOLLOWING", "FOR",
    "FOREIGN", "FROM", "FULL", "FUNCTION",
    "GENERATED", "GLOBAL", "GRANT", "GROUP", "GROUPING", "GROUPS",
    "HAVING",
    "ILIKE", "IMMEDIATE", "IMMUTABLE", "IMPORT", "IN", "INCLUDING",
    "INDEX", "INHERIT", "INHERITS", "INITIALLY", "INNER", "INOUT",
    "INSERT", "INT", "INT2", "INT4", "INT8", "INTEGER", "INTERSECT",
    "INTERVAL", "INTO", "IS", "ISNULL",
    "JOIN", "JSON", "JSONB",
    "KEY",
    "LANGUAGE", "LAST", "LATERAL", "LEADING", "LEFT", "LIKE", "LIMIT",
    "LISTEN", "LOAD", "LOCAL", "LOCALTIME", "LOCALTIMESTAMP", "LOCK",
    "MATCH", "MATERIALIZED", "MERGE",
    "NAMES", "NATIONAL", "NATURAL", "NCHAR", "NEW", "NEXT", "NO", "NONE",
    "NOT", "NOTHING", "NOTIFY", "NOTNULL", "NOWAIT", "NULL", "NULLS",
    "NUMERIC",
    "OF", "OFF", "OFFSET", "OIDS", "OLD", "ON", "ONLY", "OPERATOR",
    "OR", "ORDER", "OUT", "OUTER", "OVER", "OVERLAPS",
    "PARTITION", "PLACING", "PLANS", "POSITION", "PRECEDING",
    "PRECISION", "PREPARE", "PRESERVE", "PRIMARY", "PROCEDURE",
    "PROCEDURAL",
    "RANGE", "READ", "REAL", "REASSIGN", "RECURSIVE", "REFERENCES",
    "REFRESH", "REINDEX", "RELATIVE", "RELEASE", "RENAME", "REPEATABLE",
    "REPLACE", "RESET", "RESTART", "RESTRICT", "RETURNING", "RETURNS",
    "REVOKE", "RIGHT", "ROLE", "ROLLBACK", "ROLLUP", "ROW", "ROWS",
    "SAVEPOINT", "SCHEMA", "SCROLL", "SECURITY", "SELECT", "SEQUENCE",
    "SERIAL", "SERIALIZABLE", "SESSION", "SESSION_USER", "SET", "SETS",
    "SHARE", "SHOW", "SIMILAR", "SMALLINT", "SMALLSERIAL", "SOME",
    "STABLE", "START", "STATEMENT", "STORAGE", "STORED", "STRICT",
    "SUBSTRING", "SYMMETRIC", "SYSTEM",
    "TABLE", "TABLESAMPLE", "TABLESPACE", "TEMP", "TEMPLATE",
    "TEMPORARY", "TEXT", "THEN", "TIME", "TIMESTAMP", "TIMESTAMPTZ",
    "TIMETZ", "TO", "TRAILING", "TRANSACTION", "TRANSFORM", "TREAT",
    "TRIGGER", "TRUE", "TRUNCATE", "TRUSTED", "TYPE",
    "UNBOUNDED", "UNCOMMITTED", "UNION", "UNIQUE", "UNKNOWN", "UNLISTEN",
    "UNLOGGED", "UNTIL", "UPDATE", "USAGE", "USER", "USING", "UUID",
    "VACUUM", "VALID", "VALIDATE", "VALUES", "VARCHAR", "VARYING",
    "VERBOSE", "VIEW", "VOLATILE",
    "WHEN", "WHERE", "WINDOW", "WITH", "WITHIN", "WITHOUT", "WORK",
    "WRITE",
    "XML",
    "YEAR",
    "ZONE",
];

/// PostgreSQL built-in functions — must stay sorted.
static POSTGRES_FUNCTIONS: &[&str] = &[
    "ABS", "ACOS", "AGE", "ARRAY_AGG", "ARRAY_APPEND", "ARRAY_LENGTH",
    "ARRAY_POSITION", "ARRAY_REMOVE", "ARRAY_TO_STRING", "ASCII", "ASIN",
    "ATAN", "ATAN2", "AVG",
    "BIT_LENGTH", "BOOL_AND", "BOOL_OR", "BTRIM",
    "CARDINALITY", "CEIL", "CEILING", "CHAR_LENGTH", "CHR", "CLOCK_TIMESTAMP",
    "COALESCE", "CONCAT", "CONCAT_WS", "CORR", "COS", "COT", "COUNT",
    "CURRENT_DATABASE", "CURRENT_SCHEMA", "CURRENT_SCHEMAS",
    "CUME_DIST",
    "DATE_BIN", "DATE_PART", "DATE_TRUNC", "DEGREES", "DENSE_RANK",
    "EVERY", "EXP", "EXTRACT",
    "FLOOR", "FORMAT",
    "GENERATE_SERIES", "GREATEST",
    "HAS_SCHEMA_PRIVILEGE", "HAS_TABLE_PRIVILEGE",
    "INITCAP", "ISFINITE",
    "JSON_AGG", "JSON_ARRAY", "JSON_BUILD_ARRAY", "JSON_BUILD_OBJECT",
    "JSON_EACH", "JSON_EXTRACT_PATH", "JSON_OBJECT", "JSONB_AGG",
    "JSONB_ARRAY_ELEMENTS", "JSONB_BUILD_OBJECT", "JSONB_OBJECT_KEYS",
    "JSONB_SET",
    "LAG", "LAST_VALUE", "LEAD", "LEAST", "LEFT", "LENGTH", "LN", "LOG",
    "LOWER", "LPAD", "LTRIM",
    "MAKE_DATE", "MAKE_INTERVAL", "MAKE_TIME", "MAKE_TIMESTAMP",
    "MAX", "MD5", "MIN", "MOD", "MODE",
    "NOW", "NTILE", "NULLIF",
    "OCTET_LENGTH", "OVERLAY",
    "PERCENT_RANK", "PG_BACKEND_PID", "PG_CLIENT_ENCODING",
    "PG_DATABASE_SIZE", "PG_RELATION_SIZE", "PG_SIZE_PRETTY",
    "PG_TOTAL_RELATION_SIZE", "PG_TYPEOF", "PI", "POSITION", "POWER",
    "QUOTE_IDENT", "QUOTE_LITERAL", "QUOTE_NULLABLE",
    "RADIANS", "RANDOM", "RANK", "REGEXP_MATCH", "REGEXP_MATCHES",
    "REGEXP_REPLACE", "REGEXP_SPLIT_TO_ARRAY", "REGEXP_SPLIT_TO_TABLE",
    "REPEAT", "REPLACE", "REVERSE", "RIGHT", "ROUND", "ROW_NUMBER",
    "RPAD", "RTRIM",
    "SESSION_USER", "SETSEED", "SIGN", "SIN", "SPLIT_PART", "SQRT",
    "STARTS_WITH", "STDDEV", "STDDEV_POP", "STDDEV_SAMP",
    "STRING_AGG", "STRING_TO_ARRAY", "STRPOS", "SUBSTR", "SUBSTRING",
    "SUM",
    "TAN", "TO_ASCII", "TO_CHAR", "TO_DATE", "TO_HEX", "TO_NUMBER",
    "TO_TIMESTAMP", "TRANSLATE", "TRIM", "TRUNC",
    "UNNEST", "UPPER",
    "VAR_POP", "VAR_SAMP", "VARIANCE", "VERSION",
    "WIDTH_BUCKET",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_command_names_are_sorted() {
        for w in POSTGRES_META_COMMANDS.windows(2) {
            assert!(w[0] < w[1], "POSTGRES_META_COMMANDS out of order: {:?} >= {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn watch_reads_its_arguments_as_psql_does() {
        let watch = |args: &str| match parse_watch_args(args, &Variables::new()) {
            MetaAction::Watch { interval_ms, count, min_rows } => Ok((interval_ms, count, min_rows)),
            MetaAction::Output(lines) => Err(lines.concat()),
            _ => Err("neither".into()),
        };
        assert_eq!(watch(""), Ok((2000, None, None)));
        assert_eq!(watch("5"), Ok((5000, None, None)));
        assert_eq!(watch("0.2"), Ok((200, None, None)));
        assert_eq!(watch("0"), Ok((0, None, None)));
        assert_eq!(watch("1e0 c=1"), Ok((1000, Some(1), None)));
        assert_eq!(watch("interval=5 count=10 min_rows=1"), Ok((5000, Some(10), Some(1))));
        assert_eq!(watch("i=0.5 c=10 m=1"), Ok((500, Some(10), Some(1))));
        assert_eq!(watch("c=3 5"), Ok((5000, Some(3), None)));
        // Refused in psql's words, at the first wrong argument.
        assert_eq!(watch("abc"), Err("\\watch: incorrect interval value \"abc\"".into()));
        assert_eq!(watch("i=-1"), Err("\\watch: incorrect interval value \"-1\"".into()));
        assert_eq!(watch("c=0"), Err("\\watch: incorrect iteration count \"0\"".into()));
        assert_eq!(watch("c=x"), Err("\\watch: incorrect iteration count \"x\"".into()));
        assert_eq!(watch("m=0"), Err("\\watch: incorrect minimum row count \"0\"".into()));
        assert_eq!(watch("foo=1"), Err("\\watch: unrecognized parameter \"foo=1\"".into()));
        assert_eq!(watch("I=1"), Err("\\watch: unrecognized parameter \"I=1\"".into()));
        assert_eq!(watch("1 2"), Err("\\watch: interval value is specified more than once".into()));
        assert_eq!(watch("i=1 i=2"), Err("\\watch: interval value is specified more than once".into()));
        assert_eq!(watch("c=1 c=2"), Err("\\watch: iteration count is specified more than once".into()));
        assert_eq!(watch("m=1 m=2"), Err("\\watch: minimum row count specified more than once".into()));
    }

    fn prompt_fields(action: MetaAction) -> Option<(String, String)> {
        if let MetaAction::Prompt { text, name } = action {
            Some((text, name))
        } else {
            None
        }
    }

    #[test]
    fn parse_prompt_single_word_is_name() {
        assert_eq!(
            prompt_fields(parse_prompt_args("username")),
            Some(("".into(), "username".into())),
        );
    }

    #[test]
    fn parse_prompt_two_words_is_text_then_name() {
        assert_eq!(
            prompt_fields(parse_prompt_args("Enter username:")),
            Some(("Enter".into(), "username:".into())),
        );
    }

    #[test]
    fn parse_prompt_quoted_text_with_spaces() {
        assert_eq!(
            prompt_fields(parse_prompt_args("'Enter your name: ' username")),
            Some(("Enter your name: ".into(), "username".into())),
        );
        assert_eq!(
            prompt_fields(parse_prompt_args(r#""Enter your name: " username"#)),
            Some(("Enter your name: ".into(), "username".into())),
        );
    }

    #[test]
    fn parse_prompt_empty_returns_usage() {
        assert!(prompt_fields(parse_prompt_args("")).is_none());
    }

    #[test]
    fn parse_prompt_unterminated_quote_errors() {
        assert!(prompt_fields(parse_prompt_args("'unterminated")).is_none());
    }

    // ── \password parser ──────────────────────────────────────────────

    fn password_fields(a: MetaAction) -> Option<Option<String>> {
        if let MetaAction::Password { user } = a { Some(user) } else { None }
    }

    #[test]
    fn parse_password_no_arg_targets_current_user() {
        assert_eq!(password_fields(parse_password_args("")),    Some(None));
        assert_eq!(password_fields(parse_password_args("   ")), Some(None));
    }

    #[test]
    fn parse_password_bare_name() {
        assert_eq!(
            password_fields(parse_password_args("alice")),
            Some(Some("alice".into())),
        );
    }

    #[test]
    fn parse_password_double_quoted_preserves_case_and_spaces() {
        assert_eq!(
            password_fields(parse_password_args(r#""John Doe""#)),
            Some(Some("John Doe".into())),
        );
        assert_eq!(
            password_fields(parse_password_args("'Quoted Name'")),
            Some(Some("Quoted Name".into())),
        );
    }

    #[test]
    fn parse_password_double_quoted_with_embedded_quote() {
        // PG identifier escape: "" inside "..." → literal ".
        assert_eq!(
            password_fields(parse_password_args(r#""a""b""#)),
            Some(Some(r#"a"b"#.into())),
        );
        // Same shape for single quotes.
        assert_eq!(
            password_fields(parse_password_args("'a''b'")),
            Some(Some("a'b".into())),
        );
    }

    #[test]
    fn parse_password_reads_one_name_as_psql_does() {
        // Any more are psql's extra arguments (`extra_arguments`).
        assert_eq!(password_fields(parse_password_args(r#""alice" extra"#)), Some(Some("alice".into())));
        assert_eq!(password_fields(parse_password_args("'alice' extra")), Some(Some("alice".into())));
        assert_eq!(password_fields(parse_password_args("alice extra")), Some(Some("alice".into())));
        assert_eq!(PostgresPlane.extra_arguments("\\password alice extra"), ["\\password: extra argument \"extra\" ignored"]);
    }

    #[test]
    fn parse_password_unterminated_quote_errors() {
        assert!(password_fields(parse_password_args("\"unterminated")).is_none());
        assert!(password_fields(parse_password_args("'unterminated")).is_none());
    }

    // ── \lo_unlink, \encoding ─────────────────────────────────────────

    #[test]
    fn lo_unlink_is_staged_as_psqls_do_lo_unlink() {
        // Outside a transaction (the default context): its own, BEGIN
        // first; the argument read as psql reads it, a `;` dropped.
        assert!(matches!(PostgresPlane.meta_cmd("\\lo_unlink 42;"), Some(MetaAction::Staged(q)) if q == ["BEGIN"]));
        let next = PostgresPlane.meta_continue("\\lo_unlink 42;", &MetaContext::default(), &[shared::BatchResult::default()]);
        assert!(matches!(next, MetaStep::Call(sql) if sql == "SELECT pg_catalog.lo_unlink('42'::pg_catalog.oid)"));
        assert!(matches!(PostgresPlane.meta_cmd("\\lo_unlink"),
            Some(MetaAction::Fail(l)) if l == ["\\lo_unlink: missing required argument"]));
        // Inside one: nothing first; in a failed one, psql's refusal.
        let inside = MetaContext { txn: shared::TxnState::Active, ..MetaContext::default() };
        assert!(matches!(PostgresPlane.meta_cmd_in("\\lo_unlink 42", &inside), Some(MetaAction::Staged(q)) if q.is_empty()));
        let failed = MetaContext { txn: shared::TxnState::Failed, ..MetaContext::default() };
        assert!(matches!(PostgresPlane.meta_cmd_in("\\lo_unlink 42", &failed),
            Some(MetaAction::Fail(l)) if l == ["\\lo_unlink: current transaction is aborted"]));
    }

    #[test]
    fn quiet_leaves_psqls_words_unsaid() {
        let quiet = MetaContext {
            variables: crate::parser::Variables::from([("QUIET".to_string(), "on".to_string())]),
            ..MetaContext::default()
        };
        assert!(matches!(PostgresPlane.meta_cmd_in("\\r", &quiet), Some(MetaAction::ClearInput(None))));
        assert!(matches!(PostgresPlane.meta_cmd("\\r"), Some(MetaAction::ClearInput(Some(_)))));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\pset format", &quiet), Some(MetaAction::Output(l)) if l.is_empty()));
        // Bare `\pset` lists every setting all the same, as psql's does.
        assert!(matches!(PostgresPlane.meta_cmd_in("\\pset", &quiet), Some(MetaAction::Output(l)) if l.len() == 22));
    }

    #[test]
    fn p_prints_the_buffer_as_psql_does() {
        let ctx = MetaContext::default();
        let p = |typed: &str, previous: Option<&str>| PostgresPlane.print_buffer(typed, previous, &ctx);
        assert!(matches!(PostgresPlane.meta_cmd("\\p"), Some(MetaAction::PrintBuffer)));
        assert!(matches!(PostgresPlane.meta_cmd("select 1 \\print"), Some(MetaAction::PrintBuffer)));
        // As typed, the space before the command kept.
        assert_eq!(p("select 1 as a ", None), ["select 1 as a "]);
        // The line break before the command's own line goes; an empty
        // line goes, one of spaces stays.
        assert_eq!(p("select 2\n  as b\n", None), ["select 2", "  as b"]);
        assert_eq!(p("select 3\n\n  as c\n", None), ["select 3", "  as c"]);
        assert_eq!(p("select 5\n   ", None), ["select 5", "   "]);
        assert_eq!(p("select 5\n   \n", None), ["select 5", "   "]);
        // Whitespace and line comments ahead of it aren't kept; a block
        // comment is, and a line comment after the start.
        assert_eq!(p("-- lead\n/* block */ select 4 -- tail\n", None), ["/* block */ select 4 -- tail"]);
        assert_eq!(p(" select 10 as j ", None), ["select 10 as j "]);
        // An empty line in a quote is the string's.
        assert_eq!(p("select 'x\n\ny' as q ", None), ["select 'x", "", "y' as q "]);
        // Variables expanded where psql's lexer expands them.
        let v = MetaContext { variables: Variables::from([("v".to_string(), "42".to_string())]), ..MetaContext::default() };
        assert_eq!(PostgresPlane.print_buffer("select :v as v, ':v' ", None, &v), ["select 42 as v, ':v' "]);
        // Empty: the previous query as psql keeps it — what it sent, with
        // its `;`, or up to its `\g` line (what `\e` edits too).
        let prev = |sql: &str, terminator: &str| PostgresPlane.previous_buffer(sql, terminator);
        assert_eq!(prev("select 6 as f", ";"), "select 6 as f;");
        assert_eq!(prev("select 7 as g", " \\g"), "select 7 as g ");
        assert_eq!(prev("select 8 as h", "\n\\g"), "select 8 as h");
        assert_eq!(prev("select 1 -- why", "\n;"), "select 1 -- why\n;");
        assert_eq!(prev("", ";"), ";");
        assert_eq!(p("", Some("select 6 as f;")), ["select 6 as f;"]);
        assert_eq!(p("-- c\n", Some("select 7 as g ")), ["select 7 as g "]);
        assert_eq!(p("", Some("select 1 -- why\n;")), ["select 1 -- why", ";"]);
        // Neither: psql's words, unless QUIET.
        assert_eq!(p("", None), ["Query buffer is empty."]);
        let quiet = MetaContext { variables: Variables::from([("QUIET".to_string(), "on".to_string())]), ..MetaContext::default() };
        assert!(PostgresPlane.print_buffer("", None, &quiet).is_empty());
        assert_eq!(PostgresPlane.print_buffer("select 1", None, &quiet), ["select 1"]);
        // It reads no arguments.
        assert_eq!(PostgresPlane.extra_arguments("\\p x y"), ["\\p: extra argument \"x\" ignored", "\\p: extra argument \"y\" ignored"]);
    }

    #[test]
    fn a_returnings_tag_follows_its_rows() {
        let status = |tag: &str| shared::CommandStatus { tag: Some(tag.into()), ..Default::default() };
        for tag in ["INSERT 0 2", "UPDATE 0", "DELETE 1", "MERGE 1"] {
            assert!(PostgresPlane.status_after_rows(&status(tag)), "{tag}");
        }
        for tag in ["SELECT 1", "FETCH 2", "SHOW", "EXPLAIN"] {
            assert!(!PostgresPlane.status_after_rows(&status(tag)), "{tag}");
        }
        assert!(!PostgresPlane.status_after_rows(&shared::CommandStatus::default()));
        assert!(!crate::plane::MySqlPlane.status_after_rows(&status("INSERT 0 1")));
    }

    #[test]
    fn command_tags_print_as_psqls_print_query_status() {
        let status = shared::CommandStatus { tag: Some("INSERT 0 1".into()), ..Default::default() };
        assert_eq!(PostgresPlane.command_status(&status, OutputFormat::Aligned), ["INSERT 0 1"]);
        assert_eq!(PostgresPlane.command_status(&status, OutputFormat::Html), ["<p>INSERT 0 1</p>"]);
        assert_eq!(PostgresPlane.command_status(&shared::CommandStatus::default(), OutputFormat::Html), Vec::<String>::new());
    }

    #[test]
    fn encoding_is_the_client_encoding() {
        assert!(matches!(PostgresPlane.meta_cmd("\\encoding"), Some(MetaAction::Output(l)) if l == ["UTF8"]));
        assert!(matches!(PostgresPlane.meta_cmd("\\encoding utf8"), Some(MetaAction::Output(l)) if l.is_empty()));
        assert_eq!(PostgresPlane.client_encoding(), Some("UTF8"));
    }

    #[test]
    fn lo_list_aliases_share_one_sql() {
        // `\lo_list`, `\lol`, and `\dl` all point at the same arm so
        // result column types match across the three names.
        let plane = PostgresPlane;
        let a = plane.meta_cmd("\\lo_list");
        let b = plane.meta_cmd("\\lol");
        let c = plane.meta_cmd("\\dl");
        let to_sql = |m: Option<MetaAction>| {
            if let Some(MetaAction::List { sql, .. }) = m { sql } else { String::new() }
        };
        let sa = to_sql(a);
        let sb = to_sql(b);
        let sc = to_sql(c);
        assert!(!sa.is_empty());
        assert_eq!(sa, sb);
        assert_eq!(sa, sc);
    }

    // ── Tail-line meta-command dispatch ───────────────────────────────
    //
    // These tests exercise the "last line of a multi-line buffer is a
    // meta-command" path that `\g`, `\gx`, `\gexec`, `\gset`,
    // `\gdesc`, and `\crosstabview` all share. Without them, a
    // regression in the line-walking code would silently break the
    // "type SQL, hit Enter, type \gdesc on a fresh line" flow that's
    // the common case for these commands.

    #[test]
    fn command_arguments_expand_variables_as_psql_does() {
        let vars: Variables = [("x", "hello  world"), ("t", "film"), ("n", "2"), ("q", "O'Brien")]
            .into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let ctx = MetaContext { database: "pagila".into(), variables: vars, ..MetaContext::default() };
        let plane = PostgresPlane;
        let action = |input: &str| plane.meta_cmd_in(input, &ctx);
        // What it prints, or the error it fails with.
        let printed = |input: &str| match action(input) {
            Some(MetaAction::Output(lines) | MetaAction::Fail(lines)) => lines.join("\n"),
            _ => panic!("{input}: not printed"),
        };
        // \echo: each argument as psql reads it, one space between.
        assert_eq!(printed("\\echo :x"), "hello  world");
        assert_eq!(printed("\\echo :'x' :\"q\""), "'hello  world' \"O'Brien\"");
        assert_eq!(printed("\\echo ':x' \":x\" :nosuch"), ":x \":x\" :nosuch");
        assert_eq!(printed("\\echo a   b 'it''s'"), "a b it's");
        assert_eq!(printed("\\echo :{?x} :{?nosuch}"), "TRUE FALSE");
        assert_eq!(printed("\\echo -n done"), "done");
        assert_eq!(printed("\\echo 'open"), "unterminated quoted string");
        // \set joins its values; a variable's value copies whole.
        assert!(matches!(action("\\set z a b c"), Some(MetaAction::SetVariable(n, v)) if n == "z" && v == "abc"));
        assert!(matches!(action("\\set y :x"), Some(MetaAction::SetVariable(n, v)) if n == "y" && v == "hello  world"));
        assert!(matches!(action("\\set e ''"), Some(MetaAction::SetVariable(n, v)) if n == "e" && v.is_empty()));
        // Any command's arguments: a pattern, a count, a value.
        assert!(matches!(action("\\d :t"), Some(MetaAction::Staged(_))));
        assert!(matches!(action("select 1 \\watch c=:n"), Some(MetaAction::Watch { count: Some(2), .. })));
        assert!(matches!(action("\\pset null :'q'"), Some(MetaAction::SetNullDisplay(Some(v))) if v == "'O''Brien'"));
        assert!(matches!(action("\\C 'My title'"), Some(MetaAction::SetTitle(Some(v))) if v == "My title"));
        // Not where psql reads the whole line as typed.
        assert!(matches!(action("\\sf :t"), Some(MetaAction::Staged(q)) if q[0].contains("':t'")));
        // A trailing `;` is the argument's where psql keeps it, gone
        // where psql drops it.
        assert_eq!(printed("\\echo a;"), "a;");
        assert!(matches!(action("\\set z 1;"), Some(MetaAction::SetVariable(_, v)) if v == "1;"));
        assert!(matches!(action("\\set q 'select 1;'"), Some(MetaAction::SetVariable(_, v)) if v == "select 1;"));
        assert!(matches!(action("\\pset null x;"), Some(MetaAction::SetNullDisplay(Some(v))) if v == "x;"));
        assert!(matches!(action("\\pset null"), Some(MetaAction::SetNullDisplay(None))));
        // psql reads no `name=value`: that's an option's name.
        assert!(matches!(action("\\pset null=x"), Some(MetaAction::Fail(l)) if l == ["\\pset: unknown option: null=x"]));
        assert_eq!(printed("\\timing on;"), "unrecognized value \"on;\" for \"\\timing\": Boolean expected");
        assert!(matches!(action("\\d film;"), Some(MetaAction::Staged(_))));
        assert!(matches!(action("\\C t;"), Some(MetaAction::SetTitle(Some(v))) if v == "t"));
        assert!(matches!(action("\\sf f;"), Some(MetaAction::Staged(q)) if q[0].contains("'f'::") && !q[0].contains("f;")));
        // An odd quote is psql's error for a command it reads arguments
        // for, not for one it doesn't know.
        assert_eq!(printed("\\echo 'open"), "unterminated quoted string");
        assert!(matches!(action("\\nosuchcommand don't"), Some(MetaAction::Fail(lines)) if lines[0] == "invalid command \\nosuchcommand"));
    }

    #[test]
    fn a_command_psql_lacks_fails_as_psql_fails_it() {
        let plane = PostgresPlane;
        let failed = |input: &str| match plane.meta_cmd(input) {
            Some(MetaAction::Fail(lines)) => lines,
            _ => panic!("{input}: didn't fail"),
        };
        let invalid = |name: &str| vec![format!("invalid command \\{name}"), "Try \\? for help.".to_string()];
        assert_eq!(failed("\\foo"), invalid("foo"));
        // The name runs to whitespace or the next backslash, a `;` kept.
        assert_eq!(failed("\\foo bar \\x"), invalid("foo"));
        assert_eq!(failed("\\foo\\x"), invalid("foo"));
        assert_eq!(failed("\\foo;"), invalid("foo;"));
        assert_eq!(failed("\\ x"), invalid(""));
        assert_eq!(failed("\\\\"), invalid(""));
        assert_eq!(failed("\\fooé"), invalid("fooé"));
        // Other CLIs' commands, and suffixes psql's don't take.
        assert_eq!(failed("\\G"), invalid("G"));
        assert_eq!(failed("\\dFS"), invalid("dFS"));
        assert_eq!(failed("\\lo_foo"), invalid("lo_foo"));
        // After the buffer, the same — the buffer is the shell's to keep.
        assert_eq!(failed("select 1 as a \\G"), invalid("G"));
        assert_eq!(failed("select 1,\n2 \\zz \\x"), invalid("zz"));
        // psql's own commands this shell doesn't run yet say so instead.
        for (input, name) in [("\\copy t to 'f'", "copy"), ("\\COPY t", "COPY"), ("\\i f.sql", "i"), ("\\o out", "o"),
                              ("\\!", "!"), ("\\setenv X 1", "setenv"), ("\\w f", "w"), ("\\bind 1", "bind"),
                              ("\\cd /tmp", "cd"), ("select 1 \\w f", "w")] {
            assert_eq!(failed(input), vec![format!("\\{name} isn't supported here yet.")], "{input}");
        }
        // Its long names for ones it runs are the same commands.
        assert!(matches!(plane.meta_cmd("\\quit"), Some(MetaAction::Disconnect)));
        assert!(matches!(plane.meta_cmd("\\html"), Some(MetaAction::SetFormat { format: OutputFormat::Html, .. })));
        assert!(matches!(plane.meta_cmd("select 1 \\edit"), Some(MetaAction::EditExternal { .. })));
        // psql's branching commands, for the shell's conditional stack.
        assert!(matches!(plane.meta_cmd("\\if true"), Some(MetaAction::Branch(Branch::If(Ok(true))))));
        assert!(matches!(plane.meta_cmd("\\elif maybe"), Some(MetaAction::Branch(Branch::Elif(Err(_))))));
        assert!(matches!(plane.meta_cmd("\\else"), Some(MetaAction::Branch(Branch::Else))));
        assert!(matches!(plane.meta_cmd("select 1;\n\\endif"), Some(MetaAction::Branch(Branch::Endif))));
        // Its expression keeps a `;`, as psql gathers it: `true;` isn't one.
        assert!(matches!(plane.meta_cmd("\\if true;"), Some(MetaAction::Branch(Branch::If(Err(_))))));
    }

    #[test]
    fn a_command_after_sql_on_its_line_runs_there() {
        // psql: the line's first backslash outside quotes and comments
        // starts the command; the SQL before it is the buffer.
        let plane = PostgresPlane;
        let action = |input: &str| plane.meta_cmd(input);
        assert!(matches!(action("select 1 \\gx"), Some(MetaAction::Go { expanded: true })));
        assert!(matches!(action("select 1\\g"), Some(MetaAction::Go { expanded: false })));
        // A `;` stuck to the name is part of it, as psql reads one.
        assert!(matches!(action("select 1 \\gx;"), Some(MetaAction::Fail(lines)) if lines[0] == "invalid command \\gx;"));
        assert!(matches!(action("select 1 \\gexec"), Some(MetaAction::Gexec)));
        assert!(matches!(action("select 1 \\gdesc"), Some(MetaAction::Gdesc)));
        assert!(matches!(action("select 1 as a \\gset p_"), Some(MetaAction::Gset { prefix }) if prefix == "p_"));
        assert!(matches!(action("select 1, 2, 3 \\crosstabview"), Some(MetaAction::Crosstabview { .. })));
        assert!(matches!(action("select now() \\watch 2"), Some(MetaAction::Watch { interval_ms: 2000, .. })));
        assert!(matches!(action("select 1 \\e"), Some(MetaAction::EditExternal { seed: None, line: None })));
        assert!(matches!(action("select 1 \\r"), Some(MetaAction::ClearInput(Some(_)))));
        assert!(matches!(action("select 1\nfrom t \\gx"), Some(MetaAction::Go { expanded: true })));
        // Quoted or commented: not a command.
        assert!(action("select '\\gx'").is_none());
        assert!(action("select 1 -- \\gx").is_none());
        assert!(action("select $$\n\\gx").is_none());
        // Any other command runs there and leaves the buffer (the
        // shell keeps it: `MetaAction::works_on_buffer`), as psql's do.
        for (input, works) in [("select 1 \\gx", true), ("select 1 \\r", true), ("select 1 \\e", true),
                               ("select 1 \\x", false), ("select 1\n\\timing", false), ("select 1 \\d film", false),
                               ("select 1 \\echo hi", false), ("select 1 \\G", false)] {
            let action = action(input).unwrap_or_else(|| panic!("{input}: no command"));
            assert_eq!(action.works_on_buffer(), works, "{input}");
        }
        assert!(matches!(action("select 1 \\x"), Some(MetaAction::SetExpanded(None))));
        assert!(matches!(action("select 1 \\echo hi"), Some(MetaAction::Output(lines)) if lines == ["hi"]));
        // A branching command too, the buffer left where it is.
        assert!(matches!(action("select 1 \\if true"), Some(MetaAction::Branch(Branch::If(Ok(true))))));
        // A command typed first takes its line; the lines after it are
        // typed text again (`command_extent`).
        assert!(matches!(action("\\x\nselect 1;"), Some(MetaAction::SetExpanded(None))));
        assert!(matches!(action("\\echo a\nb"), Some(MetaAction::Output(lines)) if lines == ["a"]));
        assert_eq!(plane.command_extent("\\x\nselect 1;", ";"), 2);
    }

    #[test]
    fn extra_arguments_are_said_as_psql_says_them() {
        let said = |command: &str| PostgresPlane.extra_arguments(command);
        let extra = |cmd: &str, arg: &str| format!("\\{cmd}: extra argument \"{arg}\" ignored");
        assert_eq!(said("\\x on extra"), [extra("x", "extra")]);
        assert_eq!(said("\\x off a b"), [extra("x", "a"), extra("x", "b")]);
        // Read in an ignored branch: quotes resolved, no variables, a `;`
        // kept.
        assert_eq!(said("\\x off 'a b'"), [extra("x", "a b")]);
        assert_eq!(said("\\x on :v"), [extra("x", ":v")]);
        assert_eq!(said("\\x on extra;"), [extra("x", "extra;")]);
        assert!(said("\\x on;").is_empty());
        assert!(said("\\x on").is_empty());
        // Commands that read every argument, or their line.
        for command in ["\\echo a b c", "\\set a b c", "\\df f int text", "\\do + int int", "\\h select x", "\\watch 1 c=2"] {
            assert!(said(command).is_empty(), "{command}");
        }
        // The `\d` family, its suffixes in the name.
        assert_eq!(said("\\dt actor extra"), [extra("dt", "extra")]);
        assert_eq!(said("\\dt+ a b"), [extra("dt+", "b")]);
        assert_eq!(said("\\dA a b"), [extra("dA", "b")]);
        assert!(said("\\dAc a b").is_empty());
        assert_eq!(said("\\dAc a b c"), [extra("dAc", "c")]);
        assert_eq!(said("\\drds r d x"), [extra("drds", "x")]);
        assert_eq!(said("\\lx p x"), [extra("lx", "x")]);
        // The others psql reads a fixed number of.
        assert_eq!(said("\\conninfo x"), [extra("conninfo", "x")]);
        assert_eq!(said("\\pset null x y"), [extra("pset", "y")]);
        assert_eq!(said("\\gset p x"), [extra("gset", "x")]);
        assert_eq!(said("\\crosstabview a b c d e"), [extra("crosstabview", "e")]);
        assert_eq!(said("\\c pagila - - - x"), [extra("c", "x")]);
        assert!(said("\\c -reuse-previous=on pagila - - -").is_empty());
        assert_eq!(said("\\c \"-reuse-previous=on\" - - - - x"), [extra("c", "x")]);
        assert_eq!(said("\\endif y"), [extra("endif", "y")]);
        // An unterminated quote is the command's error; `\g`'s options
        // aren't modelled.
        assert!(said("\\x on 'a").is_empty());
        assert!(said("\\g out.txt x").is_empty());
    }

    #[test]
    fn backslash_s_is_the_history() {
        let plane = PostgresPlane;
        assert!(matches!(plane.meta_cmd("\\s"), Some(MetaAction::ShowHistory)));
        // After SQL it leaves the buffer, as psql's does.
        let action = plane.meta_cmd("select 1 \\s");
        assert!(matches!(action, Some(MetaAction::ShowHistory)));
        assert!(!action.unwrap().works_on_buffer());
        // To a file it would need the host.
        assert!(matches!(plane.meta_cmd("\\s history.txt"), Some(MetaAction::Fail(lines)) if lines[0].starts_with("\\s: writing the history to a file")));
    }

    #[test]
    fn assistance_words_are_read_as_psqls_main_loop_reads_them() {
        let plane = PostgresPlane;
        // `help`, `quit`, `exit` in any case, then only whitespace and
        // maybe one `;` — nothing before them.
        for line in ["help", "HELP;", "help ;", "help  ; "] {
            assert!(matches!(plane.named_command(line, ";"), Some(MetaAction::Output(lines)) if lines == PSQL_HELP), "{line}");
        }
        for line in ["quit", "Quit;", "exit", "exit ;"] {
            assert!(matches!(plane.named_command(line, ";"), Some(MetaAction::Disconnect)), "{line}");
        }
        for line in [" help", "helpme", "help me", "quit;;", "status", "status;", "qui"] {
            assert!(plane.named_command(line, ";").is_none(), "{line}");
        }
    }

    #[test]
    fn assistance_words_typed_while_a_statement_is_pending_get_psqls_hints() {
        let plane = PostgresPlane;
        let said = |input: &str| -> Vec<String> {
            plane.line_notes(input, ";").into_iter()
                .flat_map(|note| match note { crate::ShellEntry::MetaOutput(lines) => lines, _ => Vec::new() })
                .collect()
        };
        assert_eq!(said("select 1 as a\nhelp"), ["Use \\? for help or press control-C to clear the input buffer."]);
        assert_eq!(said("select 2 as b\nquit"), ["Use \\q to quit."]);
        assert_eq!(said("select (1\nexit;"), ["Use \\q to quit."]);
        // Inside a quote or a comment, `quit` and `\q` are text.
        assert_eq!(said("select 'x\nquit"), ["Use control-D to quit."]);
        assert_eq!(said("/* c\nquit"), ["Use control-D to quit."]);
        assert_eq!(said("select 'y\n\\q"), ["Use control-D to quit."]);
        assert_eq!(said("select 'y\nhelp"), ["Use \\? for help or press control-C to clear the input buffer."]);
        // A `/* */` comment is in the buffer; a `--` one isn't.
        assert_eq!(said("/* c */\nhelp"), ["Use \\? for help or press control-C to clear the input buffer."]);
        assert!(said("-- c\nhelp").is_empty());
        // Nothing pending: the word is a command (`named_command`).
        for input in ["help", "select 1;\nquit", "\\x\nhelp", "select 1 \\g\nquit", "select 1\n\\r\nquit"] {
            assert!(said(input).is_empty(), "{input}");
        }
        // Not an assistance word, or `\q` outside a quote: nothing.
        assert!(said("select 1\n  quit").is_empty());
        assert!(said("select 1\n\\q").is_empty());
        // A command that leaves the buffer leaves it pending.
        assert_eq!(said("select 3 as c \\x\nquit"), ["Use \\q to quit."]);
    }

    #[test]
    fn tail_line_g_dispatches_go() {
        let plane = PostgresPlane;
        assert!(matches!(
            plane.meta_cmd("SELECT 1\n\\g"),
            Some(MetaAction::Go { expanded: false }),
        ));
    }

    #[test]
    fn tail_line_gx_forces_expanded() {
        let plane = PostgresPlane;
        assert!(matches!(
            plane.meta_cmd("SELECT 1\n\\gx"),
            Some(MetaAction::Go { expanded: true }),
        ));
    }

    /// psql's `\c` arguments: `-` and nothing keep the current value,
    /// double quotes keep case; only the database can change here.
    #[test]
    fn connect_reads_its_arguments_as_psql_does() {
        let ctx = MetaContext {
            database: "pagila".into(), user: "postgres".into(), host: "127.0.0.1".into(), port: "5432".into(),
            ..MetaContext::default()
        };
        let connect = |input: &str| PostgresPlane.meta_cmd_in(input, &ctx);
        // Some(db): the database `\\c` goes to (None: the current one).
        let to = |input: &str| match connect(input) {
            Some(MetaAction::ReconnectDb(db)) => Some(db),
            _ => None,
        };
        let db = |name: &str| Some(Some(name.to_string()));
        assert_eq!(to("\\c"), Some(None));
        assert_eq!(to("\\connect -"), Some(None));
        assert_eq!(to("\\c postgres"), db("postgres"));
        assert_eq!(to("\\c \"Mixed\"\"Case\""), db("Mixed\"Case"));
        assert_eq!(to("\\c -reuse-previous=on pagila"), db("pagila"));
        assert_eq!(to("\\c pagila postgres 127.0.0.1 5432"), db("pagila"));
        assert_eq!(to("\\c - - - -"), Some(None));
        // `-reuse-previous=`: a Boolean as psql reads one, quoted or not,
        // and only first; refused in psql's words, nothing connected.
        assert_eq!(to("\\c -reuse-previous=T"), Some(None));
        assert_eq!(to("\\c \"-reuse-previous=yes\" postgres"), db("postgres"));
        assert!(matches!(connect("\\c -reuse-previous=bogus pagila"), Some(MetaAction::Fail(l))
            if l == ["unrecognized value \"bogus\" for \"-reuse-previous\": Boolean expected"]));
        assert!(matches!(connect("\\c -reuse-previous="), Some(MetaAction::Fail(l))
            if l == ["unrecognized value \"\" for \"-reuse-previous\": Boolean expected"]));
        // Off: libpq's defaults for the rest — another connection.
        assert!(matches!(connect("\\c -reuse-previous=off pagila"), Some(MetaAction::Fail(l)) if l[0].contains("isn't supported yet")));
        // Not first: an argument like any — here, the user.
        assert!(matches!(connect("\\c pagila -reuse-previous=on"), Some(MetaAction::Fail(l)) if l[0].contains("another user")));
        // On, then a connection string: still not here.
        assert!(matches!(connect("\\c -reuse-previous=on postgresql://postgres@127.0.0.1/pagila"),
            Some(MetaAction::Fail(l)) if l[0].contains("connection string")));
        let output = |input: &str| match connect(input) {
            Some(MetaAction::Output(lines) | MetaAction::Fail(lines)) => lines.join("\n"),
            _ => String::new(),
        };
        for elsewhere in ["\\c pagila bob", "\\c - - localhost", "\\c - - - 5433", "\\c dbname=pagila",
                          "\\c postgresql://postgres@127.0.0.1/pagila"] {
            assert!(output(elsewhere).contains("isn't supported yet"), "{elsewhere}");
        }
        assert_eq!(output("\\c \"pagila"), "unterminated quoted string");
    }

    /// psql's `PrintTiming`, and `\timing [on|off]`.
    #[test]
    fn timing_is_psqls() {
        assert_eq!(psql_time(205), "Time: 0.205 ms");
        assert_eq!(psql_time(3_136), "Time: 3.136 ms");
        assert_eq!(psql_time(1_205_003), "Time: 1205.003 ms (00:01.205)");
        assert_eq!(psql_time(3_723_500_000), "Time: 3723500.000 ms (01:02:03.500)");
        assert_eq!(psql_time(90_000_000_000), "Time: 90000000.000 ms (1 d 01:00:00.000)");
        let timing = |cmd: &str| match PostgresPlane.meta_cmd(cmd) {
            Some(MetaAction::SetTiming(on)) => Ok(on),
            Some(MetaAction::Fail(lines)) => Err(lines.join("\n")),
            _ => Err(String::new()),
        };
        assert_eq!(timing("\\timing"), Ok(None));
        assert_eq!(timing("\\timing on"), Ok(Some(true)));
        assert_eq!(timing("\\timing off"), Ok(Some(false)));
        assert_eq!(timing("\\timing maybe"), Err("unrecognized value \"maybe\" for \"\\timing\": Boolean expected".into()));
    }

    #[test]
    fn connect_messages_are_psqls() {
        let p = PostgresPlane;
        assert_eq!(p.connected_notice("postgres", "postgres", "127.0.0.1", "5432", true),
                   ["You are now connected to database \"postgres\" as user \"postgres\"."]);
        assert_eq!(p.connected_notice("pagila", "postgres", "127.0.0.1", "5432", false),
                   ["You are now connected to database \"pagila\" as user \"postgres\" on host \"127.0.0.1\" at port \"5432\"."]);
        let refusal = shared::DbError {
            severity: Some("FATAL".into()), message: "database \"nosuchdb\" does not exist".into(), ..shared::DbError::default()
        };
        assert_eq!(p.connect_failed("127.0.0.1", "5432", Some(&refusal), "", true), [
            "connection to server at \"127.0.0.1\", port 5432 failed: FATAL:  database \"nosuchdb\" does not exist",
            "Previous connection kept",
        ]);
    }

    #[test]
    fn tail_line_gexec_dispatches_gexec() {
        let plane = PostgresPlane;
        assert!(matches!(
            plane.meta_cmd("SELECT 'DROP TABLE foo'\n\\gexec"),
            Some(MetaAction::Gexec),
        ));
    }

    #[test]
    fn expanded_takes_on_off_auto_as_psql_does() {
        let plane = PostgresPlane;
        let set = |input: &str| match plane.meta_cmd(input) {
            Some(MetaAction::SetExpanded(value)) => Ok(value),
            Some(MetaAction::Fail(lines)) => Err(lines.join(" / ")),
            _ => Err("not \\x".into()),
        };
        assert_eq!(set("\\x"), Ok(None));
        assert_eq!(set("\\x on"), Ok(Some(Expanded::On)));
        assert_eq!(set("\\x t"), Ok(Some(Expanded::On)));
        assert_eq!(set("\\x OFF"), Ok(Some(Expanded::Off)));
        assert_eq!(set("\\x AUTO"), Ok(Some(Expanded::Auto)));
        assert_eq!(set("\\pset x"), Ok(None));
        assert_eq!(set("\\pset expanded auto"), Ok(Some(Expanded::Auto)));
        assert_eq!(set("\\x maybe"), Err("unrecognized value \"maybe\" for \"expanded\" / Available values are: on, off, auto.".into()));
        // No value toggles: on ↔ off, and auto goes off, as psql's does.
        assert_eq!(Expanded::Off.toggled(), Expanded::On);
        assert_eq!(Expanded::On.toggled(), Expanded::Off);
        assert_eq!(Expanded::Auto.toggled(), Expanded::Off);
        assert_eq!(Expanded::Auto.said(), "Expanded display is used automatically.");
        // `auto` expands what's wider than the pane.
        let columns = vec!["a".to_string(), "b".to_string()];
        let rows = vec![vec!["1".to_string(), "x".repeat(30)]];
        let width = |columns: usize, border: u16| PrintOptions {
            columns: Some(columns),
            text: TextOptions { rules: crate::types::Rules { border, ..Default::default() }, ..TextOptions::default() },
            ..PrintOptions::default()
        };
        assert!(plane.expands_automatically(&columns, &rows, &width(20, 1)));
        assert!(!plane.expands_automatically(&columns, &rows, &width(80, 1)));
        // An unmeasured pane never expands.
        assert!(!plane.expands_automatically(&columns, &rows, &PrintOptions::default()));
        // As wide as `\pset border` draws it: 31 columns of text, and
        // 2 / 5 / 7 for borders 0 / 1 / 2.
        assert!(!plane.expands_automatically(&columns, &rows, &width(34, 0)));
        assert!(plane.expands_automatically(&columns, &rows, &width(34, 1)));
        assert!(!plane.expands_automatically(&columns, &rows, &width(37, 1)));
        assert!(plane.expands_automatically(&columns, &rows, &width(37, 2)));
        // Never a single column, which would be wider still.
        assert!(!plane.expands_automatically(&columns[..1], &[vec!["x".repeat(30)]], &width(20, 1)));
    }

    #[test]
    fn sf_and_sv_print_definitions_as_psql_does() {
        use shared::{BatchResult, DbError, ResultSet};
        let plane = PostgresPlane;
        // Staged on the lookup `\ef` / `\ev` use; the name required.
        assert!(matches!(plane.meta_cmd("\\sf last_updated"), Some(MetaAction::Staged(q)) if q[0].contains("'last_updated'::pg_catalog.regproc")));
        assert!(matches!(plane.meta_cmd("\\sv+ actor_info;"), Some(MetaAction::Staged(q)) if q[0].contains("'actor_info'::pg_catalog.regclass")));
        assert!(matches!(plane.meta_cmd("\\sf"), Some(MetaAction::Fail(l)) if l == ["function name is required"]));
        assert!(matches!(plane.meta_cmd("\\sv+"), Some(MetaAction::Fail(l)) if l == ["view name is required"]));
        let reply = |row: [&str; 3]| vec![BatchResult {
            sets: vec![ResultSet { columns: Vec::new(), rows: vec![row.iter().map(|v| Some(v.to_string())).collect()] }],
            error: None,
        }];
        let printed = |step: MetaStep| match step {
            MetaStep::Done(entries) => match &entries[..] {
                [crate::ShellEntry::Printed { lines, .. }] => lines.clone(),
                other => panic!("{other:?}"),
            },
            _ => panic!("not done"),
        };
        // psql 18.4: `\sf+` numbers a function's body, from `AS`.
        let def = "CREATE OR REPLACE FUNCTION public.f()\n RETURNS trigger\nAS $function$\nBEGIN\nEND $function$\n";
        assert_eq!(printed(plane.meta_continue("\\sf f", &MetaContext::default(), &reply([def, "", ""]))),
                   ["CREATE OR REPLACE FUNCTION public.f()", " RETURNS trigger", "AS $function$", "BEGIN", "END $function$"]);
        assert_eq!(printed(plane.meta_continue("\\sf+ f", &MetaContext::default(), &reply([def, "", ""]))), [
            "        CREATE OR REPLACE FUNCTION public.f()",
            "         RETURNS trigger",
            "1       AS $function$",
            "2       BEGIN",
            "3       END $function$",
        ]);
        // `\sv+` numbers every line.
        assert_eq!(printed(plane.meta_continue("\\sv+ v", &MetaContext::default(), &reply(["CREATE OR REPLACE VIEW public.v AS\n SELECT 1", "", ""]))),
                   ["1       CREATE OR REPLACE VIEW public.v AS", "2        SELECT 1"]);
        // Refusals: psql's own, bare; the server's, as minimal_error_message.
        let failed = |step: MetaStep| match step {
            MetaStep::Failed(entries) => match &entries[..] {
                [crate::ShellEntry::CliError { lines, .. }] => lines.clone(),
                other => panic!("{other:?}"),
            },
            _ => panic!("not failed"),
        };
        assert_eq!(failed(plane.meta_continue("\\sv film", &MetaContext::default(), &reply(["", "", "\"public.film\" is not a view"]))),
                   ["\"public.film\" is not a view"]);
        let error = vec![BatchResult {
            sets: Vec::new(),
            error: Some(DbError { severity: Some("ERROR".into()), sqlstate: Some("42883".into()), message: "function \"nosuch\" does not exist".into(), ..DbError::default() }),
        }];
        assert_eq!(failed(plane.meta_continue("\\sf nosuch", &MetaContext::default(), &error)), ["ERROR:  function \"nosuch\" does not exist"]);
    }

    /// `\sf[+]` / `\sv[+]` print what psql 18 prints, refusals too — Pagila's
    /// functions and views, and a view with reloptions and a check option
    /// built in a transaction that's rolled back.
    #[test]
    #[ignore]
    #[allow(clippy::result_large_err)]
    fn sf_and_sv_match_psql() {
        let ctx = MetaContext { database: "pagila".into(), ..MetaContext::default() };
        let session = std::cell::RefCell::new(live::Session::open("pagila"));
        session.borrow_mut().step("BEGIN;\nCREATE VIEW parliament_v_opt WITH (security_barrier=true, check_option=local, security_invoker=on) \
                                   AS SELECT actor_id, first_name FROM actor WHERE actor_id < 5;");
        let mut failures = Vec::new();
        for cmd in ["\\sf last_updated", "\\sf+ last_updated", "\\sf film_in_stock(integer, integer)", "\\sf+ rewards_report",
                    "\\sv actor_info", "\\sv+ sales_by_store", "\\sv parliament_v_opt", "\\sv+ parliament_v_opt"] {
            let ours = live::staged_output(&ctx, cmd, &mut |sql| Ok(session.borrow_mut().rows(sql)));
            let theirs = session.borrow_mut().printed(cmd);
            // Not two empty outputs (a fixture that failed to build).
            assert!(theirs.contains("CREATE OR REPLACE"), "{cmd}: psql printed {theirs:?}");
            if ours != theirs { failures.push(format!("{cmd}:\n  ours: {ours:?}\n  psql: {theirs:?}")); }
        }
        session.borrow_mut().step("ROLLBACK;");
        for cmd in ["\\sf nosuch", "\\sf group_concat", "\\sv film", "\\sv nosuch", "\\sf", "\\sv"] {
            let ours = live::staged_output(&ctx, cmd, &mut live::query);
            let out = live::psql(&["-P", "pager=off", "-c", cmd]);
            let theirs = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
            if ours != theirs { failures.push(format!("{cmd}:\n  ours: {ours:?}\n  psql: {theirs:?}")); }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn vertical_output_is_psqls_expanded() {
        let plane = PostgresPlane;
        let strings = |cells: &[&str]| cells.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        // psql 18.4, `\x on`: records under the title, no blank line or
        // count after them (the shell's footer follows).
        let columns = strings(&["actor_id", "first_name", "two_lines"]);
        let rows = vec![strings(&["1", "PENELOPE", "a\nb"]), strings(&["2", "NICK", "c"])];
        let titled = PrintedWith { title: Some("T".into()), ..PrintedWith::default() };
        assert_eq!(plane.print_vertical(&columns, &rows, &titled), [
            "T",
            "-[ RECORD 1 ]--------",
            "actor_id   | 1",
            "first_name | PENELOPE",
            "two_lines  | a       +",
            "           | b",
            "-[ RECORD 2 ]--------",
            "actor_id   | 2",
            "first_name | NICK",
            "two_lines  | c",
        ]);
        // `\t`: no title or labels, a rule between records.
        let tuples_only = PrintedWith { tuples_only: true, ..titled.clone() };
        assert_eq!(plane.print_vertical(&columns[..1], &[strings(&["1"]), strings(&["2"])], &tuples_only),
                   ["actor_id | 1", "---------+--", "actor_id | 2"]);
        // Drawn as `\pset border` and `linestyle` said.
        let boxed = PrintedWith {
            rules: crate::types::Rules { border: 2, style: LineStyle::Unicode, ..Default::default() },
            ..titled.clone()
        };
        assert_eq!(plane.print_vertical(&columns[1..], &[strings(&["PENELOPE", "a\nb"])], &boxed), [
            "T",
            "┌─[ RECORD 1 ]──────────┐",
            "│ first_name │ PENELOPE │",
            "│ two_lines  │ a       ↵│",
            "│            │ b        │",
            "└────────────┴──────────┘",
        ]);
        // No rows: nothing (the footer says `(0 rows)`).
        assert!(plane.print_vertical(&columns, &[], &titled).is_empty());
    }

    #[test]
    fn pset_format_reads_as_psql_reads_it() {
        let plane = PostgresPlane;
        let in_format = |format| MetaContext { print: PrintOptions { format, ..PrintOptions::default() }, ..MetaContext::default() };
        let set = |input: &str, ctx: &MetaContext| match plane.meta_cmd_in(input, ctx) {
            Some(MetaAction::SetFormat { format, expanded }) => Ok((format, expanded)),
            Some(MetaAction::Fail(lines) | MetaAction::Output(lines)) => Err(lines.join(" / ")),
            _ => Err("not a format".into()),
        };
        let aligned = in_format(OutputFormat::Aligned);
        // Any unique abbreviation, in any case; `latex-longtable` only
        // past `latex` (psql 18.4).
        for (value, format) in [("unaligned", OutputFormat::Unaligned), ("U", OutputFormat::Unaligned), ("c", OutputFormat::Csv),
                                ("h", OutputFormat::Html), ("as", OutputFormat::Asciidoc), ("l", OutputFormat::Latex),
                                ("latex", OutputFormat::Latex), ("latex-", OutputFormat::LatexLongtable),
                                ("LATEX-LONGTABLE", OutputFormat::LatexLongtable), ("t", OutputFormat::TroffMs),
                                ("w", OutputFormat::Wrapped), ("al", OutputFormat::Aligned)] {
            assert_eq!(set(&format!("\\pset format {value}"), &aligned), Ok((format, None)), "{value}");
        }
        assert_eq!(set("\\pset format a", &aligned),
                   Err("\\pset: ambiguous abbreviation \"a\" matches both \"aligned\" and \"asciidoc\"".into()));
        assert_eq!(set("\\pset format ''", &aligned),
                   Err("\\pset: ambiguous abbreviation \"\" matches both \"aligned\" and \"asciidoc\"".into()));
        assert_eq!(set("\\pset format foo", &aligned),
                   Err("\\pset: allowed formats are aligned, asciidoc, csv, html, latex, latex-longtable, troff-ms, unaligned, wrapped".into()));
        assert!(set("\\pset format alignedx", &aligned).is_err_and(|e| e.starts_with("\\pset: allowed")));
        // No value: what it is.
        assert_eq!(set("\\pset format", &in_format(OutputFormat::TroffMs)), Err("Output format is troff-ms.".into()));
        // `\a`: aligned, or from aligned unaligned; `\H`: html, or from
        // html aligned.
        assert_eq!(set("\\a", &aligned), Ok((OutputFormat::Unaligned, None)));
        assert_eq!(set("\\a", &in_format(OutputFormat::Latex)), Ok((OutputFormat::Aligned, None)));
        assert_eq!(set("\\H", &aligned), Ok((OutputFormat::Html, None)));
        assert_eq!(set("\\H", &in_format(OutputFormat::Html)), Ok((OutputFormat::Aligned, None)));
        assert_eq!(set("\\html", &in_format(OutputFormat::Wrapped)), Ok((OutputFormat::Html, None)));
        // Parliament's `\output`: its own names, and psql's whole.
        assert_eq!(set("\\output vertical", &aligned), Ok((OutputFormat::Aligned, Some(Expanded::On))));
        assert_eq!(set("\\output table", &aligned), Ok((OutputFormat::Aligned, Some(Expanded::Off))));
        assert_eq!(set("\\output json", &aligned), Ok((OutputFormat::Json, None)));
        assert_eq!(set("\\output troff-ms", &aligned), Ok((OutputFormat::TroffMs, None)));
        assert!(set("\\output t", &aligned).is_err());
    }

    #[test]
    fn text_options_read_as_psql_reads_them() {
        let plane = PostgresPlane;
        let ctx = MetaContext::default();
        let set = |input: &str| match plane.meta_cmd_in(input, &ctx) {
            Some(MetaAction::SetTextOptions { options, said }) => Ok((options, said.join(" / "))),
            Some(MetaAction::Fail(lines)) => Err(lines.join(" / ")),
            _ => panic!("{input}: not a text option"),
        };
        let said = |input: &str| set(input).map(|(_, said)| said);
        // psql 18.4's words, each from its defaults.
        assert_eq!(said("\\pset fieldsep"), Ok("Field separator is \"|\".".into()));
        assert_eq!(said("\\pset fieldsep ';'"), Ok("Field separator is \";\".".into()));
        assert_eq!(said("\\pset fieldsep '\\t'"), Ok("Field separator is \"\t\".".into()));
        assert_eq!(said("\\pset fieldsep_zero"), Ok("Field separator is zero byte.".into()));
        assert_eq!(said("\\pset recordsep"), Ok("Record separator is <newline>.".into()));
        assert_eq!(said("\\pset recordsep '#'"), Ok("Record separator is \"#\".".into()));
        assert_eq!(said("\\pset recordsep_zero"), Ok("Record separator is zero byte.".into()));
        assert_eq!(said("\\pset csv_fieldsep"), Ok("Field separator for CSV is \",\".".into()));
        assert_eq!(said("\\pset csv_fieldsep ';'"), Ok("Field separator for CSV is \";\".".into()));
        assert_eq!(said("\\pset csv_fieldsep ab"), Err("\\pset: csv_fieldsep must be a single one-byte character".into()));
        assert_eq!(said("\\pset csv_fieldsep é"), Err("\\pset: csv_fieldsep must be a single one-byte character".into()));
        assert_eq!(said("\\pset csv_fieldsep '\"'"), Err("\\pset: csv_fieldsep cannot be a double quote, a newline, or a carriage return".into()));
        assert_eq!(said("\\pset tableattr 'class=\"x\"'"), Ok("Table attributes are \"class=\"x\"\".".into()));
        assert_eq!(said("\\pset tableattr"), Ok("Table attributes unset.".into()));
        assert_eq!(said("\\pset columns"), Ok("Target width is unset.".into()));
        assert_eq!(said("\\pset columns 40"), Ok("Target width is 40.".into()));
        assert_eq!(said("\\pset columns 40abc"), Ok("Target width is 40.".into()));
        assert_eq!(said("\\pset columns x"), Ok("Target width is unset.".into()));
        // \f and \T are the same settings.
        assert_eq!(said("\\f"), Ok("Field separator is \"|\".".into()));
        assert_eq!(said("\\f ':'"), Ok("Field separator is \":\".".into()));
        assert_eq!(said("\\T 'id=t'"), Ok("Table attributes are \"id=t\".".into()));
        assert_eq!(said("\\T"), Ok("Table attributes unset.".into()));
        // They keep a trailing `;`, as psql's `exec_command_f` / `_T` read
        // their argument (psql 18.4: `\f ;` sets `;`).
        assert_eq!(said("\\f ;"), Ok("Field separator is \";\".".into()));
        assert_eq!(said("\\T x;"), Ok("Table attributes are \"x;\".".into()));
        // A setting changes what it names, and only that.
        let (options, _) = set("\\pset recordsep '#'").unwrap();
        assert_eq!(options, TextOptions { recordsep: Separator::Text("#".into()), ..TextOptions::default() });
        // No value keeps what's set.
        let ctx = MetaContext {
            print: PrintOptions { text: TextOptions { fieldsep: Separator::Zero, ..TextOptions::default() }, ..PrintOptions::default() },
            ..MetaContext::default()
        };
        assert!(matches!(plane.meta_cmd_in("\\f", &ctx), Some(MetaAction::SetTextOptions { said, .. }) if said == ["Field separator is zero byte."]));
    }

    #[test]
    fn statements_open_and_close_as_psqls_send_query_does() {
        use crate::plane::Opening;
        use shared::TxnState::{Active, Failed, Idle};
        // psql's `command_no_begin`.
        for sql in ["begin", "  /* x */ COMMIT", "-- note\nrollback", "end", "abort", "start transaction", "vacuum t",
                    "prepare transaction 'x'", "cluster", "create database d", "CREATE TABLESPACE t location 'x'",
                    "create unique index concurrently i on t (a)", "create index concurrently i on t (a)",
                    "drop tablespace t", "drop index concurrently i", "reindex table concurrently t",
                    "reindex database d", "reindex system d", "alter system set work_mem = '4MB'", "discard all"] {
            assert!(command_no_begin(sql), "{sql}");
        }
        for sql in ["select 1", "prepare p as select 1", "cluster t", "create table t (a int)", "create index i on t (a)",
                    "drop table t", "drop index i", "reindex table t", "alter table t add b int", "discard plans",
                    "savepoint s", "release s", "beginx", "/* begin */ select 1"] {
            assert!(!command_no_begin(sql), "{sql}");
        }

        let plane = PostgresPlane;
        let vars = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Variables>();
        let opening = |sql: &str, txn, v: &Variables| plane.statement_opening(sql, txn, v);
        let begin = || Opening { queries: vec!["BEGIN".into()], savepoint: false };
        // AUTOCOMMIT on (the default): nothing.
        assert_eq!(opening("insert into t values (1)", Idle, &vars(&[])), None);
        // Off: BEGIN outside a transaction, not inside one, not before a
        // command that can't run in one.
        let off = vars(&[("AUTOCOMMIT", "off")]);
        assert_eq!(opening("insert into t values (1)", Idle, &off), Some(begin()));
        assert_eq!(opening("insert into t values (1)", Active, &off), None);
        assert_eq!(opening("vacuum t", Idle, &off), None);
        assert_eq!(opening("commit", Idle, &off), None);
        // ON_ERROR_ROLLBACK: a savepoint inside a transaction — the one
        // BEGIN opens too — not outside one or in a failed one.
        let savepoint = || "SAVEPOINT pg_psql_temporary_savepoint".to_string();
        for value in ["on", "interactive", "INTERACTIVE"] {
            let rollback = vars(&[("ON_ERROR_ROLLBACK", value)]);
            assert_eq!(opening("select 1", Active, &rollback), Some(Opening { queries: vec![savepoint()], savepoint: true }));
            assert_eq!(opening("select 1", Idle, &rollback), None);
            assert_eq!(opening("select 1", Failed, &rollback), None);
        }
        let both = vars(&[("AUTOCOMMIT", "off"), ("ON_ERROR_ROLLBACK", "on")]);
        assert_eq!(opening("select 1", Idle, &both), Some(Opening { queries: vec!["BEGIN".into(), savepoint()], savepoint: true }));
        assert_eq!(opening("select 1", Active, &vars(&[("ON_ERROR_ROLLBACK", "off")])), None);
        // Closing: ROLLBACK TO after an error, RELEASE otherwise — unless
        // the statement ended or replaced the savepoint, or the
        // transaction's over.
        let closing = |txn, tag| plane.statement_closing(txn, tag);
        assert_eq!(closing(Failed, None).as_deref(), Some("ROLLBACK TO pg_psql_temporary_savepoint"));
        assert_eq!(closing(Active, Some("INSERT 0 1")).as_deref(), Some("RELEASE pg_psql_temporary_savepoint"));
        for tag in ["COMMIT", "SAVEPOINT", "RELEASE", "ROLLBACK"] {
            assert_eq!(closing(Active, Some(tag)), None, "{tag}");
        }
        assert_eq!(closing(Idle, Some("COMMIT")), None);
        assert_eq!(crate::plane::MySqlPlane.statement_opening("select 1", Idle, &off), None);
    }

    #[test]
    fn gdesc_names_types_as_psql_asks() {
        use shared::DescribedColumn;
        let column = |name: &str, type_id: &str, type_modifier| DescribedColumn { name: name.into(), type_id: type_id.into(), type_modifier };
        // psql 18.4's `DescribeQuery`: each name as `PQescapeLiteral`
        // writes it — quotes doubled, a backslash an E'' string — with its
        // type's OID and modifier.
        assert_eq!(
            PostgresPlane.described_columns_sql(&[column("a", "23", -1), column("o'neil", "1043", 34), column("x\\y", "25", -1)]).unwrap(),
            "SELECT name AS \"Column\", pg_catalog.format_type(tp, tpm) AS \"Type\"\n\
             FROM (VALUES ('a', '23'::pg_catalog.oid, -1),('o''neil', '1043'::pg_catalog.oid, 34),( E'x\\\\y', '25'::pg_catalog.oid, -1)) s(name, tp, tpm)",
        );
        // The mysql CLI has no `\gdesc`.
        assert_eq!(crate::plane::MySqlPlane.described_columns_sql(&[column("a", "INT", -1)]), None);
    }

    #[test]
    fn pset_reads_and_says_as_psql_does() {
        use crate::types::XHeader;
        let plane = PostgresPlane;
        let ctx = MetaContext::default();
        let action = |input: &str| plane.meta_cmd_in(input, &ctx).expect(input);
        let failed = |input: &str| match action(input) {
            MetaAction::Fail(lines) => lines.join(" / "),
            _ => panic!("{input}: not what was expected"),
        };
        let said = |input: &str| match action(input) {
            MetaAction::SetTextOptions { said, .. } => said.join(" / "),
            _ => panic!("{input}: not what was expected"),
        };
        // psql 18.4. Option names are psql's exactly — no other case, no
        // `name=value`.
        assert_eq!(failed("\\pset FORMAT"), "\\pset: unknown option: FORMAT");
        assert_eq!(failed("\\pset Expanded on"), "\\pset: unknown option: Expanded");
        assert_eq!(failed("\\pset nonsense"), "\\pset: unknown option: nonsense");
        assert_eq!(failed("\\pset format=csv"), "\\pset: unknown option: format=csv");
        // Booleans: a value sets silently, none toggles; prefixes as psql
        // reads them; refused naming the option as typed.
        assert!(matches!(action("\\pset tuples_only on"), MetaAction::SetTuplesOnly(Some(true))));
        assert!(matches!(action("\\pset t of"), MetaAction::SetTuplesOnly(Some(false))));
        assert!(matches!(action("\\pset t"), MetaAction::SetTuplesOnly(None)));
        assert!(matches!(action("\\t y"), MetaAction::SetTuplesOnly(Some(true))));
        assert_eq!(failed("\\pset tuples_only x"), "unrecognized value \"x\" for \"tuples_only\": Boolean expected");
        assert_eq!(failed("\\pset t o"), "unrecognized value \"o\" for \"t\": Boolean expected");
        assert_eq!(failed("\\t x"), "unrecognized value \"x\" for \"tuples_only\": Boolean expected");
        assert!(matches!(action("\\pset footer off"), MetaAction::SetFooter(Some(false))));
        assert!(matches!(action("\\pset footer"), MetaAction::SetFooter(None)));
        assert_eq!(failed("\\pset footer x"), "unrecognized value \"x\" for \"footer\": Boolean expected");
        assert_eq!(failed("\\pset x x"), "unrecognized value \"x\" for \"x\" / Available values are: on, off, auto.");
        assert_eq!(failed("\\x x"), "unrecognized value \"x\" for \"expanded\" / Available values are: on, off, auto.");
        assert!(matches!(action("\\pset vertical on"), MetaAction::SetExpanded(Some(Expanded::On))));
        // Titles: none unsets, an empty one is a title.
        assert!(matches!(action("\\pset title"), MetaAction::SetTitle(None)));
        assert!(matches!(action("\\pset title ''"), MetaAction::SetTitle(Some(t)) if t.is_empty()));
        assert!(matches!(action("\\C"), MetaAction::SetTitle(None)));
        // The pager settings, kept and said as psql says them.
        assert_eq!(said("\\pset pager"), "Pager usage is off.");
        assert_eq!(said("\\pset pager ALWAYS"), "Pager is always used.");
        assert_eq!(said("\\pset pager on"), "Pager is used for long output.");
        assert_eq!(failed("\\pset pager x"), "unrecognized value \"x\" for \"pager\" / Available values are: on, off, always.");
        assert_eq!(said("\\pset pager_min_lines"), "Pager won't be used for less than 0 lines.");
        assert_eq!(said("\\pset pager_min_lines 1"), "Pager won't be used for less than 1 line.");
        assert_eq!(said("\\pset pager_min_lines 0x10"), "Pager won't be used for less than 16 lines.");
        assert_eq!(said("\\pset pager_min_lines 010"), "Pager won't be used for less than 8 lines.");
        assert_eq!(failed("\\pset pager_min_lines 5x"), "invalid value \"5x\" for \"pager_min_lines\": integer expected");
        assert_eq!(failed("\\pset pager_min_lines 08"), "invalid value \"08\" for \"pager_min_lines\": integer expected");
        // psql 18.4: past an `int` and a trailing space refused; a
        // leading space, a sign and hex read.
        assert_eq!(failed("\\pset pager_min_lines 99999999999"), "invalid value \"99999999999\" for \"pager_min_lines\": integer expected");
        assert_eq!(failed("\\pset pager_min_lines '7 '"), "invalid value \"7 \" for \"pager_min_lines\": integer expected");
        assert_eq!(said("\\pset pager_min_lines ' 7'"), "Pager won't be used for less than 7 lines.");
        assert_eq!(said("\\pset pager_min_lines -0x10"), "Pager won't be used for less than -16 lines.");
        assert_eq!(said("\\pset pager_min_lines -2147483648"), "Pager won't be used for less than -2147483648 lines.");
        // numericlocale: set silently, toggled aloud. xheader_width:
        // psql's words.
        assert_eq!(said("\\pset numericlocale off"), "");
        assert_eq!(said("\\pset numericlocale on"), "");
        assert_eq!(said("\\pset numericlocale"), "Locale-adjusted numeric output is on.");
        assert_eq!(failed("\\pset numericlocale x"), "unrecognized value \"x\" for \"numericlocale\": Boolean expected");
        assert_eq!(said("\\pset xheader_width"), "Expanded header width is \"full\".");
        assert_eq!(said("\\pset xheader_width COLUMN"), "Expanded header width is \"column\".");
        assert_eq!(said("\\pset xheader_width 30"), "Expanded header width is 30.");
        assert_eq!(failed("\\pset xheader_width x"),
                   "\\pset: allowed xheader_width values are \"full\" (default), \"column\", \"page\", or a number specifying the exact width");
        // Bare `\pset`: every setting, as psql lists its defaults …
        let listed = |ctx: &MetaContext| match plane.meta_cmd_in("\\pset", ctx) {
            Some(MetaAction::Output(lines)) => lines,
            _ => panic!("\\pset: not a listing"),
        };
        assert_eq!(listed(&ctx), [
            "border                   1",
            "columns                  0",
            "csv_fieldsep             ','",
            "expanded                 off",
            "fieldsep                 '|'",
            "fieldsep_zero            off",
            "footer                   on",
            "format                   aligned",
            "linestyle                ascii",
            "null                     ''",
            "numericlocale            off",
            "pager                    1",
            "pager_min_lines          0",
            "recordsep                '\\n'",
            "recordsep_zero           off",
            "tableattr                ",
            "title                    ",
            "tuples_only              off",
            "unicode_border_linestyle single",
            "unicode_column_linestyle single",
            "unicode_header_linestyle single",
            "xheader_width            full",
        ]);
        // … and what's set, quoted as psql quotes it.
        let set = MetaContext {
            null_display: "a'b".into(),
            title: Some("x\ny".into()),
            print: PrintOptions {
                text: TextOptions {
                    fieldsep: Separator::Zero, recordsep: Separator::Text("#".into()), pager: 0,
                    rules: crate::types::Rules { xheader: XHeader::Exact(30), ..Default::default() }, ..TextOptions::default()
                },
                ..PrintOptions::default()
            },
            ..MetaContext::default()
        };
        let lines = listed(&set);
        for line in ["fieldsep                 ''", "fieldsep_zero            on", "null                     'a\\'b'",
                     "pager                    0", "recordsep                '#'", "title                    'x\\ny'",
                     "xheader_width            30"] {
            assert!(lines.iter().any(|l| l == line), "{line:?} in {lines:?}");
        }
    }

    #[test]
    fn border_and_line_styles_read_as_psql_reads_them() {
        use crate::types::Rules;
        let plane = PostgresPlane;
        let ctx = MetaContext::default();
        let set = |input: &str| match plane.meta_cmd_in(input, &ctx) {
            Some(MetaAction::SetTextOptions { options, said }) => Ok((options.rules, said.join(" / "))),
            Some(MetaAction::Fail(lines)) => Err(lines.join(" / ")),
            _ => panic!("{input}: not a print option"),
        };
        let said = |input: &str| set(input).map(|(_, said)| said);
        // psql 18.4's words, each from its defaults.
        assert_eq!(said("\\pset border"), Ok("Border style is 1.".into()));
        assert_eq!(said("\\pset border 2"), Ok("Border style is 2.".into()));
        assert_eq!(said("\\pset border 7x"), Ok("Border style is 7.".into()));
        assert_eq!(said("\\pset border abc"), Ok("Border style is 0.".into()));
        // psql keeps it in an `unsigned short`.
        assert_eq!(said("\\pset border -1"), Ok("Border style is 65535.".into()));
        assert_eq!(said("\\pset linestyle"), Ok("Line style is ascii.".into()));
        assert_eq!(said("\\pset linestyle O"), Ok("Line style is old-ascii.".into()));
        assert_eq!(said("\\pset linestyle u"), Ok("Line style is unicode.".into()));
        // Any prefix — the empty one too — of the first name it starts.
        assert_eq!(said("\\pset linestyle ''"), Ok("Line style is ascii.".into()));
        assert_eq!(said("\\pset linestyle x"), Err("\\pset: allowed line styles are ascii, old-ascii, unicode".into()));
        // A double-quoted value keeps its quotes, as psql's do.
        assert_eq!(said("\\pset linestyle \"\""), Err("\\pset: allowed line styles are ascii, old-ascii, unicode".into()));
        assert_eq!(said("\\pset unicode_border_linestyle D"), Ok("Unicode border line style is \"double\".".into()));
        assert_eq!(said("\\pset unicode_column_linestyle"), Ok("Unicode column line style is \"single\".".into()));
        assert_eq!(said("\\pset unicode_header_linestyle ''"), Ok("Unicode header line style is \"single\".".into()));
        assert_eq!(said("\\pset unicode_header_linestyle x"),
                   Err("\\pset: allowed Unicode header line styles are single, double".into()));
        assert_eq!(said("\\pset unicode_column_linestyle x"),
                   Err("\\pset: allowed Unicode column line styles are single, double".into()));
        // Each changes what it names, and only that.
        assert_eq!(set("\\pset unicode_column_linestyle double").unwrap().0,
                   Rules { unicode_column: UnicodeLine::Double, ..Rules::default() });
        assert_eq!(set("\\pset linestyle unicode").unwrap().0, Rules { style: LineStyle::Unicode, ..Rules::default() });
    }

    #[test]
    fn results_print_in_psqls_other_formats() {
        let plane = PostgresPlane;
        let strings = |cells: &[&str]| cells.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        let options = |format| PrintOptions { format, ..PrintOptions::default() };
        let (columns, types, rows) = (strings(&["n", "t"]), strings(&["23", "25"]), vec![strings(&["1", "a"])]);
        // Aligned is the grid's — and json / markdown the shell's own.
        for format in [OutputFormat::Aligned, OutputFormat::Json, OutputFormat::Markdown] {
            assert_eq!(plane.print_result(&columns, &types, &rows, None, &[], &options(format)), None);
        }
        assert_eq!(plane.print_result(&columns, &types, &rows, Some("T"), &[], &options(OutputFormat::Unaligned)),
                   Some(strings(&["T", "n|t", "1|a", "(1 row)"])));
        // Numbers line up right by their type.
        let html = plane.print_result(&columns, &types, &rows, None, &[], &options(OutputFormat::Html)).unwrap();
        assert!(html.contains(&"    <td align=\"right\">1</td>".to_string()), "{html:?}");
        assert!(html.contains(&"    <td align=\"left\">a</td>".to_string()), "{html:?}");
        // Without types, left.
        let html = plane.print_result(&columns, &[], &rows, None, &[], &options(OutputFormat::Html)).unwrap();
        assert!(html.contains(&"    <td align=\"left\">1</td>".to_string()), "{html:?}");
        // The grid lines up the same way, under centred headers.
        assert_eq!(plane.column_align("23"), Align::Right);
        assert_eq!(plane.column_align("25"), Align::Left);
        assert!(plane.centres_headers());
    }

    #[test]
    fn a_semicolon_stuck_to_a_command_is_its_name_as_psql_reads_it() {
        // psql's command name runs to whitespace or a backslash, a `;`
        // included: commands matched by name are then unknown; the `\d`
        // family reads its letters as psql does — `listTables` ignores
        // ones it doesn't look for, `\df` checks its own.
        let plane = PostgresPlane;
        let invalid = |input: &str| matches!(plane.meta_cmd(input), Some(MetaAction::Fail(lines)) if lines[0].starts_with("invalid command"));
        for input in ["\\x;", "\\gx;", "\\echo;", "\\conninfo;", "\\timing;", "\\d;", "\\df;", "\\l;", "\\dP;", "\\if;"] {
            assert!(invalid(input), "{input}");
        }
        for input in ["\\dt;", "\\d+;", "\\dtq", "\\du;", "\\dx;", "\\dn;", "\\lo_list;"] {
            assert!(!invalid(input), "{input}");
        }
        assert!(matches!(plane.meta_cmd("\\dfa;"), Some(MetaAction::Fail(lines)) if lines == ["\\df only takes [anptwSx+] as options"]));
        // After a space it's an argument's: `\x on;` is `\x`, `\echo a;`
        // prints `a;`.
        assert!(!invalid("\\x on;"));
        assert!(matches!(plane.meta_cmd("\\echo a;"), Some(MetaAction::Output(lines)) if lines == ["a;"]));
    }

    #[test]
    fn edit_takes_an_optional_line_number_like_psql() {
        let plane = PostgresPlane;
        let line = |input: &str| match plane.meta_cmd(input) {
            Some(MetaAction::EditExternal { seed: None, line }) => Ok(line),
            Some(MetaAction::Fail(lines)) => Err(lines.join(" ")),
            _ => Err("not \\e".into()),
        };
        assert_eq!(line("\\e"), Ok(None));
        assert!(line("\\e;").unwrap_err().starts_with("invalid command \\e;"));
        assert_eq!(line("\\e 12"), Ok(Some(12)));
        assert_eq!(line("SELECT 1\nFROM t\n\\e 2"), Ok(Some(2)));
        assert_eq!(line("SELECT 1\n\\e"), Ok(None));
        assert!(line("\\e 0").unwrap_err().contains("invalid line number: 0"));
        // Anything else names a file, which isn't supported yet.
        assert!(line("\\e query.sql").unwrap_err().contains("editing a file isn't supported"));
        assert!(line("\\e query.sql 3").unwrap_err().contains("editing a file isn't supported"));
        // \echo is not \e.
        assert!(!matches!(plane.meta_cmd("\\echo hi"), Some(MetaAction::EditExternal { .. })));
        assert!(!matches!(plane.meta_cmd("SELECT 1\n\\echo"), Some(MetaAction::EditExternal { .. })));
    }

    #[test]
    fn relation_lists_are_psql_s_list_tables() {
        let plane = PostgresPlane;
        let ctx = MetaContext { database: "pagila".into(), ..MetaContext::default() };
        let list = |input: &str| match plane.meta_cmd_in(input, &ctx) {
            Some(MetaAction::List { sql, vertical: false, none: Some(none), .. }) => (sql, none),
            _ => panic!("{input}: unexpected action"),
        };
        // \d alone lists every kind but indexes, outside system schemas.
        let (sql, none) = list("\\d");
        assert!(sql.contains("WHERE c.relkind IN ('r','p','v','m','S','f','')\n"), "{sql}");
        assert!(sql.contains("AND n.nspname <> 'pg_catalog'"), "{sql}");
        assert!(sql.contains("AND pg_catalog.pg_table_is_visible(c.oid)\nORDER BY 1,2;"), "{sql}");
        assert_eq!(none, "Did not find any relations.");
        // A pattern: TOAST and special relations join in, no system-schema filter.
        let (sql, none) = list("\\dt film*");
        assert!(sql.contains("WHERE c.relkind IN ('r','p','t','s','')\n"), "{sql}");
        assert!(!sql.contains("<> 'pg_catalog'"), "{sql}");
        assert!(sql.contains("c.relname OPERATOR(pg_catalog.~) '^(film.*)$' COLLATE pg_catalog.default"), "{sql}");
        assert_eq!(none, "Did not find any tables named \"film*\".");
        // Combined kinds, + columns, and the index's table.
        let (sql, none) = list("\\dti+ public.*");
        assert!(sql.contains("c2.relname as \"Table\""), "{sql}");
        assert!(sql.contains("as \"Persistence\"") && sql.contains("as \"Access method\""), "{sql}");
        assert!(sql.contains("n.nspname OPERATOR(pg_catalog.~) '^(public)$'"), "{sql}");
        assert!(!sql.contains("pg_table_is_visible"), "{sql}");
        assert_eq!(none, "Did not find any relations named \"public.*\".");
        // Views have no access method; S lifts the system-schema filter.
        let (sql, none) = list("\\dvS");
        assert!(!sql.contains("am.amname") && !sql.contains("<> 'pg_catalog'"), "{sql}");
        assert_eq!(none, "Did not find any views.");
        assert_eq!(list("\\dE").1, "Did not find any foreign tables.");
        assert_eq!(list("\\dm \"Mat\"").1, "Did not find any materialized views named \"\"Mat\"\".");
        // psql's own refusals.
        let refusal = |input: &str| match plane.meta_cmd_in(input, &ctx) {
            Some(MetaAction::Output(lines)) => lines.join(" "),
            _ => panic!("{input}: unexpected action"),
        };
        assert_eq!(refusal("\\dt a.b.c.d"), "improper qualified name (too many dotted names): a.b.c.d");
        assert_eq!(refusal("\\dt nope.public.film"), "cross-database references are not implemented: nope.public.film");
        assert!(list("\\dt pagila.public.film").0.contains("'^(film)$'"));
        // Not listTables: \dn, \df, \dx …
        assert!(["dn", "dx", "df"].iter().all(|cmd| lists::list_tables_types(cmd).is_none()));
    }

    #[test]
    fn split_lineno_follows_psql() {
        assert_eq!(split_lineno("f"), Ok(("f", None)));
        assert_eq!(split_lineno("f 3"), Ok(("f", Some(3))));
        assert_eq!(split_lineno("public.f(int, text)3"), Ok(("public.f(int, text)", Some(3))));
        assert_eq!(split_lineno("f(int) 12 "), Ok(("f(int)", Some(12))));
        assert_eq!(split_lineno("f3"), Ok(("f3", None)));
        assert_eq!(split_lineno("42"), Ok(("42", None)));
        assert_eq!(split_lineno(""), Ok(("", None)));
        assert_eq!(split_lineno("f 0"), Err("invalid line number: 0".into()));
    }

    #[test]
    fn ef_and_ev_look_up_like_psql_or_open_its_templates() {
        let plane = PostgresPlane;
        let sql = |input: &str| match plane.meta_cmd(input) {
            Some(MetaAction::Introspect { sql, tag }) if tag == "edit_to_input" => sql,
            _ => panic!("{input}: not an edit lookup"),
        };
        let s = sql("\\ef film_in_stock");
        assert!(s.contains("'film_in_stock'::pg_catalog.regproc)") && s.contains("NULL::int AS \"Line\""), "{s}");
        let s = sql("\\ef public.f(int, text) 2");
        assert!(s.contains("'public.f(int, text)'::pg_catalog.regprocedure") && s.contains("2 + coalesce("), "{s}");
        assert!(sql("\\ef o'brien").contains("'o''brien'::"));
        let s = sql("\\ev public.sales_by_store 4");
        assert!(s.contains("'public.sales_by_store'::pg_catalog.regclass") && s.contains("4::int AS \"Line\""), "{s}");
        assert!(s.contains("is not a view"), "{s}");
        match plane.meta_cmd("\\ef") {
            Some(MetaAction::EditExternal { seed: Some(t), line: None }) => assert!(t.starts_with("CREATE FUNCTION ( )\n")),
            _ => panic!("\\ef without a name opens the template"),
        }
        match plane.meta_cmd("\\ev") {
            Some(MetaAction::EditExternal { seed: Some(t), .. }) => assert_eq!(t, "CREATE VIEW  AS\n SELECT \n  -- something...\n"),
            _ => panic!("\\ev without a name opens the template"),
        }
        assert!(matches!(plane.meta_cmd("\\ef f 0"), Some(MetaAction::Output(l)) if l[0].contains("invalid line number")));
    }

    #[test]
    fn tail_line_gdesc_dispatches_gdesc() {
        let plane = PostgresPlane;
        assert!(matches!(
            plane.meta_cmd("SELECT 1\n\\gdesc"),
            Some(MetaAction::Gdesc),
        ));
    }

    #[test]
    fn tail_line_gset_bare_uses_empty_prefix() {
        let plane = PostgresPlane;
        if let Some(MetaAction::Gset { prefix }) = plane.meta_cmd("SELECT 1\n\\gset") {
            assert_eq!(prefix, "");
        } else {
            panic!("expected MetaAction::Gset");
        }
    }

    #[test]
    fn tail_line_gset_with_prefix() {
        let plane = PostgresPlane;
        if let Some(MetaAction::Gset { prefix }) = plane.meta_cmd("SELECT 1\n\\gset my_") {
            assert_eq!(prefix, "my_");
        } else {
            panic!("expected MetaAction::Gset with prefix 'my_'");
        }
    }

    #[test]
    fn tail_line_gset_no_whitespace_does_not_misfire() {
        // `\gsetfoo` is NOT a `\gset` with prefix "foo" — psql rejects
        // it as unknown. The whitespace-or-EOL guard prevents the
        // mis-match.
        let plane = PostgresPlane;
        assert!(!matches!(
            plane.meta_cmd("SELECT 1\n\\gsetfoo"),
            Some(MetaAction::Gset { .. }),
        ));
    }

    #[test]
    fn tail_line_crosstabview_no_args() {
        let plane = PostgresPlane;
        if let Some(MetaAction::Crosstabview { col_v, col_h, col_d, sortcol_h })
            = plane.meta_cmd("SELECT a, b, c FROM t\n\\crosstabview")
        {
            assert!(col_v.is_none());
            assert!(col_h.is_none());
            assert!(col_d.is_none());
            assert!(sortcol_h.is_none());
        } else {
            panic!("expected MetaAction::Crosstabview");
        }
    }

    #[test]
    fn tail_line_crosstabview_with_args() {
        let plane = PostgresPlane;
        if let Some(MetaAction::Crosstabview { col_v, col_h, col_d, sortcol_h })
            = plane.meta_cmd("SELECT a, b, c FROM t\n\\crosstabview a b c")
        {
            assert_eq!(col_v.as_deref(), Some("a"));
            assert_eq!(col_h.as_deref(), Some("b"));
            assert_eq!(col_d.as_deref(), Some("c"));
            assert!(sortcol_h.is_none());
        } else {
            panic!("expected MetaAction::Crosstabview with three positional args");
        }
    }

    #[test]
    fn tail_line_crosstabview_reads_four_columns() {
        // psql reads four; a fifth is its extra argument
        // (`extra_arguments`), not an error.
        let plane = PostgresPlane;
        assert!(matches!(
            plane.meta_cmd("SELECT a, b, c FROM t\n\\crosstabview a b c d e"),
            Some(MetaAction::Crosstabview { sortcol_h: Some(d), .. }) if d == "d",
        ));
        assert_eq!(plane.extra_arguments("\\crosstabview a b c d e"), ["\\crosstabview: extra argument \"e\" ignored"]);
    }

    #[test]
    fn tail_line_crosstabview_no_whitespace_does_not_misfire() {
        // Same guard as \gset: `\crosstabview2` shouldn't parse into
        // Crosstabview with arg "2".
        let plane = PostgresPlane;
        assert!(!matches!(
            plane.meta_cmd("SELECT 1\n\\crosstabview2"),
            Some(MetaAction::Crosstabview { .. }),
        ));
    }

    // ── PG quote helpers ──────────────────────────────────────────────
    //
    // These back `\password`'s ALTER USER emission. A regression here
    // would land bad SQL on the wire — covered by tests at the helper
    // surface rather than from inside shell.rs's reactive closures.

    #[test]
    fn pg_quote_ident_wraps_in_double_quotes() {
        assert_eq!(pg_quote_ident("alice"), "\"alice\"");
    }

    #[test]
    fn pg_quote_ident_doubles_embedded_double_quote() {
        assert_eq!(pg_quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn pg_quote_ident_preserves_case_and_spaces() {
        assert_eq!(pg_quote_ident("John Doe"), "\"John Doe\"");
    }

    #[test]
    fn pg_quote_ident_empty_yields_empty_quoted_ident() {
        // PG-legal — an empty quoted identifier IS valid syntax even
        // if semantically odd. The helper doesn't get to decide; it
        // just quotes.
        assert_eq!(pg_quote_ident(""), "\"\"");
    }

    #[test]
    fn pg_quote_literal_wraps_in_single_quotes() {
        assert_eq!(pg_quote_literal("hello"), "'hello'");
    }

    #[test]
    fn pg_quote_literal_doubles_embedded_single_quote() {
        assert_eq!(pg_quote_literal("that's"), "'that''s'");
    }

    #[test]
    fn pg_quote_literal_handles_scram_hash_format_without_change() {
        // SCRAM hashes contain `$` and `:` and base64 chars but
        // never `'`. The helper should round-trip the hash verbatim
        // inside the surrounding quotes.
        let hash = "SCRAM-SHA-256$4096:AAAAAAAAAAAAAAAAAAAAAA==$abc:def";
        assert_eq!(
            pg_quote_literal(hash),
            format!("'{}'", hash),
        );
    }

    // ── Storage-tab SQL: extension and version safety ─────────────────
    //
    // PG resolves function names at parse time, so an extension
    // function may only appear in SQL built for a session that has the
    // extension (PR #107 shipped a CASE-gated call that errored on
    // every server without it).

    #[test]
    fn vm_sql_without_pg_visibility_never_names_the_function() {
        let sql = PostgresPlane.table_vm_sql("public", "t", &[]);
        assert!(!sql.contains("pg_visibility_map_summary"), "{sql}");
        assert!(sql.contains("relallvisible"));
    }

    #[test]
    fn vm_sql_with_pg_visibility_uses_the_live_read() {
        let sql = PostgresPlane.table_vm_sql("public", "t", &["pg_visibility".to_string()]);
        assert!(sql.contains("pg_visibility_map_summary"));
        assert!(sql.contains("pg_relation_size"), "total must be the live size: {sql}");
    }

    #[test]
    fn vm_sql_reads_version_dependent_columns_through_jsonb() {
        // relallfrozen exists only on PG 18+; a bare reference would be
        // a parse error on 12-17.
        let sql = PostgresPlane.table_vm_sql("public", "t", &[]);
        assert!(sql.contains("to_jsonb(c) ->> 'relallfrozen'"), "{sql}");
        assert!(!sql.contains("c.relallfrozen"), "{sql}");
    }

    #[test]
    fn autovac_sql_reads_version_dependent_columns_through_jsonb() {
        let sql = PostgresPlane.table_autovac_sql("public", "t");
        assert!(sql.contains("to_jsonb(st) ->> 'n_ins_since_vacuum'"), "{sql}");
        assert!(!sql.contains("st.n_ins_since_vacuum"), "{sql}");
        assert!(!sql.contains("c.relallfrozen"), "{sql}");
    }
}
