//! psql predefined-variable registry.
//!
//! psql special-cases ~37 variable names that control client
//! behavior or expose session state. This module classifies each
//! name into one of three buckets:
//!
//! - **ReadOnly** — auto-populated by tyto at session events.
//!   `\set <name> ...` is rejected. `:NAME` substitution returns
//!   the most recent value tyto wrote.
//!
//! - **Honored** — `\set <name> <value>` actually changes shell
//!   behavior: `ON_ERROR_STOP` (stop a multi-statement input at its
//!   first error; off by default, as in psql), `AUTOCOMMIT` (off: each
//!   statement outside a transaction opens one) and `ON_ERROR_ROLLBACK`
//!   (a savepoint around each statement in a transaction),
//!   `ECHO_HIDDEN` (a meta-command's queries shown before they run;
//!   `noexec`: shown, not run), `PROMPT1`
//!   (the prompt), `VERBOSITY` and `SHOW_CONTEXT` (how much of a server
//!   error shows). Those with psql's hooks are set as psql sets them
//!   (`assign`).
//!
//! - **AcceptedNoOp** — `\set <name> <value>` is accepted and
//!   stored (so `:NAME` substitution works) but doesn't change
//!   any tyto behavior. The user gets a one-line notice so the
//!   command doesn't silently disappear.
//!
//! Any name NOT in the registry is treated as a user-defined
//! variable (the original `\set` behavior).
//!
//! The classification list intentionally mirrors what psql's
//! `\?` variables topic documents. New psql versions occasionally
//! add predefined names — when one shows up that tyto hasn't
//! classified yet, it falls through as a user variable, which is
//! safe but means tyto-flavored docs may not reference it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsqlVarKind {
    /// Auto-populated; user-`\set` rejected.
    ReadOnly,
    /// Real behavioral toggle.
    Honored,
    /// Stored for `:VAR` substitution but no behavior change.
    AcceptedNoOp,
}

/// Classify a `\set` variable name. Case-sensitive — psql's
/// predefined names are uppercase by convention; lowercase /
/// mixed-case names fall through as user-defined.
pub(crate) fn classify(name: &str) -> Option<PsqlVarKind> {
    use PsqlVarKind::*;
    match name {
        // ── Read-only (auto-populated by tyto) ────────────────────
        // Session identity. Set when SessionOpened lands.
        "USER" | "DBNAME" | "HOST" | "PORT" | "ENCODING"
        // Server version. Populated from SessionOpened.server_version.
        | "VERSION" | "VERSION_NAME" | "VERSION_NUM"
        | "SERVER_VERSION_NAME" | "SERVER_VERSION_NUM"
        // Last-query state. Updated on QueryError / QueryResult.
        | "ERROR" | "LAST_ERROR_MESSAGE" | "LAST_ERROR_SQLSTATE"
        | "SQLSTATE" | "ROW_COUNT" | "LASTOID"
        => Some(ReadOnly),

        // ── Honored (real behavior) ───────────────────────────────
        // PROMPT2 / PROMPT3 are AcceptedNoOp until tyto has a render
        // site for them — PROMPT2 is psql's multi-line continuation
        // prompt (tyto submits whole-buffer per Enter) and PROMPT3
        // is shown during `\copy` (not yet implemented). Promoting
        // them prematurely would mislead users into thinking
        // `\set PROMPT2 'tpl'` actually changed something on screen.
        "ON_ERROR_STOP" | "AUTOCOMMIT" | "ON_ERROR_ROLLBACK" | "ECHO_HIDDEN" | "PROMPT1" | "VERBOSITY" | "SHOW_CONTEXT"
        | "QUIET"
        => Some(Honored),

        // ── Accepted-no-op (stored, doc-noted) ────────────────────
        // psql client-side toggles whose semantics need protocol
        // extensions or session-level rewiring to mean anything in
        // tyto. Each accepts the set so scripts that `\set X y` at
        // the top don't fail — they just don't change behavior yet.
        "ECHO"
        | "SHOW_ALL_RESULTS"
        | "FETCH_COUNT"
        | "HISTCONTROL" | "HISTSIZE" | "HISTFILE"
        | "IGNOREEOF"
        | "PROMPT2" | "PROMPT3"
        | "SINGLELINE" | "SINGLESTEP"
        | "COMP_KEYWORD_CASE"
        | "HIDE_TABLEAM" | "HIDE_TOAST_COMPRESSION"
        => Some(AcceptedNoOp),

        _ => None,
    }
}

/// The values psql accepts for an enum-valued honored variable, or
/// None for any other name.
pub(crate) fn enum_values(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "VERBOSITY"    => Some(&["default", "verbose", "terse", "sqlstate"]),
        "SHOW_CONTEXT" => Some(&["never", "errors", "always"]),
        _ => None,
    }
}

/// The values psql starts with for its variables with hooks
/// (`EstablishVariableSpace`) — `:AUTOCOMMIT` is `on` before any `\set`.
pub(crate) const DEFAULTS: [(&str, &str); 7] = [
    ("AUTOCOMMIT", "on"),
    ("ECHO_HIDDEN", "off"),
    ("ON_ERROR_ROLLBACK", "off"),
    ("ON_ERROR_STOP", "off"),
    ("QUIET", "off"),
    ("SHOW_CONTEXT", "errors"),
    ("VERBOSITY", "default"),
];

/// What psql stores for `\set NAME value` — `value` None: `\unset` — of
/// one of its variables with hooks: its substitute hook first (a Boolean
/// with no value is `on`, unset `off`; `\unset VERBOSITY` is `default`,
/// `SHOW_CONTEXT` `errors`), then its assign hook — the value checked,
/// as typed when it's taken (`of` stays `of`), or psql's refusal (the
/// old value kept). None for any other name.
pub(crate) fn assign(name: &str, value: Option<&str>) -> Option<Result<String, Vec<String>>> {
    let boolean = |value: Option<&str>| match value {
        None => "off".to_string(),
        Some("") => "on".to_string(),
        Some(value) => value.to_string(),
    };
    let refused = |value: &str, available: &str| Err(vec![
        format!("unrecognized value \"{value}\" for \"{name}\""),
        format!("Available values are: {available}."),
    ]);
    Some(match name {
        "AUTOCOMMIT" | "ON_ERROR_STOP" | "QUIET" => {
            let value = boolean(value);
            match parse_bool(&value) {
                Some(_) => Ok(value),
                None => Err(vec![format!("unrecognized value \"{value}\" for \"{name}\": Boolean expected")]),
            }
        }
        "ON_ERROR_ROLLBACK" => {
            let value = boolean(value);
            if value.eq_ignore_ascii_case("interactive") || parse_bool(&value).is_some() {
                Ok(value)
            } else {
                refused(&value, "on, off, interactive")
            }
        }
        "ECHO_HIDDEN" => {
            let value = boolean(value);
            if value.eq_ignore_ascii_case("noexec") || parse_bool(&value).is_some() {
                Ok(value)
            } else {
                refused(&value, "on, off, noexec")
            }
        }
        "VERBOSITY" | "SHOW_CONTEXT" => {
            let values = enum_values(name).unwrap_or_default();
            match value {
                None => Ok(DEFAULTS.iter().find(|(n, _)| *n == name).map_or("", |(_, v)| v).to_string()),
                Some(value) if values.iter().any(|v| v.eq_ignore_ascii_case(value)) => Ok(value.to_string()),
                Some(value) => refused(value, &values.join(", ")),
            }
        }
        _ => return None,
    })
}

/// How much of a server error to show, from `VERBOSITY` and
/// `SHOW_CONTEXT` (psql's defaults when unset).
pub(crate) fn error_style(vars: &std::collections::HashMap<String, String>) -> crate::plane::ErrorStyle {
    use crate::plane::{ErrorStyle, ShowContext, Verbosity};
    ErrorStyle {
        verbosity: match vars.get("VERBOSITY").map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("verbose")  => Verbosity::Verbose,
            Some("terse")    => Verbosity::Terse,
            Some("sqlstate") => Verbosity::Sqlstate,
            _ => Verbosity::Default,
        },
        show_context: match vars.get("SHOW_CONTEXT").map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("never")  => ShowContext::Never,
            Some("always") => ShowContext::Always,
            _ => ShowContext::Errors,
        },
    }
}

/// psql's `QUIET` (`pset.quiet`): command tags, `\pset`'s and `\timing`'s
/// words, `\c`'s notice, `\d`'s "Did not find …" and the like left
/// unsaid.
pub(crate) fn quiet(variables: &crate::parser::Variables) -> bool {
    variables.get("QUIET").and_then(|value| parse_bool(value)) == Some(true)
}

/// A Boolean as psql reads one (`ParseVariableBool`) — `\set`'s
/// honored variables, `\timing`, `\if`. None: not one.
pub(crate) fn parse_bool(s: &str) -> Option<bool> {
    // psql's `ParseVariableBool`: any prefix of true / false / yes / no,
    // in any case; on / off spelt to at least two letters ("o" is both);
    // 1 / 0.
    let value = s.to_ascii_lowercase();
    let prefix_of = |word: &str, least: usize| value.len() >= least && word.starts_with(&value);
    if prefix_of("true", 1) || prefix_of("yes", 1) || prefix_of("on", 2) || value == "1" {
        Some(true)
    } else if prefix_of("false", 1) || prefix_of("no", 1) || prefix_of("off", 2) || value == "0" {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_reads_as_psql_reads_it() {
        let vars = |value: &str| crate::parser::Variables::from([("QUIET".to_string(), value.to_string())]);
        assert!(quiet(&vars("on")) && quiet(&vars("t")) && quiet(&vars("1")));
        assert!(!quiet(&vars("off")) && !quiet(&vars("x")) && !quiet(&crate::parser::Variables::new()));
    }

    #[test]
    fn variables_with_hooks_are_set_as_psql_sets_them() {
        // psql 18.4: a bare `\set` is `on`, `\unset` `off`, a value kept
        // as typed, a bad one refused in psql's words.
        assert_eq!(assign("AUTOCOMMIT", None), Some(Ok("off".into())));
        assert_eq!(assign("AUTOCOMMIT", Some("")), Some(Ok("on".into())));
        assert_eq!(assign("AUTOCOMMIT", Some("of")), Some(Ok("of".into())));
        assert_eq!(assign("AUTOCOMMIT", Some("x")),
                   Some(Err(vec!["unrecognized value \"x\" for \"AUTOCOMMIT\": Boolean expected".into()])));
        assert_eq!(assign("ON_ERROR_STOP", None), Some(Ok("off".into())));
        assert_eq!(assign("ON_ERROR_ROLLBACK", Some("INTERACTIVE")), Some(Ok("INTERACTIVE".into())));
        assert_eq!(assign("ON_ERROR_ROLLBACK", Some("")), Some(Ok("on".into())));
        assert_eq!(assign("ON_ERROR_ROLLBACK", None), Some(Ok("off".into())));
        assert_eq!(assign("ON_ERROR_ROLLBACK", Some("x")), Some(Err(vec![
            "unrecognized value \"x\" for \"ON_ERROR_ROLLBACK\"".into(),
            "Available values are: on, off, interactive.".into(),
        ])));
        assert_eq!(assign("ECHO_HIDDEN", Some("NoExec")), Some(Ok("NoExec".into())));
        assert_eq!(assign("ECHO_HIDDEN", None), Some(Ok("off".into())));
        assert_eq!(assign("ECHO_HIDDEN", Some("x")), Some(Err(vec![
            "unrecognized value \"x\" for \"ECHO_HIDDEN\"".into(),
            "Available values are: on, off, noexec.".into(),
        ])));
        assert_eq!(assign("VERBOSITY", None), Some(Ok("default".into())));
        assert_eq!(assign("SHOW_CONTEXT", None), Some(Ok("errors".into())));
        assert_eq!(assign("QUIET", None), Some(Ok("off".into())));
        assert_eq!(assign("QUIET", Some("x")),
                   Some(Err(vec!["unrecognized value \"x\" for \"QUIET\": Boolean expected".into()])));
        assert_eq!(assign("VERBOSITY", Some("Terse")), Some(Ok("Terse".into())));
        assert!(matches!(assign("SHOW_CONTEXT", Some("x")), Some(Err(lines)) if lines[1] == "Available values are: never, errors, always."));
        // Others: no hooks.
        assert_eq!(assign("PROMPT1", Some("x")), None);
        assert_eq!(assign("mine", None), None);
    }

    #[test]
    fn classify_known_categories() {
        assert_eq!(classify("USER"), Some(PsqlVarKind::ReadOnly));
        assert_eq!(classify("DBNAME"), Some(PsqlVarKind::ReadOnly));
        assert_eq!(classify("LAST_ERROR_MESSAGE"), Some(PsqlVarKind::ReadOnly));
        assert_eq!(classify("ON_ERROR_STOP"), Some(PsqlVarKind::Honored));
        assert_eq!(classify("AUTOCOMMIT"), Some(PsqlVarKind::Honored));
        assert_eq!(classify("ON_ERROR_ROLLBACK"), Some(PsqlVarKind::Honored));
        assert_eq!(classify("ECHO"), Some(PsqlVarKind::AcceptedNoOp));
        assert_eq!(classify("VERBOSITY"), Some(PsqlVarKind::Honored));
        assert_eq!(classify("SHOW_CONTEXT"), Some(PsqlVarKind::Honored));
    }

    #[test]
    fn classify_user_defined_is_none() {
        assert_eq!(classify("my_var"), None);
        assert_eq!(classify("X"), None);
        // Case-sensitive — psql convention is uppercase predefineds.
        assert_eq!(classify("user"), None);
        assert_eq!(classify("autocommit"), None);
    }

    #[test]
    fn parse_bool_psql_aliases() {
        // Prefixes, as psql's ParseVariableBool takes them; "o" is both.
        assert_eq!(parse_bool("t"),     Some(true));
        assert_eq!(parse_bool("FAL"),   Some(false));
        assert_eq!(parse_bool("y"),     Some(true));
        assert_eq!(parse_bool("of"),    Some(false));
        assert_eq!(parse_bool("o"),     None);
        assert_eq!(parse_bool(""),      None);
        assert_eq!(parse_bool("truex"), None);
        assert_eq!(parse_bool("on"),    Some(true));
        assert_eq!(parse_bool("OFF"),   Some(false));
        assert_eq!(parse_bool("yes"),   Some(true));
        assert_eq!(parse_bool("No"),    Some(false));
        assert_eq!(parse_bool("1"),     Some(true));
        assert_eq!(parse_bool("0"),     Some(false));
        assert_eq!(parse_bool("true"),  Some(true));
        assert_eq!(parse_bool("false"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
        assert_eq!(parse_bool(""),      None);
    }

    #[test]
    fn error_style_reads_verbosity_and_show_context() {
        use crate::plane::{ShowContext, Verbosity};
        let vars = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let style = error_style(&vars(&[]));
        assert_eq!((style.verbosity, style.show_context), (Verbosity::Default, ShowContext::Errors));
        let style = error_style(&vars(&[("VERBOSITY", "Verbose"), ("SHOW_CONTEXT", "always")]));
        assert_eq!((style.verbosity, style.show_context), (Verbosity::Verbose, ShowContext::Always));
        assert_eq!(enum_values("VERBOSITY"), Some(&["default", "verbose", "terse", "sqlstate"][..]));
        assert_eq!(enum_values("ECHO"), None);
    }
}
