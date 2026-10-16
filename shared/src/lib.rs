use serde::{Deserialize, Serialize};

pub mod settings;

/// What the server reported for a statement besides its rows — what the
/// native CLI prints for one that returns none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandStatus {
    /// Rows returned, or the rows affected when none came back.
    pub rows_affected: u64,
    /// Postgres: the command tag — `UPDATE 3`, `CREATE TABLE`,
    /// `ROLLBACK` — which psql prints.
    #[serde(default)] pub tag: Option<String>,
    /// MySQL: the warning count, and the info line — `Rows matched: 1
    /// Changed: 0  Warnings: 0` — which the mysql CLI prints.
    #[serde(default)] pub warnings: u16,
    #[serde(default)] pub info: Option<String>,
}

/// A session's transaction state, as the server reports it after each
/// command (Postgres's ReadyForQuery; libpq's `PQtransactionStatus`):
/// what the TXN badge and psql's `%x` prompt show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TxnState {
    #[default]
    Idle,
    /// In a transaction block.
    Active,
    /// In a failed transaction block: every command but its end is refused.
    Failed,
}

/// Which database engine a connection targets. Lowercase serde values so the
/// connections.json file stays human-readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DbType {
    #[default]
    #[serde(rename = "mysql")]
    MySql,
    #[serde(rename = "postgres")]
    Postgres,
}

/// Per-connection format style applied by the shell's right-click
/// "Format" menu. Default lives on the connection definition so the
/// menu's "Format with default (X)" item knows which formatter to run
/// when the user wants their preferred shape without picking it from
/// the explicit list. New styles can be added later without breaking
/// on-disk connections.json files because the field carries
/// `#[serde(default)]` and the enum has a Default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FormatStyle {
    /// Passthrough — picking this in the menu is a no-op. Default for
    /// connections that pre-date a format choice so legacy
    /// connections.json files round-trip unchanged.
    #[default]
    #[serde(rename = "unformatted")]
    Unformatted,
    /// "River" style: right-aligned clause keywords (SELECT/FROM/WHERE
    /// padded to col 6), one column per line in SELECT/GROUP BY lists,
    /// lowercase `and`/`or` in WHERE / HAVING.
    #[serde(rename = "river")]
    River,
    /// "Block" style: keywords left-aligned at column 0, each clause on
    /// its own line, content indented 4 spaces underneath. The most
    /// commonly used SQL formatting style — what `sqlfluff`,
    /// `pgFormatter`, DBeaver, DataGrip, sqlformat.org, and most SQL
    /// style guides prescribe. Default for newly-created connections.
    #[serde(rename = "block")]
    Block,
    /// "Inline" style: each statement collapsed to a single line.
    /// Multi-statement inputs join with `; ` so the whole batch
    /// becomes one line. Useful for pasting into log entries / chat
    /// messages / scripts where multi-line SQL is awkward. Works on
    /// any statement type (unlike River / Block which currently
    /// passthrough non-SELECT statements).
    #[serde(rename = "inline")]
    Inline,
}

/// How Noctua resolves the password for a PG connection at connect time.
/// Mirrors the two most common libpq credential sources; `Plaintext` is
/// the pre-existing behaviour and stays the default so existing
/// connections.json files deserialise unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    /// Password field on `ConnectionDef` is the source of truth. This is
    /// the historical Parliament behaviour — password stored plaintext in
    /// `~/.sqlclient/connections.json`.
    #[default]
    Plaintext,
    /// Noctua reads `~/.pgpass` (or `$PGPASSFILE`) at connect time and
    /// looks up the entry matching `(host, port, database, user)`. The
    /// on-disk password field stays empty. Requires the file to be
    /// `chmod 0600` on Unix; missing files, wrong permissions, or an
    /// unmatched entry all fail the connect with an explicit error
    /// (never silently falls back to a stored plaintext).
    Pgpass,
    // Reserved for a future v2 that adds pg_service.conf named-profile
    // resolution. Enum variant is intentionally not added yet — its
    // form UX and connect flow are meaningfully different (host / port
    // / user / db come from the service file too) so we ship this
    // shape without it and let the follow-on PR add the variant.
}

/// A saved connection definition.
/// Passwords stored plaintext — prototype only. Will move to a vault-backed
/// `CredentialProvider`.
///
/// Sidebar placement uses `paths`: each string is a slash-separated folder
/// (e.g. `"Production/MySQL"`) where this connection appears. An empty
/// `paths` puts the connection at the sidebar root. Multiple entries in
/// `paths` create **links** — the same connection appears in every listed
/// folder, sharing the same config; edits propagate to all locations.
///
/// Deserialization accepts the pre-v2 shape where placement lived in
/// `folder_path: Option<String>`. On load, a non-empty legacy `folder_path`
/// becomes `paths: vec![path]`; an absent / empty one becomes `paths: vec![]`.
/// The legacy `pinned: bool` (cosmetic-only sidebar pin icon) is accepted
/// on load and discarded — the replacement is the folder/link/Recents
/// sidebar model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "RawConnectionDef")]
pub struct ConnectionDef {
    pub id: String,
    pub name: String,
    pub db_type: DbType,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    /// Slash-separated folder paths where this connection appears in the
    /// sidebar. Empty = root. Multiple entries = linked (same config,
    /// multiple placements).
    pub paths: Vec<String>,
    /// Which pane opens by default: "shell" | "schema" | "activity"
    pub default_pane: String,
    /// Free-text environment tag. Known values ("prod", "staging", "dev",
    /// "test", case-insensitive) drive a colored left-stripe on the tab.
    pub env: Option<String>,
    /// Auto-format style applied to the shell input on this connection.
    pub format_style: FormatStyle,
    /// This connection's editor override for the `\e` / `\ef` /
    /// `\ev` meta-commands. Example values: `"code -w"`, `"mvim -f"`.
    /// Parsed via shlex (so quoted paths with spaces work). Empty /
    /// None uses Settings › Editor, which in turn defaults to the
    /// database CLI's environment variables (see noctua's
    /// `editor/resolve.rs`).
    ///
    /// GUI editors MUST be invoked with their wait flag so the
    /// command blocks until the user closes the file — noctua reports
    /// one that returns at once without changes.
    #[serde(default)]
    pub editor_command: Option<String>,
    /// Memory-cleanup threshold (megabytes) per shell pane opened
    /// from this connection. **Not** a pre-allocated buffer size —
    /// nothing is reserved up-front. Just a trigger value: after each
    /// query completes, if the pane's total shell-history bytes
    /// exceed this threshold, the oldest block (one `Query` entry
    /// plus its trailing output entries as a unit) gets evicted. One
    /// eviction per query check.
    ///
    /// `None` = no threshold = no cleanup (preserves pre-feature
    /// behavior for legacy `connections.json` files via
    /// `#[serde(default)]`).
    ///
    /// Soft threshold, not a hard ceiling — a single large result can
    /// momentarily exceed it; subsequent queries trim back down
    /// naturally as their checks fire. Each pane opened from this
    /// connection carries its own independent shell-history and
    /// applies the threshold independently — two shells on the same
    /// connection do NOT share buffer state.
    #[serde(default)]
    pub buffer_threshold_mb: Option<u32>,
    /// Default schema this connection opens to. PG only — MySQL doesn't
    /// distinguish schema from database. `None` (or an empty string after
    /// the From-impl normalization) means "no preference; explorer picks
    /// the first non-system schema." Populated by the connection form's
    /// schema dropdown after the user hits ↻ to load the database's
    /// schemas.
    #[serde(default)]
    pub default_schema: Option<String>,
    /// Where the password comes from at connect time. Defaults to
    /// `Plaintext` (the pre-existing behaviour of using the on-disk
    /// `password` field) so existing connections.json files keep
    /// working. PG only — MySQL always uses `Plaintext`.
    #[serde(default)]
    pub credential_source: CredentialSource,
}

/// Wire-format mirror of [`ConnectionDef`] used only during deserialization.
/// Accepts both the v2 shape (`paths: Vec<String>`) and the v1 legacy
/// (`folder_path: Option<String>`, `pinned: bool`) — the `From` impl below
/// normalizes them into the canonical struct.
#[derive(Debug, Clone, Deserialize)]
struct RawConnectionDef {
    id: String,
    name: String,
    #[serde(default)]
    db_type: DbType,
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
    #[serde(default)]
    paths: Vec<String>,
    /// Legacy single-string folder. Migrated into `paths` when present.
    #[serde(default)]
    folder_path: Option<String>,
    /// Legacy cosmetic pin flag. Accepted on load and silently discarded.
    #[serde(default)]
    #[allow(dead_code)]
    pinned: bool,
    #[serde(default = "default_pane_shell")]
    default_pane: String,
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    format_style: FormatStyle,
    /// Editor override. See [`ConnectionDef::editor_command`].
    /// Default-None deserialization keeps existing connections.json
    /// files (pre-feature) reading cleanly as "no override."
    #[serde(default)]
    editor_command: Option<String>,
    /// Soft buffer threshold per shell pane. See doc on
    /// [`ConnectionDef::buffer_threshold_mb`]. Default-None deserialization
    /// keeps existing connections.json files (pre-feature) reading
    /// cleanly as "unlimited."
    #[serde(default)]
    buffer_threshold_mb: Option<u32>,
    /// Default schema (PG-only). See [`ConnectionDef::default_schema`].
    #[serde(default)]
    default_schema: Option<String>,
    /// Where the password comes from. See [`ConnectionDef::credential_source`].
    #[serde(default)]
    credential_source: CredentialSource,
}

impl From<RawConnectionDef> for ConnectionDef {
    fn from(r: RawConnectionDef) -> Self {
        let paths = if !r.paths.is_empty() {
            r.paths
        } else {
            r.folder_path
                .filter(|s| !s.is_empty())
                .map(|s| vec![s])
                .unwrap_or_default()
        };
        ConnectionDef {
            id: r.id,
            name: r.name,
            db_type: r.db_type,
            host: r.host,
            port: r.port,
            user: r.user,
            password: r.password,
            database: r.database,
            paths,
            default_pane: r.default_pane,
            env: r.env,
            format_style: r.format_style,
            editor_command: r.editor_command,
            buffer_threshold_mb: r.buffer_threshold_mb,
            default_schema: r.default_schema.filter(|s| !s.is_empty()),
            credential_source: r.credential_source,
        }
    }
}

fn default_pane_shell() -> String { "shell".to_string() }

/// Browser-visible view of a saved connection. Identical to
/// [`ConnectionDef`] except for the omission of `password`.
///
/// **Why this exists.** Parliament's defining security premise is that
/// the browser holds no database credentials — Noctua holds them, the
/// browser holds none. That isolation is the prerequisite for the
/// governed-access roadmap (PAM, audit logging, masking). It only
/// works if the wire actually doesn't carry credentials.
///
/// `ConnectionDefPublic` is the type the frontend sees: every
/// `ServerMessage::ConnectionList` / `ConnectionSaved` ships these,
/// and the frontend's `connections` signal is `Vec<ConnectionDefPublic>`.
/// The full [`ConnectionDef`] stays on the disk + inside Noctua's
/// in-memory state, and is rebuilt at the noctua boundary from a
/// `ConnectionDefPublic` + the saved password (or a fresh
/// `password_change` payload) when the frontend saves.
///
/// Adding a field here? Add it to [`ConnectionDef`] too and update
/// the `From<&ConnectionDef>` impl below. The two shapes must stay
/// in lockstep on every non-password field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionDefPublic {
    pub id: String,
    pub name: String,
    pub db_type: DbType,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub paths: Vec<String>,
    pub default_pane: String,
    pub env: Option<String>,
    pub format_style: FormatStyle,
    /// See [`ConnectionDef::editor_command`]. Sent so the connection
    /// form can show and edit it; when opening the editor noctua reads
    /// its own copy by connection id — the browser never sends it back.
    #[serde(default)]
    pub editor_command: Option<String>,
    /// See [`ConnectionDef::buffer_threshold_mb`]. Threaded down to the
    /// frontend so the shell pane's eviction logic can read it live
    /// from the connections signal.
    #[serde(default)]
    pub buffer_threshold_mb: Option<u32>,
    /// See [`ConnectionDef::default_schema`]. Threaded down so the
    /// explorer pane can seed its initial schema selection on session
    /// open without re-reading the saved connection file.
    #[serde(default)]
    pub default_schema: Option<String>,
    /// See [`ConnectionDef::credential_source`]. Threaded down so the
    /// connection form re-opens on Edit with the correct auth mode
    /// selected (Password vs `~/.pgpass`) and the password `<input>`
    /// stays hidden when Pgpass is active.
    #[serde(default)]
    pub credential_source: CredentialSource,
}

impl ConnectionDefPublic {
    /// Build a wire-shape [`ConnectionDef`] from this public view + a
    /// password. Used by tyto when firing `ListDatabases` /
    /// `TestConnection` against a saved connection without having
    /// the password in browser memory: pass an empty `password` and
    /// noctua substitutes the saved password by id. Form flows where
    /// the user has typed a password pass that string directly.
    pub fn with_password(self, password: String) -> ConnectionDef {
        ConnectionDef {
            id:                  self.id,
            name:                self.name,
            db_type:             self.db_type,
            host:                self.host,
            port:                self.port,
            user:                self.user,
            password,
            database:            self.database,
            paths:               self.paths,
            default_pane:        self.default_pane,
            env:                 self.env,
            format_style:        self.format_style,
            editor_command:      self.editor_command,
            buffer_threshold_mb: self.buffer_threshold_mb,
            default_schema:      self.default_schema,
            credential_source:   self.credential_source,
        }
    }
}

impl From<&ConnectionDef> for ConnectionDefPublic {
    fn from(c: &ConnectionDef) -> Self {
        ConnectionDefPublic {
            id:                  c.id.clone(),
            name:                c.name.clone(),
            db_type:             c.db_type,
            host:                c.host.clone(),
            port:                c.port,
            user:                c.user.clone(),
            database:            c.database.clone(),
            paths:               c.paths.clone(),
            default_pane:        c.default_pane.clone(),
            env:                 c.env.clone(),
            format_style:        c.format_style,
            editor_command:      c.editor_command.clone(),
            buffer_threshold_mb: c.buffer_threshold_mb,
            default_schema:      c.default_schema.clone(),
            credential_source:   c.credential_source,
        }
    }
}

/// On-disk shape of `~/.sqlclient/connections.json`. v1 was a bare array
/// of [`ConnectionDef`]; v2 wraps that in an object so empty folders can
/// persist alongside connections. noctua's `parse_connections_file`
/// reads both.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionFile {
    #[serde(default = "default_file_version")]
    pub version: u32,
    /// Slash-separated folder paths the user has explicitly created. Lets
    /// an empty folder persist even when no connection lives inside it.
    #[serde(default)]
    pub folders: Vec<String>,
    pub connections: Vec<ConnectionDef>,
}

/// The connections file's schema version this build writes; a file with
/// a higher one came from a newer Parliament, and isn't saved over.
pub const CONNECTIONS_FILE_VERSION: u32 = 2;

fn default_file_version() -> u32 { CONNECTIONS_FILE_VERSION }

/// An environment variable a database CLI reads to pick the editor for
/// `\e`. A closed set: the browser says which of these its plane's CLI
/// follows, and noctua reads them — the browser can't name any other
/// variable, or a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EditorVar {
    PsqlEditor,
    Editor,
    Visual,
}

impl EditorVar {
    pub fn name(self) -> &'static str {
        match self {
            EditorVar::PsqlEditor => "PSQL_EDITOR",
            EditorVar::Editor     => "EDITOR",
            EditorVar::Visual     => "VISUAL",
        }
    }
}

/// An environment variable holding a CLI's "open at line N" flag for
/// editors picked from the environment (psql's
/// `PSQL_EDITOR_LINENUMBER_ARG`). Closed set, like [`EditorVar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LineFlagVar {
    PsqlEditorLinenumberArg,
}

impl LineFlagVar {
    pub fn name(self) -> &'static str {
        match self {
            LineFlagVar::PsqlEditorLinenumberArg => "PSQL_EDITOR_LINENUMBER_ARG",
        }
    }
}

/// An error as the database server reported it: a Postgres
/// ErrorResponse's fields (what psql shows by `VERBOSITY` and
/// `\errverbose`), or a MySQL error's number and SQLSTATE. `message` is
/// the primary message alone, without any driver prefix.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DbError {
    /// `ERROR`, `FATAL`, `PANIC` (Postgres).
    pub severity:          Option<String>,
    pub sqlstate:          Option<String>,
    /// MySQL's error number (1146 …).
    pub number:            Option<u16>,
    pub message:           String,
    pub detail:            Option<String>,
    pub hint:              Option<String>,
    /// 1-based character position in the statement sent.
    pub position:          Option<u32>,
    /// 1-based position in `internal_query` (a query run inside a
    /// function).
    pub internal_position: Option<u32>,
    pub internal_query:    Option<String>,
    /// Postgres's CONTEXT (where in a call stack it happened).
    pub context:           Option<String>,
    pub schema:            Option<String>,
    pub table:             Option<String>,
    pub column:            Option<String>,
    pub datatype:          Option<String>,
    pub constraint:        Option<String>,
    /// Where in the server's source the error was raised.
    pub file:              Option<String>,
    pub line:              Option<u32>,
    pub routine:           Option<String>,
}

impl DbError {
    /// An error known only by its message (no server fields): a lost
    /// connection, a refused request.
    pub fn from_message(message: impl Into<String>) -> DbError {
        DbError { message: message.into(), ..DbError::default() }
    }
}

/// The rows one query returned. `rows[i][j]` is `None` for SQL NULL.
/// `columns` is empty when no rows came back (the driver only reports
/// column names alongside rows).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows:    Vec<Vec<Option<String>>>,
}

/// What an `IntrospectBatch` produced: a result set for each query that
/// completed, in order, and the error that stopped the rest, if any —
/// the server's fields when the server raised it, else just a message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BatchResult {
    pub sets:  Vec<ResultSet>,
    pub error: Option<DbError>,
}

/// Messages sent from the browser to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    // Connection management
    ListConnections,
    /// App-wide preferences (`settings::AppSettings`). Answered with
    /// `ServerMessage::Settings`.
    GetSettings,
    /// Replace the app-wide preferences. Answered with `SettingsSaved` or
    /// `SettingsError`.
    SaveSettings { settings: settings::AppSettings },
    /// Open the *saved* Settings › Editor choice on a short sample, at
    /// line 2, and report how it went (`EditorTestResult`). Carries no
    /// command, like `EditExternally`; `env_chain` / `line_flag_var`
    /// stand in for a plane's when the choice is "Environment".
    TestEditor {
        #[serde(default)]
        env_chain: Vec<EditorVar>,
        #[serde(default)]
        line_flag_var: Option<LineFlagVar>,
    },
    /// Save (insert or replace) a connection. `def` carries every field
    /// **except** the password, because the frontend never holds the
    /// password for an already-saved connection (see
    /// [`ConnectionDefPublic`]). The password to persist is resolved
    /// from two fields, evaluated in this order:
    ///
    /// 1. `password_change: Some(s)` — set the password to `s`. Used
    ///    when the user typed a new value in the New / Edit form.
    /// 2. `copy_from: Some(src_id)` — copy the password from the saved
    ///    connection whose id is `src_id`. Used by the sidebar's
    ///    "Copy" verb, which mints a fresh id for the duplicate but
    ///    should inherit the source's credentials. Without this, the
    ///    Copy path would persist an empty password since the new id
    ///    has no on-disk record to fall through to.
    /// 3. Both `None` — keep the password noctua already has on disk
    ///    for this `def.id`. If no such record exists (a truly new
    ///    connection with no typed password), persist empty.
    SaveConnection {
        def: ConnectionDefPublic,
        #[serde(default)] password_change: Option<String>,
        #[serde(default)] copy_from: Option<String>,
    },
    DeleteConnection { id: String },
    /// Replace the persisted list of explicit (empty-permitted) folders.
    /// The frontend sends the full list on every mutation; backend writes
    /// it to `connections.json`.
    SaveFolders { folders: Vec<String> },
    // Session lifecycle
    /// `client_tab_id` is a browser-generated UUID echoed back in SessionOpened/SessionError
    /// so the frontend can route the response to the right tab.
    /// `client_role` identifies which subsystem is opening the connection
    /// (e.g. `"SQL Shell"`, `"Schema Explorer"`, `"Activity Monitor"`).
    /// Noctua uses it to set PG's `application_name` to `"Parliament - <role>"`
    /// so DBAs can identify Parliament sessions in `pg_stat_activity`.
    /// `#[serde(default)]` keeps older frontends compatible — a missing
    /// role just means the connection shows up as `"Parliament"` alone.
    /// `timezone`: the zone this browser asks a Postgres session for (its
    /// own Settings choice), above `PGTZ` in noctua's environment; None
    /// leaves it to `PGTZ`, else the server, as psql does.
    Connect {
        connection_id: String,
        client_tab_id: String,
        #[serde(default)] database:    Option<String>,
        #[serde(default)] client_role: Option<String>,
        #[serde(default)] timezone:    Option<String>,
    },
    Disconnect { session_id: String },
    // Connection testing + database listing (ephemeral — no session created)
    TestConnection { def: ConnectionDef },
    /// `client_tab_id` is `Some(pane_id)` when the call comes from a Schema
    /// Explorer pane's database dropdown — the response is routed back to
    /// that specific pane. `None` for the New / Edit Connection form,
    /// whose database-list dropdown is a singleton at the App level.
    /// `#[serde(default)]` so older clients that don't include the field
    /// still deserialize cleanly.
    ListDatabases { def: ConnectionDef, #[serde(default)] client_tab_id: Option<String> },
    /// Hot-load the list of schemas inside `def.database`. PG only;
    /// MySQL has no schema-vs-database distinction so noctua returns
    /// `SchemaListError` with a "PG only" message there. Same blank-
    /// password sentinel handling as `ListDatabases`: when `def.id` is
    /// set and `def.password` is empty, noctua substitutes the saved
    /// password.
    ListSchemas { def: ConnectionDef, #[serde(default)] client_tab_id: Option<String> },
    // Query execution
    ExecuteQuery { session_id: String, sql: String },
    /// Describe the columns the buffer would produce, without
    /// executing it. Drives psql's `\gdesc`. The backend runs the
    /// driver's `describe()` (sqlx exposes it on both PG and MySQL
    /// connections) and returns the column-name + type-name pairs.
    /// The query is parsed and planned but not executed: no rows
    /// are scanned, no triggers fire, and VOLATILE functions are
    /// deferred. PG's planner MAY evaluate IMMUTABLE / STABLE
    /// functions during planning, so callers shouldn't treat this
    /// as side-effect-free for those classes.
    DescribeQuery { session_id: String, sql: String },
    // Schema introspection (results never go to shell history)
    IntrospectQuery { session_id: String, sql: String, tag: String },
    /// Run `queries` in order on the session in one round trip — what a
    /// psql meta-command that issues several catalog queries needs
    /// (`\d <table>`). The reply (`IntrospectBatchResult`) carries one
    /// result set per query, SQL NULL kept distinct from text. Stops at
    /// the first error, as psql does. Each query is one statement.
    IntrospectBatch { session_id: String, tag: String, queries: Vec<String> },
    // Out-of-band DML for the Data tab's inline cell editor — runs an
    // UPDATE / INSERT / DELETE and returns rows_affected without
    // landing in the shell. `tag` lets the frontend route the response
    // to the originating cell.
    ExecuteDml { session_id: String, sql: String, tag: String },
    /// Round-trip `content` through an external editor on noctua's
    /// host and return the edited text (`EditExternallyResult`). Used
    /// by the shell's `\e` / `\ef` / `\ev` meta-commands.
    ///
    /// The message never carries a command: noctua picks the editor
    /// from its own files — the connection's editor override (looked
    /// up by `connection_id`), then Settings › Editor, then the
    /// variables in `env_chain`, then `vi` on macOS. `env_chain` and
    /// `line_flag_var` come from the plane, so PG follows psql
    /// (`PSQL_EDITOR` → `EDITOR` → `VISUAL`) and MySQL the mysql CLI.
    /// `line` opens the editor at that line when it supports one.
    /// `tag` routes the response, like `ExecuteDml`'s.
    ///
    /// Clients from before the rework sent `editor_command`; it is
    /// ignored (unknown fields are skipped), so they still parse.
    EditExternally {
        session_id: String,
        #[serde(default)]
        connection_id: Option<String>,
        content: String,
        #[serde(default)]
        line: Option<u32>,
        tag: String,
        #[serde(default)]
        env_chain: Vec<EditorVar>,
        #[serde(default)]
        line_flag_var: Option<LineFlagVar>,
    },
}

/// A result column as the server describes a statement's (psql's
/// `\gdesc`): its name, its type as the server identifies it (Postgres:
/// the type OID; MySQL: sqlx's type name) and its type modifier
/// (Postgres's `atttypmod`, -1 for none: `varchar(30)`'s length).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribedColumn {
    pub name:          String,
    pub type_id:       String,
    pub type_modifier: i32,
}

/// A locale's numeric conventions as the C library's `localeconv()`
/// gives them, unchanged: the decimal point, the thousands separator
/// (either may be empty) and the first group's size (`grouping[0]`; 0
/// when the locale gives none). The empty default is the C locale's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumericLocale {
    pub decimal_point: String,
    pub thousands_sep: String,
    pub grouping:      i32,
}

/// Messages sent from the backend to the browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    // Connection management
    ConnectionList {
        /// Public-shape view — no password. The browser never sees
        /// stored credentials, by design (see [`ConnectionDefPublic`]).
        connections: Vec<ConnectionDefPublic>,
        /// Explicit empty-permitted folders. `#[serde(default)]` so an
        /// older backend can still talk to a newer frontend without
        /// breaking the message shape.
        #[serde(default)]
        folders: Vec<String>,
        /// Why `~/.sqlclient/connections.json` couldn't be used as it is —
        /// unreadable, or a newer Parliament's — for the sidebar to say.
        #[serde(default)]
        notice: Option<String>,
    },
    /// What the sidebar should say about the connections file now: sent
    /// when a save moved an unreadable file aside, or was refused.
    ConnectionsNotice { notice: Option<String> },
    /// Echoed after a successful `SaveConnection`. The frontend uses
    /// this to update its `connections` signal. Password omitted — see
    /// [`ConnectionDefPublic`].
    ConnectionSaved { def: ConnectionDefPublic },
    ConnectionDeleted { id: String },
    /// Acknowledgement that the explicit-folders list has been persisted.
    /// Frontend uses this to confirm the round-trip; folders themselves
    /// are echoed back so the client can resync if it desyncs.
    FoldersSaved { folders: Vec<String> },
    // Session lifecycle
    SessionOpened {
        session_id:          String,
        client_tab_id:       String,
        connection_id:       String,
        server_version:      String,
        database:            String,
        #[serde(default)] db_connection_id: String,
        /// Names of installed extensions on this connection. PG only
        /// (MySQL sends an empty vec). Populated once at session open
        /// and cached on the frontend's ShellTab. Plane methods that
        /// need to gate SQL on extension availability (e.g. `table_vm_sql`
        /// checking for `pg_visibility`) consult this list instead of
        /// running a per-query capability probe. `#[serde(default)]`
        /// keeps the on-the-wire format backward-compatible with
        /// older tyto/noctua pairs.
        #[serde(default)] extensions: Vec<String>,
        /// What the client knows of the session that the server doesn't
        /// say, for psql's `\conninfo` (PG): whether the server asked
        /// for a password (libpq's `PQconnectionUsedPassword`), and the
        /// TLS library noctua's sessions use (`PQsslAttribute(conn,
        /// "library")`).
        #[serde(default)] password_used: bool,
        #[serde(default)] ssl_library: String,
        /// How the client's locale writes numbers (noctua's environment,
        /// as psql's `setlocale(LC_ALL, "")` reads its own), for psql's
        /// `\pset numericlocale`.
        #[serde(default)] numeric_locale: NumericLocale,
    },
    /// `error`: the server's fields when it refused the connection
    /// (`database "x" does not exist`), for the CLI's wording of it.
    SessionError  {
        client_tab_id: String,
        connection_id: String,
        message: String,
        #[serde(default)] error: Option<DbError>,
    },
    SessionClosed { session_id: String },
    // Query results — session_id routes the response to the correct tab
    /// `columns` are there whenever the statement returns rows, even
    /// none (Postgres; MySQL only with a row), and `types` with them:
    /// each column's type as the server identifies it — Postgres's type
    /// OID (psql lines numbers up by it), MySQL's column type. `status`:
    /// the rest of what the server said, which the CLI prints for a
    /// statement without rows. `rows` keeps SQL NULL as `None`, apart
    /// from a text 'NULL'.
    QueryResult {
        session_id: String,
        columns: Vec<String>,
        #[serde(default)] types: Vec<String>,
        rows: Vec<Vec<Option<String>>>,
        exec_time_us: u64,
        #[serde(default)] status: CommandStatus,
    },
    /// `message` is the error's text (the primary message for a server
    /// error); `error` carries the server's fields when it was one.
    /// `connection_lost`: the session died during the statement (a
    /// `SessionClosed` follows) — where the mysql CLI reconnects and runs
    /// it again.
    QueryError {
        session_id: String,
        message: String,
        #[serde(default)] error: Option<DbError>,
        #[serde(default)] connection_lost: bool,
    },
    /// App-wide preferences. `load_error` is set when `settings.json`
    /// exists but couldn't be read; defaults are in use and the next
    /// save moves the unreadable file aside rather than losing it.
    Settings { settings: settings::AppSettings, load_error: Option<String> },
    SettingsSaved { settings: settings::AppSettings },
    SettingsError { message: String },
    /// Outcome of `TestEditor`: which editor ran and what came back, or
    /// why it couldn't run.
    EditorTestResult { ok: bool, message: String },
    /// A server notice (NOTICE / WARNING / INFO …) raised while a shell
    /// query runs — `RAISE NOTICE`, `VACUUM VERBOSE` progress,
    /// "table does not exist, skipping", "there is no transaction in
    /// progress". Streamed as it arrives, so it always precedes the
    /// query's QueryResult / QueryError on the wire, matching psql.
    /// `severity` is the label psql prints (`"NOTICE"`, `"WARNING"`, …).
    Notice { session_id: String, severity: String, message: String },
    /// The session's transaction state after a command on it (Postgres),
    /// sent just before that command's reply.
    TransactionStatus { session_id: String, state: TxnState },
    /// Result of `DescribeQuery` — the statement's result columns as
    /// the server describes them, without running it (none: it returns
    /// no rows), and how long that took. psql's `\gdesc` then asks the
    /// server for its types' names.
    DescribeResult { session_id: String, columns: Vec<DescribedColumn>, #[serde(default)] exec_time_us: u64 },
    /// Error path for `DescribeQuery` — parse / plan failures (the
    /// driver still sees the SQL even though it doesn't execute it).
    DescribeError { session_id: String, message: String, #[serde(default)] error: Option<DbError> },
    // Schema introspection responses
    IntrospectResult { session_id: String, tag: String, columns: Vec<String>, rows: Vec<Vec<String>> },
    IntrospectError  { session_id: String, tag: String, message: String, #[serde(default)] error: Option<DbError> },
    /// Reply to `IntrospectBatch`. `connection_lost`: as on `QueryError`
    /// — a `\d` that found its session dead resets it as psql does.
    IntrospectBatchResult {
        session_id: String,
        tag: String,
        result: BatchResult,
        #[serde(default)] connection_lost: bool,
    },
    // DML responses for the Data tab editor.
    DmlResult { session_id: String, tag: String, rows_affected: u64 },
    DmlError  { session_id: String, tag: String, message: String },
    // External-editor round-trip responses. Routed by `tag` (set by
    // the originating ClientMessage::EditExternally) so the
    // frontend handler can apply the result to the right surface —
    // textarea (`\e`), function source (`\ef`), view source (`\ev`).
    EditExternallyResult { session_id: String, tag: String, content: String },
    EditExternallyError  { session_id: String, tag: String, message: String },
    /// The editor exited with an error or its window was closed: the
    /// edit is discarded (psql does the same) and the input is left
    /// as it was. `message` says why.
    EditExternallyCancelled { session_id: String, tag: String, message: String },
    // Connection test + database list responses
    TestConnectionResult { success: bool, message: String },
    /// `client_tab_id` echoes the value the originating `ListDatabases`
    /// request carried, so the frontend can route the response back to
    /// the specific Schema Explorer pane that asked. `None` when the
    /// request came from the singleton connection-form dropdown.
    DatabaseList { databases: Vec<String>, #[serde(default)] client_tab_id: Option<String> },
    /// Error path for `ListDatabases` — replaces the previous practice
    /// of overloading `TestConnectionResult { success: false, .. }` on
    /// failure, which routed the error only to the connection-form
    /// modal (where it was invisible if the request came from the
    /// Schema Explorer dropdown). `client_tab_id` routes the error to
    /// the originating pane (or to the form if `None`).
    DatabaseListError { message: String, #[serde(default)] client_tab_id: Option<String> },
    /// Success response to `ListSchemas`. `client_tab_id` echoes the
    /// originating request so the frontend can route the response
    /// (None = the connection form; Some(pid) reserved for a future
    /// per-pane schema dropdown).
    SchemaList { schemas: Vec<String>, #[serde(default)] client_tab_id: Option<String> },
    /// Error path for `ListSchemas` — same routing semantics as
    /// `DatabaseListError`. Reported when the database can't be reached,
    /// when the connection is MySQL (no schema-vs-database distinction),
    /// or when `def.database` is empty.
    SchemaListError { message: String, #[serde(default)] client_tab_id: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_def() -> ConnectionDef {
        ConnectionDef {
            id: "abc".into(),
            name: "prod-mysql".into(),
            db_type: DbType::MySql,
            host: "db.example.com".into(),
            port: 3306,
            user: "root".into(),
            password: "hunter2".into(),
            database: "app".into(),
            paths: vec!["Production/MySQL".into()],
            default_pane: "shell".into(),
            env: Some("prod".into()),
            format_style: FormatStyle::Block,
            editor_command: Some("code -w".into()),
            buffer_threshold_mb: Some(50),
            default_schema: None,
            credential_source: CredentialSource::Plaintext,
        }
    }

    /// The defining invariant: a `ConnectionDefPublic` serialized to
    /// JSON must contain no `password` field — neither key nor value.
    /// If this test fails, the browser will be receiving credentials
    /// every time noctua serializes a saved connection, which
    /// violates the architecture's browser-isolation premise (see
    /// issue #6 / vault/Engineering/Audit Findings 2026-06.md Lens 5).
    #[test]
    fn public_def_drops_password_on_the_wire() {
        let pubdef = ConnectionDefPublic::from(&sample_def());
        let json   = serde_json::to_string(&pubdef).unwrap();
        assert!(
            !json.contains("password"),
            "ConnectionDefPublic JSON must not contain `password`: {}",
            json,
        );
        assert!(
            !json.contains("hunter2"),
            "ConnectionDefPublic JSON must not contain the password value: {}",
            json,
        );
    }

    /// The wire shape must round-trip every non-password field — the
    /// frontend depends on this for displaying connection state in the
    /// sidebar, the tab tooltip, and the Edit modal's non-password
    /// inputs. A missing-on-the-wire field would silently zero out
    /// after one round-trip.
    #[test]
    fn public_def_round_trips_every_non_password_field() {
        let original = sample_def();
        let pubdef   = ConnectionDefPublic::from(&original);
        let json     = serde_json::to_string(&pubdef).unwrap();
        let back: ConnectionDefPublic = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id,           original.id);
        assert_eq!(back.name,         original.name);
        assert_eq!(back.db_type,      original.db_type);
        assert_eq!(back.host,         original.host);
        assert_eq!(back.port,         original.port);
        assert_eq!(back.user,         original.user);
        assert_eq!(back.database,     original.database);
        assert_eq!(back.paths,        original.paths);
        assert_eq!(back.default_pane, original.default_pane);
        assert_eq!(back.env,          original.env);
        assert_eq!(back.format_style, original.format_style);
    }

    /// The full `ConnectionDef` must still round-trip its password —
    /// it's the on-disk format for `~/.sqlclient/connections.json`,
    /// and breaking that loses every saved credential on the user's
    /// next launch.
    #[test]
    fn full_def_round_trips_password_for_on_disk_storage() {
        let original = sample_def();
        let json     = serde_json::to_string(&original).unwrap();
        assert!(json.contains("hunter2"), "ConnectionDef storage form must include password");
        let back: ConnectionDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back.password, original.password);
    }

    /// `SaveConnection`'s `password_change: None` must round-trip
    /// faithfully — it's the sentinel noctua reads to mean "preserve
    /// the existing password on disk." If serde silently dropped it
    /// or replaced it with `Some("")`, every Edit-without-password-
    /// change would zero out the saved password.
    #[test]
    fn save_connection_password_change_none_round_trips() {
        let msg = ClientMessage::SaveConnection {
            def: ConnectionDefPublic::from(&sample_def()),
            password_change: None,
            copy_from: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        match back {
            ClientMessage::SaveConnection { password_change, copy_from, .. } => {
                assert!(password_change.is_none(), "None sentinel must round-trip");
                assert!(copy_from.is_none(),       "Absent copy_from must round-trip");
            }
            _ => panic!("expected SaveConnection, got {:?}", back),
        }
    }

    /// The sidebar's "Copy" verb mints a fresh UUID for the duplicate
    /// connection and sends `copy_from: Some(<original_id>)`. Noctua
    /// then resolves the password by looking up the original. Without
    /// the copy_from path, every Copy would silently persist an empty
    /// password (the new id has no on-disk record to fall through to).
    #[test]
    fn save_connection_copy_from_round_trips() {
        let msg = ClientMessage::SaveConnection {
            def: ConnectionDefPublic::from(&sample_def()),
            password_change: None,
            copy_from: Some("source-id-abc".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        match back {
            ClientMessage::SaveConnection { copy_from, .. } => {
                assert_eq!(copy_from.as_deref(), Some("source-id-abc"));
            }
            _ => panic!("expected SaveConnection, got {:?}", back),
        }
    }

    #[test]
    fn notice_round_trips() {
        let msg = ServerMessage::Notice {
            session_id: "sess-1".into(),
            severity:   "NOTICE".into(),
            message:    "table \"x\" does not exist, skipping".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"Notice\""), "{json}");
        match serde_json::from_str::<ServerMessage>(&json).unwrap() {
            ServerMessage::Notice { session_id, severity, message } => {
                assert_eq!(session_id, "sess-1");
                assert_eq!(severity, "NOTICE");
                assert!(message.contains("skipping"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn connections_notice_round_trips_and_defaults_to_none() {
        let msg = ServerMessage::ConnectionsNotice { notice: Some("kept as connections.json.broken-1".into()) };
        let json = serde_json::to_string(&msg).unwrap();
        match serde_json::from_str::<ServerMessage>(&json).unwrap() {
            ServerMessage::ConnectionsNotice { notice } => assert_eq!(notice.as_deref(), Some("kept as connections.json.broken-1")),
            other => panic!("wrong variant: {other:?}"),
        }
        // A ConnectionList without the field — an older noctua's — has none.
        let old = r#"{"type":"ConnectionList","data":{"connections":[],"folders":[]}}"#;
        match serde_json::from_str::<ServerMessage>(old).unwrap() {
            ServerMessage::ConnectionList { notice, .. } => assert_eq!(notice, None),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn transaction_status_round_trips() {
        let msg = ServerMessage::TransactionStatus { session_id: "s1".into(), state: TxnState::Failed };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"TransactionStatus\""), "{json}");
        match serde_json::from_str::<ServerMessage>(&json).unwrap() {
            ServerMessage::TransactionStatus { session_id, state } => {
                assert_eq!(session_id, "s1");
                assert_eq!(state, TxnState::Failed);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn introspect_batch_round_trips() {
        let msg = ClientMessage::IntrospectBatch {
            session_id: "s1".into(),
            tag:        "meta:7".into(),
            queries:    vec!["SELECT 1".into(), "SELECT NULL".into()],
        };
        let back: ClientMessage = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert!(matches!(back, ClientMessage::IntrospectBatch { ref queries, .. } if queries.len() == 2), "{back:?}");

        let result = BatchResult {
            sets:  vec![ResultSet { columns: vec!["a".into()], rows: vec![vec![Some("NULL".into())], vec![None]] }],
            error: Some(DbError { sqlstate: Some("22012".into()), message: "division by zero".into(), ..DbError::default() }),
        };
        let msg = ServerMessage::IntrospectBatchResult {
            session_id: "s1".into(), tag: "meta:7".into(), result: result.clone(), connection_lost: false,
        };
        match serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap() {
            // The text "NULL" and SQL NULL stay apart.
            ServerMessage::IntrospectBatchResult { result: back, .. } => assert_eq!(back, result),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    // ── \gdesc DescribeQuery / Result / Error round-trips ─────────
    //
    // The DescribeQuery protocol is new and unverified outside
    // type-checking; pin the wire shape so a future refactor to the
    // ClientMessage / ServerMessage enums can't silently change the
    // serialized field names. Frontends and backends compiled at
    // different versions must agree on the JSON.

    #[test]
    fn describe_query_round_trips() {
        let msg = ClientMessage::DescribeQuery {
            session_id: "sess-1".into(),
            sql:        "SELECT 1, 'x'::text".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        match back {
            ClientMessage::DescribeQuery { session_id, sql } => {
                assert_eq!(session_id, "sess-1");
                assert_eq!(sql, "SELECT 1, 'x'::text");
            }
            _ => panic!("expected DescribeQuery, got {:?}", back),
        }
    }

    #[test]
    fn describe_result_round_trips() {
        let column = |name: &str, type_id: &str, type_modifier| DescribedColumn {
            name: name.into(), type_id: type_id.into(), type_modifier,
        };
        let msg = ServerMessage::DescribeResult {
            session_id:   "sess-1".into(),
            columns:      vec![column("?column?", "23", -1), column("v", "1043", 34)],
            exec_time_us: 120,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        match back {
            ServerMessage::DescribeResult { session_id, columns, exec_time_us } => {
                assert_eq!(session_id, "sess-1");
                assert_eq!(columns, [column("?column?", "23", -1), column("v", "1043", 34)]);
                assert_eq!(exec_time_us, 120);
            }
            _ => panic!("expected DescribeResult, got {:?}", back),
        }
    }

    #[test]
    fn query_result_carries_column_types() {
        let msg = ServerMessage::QueryResult {
            session_id:   "sess-1".into(),
            columns:      vec!["n".into(), "t".into()],
            types:        vec!["23".into(), "25".into()],
            rows:         vec![vec![Some("1".into()), None]],
            exec_time_us: 5,
            status:       CommandStatus::default(),
        };
        let back: ServerMessage = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert!(matches!(back, ServerMessage::QueryResult { ref types, .. } if types == &["23", "25"]), "{back:?}");
        // From a noctua that sends none: no types.
        let old = r#"{"type":"QueryResult","data":{"session_id":"s","columns":["n"],"rows":[["1"]],"exec_time_us":1}}"#;
        let back: ServerMessage = serde_json::from_str(old).unwrap();
        assert!(matches!(back, ServerMessage::QueryResult { ref types, .. } if types.is_empty()), "{back:?}");
    }

    #[test]
    fn describe_error_round_trips() {
        let error = DbError {
            severity: Some("ERROR".into()),
            sqlstate: Some("42601".into()),
            message:  "syntax error at or near \"frmo\"".into(),
            position: Some(10),
            ..DbError::default()
        };
        let msg = ServerMessage::DescribeError {
            session_id: "sess-1".into(),
            message:    error.message.clone(),
            error:      Some(error.clone()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        match back {
            ServerMessage::DescribeError { session_id, message, error: back_error } => {
                assert_eq!(session_id, "sess-1");
                assert_eq!(message, "syntax error at or near \"frmo\"");
                assert_eq!(back_error, Some(error));
            }
            _ => panic!("expected DescribeError, got {:?}", back),
        }
        // A QueryError from an older noctua (no `error`) still reads.
        let old = r#"{"type":"QueryError","data":{"session_id":"s","message":"boom"}}"#;
        assert!(matches!(serde_json::from_str::<ServerMessage>(old).unwrap(),
                         ServerMessage::QueryError { error: None, .. }));
    }

    #[test]
    fn describe_result_empty_columns_serializes_cleanly() {
        // A query that returns no projected columns (rare but
        // possible for INSERT without RETURNING) shouldn't trip up
        // the round-trip.
        let msg = ServerMessage::DescribeResult {
            session_id:   "s".into(),
            columns:      vec![],
            exec_time_us: 0,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        match back {
            ServerMessage::DescribeResult { columns, .. } => assert!(columns.is_empty()),
            _ => panic!("expected DescribeResult"),
        }
    }

    #[test]
    fn edit_externally_from_an_older_client_still_parses() {
        let old = r#"{"type":"EditExternally","data":{"session_id":"s","editor_command":"code -w","content":"select 1","tag":"edit_to_input"}}"#;
        let ClientMessage::EditExternally { connection_id, line, env_chain, line_flag_var, content, .. } =
            serde_json::from_str(old).unwrap() else { panic!("wrong variant") };
        assert_eq!(content, "select 1");
        assert!(connection_id.is_none() && line.is_none() && line_flag_var.is_none());
        assert!(env_chain.is_empty());
    }

    #[test]
    fn edit_externally_names_only_known_variables() {
        let ok = r#"{"type":"EditExternally","data":{"session_id":"s","content":"","tag":"t","env_chain":["PSQL_EDITOR","EDITOR","VISUAL"],"line_flag_var":"PSQL_EDITOR_LINENUMBER_ARG"}}"#;
        let ClientMessage::EditExternally { env_chain, line_flag_var, .. } =
            serde_json::from_str(ok).unwrap() else { panic!("wrong variant") };
        assert_eq!(env_chain.iter().map(|v| v.name()).collect::<Vec<_>>(), ["PSQL_EDITOR", "EDITOR", "VISUAL"]);
        assert_eq!(line_flag_var.map(|v| v.name()), Some("PSQL_EDITOR_LINENUMBER_ARG"));
        let other = r#"{"type":"EditExternally","data":{"session_id":"s","content":"","tag":"t","env_chain":["SHELL"]}}"#;
        assert!(serde_json::from_str::<ClientMessage>(other).is_err());
    }
}
