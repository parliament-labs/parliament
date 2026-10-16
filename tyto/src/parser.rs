//! SQL as a CLI's lexer reads it — which text is code and which is
//! quoted or commented — for splitting an input into statements and for
//! finding where a backslash command starts; and psql's `:var`
//! substitution.

use std::ops::Range;

use crate::types::TypedStatement;

/// How a CLI's lexer reads SQL: what's quoted or commented, so neither
/// ends a statement nor starts a backslash command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lexicon {
    /// psql's (`psqlscan.l`): `'…'` (`''` inside), `E'…'` with backslash
    /// escapes too, `"…"`, `$tag$ … $tag$`, `--` to the end of the line,
    /// `/* … */` nested.
    Psql,
    /// The mysql CLI's (`add_line`): `'…'` and `"…"` with backslash
    /// escapes, `` `…` ``, `#` and `-- ` to the end of the line, and
    /// `/* … */` — but not `/*!` or `/*+` (or MariaDB's `/*M!`), which
    /// the server runs. `\g` and `\G` end a statement as the delimiter
    /// does.
    Mysql,
}

impl Lexicon {
    /// `#` starts a comment (MySQL).
    pub(crate) fn hash_comments(self) -> bool { self == Lexicon::Mysql }
}

/// What a stretch of SQL is to the lexer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    /// A character of code.
    Code,
    /// A quoted string or identifier, quotes included.
    Quoted,
    /// A comment.
    Comment,
}

/// The pieces of `text` in order, each with the bytes it covers: a
/// quoted string or a comment whole (to the end of `text` when it isn't
/// closed), code a character at a time.
pub(crate) struct Pieces<'a> {
    text:    &'a str,
    lexicon: Lexicon,
    at:      usize,
}

impl<'a> Pieces<'a> {
    pub(crate) fn new(text: &'a str, lexicon: Lexicon) -> Self {
        Pieces { text, lexicon, at: 0 }
    }

    /// Go on from byte `at` (a character boundary), past what was
    /// read there as code — a terminator.
    fn resume_at(&mut self, at: usize) {
        self.at = at;
    }
}

impl Iterator for Pieces<'_> {
    type Item = (Range<usize>, Piece);

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.at;
        let rest = self.text.get(start..).filter(|r| !r.is_empty())?;
        let (len, piece) = match self.lexicon {
            Lexicon::Psql => psql_piece(self.text, start),
            Lexicon::Mysql => mysql_piece(rest),
        }
        .unwrap_or_else(|| (rest.chars().next().map_or(1, char::len_utf8), Piece::Code));
        self.at = start + len;
        Some((start..self.at, piece))
    }
}

/// The quoted string or comment psql's lexer reads at byte `at` of
/// `text`, and its length.
fn psql_piece(text: &str, at: usize) -> Option<(usize, Piece)> {
    let rest = &text[at..];
    let b = rest.as_bytes();
    let prev = text[..at].chars().next_back();
    // A token starts here — not inside an identifier or a number.
    let token_start = !prev.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$');
    match b[0] {
        b'-' if b.get(1) == Some(&b'-') => Some((line_end(rest), Piece::Comment)),
        b'/' if b.get(1) == Some(&b'*') => Some((nested_comment_end(rest), Piece::Comment)),
        b'\'' => {
            // `E'…'` — the E read just before — takes backslash escapes.
            let escapes = at > 0 && matches!(text.as_bytes()[at - 1], b'E' | b'e')
                && !text[..at - 1].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$');
            Some((quote_end(rest, b'\'', escapes), Piece::Quoted))
        }
        b'"' => Some((quote_end(rest, b'"', false), Piece::Quoted)),
        b'$' if token_start => dollar_quote_end(rest).map(|len| (len, Piece::Quoted)),
        _ => None,
    }
}

/// The quoted string or comment the mysql CLI reads at the start of
/// `rest`, and its length — or, outside them, a backslash and the
/// character it takes: `\'` opens no string, `\;` ends nothing.
fn mysql_piece(rest: &str) -> Option<(usize, Piece)> {
    let b = rest.as_bytes();
    match b[0] {
        b'\\' => Some((1 + rest[1..].chars().next().filter(|&c| c != '\n').map_or(0, char::len_utf8), Piece::Code)),
        b'#' => Some((line_end(rest), Piece::Comment)),
        // `--` comments only before whitespace (or nothing): `--1` is
        // minus minus one.
        b'-' if b.get(1) == Some(&b'-')
            && b.get(2).is_none_or(|c| c.is_ascii_whitespace() || c.is_ascii_control()) =>
            Some((line_end(rest), Piece::Comment)),
        b'/' if b.get(1) == Some(&b'*') && !matches!(b.get(2), Some(b'!' | b'+'))
            && !rest[2..].starts_with("M!") =>
            Some((rest[2..].find("*/").map_or(rest.len(), |end| end + 4), Piece::Comment)),
        q @ (b'\'' | b'"') => Some((quote_end(rest, q, true), Piece::Quoted)),
        b'`' => Some((quote_end(rest, b'`', false), Piece::Quoted)),
        _ => None,
    }
}

/// Up to the end of the line (the newline is the next piece).
fn line_end(rest: &str) -> usize {
    rest.find('\n').unwrap_or(rest.len())
}

/// Past the `*/` closing the `/*` that `rest` starts with — nested ones
/// closed first, as Postgres reads them.
fn nested_comment_end(rest: &str) -> usize {
    let b = rest.as_bytes();
    let (mut depth, mut i) = (0usize, 0);
    while i + 1 < b.len() {
        match (b[i], b[i + 1]) {
            (b'/', b'*') => { depth += 1; i += 2; }
            (b'*', b'/') => {
                depth -= 1;
                i += 2;
                if depth == 0 { return i; }
            }
            _ => i += 1,
        }
    }
    rest.len()
}

/// Past the quote closing the one `rest` starts with: a doubled quote
/// stays inside, and with `escapes` a backslash keeps the next
/// character in.
fn quote_end(rest: &str, quote: u8, escapes: bool) -> usize {
    let b = rest.as_bytes();
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'\\' if escapes => i += 2,
            c if c == quote => {
                if b.get(i + 1) == Some(&quote) { i += 2; } else { return i + 1; }
            }
            _ => i += 1,
        }
    }
    rest.len()
}

/// Past the `$tag$` closing the one `rest` starts with, when it starts
/// with one (`$$`, `$body$` — a tag doesn't start with a digit: `$1` is
/// a parameter).
fn dollar_quote_end(rest: &str) -> Option<usize> {
    let tag_len = rest[1..].find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    let tag = &rest[1..1 + tag_len];
    if rest.as_bytes()[1 + tag_len] != b'$' || tag.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let open = &rest[..tag_len + 2];
    Some(rest[open.len()..].find(open).map_or(rest.len(), |end| open.len() + end + open.len()))
}

/// An input's statements, each with the terminator it was typed with
/// (`split_statements`), and whether the input ends one — its last
/// statement ended, so nothing waits for more. `pending`: where the
/// text no statement has ended starts (the input's length when all
/// have).
#[derive(Debug, PartialEq)]
pub(crate) struct Statements {
    pub list:    Vec<TypedStatement>,
    pub ended:   bool,
    pub pending: usize,
}

impl Statements {
    /// Whether Enter runs them: the input ended a statement, or one
    /// before the last was ended by its terminator — then the unended
    /// last runs too (`SELECT 1; SELECT 2`). A comment line ahead of an
    /// unfinished statement doesn't count: it waits with it.
    pub(crate) fn run_on_enter(&self) -> bool {
        self.ended || self.list.iter().rev().skip(1).any(|st| !st.terminator.is_empty())
    }
}

/// The statements of `sql`, each with the terminator it was typed with
/// — `delimiter`, or on MySQL `\g` / `\G` (vertical), wherever they're
/// code — the last with none when it wasn't ended. Each is trimmed; a
/// terminator that ends nothing (`;;`) gives an empty one, which the
/// mysql CLI answers and psql sends (`DbmsPlane::empty_statement`).
///
/// MySQL's line comments go where the mysql CLI (`--comments`, on by
/// default) puts them: one that starts a line with nothing pending is a
/// statement of its own, ended by the line (`-- note` alone is sent: the
/// server answers "Query OK"); one after a statement's terminator on the
/// same line goes with that statement (`select 1; -- why`); anywhere
/// else — indented, or inside a statement — it's part of the statement.
pub(crate) fn split_statements(sql: &str, delimiter: &str, lexicon: Lexicon) -> Statements {
    let mut stmts: Vec<TypedStatement> = Vec::new();
    let mut start = 0;
    let mut ended_by_line = false;
    let mut pieces = Pieces::new(sql, lexicon);
    while let Some((at, piece)) = pieces.next() {
        if piece == Piece::Comment && lexicon == Lexicon::Mysql && !sql[at.clone()].starts_with("/*") {
            let gap = &sql[start..at.start];
            if !gap.trim().is_empty() {
                continue;
            }
            let comment = sql[at.clone()].trim_end();
            match stmts.last_mut() {
                // After a statement on the same line: that statement's.
                Some(last) if start > 0 && !gap.contains('\n') && !ended_by_line => {
                    last.terminator.push_str(gap);
                    last.terminator.push_str(comment);
                }
                // At the start of a line, nothing pending: its own.
                _ if gap.is_empty() || gap.ends_with('\n') => {
                    stmts.push(TypedStatement::new(comment, ""));
                    ended_by_line = true;
                }
                // Indented: the next statement's.
                _ => continue,
            }
            start = at.end;
            continue;
        }
        if piece != Piece::Code { continue; }
        let rest = &sql[at.start..];
        let (len, vertical) = if !delimiter.is_empty() && rest.starts_with(delimiter) {
            (delimiter.len(), false)
        } else if lexicon == Lexicon::Mysql && (rest.starts_with("\\g") || rest.starts_with("\\G")) {
            (2, rest.starts_with("\\G"))
        } else {
            continue;
        };
        let stmt = sql[start..at.start].trim();
        // A statement ending in a line comment had its terminator on the
        // next line: kept there, or the comment would swallow it when
        // the statement comes back from history.
        let terminator = if ends_in_line_comment(stmt, lexicon) { format!("\n{}", &rest[..len]) } else { rest[..len].to_string() };
        stmts.push(TypedStatement::new(stmt, terminator).shown(vertical, None));
        ended_by_line = false;
        start = at.start + len;
        pieces.resume_at(start);
    }
    let last = sql[start..].trim();
    if !last.is_empty() {
        stmts.push(TypedStatement::new(last, ""));
        return Statements { list: stmts, ended: false, pending: start };
    }
    let ended = ended_by_line || stmts.last().is_some_and(|st| !st.terminator.is_empty());
    Statements { list: stmts, ended, pending: sql.len() }
}

/// A place in an input where a command may start, as the CLIs read
/// their input (`command_starts`): a backslash that's code; the start of
/// a line typed while no statement is pending — where the mysql CLI
/// reads its named commands (`use db`, `delimiter //`); or a statement
/// its delimiter ends, `start..end` without the delimiter — where the
/// mysql CLI reads them again (`status;`, `select 1; use db;`). Which of
/// them start a command is the plane's to say.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CommandStart {
    Backslash(usize),
    Line(usize),
    Statement { start: usize, end: usize },
}

/// The places in `sql` a command may start, in reading order — a
/// statement's at its delimiter, after any inside it. A terminator —
/// `delimiter`, or the mysql CLI's `\g` / `\G`, which end a statement
/// rather than start a command — leaves nothing pending; comments make
/// nothing pending, but for psql's `/* */`, which its query buffer keeps
/// (`past_leading_line_comments`).
pub(crate) fn command_starts(sql: &str, delimiter: &str, lexicon: Lexicon) -> Vec<CommandStart> {
    let mut starts = vec![CommandStart::Line(0)];
    let mut pending = false;
    // Where the statement being read starts.
    let mut statement = 0;
    let mut pieces = Pieces::new(sql, lexicon);
    while let Some((at, piece)) = pieces.next() {
        let text = &sql[at.clone()];
        match piece {
            Piece::Comment => pending |= lexicon == Lexicon::Psql && text.starts_with("/*"),
            Piece::Quoted => pending = true,
            Piece::Code if !delimiter.is_empty() && sql[at.start..].starts_with(delimiter) => {
                starts.push(CommandStart::Statement { start: statement, end: at.start });
                pending = false;
                statement = at.start + delimiter.len();
                pieces.resume_at(statement);
            }
            Piece::Code if lexicon == Lexicon::Mysql && matches!(text, "\\g" | "\\G") => {
                pending = false;
                statement = at.end;
            }
            Piece::Code if text.starts_with('\\') => {
                starts.push(CommandStart::Backslash(at.start));
                pending = true;
            }
            Piece::Code if text == "\n" => if !pending {
                starts.push(CommandStart::Line(at.end));
                statement = at.end;
            },
            Piece::Code => pending |= !text.trim().is_empty(),
        }
    }
    starts
}

/// Whether `text` ends inside a quoted string or identifier, or a block
/// comment — where psql's prompt reads `'`, `"`, `$` or `*` and the next
/// line goes on inside it.
pub(crate) fn ends_inside_quote_or_comment(text: &str, lexicon: Lexicon) -> bool {
    // What `text` left open swallows a line break after it.
    let next_line = format!("{text}\n");
    Pieces::new(&next_line, lexicon).last()
        .is_some_and(|(at, piece)| piece != Piece::Code && at.contains(&text.len()))
}

/// Whether `stmt` ends inside a line comment (`select 1 -- why`).
fn ends_in_line_comment(stmt: &str, lexicon: Lexicon) -> bool {
    Pieces::new(stmt, lexicon).last()
        .is_some_and(|(at, piece)| piece == Piece::Comment && !stmt[at].starts_with("/*"))
}

/// Where the backslash command on the last line of `input` starts, as
/// psql and the mysql CLI read a line: psql's, at the first backslash
/// there that's code — not quoted or commented, though the quote or
/// comment may have opened lines before; the mysql CLI's, at the first
/// that's one of its commands — `\N` (NULL), and `\` with a character
/// it has no command for, stay in the statement
/// (`mysql_unknown_commands`). The text before it is the query buffer
/// the command works on (`select 1 \gx`).
pub(crate) fn trailing_command(input: &str, lexicon: Lexicon) -> Option<usize> {
    line_backslashes(input, lexicon)
        .find(|&(_, next)| lexicon == Lexicon::Psql || next.is_some_and(|c| mysql_command(c).is_some()))
        .map(|(at, _)| at)
}

/// What a backslash and a character are to the mysql CLI.
#[derive(Clone, Copy, Debug, PartialEq)]
enum MysqlCommand {
    /// A command on its own (`\c`, `\G`): the line reads on after it.
    Bare,
    /// A command that reads its arguments from the rest of the line
    /// (`\u db`), ending at the delimiter after them (`mysql_command_len`).
    WithArguments,
}

/// The mysql CLI's command for `\` and `c` (`find_command`; the
/// letters its `\?` lists), if it has one.
fn mysql_command(c: char) -> Option<MysqlCommand> {
    match c {
        'c' | 'e' | 'G' | 'g' | 'n' | 'p' | 'q' | 's' | 't' | 'W' | 'w' | 'x' | '#' => Some(MysqlCommand::Bare),
        '?' | 'C' | 'd' | 'h' | 'P' | 'R' | 'r' | 'T' | 'u' | '.' | '!' => Some(MysqlCommand::WithArguments),
        _ => None,
    }
}

/// The backslashes on the last line of `input` that are code — not
/// quoted or commented, though the quote or comment may have opened
/// lines before — each with the character after it (None at the line's
/// end). The mysql CLI's lexer reads that character with its backslash
/// (`mysql_piece`): `\\` is one.
fn line_backslashes(input: &str, lexicon: Lexicon) -> impl Iterator<Item = (usize, Option<char>)> + '_ {
    let line = input.rfind('\n').map_or(0, |newline| newline + 1);
    Pieces::new(input, lexicon)
        .filter(move |(at, piece)| *piece == Piece::Code && at.start >= line && input.as_bytes()[at.start] == b'\\')
        .map(move |(at, _)| (at.start, input[at.start + 1..].chars().next().filter(|&c| c != '\n')))
}

/// Whether `\` and `c` are one of the mysql CLI's commands.
pub(crate) fn is_mysql_command(c: char) -> bool {
    mysql_command(c).is_some()
}

/// How much of `command` — text that starts with a backslash — the
/// mysql CLI's command takes (`add_line`): `\` and its letter, the line
/// reading on after it (`\s from dual;` — ` from dual;` is SQL); or for
/// one with arguments, up to the first `delimiter` after its letter on
/// its line, that included — the line reads on after it (`\u db; select
/// 1;`) — or else the line. `\d` looks for the delimiter it sets (`\d //
/// select 1 //`). The command reads its arguments from the whole line,
/// past that delimiter (`\u db;x` is database `db;x`). None: `\` and
/// that character are no command of its.
pub(crate) fn mysql_command_len(command: &str, delimiter: &str) -> Option<usize> {
    let letter = command.strip_prefix('\\')?.chars().next()?;
    let from = 1 + letter.len_utf8();
    Some(match mysql_command(letter)? {
        MysqlCommand::Bare => from,
        MysqlCommand::WithArguments => {
            let line = &command[..command.find('\n').unwrap_or(command.len())];
            let delimiter = mysql_delimiter_after(line, delimiter);
            line[from..].find(delimiter).map_or(line.len(), |at| from + at + delimiter.len())
        }
    })
}

/// The statement delimiter after the mysql command `command` (`\` and a
/// letter on): what `\d` sets — its first word — else `delimiter`.
fn mysql_delimiter_after<'a>(command: &'a str, delimiter: &'a str) -> &'a str {
    command.strip_prefix("\\d")
        .and_then(|arguments| arguments.lines().next().unwrap_or_default().split_whitespace().next())
        .unwrap_or(delimiter)
}

/// The characters of the backslash commands the mysql CLI doesn't know
/// on the last line of `input` (`\if` is `\i`), read as it reads the
/// line (`add_line`): left to right, quotes and comments skipped, `\N`
/// (NULL) no command, a command's arguments skipped as they are —
/// quotes and all — up to the `delimiter` that ends them
/// (`mysql_command_len`), the line read on after it. It says `Unknown
/// command '\i'.` for each and leaves it in the statement.
pub(crate) fn mysql_unknown_commands(input: &str, delimiter: &str) -> Vec<char> {
    let mut unknown = Vec::new();
    for (at, next) in line_backslashes(input, Lexicon::Mysql) {
        let Some(c) = next else { break };
        match mysql_command(c) {
            None if c != 'N' => unknown.push(c),
            Some(MysqlCommand::WithArguments) => {
                let command = &input[at..];
                let end = at + mysql_command_len(command, delimiter).unwrap_or_default();
                unknown.extend(mysql_unknown_commands(&input[end..], mysql_delimiter_after(command, delimiter)));
                break;
            }
            _ => {}
        }
    }
    unknown
}

/// What psql's query buffer keeps of `sql`: all of it but the line
/// comments and whitespace it starts with — a `--` comment typed while
/// the buffer is empty isn't added to it; a `/* */` one is, and so is a
/// line comment after SQL.
pub(crate) fn past_leading_line_comments(sql: &str, lexicon: Lexicon) -> &str {
    for (at, piece) in Pieces::new(sql, lexicon) {
        let text = &sql[at.clone()];
        let skipped = match piece {
            Piece::Comment => !text.starts_with("/*"),
            Piece::Code => text.chars().all(char::is_whitespace),
            Piece::Quoted => false,
        };
        if !skipped {
            return &sql[at.start..];
        }
    }
    ""
}

/// Whether `sql` holds nothing but comments and whitespace — no
/// statement, which neither psql nor the mysql CLI sends on its own. On
/// MySQL, `/*! … */` (MariaDB's `/*M! … */` too) isn't a comment — the
/// server runs it; every mysqldump starts `/*!40101 SET … */;` — and
/// `--` starts one only before whitespace (`--1` is minus minus one).
pub(crate) fn is_comment_only(sql: &str, lexicon: Lexicon) -> bool {
    Pieces::new(sql, lexicon).all(|(at, piece)| match piece {
        Piece::Comment => true,
        Piece::Quoted => false,
        Piece::Code => sql[at].chars().all(char::is_whitespace),
    })
}

/// A shell's variables (`\set`, and psql's own: `:USER`, `:DBNAME` …).
pub(crate) type Variables = std::collections::HashMap<String, String>;

/// A character of a variable's name, as psql's lexers read one
/// (`variable_char`: letters, digits, `_`, and anything past ASCII).
fn variable_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii()
}

/// A name psql takes for a variable (`valid_variable_name`): letters,
/// digits, `_` and anything past ASCII — nothing else, and not empty.
pub(crate) fn valid_variable_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(variable_char)
}

/// The variable reference `text` starts with — it starts with `:` — as
/// psql's lexers read one (psqlscan.l, psqlscanslash.l), and what it
/// becomes, with how many bytes it took: `:name` the value, `:'name'`
/// the value as a literal (`PQescapeLiteral`), `:"name"` as an
/// identifier (`PQescapeIdentifier`), `:{?name}` `TRUE` or `FALSE` for
/// whether it's set. A quoted reference to a variable that isn't set
/// stays as typed. None: nothing to replace — no reference, or `:name`
/// unset (the colon and the name read on as they are).
pub(crate) fn variable_reference(text: &str, vars: &Variables) -> Option<(usize, String)> {
    let rest = text.strip_prefix(':')?;
    let name_len = |s: &str| s.find(|c: char| !variable_char(c)).unwrap_or(s.len());
    if let Some(inner) = rest.strip_prefix("{?") {
        let n = name_len(inner);
        return (n > 0 && inner[n..].starts_with('}'))
            .then(|| (n + 4, if vars.contains_key(&inner[..n]) { "TRUE" } else { "FALSE" }.to_string()));
    }
    for (quote, escape) in [('\'', escape_literal as fn(&str) -> String), ('"', escape_identifier)] {
        if let Some(inner) = rest.strip_prefix(quote) {
            let n = name_len(inner);
            if n == 0 || !inner[n..].starts_with(quote) {
                return None;
            }
            let len = n + 3;
            return Some((len, vars.get(&inner[..n]).map_or_else(|| text[..len].to_string(), |value| escape(value))));
        }
    }
    let n = name_len(rest);
    vars.get(&rest[..n]).filter(|_| n > 0).map(|value| (n + 1, value.clone()))
}

/// A value as a SQL literal, as libpq's `PQescapeLiteral` writes it:
/// quotes doubled — and when it holds a backslash, those doubled too
/// in an escape string, ` E'…'`, the leading space included.
pub(crate) fn escape_literal(value: &str) -> String {
    let backslashes = value.contains('\\');
    let mut out = String::with_capacity(value.len() + 4);
    if backslashes {
        out.push_str(" E");
    }
    out.push('\'');
    for c in value.chars() {
        if c == '\'' || (backslashes && c == '\\') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// A value as a SQL identifier, as libpq's `PQescapeIdentifier` writes
/// it: double quotes, those inside doubled.
pub(crate) fn escape_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// psql's interpolation of a statement (psqlscan.l): every variable
/// reference in code — not in a quoted string or a comment, and not the
/// `::` of a cast — becomes what [`variable_reference`] says.
pub(crate) fn substitute_vars(sql: &str, vars: &Variables) -> String {
    if !sql.contains(':') {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len());
    let mut pieces = Pieces::new(sql, Lexicon::Psql);
    while let Some((at, piece)) = pieces.next() {
        let rest = &sql[at.start..];
        if piece != Piece::Code || !rest.starts_with(':') {
            out.push_str(&sql[at]);
        } else if rest.starts_with("::") {
            out.push_str("::");
            pieces.resume_at(at.start + 2);
        } else if let Some((len, value)) = variable_reference(rest, vars) {
            out.push_str(&value);
            pieces.resume_at(at.start + len);
        } else {
            out.push(':');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── split_statements ─────────────────────────────────────────────────

    use Lexicon::{Mysql, Psql};

    fn sqls(input: &str, delimiter: &str, lexicon: Lexicon) -> Vec<String> {
        split_statements(input, delimiter, lexicon).list.into_iter().map(|st| st.sql).collect()
    }

    #[test]
    fn statements_keep_their_terminators() {
        let typed: Vec<String> = split_statements("SELECT 1;SELECT 2 ; SELECT 3", ";", Psql)
            .list.iter().map(TypedStatement::as_typed).collect();
        assert_eq!(typed, ["SELECT 1;", "SELECT 2;", "SELECT 3"]);
        assert_eq!(sqls("SELECT 1", ";", Psql), ["SELECT 1"]);
        assert_eq!(sqls("", ";", Psql), Vec::<String>::new());
        assert_eq!(sqls("  ", ";", Psql), Vec::<String>::new());
        // A terminator that ends nothing: an empty statement.
        assert_eq!(sqls(";;;", ";", Psql), ["", "", ""]);
        assert_eq!(sqls("SELECT 1;; ", ";", Psql), ["SELECT 1", ""]);
        let typed: Vec<String> = split_statements("\\g select 1\\G;", ";", Mysql)
            .list.iter().map(TypedStatement::as_typed).collect();
        assert_eq!(typed, ["\\g", "select 1\\G", ";"]);
        assert_eq!(sqls("SELECT 1// SELECT 2//", "//", Mysql), ["SELECT 1", "SELECT 2"]);
        assert_eq!(sqls("SELECT 1$$ SELECT 2$$", "$$", Mysql), ["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn quotes_and_comments_hold_no_terminator() {
        for lexicon in [Psql, Mysql] {
            assert_eq!(sqls("SELECT 'a;b';", ";", lexicon), ["SELECT 'a;b'"]);
            assert_eq!(sqls("SELECT 'it''s; x';", ";", lexicon), ["SELECT 'it''s; x'"]);
            assert_eq!(sqls(r#"SELECT "a;b";"#, ";", lexicon), [r#"SELECT "a;b""#]);
            assert_eq!(sqls("SELECT /* ; */ 1;", ";", lexicon), ["SELECT /* ; */ 1"]);
            assert_eq!(sqls("SELECT /*/ ; */ 1;", ";", lexicon), ["SELECT /*/ ; */ 1"]);
            assert_eq!(sqls("SELECT 1 -- ;\n;", ";", lexicon), ["SELECT 1 -- ;"]);
        }
        assert_eq!(sqls("SELECT `a;b`;", ";", Mysql), ["SELECT `a;b`"]);
        assert_eq!(sqls("SELECT 1 # ;\n;", ";", Mysql), ["SELECT 1 # ;"]);
        assert_eq!(sqls("SELECT 1 # ;\n;", ";", Psql), ["SELECT 1 #", ""]);
        // Outside quotes the mysql CLI reads a backslash with the
        // character after it: `\'` opens no string, `\;` ends nothing.
        assert_eq!(sqls(r"select 1 \'abc;", ";", Mysql), [r"select 1 \'abc"]);
        let waits = split_statements(r"select 2 \;", ";", Mysql);
        assert_eq!((sqls(r"select 2 \;", ";", Mysql), waits.ended), (vec![r"select 2 \;".to_string()], false));
    }

    #[test]
    fn psql_reads_postgres_quoting() {
        assert_eq!(sqls("SELECT $$ a;b $$;", ";", Psql), ["SELECT $$ a;b $$"]);
        assert_eq!(sqls("SELECT $tag$ a;$$b $tag$;", ";", Psql), ["SELECT $tag$ a;$$b $tag$"]);
        // `$1` is a parameter and `a$b$` an identifier, not dollar quotes.
        assert_eq!(sqls("SELECT $1; SELECT a$b$c; SELECT 2;", ";", Psql), ["SELECT $1", "SELECT a$b$c", "SELECT 2"]);
        // Backslashes escape only in E'…' strings.
        assert_eq!(sqls(r"SELECT E'it\'s; x';", ";", Psql), [r"SELECT E'it\'s; x'"]);
        assert_eq!(sqls(r"SELECT 'a\'; SELECT 2;", ";", Psql), [r"SELECT 'a\'", "SELECT 2"]);
        assert_eq!(sqls(r"SELECT name'x\'; SELECT 2;", ";", Psql), [r"SELECT name'x\'", "SELECT 2"]);
        // Comments nest.
        assert_eq!(sqls("/* a /* b */ ; */ SELECT 1;", ";", Psql), ["/* a /* b */ ; */ SELECT 1"]);
    }

    #[test]
    fn mysql_reads_its_quoting_and_g_terminators() {
        assert_eq!(sqls(r"SELECT 'it\'s; x';", ";", Mysql), [r"SELECT 'it\'s; x'"]);
        assert_eq!(sqls(r#"SELECT "a\"; b";"#, ";", Mysql), [r#"SELECT "a\"; b""#]);
        // The server runs `/*!` and `/*+`: they're code.
        assert_eq!(sqls("/*!40101 SET a=1 */; SELECT 1;", ";", Mysql), ["/*!40101 SET a=1 */", "SELECT 1"]);
        // `--` is a comment only before whitespace.
        assert_eq!(sqls("SELECT 1 --1; SELECT 2;", ";", Mysql), ["SELECT 1 --1", "SELECT 2"]);
        // \g and \G end a statement wherever they're code; \G shows it vertically.
        let stmts = split_statements("select 4\\G select 5 \\g select '\\g' # \\g\n;", ";", Mysql).list;
        let got: Vec<(&str, &str, bool)> = stmts.iter().map(|st| (st.sql.as_str(), st.terminator.as_str(), st.vertical)).collect();
        assert_eq!(got, [("select 4", "\\G", true), ("select 5", "\\g", false), ("select '\\g' # \\g", "\n;", false)]);
        // Not on Postgres, where they're backslash commands.
        assert_eq!(sqls("select 1 \\g", ";", Psql), ["select 1 \\g"]);
    }

    #[test]
    fn mysql_line_comments_go_where_the_mysql_cli_puts_them() {
        let split = |input: &str, lexicon| {
            let Statements { list, ended, .. } = split_statements(input, ";", lexicon);
            (list.iter().map(|st| (st.sql.clone(), st.terminator.clone())).collect::<Vec<_>>(), ended)
        };
        let st = |sql: &str, terminator: &str| (sql.to_string(), terminator.to_string());
        // Alone at the start of a line: a statement, ended by the line.
        assert_eq!(split("-- c", Mysql), (vec![st("-- c", "")], true));
        assert_eq!(split("#c", Mysql), (vec![st("#c", "")], true));
        assert_eq!(split("-- a\n-- b\nselect 1;", Mysql),
                   (vec![st("-- a", ""), st("-- b", ""), st("select 1", ";")], true));
        assert_eq!(split("select 1;\n-- c", Mysql), (vec![st("select 1", ";"), st("-- c", "")], true));
        // After a statement on its line: that statement's.
        assert_eq!(split("select 1 as a; # trailing", Mysql), (vec![st("select 1 as a", "; # trailing")], true));
        // Indented, or inside a statement: part of it.
        assert_eq!(split("   -- indented", Mysql), (vec![st("-- indented", "")], false));
        assert_eq!(split("select 2 as b -- mid\n;", Mysql), (vec![st("select 2 as b -- mid", "\n;")], true));
        assert_eq!(split("select 2 -- mid\n;", Psql), (vec![st("select 2 -- mid", "\n;")], true));
        assert_eq!(split("select 4; /* after */ select 5;", Mysql),
                   (vec![st("select 4", ";"), st("/* after */ select 5", ";")], true));
        // A comment line ahead of an unfinished statement waits with it;
        // one ahead of a finished one goes first.
        assert!(!split_statements("-- note\nselect a,", ";", Mysql).run_on_enter());
        assert!(split_statements("-- note\nselect a, b;", ";", Mysql).run_on_enter());
        assert!(split_statements("select 1; select 2", ";", Mysql).run_on_enter());
        assert!(!split_statements("select 1", ";", Psql).run_on_enter());
        // `--` without a space isn't a comment to MySQL; psql has no such rule.
        assert_eq!(split("--c", Mysql), (vec![st("--c", "")], false));
        assert_eq!(split("-- c", Psql), (vec![st("-- c", "")], false));
    }

    #[test]
    fn a_backslash_command_starts_at_the_first_code_backslash_of_the_last_line() {
        fn at(input: &str, lexicon: Lexicon) -> Option<&str> {
            trailing_command(input, lexicon).map(|at| &input[at..])
        }
        assert_eq!(at("select 1 \\gx", Psql), Some("\\gx"));
        assert_eq!(at("select 1\\g", Psql), Some("\\g"));
        assert_eq!(at("select 1\n\\gx", Psql), Some("\\gx"));
        assert_eq!(at("\\gx", Psql), Some("\\gx"));
        assert_eq!(at("select 1 \\gset p_ \\x", Psql), Some("\\gset p_ \\x"));
        // Only the last line's.
        assert_eq!(at("select 1 \\x\nfrom t \\gx", Psql), Some("\\gx"));
        assert_eq!(at("select 1 \\gx\nfrom t", Psql), None);
        // Not quoted or commented — though it opened lines before.
        assert_eq!(at("select '\\gx'", Psql), None);
        assert_eq!(at("select 'a\n\\gx'", Psql), None);
        assert_eq!(at("select $$a\n\\gx", Psql), None);
        assert_eq!(at("select 1 -- \\gx", Psql), None);
        assert_eq!(at("select 1 /* \\gx */ \\gx", Psql), Some("\\gx"));
        assert_eq!(at("select $$x\\gx$$ \\gx", Psql), Some("\\gx"));
        assert_eq!(at("select E'\\'' \\gx", Psql), Some("\\gx"));
        assert_eq!(at("select 'it\\'s' \\c", Mysql), Some("\\c"));
        assert_eq!(at("select 1 # \\c", Mysql), None);
        assert_eq!(at("select 1", Psql), None);
        // The mysql CLI's: one of its commands — `\N` (NULL) and a
        // letter it has none for stay in the statement.
        assert_eq!(at("\\if 1 \\c", Mysql), Some("\\c"));
        assert_eq!(at("select \\N \\e", Mysql), Some("\\e"));
        assert_eq!(at("select \\\\c", Mysql), None);
        assert_eq!(at("\\if 1", Mysql), None);
        assert_eq!(at("\\if 1", Psql), Some("\\if 1"));
    }

    #[test]
    fn psql_keeps_a_buffer_past_the_line_comments_it_starts_with() {
        assert_eq!(past_leading_line_comments("-- note\n  -- more\nselect 1 -- c", Psql), "select 1 -- c");
        assert_eq!(past_leading_line_comments("-- note", Psql), "");
        assert_eq!(past_leading_line_comments("/* blk */", Psql), "/* blk */");
        assert_eq!(past_leading_line_comments("-- a\n/* b */ select", Psql), "/* b */ select");
        assert_eq!(past_leading_line_comments("select 1", Psql), "select 1");
        assert_eq!(past_leading_line_comments("", Psql), "");
    }

    #[test]
    fn commands_may_start_at_a_code_backslash_a_line_with_nothing_pending_or_a_statement() {
        use CommandStart::{Backslash, Line, Statement};
        assert_eq!(command_starts("select 1; select 2 \\gx", ";", Psql), [Line(0), Statement { start: 0, end: 8 }, Backslash(19)]);
        // (`use db` is text here — which lines and statements are
        // commands is the plane's to say; the shell reads on after one.)
        assert_eq!(command_starts("select 1;\nuse db\nselect 2;", ";", Mysql),
            [Line(0), Statement { start: 0, end: 8 }, Line(10), Statement { start: 10, end: 25 }]);
        assert_eq!(command_starts("select 1; status; select 2;", ";", Mysql),
            [Line(0), Statement { start: 0, end: 8 }, Statement { start: 9, end: 16 }, Statement { start: 17, end: 26 }]);
        // A statement pending at the line's end: the next line is its.
        assert_eq!(command_starts("select 1,\n2;", ";", Psql), [Line(0), Statement { start: 0, end: 11 }]);
        // Quoted or commented, a backslash is text.
        assert_eq!(command_starts("select '\\x'\n\\x", ";", Psql), [Line(0), Backslash(12)]);
        assert_eq!(command_starts("select 1 -- \\x\n", ";", Psql), [Line(0)]);
        // A comment line leaves nothing pending — but psql keeps a
        // `/* */` one in its buffer.
        assert_eq!(command_starts("-- note\n\\s", ";", Mysql), [Line(0), Line(8), Backslash(8)]);
        assert_eq!(command_starts("/* c */\nhelp", ";", Mysql), [Line(0), Line(8)]);
        assert_eq!(command_starts("/* c */\nhelp", ";", Psql), [Line(0)]);
        assert_eq!(command_starts("-- c\nhelp", ";", Psql), [Line(0), Line(5)]);
        // mysql's `\g` / `\G` end a statement; a custom delimiter does.
        assert_eq!(command_starts("select 1 \\G select 2 \\s", ";", Mysql), [Line(0), Backslash(21)]);
        assert_eq!(command_starts("select 1\\G status;", ";", Mysql), [Line(0), Statement { start: 10, end: 17 }]);
        assert_eq!(command_starts("create p() begin end //\ndelimiter ;", "//", Mysql),
            [Line(0), Statement { start: 0, end: 21 }, Line(24)]);
    }

    #[test]
    fn text_ends_inside_a_quote_or_a_comment_as_the_lexer_reads_it() {
        for open in ["select 'x", "select \"a", "select $$x", "select $b$ x $$", "/* c", "/* a /* b */", "select E'\\'"] {
            assert!(ends_inside_quote_or_comment(open, Psql), "{open}");
        }
        for closed in ["select 'x'", "select (1", "/* c */", "select 1 -- c", "", "select 'it''s'"] {
            assert!(!ends_inside_quote_or_comment(closed, Psql), "{closed}");
        }
        assert!(ends_inside_quote_or_comment("select `a", Mysql));
        assert!(!ends_inside_quote_or_comment("select 1 # c", Mysql));
    }

    #[test]
    fn a_mysql_command_takes_its_letter_or_its_arguments_to_the_delimiter() {
        assert_eq!(mysql_command_len("\\s from dual;", ";"), Some(2));
        assert_eq!(mysql_command_len("\\c", ";"), Some(2));
        // `\s;`: the `;` is the line's, read on (mysql 9.6: "No query
        // specified").
        assert_eq!(mysql_command_len("\\s;", ";"), Some(2));
        // Arguments run to the delimiter, that included; the line reads
        // on after it — or to the line's end.
        assert_eq!(mysql_command_len("\\u sakila\nselect 1;", ";"), Some("\\u sakila".len()));
        assert_eq!(mysql_command_len("\\u sakila; select 1;", ";"), Some("\\u sakila;".len()));
        assert_eq!(mysql_command_len("\\u mysql ;; select 6;", ";"), Some("\\u mysql ;".len()));
        assert_eq!(mysql_command_len("\\u sakila select 2;", ";"), Some("\\u sakila select 2;".len()));
        assert_eq!(mysql_command_len("\\u sakila select 2", ";"), Some("\\u sakila select 2".len()));
        assert_eq!(mysql_command_len("\\h select ; select 3;", ";"), Some("\\h select ;".len()));
        assert_eq!(mysql_command_len("\\u sakila// x", "//"), Some("\\u sakila//".len()));
        // `\d` ends at the delimiter it sets.
        assert_eq!(mysql_command_len("\\d //", ";"), Some("\\d //".len()));
        assert_eq!(mysql_command_len("\\d// select 4 //", ";"), Some("\\d//".len()));
        assert_eq!(mysql_command_len("\\d //; x", ";"), Some("\\d //;".len()));
        assert_eq!(mysql_command_len("\\d;", ";"), Some(3));
        // Without its argument, `\d` sets nothing: the line is its.
        assert_eq!(mysql_command_len("\\d", ";"), Some(2));
        assert_eq!(mysql_command_len("\\d \nselect 1;", ";"), Some(3));
        assert_eq!(mysql_command_len("\\h select $$ x", "$$"), Some("\\h select $$".len()));
        assert_eq!(mysql_command_len("\\i", ";"), None);
        assert_eq!(mysql_command_len("\\N", ";"), None);
        assert_eq!(mysql_command_len("\\", ";"), None);
        assert_eq!(mysql_command_len("select", ";"), None);
    }

    #[test]
    fn mysql_unknown_commands_are_read_as_the_cli_reads_the_line() {
        assert_eq!(mysql_unknown_commands("\\if 1", ";"), vec!['i']);
        assert_eq!(mysql_unknown_commands("select 4 \\i \\j;", ";"), vec!['i', 'j']);
        assert_eq!(mysql_unknown_commands("\\if 1 \\c", ";"), vec!['i']);
        assert_eq!(mysql_unknown_commands("select 1 \\ x", ";"), vec![' ']);
        assert_eq!(mysql_unknown_commands("select \\\\", ";"), vec!['\\']);
        // The character it takes is its own: `\'` opens no string.
        assert_eq!(mysql_unknown_commands("select 1 \\'x \\i", ";"), vec!['\'', 'i']);
        // Its own commands, and `\N`: none of them unknown.
        assert!(mysql_unknown_commands("\\endif", ";").is_empty());
        assert!(mysql_unknown_commands("select \\N \\G", ";").is_empty());
        // A backslash ending the line is nothing.
        assert!(mysql_unknown_commands("select 3 \\", ";").is_empty());
        // Quoted or commented, the backslash is the text's — though the
        // quote opened lines before.
        assert!(mysql_unknown_commands("select \"a\\ib\", 'x\\y'", ";").is_empty());
        assert!(mysql_unknown_commands("select 1 /* \\i */ as c; -- \\k", ";").is_empty());
        assert!(mysql_unknown_commands("select 1 # \\k", ";").is_empty());
        assert!(mysql_unknown_commands("select 'a\n\\i' as q;", ";").is_empty());
        // Only the last line's: the lines before were read when entered.
        assert!(mysql_unknown_commands("\\if 1\nselect 1;", ";").is_empty());
        // A command's arguments are its, up to the delimiter; the line
        // is read on after it (mysql 9.6: `\u sakila ; select 1 \i;`).
        assert!(mysql_unknown_commands("\\u sakila \\i", ";").is_empty());
        assert_eq!(mysql_unknown_commands("\\u sakila ; select 1 \\i;", ";"), vec!['i']);
        assert!(mysql_unknown_commands("\\u sakila ; select 1 \\i;", "//").is_empty());
        // Its arguments are skipped as they are, a quote in them none.
        assert_eq!(mysql_unknown_commands("\\h it's ; select 1 \\i;", ";"), vec!['i']);
        assert!(mysql_unknown_commands("\\u sakila ; select '\\i';", ";").is_empty());
        // After `\d`, its delimiter ends the next command's arguments.
        assert_eq!(mysql_unknown_commands("\\d // \\u sakila // select \\i", ";"), vec!['i']);
        assert_eq!(mysql_unknown_commands("\\s \\i", ";"), vec!['i']);
    }

    #[test]
    fn unclosed_and_non_ascii_text_is_read_whole() {
        // Every byte lands in exactly one piece, and nothing panics on a
        // quote, comment or dollar quote left open, or a multi-byte
        // character next to the syntax.
        for s in ["", "'", "\"", "`", "$", "$$", "$a", "$é$ ; $é$;", "E'", "é'x\\'", "e'\\", "/*", "/* /*",
                  "--", "#", "select 'é\\é' \\gx", "select $1$", "x\\", "a\\\nb", "select '🙂' \\G 🙂", "é$$x$$é;", "\\é\\", "\\\\\\"] {
            for lexicon in [Psql, Mysql] {
                let covered: usize = Pieces::new(s, lexicon).map(|(at, _)| at.len()).sum();
                assert_eq!(covered, s.len(), "{s:?}");
                let _ = (split_statements(s, "é", lexicon), trailing_command(s, lexicon), is_comment_only(s, lexicon));
            }
        }
    }

    #[test]
    fn comment_only_is_no_statement() {
        assert!(is_comment_only("-- c", Psql));
        assert!(is_comment_only("  /* a */ -- b\n  ", Psql));
        assert!(is_comment_only("# c", Mysql));
        assert!(!is_comment_only("# c", Psql));
        assert!(!is_comment_only("-- c\nSELECT 1", Psql));
        assert!(!is_comment_only("/* c */ SELECT 1", Psql));
        assert!(!is_comment_only("'x'", Psql));
        // An unterminated block comment runs to the end: all comment.
        assert!(is_comment_only("/* unterminated SELECT", Psql));
        // MySQL runs `/*! … */` — mysqldump's `/*!40101 SET … */;`.
        assert!(!is_comment_only("/*!40101 SET NAMES utf8 */", Mysql));
        assert!(!is_comment_only("/*M!100101 SET NAMES utf8 */", Mysql));
        assert!(is_comment_only("/*!40101 SET NAMES utf8 */", Psql));
        // MySQL's `--` comment wants whitespace after it.
        assert!(!is_comment_only("--1", Mysql));
        assert!(is_comment_only("-- c", Mysql));
        assert!(is_comment_only("--", Mysql));
        assert!(is_comment_only("--1", Psql));
        assert!(!is_comment_only("SELECT 1", Mysql));
    }

    // ── substitute_vars ──────────────────────────────────────────────────

    #[test]
    fn statements_interpolate_as_psql_does() {
        let vars: Variables = [("x", "hello  world"), ("int", "5"), ("bs", r"a\b"), ("q", "O'Brien"), ("t", "film")]
            .into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let sub = |sql: &str| substitute_vars(sql, &vars);
        // Not in a cast, a string, a comment or a dollar-quoted body.
        assert_eq!(sub("select 1::int, 'a:x', :x"), "select 1::int, 'a:x', hello  world");
        assert_eq!(sub("select :t -- :x\n"), "select film -- :x\n");
        assert_eq!(sub("select $$:x$$, \":x\", /* :x */ 1"), "select $$:x$$, \":x\", /* :x */ 1");
        // Literals and identifiers as libpq escapes them.
        assert_eq!(sub("select :'q', :\"q\""), "select 'O''Brien', \"O'Brien\"");
        assert_eq!(sub("select :'bs'"), r"select  E'a\\b'");
        // Whether a variable is set; one that isn't stays as typed.
        assert_eq!(sub("select :{?x}, :{?nosuch}"), "select TRUE, FALSE");
        assert_eq!(sub("select :nosuch, :'nosuch', :\"nosuch\""), "select :nosuch, :'nosuch', :\"nosuch\"");
        // In a name, and in a whole statement.
        assert_eq!(sub("select a:t"), "select afilm");
        assert_eq!(substitute_vars(":cmd", &[("cmd".to_string(), "select 7".to_string())].into()), "select 7");
    }

    #[test]
    fn test_raw_substitution() {
        let mut vars = HashMap::new();
        vars.insert("foo".to_string(), "42".to_string());
        assert_eq!(substitute_vars(":foo", &vars), "42");
    }

    #[test]
    fn test_string_literal_substitution() {
        let mut vars = HashMap::new();
        vars.insert("foo".to_string(), "O'Brien".to_string());
        assert_eq!(substitute_vars(":'foo'", &vars), "'O''Brien'");
    }

    #[test]
    fn test_identifier_substitution() {
        let mut vars = HashMap::new();
        vars.insert("foo".to_string(), "my table".to_string());
        assert_eq!(substitute_vars(r#":"foo""#, &vars), r#""my table""#);
    }

    #[test]
    fn test_undefined_preserved() {
        let vars = HashMap::new();
        assert_eq!(substitute_vars(":bar", &vars), ":bar");
    }

    #[test]
    fn test_multiple_substitutions() {
        let mut vars = HashMap::new();
        vars.insert("a".to_string(), "1".to_string());
        vars.insert("b".to_string(), "2".to_string());
        assert_eq!(substitute_vars("SELECT :a, :b", &vars), "SELECT 1, 2");
    }

    #[test]
    fn test_empty_vars() {
        let vars = HashMap::new();
        assert_eq!(substitute_vars("SELECT :foo", &vars), "SELECT :foo");
    }

    #[test]
    fn test_no_vars_in_sql() {
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "99".to_string());
        assert_eq!(substitute_vars("SELECT 1", &vars), "SELECT 1");
    }
}
