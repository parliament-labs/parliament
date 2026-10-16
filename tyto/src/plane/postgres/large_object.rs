//! psql's `\lo_unlink` (large_obj.c `do_lo_unlink`): the large object
//! removed by the server's `lo_unlink` — which psql calls through libpq's
//! fast path, so `ECHO_HIDDEN` never shows it (`MetaStep::Call`) — in a
//! transaction of its own outside one (`start_lo_xact`: `BEGIN` first,
//! `COMMIT` after unless `AUTOCOMMIT` is off, `ROLLBACK` when it fails),
//! inside the session's when there is one; refused in a failed one.

use shared::{BatchResult, TxnState};

use super::error::psql_error;
use crate::plane::{MetaContext, MetaStep};
use crate::types::{MetaAction, OutputFormat};
use crate::ShellEntry;

/// `\lo_unlink LOBOID`: its first step — `BEGIN` outside a transaction,
/// nothing inside one (the call comes next) — or psql's refusal.
pub(super) fn unlink(arg: Option<&str>, ctx: &MetaContext) -> MetaAction {
    if arg.is_none() {
        return MetaAction::Fail(vec!["\\lo_unlink: missing required argument".into()]);
    }
    match ctx.txn {
        TxnState::Idle => MetaAction::Staged(vec!["BEGIN".into()]),
        TxnState::Active => MetaAction::Staged(Vec::new()),
        TxnState::Failed => MetaAction::Fail(vec!["\\lo_unlink: current transaction is aborted".into()]),
    }
}

/// What follows each reply: the call, then — in its own transaction,
/// with `AUTOCOMMIT` on — `COMMIT`, or `ROLLBACK` when the call (or the
/// `COMMIT`) failed; then `lo_unlink 16405`, or the error.
pub(super) fn unlink_next(arg: &str, ctx: &MetaContext, replies: &[BatchResult]) -> MetaStep {
    let own = ctx.txn == TxnState::Idle;
    let closes = own && ctx.variables.get("AUTOCOMMIT").and_then(|v| crate::psql_vars::parse_bool(v)).unwrap_or(true);
    let oid = atooid(arg);
    let failed = |reply: &BatchResult| reply.error.as_ref().map(|error| ShellEntry::CliError {
        lines:   psql_error(error, None, ctx.error_style),
        message: error.message.clone(),
    });
    // psql's `print_lo_result`: nothing under `QUIET`.
    let done = || {
        if super::quiet(ctx) {
            return MetaStep::Done(Vec::new());
        }
        let said = format!("lo_unlink {oid}");
        MetaStep::Done(vec![ShellEntry::MetaOutput(vec![match ctx.print.format {
            OutputFormat::Html => format!("<p>{said}</p>"),
            _ => said,
        }])])
    };
    match replies {
        // `BEGIN` (or nothing): refused, it says so; else the call.
        [first] => match failed(first) {
            Some(error) => MetaStep::Failed(vec![error]),
            None => MetaStep::Call(format!("SELECT pg_catalog.lo_unlink('{oid}'::pg_catalog.oid)")),
        },
        // The call: what closes its own transaction, if anything.
        [_, call] => match (failed(call), closes) {
            (Some(_), true) => MetaStep::Run(vec!["ROLLBACK".into()]),
            (Some(error), false) => MetaStep::Failed(vec![error]),
            (None, true) => MetaStep::Run(vec!["COMMIT".into()]),
            (None, false) => done(),
        },
        // The call failed and was rolled back: its error (and a failed
        // `ROLLBACK`'s, as psql's `PSQLexec` says it). Before the COMMIT
        // arm, which the same shape would match.
        [_, call, rollback] if call.error.is_some() => {
            MetaStep::Failed(failed(call).into_iter().chain(failed(rollback)).collect())
        }
        // `COMMIT`: refused, roll back; else done.
        [_, _, commit] => match failed(commit) {
            Some(_) => MetaStep::Run(vec!["ROLLBACK".into()]),
            None => done(),
        },
        [_, _, commit, rollback] => MetaStep::Failed(failed(commit).into_iter().chain(failed(rollback)).collect()),
        _ => MetaStep::Done(Vec::new()),
    }
}

/// libpq's `atooid`: `strtoul(s, NULL, 10)` cast to an `Oid` — leading
/// space and a sign skipped, the digits that follow read (none: 0), past
/// `ULONG_MAX` that, a minus negating it as unsigned, the low 32 bits
/// kept (`-1` is 4294967295, `abc` 0).
fn atooid(s: &str) -> u32 {
    let s = s.trim_start_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r']);
    let (negative, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits = &s[..s.bytes().take_while(u8::is_ascii_digit).count()];
    if digits.is_empty() {
        return 0;
    }
    let value = match digits.parse::<u64>() {
        Ok(value) if negative => value.wrapping_neg(),
        Ok(value) => value,
        Err(_) => u64::MAX,
    };
    value as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::DbError;

    fn ctx(txn: TxnState, autocommit: bool) -> MetaContext {
        let mut ctx = MetaContext { txn, ..MetaContext::default() };
        if !autocommit {
            ctx.variables.insert("AUTOCOMMIT".into(), "off".into());
        }
        ctx
    }
    fn ok() -> BatchResult { BatchResult::default() }
    fn refused(message: &str) -> BatchResult {
        BatchResult { sets: Vec::new(), error: Some(DbError { severity: Some("ERROR".into()), message: message.into(), ..DbError::default() }) }
    }
    fn errors(step: MetaStep) -> Vec<String> {
        match step {
            MetaStep::Failed(entries) => entries.into_iter().flat_map(|e| match e {
                ShellEntry::CliError { lines, .. } => lines,
                _ => Vec::new(),
            }).collect(),
            _ => panic!("not a failure"),
        }
    }

    #[test]
    fn atooid_is_libpqs() {
        assert_eq!(atooid("16405"), 16405);
        assert_eq!(atooid("abc"), 0);
        assert_eq!(atooid(""), 0);
        assert_eq!(atooid("  12abc"), 12);
        assert_eq!(atooid("-1"), 4_294_967_295);
        assert_eq!(atooid("+7"), 7);
        assert_eq!(atooid("99999999999"), 1_215_752_191);
        assert_eq!(atooid("99999999999999999999999"), 4_294_967_295);
    }

    #[test]
    fn unlink_starts_as_psqls_start_lo_xact() {
        assert!(matches!(unlink(None, &ctx(TxnState::Idle, true)), MetaAction::Fail(l) if l == ["\\lo_unlink: missing required argument"]));
        assert!(matches!(unlink(Some("1"), &ctx(TxnState::Idle, true)), MetaAction::Staged(q) if q == ["BEGIN"]));
        assert!(matches!(unlink(Some("1"), &ctx(TxnState::Active, true)), MetaAction::Staged(q) if q.is_empty()));
        assert!(matches!(unlink(Some("1"), &ctx(TxnState::Failed, true)),
            MetaAction::Fail(l) if l == ["\\lo_unlink: current transaction is aborted"]));
    }

    #[test]
    fn unlink_goes_on_as_psqls_do_lo_unlink() {
        let call = |step: MetaStep| match step { MetaStep::Call(sql) => sql, _ => panic!("no call") };
        let run = |step: MetaStep| match step { MetaStep::Run(q) => q, _ => panic!("no batch") };
        let done = |step: MetaStep| match step {
            MetaStep::Done(entries) => match &entries[..] { [ShellEntry::MetaOutput(lines)] => lines.clone(), _ => panic!() },
            _ => panic!("not done"),
        };
        // Its own transaction: BEGIN, the call, COMMIT, then what it says.
        let own = ctx(TxnState::Idle, true);
        assert_eq!(call(unlink_next("-1", &own, &[ok()])), "SELECT pg_catalog.lo_unlink('4294967295'::pg_catalog.oid)");
        assert_eq!(run(unlink_next("16405", &own, &[ok(), ok()])), ["COMMIT"]);
        assert_eq!(done(unlink_next("16405", &own, &[ok(), ok(), ok()])), ["lo_unlink 16405"]);
        // The call refused: ROLLBACK, then its error.
        assert_eq!(run(unlink_next("9", &own, &[ok(), refused("large object 9 does not exist")])), ["ROLLBACK"]);
        assert_eq!(errors(unlink_next("9", &own, &[ok(), refused("large object 9 does not exist"), ok()])),
                   ["ERROR:  large object 9 does not exist"]);
        // COMMIT refused: ROLLBACK, then its error.
        assert_eq!(run(unlink_next("9", &own, &[ok(), ok(), refused("could not commit")])), ["ROLLBACK"]);
        assert_eq!(errors(unlink_next("9", &own, &[ok(), ok(), refused("could not commit"), ok()])),
                   ["ERROR:  could not commit"]);
        // BEGIN refused: its error.
        assert_eq!(errors(unlink_next("9", &own, &[refused("no")])), ["ERROR:  no"]);
        // AUTOCOMMIT off: its transaction stays open, as psql's does.
        let open = ctx(TxnState::Idle, false);
        assert_eq!(done(unlink_next("5", &open, &[ok(), ok()])), ["lo_unlink 5"]);
        assert_eq!(errors(unlink_next("5", &open, &[ok(), refused("large object 5 does not exist")])),
                   ["ERROR:  large object 5 does not exist"]);
        // Inside the session's transaction: the call alone.
        let inside = ctx(TxnState::Active, true);
        assert!(matches!(unlink_next("5", &inside, &[ok()]), MetaStep::Call(_)));
        assert_eq!(done(unlink_next("5", &inside, &[ok(), ok()])), ["lo_unlink 5"]);
        assert_eq!(errors(unlink_next("5", &inside, &[ok(), refused("large object 5 does not exist")])),
                   ["ERROR:  large object 5 does not exist"]);
        // Under `QUIET`, nothing.
        let mut quiet = inside.clone();
        quiet.variables.insert("QUIET".into(), "on".into());
        assert!(matches!(unlink_next("5", &quiet, &[ok(), ok()]), MetaStep::Done(entries) if entries.is_empty()));
        // Under `\pset format html`, a paragraph.
        let mut html = inside.clone();
        html.print.format = OutputFormat::Html;
        assert_eq!(done(unlink_next("5", &html, &[ok(), ok()])), ["<p>lo_unlink 5</p>"]);
    }
}
