//! Test-only: the opt-in parity tests' bridge to psql. The PG plane's
//! meta-commands run against the local Pagila database — psql on PATH,
//! PGPASSWORD from ~/.parliament/secrets.env — their queries through
//! psql, their output as psql would print it.

use shared::{BatchResult, DbError, ResultSet};

use super::PostgresPlane;
use crate::plane::{DbmsPlane, MetaContext, MetaStep};
use crate::types::{MetaAction, ShellEntry};

/// Separators no catalog value holds, so a result reads back exactly,
/// SQL NULL kept apart from '' — psql's `\pset fieldsep '\037'`,
/// `recordsep '\036\n'`, `null '\001NULL\001'`.
pub(super) const FIELD: &str = "\u{1f}";
pub(super) const RECORD: &str = "\u{1e}\n";
pub(super) const NULL: &str = "\u{1}NULL\u{1}";

/// psql on Pagila as postgres, with `args`.
pub(super) fn psql(args: &[&str]) -> std::process::Output {
    std::process::Command::new("psql")
        .args(["-X", "-d", "pagila", "-U", "postgres"]).args(args)
        .output().expect("psql on PATH")
}

/// `sql` run by psql in a session of its own: its rows, or its error —
/// a `DbError`, as the plane gets in a `BatchResult`.
#[allow(clippy::result_large_err)]
pub(super) fn query(sql: &str) -> Result<ResultSet, DbError> {
    let out = psql(&["-q", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-P", &format!("fieldsep={FIELD}"),
                     "-P", &format!("recordsep={RECORD}"), "-P", &format!("null={NULL}"), "-c", sql]);
    if out.status.success() {
        Ok(read_rows(&String::from_utf8_lossy(&out.stdout)))
    } else {
        Err(psql_error_text(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// psql's rows, printed with [`FIELD`] / [`RECORD`] / [`NULL`].
pub(super) fn read_rows(text: &str) -> ResultSet {
    let rows = if text.is_empty() { Vec::new() } else {
        text.strip_suffix('\n').unwrap_or(text).split(RECORD)
            .map(|r| r.split(FIELD).map(|f| (f != NULL).then(|| f.to_string())).collect())
            .collect()
    };
    ResultSet { columns: Vec::new(), rows }
}

/// psql's `SEVERITY:  message` (first line of its stderr) as an error.
fn psql_error_text(stderr: &str) -> DbError {
    let first = stderr.lines().next().unwrap_or_default();
    match first.split_once(":  ") {
        Some((severity, message)) => DbError {
            severity: Some(severity.to_string()), message: message.to_string(), ..DbError::default()
        },
        None => DbError::from_message(first),
    }
}

/// `cmd` run through the PG plane — a staged command step by step, each
/// query run by `query` — as psql would print the result.
pub(super) fn staged_output(ctx: &MetaContext, cmd: &str, query: &mut dyn FnMut(&str) -> Result<ResultSet, DbError>) -> String {
    let mut queries = match PostgresPlane.meta_cmd_in(cmd, ctx) {
        Some(MetaAction::Staged(q)) => q,
        // psql's refusals, said as psql says them.
        Some(MetaAction::Output(lines) | MetaAction::Fail(lines)) => return lines.join("\n") + "\n",
        _ => panic!("{cmd}: not a staged command"),
    };
    let mut replies = Vec::new();
    loop {
        let mut batch = BatchResult::default();
        for q in &queries {
            match query(q) {
                Ok(set) => batch.sets.push(set),
                Err(e) => { batch.error = Some(e); break; }
            }
        }
        replies.push(batch);
        match PostgresPlane.meta_continue(cmd, ctx, &replies) {
            MetaStep::Run(next) => queries = next,
            MetaStep::Call(sql) => queries = vec![sql],
            MetaStep::Done(entries) | MetaStep::Failed(entries) => {
                return entries.iter().map(|e| match e {
                    ShellEntry::Printed { lines, .. } => lines.iter().map(|l| format!("{l}\n")).collect(),
                    ShellEntry::MetaOutput(lines) => lines.join("\n") + "\n",
                    ShellEntry::CliError { lines, .. } => lines.iter().map(|l| format!("{l}\n")).collect(),
                    _ => String::new(),
                }).collect();
            }
        }
    }
}

/// One psql session reading commands from a pipe, so a fixture built in
/// an open transaction is what the comparison then sees. Each step's
/// output ends at a marker line; psql's stderr is dropped.
pub(super) struct Session {
    stdin:  std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    _child: std::process::Child,
}

/// `\pset`s under which a result reads back exactly (see [`FIELD`] …).
const ROWS_MODE: &str = "\\pset format unaligned\n\\pset tuples_only on\n\\pset null '\\001NULL\\001'\n\
                         \\pset fieldsep '\\037'\n\\pset recordsep '\\036\\n'";
/// psql's defaults, for comparing what it prints.
const TEXT_MODE: &str = "\\pset format aligned\n\\pset tuples_only off\n\\pset null ''\n\
                         \\pset fieldsep '|'\n\\pset recordsep '\\n'";

impl Session {
    pub(super) fn open(db: &str) -> Session {
        use std::process::{Command, Stdio};
        let mut child = Command::new("psql")
            .args(["-X", "-q", "-d", db, "-U", "postgres", "-v", "ON_ERROR_STOP=0", "-P", "pager=off"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().expect("psql on PATH");
        Session {
            stdin:  child.stdin.take().expect("stdin"),
            stdout: std::io::BufReader::new(child.stdout.take().expect("stdout")),
            _child: child,
        }
    }

    /// Send `input`; return what psql printed for it.
    pub(super) fn step(&mut self, input: &str) -> String {
        use std::io::{BufRead, Write};
        const END: &str = "<<end of step>>";
        writeln!(self.stdin, "{input}\n\\echo '{END}'").expect("write");
        self.stdin.flush().expect("flush");
        let mut out = String::new();
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line).expect("read") == 0 || line.trim_end() == END { return out; }
            out.push_str(&line);
        }
    }

    /// `sql`'s rows, read back exactly.
    pub(super) fn rows(&mut self, sql: &str) -> ResultSet {
        self.step(ROWS_MODE);
        read_rows(&self.step(&format!("{sql}\n;")))
    }

    /// What psql prints for `cmd` with its default settings.
    pub(super) fn printed(&mut self, cmd: &str) -> String {
        self.step(TEXT_MODE);
        self.step(cmd)
    }
}
