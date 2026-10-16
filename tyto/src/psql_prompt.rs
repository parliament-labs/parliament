//! psql `PROMPT1` / `PROMPT2` / `PROMPT3` template formatter.
//!
//! Renders a psql-style prompt template (e.g. `"%n@%/ %l SQL>"`)
//! into a concrete string using the current session state. The
//! substitutions follow psql's documented `%`-codes:
//!
//! | Code        | Expansion                                            |
//! |-------------|------------------------------------------------------|
//! | `%n`        | session user                                         |
//! | `%/`        | current database                                     |
//! | `%~`        | abbreviated database (same as `%/` for now)          |
//! | `%M`        | full host (or `[local]`)                             |
//! | `%m`        | short host (chars before first `.`)                  |
//! | `%>`        | port                                                 |
//! | `%R`        | state indicator (`=` ready, `^` single-line)         |
//! | `%x`        | txn state (empty Idle / `*` Active / `!` Failed)     |
//! | `%#`        | privilege indicator (`#` superuser / `>` otherwise)  |
//! | `%p`        | backend PID (empty if unknown)                       |
//! | `%l`        | statement number (1-indexed)                         |
//! | `%:VAR:`    | value of psql variable `VAR`                         |
//! | `%%`        | literal `%`                                          |
//! | other `%X`  | left unsubstituted (literal `%X`)                    |
//!
//! Codes psql supports but Tyto doesn't track yet — `%w` (whitespace
//! padding to PROMPT1's width), `%[…%]` (terminal escape passthrough),
//! `%`cmd`` (shell substitution) — pass through unchanged. Future
//! work can add them without breaking the existing surface.

use std::collections::HashMap;

use crate::types::TxnState;

/// Snapshot of the per-tab session state the prompt formatter
/// reads. Built fresh per render so the prompt always reflects
/// the current signals.
pub(crate) struct PromptContext<'a> {
    pub user:        &'a str,
    pub dbname:      &'a str,
    pub host:        &'a str,
    pub port:        &'a str,
    /// PG backend process id (Postgres `:VERSION_NAME` doesn't expose
    /// this directly; tyto stores it under `pg_backend_pid()` when
    /// the session-status query returns it). Empty string if
    /// unknown.
    pub pid:         &'a str,
    pub txn_state:   TxnState,
    /// True only when the connection's role is a superuser. Tyto
    /// doesn't introspect this yet, so callers pass `false` and
    /// `%#` always expands to `>`. Future work: populate from
    /// `pg_roles.rolsuper` at SessionOpened time.
    pub is_superuser: bool,
    /// 1-indexed statement counter — the same `N` that prefixes the
    /// rendered query block in history.
    pub query_num:   u32,
    pub variables:   &'a HashMap<String, String>,
    /// False once a lost session couldn't be replaced (psql's `pset.db`
    /// is NULL): `%R` is `!`, `%x` is `?`, and the connection's own codes
    /// print nothing.
    pub connected:   bool,
    /// In an inactive branch of psql's `\if`: `%R` is `@`.
    pub inactive_branch: bool,
}

/// Render `template` with `%`-substitutions filled from `ctx`.
///
/// Walks the template as UTF-8 chars (not bytes) so multi-byte
/// codepoints — emojis, arrows, accented letters — survive in the
/// literal segments of the prompt. Only the `%`-code matching is
/// ASCII-narrow, which is correct: psql's `%`-codes are single
/// ASCII characters by spec.
pub(crate) fn format_prompt(template: &str, ctx: &PromptContext) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut i = 0;
    while i < template.len() {
        let ch = template[i..].chars().next().unwrap();
        let ch_len = ch.len_utf8();

        if ch != '%' {
            out.push(ch);
            i += ch_len;
            continue;
        }

        // ch == '%'. Peek the next char.
        let next_start = i + ch_len; // == i + 1 because '%' is one byte
        if next_start >= template.len() {
            // Trailing `%` with no code — emit it literally.
            out.push('%');
            i = next_start;
            continue;
        }
        let next_ch = template[next_start..].chars().next().unwrap();
        let next_len = next_ch.len_utf8();

        match next_ch {
            // The connection's own codes say nothing without one.
            'n' | '/' | '~' | 'M' | 'm' | '>' | 'p' if !ctx.connected => { i = next_start + next_len; }
            'n' => { out.push_str(ctx.user);    i = next_start + next_len; }
            '/' => { out.push_str(ctx.dbname);  i = next_start + next_len; }
            '~' => { out.push_str(ctx.dbname);  i = next_start + next_len; }
            'M' => {
                if ctx.host.is_empty() { out.push_str("[local]"); }
                else { out.push_str(ctx.host); }
                i = next_start + next_len;
            }
            'm' => {
                if ctx.host.is_empty() { out.push_str("[local]"); }
                else {
                    let short = ctx.host.split('.').next().unwrap_or(ctx.host);
                    out.push_str(short);
                }
                i = next_start + next_len;
            }
            '>' => { out.push_str(ctx.port); i = next_start + next_len; }
            'R' => {
                // Tyto submits the whole buffer per Enter — no
                // single-line / multi-line / open-literal sub-states
                // to track from the renderer's vantage. `=` matches
                // psql's "ready" state; `@` an inactive `\if` branch,
                // `!` "no connection", in psql's order.
                out.push(if ctx.inactive_branch { '@' } else if ctx.connected { '=' } else { '!' });
                i = next_start + next_len;
            }
            'x' => {
                match ctx.txn_state {
                    _ if !ctx.connected => out.push('?'),
                    TxnState::Idle   => {}
                    TxnState::Active => out.push('*'),
                    TxnState::Failed => out.push('!'),
                }
                i = next_start + next_len;
            }
            '#' => {
                out.push(if ctx.is_superuser { '#' } else { '>' });
                i = next_start + next_len;
            }
            'p' => { out.push_str(ctx.pid); i = next_start + next_len; }
            'l' => {
                out.push_str(&ctx.query_num.to_string());
                i = next_start + next_len;
            }
            '%' => { out.push('%'); i = next_start + next_len; }
            ':' => {
                // %:VAR: — find the next ':' and look up VAR.
                let var_start = next_start + next_len;
                if let Some(end_rel) = template[var_start..].find(':') {
                    let var_name = &template[var_start..var_start + end_rel];
                    if let Some(v) = ctx.variables.get(var_name) {
                        out.push_str(v);
                    } else {
                        // Unset variable — psql leaves it empty.
                    }
                    i = var_start + end_rel + 1; // past closing `:`
                } else {
                    // Unterminated %:… — pass through as-is.
                    out.push('%');
                    i = next_start;
                }
            }
            _ => {
                // Unknown / unsupported %-code — pass through
                // verbatim so a future addition (`%w`, `%[…%]`,
                // `%`cmd``) doesn't get eaten silently.
                out.push('%');
                out.push(next_ch);
                i = next_start + next_len;
            }
        }
    }
    out
}

/// psql's default for PROMPT1. Tyto's pre-customization rendering
/// was `"<N> SQL>"`; this template reproduces that exactly so the
/// new code path doesn't shift pixels for users on stock settings.
pub(crate) const DEFAULT_PROMPT1: &str = "%l SQL>";

/// psql's default for PROMPT2 (continuation lines inside a multi-
/// line buffer). Tyto submits per Enter, so PROMPT2 is currently
/// only visible in copied-block clipboard output and in any future
/// line-by-line mode.
pub(crate) const DEFAULT_PROMPT2: &str = "%l SQL-";

/// psql's default for PROMPT3 — shown during a `\copy` operation's
/// inline data block. Tyto doesn't support `\copy` yet (Wave 3
/// follow-up), so this is reserved.
pub(crate) const DEFAULT_PROMPT3: &str = ">>";

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn ctx<'a>(
        user: &'a str,
        dbname: &'a str,
        host: &'a str,
        port: &'a str,
        pid: &'a str,
        txn: TxnState,
        is_super: bool,
        query_num: u32,
        v: &'a HashMap<String, String>,
    ) -> PromptContext<'a> {
        PromptContext {
            user, dbname, host, port, pid,
            txn_state: txn, is_superuser: is_super, query_num, variables: v, connected: true, inactive_branch: false,
        }
    }

    #[test]
    fn no_codes_passes_through() {
        let v = vars(&[]);
        let c = ctx("u", "db", "h", "5432", "1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("hello world", &c), "hello world");
    }

    #[test]
    fn default_prompt1_matches_legacy_shape() {
        let v = vars(&[]);
        let c = ctx("u", "db", "h", "5432", "1", TxnState::Idle, false, 7, &v);
        // Pre-templated tyto rendered `"7 SQL>"`. Default template
        // must reproduce that EXACTLY so users on stock prompts see
        // no UI shift.
        assert_eq!(format_prompt(DEFAULT_PROMPT1, &c), "7 SQL>");
    }

    #[test]
    fn user_db_substitutions() {
        let v = vars(&[]);
        let c = ctx("alice", "pagila", "h", "5432", "1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("%n@%/", &c),  "alice@pagila");
        assert_eq!(format_prompt("%n=%~",  &c), "alice=pagila");
    }

    #[test]
    fn host_short_and_full() {
        let v = vars(&[]);
        let c = ctx("u", "db", "db.internal.example.com", "5432", "1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("%M",     &c), "db.internal.example.com");
        assert_eq!(format_prompt("%m",     &c), "db");
    }

    #[test]
    fn host_empty_renders_local() {
        let v = vars(&[]);
        let c = ctx("u", "db", "", "5432", "1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("%M",     &c), "[local]");
        assert_eq!(format_prompt("%m",     &c), "[local]");
    }

    #[test]
    fn port_and_pid_and_l() {
        let v = vars(&[]);
        let c = ctx("u", "db", "h", "5433", "9991", TxnState::Idle, false, 42, &v);
        assert_eq!(format_prompt("%>:%p L%l", &c), "5433:9991 L42");
    }

    #[test]
    fn state_indicator_R() {
        let v = vars(&[]);
        let c = ctx("u", "db", "h", "5432", "1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("%R",  &c), "=");
    }

    #[test]
    fn txn_state_x() {
        let v = vars(&[]);
        let i = ctx("u","db","h","5432","1", TxnState::Idle,   false, 1, &v);
        let a = ctx("u","db","h","5432","1", TxnState::Active, false, 1, &v);
        let f = ctx("u","db","h","5432","1", TxnState::Failed, false, 1, &v);
        assert_eq!(format_prompt("[%x]", &i), "[]");
        assert_eq!(format_prompt("[%x]", &a), "[*]");
        assert_eq!(format_prompt("[%x]", &f), "[!]");
    }

    /// psql with no connection (its reset failed): psql's default
    /// `%/%R%x%# ` reads `!?> `.
    #[test]
    fn disconnected() {
        let v = vars(&[]);
        let c = PromptContext { connected: false, ..ctx("u", "pagila", "h", "5432", "77", TxnState::Active, false, 3, &v) };
        // In an inactive `\if` branch `%R` is `@`, before `!` (psql's order).
        let skipping = PromptContext { inactive_branch: true, ..ctx("u", "pagila", "h", "5432", "77", TxnState::Idle, false, 3, &v) };
        assert_eq!(format_prompt("%R", &skipping), "@");
        assert_eq!(format_prompt("%/%R%x%# ", &c), "!?> ");
        assert_eq!(format_prompt("[%n|%M|%m|%>|%p|%~] %l", &c), "[|||||] 3");
    }

    #[test]
    fn privilege_indicator_hash() {
        let v = vars(&[]);
        let normal = ctx("u","db","h","5432","1", TxnState::Idle, false, 1, &v);
        let supe   = ctx("u","db","h","5432","1", TxnState::Idle, true,  1, &v);
        assert_eq!(format_prompt("%#", &normal), ">");
        assert_eq!(format_prompt("%#", &supe),   "#");
    }

    #[test]
    fn literal_percent() {
        let v = vars(&[]);
        let c = ctx("u","db","h","5432","1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("100%%",  &c), "100%");
        assert_eq!(format_prompt("%%/%/",  &c), "%/db");
    }

    #[test]
    fn unknown_code_passes_through() {
        let v = vars(&[]);
        let c = ctx("u","db","h","5432","1", TxnState::Idle, false, 1, &v);
        // Tyto doesn't implement `%w` etc. yet — they should
        // survive verbatim so a future expansion can wire them up.
        assert_eq!(format_prompt("%w done", &c), "%w done");
    }

    #[test]
    fn var_substitution_known() {
        let v = vars(&[("MYVAR", "hello"), ("OTHER", "world")]);
        let c = ctx("u","db","h","5432","1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("%:MYVAR:!", &c), "hello!");
        assert_eq!(format_prompt("%:MYVAR: + %:OTHER:", &c), "hello + world");
    }

    #[test]
    fn var_substitution_unknown_is_empty() {
        let v = vars(&[]);
        let c = ctx("u","db","h","5432","1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("[%:NOPE:]", &c), "[]");
    }

    #[test]
    fn var_substitution_unterminated_falls_through() {
        let v = vars(&[("M", "x")]);
        let c = ctx("u","db","h","5432","1", TxnState::Idle, false, 1, &v);
        // No closing `:` — psql leaves the `%` alone and continues
        // (our impl drops just the `%` and resumes).
        assert_eq!(format_prompt("%:UNCLOSED", &c), "%:UNCLOSED");
    }

    #[test]
    fn trailing_percent_alone() {
        let v = vars(&[]);
        let c = ctx("u","db","h","5432","1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("ends%",  &c), "ends%");
    }

    #[test]
    fn preserves_multibyte_utf8_in_literal_segments() {
        // Arrow (3 UTF-8 bytes), emoji (4 bytes), accented letter (2).
        // A byte-walking implementation would emit mojibake here.
        let v = vars(&[]);
        let c = ctx("u", "db", "h", "5432", "1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("→%n", &c), "→u");
        assert_eq!(format_prompt("%n 🐍 %/", &c), "u 🐍 db");
        assert_eq!(format_prompt("café %l", &c), "café 1");
    }

    #[test]
    fn typical_psql_default() {
        // psql's actual default PROMPT1 is `%/%R%# `. Render against
        // a realistic ctx to confirm the assembly works.
        let v = vars(&[]);
        let c = ctx("u","pagila","h","5432","1", TxnState::Idle, false, 1, &v);
        assert_eq!(format_prompt("%/%R%# ", &c), "pagila=> ");
    }
}
