//! Server notices (NOTICE / WARNING / INFO …) for shell queries.
//!
//! sqlx 0.8 has no notice callback. `PgStream` decodes each
//! NoticeResponse and emits it as a `tracing` event — target
//! `sqlx::postgres::notice`, text in the `message` field — synchronously,
//! from inside the future that is awaiting the query. So a task-local
//! sink set around a shell query, plus a tracing layer that forwards
//! those events to it, streams notices to the browser while the query
//! runs: `VACUUM VERBOSE` progress shows line by line, as in psql, and
//! every notice reaches the socket ahead of the query's result.
//!
//! Limits of what sqlx hands over: only the message text (DETAIL, HINT
//! and the SQLSTATE are dropped), and INFO and LOG share one level.
//! Servers rarely send LOG to clients, so that level reads as INFO.

use std::future::Future;

use shared::ServerMessage;
use tokio::sync::mpsc::UnboundedSender;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::{Filtered, Targets};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};

/// The tracing target sqlx logs Postgres notices under.
pub(crate) const TARGET: &str = "sqlx::postgres::notice";

/// Where a running shell query's notices go: the socket's outgoing
/// channel, tagged with the session the query runs on.
#[derive(Clone)]
pub(crate) struct NoticeSink {
    pub(crate) session_id: String,
    pub(crate) tx:         UnboundedSender<ServerMessage>,
}

tokio::task_local! {
    static SINK: NoticeSink;
}

/// Run `fut` with the notices it triggers forwarded to `sink`.
pub(crate) async fn forward<F: Future>(sink: NoticeSink, fut: F) -> F::Output {
    SINK.scope(sink, fut).await
}

/// The notice layer, filtered to [`TARGET`] only, so other layers'
/// filters (the `RUST_LOG` fmt output) are unaffected.
///
/// Cost: notices span every level down to TRACE (PG's INFO arrives as
/// TRACE), so this raises the process-wide max-level hint — and the
/// `log` facade's level via LogTracer — to TRACE. Debug/trace callsites
/// elsewhere then pass the static level check and are rejected by their
/// cached per-callsite interest instead: an atomic load rather than a
/// compare, and `log` records are built before being dropped. Small,
/// but not free.
pub(crate) fn layer<S>() -> Filtered<NoticeLayer, Targets, S>
where
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    NoticeLayer.with_filter(Targets::new().with_target(TARGET, Level::TRACE))
}

/// Forwards each sqlx notice event to the current task's sink, if the
/// task has one. Notices raised outside a shell query (explorer
/// introspection, Data-tab DML) have no sink and are dropped.
pub(crate) struct NoticeLayer;

impl<S: Subscriber> Layer<S> for NoticeLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != TARGET {
            return;
        }
        let _ = SINK.try_with(|sink| {
            let mut text = MessageText(String::new());
            event.record(&mut text);
            let _ = sink.tx.send(ServerMessage::Notice {
                session_id: sink.session_id.clone(),
                severity:   severity_label(*event.metadata().level()).to_string(),
                message:    text.0,
            });
        });
    }
}

/// psql's label for a notice, from the level sqlx logged it at.
fn severity_label(level: Level) -> &'static str {
    match level {
        Level::ERROR => "ERROR",
        Level::WARN  => "WARNING",
        Level::INFO  => "NOTICE",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "INFO",
    }
}

struct MessageText(String);

impl Visit for MessageText {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" && self.0.is_empty() {
            self.0 = format!("{value:?}");
        }
    }
}

/// Build the process-wide subscriber: the usual `RUST_LOG`-filtered fmt
/// output plus the notice layer, each with its own filter.
pub(crate) fn init_tracing() {
    use tracing_subscriber::util::SubscriberInitExt as _;
    use tracing_subscriber::{fmt, EnvFilter};
    tracing_subscriber::registry()
        .with(fmt::layer().with_filter(log_filter(EnvFilter::from_default_env())))
        .with(layer())
        .init();
}

/// The log output's filter: `base` (from `RUST_LOG`), with what sqlx
/// logs of a session's traffic always off, whatever `RUST_LOG` asks
/// for. sqlx logs the text of every statement (target `sqlx::query`;
/// debug, warn when slow), and a statement can carry a password —
/// `ALTER ROLE … PASSWORD`, `CREATE USER … IDENTIFIED BY`. It logs each
/// server notice too ([`TARGET`]), which can carry data (`RAISE NOTICE
/// '%', col`); the shell shows those, through the notice layer's own
/// filter. More specific than any `sqlx=…` directive and added last,
/// these win.
fn log_filter(base: tracing_subscriber::EnvFilter) -> tracing_subscriber::EnvFilter {
    ["sqlx::query", TARGET].into_iter().fold(base, |filter, target| {
        filter.add_directive(format!("{target}=off").parse().expect("a valid directive"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use tracing_subscriber::layer::SubscriberExt;

    /// Emit a notice exactly the way sqlx does (`message = <&str>`).
    fn emit(level: Level, text: &str) {
        match level {
            Level::WARN  => tracing::event!(target: "sqlx::postgres::notice", Level::WARN,  message = text),
            Level::INFO  => tracing::event!(target: "sqlx::postgres::notice", Level::INFO,  message = text),
            Level::TRACE => tracing::event!(target: "sqlx::postgres::notice", Level::TRACE, message = text),
            _ => unreachable!(),
        }
    }

    fn subscriber() -> impl Subscriber {
        tracing_subscriber::registry().with(layer())
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<ServerMessage>) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let ServerMessage::Notice { session_id, severity, message } = msg {
                out.push((session_id, severity, message));
            }
        }
        out
    }

    #[tokio::test(flavor = "current_thread")]
    async fn notices_inside_a_shell_query_reach_its_session() {
        let _guard = tracing::subscriber::set_default(subscriber());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = NoticeSink { session_id: "s1".into(), tx };
        forward(sink, async {
            emit(Level::INFO, "table \"x\" does not exist, skipping");
            emit(Level::TRACE, "vacuuming \"public.exam\"");
            emit(Level::WARN, "there is no transaction in progress");
        }).await;
        assert_eq!(drain(&mut rx), vec![
            ("s1".into(), "NOTICE".into(),  "table \"x\" does not exist, skipping".into()),
            ("s1".into(), "INFO".into(),    "vacuuming \"public.exam\"".into()),
            ("s1".into(), "WARNING".into(), "there is no transaction in progress".into()),
        ]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn notices_outside_a_shell_query_are_dropped() {
        let _guard = tracing::subscriber::set_default(subscriber());
        let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();
        emit(Level::INFO, "from explorer introspection");
        forward(NoticeSink { session_id: "s1".into(), tx }, async {
            tracing::info!(target: "noctua::other", message = "not a notice");
        }).await;
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn session_traffic_never_reaches_the_log() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::EnvFilter;

        /// Records the target of every event it's handed.
        struct Targets(Arc<Mutex<Vec<String>>>);
        impl<S: Subscriber> Layer<S> for Targets {
            fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
                self.0.lock().unwrap().push(event.metadata().target().to_string());
            }
        }

        for rust_log in ["trace", "sqlx=trace", "error,sqlx::query=trace", "warn,sqlx::query=debug"] {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::registry()
                .with(Targets(seen.clone()).with_filter(log_filter(EnvFilter::new(rust_log))));
            tracing::subscriber::with_default(subscriber, || {
                tracing::warn!(target: "sqlx::query", summary = "ALTER ROLE app PASSWORD …", "slow statement");
                tracing::debug!(target: "sqlx::query", summary = "ALTER ROLE app PASSWORD …");
                tracing::info!(target: "sqlx::postgres::notice", message = "token is abc123");
                tracing::error!(target: "sqlx::pool", "pool timed out");
            });
            assert_eq!(*seen.lock().unwrap(), ["sqlx::pool"], "RUST_LOG={rust_log}");
        }
    }

    /// Live check of the sqlx behaviour this module depends on (the
    /// target name, the `message` field, the level mapping) — a sqlx
    /// upgrade that changes any of them fails here. Needs a Postgres
    /// server; libpq-style env vars pick it (PGHOST, PGPORT, PGPASSWORD
    /// from ~/.parliament/secrets.env), defaulting to postgres@pagila:
    /// `cargo test -p noctua -- --ignored sqlx_notice_contract`
    #[tokio::test(flavor = "current_thread")]
    #[ignore]
    async fn sqlx_notice_contract() {
        use sqlx::postgres::PgConnectOptions;
        use sqlx::Executor;

        let _guard = tracing::subscriber::set_default(subscriber());
        let mut opts = PgConnectOptions::new();
        if std::env::var("PGUSER").is_err()     { opts = opts.username("postgres"); }
        if std::env::var("PGDATABASE").is_err() { opts = opts.database("pagila"); }
        let mut conn = crate::connect_pg(opts, None).await.expect("connect");

        let (tx, mut rx) = mpsc::unbounded_channel();
        forward(NoticeSink { session_id: "live".into(), tx }, async {
            conn.execute("DO $$ BEGIN \
                              RAISE NOTICE 'hello'; \
                              RAISE WARNING 'careful'; \
                              RAISE INFO 'fyi'; \
                          END $$").await.expect("DO block");
        }).await;
        let got: Vec<(String, String)> = drain(&mut rx).into_iter()
            .map(|(_, sev, msg)| (sev, msg)).collect();
        assert_eq!(got, vec![
            ("NOTICE".into(),  "hello".into()),
            ("WARNING".into(), "careful".into()),
            ("INFO".into(),    "fyi".into()),
        ]);
    }
}
