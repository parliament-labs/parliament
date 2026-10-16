//! Noctua — the Parliament backend.
//!
//! Axum 0.7 HTTP + WebSocket server. Holds one [`LiveConnection`]
//! (`sqlx::mysql::MySqlConnection` or `sqlx::postgres::PgConnection`)
//! per WebSocket session, never pooled — required so the connection
//! preserves temp tables, transactions, session variables, etc.
//!
//! Stays **DBMS-agnostic**: ships every `ExecuteQuery` SQL verbatim to
//! the upstream DB, never parses or translates. All dialect awareness
//! lives in `tyto/src/plane/`. Meta-commands (`\dt`, `\du`, etc.) are
//! translated to SQL **client-side** before being sent over the wire.
//!
//! Wraps every per-session DB I/O in a `tokio::spawn` so the WS
//! reader is never blocked behind a long-running query — protects
//! concurrent sessions on the same connection from head-of-line
//! blocking. Connections persist to `~/.sqlclient/connections.json`
//! (plaintext — prototype only; a vault-backed `CredentialProvider` is
//! on the roadmap).
//!
//! Wire shape: `password` is stripped from every `ConnectionList` /
//! `ConnectionSaved` response (see `shared::ConnectionDefPublic`) so
//! the browser never holds stored credentials in WASM linear memory.
//!
//! See also: `.claude/rules/noctua.md` for the DBMS-agnostic relay
//! contract, session-pinning rules, and value-converter edge cases.

mod db_error;
mod editor;
#[cfg(feature = "embed-frontend")]
mod frontend;
mod locale;
mod notices;
mod pgpass;
mod settings;
mod values;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use shared::{
    BatchResult, ClientMessage, CommandStatus, DbError, ConnectionDef, ConnectionDefPublic, ConnectionFile,
    CredentialSource, DbType, ResultSet, ServerMessage, TxnState,
};
use sqlx::{
    mysql::MySqlConnectOptions,
    postgres::PgConnectOptions,
    Connection, Executor, MySqlConnection, PgConnection,
};
use std::{collections::HashMap, path::{Path, PathBuf}, sync::Arc, time::Instant};
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

/// Split a multi-statement `fetch_many` stream into one [`ResultSet`]
/// per statement: the driver yields a statement's rows, then its
/// completion (`Either::Left`).
async fn collect_result_sets<'s, R, Done, F>(
    mut stream: futures_util::stream::BoxStream<'s, Result<sqlx::Either<Done, R>, sqlx::Error>>,
    convert: F,
) -> BatchResult
where
    R: sqlx::Row,
    F: Fn(&R, usize, &str) -> Option<String>,
{
    use futures_util::StreamExt;
    let mut result = BatchResult::default();
    let mut rows = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(sqlx::Either::Right(row)) => rows.push(row),
            Ok(sqlx::Either::Left(_)) => {
                let (columns, cells) = values::rows_to_cells(&rows, &convert);
                result.sets.push(ResultSet { columns, rows: cells });
                rows.clear();
            }
            Err(e) => {
                result.error = Some(db_error::fields(&e).unwrap_or_else(|| DbError::from_message(db_error::message(&e))));
                break;
            }
        }
    }
    result
}

/// A live database connection — dispatch target for queries and introspection.
/// One variant per supported DBMS. Stored per-session; never pooled, so temp
/// tables / transactions / session state survive across queries.
enum LiveConnection {
    MySql(MySqlConnection),
    Postgres(PgConnection),
}

/// A statement's result, as `LiveConnection::fetch` returns it.
struct Fetched {
    /// The rows' columns — on Postgres also when there are no rows — and
    /// each one's type as the server identifies it (`QueryResult.types`).
    columns: Vec<String>,
    types:   Vec<String>,
    /// The cells as the CLI prints them, SQL NULL as `None`.
    rows:    Vec<Vec<Option<String>>>,
    /// The last statement's tag (Postgres) or warnings and info line
    /// (MySQL); `rows_affected` counts the rows returned, or the rows
    /// affected when none came back.
    status:  CommandStatus,
}

impl LiveConnection {
    /// Run a query and return (column names, stringified rows, rows_affected).
    ///
    /// Uses sqlx's `fetch_many` streaming API which yields
    /// `Either<QueryResult, Row>` — `QueryResult` carries the protocol's
    /// `rows_affected` (meaningful for INSERT/UPDATE/DELETE),
    /// `Row` carries the actual data row (the RETURNING projection, the
    /// SELECT result, etc). Pre-fix, `execute_query` did a SQL-prefix
    /// dispatch — SELECT/SHOW/WITH/EXPLAIN/etc. went to `fetch_all` (rows
    /// only); everything else went to `execute_raw` (rows_affected only).
    /// `INSERT … RETURNING id` fell into the `execute_raw` bucket and the
    /// returning rows were silently discarded. The streaming path captures
    /// BOTH in a single pass, no prefix dispatch needed.
    ///
    /// The returned `count` is `rows.len()` when rows are non-empty
    /// (matches what the user sees in the table) and the protocol's
    /// `rows_affected` when rows are empty (matches what psql shows for a
    /// vanilla INSERT). SQL NULL reads as the text "NULL" — what the
    /// introspect parsers expect.
    async fn fetch_all_strings(
        &mut self,
        sql: &str,
    ) -> Result<(Vec<String>, Vec<Vec<String>>, u64), sqlx::Error> {
        let fetched = self.fetch(sql).await?;
        let rows = fetched.rows.into_iter()
            .map(|row| row.into_iter().map(|cell| cell.unwrap_or_else(|| "NULL".into())).collect())
            .collect();
        Ok((fetched.columns, rows, fetched.status.rows_affected))
    }

    /// [`fetch_all_strings`](Self::fetch_all_strings), with the rest of
    /// what the server said (the patched sqlx drivers keep it —
    /// vendor/): psql prints an empty result's header and a command's
    /// tag, the mysql CLI a command's warnings and info line.
    async fn fetch(&mut self, sql: &str) -> Result<Fetched, sqlx::Error> {
        use futures_util::StreamExt;
        use sqlx::{Column, Either, Row, TypeInfo};

        let mut rows_affected_from_protocol: u64 = 0;
        let mut status = CommandStatus::default();

        match self {
            LiveConnection::MySql(c) => {
                let mut collected = Vec::<sqlx::mysql::MySqlRow>::new();
                {
                    // raw_sql only: values.rs reads the text-protocol bytes as-is;
                    // a prepared statement would hand it binary values instead.
                    let mut stream = c.fetch_many(sqlx::raw_sql(sql));
                    while let Some(item) = stream.next().await {
                        match item? {
                            Either::Left(qr) => {
                                rows_affected_from_protocol =
                                    rows_affected_from_protocol.saturating_add(qr.rows_affected());
                                status.warnings = qr.warnings();
                                status.info = Some(qr.info().to_string()).filter(|info| !info.is_empty());
                            }
                            Either::Right(row) => collected.push(row),
                        }
                    }
                }
                let (columns, rows) = values::rows_to_cells(&collected, values::mysql_value);
                let types = collected.first()
                    .map(|row| row.columns().iter().map(|c| c.type_info().name().to_string()).collect())
                    .unwrap_or_default();
                status.rows_affected = if !collected.is_empty() {
                    collected.len() as u64
                } else {
                    rows_affected_from_protocol
                };
                Ok(Fetched { columns, types, rows, status })
            }
            LiveConnection::Postgres(c) => {
                let mut collected = Vec::<sqlx::postgres::PgRow>::new();
                let mut described = Vec::new();
                {
                    // raw_sql only: values.rs reads the text-protocol bytes as-is;
                    // a prepared statement would hand it binary values instead.
                    let mut stream = c.fetch_many(sqlx::raw_sql(sql));
                    while let Some(item) = stream.next().await {
                        match item? {
                            Either::Left(qr) => {
                                rows_affected_from_protocol =
                                    rows_affected_from_protocol.saturating_add(qr.rows_affected());
                                status.tag = Some(qr.command_tag().to_string());
                                described = qr.columns().iter().map(|c| (c.name().to_string(), pg_type_oid(c))).collect();
                            }
                            Either::Right(row) => collected.push(row),
                        }
                    }
                }
                let (columns, rows) = values::rows_to_cells(&collected, values::pg_value);
                status.rows_affected = if !collected.is_empty() {
                    collected.len() as u64
                } else {
                    rows_affected_from_protocol
                };
                let (columns, types) = match collected.first() {
                    Some(row) => (columns, row.columns().iter().map(pg_type_oid).collect()),
                    None => described.into_iter().unzip(),
                };
                Ok(Fetched { columns, types, rows, status })
            }
        }
    }

    /// Run `queries` in order, as one simple-query message (one round
    /// trip), and return a result set per query — SQL NULL as `None`.
    /// The server stops at the first error; the sets completed before
    /// it come back with the error. A query holding more than one
    /// statement would shift every set after it, so the count is
    /// checked — when all of them ran; next to an error the sets line
    /// up only if every query was one statement, which is the caller's
    /// contract.
    ///
    /// Postgres runs a multi-statement message as one transaction
    /// (inside the user's, if one is open), so a batch is for reads —
    /// never VACUUM, CREATE DATABASE or anything else that refuses a
    /// transaction block.
    async fn fetch_result_sets(&mut self, queries: &[String]) -> BatchResult {
        // The newline before each `;` keeps a trailing `--` comment from
        // swallowing it.
        let sql = queries.iter()
            .map(|q| q.trim().trim_end_matches(';'))
            .collect::<Vec<_>>()
            .join("\n;\n");
        let result = match self {
            LiveConnection::MySql(c)    => collect_result_sets(c.fetch_many(sqlx::raw_sql(&sql)), values::mysql_value).await,
            LiveConnection::Postgres(c) => collect_result_sets(c.fetch_many(sqlx::raw_sql(&sql)), values::pg_value).await,
        };
        if result.error.is_none() && result.sets.len() != queries.len() {
            return BatchResult {
                sets:  Vec::new(),
                error: Some(DbError::from_message(format!("{} queries returned {} result sets", queries.len(), result.sets.len()))),
            };
        }
        result
    }

    /// Run a non-row statement (DML / DDL) and return rows_affected.
    async fn execute_raw(&mut self, sql: &str) -> Result<u64, sqlx::Error> {
        match self {
            LiveConnection::MySql(c) => c
                .execute(sqlx::raw_sql(sql))
                .await
                .map(|r| r.rows_affected()),
            LiveConnection::Postgres(c) => c
                .execute(sqlx::raw_sql(sql))
                .await
                .map(|r| r.rows_affected()),
        }
    }

    /// Query the server version banner.
    async fn version(&mut self) -> String {
        let (_, rows, _) = self
            .fetch_all_strings("SELECT version()")
            .await
            .unwrap_or_default();
        rows.into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .unwrap_or_else(|| "unknown".into())
    }

    /// Whether the server asked for a password when the session opened
    /// — libpq's `PQconnectionUsedPassword`, for psql's `\conninfo`
    /// (the patched sqlx-postgres keeps it). MySQL: no.
    fn password_used(&self) -> bool {
        match self {
            LiveConnection::Postgres(c) => c.password_used(),
            LiveConnection::MySql(_) => false,
        }
    }

    /// List installed extensions on this connection. PG returns names
    /// from `pg_extension`; MySQL has no equivalent so returns empty.
    /// Called once at session open, cached on the frontend's ShellTab.
    /// Downstream plane methods that would otherwise reference an
    /// extension function by name (which PG parse-resolves eagerly)
    /// gate their SQL on the cached list — see `table_vm_sql`'s
    /// `pg_visibility` gating for the canonical example.
    async fn extensions(&mut self) -> Vec<String> {
        let sql = match self {
            LiveConnection::MySql(_) => return Vec::new(),
            LiveConnection::Postgres(_) => "SELECT extname FROM pg_extension ORDER BY extname",
        };
        let (_, rows, _) = self
            .fetch_all_strings(sql)
            .await
            .unwrap_or_default();
        rows.into_iter()
            .filter_map(|r| r.into_iter().next())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Describe the columns the query would produce, without
    /// executing it. Drives psql's `\gdesc`. sqlx exposes
    /// `describe()` on `Executor` for both PG and MySQL — it issues
    /// a Parse + Describe to the wire protocol and returns the
    /// column metadata. No rows are scanned and no side effects fire.
    /// Returns `(column_name, type_name)` pairs in result order.
    /// `sql`'s result columns as the server describes them, without
    /// running it. Postgres: as psql's `\gdesc` asks — the unnamed
    /// statement parsed and described, nothing else (the patched sqlx's
    /// `describe_result_columns`) — each column's type OID and modifier.
    /// MySQL: sqlx's describe, its type names.
    async fn describe_columns(&mut self, sql: &str) -> Result<Vec<shared::DescribedColumn>, sqlx::Error> {
        use sqlx::{Column, TypeInfo};
        match self {
            LiveConnection::MySql(c) => {
                let d = c.describe(sql).await?;
                Ok(d.columns()
                    .iter()
                    .map(|col| shared::DescribedColumn {
                        name: col.name().to_string(), type_id: col.type_info().name().to_string(), type_modifier: -1,
                    })
                    .collect())
            }
            LiveConnection::Postgres(c) => Ok(c.describe_result_columns(sql).await?
                .into_iter()
                .map(|(name, oid, type_modifier)| shared::DescribedColumn { name, type_id: oid.to_string(), type_modifier })
                .collect()),
        }
    }

    /// Probe whether the upstream DB connection is still alive. sqlx
    /// exposes this via the `Connection` trait's `ping`. Used after a
    /// query / introspect / DML error to distinguish "your SQL was bad"
    /// (session still healthy) from "your session is dead and noctua
    /// hasn't noticed yet" (e.g. `pg_terminate_backend` on this PID,
    /// MySQL `KILL`, idle-in-transaction timeout, upstream restart).
    /// On the dead path the caller emits `SessionClosed` so the frontend
    /// surfaces the "Session ended — Reconnect" banner.
    /// The session's transaction state, as the server reported it after
    /// the last command (the patched sqlx drivers expose it). MySQL has
    /// no failed state — an error doesn't end its transaction, or ends
    /// it outright — and its error reply carries no status: after one,
    /// read this after `is_alive` (a ping, whose reply does). None when
    /// the connection can't say (it's gone).
    async fn transaction_state(&mut self) -> Option<TxnState> {
        use sqlx::postgres::PgTransactionStatus;
        match self {
            LiveConnection::MySql(c) => Some(if c.in_transaction() { TxnState::Active } else { TxnState::Idle }),
            LiveConnection::Postgres(c) => match c.transaction_status().await.ok()? {
                PgTransactionStatus::Idle        => Some(TxnState::Idle),
                PgTransactionStatus::Transaction => Some(TxnState::Active),
                PgTransactionStatus::Error       => Some(TxnState::Failed),
            },
        }
    }

    async fn is_alive(&mut self) -> bool {
        match self {
            LiveConnection::MySql(c)    => c.ping().await.is_ok(),
            LiveConnection::Postgres(c) => c.ping().await.is_ok(),
        }
    }

    /// Query the server-side connection id so the UI can display it next to
    /// the pane. Lets a DBA correlate a pane with `SHOW PROCESSLIST` /
    /// `pg_stat_activity` without manually running a query to find out.
    async fn db_connection_id(&mut self) -> String {
        let sql = match self {
            LiveConnection::MySql(_)    => "SELECT CONNECTION_ID()",
            LiveConnection::Postgres(_) => "SELECT pg_backend_pid()",
        };
        let (_, rows, _) = self.fetch_all_strings(sql).await.unwrap_or_default();
        rows.into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .unwrap_or_default()
    }
}

// TODO: multi-user — sessions are currently a flat global map, assuming a
// single trusted user. Team-shared deployment needs per-user ownership so
// user A can't touch user B's sessions. See
// vault/Architecture/Deployment Topologies.md for the shared-mode prereqs.
type Sessions = Arc<Mutex<HashMap<String, Arc<Mutex<LiveConnection>>>>>;
type ConnStore = Arc<Mutex<ConnectionFile>>;

#[derive(Clone)]
struct AppState {
    sessions: Sessions,
    connections: ConnStore,
    /// App-wide preferences (`~/.sqlclient/settings.json`).
    settings: Arc<Mutex<settings::LoadedSettings>>,
}

// ── Connection file storage ───────────────────────────────────────────────────

fn connections_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".sqlclient").join("connections.json")
}

// TODO: multi-user — connections.json is a single global file keyed off
// $HOME, which makes it the wrong shape for team-shared deployment. Replace
// with a per-user metadata store (SQLite next to the backend, or external)
// before the backend is exposed to more than one user. See
// vault/Architecture/Deployment Topologies.md.

/// Untagged on-disk parse target. The v2 file is a `ConnectionFile` object
/// with `version` / `folders` / `connections`. The v1 file was a bare array
/// of `ConnectionDef`. `untagged` lets serde try v2 first, then fall back
/// to v1 — both shapes deserialize transparently.
#[derive(Deserialize)]
#[serde(untagged)]
enum ConnectionFileRaw {
    V2(ConnectionFile),
    Legacy(Vec<ConnectionDef>),
}

/// Parse the on-disk file contents into a v2 [`ConnectionFile`], migrating
/// the legacy bare-array shape transparently. Pulled out so the migration
/// path is unit-testable without touching the filesystem.
fn parse_connections_file(s: &str) -> ConnectionFile {
    match serde_json::from_str::<ConnectionFileRaw>(s) {
        Ok(ConnectionFileRaw::V2(file)) => file,
        Ok(ConnectionFileRaw::Legacy(connections)) => ConnectionFile {
            version: 2,
            folders: vec![],
            connections,
        },
        Err(_) => ConnectionFile::default(),
    }
}

async fn load_connections() -> ConnectionFile {
    let path = connections_path();
    make_private(&path);
    match tokio::fs::read_to_string(&path).await {
        Ok(data) => parse_connections_file(&data),
        Err(_) => ConnectionFile::default(),
    }
}

/// The connections file holds passwords, so it's the user's alone: one
/// others can read — saved before saves were 0600 — is made 0600 when
/// noctua starts (a symlink's target, if it's a link).
fn make_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(path) else { return };
        if meta.is_file() && meta.permissions().mode() & 0o077 != 0 {
            if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
                tracing::error!("couldn't make {} private: {e}", path.display());
            }
        }
    }
}

fn persist_connections(file: &ConnectionFile) {
    save_connections_to(&connections_path(), file);
}

/// Write the connections file as settings.json is written: atomically,
/// 0600 (`settings::write_atomic_0600`), a symlink replaced by a regular
/// file. A save that fails is logged; the change stays in memory.
fn save_connections_to(path: &Path, file: &ConnectionFile) {
    let saved = serde_json::to_string_pretty(file)
        .map_err(std::io::Error::other)
        .and_then(|json| settings::write_atomic_0600(path, json.as_bytes()));
    if let Err(e) = saved {
        tracing::error!("couldn't save {}: {e}", path.display());
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    notices::init_tracing();

    let state = AppState {
        sessions: Arc::new(Mutex::new(HashMap::new())),
        connections: Arc::new(Mutex::new(load_connections().await)),
        settings: Arc::new(Mutex::new(settings::load(&settings::settings_path()))),
    };

    {
        let file = state.connections.lock().await;
        tracing::info!(
            "Loaded {} saved connection(s) and {} explicit folder(s)",
            file.connections.len(),
            file.folders.len()
        );
    }

    // Defense in depth:
    // 1. bind() restricts the kernel to loopback — non-localhost packets are
    //    dropped at the TCP layer.
    // 2. CORS allowlist stops cross-site fetch() / XHR requests from reaching
    //    HTTP endpoints even when both originate inside the same browser.
    // 3. ws_handler's Origin check gates the WebSocket upgrade, since the WS
    //    handshake bypasses the CORS layer entirely.
    // All three stack — if any one fails, the remaining two still block the
    // common attack shapes. See vault/Architecture/Deployment Topologies.md
    // for the threat model and the shared-deployment upgrade path.
    let cors = CorsLayer::new()
        .allow_origin(ALLOWED_ORIGINS.map(|o| o.parse().unwrap()))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);

    let app = Router::new().route("/ws", get(ws_handler));
    #[cfg(feature = "embed-frontend")]
    let app = app.fallback_service(get(frontend::serve));
    #[cfg(not(feature = "embed-frontend"))]
    let app = app.fallback_service(tower_http::services::ServeDir::new("dist"));
    let app = app.layer(cors).with_state(state);

    let listener = match tokio::net::TcpListener::bind("127.0.0.1:3000").await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("noctua: can't listen on 127.0.0.1:3000 ({e}). Is Parliament already running, or another program using port 3000?");
            std::process::exit(1);
        }
    };
    tracing::info!("Backend listening on http://127.0.0.1:3000 (loopback only)");
    #[cfg(feature = "embed-frontend")]
    println!("Parliament is running: open http://localhost:3000 in your browser. Ctrl+C stops it.");
    axum::serve(listener, app).await.unwrap();
}

// ── WebSocket handling ────────────────────────────────────────────────────────

async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    // The WS upgrade handshake bypasses CORS entirely, so we check the Origin
    // header manually here: only Parliament's own pages may connect.
    if !origin_is_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin WebSocket rejected").into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state)).into_response()
}

/// The pages allowed to talk to noctua: noctua itself (`:3000`) and the
/// `trunk serve` dev server (`:8080`). Any other page — including one
/// served from another localhost port — is refused, since the socket can
/// run SQL on saved connections and open the user's editor.
const ALLOWED_ORIGINS: [&str; 4] = [
    "http://localhost:3000",
    "http://localhost:8080",
    "http://127.0.0.1:3000",
    "http://127.0.0.1:8080",
];

/// True if the request's `Origin` header is one of [`ALLOWED_ORIGINS`],
/// or is absent entirely.
///
/// An absent Origin is accepted because browsers always emit it; only
/// hand-crafted tooling (curl, wscat without `--origin`) omits it, and we'd
/// rather not break loopback dev tools. Real browsers emit the header
/// unconditionally on WebSocket upgrades, so a missing Origin cannot come
/// from a malicious web page.
fn origin_is_allowed(headers: &HeaderMap) -> bool {
    match headers.get(header::ORIGIN) {
        None => true,
        Some(v) => v.to_str().is_ok_and(|o| ALLOWED_ORIGINS.contains(&o)),
    }
}

/// Split the WebSocket into a reader task and a writer task so that a
/// long-running query (e.g. a MySQL UPDATE blocked on another session's
/// lock) does not prevent the reader from processing subsequent messages
/// — notably, the COMMIT that would release the lock.
///
/// Messages whose handler does a DB round-trip (`needs_spawn`) are run
/// on `tokio::spawn`'d tasks, each writing its responses into a shared
/// mpsc. Per-session serialization is preserved by the session's
/// `Arc<Mutex<LiveConnection>>`. Fast state-mutating handlers (save /
/// delete / list / disconnect) run inline on the reader to keep their
/// "issue-then-read-back" ordering observable to the client.
async fn handle_socket(socket: WebSocket, state: AppState) {
    use futures_util::{SinkExt, StreamExt};
    use tokio::sync::mpsc;
    use tokio::task::JoinSet;

    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let json = serde_json::to_string(&msg).unwrap();
            if sink.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    // Track every session_id this socket's Connect calls produce. On
    // any exit from the read loop (clean WS close, error, page reload,
    // browser tab close, network drop), drain this list and remove the
    // sessions from state.sessions so the upstream DB connections close.
    //
    // Without this, every Cmd+R / browser-close leaks one upstream TCP
    // connection per open pane — over a workday a DBA can hit upstream
    // max_connections for reasons that aren't visible from the UI.
    // close_pane → Disconnect cleans up correctly; this fixes every
    // other socket-termination path.
    let owned_sessions: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // Track in-flight spawned tasks so we can wait for a mid-flight
    // Connect to finish inserting before we drain `owned_sessions`. Without
    // this, the cleanup races a Connect that's still negotiating with the
    // DB at the moment the socket dies — the Connect lands AFTER the
    // drain, and the new session leaks despite our tracking.
    let mut tasks: JoinSet<()> = JoinSet::new();
    // Open editors wait on the user, possibly for hours. They're kept
    // apart so a closed socket aborts them instead of waiting — which
    // also removes their temp files.
    let mut editor_tasks: JoinSet<()> = JoinSet::new();

    while let Some(Ok(Message::Text(text))) = stream.next().await {
        // Finished tasks stay in a JoinSet until joined; reap them.
        while tasks.try_join_next().is_some() {}
        while editor_tasks.try_join_next().is_some() {}
        let parsed = serde_json::from_str::<ClientMessage>(&text);
        match parsed {
            Err(e) => {
                let _ = tx.send(ServerMessage::QueryError {
                    session_id: String::new(),
                    message: format!("Invalid message: {e}"),
                    error: None,
                    connection_lost: false,
                });
            }
            Ok(msg) if needs_spawn(&msg) => {
                let state = state.clone();
                let tx = tx.clone();
                let owned = owned_sessions.clone();
                // Shell queries stream server notices to the socket as
                // they arrive, ahead of the result (see notices.rs).
                let notice_sink = match &msg {
                    ClientMessage::ExecuteQuery { session_id, .. } => Some(notices::NoticeSink {
                        session_id: session_id.clone(),
                        tx:         tx.clone(),
                    }),
                    _ => None,
                };
                let set = match &msg {
                    ClientMessage::EditExternally { .. } | ClientMessage::TestEditor { .. } => &mut editor_tasks,
                    _ => &mut tasks,
                };
                set.spawn(async move {
                    let responses = match notice_sink {
                        Some(sink) => notices::forward(sink, process_message(msg, &state)).await,
                        None       => process_message(msg, &state).await,
                    };
                    for resp in responses {
                        // Record session_id BEFORE forwarding so the
                        // tracker is consistent even if the writer
                        // drops the message.
                        if let ServerMessage::SessionOpened { ref session_id, .. } = resp {
                            owned.lock().await.push(session_id.clone());
                        }
                        let _ = tx.send(resp);
                    }
                });
            }
            Ok(msg) => {
                // Inline path handles ListConnections / SaveConnection /
                // DeleteConnection / SaveFolders / Disconnect — none
                // produce SessionOpened today (Connect is in needs_spawn).
                // Pattern-matching on SessionOpened here anyway as cheap
                // insurance against future refactors of needs_spawn.
                for resp in process_message(msg, &state).await {
                    if let ServerMessage::SessionOpened { ref session_id, .. } = resp {
                        owned_sessions.lock().await.push(session_id.clone());
                    }
                    let _ = tx.send(resp);
                }
            }
        }
    }

    // Read loop exited (close frame, non-text message, stream error).
    // Abort open editors — nobody is left to receive their result.
    // Drain in-flight tasks so a mid-flight Connect's insertion lands
    // before we attempt cleanup. JoinSet::join_next returns None when
    // all spawned tasks have completed.
    editor_tasks.shutdown().await;
    while tasks.join_next().await.is_some() {}

    // Remove every session this socket owned from state.sessions. Each
    // removal drops the last Arc<Mutex<LiveConnection>>, which closes
    // the upstream TCP connection via sqlx's Drop impl. Sessions already
    // removed via an explicit Disconnect (close_pane path) are simply
    // no-ops here.
    let ids = owned_sessions.lock().await.drain(..).collect::<Vec<_>>();
    if !ids.is_empty() {
        let mut sessions = state.sessions.lock().await;
        for id in &ids {
            sessions.remove(id);
        }
        tracing::info!(
            "WS closed; cleaned up {} orphaned session(s)",
            ids.len(),
        );
    }

    drop(tx);
    let _ = writer.await;
}

/// True if the handler for this message does a DB round-trip and should
/// be run concurrently with other messages on the same socket. Keeping
/// the fast state-store handlers (save / delete / list / disconnect)
/// inline preserves their observable ordering with each other.
fn needs_spawn(msg: &ClientMessage) -> bool {
    matches!(
        msg,
        ClientMessage::ExecuteQuery { .. }
            | ClientMessage::DescribeQuery { .. }
            | ClientMessage::IntrospectQuery { .. }
            | ClientMessage::IntrospectBatch { .. }
            | ClientMessage::ExecuteDml { .. }
            | ClientMessage::Connect { .. }
            | ClientMessage::TestConnection { .. }
            | ClientMessage::ListDatabases { .. }
            | ClientMessage::ListSchemas { .. }
            // External-editor round-trip blocks until the user
            // closes their editor — anywhere from seconds to many
            // minutes. Must spawn so the WS reader is never blocked
            // and other panes' queries keep working.
            | ClientMessage::EditExternally { .. }
            | ClientMessage::TestEditor { .. }
    )
}

/// Resolve the password to use for a PG connect. Dispatches on
/// `credential_source`:
///
/// - `Plaintext`: returns the on-`def` password verbatim (mirrors the
///   pre-existing behaviour). Callers pre-fill `def.password` from the
///   saved connection store when the Edit modal / Schema Explorer sends
///   a blank password + a known id.
/// - `Pgpass`: reads `~/.pgpass` (or `$PGPASSFILE`) and looks up the
///   entry matching `(host, port, database, user)`. Never falls back to
///   `def.password` — a miss, missing file, or wrong permissions all
///   surface as an explicit `Err(msg)` that the caller routes to the
///   handler's own error variant.
///
/// PG only. MySQL always uses `Plaintext` (any non-Plaintext value on a
/// MySQL connection is treated as Plaintext defensively).
fn resolve_pg_password(def: &ConnectionDef, database_for_lookup: &str) -> Result<String, String> {
    match def.credential_source {
        CredentialSource::Plaintext => Ok(def.password.clone()),
        CredentialSource::Pgpass => pgpass::lookup(
            &def.host, def.port, database_for_lookup, &def.user,
        ),
    }
}

/// The TLS library noctua's sessions use — sqlx's native-tls, over the
/// platform's own — as libpq's `PQsslAttribute(conn, "library")` names
/// its OpenSSL: psql's `\conninfo` shows it.
const SSL_LIBRARY: &str = if cfg!(target_os = "macos") {
    "Secure Transport"
} else if cfg!(windows) {
    "SChannel"
} else {
    "OpenSSL"
};

/// A Postgres session as psql gets it. psql leaves the session's settings
/// to the server — `TimeZone`, `DateStyle`, `extra_float_digits` from
/// postgresql.conf, `ALTER ROLE` / `ALTER DATABASE ... SET` — except what
/// libpq's environment variables ask for (`PGTZ`, `PGDATESTYLE`,
/// `PGGEQO`), which it sends at connect and which beat the role's and
/// the database's. noctua plays libpq's part: its environment's settings
/// go in the startup message (`pg_session_settings`), above them the time
/// zone the browser asks for (`timezone`, its own Settings choice), the
/// closer to the user the stronger. Upstream sqlx sends `TimeZone=UTC` and
/// `DateStyle=ISO, MDY` itself, which the patched sqlx-postgres doesn't
/// (vendor/sqlx-postgres), and `extra_float_digits=2`, which it's told not
/// to here.
async fn connect_pg(opts: PgConnectOptions, timezone: Option<&str>) -> Result<PgConnection, sqlx::Error> {
    let settings = pg_session_settings(&|var| std::env::var(var).ok(), timezone);
    connect_pg_with(opts, &settings).await
}

/// Connect with these session settings sent as libpq sends them: each a
/// parameter of its own in the startup message (the patched sqlx's
/// `startup_param`), which the server applies with source `client` and
/// which poolers such as PgBouncer pass on (`timezone`, `datestyle`) —
/// where `-c` in the `options` parameter would be refused.
async fn connect_pg_with(opts: PgConnectOptions, settings: &[(&str, String)]) -> Result<PgConnection, sqlx::Error> {
    let opts = settings.iter()
        .fold(opts.extra_float_digits(None), |opts, (name, value)| opts.startup_param(name, value));
    PgConnection::connect_with(&opts).await
}

/// libpq's environment variables that set a session's defaults, and the
/// settings they send (`EnvironmentOptions` in fe-connect.c).
const PG_ENVIRONMENT_SETTINGS: [(&str, &str); 3] =
    [("PGDATESTYLE", "datestyle"), ("PGTZ", "timezone"), ("PGGEQO", "geqo")];

/// The settings a Postgres session asks for at startup: each of libpq's
/// environment variables that's set — but not to `default`, which libpq
/// skips — and then the browser's time zone, which replaces `PGTZ`'s.
fn pg_session_settings(env: &dyn Fn(&str) -> Option<String>, timezone: Option<&str>) -> Vec<(&'static str, String)> {
    let mut settings: Vec<(&'static str, String)> = PG_ENVIRONMENT_SETTINGS.iter()
        .filter_map(|&(var, name)| env(var).filter(|value| !value.eq_ignore_ascii_case("default")).map(|value| (name, value)))
        .collect();
    if let Some(zone) = timezone {
        settings.retain(|&(name, _)| name != "timezone");
        settings.push(("timezone", zone.to_string()));
    }
    settings
}

/// A MySQL session as the mysql CLI gets it. Unless told otherwise, sqlx
/// changes the session after connecting — `time_zone = '+00:00'`,
/// `PIPES_AS_CONCAT` and `NO_ENGINE_SUBSTITUTION` added to `sql_mode`, and
/// `SET NAMES utf8mb4 COLLATE utf8mb4_unicode_ci` — so `||` meant string
/// concatenation instead of OR, times showed in UTC instead of the
/// server's zone, and comparing a column with a user variable could fail
/// with "Illegal mix of collations". All four are off; a plain
/// `SET NAMES utf8mb4` lets the server pick its default collation for
/// utf8mb4, which on MySQL is what the CLI gets (on MariaDB 11.4.5+ the
/// server default, `utf8mb4_uca1400_ai_ci`, can differ from the CLI's).
///
/// Two handshake flags upstream sqlx sends — `CLIENT_IGNORE_SPACE`
/// (adds `IGNORE_SPACE` to `sql_mode`) and `CLIENT_FOUND_ROWS` (an
/// UPDATE reports the rows it matched, not changed) — the patched
/// sqlx-mysql leaves out, as the CLI does (vendor/sqlx-mysql).
///
/// The setup runs after the server's `init_connect`, so a character-set
/// change made there is overridden. A server that refuses the setup
/// still gets its session, with sqlx's settings.
async fn connect_mysql(opts: MySqlConnectOptions) -> Result<MySqlConnection, sqlx::Error> {
    let opts = opts
        .timezone(None)
        .pipes_as_concat(false)
        .no_engine_substitution(false)
        .set_names(false);
    let mut conn = MySqlConnection::connect_with(&opts).await?;
    let setup = conn.execute(sqlx::raw_sql("SET NAMES utf8mb4")).await;
    if let Err(e) = setup {
        tracing::warn!("MySQL session setup refused, keeping sqlx's settings: {e}");
    }
    Ok(conn)
}

async fn process_message(msg: ClientMessage, state: &AppState) -> Vec<ServerMessage> {
    match msg {
        // ── Connection management ─────────────────────────────────────────────
        ClientMessage::GetSettings => {
            let loaded = state.settings.lock().await.clone();
            vec![ServerMessage::Settings { settings: loaded.settings, load_error: loaded.load_error }]
        }
        ClientMessage::SaveSettings { settings: mut new } => {
            // Lock first and hold it across the write, so two tabs saving
            // at once can't leave memory and disk disagreeing.
            let mut stored = state.settings.lock().await;
            match settings::save(&settings::settings_path(), &new) {
                Ok(()) => {
                    new.version = shared::settings::SETTINGS_VERSION;
                    stored.settings = new.clone();
                    stored.load_error = None;
                    vec![ServerMessage::SettingsSaved { settings: new }]
                }
                Err(message) => vec![ServerMessage::SettingsError { message }],
            }
        }
        ClientMessage::TestEditor { env_chain, line_flag_var } => {
            let editor_settings = state.settings.lock().await.settings.editor.clone();
            let request = editor::Request {
                connection_override: None,
                settings:            &editor_settings,
                env_chain:           &env_chain,
                line_flag_var,
            };
            let (ok, message) = match editor::test(&request).await {
                Ok(message) => (true, message),
                Err(message) => (false, message),
            };
            vec![ServerMessage::EditorTestResult { ok, message }]
        }
        ClientMessage::ListConnections => {
            let file = state.connections.lock().await.clone();
            // Strip passwords on the way out — the browser never holds
            // stored credentials (see ConnectionDefPublic + issue #6).
            let pubs: Vec<ConnectionDefPublic> =
                file.connections.iter().map(ConnectionDefPublic::from).collect();
            vec![ServerMessage::ConnectionList {
                connections: pubs,
                folders: file.folders,
            }]
        }

        ClientMessage::SaveConnection { def, password_change, copy_from } => {
            let mut file = state.connections.lock().await;
            // Resolve the password to persist (in priority order):
            //   1. password_change=Some(s) → the user typed a new value
            //      (new connection, or password change in Edit modal)
            //   2. copy_from=Some(src_id)  → the sidebar's "Copy" verb
            //      minted a fresh id for the duplicate; inherit the
            //      source's password from disk
            //   3. fall through              → keep whatever is on disk
            //      for this id (Edit-mode blank password, or round-trip
            //      paths that just edit `paths`); a truly new id with
            //      no record persists empty
            let password = match (password_change, copy_from) {
                (Some(s), _) => s,
                (None, Some(src_id)) => file.connections.iter()
                    .find(|c| c.id == src_id)
                    .map(|c| c.password.clone())
                    .unwrap_or_default(),
                (None, None) => file.connections.iter()
                    .find(|c| c.id == def.id)
                    .map(|c| c.password.clone())
                    .unwrap_or_default(),
            };
            let full = ConnectionDef {
                id:           def.id,
                name:         def.name,
                db_type:      def.db_type,
                host:         def.host,
                port:         def.port,
                user:         def.user,
                password,
                database:     def.database,
                paths:        def.paths,
                default_pane:        def.default_pane,
                env:                 def.env,
                format_style:        def.format_style,
                editor_command:      def.editor_command,
                buffer_threshold_mb: def.buffer_threshold_mb,
                default_schema:      def.default_schema,
                credential_source:   def.credential_source,
            };
            if let Some(existing) = file.connections.iter_mut().find(|c| c.id == full.id) {
                *existing = full.clone();
            } else {
                file.connections.push(full.clone());
            }
            persist_connections(&file);
            // Echo back the password-free public view — the saved
            // connection's row in the sidebar refreshes without ever
            // carrying the password down the wire.
            vec![ServerMessage::ConnectionSaved { def: (&full).into() }]
        }

        ClientMessage::DeleteConnection { id } => {
            let mut file = state.connections.lock().await;
            file.connections.retain(|c| c.id != id);
            persist_connections(&file);
            vec![ServerMessage::ConnectionDeleted { id }]
        }

        ClientMessage::SaveFolders { folders } => {
            let mut file = state.connections.lock().await;
            file.folders = folders.clone();
            persist_connections(&file);
            vec![ServerMessage::FoldersSaved { folders }]
        }

        // ── Connection test (ephemeral — connect + disconnect immediately) ────
        ClientMessage::TestConnection { mut def } => {
            // The browser does not hold the saved password (see
            // ConnectionDefPublic). If the incoming def has an empty
            // password and matches a saved connection by id, look the
            // password up here. Lets the Edit modal's "Test Connection"
            // button work without forcing the user to retype the
            // password every time. Only applies to Plaintext — Pgpass
            // resolves at connect time and doesn't touch the stored
            // password field.
            if def.credential_source == CredentialSource::Plaintext
                && def.password.is_empty() && !def.id.is_empty()
            {
                let file = state.connections.lock().await;
                if let Some(saved) = file.connections.iter().find(|c| c.id == def.id) {
                    def.password = saved.password.clone();
                }
            }
            let result: Result<(), String> = match def.db_type {
                DbType::MySql => {
                    let mut opts = MySqlConnectOptions::new()
                        .host(&def.host).port(def.port)
                        .username(&def.user).password(&def.password);
                    if !def.database.is_empty() { opts = opts.database(&def.database); }
                    connect_mysql(opts).await
                        .map(|_| ())
                        .map_err(|e| db_error::message(&e))
                }
                DbType::Postgres => {
                    let db = if def.database.is_empty() { "postgres" } else { &def.database };
                    match resolve_pg_password(&def, db) {
                        Err(msg) => Err(msg),
                        Ok(resolved) => {
                            let opts = PgConnectOptions::new()
                                .host(&def.host).port(def.port)
                                .username(&def.user).password(&resolved)
                                .database(db)
                                .application_name("Parliament - Test Connection");
                            connect_pg(opts, None).await
                                .map(|_| ())
                                .map_err(|e| db_error::message(&e))
                        }
                    }
                }
            };
            match result {
                Ok(()) => vec![ServerMessage::TestConnectionResult {
                    success: true, message: "Connection successful.".into(),
                }],
                Err(message) => vec![ServerMessage::TestConnectionResult {
                    success: false, message,
                }],
            }
        }

        // ── List databases (ephemeral — connect, query, disconnect) ──────────
        ClientMessage::ListDatabases { mut def, client_tab_id } => {
            // Same blank-password-means-look-it-up sentinel as
            // TestConnection: the Schema Explorer's database-dropdown
            // never holds the saved password, so it sends a blank one
            // alongside the saved id and noctua resolves it. Only
            // applies to Plaintext — Pgpass resolves at connect time.
            if def.credential_source == CredentialSource::Plaintext
                && def.password.is_empty() && !def.id.is_empty()
            {
                let file = state.connections.lock().await;
                if let Some(saved) = file.connections.iter().find(|c| c.id == def.id) {
                    def.password = saved.password.clone();
                }
            }
            let result: Result<Vec<String>, String> = match def.db_type {
                DbType::MySql => {
                    let opts = MySqlConnectOptions::new()
                        .host(&def.host).port(def.port)
                        .username(&def.user).password(&def.password);
                    match connect_mysql(opts).await {
                        Err(e) => Err(db_error::message(&e)),
                        Ok(mut conn) => {
                            match conn.fetch_all(sqlx::raw_sql("SHOW DATABASES")).await {
                                Err(e) => Err(db_error::message(&e)),
                                Ok(rows) => Ok(rows.iter().map(|r| {
                                    use sqlx::Row;
                                    r.try_get::<String, _>(0).unwrap_or_default()
                                }).collect()),
                            }
                        }
                    }
                }
                DbType::Postgres => {
                    // .pgpass keys on (host, port, database, user). The
                    // database enumeration query connects to the
                    // maintenance DB "postgres"; use that same value as
                    // the lookup key so a `.pgpass` entry with an
                    // explicit `postgres` database works. (Wildcard
                    // entries `*:*:*:...:pw` also work either way.)
                    match resolve_pg_password(&def, "postgres") {
                        Err(msg) => Err(msg),
                        Ok(resolved) => {
                            let opts = PgConnectOptions::new()
                                .host(&def.host).port(def.port)
                                .username(&def.user).password(&resolved)
                                .database("postgres")
                                .application_name("Parliament - List Databases");
                            match connect_pg(opts, None).await {
                                Err(e) => Err(db_error::message(&e)),
                                Ok(mut conn) => {
                                    match conn.fetch_all(sqlx::raw_sql(
                                        "SELECT datname FROM pg_database WHERE datistemplate = false ORDER BY datname"
                                    )).await {
                                        Err(e) => Err(db_error::message(&e)),
                                        Ok(rows) => Ok(rows.iter().map(|r| {
                                            use sqlx::Row;
                                            r.try_get::<String, _>(0).unwrap_or_default()
                                        }).collect()),
                                    }
                                }
                            }
                        }
                    }
                }
            };
            match result {
                Ok(dbs) => vec![ServerMessage::DatabaseList {
                    databases: dbs,
                    client_tab_id,
                }],
                // Previously this emitted `TestConnectionResult { success:
                // false, .. }`, which routed to the connection-form modal
                // only — invisible to a Schema Explorer pane that fired
                // the request from a database dropdown. Use the dedicated
                // error variant and echo client_tab_id so the originating
                // pane (or the form, if None) surfaces the failure.
                Err(message) => vec![ServerMessage::DatabaseListError {
                    message: format!("Failed to list databases: {}", message),
                    client_tab_id,
                }],
            }
        }

        // ── List schemas (ephemeral — PG only) ───────────────────────────────
        // Mirrors ListDatabases: blank-password sentinel resolves to the saved
        // password; transient connection opens against the user-chosen database
        // (NOT hardcoded "postgres" — pg_namespace is per-database), queries
        // user schemas, drops the connection. MySQL gets a clear error since
        // its "schemas" are databases.
        ClientMessage::ListSchemas { mut def, client_tab_id } => {
            if def.credential_source == CredentialSource::Plaintext
                && def.password.is_empty() && !def.id.is_empty()
            {
                let file = state.connections.lock().await;
                if let Some(saved) = file.connections.iter().find(|c| c.id == def.id) {
                    def.password = saved.password.clone();
                }
            }
            let result: Result<Vec<String>, String> = match def.db_type {
                DbType::MySql => Err(
                    "Schemas are a Postgres-only concept; MySQL uses databases as its top-level namespace.".to_string()
                ),
                DbType::Postgres => {
                    if def.database.is_empty() {
                        Err("Pick a database first — schemas are per-database in Postgres.".to_string())
                    } else {
                        match resolve_pg_password(&def, &def.database) {
                            Err(msg) => Err(msg),
                            Ok(resolved) => {
                                let opts = PgConnectOptions::new()
                                    .host(&def.host).port(def.port)
                                    .username(&def.user).password(&resolved)
                                    .database(&def.database)
                                    .application_name("Parliament - List Schemas");
                                match connect_pg(opts, None).await {
                                    Err(e) => Err(format!("connect: {}", e)),
                                    Ok(mut conn) => {
                                        // pg_namespace covers user schemas + system ones
                                        // (pg_catalog, pg_toast, information_schema).
                                        // Filter system schemas — the user wants to pick
                                        // a default schema for their work, not pg_catalog.
                                        let sql = "SELECT nspname FROM pg_catalog.pg_namespace \
                                                   WHERE nspname NOT LIKE 'pg\\_%' ESCAPE '\\' \
                                                     AND nspname <> 'information_schema' \
                                                   ORDER BY nspname";
                                        match conn.fetch_all(sqlx::raw_sql(sql)).await {
                                            Err(e) => Err(format!("query: {}", e)),
                                            Ok(rows) => Ok(rows.iter().map(|r| {
                                                use sqlx::Row;
                                                r.try_get::<String, _>(0).unwrap_or_default()
                                            }).collect()),
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            };
            match result {
                Ok(schemas) => vec![ServerMessage::SchemaList {
                    schemas,
                    client_tab_id,
                }],
                Err(message) => vec![ServerMessage::SchemaListError {
                    message: format!("Failed to list schemas: {}", message),
                    client_tab_id,
                }],
            }
        }

        // ── Session lifecycle ─────────────────────────────────────────────────
        ClientMessage::Connect { connection_id, client_tab_id, database, client_role, timezone } => {
            let def = {
                let file = state.connections.lock().await;
                file.connections.iter().find(|c| c.id == connection_id).cloned()
                    .map(|mut c| {
                        if let Some(db) = database { c.database = db; }
                        c
                    })
            };

            let Some(def) = def else {
                return vec![ServerMessage::SessionError {
                    client_tab_id,
                    connection_id,
                    message: "Connection not found".into(),
                    error: None,
                }];
            };

            // Compose the PG application_name from the client-supplied role.
            // Shows up in pg_stat_activity.application_name so DBAs can
            // identify Parliament sessions and tell panes apart (Shell vs
            // Schema Explorer vs Activity Monitor). Falls back to plain
            // "Parliament" when the client didn't send a role (older
            // frontends via #[serde(default)]).
            //
            // MySQL note: sqlx-mysql 0.8 does not expose a way to set the
            // `program_name` connect attribute (equivalent of PG's
            // application_name). MySQL sessions are unlabeled today; revisit
            // when sqlx-mysql adds an .attribute() or equivalent API.
            let app_name = match client_role.as_deref() {
                Some(role) if !role.is_empty() => format!("Parliament - {}", role),
                _ => "Parliament".to_string(),
            };

            // Dispatch to the right connect path based on the connection's
            // DB type. Postgres requires a target database; MySQL doesn't.
            // A refusal keeps the server's fields, for the CLI's wording.
            let refused = |e: sqlx::Error| (db_error::message(&e), db_error::fields(&e));
            let connect_result: Result<LiveConnection, (String, Option<DbError>)> = match def.db_type {
                DbType::MySql => {
                    let mut opts = MySqlConnectOptions::new()
                        .host(&def.host)
                        .port(def.port)
                        .username(&def.user)
                        .password(&def.password);
                    if !def.database.is_empty() {
                        opts = opts.database(&def.database);
                    }
                    connect_mysql(opts)
                        .await
                        .map(LiveConnection::MySql)
                        .map_err(refused)
                }
                DbType::Postgres => {
                    if def.database.trim().is_empty() {
                        return vec![ServerMessage::SessionError {
                            client_tab_id,
                            connection_id,
                            message: "PostgreSQL connections require a target database".into(),
                            error: None,
                        }];
                    }
                    match resolve_pg_password(&def, &def.database) {
                        Err(msg) => Err((msg, None)),
                        Ok(resolved) => {
                            let opts = PgConnectOptions::new()
                                .host(&def.host)
                                .port(def.port)
                                .username(&def.user)
                                .password(&resolved)
                                .database(&def.database)
                                .application_name(&app_name);
                            connect_pg(opts, timezone.as_deref())
                                .await
                                .map(LiveConnection::Postgres)
                                .map_err(refused)
                        }
                    }
                }
            };

            match connect_result {
                Err((message, error)) => vec![ServerMessage::SessionError {
                    client_tab_id,
                    connection_id,
                    message,
                    error,
                }],
                Ok(mut conn) => {
                    let version = conn.version().await;
                    let db_connection_id = conn.db_connection_id().await;
                    // Probe installed extensions once at session open so
                    // plane methods that need to gate on extension
                    // availability (e.g. table_vm_sql / pg_visibility)
                    // never have to reference an extension function by
                    // name unless it's actually installed. Called on the
                    // same connection that's about to be pinned into the
                    // session, so results are guaranteed consistent with
                    // what queries on this session will see. Empty on
                    // MySQL (no analogous mechanism).
                    let extensions = conn.extensions().await;
                    let password_used = conn.password_used();

                    let session_id = Uuid::new_v4().to_string();
                    state
                        .sessions
                        .lock()
                        .await
                        .insert(session_id.clone(), Arc::new(Mutex::new(conn)));

                    vec![ServerMessage::SessionOpened {
                        session_id,
                        client_tab_id,
                        connection_id,
                        server_version: version,
                        database: def.database.clone(),
                        db_connection_id,
                        extensions,
                        password_used,
                        ssl_library: SSL_LIBRARY.into(),
                        numeric_locale: locale::numeric_locale(),
                    }]
                }
            }
        }

        ClientMessage::Disconnect { session_id } => {
            state.sessions.lock().await.remove(&session_id);
            vec![ServerMessage::SessionClosed { session_id }]
        }

        // ── Schema introspection ─────────────────────────────────────────────
        ClientMessage::IntrospectQuery { session_id, sql, tag } => {
            let conn = state.sessions.lock().await.get(&session_id).cloned();
            match conn {
                None => vec![ServerMessage::IntrospectError {
                    session_id,
                    tag,
                    message: "Session not found".into(),
                    error: None,
                }],
                Some(conn) => {
                    let mut c = conn.lock().await;
                    let result = execute_introspect(&mut c, &sql, &session_id, &tag).await;
                    let is_error = matches!(result, ServerMessage::IntrospectError { .. });
                    finalize_session_response(&state, &mut c, &session_id, result, is_error).await
                }
            }
        }

        ClientMessage::IntrospectBatch { session_id, tag, queries } => {
            let conn = state.sessions.lock().await.get(&session_id).cloned();
            match conn {
                None => vec![ServerMessage::IntrospectBatchResult {
                    session_id,
                    tag,
                    result: BatchResult { sets: Vec::new(), error: Some(DbError::from_message("Session not found")) },
                    connection_lost: false,
                }],
                Some(conn) => {
                    let mut c = conn.lock().await;
                    let result = c.fetch_result_sets(&queries).await;
                    let is_error = result.error.is_some();
                    let response = ServerMessage::IntrospectBatchResult { session_id: session_id.clone(), tag, result, connection_lost: false };
                    finalize_session_response(state, &mut c, &session_id, response, is_error).await
                }
            }
        }

        // ── DML for the Data tab editor ──────────────────────────────────────
        ClientMessage::ExecuteDml { session_id, sql, tag } => {
            let conn = state.sessions.lock().await.get(&session_id).cloned();
            match conn {
                None => vec![ServerMessage::DmlError {
                    session_id,
                    tag,
                    message: "Session not found".into(),
                }],
                Some(conn) => {
                    let mut c = conn.lock().await;
                    let result = match c.execute_raw(&sql).await {
                        Ok(n) => ServerMessage::DmlResult {
                            session_id: session_id.clone(), tag, rows_affected: n,
                        },
                        Err(e) => ServerMessage::DmlError {
                            session_id: session_id.clone(), tag, message: db_error::message(&e),
                        },
                    };
                    let is_error = matches!(result, ServerMessage::DmlError { .. });
                    finalize_session_response(&state, &mut c, &session_id, result, is_error).await
                }
            }
        }

        // ── External-editor round-trip ───────────────────────────────────────
        // Open the user's editor on `content` and return what they
        // saved. Session-pinned via session_id so the response routes
        // back to the right pane; `tag` distinguishes the originating
        // meta-command (`\e` / `\ef` / `\ev`) so the frontend applies
        // it to the right surface. The editor is chosen here, from
        // noctua's own files (see editor/resolve.rs) — the message
        // can't name a command.
        ClientMessage::EditExternally {
            session_id, connection_id, content, line, tag, env_chain, line_flag_var,
        } => {
            let session_exists = state.sessions.lock().await.contains_key(&session_id);
            if !session_exists {
                return vec![ServerMessage::EditExternallyError {
                    session_id, tag, message: "Session not found".into(),
                }];
            }
            let connection_override = match &connection_id {
                Some(id) => state.connections.lock().await.connections.iter()
                    .find(|c| &c.id == id)
                    .and_then(|c| c.editor_command.clone()),
                None => None,
            };
            let editor_settings = state.settings.lock().await.settings.editor.clone();
            let request = editor::Request {
                connection_override: connection_override.as_deref(),
                settings:            &editor_settings,
                env_chain:           &env_chain,
                line_flag_var,
            };
            match editor::edit(&request, &content, line).await {
                Ok(editor::Outcome::Saved(new_content)) => vec![ServerMessage::EditExternallyResult {
                    session_id, tag, content: new_content,
                }],
                Ok(editor::Outcome::Discarded(message)) => vec![ServerMessage::EditExternallyCancelled {
                    session_id, tag, message,
                }],
                Err(e) => vec![ServerMessage::EditExternallyError {
                    session_id, tag, message: e,
                }],
            }
        }

        // ── Query execution ───────────────────────────────────────────────────
        ClientMessage::ExecuteQuery { session_id, sql } => {
            let conn = state.sessions.lock().await.get(&session_id).cloned();
            match conn {
                None => vec![ServerMessage::QueryError {
                    session_id,
                    message: "Session not found".into(),
                    error: None,
                    connection_lost: false,
                }],
                Some(conn) => {
                    let mut c = conn.lock().await;
                    let result = execute_query(&mut c, &sql, &session_id).await;
                    let is_error = matches!(result, ServerMessage::QueryError { .. });
                    finalize_session_response(&state, &mut c, &session_id, result, is_error).await
                }
            }
        }

        // ── Describe query (\gdesc) ──────────────────────────────────────────
        ClientMessage::DescribeQuery { session_id, sql } => {
            let conn = state.sessions.lock().await.get(&session_id).cloned();
            match conn {
                None => vec![ServerMessage::DescribeError {
                    session_id,
                    message: "Session not found".into(),
                    error: None,
                }],
                Some(conn) => {
                    let mut c = conn.lock().await;
                    let started = Instant::now();
                    let result = match c.describe_columns(&sql).await {
                        Ok(columns) => ServerMessage::DescribeResult {
                            session_id: session_id.clone(),
                            columns,
                            exec_time_us: started.elapsed().as_micros() as u64,
                        },
                        Err(e) => ServerMessage::DescribeError {
                            session_id: session_id.clone(),
                            message: db_error::message(&e),
                            error: db_error::fields(&e),
                        },
                    };
                    let is_error = matches!(result, ServerMessage::DescribeError { .. });
                    finalize_session_response(&state, &mut c, &session_id, result, is_error).await
                }
            }
        }
    }
}

/// Wrap a per-session query/introspect/DML response with a liveness probe.
/// If the inner operation errored AND the upstream DB connection is now
/// dead, drop the session from `state.sessions` and append
/// `ServerMessage::SessionClosed` so the frontend can surface the
/// "Session ended — Reconnect" banner. The frontend's banner condition
/// (`session_stale && session_id.is_none()`) is the SessionClosed shape
/// specifically — without us emitting SessionClosed, the dead session
/// would surface only as the error string in the shell, and the user
/// would have no recovery affordance.
///
/// On a live session the session's transaction state goes first
/// (`TransactionStatus`): any command can change it — on Postgres an
/// error inside a transaction block fails the block, a `\d` batch
/// included; on MySQL a statement can commit implicitly (DDL), and a
/// deadlock rolls the transaction back (read after the error's ping).
async fn finalize_session_response(
    state:      &AppState,
    conn:       &mut LiveConnection,
    session_id: &str,
    response:   ServerMessage,
    is_error:   bool,
) -> Vec<ServerMessage> {
    if is_error && !conn.is_alive().await {
        // Connection is dead. Note that we DON'T drop `conn` here — the
        // caller holds the mutex guard. `remove` from state.sessions
        // drops the Arc<Mutex<LiveConnection>>; once the caller's guard
        // is dropped (returning from process_message), the inner
        // LiveConnection drops too, closing the upstream TCP via sqlx.
        state.sessions.lock().await.remove(session_id);
        // A command that found its session dead says so, for a client
        // that carries on as its CLI does (mysql reconnects and runs it
        // again; psql resets at once).
        let response = match response {
            ServerMessage::QueryError { session_id, message, error, .. } =>
                ServerMessage::QueryError { session_id, message, error, connection_lost: true },
            ServerMessage::IntrospectBatchResult { session_id, tag, result, .. } =>
                ServerMessage::IntrospectBatchResult { session_id, tag, result, connection_lost: true },
            other => other,
        };
        vec![response, ServerMessage::SessionClosed { session_id: session_id.into() }]
    } else {
        match conn.transaction_state().await {
            Some(txn) => vec![ServerMessage::TransactionStatus { session_id: session_id.into(), state: txn }, response],
            None => vec![response],
        }
    }
}

// ── Schema introspection helper ───────────────────────────────────────────────

async fn execute_introspect(
    conn: &mut LiveConnection,
    sql: &str,
    session_id: &str,
    tag: &str,
) -> ServerMessage {
    match conn.fetch_all_strings(sql).await {
        Err(e) => ServerMessage::IntrospectError {
            session_id: session_id.into(),
            tag: tag.into(),
            message: db_error::message(&e),
            error: db_error::fields(&e),
        },
        Ok((columns, rows, _)) => ServerMessage::IntrospectResult {
            session_id: session_id.into(),
            tag: tag.into(),
            columns,
            rows,
        },
    }
}

// ── Query execution ───────────────────────────────────────────────────────────

/// A Postgres column's type as the server sent it: its OID — which sqlx
/// knows for the types psql lines up, even those it can't name (`xid`).
fn pg_type_oid(column: &sqlx::postgres::PgColumn) -> String {
    use sqlx::Column;
    column.type_info().oid().map(|oid| oid.0.to_string()).unwrap_or_default()
}

async fn execute_query(conn: &mut LiveConnection, sql: &str, session_id: &str) -> ServerMessage {
    let start = Instant::now();
    let sid = session_id.to_string();

    // Single unified path. `fetch_all_strings` now uses sqlx's `fetch_many`
    // streaming API, which gives us both rows and rows_affected in one
    // pass. No more SQL-prefix dispatch — `INSERT … RETURNING id`,
    // `UPDATE … RETURNING tier_changed_at`, `DELETE … RETURNING *`, and
    // any future row-returning DML (PG 17+ MERGE … RETURNING, MariaDB's
    // RETURNING extension, etc.) all just work without us enumerating
    // them.
    //
    // For row-bearing responses the count is `rows.len()`; for vanilla
    // INSERT / UPDATE / DELETE / DDL it's the protocol's `rows_affected`.
    match conn.fetch(sql).await {
        Err(e) => ServerMessage::QueryError {
            session_id: sid,
            message: db_error::message(&e),
            error: db_error::fields(&e),
            connection_lost: false,
        },
        Ok(fetched) => ServerMessage::QueryResult {
            session_id: sid,
            columns: fetched.columns,
            types: fetched.types,
            rows: fetched.rows,
            exec_time_us: start.elapsed().as_micros() as u64,
            status: fetched.status,
        },
    }
}

#[cfg(test)]
mod tests {
    //! Concurrency classification for `needs_spawn`. Each `ClientMessage`
    //! variant gets one test asserting the expected dispatch class.
    //!
    //! Adding a new variant without adding a test here leaves coverage
    //! visibly incomplete at review time — this doesn't enforce
    //! exhaustiveness at compile time, but it gives a reliable paper
    //! trail. Guidance: any handler that does a DB round-trip (network
    //! I/O) must go in `spawn`; handlers that only touch the in-memory
    //! connection store stay `inline` so "save then list" returns the
    //! saved item.
    use super::*;
    use shared::{ConnectionDef, ConnectionDefPublic};

    fn dummy_def() -> ConnectionDef {
        ConnectionDef {
            id: "t".into(),
            name: "t".into(),
            db_type: DbType::MySql,
            host: "".into(),
            port: 0,
            user: "".into(),
            password: "".into(),
            database: "".into(),
            paths: vec![],
            default_pane: "shell".into(),
            env: None,
            format_style: shared::FormatStyle::default(),
            editor_command: None,
            buffer_threshold_mb: None,
            default_schema: None,
            credential_source: shared::CredentialSource::Plaintext,
        }
    }

    fn dummy_pub() -> ConnectionDefPublic {
        ConnectionDefPublic::from(&dummy_def())
    }

    // ── Inline handlers (connection store only; ordering matters) ─────

    #[test]
    fn list_connections_is_inline() {
        assert!(!needs_spawn(&ClientMessage::ListConnections));
        assert!(!needs_spawn(&ClientMessage::GetSettings));
        assert!(!needs_spawn(&ClientMessage::SaveSettings { settings: Default::default() }));
    }

    #[test]
    fn save_connection_is_inline() {
        assert!(!needs_spawn(&ClientMessage::SaveConnection { def: dummy_pub(), password_change: None, copy_from: None }));
    }

    #[test]
    fn delete_connection_is_inline() {
        assert!(!needs_spawn(&ClientMessage::DeleteConnection { id: "t".into() }));
    }

    #[test]
    fn save_folders_is_inline() {
        assert!(!needs_spawn(&ClientMessage::SaveFolders { folders: vec![] }));
    }

    // ── On-disk file migration ────────────────────────────────────────

    #[test]
    fn parses_v2_file_shape() {
        let json = r#"{
            "version": 2,
            "folders": ["Production", "Production/MySQL"],
            "connections": []
        }"#;
        let file = parse_connections_file(json);
        assert_eq!(file.version, 2);
        assert_eq!(file.folders, vec!["Production", "Production/MySQL"]);
        assert!(file.connections.is_empty());
    }

    #[test]
    fn parses_legacy_array_shape() {
        let json = r#"[{
            "id": "c1",
            "name": "Old",
            "db_type": "mysql",
            "host": "h", "port": 3306,
            "user": "u", "password": "p", "database": "d",
            "folder_path": "Old/MySQL",
            "pinned": true
        }]"#;
        let file = parse_connections_file(json);
        assert_eq!(file.version, 2);
        assert!(file.folders.is_empty());
        assert_eq!(file.connections.len(), 1);
        // folder_path migrated into paths
        assert_eq!(file.connections[0].paths, vec!["Old/MySQL".to_string()]);
        // pinned field accepted then discarded — no field to assert against
    }

    #[test]
    fn legacy_empty_folder_path_becomes_root() {
        let json = r#"[{
            "id": "c1", "name": "Root", "db_type": "mysql",
            "host": "h", "port": 3306, "user": "u", "password": "p",
            "database": "d", "folder_path": null
        }]"#;
        let file = parse_connections_file(json);
        assert_eq!(file.connections[0].paths, Vec::<String>::new());
    }

    #[test]
    fn malformed_file_falls_back_to_empty() {
        let file = parse_connections_file("not json at all");
        assert!(file.connections.is_empty());
        assert!(file.folders.is_empty());
    }

    /// `path`'s permission bits.
    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn saved_connections_are_private_even_over_a_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        std::fs::write(&path, "[]").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let file = parse_connections_file(r#"{"version": 2, "folders": ["Prod"], "connections": []}"#);
        save_connections_to(&path, &file);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(parse_connections_file(&std::fs::read_to_string(&path).unwrap()).folders, vec!["Prod"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_connections_file_others_can_read_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        std::fs::write(&path, "[]").unwrap();
        for loose in [0o644, 0o640, 0o604, 0o666] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(loose)).unwrap();
            make_private(&path);
            assert_eq!(mode(&path), 0o600, "from {loose:o}");
        }
        // The user's alone already, if stricter: left as it is.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        make_private(&path);
        assert_eq!(mode(&path), 0o400);
        // No file yet: nothing to do.
        make_private(&dir.path().join("missing.json"));
    }

    #[test]
    fn disconnect_is_inline() {
        assert!(!needs_spawn(&ClientMessage::Disconnect { session_id: "t".into() }));
    }

    // ── Spawned handlers (DB round-trips) ─────────────────────────────

    #[test]
    fn connect_is_spawned() {
        assert!(needs_spawn(&ClientMessage::Connect {
            connection_id: "t".into(),
            client_tab_id: "t".into(),
            database: None,
            client_role: None,
            timezone: None,
        }));
    }

    #[test]
    fn test_connection_is_spawned() {
        assert!(needs_spawn(&ClientMessage::TestConnection { def: dummy_def() }));
    }

    #[test]
    fn list_databases_is_spawned() {
        assert!(needs_spawn(&ClientMessage::ListDatabases { def: dummy_def(), client_tab_id: None }));
    }

    #[test]
    fn list_schemas_is_spawned() {
        assert!(needs_spawn(&ClientMessage::ListSchemas { def: dummy_def(), client_tab_id: None }));
    }

    #[test]
    fn execute_query_is_spawned() {
        assert!(needs_spawn(&ClientMessage::ExecuteQuery {
            session_id: "t".into(),
            sql: "SELECT 1".into(),
        }));
    }

    #[test]
    fn introspect_query_is_spawned() {
        assert!(needs_spawn(&ClientMessage::IntrospectQuery {
            session_id: "t".into(),
            sql: "SELECT 1".into(),
            tag: "t".into(),
        }));
    }

    #[test]
    fn introspect_batch_is_spawned() {
        assert!(needs_spawn(&ClientMessage::IntrospectBatch {
            session_id: "t".into(),
            tag: "t".into(),
            queries: vec!["SELECT 1".into()],
        }));
    }

    /// What psql prints, kept by the patched sqlx-postgres
    /// (vendor/sqlx-postgres): each statement's command tag, and the
    /// columns of an empty result. Everything runs in a transaction that
    /// fails and is rolled back. Needs the local Pagila database:
    /// `cargo test -p noctua -- --ignored pg_results_keep_tag_and_columns`
    #[tokio::test]
    #[ignore]
    async fn pg_results_keep_tag_and_columns() {
        use sqlx::postgres::PgConnectOptions;

        async fn run(live: &mut LiveConnection, sql: &str) -> (Vec<String>, usize, String) {
            match execute_query(live, sql, "s").await {
                ServerMessage::QueryResult { columns, rows, status, .. } =>
                    (columns, rows.len(), status.tag.expect("a tag")),
                other => panic!("{sql}: {other:?}"),
            }
        }
        let cols = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();

        let mut opts = PgConnectOptions::new();
        if std::env::var("PGUSER").is_err()     { opts = opts.username("postgres"); }
        if std::env::var("PGDATABASE").is_err() { opts = opts.database("pagila"); }
        let mut live = LiveConnection::Postgres(connect_pg(opts, None).await.expect("connect"));

        // An empty result keeps its columns; a command has none.
        assert_eq!(run(&mut live, "SELECT actor_id, first_name FROM actor WHERE false").await,
                   (cols(&["actor_id", "first_name"]), 0, "SELECT 0".into()));
        assert_eq!(run(&mut live, "SELECT actor_id FROM actor WHERE actor_id = 1").await,
                   (cols(&["actor_id"]), 1, "SELECT 1".into()));
        assert_eq!(run(&mut live, "UPDATE actor SET first_name = first_name WHERE false").await,
                   (cols(&[]), 0, "UPDATE 0".into()));

        assert_eq!(run(&mut live, "BEGIN").await.2, "BEGIN");
        assert_eq!(run(&mut live, "CREATE TEMP TABLE t AS SELECT 1 AS a").await, (cols(&[]), 0, "SELECT 1".into()));
        assert_eq!(run(&mut live, "INSERT INTO t VALUES (2), (3)").await, (cols(&[]), 0, "INSERT 0 2".into()));
        assert_eq!(run(&mut live, "DELETE FROM t WHERE a = 2 RETURNING a").await, (cols(&["a"]), 1, "DELETE 1".into()));
        assert_eq!(run(&mut live, "DELETE FROM t WHERE false RETURNING a").await, (cols(&["a"]), 0, "DELETE 0".into()));
        // Several statements: the last one's.
        assert_eq!(run(&mut live, "SELECT 1 AS x WHERE false; UPDATE t SET a = a WHERE false").await,
                   (cols(&[]), 0, "UPDATE 0".into()));
        assert_eq!(run(&mut live, "UPDATE t SET a = a WHERE false; SELECT 1 AS x WHERE false").await,
                   (cols(&["x"]), 0, "SELECT 0".into()));
        // COMMIT of a failed transaction rolls it back, and says so.
        assert!(matches!(execute_query(&mut live, "SELECT 1/0", "s").await, ServerMessage::QueryError { .. }));
        assert_eq!(run(&mut live, "COMMIT").await.2, "ROLLBACK");

        // Each column's type OID — those sqlx can't name too (xid, cid,
        // xid8) — and a domain's base type, as the server sends it; with
        // no rows as well.
        async fn types(live: &mut LiveConnection, sql: &str) -> Vec<String> {
            match execute_query(live, sql, "s").await {
                ServerMessage::QueryResult { types, .. } => types,
                other => panic!("{sql}: {other:?}"),
            }
        }
        let all = "SELECT 1::int4, 't'::text, '1'::xid, '1'::cid, '1'::xid8, 1.5::numeric, 2::money, 2010::year, '{1}'::int4[]";
        let expected = ["23", "25", "28", "29", "5069", "1700", "790", "23", "1007"];
        assert_eq!(types(&mut live, all).await, expected);
        assert_eq!(types(&mut live, &format!("SELECT * FROM ({all}) s WHERE false")).await, expected);

        // SQL NULL arrives apart from the text 'NULL'.
        match execute_query(&mut live, "SELECT NULL::text, 'NULL'::text, ''::text", "s").await {
            ServerMessage::QueryResult { rows, .. } => assert_eq!(rows, [[None, Some("NULL".into()), Some(String::new())]]),
            other => panic!("{other:?}"),
        }
    }

    /// The session's transaction state comes from the server (the patched
    /// sqlx-postgres) — current after an error too, and back to "in a
    /// transaction" after ROLLBACK TO SAVEPOINT. Everything runs in a
    /// transaction that's rolled back. Needs the local Pagila database:
    /// `cargo test -p noctua -- --ignored pg_transaction_state_follows_the_server`
    #[tokio::test]
    #[ignore]
    async fn pg_transaction_state_follows_the_server() {
        use sqlx::postgres::PgConnectOptions;

        let mut opts = PgConnectOptions::new();
        if std::env::var("PGUSER").is_err()     { opts = opts.username("postgres"); }
        if std::env::var("PGDATABASE").is_err() { opts = opts.database("pagila"); }
        let mut live = LiveConnection::Postgres(connect_pg(opts, None).await.expect("connect"));
        let ok = |m: ServerMessage| assert!(matches!(m, ServerMessage::QueryResult { .. }), "{m:?}");

        assert_eq!(live.transaction_state().await, Some(TxnState::Idle));
        ok(execute_query(&mut live, "BEGIN", "s").await);
        assert_eq!(live.transaction_state().await, Some(TxnState::Active));
        ok(execute_query(&mut live, "SAVEPOINT a", "s").await);
        // The error's ReadyForQuery is read first, so this is current.
        assert!(matches!(execute_query(&mut live, "SELECT 1/0", "s").await, ServerMessage::QueryError { .. }));
        assert_eq!(live.transaction_state().await, Some(TxnState::Failed));
        ok(execute_query(&mut live, "ROLLBACK TO SAVEPOINT a", "s").await);
        assert_eq!(live.transaction_state().await, Some(TxnState::Active));
        // A meta-command's batch that fails inside the block fails it too.
        let batch = live.fetch_result_sets(&["SELECT 1".into(), "SELECT 1/0".into()]).await;
        assert!(batch.error.is_some());
        assert_eq!(live.transaction_state().await, Some(TxnState::Failed));
        ok(execute_query(&mut live, "COMMIT", "s").await);
        assert_eq!(live.transaction_state().await, Some(TxnState::Idle));
    }

    /// Live check of how sqlx splits a multi-statement simple query —
    /// one completion per statement, rows before it — which
    /// `fetch_result_sets` relies on. Needs a Postgres server; libpq-style
    /// env vars pick it (PGHOST, PGPORT, PGPASSWORD from
    /// ~/.parliament/secrets.env), defaulting to postgres@pagila:
    /// `cargo test -p noctua -- --ignored pg_introspect_batch_contract`
    #[tokio::test]
    #[ignore]
    async fn pg_introspect_batch_contract() {
        use sqlx::postgres::PgConnectOptions;

        let mut opts = PgConnectOptions::new();
        if std::env::var("PGUSER").is_err()     { opts = opts.username("postgres"); }
        if std::env::var("PGDATABASE").is_err() { opts = opts.database("pagila"); }
        let conn = connect_pg(opts, None).await.expect("connect");
        let mut live = LiveConnection::Postgres(conn);
        let q = |qs: &[&str]| qs.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let got = live.fetch_result_sets(&q(&[
            "SELECT 1 AS a, NULL::text AS b, 'NULL' AS c",
            "SELECT 1 WHERE false;",
            "SELECT g FROM generate_series(1, 3) g -- trailing comment",
            "SET LOCAL statement_timeout = 0",
        ])).await;
        assert_eq!(got.error, None);
        assert_eq!(got.sets, vec![
            ResultSet { columns: vec!["a".into(), "b".into(), "c".into()],
                        rows: vec![vec![Some("1".into()), None, Some("NULL".into())]] },
            ResultSet::default(),
            ResultSet { columns: vec!["g".into()],
                        rows: vec![vec![Some("1".into())], vec![Some("2".into())], vec![Some("3".into())]] },
            ResultSet::default(),
        ]);

        // Stops at the first error, keeping what completed before it.
        let got = live.fetch_result_sets(&q(&["SELECT 1", "SELECT 1/0", "SELECT 2"])).await;
        assert_eq!(got.sets.len(), 1);
        assert!(got.error.as_ref().is_some_and(|e| e.message == "division by zero" && e.sqlstate.as_deref() == Some("22012")), "{got:?}");

        // A query holding two statements is refused, not misread.
        let got = live.fetch_result_sets(&q(&["SELECT 1; SELECT 2", "SELECT 3"])).await;
        assert_eq!(got.error.map(|e| e.message).as_deref(), Some("2 queries returned 3 result sets"));

        // The session is usable afterwards.
        assert_eq!(live.fetch_result_sets(&q(&["SELECT 4"])).await.sets[0].rows, vec![vec![Some("4".into())]]);
    }

    /// The same contract on MySQL (sqlx sets CLIENT_MULTI_STATEMENTS),
    /// against the local sakila database (MYSQL_PWD from
    /// ~/.parliament/secrets.env):
    /// `cargo test -p noctua -- --ignored mysql_introspect_batch_contract`
    #[tokio::test]
    #[ignore]
    async fn mysql_introspect_batch_contract() {
        let mut live = LiveConnection::MySql(sakila().await);
        let q = |qs: &[&str]| qs.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let got = live.fetch_result_sets(&q(&[
            "SELECT 1 AS a, NULL AS b, 'NULL' AS c",
            "SELECT 1 FROM DUAL WHERE false;",
            "SELECT 2 AS g -- trailing comment",
        ])).await;
        assert_eq!(got.error, None);
        assert_eq!(got.sets, vec![
            ResultSet { columns: vec!["a".into(), "b".into(), "c".into()],
                        rows: vec![vec![Some("1".into()), None, Some("NULL".into())]] },
            ResultSet::default(),
            ResultSet { columns: vec!["g".into()], rows: vec![vec![Some("2".into())]] },
        ]);

        let got = live.fetch_result_sets(&q(&["SELECT 1", "SELECT * FROM no_such_table", "SELECT 2"])).await;
        assert_eq!(got.sets.len(), 1);
        assert!(got.error.as_ref().is_some_and(|e| e.message.contains("no_such_table") && e.number == Some(1146)), "{got:?}");

        assert_eq!(live.fetch_result_sets(&q(&["SELECT 4"])).await.sets[0].rows, vec![vec![Some("4".into())]]);
    }

    /// A session on the local sakila database, opened the way noctua opens
    /// one (MYSQL_PWD from ~/.parliament/secrets.env).
    async fn sakila() -> MySqlConnection {
        let mut opts = MySqlConnectOptions::new().username("root").database("sakila");
        if let Ok(pwd) = std::env::var("MYSQL_PWD") { opts = opts.password(&pwd); }
        connect_mysql(opts).await.expect("connect")
    }

    /// Whether the server asked for a password when the session opened
    /// is libpq's `PQconnectionUsedPassword` — the "Password Used" row of
    /// psql's `\conninfo` for the same user (`PGUSER`; a trust user gives
    /// false, a password user true). Needs psql on PATH and the local
    /// pagila database: `cargo test -p noctua -- --ignored pg_password_used_matches_psql`
    #[tokio::test]
    #[ignore]
    async fn pg_password_used_matches_psql() {
        let user = std::env::var("PGUSER").unwrap_or_else(|_| "postgres".into());
        let database = std::env::var("PGDATABASE").unwrap_or_else(|_| "pagila".into());
        let out = std::process::Command::new("psql")
            .args(["-X", "-h", "127.0.0.1", "-U", &user, &database, "-c", "\\conninfo"])
            .output().expect("psql on PATH");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let psql = String::from_utf8_lossy(&out.stdout).lines()
            .find_map(|line| line.trim_start().strip_prefix("Password Used").map(|rest| rest.trim_start_matches([' ', '|']).trim().to_string()))
            .expect("psql's Password Used row");
        let opts = PgConnectOptions::new().host("127.0.0.1").username(&user).database(&database);
        let conn = connect_pg(opts, None).await.expect("connect");
        assert_eq!(conn.password_used().to_string(), psql);
    }

    /// A statement's result columns, as psql's `\gdesc` describes them:
    /// the patched sqlx's `describe_result_columns` gives each column's
    /// name, type OID and modifier — named here by the server's
    /// `format_type`, as psql's second query names them — the same as
    /// psql's `\gdesc` prints; nothing runs, and no prepared statement is
    /// left on the session. Needs psql on PATH and the local pagila
    /// database: `cargo test -p noctua -- --ignored pg_describe_matches_psql`
    #[tokio::test]
    #[ignore]
    async fn pg_describe_matches_psql() {
        use sqlx::Row;
        use std::io::Write;
        const STATEMENTS: [&str; 5] = [
            "select film_id, title, 'x'::varchar(30) as v, 1.5::numeric(5,2) as n, now() as t, \
             array[1]::int[] as arr, null as u, rating, '1 day'::interval hour as i, 'b'::bit(3) as b from film",
            "select 1 as \"Mixed Case\", 'it''s' as \"o'neil\", 1::numeric as plain",
            "insert into actor (first_name, last_name) values ('A', 'B') returning actor_id, last_update",
            "update actor set first_name = first_name where false",
            "select * from film_list",
        ];
        let user = std::env::var("PGUSER").unwrap_or_else(|_| "postgres".into());
        let database = std::env::var("PGDATABASE").unwrap_or_else(|_| "pagila".into());
        let opts = PgConnectOptions::new().host("127.0.0.1").username(&user).database(&database);
        let mut conn = connect_pg(opts, None).await.expect("connect");
        let actors = "select count(*)::text from actor";
        let before: String = sqlx::raw_sql(actors).fetch_one(&mut conn).await.expect("count").get(0);
        for statement in STATEMENTS {
            let mut psql = std::process::Command::new("psql")
                .args(["-X", "-A", "-t", "-h", "127.0.0.1", "-U", &user, &database])
                .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
                .spawn().expect("psql on PATH");
            psql.stdin.take().unwrap().write_all(format!("{statement} \\gdesc\n").as_bytes()).unwrap();
            let theirs = String::from_utf8_lossy(&psql.wait_with_output().unwrap().stdout).trim_end().to_string();
            let mut ours = Vec::new();
            for (name, oid, typmod) in conn.describe_result_columns(statement).await.expect(statement) {
                let row = sqlx::raw_sql(&format!("select pg_catalog.format_type({oid}, {typmod})"))
                    .fetch_one(&mut conn).await.expect("format_type");
                ours.push(format!("{name}|{}", row.get::<String, _>(0)));
            }
            let ours = if ours.is_empty() { "The command has no result, or the result has no columns.".to_string() } else { ours.join("\n") };
            assert_eq!(ours, theirs, "{statement}");
        }
        // Described, not run; and nothing left prepared.
        let after: String = sqlx::raw_sql(actors).fetch_one(&mut conn).await.expect("count").get(0);
        assert_eq!(after, before);
        let left: String = sqlx::raw_sql("select count(*)::text from pg_prepared_statements")
            .fetch_one(&mut conn).await.expect("count").get(0);
        assert_eq!(left, "0");
    }

    /// A noctua Postgres session has the settings a psql session has —
    /// the server's TimeZone, DateStyle, extra_float_digits,
    /// IntervalStyle, unless libpq's environment variables say otherwise —
    /// so a timestamp, a date and a float print as psql prints them; and
    /// the browser's time zone beats `PGTZ`. Each case is psql run with
    /// the same variables. Needs psql on PATH and the local pagila
    /// database: `cargo test -p noctua -- --ignored pg_session_matches_psql`
    #[tokio::test]
    #[ignore]
    async fn pg_session_matches_psql() {
        const SESSION: &str = "SELECT current_setting('TimeZone'), current_setting('DateStyle'), \
                               current_setting('extra_float_digits'), current_setting('IntervalStyle'), \
                               '2026-10-02 12:34:56.5+02'::timestamptz, '2026-10-02'::date, 0.1::float8 * 3";
        let user = std::env::var("PGUSER").unwrap_or_else(|_| "postgres".into());
        let database = std::env::var("PGDATABASE").unwrap_or_else(|_| "pagila".into());
        let psql = |env: &[(&str, &str)]| -> Vec<String> {
            let out = std::process::Command::new("psql")
                .args(["-X", "-A", "-t", "-F", "\t", "-h", "127.0.0.1", "-U", &user, &database, "-c", SESSION])
                .envs(env.iter().copied())
                .output().expect("psql on PATH");
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim_end().split('\t').map(String::from).collect()
        };
        let opts = || PgConnectOptions::new().host("127.0.0.1").username(&user).database(&database);
        let noctua = |settings: Vec<(&'static str, String)>| {
            let opts = opts();
            async move {
                let mut live = LiveConnection::Postgres(connect_pg_with(opts, &settings).await.expect("connect"));
                live.fetch_all_strings(SESSION).await.expect("session settings").1.remove(0)
            }
        };

        // As this process's environment has it — the server's settings
        // when PGTZ and the rest are unset.
        let mut live = LiveConnection::Postgres(connect_pg(opts(), None).await.expect("connect"));
        assert_eq!(live.fetch_all_strings(SESSION).await.expect("session settings").1[0], psql(&[]));

        // libpq's variables, as psql reads them.
        let env = [("PGTZ", "Europe/Berlin"), ("PGDATESTYLE", "SQL, DMY")];
        let lookup = |var: &str| env.iter().find(|(name, _)| *name == var).map(|(_, value)| value.to_string());
        let got = noctua(pg_session_settings(&lookup, None)).await;
        assert_eq!(got, psql(&env));
        assert_eq!(&got[..2], ["Europe/Berlin", "SQL, DMY"]);

        // The browser's zone over PGTZ: what psql gives with that PGTZ.
        let got = noctua(pg_session_settings(&lookup, Some("Asia/Tokyo"))).await;
        assert_eq!(got, psql(&[("PGTZ", "Asia/Tokyo"), ("PGDATESTYLE", "SQL, DMY")]));

        // A zone the server doesn't know refuses the session, as psql's.
        let refused = connect_pg_with(opts(), &pg_session_settings(&|_| None, Some("Nowhere/Land"))).await;
        let message = crate::db_error::message(&refused.expect_err("unknown zone"));
        assert_eq!(message, "invalid value for parameter \"TimeZone\": \"Nowhere/Land\"");
    }

    #[test]
    fn session_settings_are_libpqs_then_the_browsers_zone() {
        let env = |pairs: &'static [(&'static str, &'static str)]| move |var: &str| {
            pairs.iter().find(|(name, _)| *name == var).map(|(_, value)| value.to_string())
        };
        assert_eq!(pg_session_settings(&env(&[]), None), []);
        assert_eq!(pg_session_settings(&env(&[("PGTZ", "UTC"), ("PGDATESTYLE", "SQL, DMY"), ("PGGEQO", "off")]), None),
                   [("datestyle", "SQL, DMY".to_string()), ("timezone", "UTC".to_string()), ("geqo", "off".to_string())]);
        // libpq skips `default`.
        assert_eq!(pg_session_settings(&env(&[("PGTZ", "DEFAULT")]), None), []);
        // The browser's zone replaces PGTZ's.
        assert_eq!(pg_session_settings(&env(&[("PGTZ", "UTC")]), Some("Asia/Tokyo")), [("timezone", "Asia/Tokyo".to_string())]);
    }

    /// A noctua MySQL session has the settings a mysql CLI session has —
    /// time zone, sql_mode, character sets, collation — so the same SQL
    /// means the same thing in both (`||` included). Needs the mysql CLI
    /// on PATH and the local sakila database:
    /// `cargo test -p noctua -- --ignored mysql_session_matches_the_cli`
    #[tokio::test]
    #[ignore]
    async fn mysql_session_matches_the_cli() {
        const SESSION: &str = "SELECT @@session.time_zone, @@session.sql_mode, @@character_set_client, \
                               @@character_set_connection, @@character_set_results, @@collation_connection, 1 || 0";
        let cli = std::process::Command::new("mysql")
            .args(["-u", "root", "sakila", "--batch", "--skip-column-names", "-e", SESSION])
            .output().expect("mysql on PATH");
        assert!(cli.status.success(), "{}", String::from_utf8_lossy(&cli.stderr));
        let cli: Vec<String> = String::from_utf8_lossy(&cli.stdout).trim_end().split('\t').map(String::from).collect();

        let mut live = LiveConnection::MySql(sakila().await);
        let (_, rows, _) = live.fetch_all_strings(SESSION).await.expect("session settings");
        assert_eq!(rows[0], cli);

    }

    /// What the mysql CLI prints for a statement — rows affected (changed,
    /// not matched), warnings, the info line — and whether the session is
    /// in a transaction, kept by the patched sqlx-mysql
    /// (vendor/sqlx-mysql). The expected text is the mysql CLI's for the
    /// same statements. Everything runs on a temporary table. Needs the
    /// local sakila database:
    /// `cargo test -p noctua -- --ignored mysql_results_and_transactions_match_the_cli`
    #[tokio::test]
    #[ignore]
    async fn mysql_results_and_transactions_match_the_cli() {
        async fn run(live: &mut LiveConnection, sql: &str) -> CommandStatus {
            match execute_query(live, sql, "s").await {
                ServerMessage::QueryResult { status, .. } => status,
                other => panic!("{sql}: {other:?}"),
            }
        }
        let status = |rows_affected, warnings, info: Option<&str>| CommandStatus {
            rows_affected, tag: None, warnings, info: info.map(String::from),
        };
        let mut live = LiveConnection::MySql(sakila().await);

        assert_eq!(run(&mut live, "CREATE TEMPORARY TABLE t (a int)").await, status(0, 0, None));
        assert_eq!(live.transaction_state().await, Some(TxnState::Idle));
        run(&mut live, "BEGIN").await;
        assert_eq!(live.transaction_state().await, Some(TxnState::Active));
        assert_eq!(run(&mut live, "INSERT INTO t VALUES (1),(2)").await,
                   status(2, 0, Some("Records: 2  Duplicates: 0  Warnings: 0")));
        assert_eq!(run(&mut live, "INSERT INTO t VALUES (3)").await, status(1, 0, None));
        // Rows changed, not matched: the CLI's "0 rows affected".
        assert_eq!(run(&mut live, "UPDATE t SET a = a").await,
                   status(0, 0, Some("Rows matched: 3  Changed: 0  Warnings: 0")));
        run(&mut live, "ROLLBACK").await;
        assert_eq!(live.transaction_state().await, Some(TxnState::Idle));
        assert_eq!(run(&mut live, "CREATE TABLE IF NOT EXISTS actor (x int)").await, status(0, 1, None));
        assert_eq!(run(&mut live, "SELECT 1/0").await, status(1, 1, None));

        // A statement that fails can still end the transaction — DDL
        // commits before it runs. Its error reply has no status; the
        // ping noctua sends after an error reads it.
        run(&mut live, "BEGIN").await;
        run(&mut live, "INSERT INTO t VALUES (4)").await;
        assert!(matches!(execute_query(&mut live, "CREATE TABLE actor (x int)", "s").await, ServerMessage::QueryError { .. }));
        assert!(live.is_alive().await);
        assert_eq!(live.transaction_state().await, Some(TxnState::Idle));

        // autocommit off: a write opens a transaction no BEGIN announced.
        run(&mut live, "SET autocommit = 0").await;
        run(&mut live, "INSERT INTO t VALUES (5)").await;
        assert_eq!(live.transaction_state().await, Some(TxnState::Active));
        run(&mut live, "ROLLBACK").await;
        run(&mut live, "SET autocommit = 1").await;
        assert_eq!(live.transaction_state().await, Some(TxnState::Idle));

        // Each column's type, as sqlx names the server's (the plane lines
        // the CLI's numeric ones up right).
        let types = match execute_query(&mut live, "SELECT NULL, 1, 'x', 1.5, CAST(NOW() AS DATE), CAST(1 AS UNSIGNED), \
            release_year, rental_rate, TRUE, CAST(1 AS DOUBLE), last_update, rating FROM film WHERE film_id = 1", "s").await {
            ServerMessage::QueryResult { types, .. } => types,
            other => panic!("{other:?}"),
        };
        assert_eq!(types, ["NULL", "BIGINT", "VARCHAR", "DECIMAL", "DATE", "BIGINT UNSIGNED", "YEAR", "DECIMAL",
                           "BIGINT", "DOUBLE", "TIMESTAMP", "ENUM"]);
    }

    #[test]
    fn test_editor_is_spawned() {
        assert!(needs_spawn(&ClientMessage::TestEditor { env_chain: vec![], line_flag_var: None }));
    }

    #[test]
    fn edit_externally_is_spawned() {
        // Waits for the user to close their editor — minutes, not ms.
        assert!(needs_spawn(&ClientMessage::EditExternally {
            session_id: "s".into(), connection_id: None, content: String::new(),
            line: None, tag: "t".into(), env_chain: vec![], line_flag_var: None,
        }));
    }

    fn origin(value: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = value {
            h.insert(header::ORIGIN, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn only_parliaments_own_pages_may_open_the_socket() {
        for ok in ALLOWED_ORIGINS {
            assert!(origin_is_allowed(&origin(Some(ok))), "{ok}");
        }
        assert!(origin_is_allowed(&origin(None)), "tools without an Origin");
        for bad in [
            "http://localhost:5173",     // another dev server on this machine
            "http://localhost",
            "https://localhost:3000",
            "http://[::1]:3000",
            "http://localhost:3000.evil.example",
            "http://evil.example",
            "null",
        ] {
            assert!(!origin_is_allowed(&origin(Some(bad))), "{bad}");
        }
    }
}
