//! A server error as psql prints it — libpq's `pqBuildErrorMessage3`
//! and `reportErrorPosition` (fe-protocol3.c): severity, SQLSTATE (when
//! verbose), primary message, the `LINE n:` excerpt with a caret under
//! the error position, then DETAIL / HINT / QUERY / CONTEXT and, when
//! verbose, the object names and LOCATION. psql's `VERBOSITY` and
//! `SHOW_CONTEXT` choose how much (`ErrorStyle`).

use shared::DbError;
use unicode_width::UnicodeWidthChar;

use crate::plane::{ErrorStyle, ShowContext, Verbosity};

/// The error's lines, as psql prints them. `query` is the statement
/// the error's position counts into (absent for a meta-command's own
/// queries, which psql doesn't excerpt either).
pub(super) fn psql_error(e: &DbError, query: Option<&str>, style: ErrorStyle) -> Vec<String> {
    let mut verbosity = style.verbosity;
    if verbosity == Verbosity::Minimal {
        let message = if e.message.is_empty() { "(not available)" } else { &e.message };
        return vec![format!("{}:  {message}", e.severity.as_deref().unwrap_or("ERROR"))];
    }
    let mut msg = String::new();
    if let Some(severity) = &e.severity {
        msg.push_str(&format!("{severity}:  "));
    }
    if verbosity == Verbosity::Sqlstate {
        // The SQLSTATE and nothing else; without one, terse.
        if let Some(code) = &e.sqlstate {
            msg.push_str(code);
            return lines(&msg);
        }
        verbosity = Verbosity::Terse;
    }
    if verbosity == Verbosity::Verbose {
        if let Some(code) = &e.sqlstate { msg.push_str(&format!("{code}: ")); }
    }
    msg.push_str(&e.message);

    // Where the error is: a caret under the statement (or the query a
    // function ran), or " at character N" when terse or the text is
    // unknown.
    let mut cursor: Option<(&str, u32)> = None;
    if let Some(at) = e.position {
        match query.filter(|_| verbosity != Verbosity::Terse) {
            Some(q) => cursor = Some((q, at)),
            None => msg.push_str(&format!(" at character {at}")),
        }
    } else if let Some(at) = e.internal_position {
        match e.internal_query.as_deref().filter(|_| verbosity != Verbosity::Terse) {
            Some(q) => cursor = Some((q, at)),
            None => msg.push_str(&format!(" at character {at}")),
        }
    }
    msg.push('\n');

    if verbosity != Verbosity::Terse {
        if let Some((q, at)) = cursor.filter(|(_, at)| *at > 0) {
            if let Some(excerpt) = error_position(q, at as usize) {
                msg.push_str(&excerpt);
            }
        }
        for (label, value) in [("DETAIL", &e.detail), ("HINT", &e.hint), ("QUERY", &e.internal_query)] {
            if let Some(v) = value { msg.push_str(&format!("{label}:  {v}\n")); }
        }
        // Every error we show is a failed command, so `errors` shows it.
        if style.show_context != ShowContext::Never {
            if let Some(context) = &e.context { msg.push_str(&format!("CONTEXT:  {context}\n")); }
        }
    }
    if verbosity == Verbosity::Verbose {
        for (label, value) in [
            ("SCHEMA NAME", &e.schema), ("TABLE NAME", &e.table), ("COLUMN NAME", &e.column),
            ("DATATYPE NAME", &e.datatype), ("CONSTRAINT NAME", &e.constraint),
        ] {
            if let Some(v) = value { msg.push_str(&format!("{label}:  {v}\n")); }
        }
        if e.routine.is_some() || e.file.is_some() || e.line.is_some() {
            msg.push_str("LOCATION:  ");
            if let Some(routine) = &e.routine { msg.push_str(&format!("{routine}, ")); }
            if let (Some(file), Some(line)) = (&e.file, e.line) { msg.push_str(&format!("{file}:{line}")); }
            msg.push('\n');
        }
    }
    lines(&msg)
}

fn lines(msg: &str) -> Vec<String> {
    msg.trim_end_matches('\n').split('\n').map(String::from).collect()
}

/// psql's excerpt around an error: `LINE n: <the line>` and a caret
/// under character `position` (1-based), the line cut to 60 columns
/// with `...` where it was cut, at least 10 columns kept right of the
/// caret. Tabs show as spaces. None when the position is past the end.
fn error_position(query: &str, position: usize) -> Option<String> {
    const DISPLAY_SIZE: usize = 60;
    const MIN_RIGHT_CUT: usize = 10;

    let loc = position.checked_sub(1)?;
    let chars: Vec<char> = query.chars().map(|c| if c == '\t' { ' ' } else { c }).collect();
    // Each character's starting screen column, up to the end of the
    // error's line (plus one past it).
    let mut col = Vec::with_capacity(chars.len() + 1);
    let mut screen = 0;
    let mut line_no = 1;
    let mut begin = 0;
    let mut end = None;
    for (i, &c) in chars.iter().enumerate() {
        col.push(screen);
        if c == '\r' || c == '\n' {
            if i < loc {
                // `\r\n` is one line break.
                if c == '\r' || i == 0 || chars[i - 1] != '\r' { line_no += 1; }
                begin = i + 1;
            } else {
                end = Some(i);
                break;
            }
        }
        // Control characters, like zero-width ones, take a column.
        screen += c.width().filter(|w| *w > 0).unwrap_or(1);
    }
    let mut end = end.unwrap_or_else(|| {
        col.push(screen);
        chars.len()
    });
    if loc > end {
        return None;
    }

    let (mut cut_left, mut cut_right) = (false, false);
    if col[end] - col[begin] > DISPLAY_SIZE {
        if col[begin] + DISPLAY_SIZE >= col[loc] + MIN_RIGHT_CUT {
            while col[end] - col[begin] > DISPLAY_SIZE { end -= 1; }
            cut_right = true;
        } else {
            while col[loc] + MIN_RIGHT_CUT < col[end] {
                end -= 1;
                cut_right = true;
            }
            while col[end] - col[begin] > DISPLAY_SIZE {
                begin += 1;
                cut_left = true;
            }
        }
    }

    let prefix = format!("LINE {line_no}: {}", if cut_left { "..." } else { "" });
    let excerpt: String = chars[begin..end].iter().collect();
    let caret = prefix.chars().count() + col[loc] - col[begin];
    Some(format!(
        "{prefix}{excerpt}{}\n{}^\n",
        if cut_right { "..." } else { "" },
        " ".repeat(caret),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: ErrorStyle = ErrorStyle { verbosity: Verbosity::Default, show_context: ShowContext::Errors };

    fn missing_table() -> DbError {
        DbError {
            severity: Some("ERROR".into()),
            sqlstate: Some("42P01".into()),
            message:  "relation \"nosuch\" does not exist".into(),
            position: Some(15),
            file:     Some("parse_relation.c".into()),
            line:     Some(1469),
            routine:  Some("parserOpenTable".into()),
            ..DbError::default()
        }
    }

    /// Each expected block was captured from psql 18 against PG 18.
    #[test]
    fn prints_what_psql_prints() {
        assert_eq!(psql_error(&missing_table(), Some("select * from nosuch;"), DEFAULT), [
            "ERROR:  relation \"nosuch\" does not exist",
            "LINE 1: select * from nosuch;",
            "                      ^",
        ]);
        let verbose = ErrorStyle { verbosity: Verbosity::Verbose, show_context: ShowContext::Always };
        assert_eq!(psql_error(&missing_table(), Some("select * from nosuch;"), verbose), [
            "ERROR:  42P01: relation \"nosuch\" does not exist",
            "LINE 1: select * from nosuch;",
            "                      ^",
            "LOCATION:  parserOpenTable, parse_relation.c:1469",
        ]);
        let terse = ErrorStyle { verbosity: Verbosity::Terse, ..DEFAULT };
        assert_eq!(psql_error(&missing_table(), Some("select * from nosuch;"), terse),
                   ["ERROR:  relation \"nosuch\" does not exist at character 15"]);
        let sqlstate = ErrorStyle { verbosity: Verbosity::Sqlstate, ..DEFAULT };
        assert_eq!(psql_error(&missing_table(), None, sqlstate), ["ERROR:  42P01"]);
        // psql's \ef / \ev lookups: severity and message, nothing else.
        let minimal = ErrorStyle { verbosity: Verbosity::Minimal, ..DEFAULT };
        assert_eq!(psql_error(&missing_table(), Some("select 'x'::regproc"), minimal),
                   ["ERROR:  relation \"nosuch\" does not exist"]);

        // An error inside a function: the caret goes under its query.
        let internal = DbError {
            severity:          Some("ERROR".into()),
            sqlstate:          Some("42P01".into()),
            message:           "relation \"nosuch_internal\" does not exist".into(),
            internal_position: Some(15),
            internal_query:    Some("SELECT * FROM nosuch_internal".into()),
            context:           Some("PL/pgSQL function inline_code_block line 1 at PERFORM".into()),
            ..DbError::default()
        };
        assert_eq!(psql_error(&internal, Some("DO $$ BEGIN PERFORM * FROM nosuch_internal; END $$"), DEFAULT), [
            "ERROR:  relation \"nosuch_internal\" does not exist",
            "LINE 1: SELECT * FROM nosuch_internal",
            "                      ^",
            "QUERY:  SELECT * FROM nosuch_internal",
            "CONTEXT:  PL/pgSQL function inline_code_block line 1 at PERFORM",
        ]);
        let quiet = ErrorStyle { show_context: ShowContext::Never, ..DEFAULT };
        assert!(!psql_error(&internal, None, quiet).iter().any(|l| l.starts_with("CONTEXT")));

        let raised = DbError {
            severity: Some("ERROR".into()),
            message:  "boom".into(),
            detail:   Some("the detail".into()),
            hint:     Some("a hint".into()),
            ..DbError::default()
        };
        assert_eq!(psql_error(&raised, None, DEFAULT), ["ERROR:  boom", "DETAIL:  the detail", "HINT:  a hint"]);
    }

    #[test]
    fn the_excerpt_follows_lines_and_cuts_long_ones() {
        // Second line of a multi-line statement.
        assert_eq!(error_position("select 1,\n  nosuch\nfrom t", 13).as_deref(),
                   Some("LINE 2:   nosuch\n          ^\n"));
        // `\r\n` counts once.
        assert_eq!(error_position("select\r\nx", 9).as_deref(), Some("LINE 2: x\n        ^\n"));
        // Past the end: nothing.
        assert_eq!(error_position("select 1", 20), None);
        // A long line keeps 10 columns right of the caret and cuts the left.
        let long = format!("select {} from nosuch_table_name_here", "a, ".repeat(30));
        let at = long.find("nosuch").unwrap() + 1;
        let lines: Vec<String> = error_position(&long, at).unwrap().lines().map(String::from).collect();
        assert!(lines[0].starts_with("LINE 1: ...") && lines[0].ends_with("..."), "{lines:?}");
        assert_eq!(lines[1].len() - 1, lines[0].find("nosuch").unwrap(), "{lines:?}");
    }
}
