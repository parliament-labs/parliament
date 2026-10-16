//! Reader for `~/.pgpass` — libpq's convention for storing PG credentials
//! outside the connection string.
//!
//! Standard format: one entry per line, five colon-separated fields
//!
//!     hostname:port:database:username:password
//!
//! Any of the first four fields can be `*` (wildcard). The first line
//! that matches all four keys wins. Literal `:` and `\` inside a field
//! are backslash-escaped (`\:` and `\\`). Empty lines and lines starting
//! with `#` are ignored.
//!
//! libpq refuses to read `.pgpass` unless it is `chmod 0600` — this
//! module enforces the same on Unix. On Windows the file is read without
//! a permission check (matching libpq's Windows behaviour).
//!
//! The file location is `$PGPASSFILE` if set, else `$HOME/.pgpass`.

use std::path::{Path, PathBuf};

/// Look up the password for `(host, port, database, user)` in
/// `~/.pgpass`. Returns:
///
/// - `Ok(password)` on the first matching line, with `\:` / `\\`
///   escape sequences decoded.
/// - `Err("~/.pgpass not found")` when the file is missing.
/// - `Err("~/.pgpass has permissions 0644; libpq requires 0600. Run: chmod 0600 <path>")`
///   when the file's mode isn't `0600` on Unix.
/// - `Err("no matching ~/.pgpass entry for {user}@{host}:{port}/{database}")`
///   when the file exists and is readable but no line matches.
pub(crate) fn lookup(
    host: &str,
    port: u16,
    database: &str,
    user: &str,
) -> Result<String, String> {
    let path = resolve_path(|name| std::env::var(name).ok())?;
    lookup_in(&path, host, port, database, user)
}

/// [`lookup`] against the file at `path`.
fn lookup_in(
    path: &Path,
    host: &str,
    port: u16,
    database: &str,
    user: &str,
) -> Result<String, String> {
    let display = path.display().to_string();
    // Permission check runs BEFORE we read any bytes off disk. libpq
    // does it this way — a world-readable ~/.pgpass shouldn't have
    // its contents loaded into process memory just to be rejected
    // afterwards. `check_permissions` also folds in the missing-file
    // path so it and the read-fallback below emit the same "not
    // found" error rather than a stat-error variant. On non-Unix the
    // check is a no-op and read handles the NotFound path directly.
    #[cfg(unix)]
    check_permissions(path, &display)?;
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("{} not found", display));
        }
        Err(e) => return Err(format!("read {}: {}", display, e)),
    };

    let port_s = port.to_string();
    for raw in contents.lines() {
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(fields) = split_pgpass_line(line) else {
            continue;
        };
        // fields is exactly 5: [host, port, db, user, password].
        // `split_pgpass_line` has already decoded `\:` / `\\` escapes
        // for every field including the password — return the fifth
        // field verbatim. A previous version called `unescape()` on
        // top of that, corrupting any password containing a literal
        // backslash (double-decode dropped the escape prefix).
        if field_matches(&fields[0], host)
            && field_matches(&fields[1], &port_s)
            && field_matches(&fields[2], database)
            && field_matches(&fields[3], user)
        {
            return Ok(fields[4].clone());
        }
    }
    Err(format!(
        "no matching {} entry for {}@{}:{}/{}",
        display, user, host, port, database
    ))
}

/// `$PGPASSFILE` if set and non-empty, else `$HOME/.pgpass`. The
/// environment is passed in so tests never touch the process's own.
fn resolve_path(env: impl Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    if let Some(env_path) = env("PGPASSFILE").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(env_path));
    }
    let home = env("HOME").ok_or_else(|| "$HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".pgpass"))
}

#[cfg(unix)]
fn check_permissions(path: &Path, display: &str) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        // Missing file uses the same wording the read fallback uses so
        // callers see one consistent error regardless of which layer
        // detects it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("{} not found", display));
        }
        Err(e) => return Err(format!("stat {}: {}", display, e)),
    };
    let mode = meta.mode() & 0o777;
    if mode != 0o600 {
        return Err(format!(
            "{} has permissions {:04o}; libpq requires 0600. Run: chmod 0600 {}",
            display, mode, display
        ));
    }
    Ok(())
}

/// Split a `.pgpass` line into 5 fields, honouring `\:` (literal colon)
/// and `\\` (literal backslash). Returns `None` if the line doesn't
/// produce exactly 5 fields — malformed lines are skipped by the caller.
fn split_pgpass_line(line: &str) -> Option<[String; 5]> {
    let mut fields: Vec<String> = Vec::with_capacity(5);
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                // Backslash escapes the next character (colon or backslash).
                // Any other next character passes through literally; the
                // trailing-backslash edge case appends the backslash itself.
                match chars.next() {
                    Some(next) => current.push(next),
                    None => current.push('\\'),
                }
            }
            ':' => {
                fields.push(std::mem::take(&mut current));
                if fields.len() == 5 {
                    // Fifth colon means the line has too many fields;
                    // the password field can itself contain unescaped
                    // content but not additional colons at the top
                    // level. Reject malformed lines.
                    return None;
                }
            }
            other => current.push(other),
        }
    }
    fields.push(current);
    if fields.len() != 5 {
        return None;
    }
    // Vec -> array. Safe: we just checked the length.
    let arr: [String; 5] = fields.try_into().ok()?;
    Some(arr)
}

/// A field matches when it's a literal-value equality OR the pattern is
/// `*` (wildcard). Case-sensitive per libpq's own behaviour.
fn field_matches(pattern: &str, value: &str) -> bool {
    pattern == "*" || pattern == value
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── split_pgpass_line ─────────────────────────────────────────────

    #[test]
    fn splits_simple_line() {
        let f = split_pgpass_line("localhost:5432:pagila:postgres:secret").unwrap();
        assert_eq!(f, ["localhost", "5432", "pagila", "postgres", "secret"].map(String::from));
    }

    #[test]
    fn splits_wildcard_line() {
        let f = split_pgpass_line("*:*:*:*:hunter2").unwrap();
        assert_eq!(f, ["*", "*", "*", "*", "hunter2"].map(String::from));
    }

    #[test]
    fn escaped_colon_in_password_is_literal() {
        let f = split_pgpass_line("localhost:5432:pagila:postgres:p\\:a\\:s\\:s").unwrap();
        assert_eq!(f[4], "p:a:s:s");
    }

    #[test]
    fn escaped_backslash_in_password_is_literal() {
        let f = split_pgpass_line(r"localhost:5432:pagila:postgres:back\\slash").unwrap();
        assert_eq!(f[4], r"back\slash");
    }

    #[test]
    fn too_few_fields_returns_none() {
        assert!(split_pgpass_line("only:four:fields:here").is_none());
    }

    #[test]
    fn too_many_fields_returns_none() {
        assert!(split_pgpass_line("a:b:c:d:e:f").is_none());
    }

    // ── field_matches ─────────────────────────────────────────────────

    #[test]
    fn wildcard_matches_anything() {
        assert!(field_matches("*", "anything"));
        assert!(field_matches("*", ""));
    }

    #[test]
    fn literal_matches_exact_case() {
        assert!(field_matches("Postgres", "Postgres"));
        assert!(!field_matches("Postgres", "postgres"));
    }

    // ── lookup (integration-ish via a tempfile) ───────────────────────
    //
    // Every test reads its own file through `lookup_in`. None of them
    // sets $PGPASSFILE: tests run on parallel threads in one process,
    // and they used to overwrite each other's value mid-lookup.

    /// Test-only file writer that also sets 0600 mode on Unix so
    /// `check_permissions` is satisfied.
    fn write_pgpass(dir: &Path, contents: &str) -> PathBuf {
        let path = dir.join("pgpass");
        std::fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&path).unwrap().permissions();
            perm.set_mode(0o600);
            std::fs::set_permissions(&path, perm).unwrap();
        }
        path
    }

    /// Look up `postgres@localhost:5432/pagila` in a file holding `contents`.
    fn lookup_pagila(contents: &str) -> Result<String, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = write_pgpass(dir.path(), contents);
        lookup_in(&path, "localhost", 5432, "pagila", "postgres")
    }

    #[test]
    fn path_is_pgpassfile_else_home() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        assert_eq!(resolve_path(env(&[("PGPASSFILE", "/etc/pg/pass"), ("HOME", "/h")])), Ok("/etc/pg/pass".into()));
        assert_eq!(resolve_path(env(&[("PGPASSFILE", ""), ("HOME", "/h")])), Ok("/h/.pgpass".into()));
        assert_eq!(resolve_path(env(&[("HOME", "/h")])), Ok("/h/.pgpass".into()));
        assert!(resolve_path(env(&[])).unwrap_err().contains("$HOME is not set"));
    }

    #[test]
    fn lookup_exact_match() {
        assert_eq!(lookup_pagila("localhost:5432:pagila:postgres:secret\n").unwrap(), "secret");
    }

    #[test]
    fn lookup_wildcards() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_pgpass(dir.path(), "*:*:*:*:catchall\n");
        assert_eq!(lookup_in(&path, "any-host", 6543, "any-db", "any-user").unwrap(), "catchall");
    }

    #[test]
    fn lookup_first_match_wins() {
        let r = lookup_pagila(
            "localhost:5432:pagila:postgres:first\n\
             *:*:*:*:catchall\n",
        );
        assert_eq!(r.unwrap(), "first");
    }

    #[test]
    fn lookup_skips_comments_and_blanks() {
        let r = lookup_pagila(
            "# this line is a comment\n\
             \n\
             localhost:5432:pagila:postgres:real\n",
        );
        assert_eq!(r.unwrap(), "real");
    }

    #[test]
    fn lookup_no_match_returns_error() {
        let err = lookup_pagila("otherhost:5432:otherdb:otheruser:x\n").unwrap_err();
        assert!(err.contains("no matching"));
        assert!(err.contains("postgres"));
        assert!(err.contains("localhost"));
    }

    #[test]
    fn lookup_backslash_and_colon_in_password_survive_full_path() {
        // Regression: a prior implementation ran the password through
        // an `unescape` decoder after `split_pgpass_line` had already
        // decoded escapes, so `back\slash` (encoded as `back\\slash`)
        // came out as `backslash`. This test drives the full `lookup`
        // path with a password containing both a literal backslash
        // and a literal colon and asserts the exact bytes come back.
        //
        // In the file on disk:
        //   password field is `p\\ass:with\\slash\:and\:colons`
        //   which decodes (single decode) to `p\ass:with\slash:and:colons`.
        let r = lookup_pagila("localhost:5432:pagila:postgres:p\\\\ass\\:with\\\\slash\\:and\\:colons\n");
        assert_eq!(r.unwrap(), r"p\ass:with\slash:and:colons");
    }

    #[test]
    fn lookup_missing_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let ghost = dir.path().join("does-not-exist");
        let err = lookup_in(&ghost, "localhost", 5432, "pagila", "postgres").unwrap_err();
        assert!(err.contains("not found"), "expected 'not found', got: {}", err);
    }

    #[cfg(unix)]
    #[test]
    fn lookup_rejects_wrong_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pgpass_bad");
        std::fs::write(&path, "localhost:5432:pagila:postgres:x\n").unwrap();
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o644);
        std::fs::set_permissions(&path, perm).unwrap();
        let err = lookup_in(&path, "localhost", 5432, "pagila", "postgres").unwrap_err();
        assert!(err.contains("0644"), "expected mode in error, got: {}", err);
        assert!(err.contains("chmod 0600"), "expected chmod hint, got: {}", err);
    }
}
