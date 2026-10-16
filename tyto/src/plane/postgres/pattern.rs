//! psql's object-name patterns — `\dt sales.*`, `\d "Film"`,
//! `\df pg_catalog.array_*` — ported from fe_utils/string_utils.c
//! (`patternToSQLRegex`, `processSQLNamePattern`) and psql's describe.c
//! (`validateSQLNamePattern`), so every `\d*` command matches names the
//! way psql does:
//!
//! - `*` matches any run of characters, `?` any one character.
//! - A `.` separates the schema from the name (and, where a command
//!   allows it, the database from the schema).
//! - Unquoted ASCII letters fold to lower case; inside double quotes
//!   case is kept and every character is literal (`""` is one quote).
//! - Outside quotes, other regular-expression characters pass through,
//!   so `\d (film|actor)` works. `$` is always literal, and so is `[]`.
//! - The result is anchored, `^(...)$`, and compared with
//!   `OPERATOR(pg_catalog.~)` under the database's default collation.
//!
//! The SQL text matches what psql sends, line for line, so the ported
//! describe.c queries read the same as psql's.

use std::fmt;

use crate::parser::Variables;

/// Where a pattern applies in a catalog query, and how: the columns
/// holding the schema and object names, an alternative name column
/// (`\dT` also matches the `format_type` spelling), and the rule that
/// limits an unqualified pattern to objects on the search path.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct NameMatch<'a> {
    pub schema_var:      Option<&'a str>,
    pub name_var:        Option<&'a str>,
    pub alt_name_var:    Option<&'a str>,
    pub visibility_rule: Option<&'a str>,
    /// Treat regular-expression characters as literal even outside
    /// double quotes.
    pub force_escape:    bool,
}

/// A pattern psql refuses, with psql's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PatternError {
    TooManyDots(String),
    CrossDatabase(String),
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatternError::TooManyDots(p)   => write!(f, "improper qualified name (too many dotted names): {p}"),
            PatternError::CrossDatabase(p) => write!(f, "cross-database references are not implemented: {p}"),
        }
    }
}

impl NameMatch<'_> {
    /// psql's `validateSQLNamePattern`: append the pattern's WHERE
    /// clauses to `buf` (see [`NameMatch::process`]), then refuse a
    /// pattern with `max_parts` or more dotted parts, or one naming a
    /// database other than `current_db`. Returns whether a clause was
    /// added.
    pub(super) fn validate(
        &self,
        buf: &mut String,
        pattern: Option<&str>,
        have_where: bool,
        max_parts: usize,
        current_db: &str,
    ) -> Result<bool, PatternError> {
        let processed = self.process(buf, pattern, have_where);
        let pattern = pattern.unwrap_or_default();
        if processed.dots >= max_parts {
            return Err(PatternError::TooManyDots(pattern.to_string()));
        }
        if max_parts > 1 && processed.dots == max_parts - 1 && processed.dbname != current_db {
            return Err(PatternError::CrossDatabase(pattern.to_string()));
        }
        Ok(processed.added)
    }

    /// psql's `processSQLNamePattern`: append to `buf` the WHERE clauses
    /// (`WHERE` first unless `have_where`, then `  AND `, each line
    /// ending in a newline) that limit a catalog query to `pattern`. No
    /// pattern — or no schema part — limits it to visible objects.
    pub(super) fn process(&self, buf: &mut String, pattern: Option<&str>, mut have_where: bool) -> Processed {
        let mut added = false;
        let mut clause = |buf: &mut String, text: &str| {
            buf.push_str(if have_where { "  AND " } else { "WHERE " });
            buf.push_str(text);
            buf.push('\n');
            have_where = true;
            added = true;
        };
        let Some(pattern) = pattern else {
            if let Some(rule) = self.visibility_rule { clause(buf, rule); }
            return Processed { added, dbname: String::new(), dots: 0 };
        };

        let parts = pattern_to_regex(pattern, self.schema_var.is_some(), self.schema_var.is_some(), self.force_escape);
        let matches = |var: &str, regex: &str| format!("{var} OPERATOR(pg_catalog.~) {} COLLATE pg_catalog.default", sql_literal(regex));

        // A bare `*` matches everything; psql drops the clause.
        if let Some(name_var) = self.name_var.filter(|_| parts.name != "^(.*)$") {
            match self.alt_name_var {
                Some(alt) => clause(buf, &format!("({}\n        OR {})", matches(name_var, &parts.name), matches(alt, &parts.name))),
                None      => clause(buf, &matches(name_var, &parts.name)),
            }
        }
        match (self.schema_var, &parts.schema) {
            (Some(schema_var), Some(schema)) => {
                if schema != "^(.*)$" { clause(buf, &matches(schema_var, schema)); }
            }
            _ => if let Some(rule) = self.visibility_rule { clause(buf, rule); },
        }
        Processed { added, dbname: parts.dbname.unwrap_or_default(), dots: parts.dots }
    }
}

/// What [`NameMatch::process`] found in a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Processed {
    pub added:  bool,
    /// The database part, as typed (case folded, wildcards literal).
    pub dbname: String,
    /// How many `.` separators the pattern had (outside quotes).
    pub dots:   usize,
}

/// A pattern split into anchored regular expressions.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Regexes {
    dbname: Option<String>,
    schema: Option<String>,
    name:   String,
    dots:   usize,
}

/// psql's `patternToSQLRegex` (with `want_literal_dbname`): split a
/// shell-style pattern on unquoted dots — into database, schema and
/// name when `want_dbname`, schema and name when only `want_schema` —
/// and turn each part into an anchored regular expression. Dots past
/// the last part are literal characters but still counted.
fn pattern_to_regex(pattern: &str, want_dbname: bool, want_schema: bool, force_escape: bool) -> Regexes {
    let max_parts = if want_dbname { 3 } else if want_schema { 2 } else { 1 };
    let mut parts = vec![String::from("^(")];
    let mut dbname_literal = String::new(); // the first part, as typed
    let mut first_part = true;
    let mut dots = 0;
    let mut in_quotes = false;
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        let room_for_part = parts.len() < max_parts;
        let cur = parts.last_mut().expect("never empty");
        match ch {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                chars.next();
                cur.push('"');
                if first_part { dbname_literal.push('"'); }
            }
            '"' => in_quotes = !in_quotes,
            c if !in_quotes && c.is_ascii_uppercase() => {
                cur.push(c.to_ascii_lowercase());
                if first_part { dbname_literal.push(c.to_ascii_lowercase()); }
            }
            '*' if !in_quotes => {
                cur.push_str(".*");
                if first_part { dbname_literal.push('*'); }
            }
            '?' if !in_quotes => {
                cur.push('.');
                if first_part { dbname_literal.push('?'); }
            }
            '.' if !in_quotes => {
                first_part = false;
                dots += 1;
                if room_for_part {
                    cur.push_str(")$");
                    parts.push(String::from("^("));
                } else {
                    cur.push('.');
                }
            }
            // Always literal: legal in identifiers, and pointless as an
            // anchor in an anchored pattern.
            '$' => {
                cur.push_str("\\$");
                if first_part { dbname_literal.push('$'); }
            }
            c => {
                let special = "|*+?()[]{}.^$\\".contains(c);
                // `[]` is far likelier an array type than a bracket
                // expression.
                if ((in_quotes || force_escape) && special) || (c == '[' && chars.peek() == Some(&']')) {
                    cur.push('\\');
                }
                cur.push(c);
                if first_part { dbname_literal.push(c); }
            }
        }
    }
    parts.last_mut().expect("never empty").push_str(")$");

    let name = parts.pop().expect("never empty");
    let schema = if want_schema { parts.pop() } else { None };
    let dbname = if want_dbname && !parts.is_empty() { Some(dbname_literal) } else { None };
    Regexes { dbname, schema, name, dots }
}

/// A string literal the way psql's `appendStringLiteralConn` writes
/// one: `'...'` with quotes doubled, or `E'...'` with backslashes
/// doubled too when the text holds a backslash.
fn sql_literal(text: &str) -> String {
    let quoted = text.replace('\'', "''");
    if text.contains('\\') {
        format!("E'{}'", quoted.replace('\\', "\\\\"))
    } else {
        format!("'{quoted}'")
    }
}

/// Split a meta-command's argument text the way psql reads ordinary
/// arguments (psqlscanslash.l, `OT_NORMAL`): whitespace separates them;
/// double-quoted text keeps its quotes (patterns use them); single-quoted
/// text loses them, with `''` and the escapes `\n` `\t` `\b` `\r` `\f`
/// resolved (any other escaped character stands for itself — psql's octal
/// / hex escapes aren't read); quoted and unquoted pieces next to each
/// other join into one argument. Outside quotes, variable references
/// become their values (`parser::variable_reference`: `:name`, `:'name'`,
/// `:"name"`, `:{?name}`) as part of the argument they're in — a value
/// with spaces stays one argument. With `strip_semicolons`, semicolons
/// ending an argument outside quotes go (`\d film;`), as psql strips them
/// for most commands. An unterminated quote is psql's error, and the
/// arguments read so far.
pub(super) fn slash_args(text: &str, vars: &Variables, strip_semicolons: bool) -> (Vec<String>, Option<&'static str>) {
    const UNTERMINATED: Option<&str> = Some("unterminated quoted string");
    let mut args = Vec::new();
    let mut at = 0;
    let char_at = |at: usize| text[at..].chars().next();
    loop {
        while let Some(c) = char_at(at).filter(|c| c.is_whitespace()) { at += c.len_utf8(); }
        if at >= text.len() { return (args, None); }
        let mut arg = String::new();
        // How many of the argument's last characters were typed outside
        // quotes (psql's `unquoted_option_chars`): only those semicolons go.
        let mut unquoted = 0usize;
        while let Some(c) = char_at(at).filter(|c| !c.is_whitespace()) {
            at += c.len_utf8();
            if !matches!(c, '"' | '\'' | ':') {
                unquoted += 1;
            }
            match c {
                '"' => {
                    arg.push('"');
                    loop {
                        let Some(c) = char_at(at) else { return (args, UNTERMINATED) };
                        at += c.len_utf8();
                        arg.push(c);
                        if c == '"' {
                            if char_at(at) != Some('"') { break; }
                            at += 1;
                            arg.push('"');
                        }
                    }
                }
                '\'' => loop {
                    let Some(c) = char_at(at) else { return (args, UNTERMINATED) };
                    at += c.len_utf8();
                    match c {
                        '\'' if char_at(at) == Some('\'') => { at += 1; arg.push('\''); }
                        '\'' => break,
                        '\\' => {
                            let Some(escaped) = char_at(at) else { return (args, UNTERMINATED) };
                            at += escaped.len_utf8();
                            arg.push(match escaped {
                                'n' => '\n',
                                't' => '\t',
                                'b' => '\u{8}',
                                'r' => '\r',
                                'f' => '\u{c}',
                                c => c,
                            });
                        }
                        c => arg.push(c),
                    }
                },
                ':' => match crate::parser::variable_reference(&text[at - 1..], vars) {
                    Some((len, value)) => { arg.push_str(&value); at += len - 1; unquoted = 0; }
                    None => { arg.push(':'); unquoted += 1; }
                },
                c => arg.push(c),
            }
            if matches!(c, '"' | '\'') {
                unquoted = 0;
            }
        }
        if strip_semicolons {
            while unquoted > 0 && arg.ends_with(';') {
                arg.pop();
                unquoted -= 1;
            }
        }
        args.push(arg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELATIONS: NameMatch<'static> = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("c.relname"),
        alt_name_var:    None,
        visibility_rule: Some("pg_catalog.pg_table_is_visible(c.oid)"),
        force_escape:    false,
    };

    fn clauses(m: &NameMatch, pattern: Option<&str>) -> String {
        let mut buf = String::new();
        m.process(&mut buf, pattern, false);
        buf
    }

    #[test]
    fn patterns_become_the_regexes_psql_builds() {
        let regex = |p: &str| {
            let r = pattern_to_regex(p, true, true, false);
            (r.dbname, r.schema, r.name)
        };
        let name = |n: &str| (None, None, n.to_string());
        let qualified = |s: &str, n: &str| (None, Some(s.to_string()), n.to_string());
        assert_eq!(regex("Film"), name("^(film)$"));
        assert_eq!(regex("\"Film\""), name("^(Film)$"));
        assert_eq!(regex("film*"), name("^(film.*)$"));
        assert_eq!(regex("fi?m"), name("^(fi.m)$"));
        assert_eq!(regex("*"), name("^(.*)$"));
        assert_eq!(regex("public.film"), qualified("^(public)$", "^(film)$"));
        assert_eq!(regex("*.film"), qualified("^(.*)$", "^(film)$"));
        assert_eq!(regex("\"a.b\".c"), qualified("^(a\\.b)$", "^(c)$"));
        assert_eq!(regex("\"say \"\"hi\"\"\""), name("^(say \"hi\")$"));
        assert_eq!(regex("Pagila.Public.Film"),
                   (Some("pagila".into()), Some("^(public)$".into()), "^(film)$".into()));
        // The database part is kept as typed: wildcards and `$` literal.
        assert_eq!(regex("Pag*$.\"S\".t").0.as_deref(), Some("pag*$"));
        assert_eq!(regex(r#""My ""Db""".s.t"#).0.as_deref(), Some(r#"My "Db""#));
        // Inside quotes a dot doesn't split.
        assert_eq!(regex(r#""a.b.c""#), name(r"^(a\.b\.c)$"));
        // Dots past the last part are literal, but counted.
        let r = pattern_to_regex("a.b.c.d", true, true, false);
        assert_eq!((r.name.as_str(), r.dots), ("^(c.d)$", 3));
        // `$` and `[]` are literal; other regex characters pass through
        // outside quotes and are escaped inside them.
        assert_eq!(regex("price$"), name("^(price\\$)$"));
        assert_eq!(regex("int[]"), name("^(int\\[])$"));
        assert_eq!(regex("(film|actor)"), name("^((film|actor))$"));
        assert_eq!(regex("\"(film|actor)\""), name("^(\\(film\\|actor\\))$"));
        assert_eq!(pattern_to_regex("a+b", false, false, true).name, "^(a\\+b)$");
        // Only ASCII letters fold, as in psql.
        assert_eq!(regex("ÉTÉ"), name("^(ÉtÉ)$"));
    }

    #[test]
    fn clauses_read_like_psql_s() {
        assert_eq!(clauses(&RELATIONS, Some("film*")),
            "WHERE c.relname OPERATOR(pg_catalog.~) '^(film.*)$' COLLATE pg_catalog.default\n  \
             AND pg_catalog.pg_table_is_visible(c.oid)\n");
        assert_eq!(clauses(&RELATIONS, Some("pub*.film|actor")),
            "WHERE c.relname OPERATOR(pg_catalog.~) '^(film|actor)$' COLLATE pg_catalog.default\n  \
             AND n.nspname OPERATOR(pg_catalog.~) '^(pub.*)$' COLLATE pg_catalog.default\n");
        // No pattern, or `*`: just the visibility rule. `*.*`: nothing.
        assert_eq!(clauses(&RELATIONS, None), "WHERE pg_catalog.pg_table_is_visible(c.oid)\n");
        assert_eq!(clauses(&RELATIONS, Some("*")), "WHERE pg_catalog.pg_table_is_visible(c.oid)\n");
        assert_eq!(clauses(&RELATIONS, Some("*.*")), "");
        // A backslash in the regex makes psql write an E'' literal.
        assert_eq!(clauses(&NameMatch { visibility_rule: None, ..RELATIONS }, Some("int[]")),
            "WHERE c.relname OPERATOR(pg_catalog.~) E'^(int\\\\[])$' COLLATE pg_catalog.default\n");
        assert_eq!(clauses(&NameMatch { visibility_rule: None, ..RELATIONS }, Some("o'neil")),
            "WHERE c.relname OPERATOR(pg_catalog.~) '^(o''neil)$' COLLATE pg_catalog.default\n");
        // An alternative name column is ORed in.
        let types = NameMatch { alt_name_var: Some("pg_catalog.format_type(t.oid, NULL)"), ..RELATIONS };
        assert_eq!(clauses(&types, Some("int4")),
            "WHERE (c.relname OPERATOR(pg_catalog.~) '^(int4)$' COLLATE pg_catalog.default\n        \
             OR pg_catalog.format_type(t.oid, NULL) OPERATOR(pg_catalog.~) '^(int4)$' COLLATE pg_catalog.default)\n  \
             AND pg_catalog.pg_table_is_visible(c.oid)\n");
        // Appended to an existing WHERE.
        let mut buf = String::from("WHERE c.relkind = 'r'\n");
        assert!(RELATIONS.process(&mut buf, Some("film"), true).added);
        assert!(buf.ends_with("  AND pg_catalog.pg_table_is_visible(c.oid)\n"), "{buf}");
    }

    #[test]
    fn validation_refuses_what_psql_refuses() {
        let check = |m: &NameMatch, p: &str, max_parts| m.validate(&mut String::new(), Some(p), false, max_parts, "pagila");
        assert_eq!(check(&RELATIONS, "pagila.public.film", 3), Ok(true));
        assert_eq!(check(&RELATIONS, "a.b.c.d", 3).unwrap_err().to_string(),
                   "improper qualified name (too many dotted names): a.b.c.d");
        assert_eq!(check(&RELATIONS, "nope.public.film", 3).unwrap_err().to_string(),
                   "cross-database references are not implemented: nope.public.film");
        // Schemas (`\dn`): one dot is a database part psql never reads.
        let schemas = NameMatch { name_var: Some("n.nspname"), ..NameMatch::default() };
        assert_eq!(check(&schemas, "pagila.public", 2).unwrap_err().to_string(),
                   "cross-database references are not implemented: pagila.public");
        assert_eq!(check(&schemas, "x.y.z", 2).unwrap_err().to_string(),
                   "improper qualified name (too many dotted names): x.y.z");
        assert_eq!(check(&schemas, "pub*", 2), Ok(true));
    }

    #[test]
    fn arguments_split_the_way_psql_reads_them() {
        let none = Variables::new();
        let slash_args = |t: &str| super::slash_args(t, &none, false);
        let args = |t: &str| slash_args(t).0;
        assert_eq!(args("  film   actor "), ["film", "actor"]);
        assert_eq!(args("\"my table\" x"), ["\"my table\"", "x"]);
        assert_eq!(args("public.\"My \"\"T\"\"\""), ["public.\"My \"\"T\"\"\""]);
        assert_eq!(args("'it''s' 'a\\tb'"), ["it's", "a\tb"]);
        assert_eq!(args("pre'mid'post"), ["premidpost"]);
        assert!(args("").is_empty());
        assert_eq!(slash_args("film o'neil"), (vec!["film".to_string()], Some("unterminated quoted string")));
        assert_eq!(slash_args("\"open"), (vec![], Some("unterminated quoted string")));
    }

    #[test]
    fn variables_become_part_of_the_argument_they_are_in() {
        let vars: Variables = [("x", "hello  world"), ("q", "O'Brien"), ("t", "film")]
            .into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let args = |t: &str| slash_args(t, &vars, false).0;
        // One argument, spaces and all — psql doesn't split a value.
        assert_eq!(args(":x"), ["hello  world"]);
        assert_eq!(args("pre:x post"), ["prehello  world", "post"]);
        assert_eq!(args("c=:t i=0.1"), ["c=film", "i=0.1"]);
        assert_eq!(args(":'q' :\"q\""), ["'O''Brien'", "\"O'Brien\""]);
        assert_eq!(args(":{?x} :{?nosuch}"), ["TRUE", "FALSE"]);
        // Not inside quotes; an unset one stays as typed.
        assert_eq!(args("':x' \":x\""), [":x", "\":x\""]);
        assert_eq!(args(":nosuch :'nosuch'"), [":nosuch", ":'nosuch'"]);
        // A colon that starts no reference.
        assert_eq!(args("a::t :"), ["a:film", ":"]);
    }

    #[test]
    fn a_trailing_semicolon_goes_only_where_psql_drops_it() {
        let vars: Variables = [("t", "film;")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let stripped = |t: &str| slash_args(t, &vars, true).0;
        assert_eq!(stripped("film;"), ["film"]);
        assert_eq!(stripped("a b;;"), ["a", "b"]);
        // Not inside quotes, nor from a variable's value.
        assert_eq!(stripped("'film;'"), ["film;"]);
        assert_eq!(stripped("\"film;\";"), ["\"film;\""]);
        assert_eq!(stripped(":t"), ["film;"]);
        assert_eq!(stripped(":t;"), ["film;"]);
        // Kept where psql keeps it (`\echo a;`).
        assert_eq!(slash_args("a;", &vars, false).0, ["a;"]);
    }
}
