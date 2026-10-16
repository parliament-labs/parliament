//! psql 18's `\conninfo` (command.c `exec_command_conninfo`): a table of
//! the session as libpq sees it. What the server knows comes from one
//! catalog query; what only the client knows — who and where it
//! connected, whether the server asked for a password, its TLS library —
//! from the shell (`MetaContext`, filled from `SessionOpened`).

use shared::BatchResult;

use super::describe::batch_error;
use super::print::{print_table, PrintTable};
use crate::plane::{MetaContext, MetaStep};
use crate::ShellEntry;

/// What the server says of the session: the address the client reached
/// it at (libpq's `PQhostaddr`, which the server sees as its own), the
/// backend's PID, its TLS, and the parameters psql reads from the
/// session's status (`in_hot_standby` from PG 14).
pub(super) const QUERY: &str = "SELECT pg_catalog.host(pg_catalog.inet_server_addr()), pg_catalog.pg_backend_pid(), \
    s.ssl, s.version, s.bits, s.cipher, \
    pg_catalog.current_setting('is_superuser'), pg_catalog.current_setting('in_hot_standby', true) \
    FROM (SELECT 1) AS one LEFT JOIN pg_catalog.pg_stat_ssl s ON s.pid = pg_catalog.pg_backend_pid()";

/// psql's table, once the query's reply is in.
pub(super) fn next(ctx: &MetaContext, replies: &[BatchResult]) -> MetaStep {
    let Some(reply) = replies.first() else { return MetaStep::Done(Vec::new()) };
    if let Some(error) = batch_error(reply, &[QUERY.to_string()], ctx) {
        return MetaStep::Failed(vec![error]);
    }
    let row = reply.sets.first().and_then(|set| set.rows.first()).cloned().unwrap_or_default();
    let server = |col: usize| row.get(col).cloned().flatten();
    MetaStep::Done(vec![ShellEntry::Printed {
        lines: print_table(&table(ctx, server), &ctx.print),
        links: Vec::new(),
    }])
}

/// The rows psql prints, in its order; `server` is the query's column.
fn table(ctx: &MetaContext, server: impl Fn(usize) -> Option<String>) -> PrintTable {
    let known = |value: Option<String>| value.unwrap_or_else(|| "unknown".into());
    let said = |yes: bool| (if yes { "true" } else { "false" }).to_string();
    let mut rows: Vec<(&str, String)> = vec![
        ("Database", ctx.database.clone()),
        ("Client User", ctx.user.clone()),
    ];
    // A host that's a socket directory (libpq's `is_unixsock_path`)
    // shows as one; otherwise the address reached, when it isn't the
    // host's own text (`localhost` → `::1`).
    let address = server(0).filter(|a| !a.is_empty());
    if ctx.host.starts_with('/') || ctx.host.starts_with('@') {
        match address {
            Some(address) => rows.push(("Host Address", address)),
            None => rows.push(("Socket Directory", ctx.host.clone())),
        }
    } else {
        rows.push(("Host", ctx.host.clone()));
        if let Some(address) = address.filter(|a| *a != ctx.host) {
            rows.push(("Host Address", address));
        }
    }
    let ssl = server(2).as_deref() == Some("t");
    rows.extend([
        ("Server Port", ctx.port.clone()),
        // noctua sends no `options`, and speaks protocol 3.0 without
        // GSSAPI.
        ("Options", String::new()),
        ("Protocol Version", "3.0".into()),
        ("Password Used", said(ctx.password_used)),
        ("GSSAPI Authenticated", said(false)),
        ("Backend PID", known(server(1))),
        ("SSL Connection", said(ssl)),
    ]);
    if ssl {
        rows.extend([
            ("SSL Library", known(Some(ctx.ssl_library.clone()).filter(|l| !l.is_empty()))),
            ("SSL Protocol", known(server(3))),
            ("SSL Key Bits", known(server(4))),
            ("SSL Cipher", known(server(5))),
            ("SSL Compression", said(false)),
            // noctua's TLS asks for no ALPN protocol.
            ("ALPN", "none".into()),
        ]);
    }
    rows.extend([
        ("Superuser", known(server(6))),
        ("Hot Standby", known(server(7))),
    ]);
    PrintTable {
        title:   Some("Connection Information".into()),
        headers: vec!["Parameter".into(), "Value".into()],
        aligns:  Vec::new(),
        footers: Vec::new(),
        count_rows: true,
        rows:    rows.into_iter().map(|(name, value)| vec![name.to_string(), value]).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::ResultSet;

    fn ctx(host: &str, password_used: bool) -> MetaContext {
        MetaContext {
            database: "pagila".into(),
            user: "dbadmin".into(),
            host: host.into(),
            port: "5432".into(),
            password_used,
            ssl_library: "OpenSSL".into(),
            ..MetaContext::default()
        }
    }

    fn reply(row: &[Option<&str>]) -> BatchResult {
        BatchResult {
            sets: vec![ResultSet { columns: Vec::new(), rows: vec![row.iter().map(|v| v.map(String::from)).collect()] }],
            error: None,
        }
    }

    fn printed(step: MetaStep) -> Vec<String> {
        match step {
            MetaStep::Done(entries) => match &entries[..] {
                [ShellEntry::Printed { lines, .. }] => lines.clone(),
                _ => panic!("not one printed block"),
            },
            _ => panic!("not done"),
        }
    }

    #[test]
    fn prints_psqls_table() {
        // psql 18.4, `\conninfo` over 127.0.0.1 with trust auth.
        let local = reply(&[Some("127.0.0.1"), Some("76539"), Some("f"), None, None, None, Some("on"), Some("off")]);
        assert_eq!(printed(next(&ctx("127.0.0.1", false), &[local])), [
            "      Connection Information",
            "      Parameter       |   Value   ",
            "----------------------+-----------",
            " Database             | pagila",
            " Client User          | dbadmin",
            " Host                 | 127.0.0.1",
            " Server Port          | 5432",
            " Options              | ",
            " Protocol Version     | 3.0",
            " Password Used        | false",
            " GSSAPI Authenticated | false",
            " Backend PID          | 76539",
            " SSL Connection       | false",
            " Superuser            | on",
            " Hot Standby          | off",
            "(12 rows)",
            "",
        ]);
    }

    #[test]
    fn x_prints_it_expanded_as_psql_does() {
        // psql 18.4, `\x on` then `\conninfo`.
        let local = reply(&[Some("127.0.0.1"), Some("76539"), Some("f"), None, None, None, Some("on"), Some("off")]);
        let ctx = MetaContext {
            print: crate::types::PrintOptions { expanded: crate::types::Expanded::On, ..Default::default() },
            ..ctx("127.0.0.1", false)
        };
        let lines = printed(next(&ctx, &[local]));
        assert_eq!(lines[..5], [
            "Connection Information",
            "-[ RECORD 1 ]-------------------",
            "Parameter | Database",
            "Value     | pagila",
            "-[ RECORD 2 ]-------------------",
        ]);
        assert_eq!(lines[lines.len() - 4..], ["-[ RECORD 12 ]------------------", "Parameter | Hot Standby", "Value     | off", ""]);
    }

    #[test]
    fn the_address_reached_shows_when_it_isnt_the_host() {
        // psql 18.4 over `localhost`: `Host Address | ::1`.
        let lines = printed(next(&ctx("localhost", false), &[reply(&[Some("::1"), Some("1"), Some("f"), None, None, None, Some("on"), Some("off")])]));
        assert!(lines.contains(&" Host Address         | ::1".to_string()), "{lines:?}");
        assert_eq!(lines[lines.len() - 2], "(13 rows)");
        // A socket directory, and an older server without `in_hot_standby`.
        let lines = printed(next(&ctx("/tmp", true), &[reply(&[None, Some("1"), None, None, None, None, Some("off"), None])]));
        assert!(lines.iter().any(|l| l.starts_with(" Socket Directory") && l.ends_with("| /tmp")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with(" Password Used") && l.ends_with("| true")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with(" Hot Standby") && l.ends_with("| unknown")), "{lines:?}");
    }

    #[test]
    fn tls_adds_psqls_six_rows() {
        let lines = printed(next(&ctx("db.example.com", true), &[reply(&[Some("10.0.0.5"), Some("1"), Some("t"), Some("TLSv1.3"), Some("256"), Some("TLS_AES_256_GCM_SHA384"), Some("off"), Some("off")])]));
        for row in ["SSL Connection       | true", "SSL Library          | OpenSSL", "SSL Protocol         | TLSv1.3",
                    "SSL Key Bits         | 256", "SSL Cipher           | TLS_AES_256_GCM_SHA384", "SSL Compression      | false", "ALPN                 | none"] {
            assert!(lines.iter().any(|l| l.trim_start() == row), "{row}: {lines:?}");
        }
        assert_eq!(lines[lines.len() - 2], "(19 rows)");
    }
}
