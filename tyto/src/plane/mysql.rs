//! MySQL plane — meta-commands, status/help, schema-explorer queries,
//! and the syntax-highlighting keyword/function lists.

use shared::{DbType, EditorVar};
use crate::{AdminArea, MetaAction, ObjectKind, PaneKind, PaneMenuItem};
use crate::types::AdminRow;
use crate::parser::{self, Lexicon};
use super::{DbmsPlane, ErrorStyle, LostConnection, Timing};

pub struct MySqlPlane;

/// What the mysql CLI prints for a `use` (or `\u`) without a database.
const USE_WITHOUT_DATABASE: [&str; 2] = ["ERROR: ", "USE must be followed by a database name"];

/// What the mysql CLI prints for a statement with nothing in it (`com_go`).
const NO_QUERY: [&str; 2] = ["ERROR: ", "No query specified"];

/// The mysql CLI's `delimiter` / `\d` (`com_delimiter`): the first word
/// after it is the new delimiter — `;` included, so `delimiter ;` goes
/// back — set without a word; nothing after it is an error.
fn set_delimiter(rest: &str) -> MetaAction {
    match rest.split_whitespace().next() {
        Some(delimiter) => MetaAction::SetDelimiter(delimiter.to_string()),
        None => MetaAction::Output(vec!["ERROR: ".into(), "DELIMITER must be followed by a 'delimiter' character or string".into()]),
    }
}

/// The mysql CLI's named commands (`commands[]` in client/mysql.cc):
/// each name, the letter of its backslash form (the last two have
/// none), and whether it takes arguments.
const NAMED_COMMANDS: [(&str, Option<char>, bool); 27] = [
    ("?", Some('?'), true),
    ("clear", Some('c'), false),
    ("connect", Some('r'), true),
    ("delimiter", Some('d'), true),
    ("edit", Some('e'), false),
    ("ego", Some('G'), false),
    ("exit", Some('q'), false),
    ("go", Some('g'), false),
    ("help", Some('h'), true),
    ("nopager", Some('n'), false),
    ("notee", Some('t'), false),
    ("pager", Some('P'), true),
    ("print", Some('p'), false),
    ("prompt", Some('R'), true),
    ("quit", Some('q'), false),
    ("rehash", Some('#'), false),
    ("source", Some('.'), true),
    ("status", Some('s'), false),
    ("system", Some('!'), true),
    ("tee", Some('T'), true),
    ("use", Some('u'), true),
    ("charset", Some('C'), true),
    ("warnings", Some('W'), false),
    ("nowarning", Some('w'), false),
    ("resetconnection", Some('x'), false),
    ("query_attributes", None, true),
    ("ssl_session_data_print", None, true),
];

/// `text` past the whitespace and `/* */` comments it starts with, as
/// the mysql CLI skips them before a named command
/// (`skip_over_comments_and_space`).
fn past_comments_and_space(text: &str) -> &str {
    let mut rest = text.trim_start();
    while let Some(end) = rest.strip_prefix("/*").and_then(|comment| comment.find("*/")) {
        rest = rest[2 + end + 2..].trim_start();
    }
    rest
}

/// The named command `text` is, as the mysql CLI's `find_command` reads
/// one: its name, in any case, up to the first space or tab; anything
/// after it only for a command that takes arguments. With its letter
/// and those arguments.
fn find_named(text: &str) -> Option<(&'static str, Option<char>, &str)> {
    let (name, arguments) = match text.find([' ', '\t']) {
        Some(at) => (&text[..at], text[at..].trim()),
        None => (text, ""),
    };
    NAMED_COMMANDS.iter()
        .find(|(named, _, takes_arguments)| named.eq_ignore_ascii_case(name) && (arguments.is_empty() || *takes_arguments))
        .map(|&(named, letter, _)| (named, letter, arguments))
}

/// The mysql CLI's `get_arg`: the argument `rest` starts with, past
/// whitespace — quoted with `'`, `"` or `` ` ``, up to its closing quote,
/// as it is; or else up to a space, a backslash keeping the character
/// after it — and what follows it. None for none, or an empty quoted one.
fn get_arg(rest: &str) -> Option<(String, &str)> {
    let rest = rest.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"' | '`'));
    let body = &rest[quote.map_or(0, char::len_utf8)..];
    let mut arg = String::new();
    let mut end = body.len();
    let mut chars = body.char_indices();
    while let Some((at, c)) = chars.next() {
        match quote {
            Some(quote) if c == quote => { end = at + c.len_utf8(); break; }
            Some(_) => arg.push(c),
            None if c == ' ' => { end = at + 1; break; }
            None if c == '\\' => match chars.next() {
                Some((_, escaped)) => arg.push(escaped),
                None => arg.push(c),
            },
            None => arg.push(c),
        }
    }
    (!arg.is_empty()).then(|| (arg, &body[end..]))
}

/// What this shell says for one of the CLI's commands it doesn't run yet.
fn not_supported(name: &str) -> MetaAction {
    MetaAction::Fail(vec![format!("{name} isn't supported here yet.")])
}

/// Major-keyword list for the MySQL plane. Sorted (binary_search).
/// The universal SQL-92 base + MySQL's daily-driver clause verbs:
///
/// - `ANALYZE` — statistics refresh (`ANALYZE TABLE`).
/// - `EXPLAIN` — query plan / `EXPLAIN ANALYZE` (8.0+).
/// - `SHOW` — by far the most-used MySQL meta-verb (`SHOW DATABASES`,
///   `SHOW CREATE TABLE`, `SHOW PROCESSLIST`). Without it, MySQL's
///   most distinctive shell idiom is rendered flat-colored.
/// - `USE` — database switch (`USE app`). MySQL's session-level
///   default-schema selector, structurally a clause verb.
///
/// Pre-fix the trait default (`MAJOR_KEYWORDS_UNIVERSAL`) omitted all
/// four. Audit finding 4 of issue #10.
static MYSQL_MAJOR_KEYWORDS: &[&str] = &[
    "ALTER", "ANALYZE", "BEGIN", "BY", "COMMIT", "CREATE", "CROSS",
    "DELETE", "DROP", "EXPLAIN", "FROM", "FULL", "GRANT", "GROUP",
    "HAVING", "INNER", "INSERT", "INTO", "JOIN", "LATERAL", "LEFT",
    "LIMIT", "NATURAL", "OFFSET", "ORDER", "OUTER", "RECURSIVE",
    "RETURNING", "REVOKE", "RIGHT", "ROLLBACK", "SAVEPOINT", "SELECT",
    "SET", "SHOW", "START", "TRUNCATE", "UPDATE", "USE", "VALUES",
    "WHERE", "WITH",
];

impl DbmsPlane for MySqlPlane {
    fn db_type(&self) -> DbType { DbType::MySql }
    fn name(&self) -> &'static str { "MySQL" }

    fn default_port(&self) -> u16 { 3306 }
    fn default_user(&self) -> &'static str { "root" }
    fn database_required(&self) -> bool { false }

    fn meta_cmd(&self, input: &str) -> Option<MetaAction> {
        let t = input.trim();

        // A command typed after the query buffer — on its line (`select
        // 1 \s`) or below — runs there: the mysql CLI runs one at the
        // line's first backslash outside quotes and comments that starts
        // one of its commands. It works on the buffer (`\e`, `\c`) or
        // leaves it (`MetaAction::works_on_buffer`), and takes what
        // `command_extent` says; `shell.rs` splits the input the same
        // way. (`\g` and `\G` end a statement instead:
        // `parser::split_statements`.)
        if let Some(at) = parser::trailing_command(t, Lexicon::Mysql).filter(|&at| at > 0) {
            return self.meta_cmd(&t[at..]);
        }
        // Its line: a command reads its arguments from the rest of it,
        // past the delimiter where it ends (`\usakila;x` is database
        // `sakila;x`), as the CLI hands its commands the line.
        let t = &t[..t.find('\n').unwrap_or(t.len())];

        if let Some(after) = t.strip_prefix('\\') {
            // Parliament's own `\safe-mode on|off` — a word, as on psql.
            if let Some(mode) = after.strip_prefix("safe-mode").filter(|m| m.is_empty() || m.starts_with(|c: char| c.is_whitespace() || c == ';')) {
                return Some(match mode.trim().trim_end_matches(';').to_ascii_lowercase().as_str() {
                    "on"  => MetaAction::SafeMode(true),
                    "off" => MetaAction::SafeMode(false),
                    _     => MetaAction::Output(vec!["Usage: \\safe-mode on | off".into()]),
                });
            }
            // The mysql CLI's: `\` and a letter, its arguments what
            // follows it (`\usakila`, `\d//`) — except `com_help`'s
            // topic, what follows the first space (`\hselect` is plain
            // `\h`) — to the line's end (`\h select;` is topic `select;`).
            let letter = after.chars().next()?;
            let following = &after[letter.len_utf8()..];
            let arguments = match letter {
                '?' | 'h' => following.split_once(char::is_whitespace).map_or("", |(_, topic)| topic.trim()),
                _ => following.trim(),
            };
            return self.letter_command(letter, arguments, &t[..1 + letter.len_utf8()]);
        }
        // Its named commands (`status`, `use db`) are a line's or a
        // statement's: `named_command`, `statement_command`.
        None
    }

    /// A line typed while no statement is pending, as the mysql CLI's
    /// `read_and_execute` reads it: one of its named commands — unless
    /// the line has `\g` or the delimiter in it (but for `delimiter
    /// …`), when it's read as statements, each of which may be one
    /// (`statement_command`). `go` / `ego` run the empty buffer: "No
    /// query specified".
    fn named_command(&self, line: &str, delimiter: &str) -> Option<MetaAction> {
        let text = past_comments_and_space(line);
        let sets_delimiter = text.get(..9).is_some_and(|word| word.eq_ignore_ascii_case("delimiter"));
        if text.contains("\\g") || (text.contains(delimiter) && !sets_delimiter) {
            return None;
        }
        match find_named(text)? {
            (_, Some('g' | 'G'), _) => Some(MetaAction::Fail(NO_QUERY.iter().map(|l| l.to_string()).collect())),
            (name, letter, arguments) => self.named(name, letter, text, arguments),
        }
    }

    /// A statement its delimiter ended, as the mysql CLI's `add_line`
    /// reads it: one of its named commands runs as that command
    /// (`status;`, `select 1; use db;`). `go` / `ego` run the statement —
    /// their own name — which goes to the server.
    fn statement_command(&self, statement: &str) -> Option<MetaAction> {
        let text = past_comments_and_space(statement);
        match find_named(text)? {
            (_, Some('g' | 'G'), _) => None,
            (name, letter, arguments) => self.named(name, letter, text, arguments),
        }
    }

    /// The mysql CLI's: `\` and its letter, the line reading on after
    /// it (`\sf f` is `\s` and SQL, `\s;` is `\s` and an empty
    /// statement), or for a command with arguments up to the delimiter
    /// after them (`\u db; select 1;`) or the line's end
    /// (`parser::mysql_command_len`). Parliament's own `\safe-mode` and
    /// a named command typed alone on its line (`use db`) take the line.
    fn command_extent(&self, command: &str, delimiter: &str) -> usize {
        let line = command.find('\n').unwrap_or(command.len());
        let word = command[..line].strip_prefix('\\')
            .and_then(|after| after.split(char::is_whitespace).next())
            .map(|word| word.trim_end_matches(';'));
        match word {
            Some("safe-mode") => line,
            _ => parser::mysql_command_len(command, delimiter).unwrap_or(line),
        }
    }

    // mysql CLI: EDITOR, then VISUAL. No line-number convention.
    fn editor_env_chain(&self) -> &'static [EditorVar] {
        &[EditorVar::Editor, EditorVar::Visual]
    }

    /// The one staged command: the server's help (`\h topic`), its
    /// answer printed as the CLI's `com_server_help` prints it — or the
    /// server's error, as the CLI says one.
    fn meta_continue(&self, input: &str, _ctx: &super::MetaContext, replies: &[shared::BatchResult]) -> super::MetaStep {
        let Some(reply) = replies.first() else { return super::MetaStep::Done(Vec::new()) };
        if let Some(error) = &reply.error {
            let lines = self.format_error(error, None, ErrorStyle::default());
            return super::MetaStep::Failed(vec![crate::ShellEntry::CliError { lines, message: error.message.clone() }]);
        }
        let lines = server_help_lines(&server_help_query(help_topic(input)), reply.sets.first());
        super::MetaStep::Done(vec![crate::ShellEntry::Printed { lines, links: Vec::new() }])
    }
    /// `print_warnings` asks `show warnings`…
    fn warnings_query(&self) -> Option<&'static str> { Some("show warnings") }
    /// … and prints each row as `Warning (Code 1365): Division by 0` —
    /// but nothing when the one row is the statement's own error again.
    fn warning_lines(&self, set: &shared::ResultSet, error: Option<u16>) -> Vec<String> {
        let cell = |row: &[Option<String>], i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        if let [only] = &set.rows[..] {
            if error.is_some_and(|error| cell(only, 1).parse::<u16>().ok() == Some(error)) {
                return Vec::new();
            }
        }
        set.rows.iter().map(|row| format!("{} (Code {}): {}", cell(row, 0), cell(row, 1), cell(row, 2))).collect()
    }
    /// The mysql CLI's `ERROR 1146 (42S02): Table 'sakila.nosuch' doesn't exist`.
    fn format_error(&self, error: &shared::DbError, _query: Option<&str>, _style: ErrorStyle) -> Vec<String> {
        vec![match (error.number, &error.sqlstate) {
            (Some(n), Some(state)) => format!("ERROR {n} ({state}): {}", error.message),
            (Some(n), None) => format!("ERROR {n}: {}", error.message),
            _ => format!("ERROR: {}", error.message),
        }]
    }

    fn help_lines(&self) -> Vec<String> {
        crate::format::format_help("List of all MySQL commands", &[
            ("? (\\?)",           "Synonym for help."),
            ("\\G",               "Execute query, display result vertically."),
            ("help (\\h)",        "Display this help. \\h KEYWORD for SQL syntax help."),
            ("quit (\\q)",        "Disconnect from server."),
            ("exit",              "Synonym for quit."),
            ("status (\\s)",      "Get status information from the server."),
            ("use (\\u) db",      "Use another database."),
            ("\\c",               "Clear the current input buffer."),
            ("\\safe-mode on|off","Confirm before destructive commands."),
            ("\\d str",           "Set statement delimiter (e.g. \\d //)."),
            ("DELIMITER str",     "Set statement delimiter (e.g. DELIMITER //)."),
            ("edit (\\e)",        "Edit the input buffer (or previous command) with $EDITOR."),
        ])
    }

    // ── Schema explorer ────────────────────────────────────────────────

    fn admin_areas(&self) -> &'static [crate::AdminArea] {
        // MySQL: users (under Roles label — they're the same thing here),
        // databases, server config. No real Tablespaces / Extensions.
        &[
            crate::AdminArea::Roles,
            crate::AdminArea::Databases,
            crate::AdminArea::ServerConfig,
        ]
    }

    fn major_keywords(&self) -> &'static [&'static str] { MYSQL_MAJOR_KEYWORDS }

    fn schema_picker_label(&self) -> &'static str { "Database" }

    fn list_roles_sql(&self) -> Option<&'static str> {
        // One row per (User, Host) account — that's the actual primary key
        // in mysql.user. Each row is its own visible entity in the flat
        // list rather than nested under a username (per Parliament's
        // no-trees design language).
        //
        // `Superuser` is derived from `Super_priv` (the static SUPER
        // privilege column on mysql.user). A user with `Super_priv = 'Y'`
        // bypasses most privilege checks server-wide. This matches PG's
        // `rolsuper` semantically — the bright line the Privileges tab
        // banner needs to render. Audit finding 1 of issue #10.
        //
        // MySQL 8.0 also has dynamic privileges (SYSTEM_USER,
        // BACKUP_ADMIN, etc.) stored in `mysql.global_grants`. These
        // are deliberately NOT joined here because mysql.global_grants
        // doesn't exist on 5.7 and a query against it would fail at
        // parse time. A dedicated 8.0-version-gated SYSTEM_USER probe
        // is a separate follow-up (issue #10 item 1's adjacent).
        Some(
            "SELECT User AS `Name`, \
                    Host, \
                    plugin AS `Auth Plugin`, \
                    CASE WHEN Super_priv      = 'Y' THEN 'Yes' ELSE '' END AS `Superuser`, \
                    CASE WHEN account_locked  = 'Y' THEN 'Yes' ELSE '' END AS `Locked`, \
                    CASE WHEN password_expired = 'Y' THEN 'Yes' ELSE '' END AS `Pwd Expired` \
             FROM mysql.user \
             ORDER BY User, Host"
        )
    }

    fn role_list_extra_columns(&self) -> &'static [&'static str] {
        &["Host", "Auth Plugin", "Superuser", "Locked", "Pwd Expired"]
    }

    fn role_is_superuser(&self, row: &AdminRow) -> bool {
        // `role_list_extra_columns` puts Superuser at index 2 (after
        // Host and Auth Plugin). The list_roles_sql above emits 'Yes'
        // when Super_priv = 'Y'. Pre-fix this method had no MySQL
        // override and the trait default returned false unconditionally,
        // so MySQL DBAs inspecting `'root'@'localhost'` saw an empty
        // Privileges matrix AND no "superuser bypasses checks" banner.
        // Audit finding 1 of issue #10.
        row.extras.get(2).map(|s| s == "Yes").unwrap_or(false)
    }

    fn role_privileges_sql(&self, name: &str, extras: &[String]) -> Option<String> {
        // Structured rows from `information_schema.{USER,SCHEMA,TABLE}
        // _PRIVILEGES` instead of the legacy `SHOW GRANTS` text. Each
        // row is one scope's grant set — `*.*` (global), `db.*`
        // (schema), or `db.tbl` (table) — aggregated by privilege.
        // Shape matches PG's `role_privileges_sql` output so the
        // renderer can treat both planes uniformly + the "Show
        // affected objects" resolver has a clean `Object` column to
        // parse for scope.
        //
        // Compatibility constraints (intentionally respected):
        //   * No `WITH` CTE — flat UNION works on MySQL 5.7+.
        //   * No `mysql.user` — `information_schema.*_PRIVILEGES`
        //     visibility follows the connecting user's grants, which
        //     is what we want anyway.
        //
        // The privilege filter omits routine-level types (EXECUTE,
        // ALTER ROUTINE, etc.) — those belong on a future Procedure
        // / Function role-privileges view.
        let host = extras.first().cloned().unwrap_or_else(|| "%".to_string());
        let user_q = name.replace('\'', "''");
        let host_q = host.replace('\'', "''");
        let grantee_literal = format!("'{}'@'{}'", user_q, host_q);
        let priv_filter = "PRIVILEGE_TYPE IN \
            ('SELECT','INSERT','UPDATE','DELETE','REFERENCES','INDEX',\
             'ALTER','DROP','TRIGGER','CREATE','CREATE VIEW','SHOW VIEW',\
             'EVENT','LOCK TABLES')";
        // Kind classification: global / schema are obvious. Table-level
        // grants can land on either a base table or a view, so we look
        // up the actual TABLE_TYPE; if the row is missing from
        // information_schema.TABLES (visibility quirk on the connecting
        // user's grants) we fall back to 'table'.
        Some(format!(
            "SELECT '*.*' AS `Object`, \
                    GROUP_CONCAT(PRIVILEGE_TYPE ORDER BY PRIVILEGE_TYPE SEPARATOR ', ') AS `Privileges`, \
                    MAX(IS_GRANTABLE) AS `Grantable`, \
                    'global' AS `Source`, \
                    'global' AS `Kind` \
             FROM information_schema.USER_PRIVILEGES \
             WHERE GRANTEE = '{grantee_literal}' AND {priv_filter} \
             HAVING COUNT(*) > 0 \
             UNION ALL \
             SELECT CONCAT(TABLE_SCHEMA, '.*'), \
                    GROUP_CONCAT(PRIVILEGE_TYPE ORDER BY PRIVILEGE_TYPE SEPARATOR ', '), \
                    MAX(IS_GRANTABLE), \
                    'schema', \
                    'schema' \
             FROM information_schema.SCHEMA_PRIVILEGES \
             WHERE GRANTEE = '{grantee_literal}' AND {priv_filter} \
             GROUP BY TABLE_SCHEMA \
             UNION ALL \
             SELECT CONCAT(tp.TABLE_SCHEMA, '.', tp.TABLE_NAME), \
                    GROUP_CONCAT(tp.PRIVILEGE_TYPE ORDER BY tp.PRIVILEGE_TYPE SEPARATOR ', '), \
                    MAX(tp.IS_GRANTABLE), \
                    'table', \
                    COALESCE( \
                      CASE MAX(t.TABLE_TYPE) \
                        WHEN 'BASE TABLE' THEN 'table' \
                        WHEN 'VIEW' THEN 'view' \
                        WHEN 'SYSTEM VIEW' THEN 'view' \
                        ELSE LOWER(MAX(t.TABLE_TYPE)) \
                      END, 'table') \
             FROM information_schema.TABLE_PRIVILEGES tp \
             LEFT JOIN information_schema.TABLES t \
                    ON t.TABLE_SCHEMA = tp.TABLE_SCHEMA \
                   AND t.TABLE_NAME = tp.TABLE_NAME \
             WHERE tp.GRANTEE = '{grantee_literal}' \
             GROUP BY tp.TABLE_SCHEMA, tp.TABLE_NAME \
             UNION ALL \
             /* Routine direct grants from mysql.procs_priv. Aggregates
                Proc_priv (a SET) into a comma-separated list per row.
                Requires SELECT on mysql.procs_priv — non-root users
                may see nothing here, same failure mode as the table
                query against an invisible grant. Kind = function or
                procedure based on Routine_type. */ \
             SELECT CONCAT(pp.Db, '.', pp.Routine_name), \
                    REPLACE(pp.Proc_priv, ',', ', '), \
                    '', \
                    'Grant', \
                    LOWER(pp.Routine_type) \
             FROM mysql.procs_priv pp \
             WHERE pp.User = '{user_q}' AND pp.Host = '{host_q}' \
             ORDER BY 4, 1",
            // Two-level escape: the inner `'root'@'%'` form *is*
            // the value stored in information_schema GRANTEE,
            // quotes and all. To match it inside a SQL string
            // literal, each of those embedded quotes is doubled
            // here; the format string then wraps the result in
            // outer single quotes.
            grantee_literal = grantee_literal.replace('\'', "''"),
            priv_filter = priv_filter,
            // procs_priv stores User and Host separately, not as the
            // quoted 'user'@'host' string GRANTEE carries — feed the
            // raw escaped halves through.
            user_q = user_q,
            host_q = host_q,
        ))
    }

    fn role_privileges_columns(&self) -> &'static [&'static str] {
        // Matches the PG plane: structured per-scope rows with
        // an Object qualifier, privilege list, grantable flag,
        // a source category (`global` / `schema` / `table`), and
        // the underlying object kind (`global` / `schema` /
        // `table` / `view`).
        &["Object", "Privileges", "Grantable", "Source", "Kind"]
    }

    fn role_affected_objects_sql(
        &self,
        _name: &str,
        _extras: &[String],
        selected_objects: &[String],
    ) -> Option<String> {
        if selected_objects.is_empty() { return None; }
        // Per-scope OR-joined WHERE filter. Built once per UNION arm
        // since the schema/name column names differ: information_schema.
        // TABLES uses TABLE_SCHEMA/TABLE_NAME, ROUTINES uses
        // ROUTINE_SCHEMA/ROUTINE_NAME.
        let esc = |s: &str| s.replace('\'', "''");
        let build_clauses = |schema_col: &str, name_col: &str| -> String {
            selected_objects.iter().map(|obj| {
                if obj == "*.*" {
                    "TRUE".to_string()
                } else if let Some(schema) = obj.strip_suffix(".*") {
                    format!("{} = '{}'", schema_col, esc(schema))
                } else if let Some((s, t)) = obj.split_once('.') {
                    format!("({} = '{}' AND {} = '{}')",
                            schema_col, esc(s), name_col, esc(t))
                } else {
                    format!("{} = '{}'", schema_col, esc(obj))
                }
            }).collect::<Vec<_>>().join(" OR ")
        };
        let where_tables   = build_clauses("TABLE_SCHEMA",   "TABLE_NAME");
        let where_routines = build_clauses("ROUTINE_SCHEMA", "ROUTINE_NAME");
        Some(format!(
            "SELECT DISTINCT \
                    TABLE_SCHEMA AS `Schema`, \
                    CASE TABLE_TYPE \
                        WHEN 'BASE TABLE' THEN 'table' \
                        WHEN 'VIEW' THEN 'view' \
                        WHEN 'SYSTEM VIEW' THEN 'view' \
                        ELSE LOWER(TABLE_TYPE) \
                    END AS `Kind`, \
                    TABLE_NAME   AS `Object` \
             FROM information_schema.TABLES \
             WHERE ({where_tables}) \
               AND TABLE_SCHEMA NOT IN ('mysql', 'information_schema', \
                                        'performance_schema', 'sys') \
             UNION ALL \
             SELECT DISTINCT \
                    ROUTINE_SCHEMA, \
                    LOWER(ROUTINE_TYPE), \
                    ROUTINE_NAME \
             FROM information_schema.ROUTINES \
             WHERE ({where_routines}) \
               AND ROUTINE_SCHEMA NOT IN ('mysql', 'information_schema', \
                                          'performance_schema', 'sys') \
             ORDER BY 2, 1, 3",
        ))
    }

    fn object_grants_sql(
        &self,
        kind: ObjectKind,
        schema: &str,
        name: &str,
    ) -> Option<String> {
        let s = schema.replace('\'', "''");
        let n = name.replace('\'', "''");
        // Routines branch first — completely different shape from
        // tables/views, so it gets its own SQL. Grants live in
        // mysql.procs_priv (5.7 + 8.0). The Proc_priv column is a
        // SET — split it into one row per privilege via FIND_IN_SET
        // crossed with a literal privilege-types VALUES list. No
        // CTEs (5.7 compatibility), no JSON_TABLE (8.0+).
        //
        // procs_priv requires SELECT on the mysql schema. For users
        // who can't read it, the query returns zero rows — same
        // failure mode as our table query against an invisible
        // grant.
        if matches!(kind, ObjectKind::Procedures | ObjectKind::Functions) {
            let routine_type = if matches!(kind, ObjectKind::Procedures) { "PROCEDURE" } else { "FUNCTION" };
            // Three privilege layers parallel to the tables query:
            //   1. mysql.procs_priv  — routine-level direct grants
            //   2. SCHEMA_PRIVILEGES — db-level routine privileges
            //   3. USER_PRIVILEGES   — global routine privileges
            //
            // The matrix has three columns — Execute, Alter Routine,
            // Grant. At the db/global layers each information_schema
            // row maps to exactly ONE column (cross-joining produces
            // false positives: a db-level EXECUTE doesn't imply
            // Alter Routine). The mapping intentionally omits
            // PRIVILEGE_TYPE='GRANT OPTION': at the db/global level
            // that's a meta-privilege (the right to re-grant
            // existing privs), not a separate routine-level Grant.
            // The matrix already conveys grantability per cell via
            // the IS_GRANTABLE flag rendered as a "(with grant
            // option)" tooltip — emitting a Privilege='Grant' row
            // for GRANT OPTION would double-count it and imply the
            // routine-level Grant privilege from procs_priv exists,
            // which it doesn't at the wider scopes.
            let is_priv_map = "CASE PRIVILEGE_TYPE \
                WHEN 'EXECUTE' THEN 'Execute' \
                WHEN 'ALTER ROUTINE' THEN 'Alter Routine' \
                END";
            let is_priv_filter = "PRIVILEGE_TYPE IN ('EXECUTE','ALTER ROUTINE')";
            return Some(format!(
                "SELECT CONCAT(pp.User, '@', pp.Host) AS `Grantee`, \
                        pt.priv AS `Privilege`, \
                        'direct' AS `Source`, \
                        '' AS `Chain`, \
                        '' AS `Grantable`, \
                        'direct' AS `RoleCategory` \
                 FROM mysql.procs_priv pp \
                 JOIN ( \
                    SELECT 'Execute' AS priv UNION ALL \
                    SELECT 'Alter Routine' UNION ALL \
                    SELECT 'Grant' \
                 ) pt ON FIND_IN_SET(pt.priv, pp.Proc_priv) > 0 \
                 WHERE pp.Db = '{s}' AND pp.Routine_name = '{n}' AND pp.Routine_type = '{rt}' \
                 UNION ALL \
                 SELECT REPLACE(GRANTEE, '''', ''), \
                        {is_priv_map}, 'direct', CONCAT('{s}', '.*'), \
                        IS_GRANTABLE, 'direct' \
                 FROM information_schema.SCHEMA_PRIVILEGES \
                 WHERE TABLE_SCHEMA = '{s}' AND {is_priv_filter} \
                 UNION ALL \
                 SELECT REPLACE(GRANTEE, '''', ''), \
                        {is_priv_map}, 'direct', '*.*', \
                        IS_GRANTABLE, 'direct' \
                 FROM information_schema.USER_PRIVILEGES \
                 WHERE {is_priv_filter} \
                 ORDER BY 3, 1",
                s = s, n = n, rt = routine_type,
                is_priv_map = is_priv_map,
                is_priv_filter = is_priv_filter,
            ));
        }
        // Tables / views branch (unchanged).
        if !matches!(kind, ObjectKind::Tables | ObjectKind::Views) {
            return None;
        }
        // v1 (MySQL): UNION of three scope-leveled grant sources,
        // since MySQL stores privileges at four levels and our matrix
        // wants to show every grant that effectively applies to the
        // target table:
        //
        //   1. `information_schema.USER_PRIVILEGES`   — global  (*.*)
        //   2. `information_schema.SCHEMA_PRIVILEGES` — database
        //   3. `information_schema.TABLE_PRIVILEGES`  — table itself
        //   (column-level grants intentionally skipped — they'd
        //   need to render per-column, which is a future phase).
        //
        // Each source contributes the same shape; the `Chain` field
        // carries the scope qualifier (`*.*`, `sakila.*`, or empty)
        // so the cell tooltip can read "Direct grant via *.*". The
        // user sees ONE row per (user, privilege) cell — the pivoter
        // collapses duplicates by source rank, with table-level
        // beating wider scopes when both exist.
        //
        // Two compatibility constraints we keep:
        //   * No WITH-CTEs — would break MySQL 5.7.
        //   * No `mysql.user` reads — would require SELECT on the
        //     mysql schema, which most non-root users don't have.
        //     `information_schema.*_PRIVILEGES` is accessible to
        //     any authenticated user (filtered to their own grants
        //     unless they have wider visibility).
        //
        // Owner / superuser / inherited / no-grant sections remain
        // PG-only — phase 8b territory for MySQL.
        //
        // GRANTEE comes back literally quoted as `'user'@'host'`
        // (with the single-quote characters as part of the string);
        // strip them so the matrix labels read as `user@host`.
        // Filter list mirrors `object_privilege_kinds(Tables)` —
        // skipping CREATE (meaningless on existing-object scope) and
        // anything routine-level.
        let priv_filter = "PRIVILEGE_TYPE IN \
            ('SELECT','INSERT','UPDATE','DELETE','REFERENCES',\
             'INDEX','ALTER','DROP','TRIGGER')";
        Some(format!(
            "SELECT REPLACE(GRANTEE, '''', '') AS `Grantee`, \
                    PRIVILEGE_TYPE             AS `Privilege`, \
                    'direct'                   AS `Source`, \
                    ''                         AS `Chain`, \
                    IS_GRANTABLE               AS `Grantable`, \
                    'direct'                   AS `RoleCategory` \
             FROM information_schema.TABLE_PRIVILEGES \
             WHERE TABLE_SCHEMA = '{s}' AND TABLE_NAME = '{n}' \
             UNION ALL \
             SELECT REPLACE(GRANTEE, '''', ''), \
                    PRIVILEGE_TYPE, 'direct', \
                    CONCAT('{s}', '.*'), \
                    IS_GRANTABLE, 'direct' \
             FROM information_schema.SCHEMA_PRIVILEGES \
             WHERE TABLE_SCHEMA = '{s}' AND {priv_filter} \
             UNION ALL \
             SELECT REPLACE(GRANTEE, '''', ''), \
                    PRIVILEGE_TYPE, 'direct', \
                    '*.*', \
                    IS_GRANTABLE, 'direct' \
             FROM information_schema.USER_PRIVILEGES \
             WHERE {priv_filter} \
             ORDER BY 1, 2"
        ))
    }

    fn grant_sql(
        &self,
        kind: ObjectKind,
        schema: &str,
        name: &str,
        grantee: &str,
        privilege: &str,
    ) -> Option<String> {
        my_grant_or_revoke("GRANT", "TO", kind, schema, name, grantee, privilege)
    }

    fn revoke_sql(
        &self,
        kind: ObjectKind,
        schema: &str,
        name: &str,
        grantee: &str,
        privilege: &str,
    ) -> Option<String> {
        my_grant_or_revoke("REVOKE", "FROM", kind, schema, name, grantee, privilege)
    }

    fn object_privilege_kinds(&self, kind: ObjectKind) -> &'static [&'static str] {
        match kind {
            // MySQL table-level privileges. CREATE is dropped from
            // this list deliberately — at an existing-object scope
            // it's near-meaningless (the table already exists), and
            // its real home is `db.*` / `*.*` "create new things in
            // this scope" grants. The remaining privileges all
            // describe operations you can do TO an existing table.
            ObjectKind::Tables => &[
                "SELECT", "INSERT", "UPDATE", "DELETE",
                "REFERENCES", "INDEX", "ALTER",
                "DROP", "TRIGGER",
            ],
            // Views accept the same privilege set as Tables. INDEX /
            // ALTER / DROP / TRIGGER are unusual on a view in
            // practice but they're legitimate grants that
            // `information_schema.TABLE_PRIVILEGES` returns for view
            // rows (MySQL stores view grants in the same table). The
            // pre-fix matrix listed only the DML 4, so the 5 extra
            // privilege types came back from `object_grants_sql` and
            // silently landed nowhere — the DBA couldn't see that the
            // grants existed even though they were affecting access.
            // Audit finding 8 of issue #10.
            //
            // CREATE VIEW / SHOW VIEW are deliberately not here —
            // they're database-scope grants, not view-scope, and
            // belong on a future db-level privileges view.
            ObjectKind::Views => &[
                "SELECT", "INSERT", "UPDATE", "DELETE",
                "REFERENCES", "INDEX", "ALTER",
                "DROP", "TRIGGER",
            ],
            // Routines: MySQL's mysql.procs_priv stores a SET of
            // 'Execute' / 'Alter Routine' / 'Grant'. Match the
            // exact spelling from the SET so the matrix pivoter
            // lines up rows with column headers — case-insensitive
            // lookup would work either way, but staying explicit
            // documents the on-disk vocabulary.
            ObjectKind::Procedures | ObjectKind::Functions => &[
                "Execute", "Alter Routine", "Grant",
            ],
            _ => &[],
        }
    }

    fn area_list_sql(&self, area: AdminArea) -> Option<String> {
        match area {
            AdminArea::Databases => Some(
                "SELECT SCHEMA_NAME AS `Name`, \
                        DEFAULT_CHARACTER_SET_NAME AS `Charset`, \
                        DEFAULT_COLLATION_NAME AS `Collation` \
                 FROM information_schema.SCHEMATA \
                 ORDER BY SCHEMA_NAME".to_string()
            ),
            AdminArea::ServerConfig => Some(
                // performance_schema.global_variables is the modern source
                // (8.0+); SHOW VARIABLES is the legacy alternative. PS
                // exposes the same variables with stable column names.
                "SELECT VARIABLE_NAME AS `Name`, VARIABLE_VALUE AS `Value` \
                 FROM performance_schema.global_variables \
                 ORDER BY VARIABLE_NAME".to_string()
            ),
            _ => None,
        }
    }

    fn area_list_columns(&self, area: AdminArea) -> &'static [&'static str] {
        match area {
            AdminArea::Databases    => &["Name", "Charset", "Collation"],
            AdminArea::ServerConfig => &["Name", "Value"],
            _ => &[],
        }
    }

    fn area_row_clickable(&self, area: AdminArea) -> bool {
        // MySQL's Database IS its schema — the one navigable container
        // the RDBMS has. Pre-fix the trait default returned false, so
        // Admin → Databases reached a dead-end: a DBA could see the
        // database list but couldn't drill into any of them. Issue #21
        // item 2 / audit cluster D finding 3.2.
        matches!(area, AdminArea::Databases)
    }

    fn area_row_detail_sections(&self, area: AdminArea) -> &'static [&'static str] {
        match area {
            // Three sections, ordered by "what does a DBA most want to
            // see when they open a database?": the tables inside it
            // (the actual work), the privilege grants on it, and the
            // charset / collation defaults (the structural metadata).
            AdminArea::Databases => &["Tables", "Grants", "Charset"],
            _ => &[],
        }
    }

    fn area_row_detail_sql(&self, area: AdminArea, row_id: &str, section: &str) -> Option<String> {
        // The database name (row_id) comes from the row the user just
        // clicked — already an identifier known to the server. Quote
        // single quotes for SQL-string embedding; backtick-quoting
        // happens nowhere here because we don't interpolate the
        // database name as an identifier.
        let db = row_id.replace('\'', "''");
        match (area, section) {
            (AdminArea::Databases, "Tables") => Some(format!(
                // information_schema.TABLES is the canonical cross-engine
                // source — works on InnoDB / MyISAM / Memory / etc.
                // TABLE_ROWS is an estimate (statistics-driven; equals
                // 0 for views); the DBA reads it that way. DATA_LENGTH
                // is pre-formatted to a human-readable string here
                // (B/KB/MB/GB) since the drill-in renderer is generic
                // string-display and doesn't know to apply
                // human-byte formatting to specific columns.
                //
                // Views and certain engines (INFORMATION_SCHEMA,
                // PERFORMANCE_SCHEMA) have NULL DATA_LENGTH / ENGINE
                // — both render as empty cells, paired with the Type
                // column reading `VIEW` / `SYSTEM VIEW`, which is the
                // DBA-recognizable signal for "no physical storage."
                "SELECT TABLE_NAME AS `Table`, \
                        TABLE_TYPE AS `Type`, \
                        COALESCE(ENGINE, '') AS `Engine`, \
                        CAST(COALESCE(TABLE_ROWS, 0) AS UNSIGNED) AS `Rows`, \
                        CASE \
                            WHEN DATA_LENGTH IS NULL              THEN '' \
                            WHEN DATA_LENGTH < 1024               THEN CONCAT(DATA_LENGTH, ' B') \
                            WHEN DATA_LENGTH < 1048576            THEN CONCAT(ROUND(DATA_LENGTH/1024, 1), ' KB') \
                            WHEN DATA_LENGTH < 1073741824         THEN CONCAT(ROUND(DATA_LENGTH/1048576, 1), ' MB') \
                            ELSE CONCAT(ROUND(DATA_LENGTH/1073741824, 2), ' GB') \
                        END AS `Data size` \
                 FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = '{db}' \
                 ORDER BY TABLE_NAME",
                db = db,
            )),
            (AdminArea::Databases, "Grants") => Some(format!(
                // information_schema.SCHEMA_PRIVILEGES — db-level
                // grants (`db.*`). GRANTEE comes literally quoted as
                // `'user'@'host'`; strip the quotes so the matrix
                // reads as `user@host`. We do NOT pivot to a matrix
                // here because the drill-in renderer is the generic
                // flat-table path — different code path from the
                // per-schema Privileges section which has its own
                // pivot logic.
                "SELECT REPLACE(GRANTEE, '''', '') AS `Grantee`, \
                        PRIVILEGE_TYPE AS `Privilege`, \
                        IS_GRANTABLE AS `Grantable` \
                 FROM information_schema.SCHEMA_PRIVILEGES \
                 WHERE TABLE_SCHEMA = '{db}' \
                 ORDER BY GRANTEE, PRIVILEGE_TYPE",
                db = db,
            )),
            (AdminArea::Databases, "Charset") => Some(format!(
                // One row per database — same data as the outer list
                // already shows, but re-surfaced inside the drill-in
                // for completeness. The single row makes the section
                // look like a key/value summary.
                "SELECT DEFAULT_CHARACTER_SET_NAME AS `Charset`, \
                        DEFAULT_COLLATION_NAME AS `Collation` \
                 FROM information_schema.SCHEMATA \
                 WHERE SCHEMA_NAME = '{db}'",
                db = db,
            )),
            _ => None,
        }
    }

    fn area_row_detail_columns(&self, area: AdminArea, section: &str) -> &'static [&'static str] {
        // Header labels MUST be position-aligned with the SELECT
        // column list in area_row_detail_sql above. The renderer
        // walks rows positionally — a mismatch silently drops a
        // column or labels it wrong.
        match (area, section) {
            (AdminArea::Databases, "Tables")  => &["Table", "Type", "Engine", "Rows", "Data size"],
            (AdminArea::Databases, "Grants")  => &["Grantee", "Privilege", "Grantable"],
            (AdminArea::Databases, "Charset") => &["Charset", "Collation"],
            _ => &[],
        }
    }

    fn object_kinds(&self) -> &'static [ObjectKind] {
        // MySQL supports the full set.
        &[
            ObjectKind::Tables,
            ObjectKind::Views,
            ObjectKind::Procedures,
            ObjectKind::Functions,
            ObjectKind::Triggers,
            ObjectKind::Events,
        ]
    }

    fn list_schemas_sql(&self) -> &'static str {
        "SHOW DATABASES"
    }

    fn system_schemas(&self) -> &'static [&'static str] {
        &["information_schema", "performance_schema", "mysql", "sys"]
    }

    fn list_sql(&self, kind: ObjectKind, schema: Option<&str>) -> String {
        let where_schema = |col: &str| -> String {
            match schema {
                Some(s) => format!("WHERE {} = '{}'", col, s.replace('\'', "''")),
                None    => String::new(),
            }
        };
        match kind {
            ObjectKind::Tables => format!(
                "SELECT TABLE_SCHEMA, TABLE_NAME, ENGINE, \
                        IFNULL(TABLE_ROWS,'') AS TABLE_ROWS, \
                        IFNULL(DATE_FORMAT(UPDATE_TIME,'%Y-%m-%d %H:%i'),'') AS UPDATED, \
                        IFNULL(TABLE_COMMENT,'') AS TABLE_COMMENT \
                 FROM information_schema.TABLES \
                 {} {} TABLE_TYPE='BASE TABLE' \
                 ORDER BY TABLE_SCHEMA, TABLE_NAME",
                where_schema("TABLE_SCHEMA"),
                if schema.is_some() { "AND" } else { "WHERE" },
            ),
            ObjectKind::Views => format!(
                "SELECT TABLE_SCHEMA, TABLE_NAME, DEFINER, IS_UPDATABLE, SECURITY_TYPE \
                 FROM information_schema.VIEWS \
                 {} \
                 ORDER BY TABLE_SCHEMA, TABLE_NAME",
                where_schema("TABLE_SCHEMA"),
            ),
            ObjectKind::Procedures => format!(
                "SELECT ROUTINE_SCHEMA, ROUTINE_NAME, DEFINER, \
                        IFNULL(DATE_FORMAT(CREATED,'%Y-%m-%d'),'') AS CREATED, \
                        IFNULL(DATE_FORMAT(LAST_ALTERED,'%Y-%m-%d'),'') AS MODIFIED \
                 FROM information_schema.ROUTINES \
                 {} {} ROUTINE_TYPE='PROCEDURE' \
                 ORDER BY ROUTINE_SCHEMA, ROUTINE_NAME",
                where_schema("ROUTINE_SCHEMA"),
                if schema.is_some() { "AND" } else { "WHERE" },
            ),
            ObjectKind::Functions => format!(
                "SELECT ROUTINE_SCHEMA, ROUTINE_NAME, DEFINER, \
                        IFNULL(DTD_IDENTIFIER,'') AS RETURNS, \
                        IFNULL(DATE_FORMAT(CREATED,'%Y-%m-%d'),'') AS CREATED \
                 FROM information_schema.ROUTINES \
                 {} {} ROUTINE_TYPE='FUNCTION' \
                 ORDER BY ROUTINE_SCHEMA, ROUTINE_NAME",
                where_schema("ROUTINE_SCHEMA"),
                if schema.is_some() { "AND" } else { "WHERE" },
            ),
            ObjectKind::Triggers => format!(
                "SELECT TRIGGER_SCHEMA, TRIGGER_NAME, EVENT_OBJECT_TABLE, \
                        EVENT_MANIPULATION, ACTION_TIMING, DEFINER \
                 FROM information_schema.TRIGGERS \
                 {} \
                 ORDER BY TRIGGER_SCHEMA, TRIGGER_NAME",
                where_schema("TRIGGER_SCHEMA"),
            ),
            ObjectKind::Events => format!(
                "SELECT EVENT_SCHEMA, EVENT_NAME, DEFINER, STATUS, EVENT_TYPE, \
                        IFNULL(CONCAT(INTERVAL_VALUE,' ',INTERVAL_FIELD),'') AS INTRVL \
                 FROM information_schema.EVENTS \
                 {} \
                 ORDER BY EVENT_SCHEMA, EVENT_NAME",
                where_schema("EVENT_SCHEMA"),
            ),
        }
    }

    fn table_status_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // Storage_factor formula: (data + idx + free) / (data + idx).
        // 1.0× = ideal (no unused space); higher = bloat.
        //
        // Caveat: when `innodb_file_per_table = OFF` (shared tablespace),
        // DATA_FREE reflects the entire shared tablespace and the factor
        // will be misleadingly high. v1 ships as-is — showing red is the
        // correct prompt to investigate. A future iteration could probe
        // `@@innodb_file_per_table` and grey out the metric in shared
        // mode. Non-InnoDB engines emit '' (no DATA_FREE concept).
        format!(
            "SELECT \
                ENGINE                                             AS `Engine`, \
                TABLE_COLLATION                                    AS `Collation`, \
                CAST(TABLE_ROWS AS UNSIGNED)                       AS `Rows`, \
                CAST(DATA_LENGTH AS UNSIGNED)                      AS `Data_length`, \
                CAST(INDEX_LENGTH AS UNSIGNED)                     AS `Index_length`, \
                CAST(AVG_ROW_LENGTH AS UNSIGNED)                   AS `Avg_row_length`, \
                IFNULL(CAST(AUTO_INCREMENT AS CHAR), '')           AS `Auto_increment`, \
                IFNULL(DATE_FORMAT(CREATE_TIME, '%Y-%m-%d %H:%i'), '') AS `Create_time`, \
                IFNULL(DATE_FORMAT(UPDATE_TIME, '%Y-%m-%d %H:%i'), '') AS `Update_time`, \
                IFNULL(TABLE_COMMENT, '')                          AS `Comment`, \
                (SELECT COUNT(*) FROM information_schema.PARTITIONS \
                    WHERE TABLE_SCHEMA = '{schema}' AND TABLE_NAME = '{name}' \
                      AND PARTITION_NAME IS NOT NULL) AS `Partition_count`, \
                CASE \
                    WHEN ENGINE IS NULL OR ENGINE <> 'InnoDB' THEN '' \
                    WHEN (DATA_LENGTH + INDEX_LENGTH) <= 0 THEN '' \
                    /* CAST AS DECIMAL gives a plain decimal string with \
                       no locale thousands separator. FORMAT(n, 2) would \
                       emit '1,234.56' which would fail to parse later. */ \
                    ELSE CAST( \
                        (DATA_LENGTH + INDEX_LENGTH + IFNULL(DATA_FREE, 0)) \
                            / (DATA_LENGTH + INDEX_LENGTH) \
                        AS DECIMAL(10, 2) \
                    ) \
                END                                                AS `Storage_factor`, \
                CAST(DATA_LENGTH + INDEX_LENGTH + IFNULL(DATA_FREE, 0) AS UNSIGNED) AS `Storage_actual_bytes`, \
                /* Page_count is what the storage factor's NUMERATOR is \
                   divided into pages (DATA + IDX + FREE). Page_count_ideal \
                   is the DENOMINATOR (DATA + IDX). Both use 16384 (default \
                   InnoDB page size) — page size can be configured but is \
                   16K in practice for almost everyone. */ \
                CAST( \
                    CEIL((DATA_LENGTH + INDEX_LENGTH + IFNULL(DATA_FREE, 0)) / 16384) \
                    AS UNSIGNED \
                )                                                  AS `Page_count`, \
                CASE \
                    WHEN ENGINE IS NULL OR ENGINE <> 'InnoDB' THEN '' \
                    WHEN (DATA_LENGTH + INDEX_LENGTH) <= 0 THEN '' \
                    ELSE CAST( \
                        CAST(CEIL((DATA_LENGTH + INDEX_LENGTH) / 16384) AS UNSIGNED) \
                        AS CHAR \
                    ) \
                END                                                AS `Page_count_ideal` \
             FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = '{schema}' AND TABLE_NAME = '{name}'",
            schema = esc(schema), name = esc(name),
        )
    }

    fn table_columns_sql(&self, schema: &str, name: &str) -> String {
        format!("SHOW FULL COLUMNS FROM `{}`.`{}`", schema, name)
    }

    fn view_columns_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // MySQL views: emit the same per-column shape as `SHOW FULL
        // COLUMNS` (Field / Type / Null / Default / Extra / Comment)
        // but pulled from `information_schema.COLUMNS` so we can JOIN
        // against `information_schema.VIEWS` for the view's defining
        // SQL. The `Source` column carries that definition.
        //
        // Pre-fix the MySQL plane inherited the trait default which
        // routes view columns through `table_columns_sql` — useful
        // shape, but it loses the view definition entirely. The DDL
        // tab still surfaces `SHOW CREATE VIEW`, but a DBA inspecting
        // the columns list had no signal about what backs the view.
        // Audit cluster D finding 3.10 / issue #21 item 11.
        //
        // VIEW_DEFINITION is the FULL SELECT text — same value per
        // row (a view-level property, not per-column). Truncated to
        // 200 chars in-SQL with an ellipsis so it doesn't blow out
        // the column-grid cell width. DBA who wants the unabridged
        // text reads the DDL tab.
        //
        // (MySQL has no equivalent of PG's `information_schema.
        // view_column_usage` that maps each view column to its
        // underlying base column. Per-column source mapping would
        // require AST-level SELECT-list parsing — separate work, well
        // beyond a plane SQL query.)
        format!(
            "SELECT c.COLUMN_NAME              AS `Field`, \
                    c.COLUMN_TYPE              AS `Type`, \
                    c.IS_NULLABLE              AS `Null`, \
                    IFNULL(c.COLUMN_DEFAULT, '') AS `Default`, \
                    c.EXTRA                    AS `Extra`, \
                    IFNULL(c.COLUMN_COMMENT, '') AS `Comment`, \
                    CASE \
                        WHEN v.VIEW_DEFINITION IS NULL THEN '' \
                        WHEN CHAR_LENGTH(v.VIEW_DEFINITION) > 200 \
                            THEN CONCAT(LEFT(v.VIEW_DEFINITION, 197), '…') \
                        ELSE v.VIEW_DEFINITION \
                    END                        AS `Source` \
             FROM information_schema.COLUMNS c \
             LEFT JOIN information_schema.VIEWS v \
                ON v.TABLE_SCHEMA = c.TABLE_SCHEMA AND v.TABLE_NAME = c.TABLE_NAME \
             WHERE c.TABLE_SCHEMA = '{schema}' AND c.TABLE_NAME = '{name}' \
             ORDER BY c.ORDINAL_POSITION",
            schema = esc(schema), name = esc(name),
        )
    }

    fn table_indexes_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        format!(
            "SELECT \
                s.INDEX_NAME                                          AS `Key_name`, \
                CASE WHEN s.NON_UNIQUE = 0 THEN '0' ELSE '1' END     AS `Non_unique`, \
                s.INDEX_TYPE                                          AS `Index_type`, \
                CAST(COALESCE(ist.stat_value * @@innodb_page_size, 0) AS UNSIGNED) AS `Index_size`, \
                s.COLUMN_NAME                                         AS `Column_name`, \
                s.SEQ_IN_INDEX                                        AS `Seq_in_index` \
             FROM information_schema.STATISTICS s \
             LEFT JOIN mysql.innodb_index_stats ist \
                    ON ist.database_name = s.TABLE_SCHEMA \
                   AND ist.table_name = s.TABLE_NAME \
                   AND ist.index_name = s.INDEX_NAME \
                   AND ist.stat_name = 'size' \
             WHERE s.TABLE_SCHEMA = '{}' AND s.TABLE_NAME = '{}' \
             ORDER BY (CASE WHEN s.INDEX_NAME = 'PRIMARY' THEN 0 ELSE 1 END), s.INDEX_NAME, s.SEQ_IN_INDEX",
            esc(schema), esc(name),
        )
    }

    fn table_ddl_sql(&self, schema: &str, name: &str) -> String {
        format!("SHOW CREATE TABLE `{}`.`{}`", schema, name)
    }

    fn table_relationship_ancestors_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // depth 1 = tables THIS references; depth 2 = tables those parents
        // reference (grand-parents). KEY_COLUMN_USAGE has one row per FK
        // *column* — we GROUP BY constraint to collapse composite FKs into
        // one row, with GROUP_CONCAT(... ORDER BY ORDINAL_POSITION) building
        // the column-name lists in FK declaration order.
        //
        // Depth-2 uses EXISTS rather than JOIN-and-DISTINCT so that an FK
        // owning multiple columns on the focal table doesn't multiply the
        // rows the GROUP_CONCAT sees on the depth-2 side.
        //
        // Output rows are one per (depth, ref-table, via-table) — duplicate
        // edges that reach the same grand-parent through different bridges
        // appear as separate rows on purpose; the ERD draws them all.
        format!(
            "SELECT 1 AS `Depth`, \
                    REFERENCED_TABLE_SCHEMA AS `Ref_schema`, \
                    REFERENCED_TABLE_NAME   AS `Ref_table`, \
                    GROUP_CONCAT(COLUMN_NAME ORDER BY ORDINAL_POSITION SEPARATOR ',') AS `From_columns`, \
                    GROUP_CONCAT(REFERENCED_COLUMN_NAME ORDER BY ORDINAL_POSITION SEPARATOR ',') AS `To_columns`, \
                    '' AS `Via_schema`, \
                    '' AS `Via_table` \
             FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA = '{schema}' AND TABLE_NAME = '{name}' \
               AND REFERENCED_TABLE_NAME IS NOT NULL \
             GROUP BY CONSTRAINT_SCHEMA, CONSTRAINT_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME \
             UNION ALL \
             SELECT 2, \
                    kcu.REFERENCED_TABLE_SCHEMA, \
                    kcu.REFERENCED_TABLE_NAME, \
                    GROUP_CONCAT(kcu.COLUMN_NAME ORDER BY kcu.ORDINAL_POSITION SEPARATOR ','), \
                    GROUP_CONCAT(kcu.REFERENCED_COLUMN_NAME ORDER BY kcu.ORDINAL_POSITION SEPARATOR ','), \
                    kcu.TABLE_SCHEMA, \
                    kcu.TABLE_NAME \
             FROM information_schema.KEY_COLUMN_USAGE kcu \
             WHERE kcu.REFERENCED_TABLE_NAME IS NOT NULL \
               AND EXISTS ( \
                 SELECT 1 FROM information_schema.KEY_COLUMN_USAGE k1 \
                 WHERE k1.TABLE_SCHEMA = '{schema}' AND k1.TABLE_NAME = '{name}' \
                   AND k1.REFERENCED_TABLE_SCHEMA = kcu.TABLE_SCHEMA \
                   AND k1.REFERENCED_TABLE_NAME   = kcu.TABLE_NAME \
               ) \
             GROUP BY kcu.CONSTRAINT_SCHEMA, kcu.CONSTRAINT_NAME, \
                      kcu.TABLE_SCHEMA, kcu.TABLE_NAME, \
                      kcu.REFERENCED_TABLE_SCHEMA, kcu.REFERENCED_TABLE_NAME",
            schema = esc(schema), name = esc(name),
        )
    }

    fn table_relationship_descendants_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // depth 1 = tables that reference THIS; depth 2 = grand-children.
        // See ancestors comment for the GROUP BY / EXISTS rationale. The
        // mirrored direction filters on REFERENCED_TABLE_* and emits the
        // descendant's TABLE_* as `Ref_*`. From_columns is the FK on the
        // descendant side (kcu.COLUMN_NAME), To_columns is the column on
        // THIS table (kcu.REFERENCED_COLUMN_NAME).
        format!(
            "SELECT 1 AS `Depth`, \
                    TABLE_SCHEMA AS `Ref_schema`, \
                    TABLE_NAME   AS `Ref_table`, \
                    GROUP_CONCAT(COLUMN_NAME ORDER BY ORDINAL_POSITION SEPARATOR ',') AS `From_columns`, \
                    GROUP_CONCAT(REFERENCED_COLUMN_NAME ORDER BY ORDINAL_POSITION SEPARATOR ',') AS `To_columns`, \
                    '' AS `Via_schema`, \
                    '' AS `Via_table` \
             FROM information_schema.KEY_COLUMN_USAGE \
             WHERE REFERENCED_TABLE_SCHEMA = '{schema}' AND REFERENCED_TABLE_NAME = '{name}' \
             GROUP BY CONSTRAINT_SCHEMA, CONSTRAINT_NAME, TABLE_SCHEMA, TABLE_NAME \
             UNION ALL \
             SELECT 2, \
                    kcu.TABLE_SCHEMA, \
                    kcu.TABLE_NAME, \
                    GROUP_CONCAT(kcu.COLUMN_NAME ORDER BY kcu.ORDINAL_POSITION SEPARATOR ','), \
                    GROUP_CONCAT(kcu.REFERENCED_COLUMN_NAME ORDER BY kcu.ORDINAL_POSITION SEPARATOR ','), \
                    kcu.REFERENCED_TABLE_SCHEMA, \
                    kcu.REFERENCED_TABLE_NAME \
             FROM information_schema.KEY_COLUMN_USAGE kcu \
             WHERE kcu.REFERENCED_TABLE_NAME IS NOT NULL \
               AND EXISTS ( \
                 SELECT 1 FROM information_schema.KEY_COLUMN_USAGE k1 \
                 WHERE k1.REFERENCED_TABLE_SCHEMA = '{schema}' AND k1.REFERENCED_TABLE_NAME = '{name}' \
                   AND k1.TABLE_SCHEMA = kcu.REFERENCED_TABLE_SCHEMA \
                   AND k1.TABLE_NAME   = kcu.REFERENCED_TABLE_NAME \
               ) \
             GROUP BY kcu.CONSTRAINT_SCHEMA, kcu.CONSTRAINT_NAME, \
                      kcu.TABLE_SCHEMA, kcu.TABLE_NAME, \
                      kcu.REFERENCED_TABLE_SCHEMA, kcu.REFERENCED_TABLE_NAME",
            schema = esc(schema), name = esc(name),
        )
    }

    fn object_ddl_sql(&self, kind: ObjectKind, schema: &str, name: &str) -> String {
        let what = match kind {
            ObjectKind::Tables     => "TABLE",
            ObjectKind::Views      => "VIEW",
            ObjectKind::Procedures => "PROCEDURE",
            ObjectKind::Functions  => "FUNCTION",
            ObjectKind::Triggers   => "TRIGGER",
            ObjectKind::Events     => "EVENT",
        };
        format!("SHOW CREATE {} `{}`.`{}`", what, schema, name)
    }

    fn table_columns_bulk_sql(&self, tables: &[(String, String)]) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        if tables.is_empty() {
            // Return a query that produces zero rows but the right schema.
            // Avoids a parser error from `IN ()` and keeps the column-alias
            // contract intact for the introspect handler.
            return "SELECT '' AS `Schema_`, '' AS `Table_`, '' AS `Field`, \
                          0 AS `Ord`, 0 AS `Is_pk` WHERE 1=0".to_string();
        }
        let tuples = tables.iter()
            .map(|(s, t)| format!("('{}','{}')", esc(s), esc(t)))
            .collect::<Vec<_>>()
            .join(",");
        // LEFT JOIN against KEY_COLUMN_USAGE on the PRIMARY constraint to
        // mark PK columns. KCU has one row per (constraint, column), so
        // multi-column PKs all match; non-PK columns get NULL → Is_pk = 0.
        format!(
            "SELECT c.TABLE_SCHEMA AS `Schema_`, \
                    c.TABLE_NAME   AS `Table_`, \
                    c.COLUMN_NAME  AS `Field`, \
                    c.ORDINAL_POSITION AS `Ord`, \
                    CASE WHEN k.COLUMN_NAME IS NULL THEN 0 ELSE 1 END AS `Is_pk` \
             FROM information_schema.COLUMNS c \
             LEFT JOIN information_schema.KEY_COLUMN_USAGE k \
               ON k.TABLE_SCHEMA   = c.TABLE_SCHEMA \
              AND k.TABLE_NAME     = c.TABLE_NAME \
              AND k.COLUMN_NAME    = c.COLUMN_NAME \
              AND k.CONSTRAINT_NAME = 'PRIMARY' \
             WHERE (c.TABLE_SCHEMA, c.TABLE_NAME) IN ({tuples}) \
             ORDER BY c.TABLE_SCHEMA, c.TABLE_NAME, c.ORDINAL_POSITION",
            tuples = tuples,
        )
    }

    fn update_row_sql(
        &self,
        schema:  &str,
        table:   &str,
        set_col: &str,
        set_val: &str,
        pk_cols: &[String],
        pk_vals: &[String],
    ) -> String {
        let qid = |s: &str| s.replace('`', "``");
        let qval = |v: &str| {
            if v == "NULL" { "NULL".to_string() }
            else { format!("'{}'", v.replace('\'', "''")) }
        };
        let where_parts: Vec<String> = pk_cols.iter().zip(pk_vals.iter())
            .map(|(c, v)| format!("`{}` = {}", qid(c), qval(v)))
            .collect();
        format!(
            "UPDATE `{}`.`{}` SET `{}` = {} WHERE {}",
            qid(schema), qid(table), qid(set_col), qval(set_val),
            where_parts.join(" AND "),
        )
    }

    fn insert_row_sql(
        &self,
        schema: &str,
        table:  &str,
        cols:   &[String],
        vals:   &[String],
    ) -> String {
        let qid = |s: &str| s.replace('`', "``");
        let qval = |v: &str| {
            if v == "NULL" { "NULL".to_string() }
            else { format!("'{}'", v.replace('\'', "''")) }
        };
        if cols.is_empty() {
            // No columns set → ask MySQL to take all defaults.
            return format!(
                "INSERT INTO `{}`.`{}` () VALUES ()",
                qid(schema), qid(table),
            );
        }
        let col_list = cols.iter()
            .map(|c| format!("`{}`", qid(c)))
            .collect::<Vec<_>>()
            .join(", ");
        let val_list = vals.iter()
            .map(|v| qval(v))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO `{}`.`{}` ({}) VALUES ({})",
            qid(schema), qid(table), col_list, val_list,
        )
    }

    fn delete_row_sql(
        &self,
        schema:  &str,
        table:   &str,
        pk_cols: &[String],
        pk_vals: &[String],
    ) -> String {
        let qid = |s: &str| s.replace('`', "``");
        let qval = |v: &str| {
            if v == "NULL" { "NULL".to_string() }
            else { format!("'{}'", v.replace('\'', "''")) }
        };
        let where_parts: Vec<String> = pk_cols.iter().zip(pk_vals.iter())
            .map(|(c, v)| format!("`{}` = {}", qid(c), qval(v)))
            .collect();
        format!(
            "DELETE FROM `{}`.`{}` WHERE {}",
            qid(schema), qid(table),
            where_parts.join(" AND "),
        )
    }

    fn upsert_row_sql(
        &self,
        schema:  &str,
        table:   &str,
        cols:    &[String],
        vals:    &[String],
        pk_cols: &[String],
    ) -> String {
        let qid = |s: &str| s.replace('`', "``");
        let qval = |v: &str| {
            if v == "NULL" { "NULL".to_string() }
            else { format!("'{}'", v.replace('\'', "''")) }
        };
        let pk_lower: std::collections::HashSet<String> = pk_cols.iter()
            .map(|c| c.to_ascii_lowercase()).collect();
        // Non-PK columns the caller filled — these are what we update
        // on duplicate-key.
        let set_cols: Vec<&String> = cols.iter()
            .filter(|c| !pk_lower.contains(&c.to_ascii_lowercase()))
            .collect();

        // Empty cols → empty value list; same DEFAULT-takes-all
        // contract as `insert_row_sql`.
        let col_list = if cols.is_empty() {
            String::new()
        } else {
            cols.iter().map(|c| format!("`{}`", qid(c)))
                .collect::<Vec<_>>().join(", ")
        };
        let val_list = vals.iter().map(|v| qval(v))
            .collect::<Vec<_>>().join(", ");
        let insert_prefix = if cols.is_empty() {
            format!("INSERT INTO `{}`.`{}` () VALUES ()",
                qid(schema), qid(table))
        } else {
            format!("INSERT INTO `{}`.`{}` ({}) VALUES ({})",
                qid(schema), qid(table), col_list, val_list)
        };

        if set_cols.is_empty() {
            // Caller filled only PK columns (or nothing) — fall back
            // to INSERT IGNORE so a duplicate key is silently skipped
            // instead of erroring. Equivalent to PG's `ON CONFLICT
            // DO NOTHING` for this case.
            return insert_prefix.replacen("INSERT INTO", "INSERT IGNORE INTO", 1);
        }
        let set_clause = set_cols.iter()
            .map(|c| format!("`{c}` = VALUES(`{c}`)", c = qid(c)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} ON DUPLICATE KEY UPDATE {}", insert_prefix, set_clause)
    }

    fn count_rows_sql(&self, schema: &str, table: &str, where_clause: Option<&str>) -> String {
        let q = |s: &str| s.replace('`', "``");
        let where_part = where_clause
            .map(|w| w.trim())
            .filter(|w| !w.is_empty())
            .map(|w| {
                let trimmed = w.strip_prefix("WHERE ")
                    .or_else(|| w.strip_prefix("where "))
                    .unwrap_or(w);
                format!(" WHERE {}", trimmed)
            })
            .unwrap_or_default();
        format!("SELECT COUNT(*) AS `count` FROM `{}`.`{}`{}", q(schema), q(table), where_part)
    }

    fn select_paged_sql(
        &self,
        schema:       &str,
        table:        &str,
        limit:        usize,
        offset:       usize,
        order_by:     Option<(&str, bool)>,
        where_clause: Option<&str>,
    ) -> String {
        // Identifier quoting via backticks. Embedded backticks are
        // doubled per MySQL's standard escape. Caller-supplied `schema`,
        // `table`, and `order_by.0` come from catalog data so they're
        // safe-by-shape, but we escape anyway as defense-in-depth.
        let q = |s: &str| s.replace('`', "``");
        let where_part = where_clause
            .map(|w| w.trim())
            .filter(|w| !w.is_empty())
            .map(|w| {
                // Be tolerant of users who paste "WHERE foo=1" vs "foo=1".
                let trimmed = w.strip_prefix("WHERE ")
                    .or_else(|| w.strip_prefix("where "))
                    .unwrap_or(w);
                format!(" WHERE {}", trimmed)
            })
            .unwrap_or_default();
        let order_clause = match order_by {
            Some((col, desc)) => format!(
                " ORDER BY `{}`{}",
                q(col),
                if desc { " DESC" } else { " ASC" }
            ),
            None => String::new(),
        };
        format!(
            "SELECT * FROM `{}`.`{}`{}{} LIMIT {} OFFSET {}",
            q(schema), q(table), where_part, order_clause, limit, offset,
        )
    }

    fn table_toast_sql(&self, _schema: &str, _name: &str) -> String {
        // MySQL has no TOAST — oversize row values live inline (row_format
        // = DYNAMIC / COMPRESSED just changes overflow-page handling
        // internally, no user-visible relation). Return a query that
        // yields zero rows so the TOAST section stays hidden. The stub
        // query still emits all the aliases the PG plane emits so the
        // column-alias contract is technically upheld; the parser
        // dispatches on Row_kind and just doesn't find any.
        "SELECT '' AS `Row_kind`, '' AS `Kind`, \
                '' AS `Toast_schema`, '' AS `Toast_name`, \
                '' AS `Heap_bytes`, '' AS `Index_bytes`, \
                '' AS `Rows_est`, '' AS `Storage_ratio`, \
                '' AS `Partition_count`, '' AS `Total_toast_bytes`, \
                '' AS `Largest_schema`, '' AS `Largest_name`, \
                '' AS `Largest_bytes`, \
                '' AS `Parent_schema`, '' AS `Parent_name`, \
                '' AS `Column_name`, '' AS `Attstorage` \
         WHERE 1 = 0".to_string()
    }

    fn table_vm_sql(&self, _schema: &str, _name: &str, _extensions: &[String]) -> String {
        // MySQL has no visibility map — MVCC via undo logs, not per-page
        // visibility bits. Zero-row stub with matching aliases so the
        // column-alias contract is upheld; the parser sees no rows,
        // produces `None`, and the render suppresses the section.
        "SELECT '' AS `Total_pages`, '' AS `All_visible_pages`, \
                '' AS `All_frozen_pages`, '' AS `Source` \
         WHERE 1 = 0".to_string()
    }

    fn table_autovac_sql(&self, _schema: &str, _name: &str) -> String {
        // MySQL has no analogous per-table autovacuum knob surface.
        // InnoDB's stats + purge are cluster-scoped (via
        // innodb_stats_persistent_sample_pages,
        // innodb_purge_threads, etc.). Zero-row stub with the alias
        // contract preserved so the parser sees no rows, produces
        // None, and the section suppresses.
        "SELECT '' AS `Setting`, '' AS `Effective_value`, \
                '' AS `Source`, '' AS `Reloption_value`, \
                '' AS `Cluster_value` \
         WHERE 1 = 0".to_string()
    }

    fn table_partitions_sql(&self, schema: &str, name: &str) -> String {
        let esc = |s: &str| s.replace('\'', "''");
        // MySQL partitions are storage divisions inside one logical table —
        // not separately addressable like PG declarative partitions, so the
        // renderer surfaces them as plain text (see partitions_are_navigable
        // returning false / default).
        //
        // INFORMATION_SCHEMA.PARTITIONS returns one row per base table even
        // when not partitioned, with PARTITION_NAME = NULL. Filter those out
        // so the section stays hidden for non-partitioned tables.
        //
        // Reconstruct the bound to match SHOW CREATE TABLE syntax. RANGE /
        // LIST (and their COLUMNS variants) carry the values in
        // PARTITION_DESCRIPTION; HASH and KEY (plus LINEAR HASH / LINEAR
        // KEY, which arrive as space-separated strings) have no per-
        // partition values — the modulus is implied by the partition
        // count — so they fall through to the empty bound.
        format!(
            "SELECT TABLE_SCHEMA   AS `Partition_schema`, \
                    PARTITION_NAME AS `Partition_name`, \
                    CASE PARTITION_METHOD \
                        WHEN 'RANGE'         THEN CONCAT('VALUES LESS THAN (', PARTITION_DESCRIPTION, ')') \
                        WHEN 'RANGE COLUMNS' THEN CONCAT('VALUES LESS THAN (', PARTITION_DESCRIPTION, ')') \
                        WHEN 'LIST'          THEN CONCAT('VALUES IN (', PARTITION_DESCRIPTION, ')') \
                        WHEN 'LIST COLUMNS'  THEN CONCAT('VALUES IN (', PARTITION_DESCRIPTION, ')') \
                        ELSE '' \
                    END AS `Partition_bound`, \
                    CAST(IFNULL(TABLE_ROWS, 0)  AS UNSIGNED) AS `Partition_rows`, \
                    CAST(IFNULL(DATA_LENGTH, 0) AS UNSIGNED) AS `Partition_data_length` \
             FROM information_schema.PARTITIONS \
             WHERE TABLE_SCHEMA = '{}' AND TABLE_NAME = '{}' \
               AND PARTITION_NAME IS NOT NULL \
             ORDER BY PARTITION_ORDINAL_POSITION",
            esc(schema), esc(name),
        )
    }

    fn keywords(&self) -> &'static [&'static str] { MYSQL_KEYWORDS }
    fn functions(&self) -> &'static [&'static str] { MYSQL_FUNCTIONS }
    fn meta_command_names(&self) -> &'static [&'static str] { MYSQL_META_COMMANDS }
    fn completion_columns_sql(&self, schema: &str, table: &str) -> String {
        let s = schema.replace('\'', "''");
        let t = table.replace('\'',  "''");
        format!(
            "SELECT COLUMN_NAME AS `Column_` \
               FROM information_schema.COLUMNS \
              WHERE TABLE_SCHEMA = '{}' AND TABLE_NAME = '{}' \
              ORDER BY ORDINAL_POSITION",
            s, t,
        )
    }
    fn completion_tables_sql(&self) -> String {
        // Across every database the connected user can see. Includes
        // `information_schema`, `performance_schema`, `mysql`, `sys` —
        // useful for completion when the user types the system table
        // name they want.
        "SELECT TABLE_SCHEMA AS `Schema_`, TABLE_NAME AS `Table_` \
           FROM information_schema.TABLES \
          ORDER BY 1, 2".into()
    }
    fn lexicon(&self) -> Lexicon { Lexicon::Mysql }
    fn sql_dialect(&self) -> &'static dyn sqlparser::dialect::Dialect {
        &sqlparser::dialect::MySqlDialect {}
    }
    fn empty_set(&self) -> Option<&'static str> { Some("Empty set") }
    fn empty_statement(&self) -> Option<&'static [&'static str]> { Some(&NO_QUERY) }
    fn keeps_unchanged_recall(&self) -> bool { true }

    /// The mysql CLI's `old_buffer`, which `com_go` and its server help
    /// copy their buffer to: empty after an empty statement it answered,
    /// or `go` / `ego` typed alone — `com_go` copies the empty buffer
    /// before "No query specified" — or a help typed alone (`help
    /// select`, `\h select`: the buffer is empty), the statement after
    /// one typed as a statement (`help select;`). A help without a topic
    /// lists the commands and leaves it.
    fn previous_buffer_after(&self, command: &str, terminator: &str) -> Option<String> {
        let runs_the_buffer = find_named(past_comments_and_space(command))
            .is_some_and(|(_, letter, _)| matches!(letter, Some('g' | 'G')));
        if command.is_empty() || runs_the_buffer {
            return Some(String::new());
        }
        asks_server_help(command).then(|| if terminator.is_empty() { String::new() } else { command.to_string() })
    }

    /// The mysql CLI's `print` (`com_print`): its buffer — every line
    /// since the last statement ended, comments and blank lines kept, as
    /// `add_line` keeps them — between two rules, and a blank line. Not
    /// the space after the last statement's delimiter, which went with
    /// it, nor an empty line with nothing pending; no previous query (the
    /// buffer is empty once a statement has run).
    fn print_buffer(&self, typed: &str, _previous: Option<&str>, _ctx: &super::MetaContext) -> Vec<String> {
        const RULE: &str = "--------------";
        let buffer = typed.trim_start_matches([' ', '\t']).trim_start_matches('\n');
        let buffer = buffer.strip_suffix('\n').unwrap_or(buffer);
        std::iter::once(RULE).chain(buffer.split('\n')).chain([RULE, ""]).map(String::from).collect()
    }
    fn line_notes(&self, input: &str, delimiter: &str) -> Vec<crate::ShellEntry> {
        // A line without a backslash has no command to note: the lines
        // before it are read only for one that has.
        if !input.rsplit('\n').next().unwrap_or_default().contains('\\') {
            return Vec::new();
        }
        parser::mysql_unknown_commands(input, delimiter).into_iter()
            .map(|c| {
                let message = format!("Unknown command '\\{c}'.");
                crate::ShellEntry::CliError { lines: vec!["ERROR: ".into(), message.clone()], message }
            })
            .collect()
    }
    /// The mysql CLI's `\G` (`print_table_data_vertically`): a
    /// `*** N. row ***` line over each row, then each column's name —
    /// right-justified to the longest, by bytes, as its `%*s` pads — and
    /// value as it is (a zero byte a space); a null reads `NULL`.
    fn print_vertical(&self, columns: &[String], rows: &[Vec<String>], _style: &crate::types::PrintedWith) -> Vec<String> {
        let width = columns.iter().map(String::len).max().unwrap_or(0);
        let mut lines = Vec::new();
        for (n, row) in rows.iter().enumerate() {
            lines.push(format!("*************************** {}. row ***************************", n + 1));
            for (name, value) in columns.iter().zip(row) {
                let field = format!("{}{name}: {}", " ".repeat(width - name.len()), value.replace('\0', " "));
                lines.extend(field.split('\n').map(String::from));
            }
        }
        lines
    }
    /// The mysql CLI right-justifies a numeric column (`IS_NUM`): the
    /// integer, decimal and floating-point types, YEAR, and NULL's own
    /// type — by sqlx's name for each, which keeps those apart
    /// (`tinyint(1)` is `BOOLEAN`, still TINY to the CLI).
    fn column_align(&self, type_id: &str) -> crate::types::Align {
        let base = type_id.strip_suffix(" UNSIGNED").unwrap_or(type_id);
        match base {
            "TINYINT" | "BOOLEAN" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT"
            | "FLOAT" | "DOUBLE" | "DECIMAL" | "YEAR" | "NULL" => crate::types::Align::Right,
            _ => crate::types::Align::Left,
        }
    }
    fn rows_footer(&self, rows: usize, warnings: u16, _vertical: bool) -> Option<String> {
        let plural = |n: u64| if n == 1 { "" } else { "s" };
        let mut footer = match rows {
            0 => "Empty set".to_string(),
            n => format!("{n} row{} in set", plural(n as u64)),
        };
        if warnings > 0 {
            footer.push_str(&format!(", {warnings} warning{}", plural(u64::from(warnings))));
        }
        Some(footer)
    }
    fn timing(&self, exec_time_us: u64) -> Timing {
        Timing::Inline(format!("({}.{:03} sec)", exec_time_us / 1_000_000, exec_time_us / 1000 % 1000))
    }
    /// The mysql CLI's `use` (com_use) — typed, or a statement of a
    /// batch — says "Database changed" where the server's OK would read
    /// "Query OK", and becomes the CLI's current database.
    fn switches_database(&self, sql: &str) -> Option<(String, &'static str)> {
        use sqlparser::ast::{Statement, Use};
        let statements = sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::MySqlDialect {}, sql).ok()?;
        match statements.as_slice() {
            [Statement::Use(Use::Object(name))] => {
                Some((name.0.last()?.as_ident()?.value.clone(), "Database changed"))
            }
            _ => None,
        }
    }
    fn lost_connection(&self) -> LostConnection {
        LostConnection::OnNextStatement {
            error:  "ERROR 2013 (HY000): Lost connection to MySQL server during query",
            notice: "No connection. Trying to reconnect...",
        }
    }
    /// `com_connect`'s lines, after its reconnect or a `connect` (a blank
    /// one after them: `Current database: %s\n`); `reconnect`'s when the
    /// new session couldn't be had.
    fn reconnected_notice(&self, ok: bool, connection_id: &str, database: &str) -> Vec<String> {
        if !ok {
            return vec!["ERROR: ".into(), "Can't connect to the server".into()];
        }
        let database = if database.is_empty() { "*** NONE ***" } else { database };
        vec![format!("Connection id:    {connection_id}"), format!("Current database: {database}"), String::new()]
    }
    fn completion_notice(&self) -> Vec<String> {
        vec![
            "Reading table information for completion of table and column names".into(),
            "You can turn off this feature to get a quicker startup with -A".into(),
            String::new(),
        ]
    }
    /// A `connect` (or a reconnect) the server refused: its error, as the
    /// CLI prints one (`ERROR 1049 (42000): Unknown database 'x'`); the
    /// connection's own failure as noctua words it.
    fn connect_failed(&self, _host: &str, _port: &str, error: Option<&shared::DbError>, message: &str, _kept: bool) -> Vec<String> {
        match error {
            Some(error) => self.format_error(error, None, ErrorStyle::default()),
            None => vec![message.to_string()],
        }
    }
    fn command_status(&self, status: &shared::CommandStatus, _format: crate::types::OutputFormat) -> Vec<String> {
        let plural = |n: u64| if n == 1 { "" } else { "s" };
        let n = status.rows_affected;
        let mut first = format!("Query OK, {n} row{} affected", plural(n));
        if status.warnings > 0 {
            let w = u64::from(status.warnings);
            first.push_str(&format!(", {w} warning{}", plural(w)));
        }
        std::iter::once(first).chain(status.info.clone()).collect()
    }

    // ── Activity pane ──────────────────────────────────────────────────

    fn supported_panes(&self) -> &'static [PaneMenuItem] {
        &[
            PaneMenuItem { kind: PaneKind::SchemaExplorer, label: "Schema Explorer", icon: "⊞" },
            PaneMenuItem { kind: PaneKind::Activity,       label: "Activity",        icon: "⚡" },
        ]
    }

    fn sessions_sql(&self, hide_idle: bool) -> String {
        // We expose MySQL's COMMAND column as "State" so the sessions table
        // column header lines up with Postgres's pg_stat_activity.state. The
        // two aren't perfectly synonymous — MySQL's finer-grained STATE column
        // is often empty for idle sessions, while COMMAND ("Query", "Sleep",
        // "Connect", …) is always populated and closer in spirit to PG's
        // state enum. Revisit if we want to surface both columns later.
        // "Hide idle" is focus mode — strip Sleep sessions (unless they're
        // blocking, which makes them the root cause of someone else's wait)
        // AND hide the monitoring session itself. Unchecked is full
        // transparency: show everything, including ourselves.
        let (self_filter, idle_filter) = if hide_idle {
            (
                "AND ID <> CONNECTION_ID()",
                "AND (COMMAND <> 'Sleep' OR ID IN (SELECT blocking_pid FROM sys.innodb_lock_waits))",
            )
        } else {
            ("", "")
        };
        format!(
            "SELECT \
                ID                        AS `Pid`, \
                COALESCE(USER, '')        AS `User`, \
                COALESCE(HOST, '')        AS `Client`, \
                COALESCE(DB, '')          AS `DB`, \
                COALESCE(COMMAND, '')     AS `State`, \
                IFNULL(TIME, 0)           AS `Time`, \
                COALESCE(INFO, '')        AS `Query` \
             FROM information_schema.processlist \
             WHERE 1=1 {} {} \
             ORDER BY TIME DESC",
            self_filter, idle_filter,
        )
    }

    fn blockers_sql(&self) -> String {
        // sys.innodb_lock_waits packages waiter/blocker pairs ready to use.
        // Available on MySQL 5.7+ / MariaDB with sys schema installed.
        "SELECT \
            waiting_pid  AS `Waiter_pid`, \
            blocking_pid AS `Blocker_pid` \
         FROM sys.innodb_lock_waits"
            .into()
    }

    fn locks_sql(&self, server_version: &str) -> String {
        // MySQL 8.0+ uses `performance_schema.data_locks`. The 5.x
        // series doesn't have that view — the equivalent lives in
        // `information_schema.INNODB_LOCKS` (joined with INNODB_TRX
        // to recover the connection thread id). Pre-fix the plane
        // unconditionally used data_locks and 5.7 users hit a hard
        // error on the Locks tab. Audit cluster D finding 3.6 /
        // issue #21 item 6.
        //
        // Version detection: parse the leading major-number from
        // `VERSION()`. Examples: "8.0.32" → 8, "5.7.39" → 5,
        // "5.7.39-log" → 5. MariaDB (`"10.6.7-MariaDB"`) returns
        // None from the helper and lands on the empty-shape branch
        // below — MariaDB doesn't have `performance_schema.data_locks`
        // OR `information_schema.INNODB_LOCKS` (its lock-monitoring
        // surface is different: `information_schema.METADATA_LOCK_INFO`,
        // which is a separate plane-extension question outside #21).
        // Anything else we can't parse falls into the 8.0+ path —
        // the modern source — because that's the more likely target
        // for unrecognized version strings (custom builds, future
        // 9.x, etc.).
        match mysql_server_major_version(server_version) {
            // 5.x branch. `INNODB_LOCKS.LOCK_TABLE` arrives backtick-
            // quoted (`` `db`.`tbl` ``) — strip the backticks to
            // match the `schema.table` shape the 8.0 path emits.
            // (Pathological corner: identifiers with EMBEDDED
            // backticks lose those too. Acceptable for monitoring
            // surface; killable Pid still works.)
            //
            // Granted check: a LEFT JOIN against `INNODB_LOCK_WAITS`
            // turns "is this trx waiting" into a NULL-safe predicate
            // — `w.requesting_trx_id IS NULL` means "not in the
            // waiters table, so the lock is granted." The pre-fix
            // first draft used `NOT IN (subquery)` which falls into
            // MySQL's well-known NULL trap (any NULL in the subquery
            // result poisons every outer row to "unknown") and also
            // costs re-evaluation per outer row when the optimizer
            // can't hoist the uncorrelated form.
            //
            // Privilege: rows from these three I_S views require the
            // PROCESS privilege to see locks held by OTHER users.
            // A non-PROCESS user gets an empty result set, not a
            // permission error — so the Locks tab will read "no
            // locks" even when other users are blocked. Documented
            // here so a DBA seeing an always-empty pane knows where
            // to look.
            //
            // Background trx (recovery / undo purge) have
            // `trx_mysql_thread_id` NULL — the LEFT JOIN keeps the
            // lock row visible (it's still a real lock-holder a DBA
            // needs to see) and the Pid renders blank, which
            // correctly disables the kill affordance for unkillable
            // background work.
            Some(major) if major < 8 => {
                "SELECT \
                    CAST(t.trx_mysql_thread_id AS CHAR)              AS `Pid`, \
                    l.LOCK_MODE                                      AS `Mode`, \
                    REPLACE(l.LOCK_TABLE, '`', '')                   AS `Object`, \
                    (w.requesting_trx_id IS NULL)                    AS `Granted`, \
                    ''                                               AS `Duration` \
                 FROM information_schema.INNODB_LOCKS l \
                 LEFT JOIN information_schema.INNODB_TRX t \
                    ON l.LOCK_TRX_ID = t.trx_id \
                 LEFT JOIN information_schema.INNODB_LOCK_WAITS w \
                    ON l.LOCK_TRX_ID = w.requesting_trx_id"
                    .into()
            }
            // MariaDB (`None` from the helper): neither system view
            // exists, so neither branch's SQL would parse. Emit an
            // always-empty result of the right shape — the Locks
            // tab renders cleanly as "no locks" until somebody adds
            // a dedicated MariaDB plane (or extends this one with a
            // METADATA_LOCK_INFO path).
            None => {
                "SELECT \
                    '' AS `Pid`, \
                    '' AS `Mode`, \
                    '' AS `Object`, \
                    0  AS `Granted`, \
                    '' AS `Duration` \
                 WHERE 1 = 0"
                    .into()
            }
            // 8.0+ branch. `performance_schema.data_locks` is the
            // modern source, available since 8.0. THREAD_ID is the
            // connection thread (same shape as 5.x's
            // trx_mysql_thread_id).
            Some(_) => {
                "SELECT \
                    CAST(THREAD_ID AS CHAR)                          AS `Pid`, \
                    LOCK_MODE                                        AS `Mode`, \
                    CONCAT(OBJECT_SCHEMA, '.', OBJECT_NAME)          AS `Object`, \
                    (LOCK_STATUS = 'GRANTED')                        AS `Granted`, \
                    ''                                               AS `Duration` \
                 FROM performance_schema.data_locks"
                    .into()
            }
        }
    }

    fn kill_query_sql(&self, pid: &str) -> String {
        // PID is always numeric here — the parser returns it as the string form
        // of the server's thread id. KILL QUERY cancels the running statement
        // without tearing down the session.
        format!("KILL QUERY {}", pid)
    }
}

/// Quote a MySQL identifier (schema / table / column) with backticks
/// and embedded `` ` `` doubled.
fn my_quote_ident(s: &str) -> String {
    format!("`{}`", s.replace('`', "``"))
}

/// Parse the major version from a MySQL `VERSION()` string. Handles
/// the common shapes seen in the wild:
///
/// - `"8.0.32"`         → `Some(8)`
/// - `"5.7.39"`         → `Some(5)`
/// - `"5.7.39-log"`     → `Some(5)` (the trailing `-log` is the suffix
///                        for a binary-log-enabled build; common on
///                        managed-MySQL providers)
/// - `"8.0.32-cluster"` → `Some(8)`
/// - `""` / `"unknown"` → `None`
/// - `"10.6.7-MariaDB"` → `None` (MariaDB is a different RDBMS that
///                        speaks the MySQL wire protocol but doesn't
///                        share `performance_schema.data_locks` OR
///                        `information_schema.INNODB_LOCKS`. Callers
///                        in this plane treat `None` as the "I don't
///                        know how to talk to this server's lock
///                        catalog" sentinel and produce an empty-
///                        shape result rather than a hard error.)
///
/// For non-MariaDB cases where parsing fails, callers default to the
/// 8.x path — falling back to a deprecated info-schema view by
/// default would silently drop 8.0 features for users whose version
/// string is unrecognized (Aurora variants, future 9.x, etc.).
fn mysql_server_major_version(version: &str) -> Option<u8> {
    if version.contains("MariaDB") {
        return None;
    }
    version
        .split(|c: char| c == '.' || c == '-')
        .next()?
        .trim()
        .parse::<u8>()
        .ok()
}

#[cfg(test)]
mod version_tests {
    use super::mysql_server_major_version;

    #[test]
    fn modern_8x() {
        assert_eq!(mysql_server_major_version("8.0.32"), Some(8));
        assert_eq!(mysql_server_major_version("8.4.1"),  Some(8));
    }

    #[test]
    fn legacy_5x() {
        assert_eq!(mysql_server_major_version("5.7.39"), Some(5));
        assert_eq!(mysql_server_major_version("5.6.51"), Some(5));
    }

    #[test]
    fn binary_log_suffix_does_not_break_parse() {
        // Common on managed-MySQL providers: VERSION() returns
        // `5.7.39-log` to indicate a binlog-enabled build. The
        // pre-fix parser would also have to handle this — but the
        // pre-fix didn't parse the version at all.
        assert_eq!(mysql_server_major_version("5.7.39-log"), Some(5));
        assert_eq!(mysql_server_major_version("8.0.32-cluster"), Some(8));
    }

    #[test]
    fn unknown_or_empty_returns_none() {
        assert_eq!(mysql_server_major_version(""),         None);
        assert_eq!(mysql_server_major_version("unknown"),  None);
    }

    /// MariaDB shares the MySQL wire protocol so users commonly
    /// connect via the MySql plane. But MariaDB doesn't have
    /// `performance_schema.data_locks` OR
    /// `information_schema.INNODB_LOCKS`, so neither branch's SQL
    /// would parse. The helper returns `None` and `locks_sql`'s
    /// MariaDB branch emits an always-empty result of the right
    /// shape, so the Locks tab renders cleanly instead of hard-
    /// erroring.
    #[test]
    fn mariadb_returns_none() {
        assert_eq!(mysql_server_major_version("10.6.7-MariaDB"),          None);
        assert_eq!(mysql_server_major_version("5.5.5-10.11.2-MariaDB"),   None);
        assert_eq!(mysql_server_major_version("11.0.0-MariaDB-1:11.0.0+maria~ubu2204"), None);
    }
}

/// Split `user@host` and re-emit as MySQL's quoted grantee literal:
/// `'user'@'host'`. Usernames can contain `@`; hostnames can't, so we
/// split on the LAST `@`. Falls back to `'<whole_string>'@'%'` if no
/// `@` is present (defensive — shouldn't happen with current row
/// shapes).
fn my_quote_grantee(g: &str) -> String {
    let (user, host) = match g.rsplit_once('@') {
        Some((u, h)) => (u, h),
        None => (g, "%"),
    };
    let uq = user.replace('\'', "''");
    let hq = host.replace('\'', "''");
    format!("'{}'@'{}'", uq, hq)
}

/// Shared MySQL GRANT/REVOKE builder. Routine grants use `ON
/// PROCEDURE` or `ON FUNCTION` (no `ON ROUTINE` in MySQL).
fn my_grant_or_revoke(
    verb: &str,
    prep: &str,
    kind: crate::ObjectKind,
    schema: &str,
    name: &str,
    grantee: &str,
    privilege: &str,
) -> Option<String> {
    use crate::ObjectKind;
    let on_clause = match kind {
        ObjectKind::Tables | ObjectKind::Views => "TABLE",
        ObjectKind::Procedures => "PROCEDURE",
        ObjectKind::Functions  => "FUNCTION",
        _ => return None,
    };
    let s = my_quote_ident(schema);
    let n = my_quote_ident(name);
    let g = my_quote_grantee(grantee);
    // MySQL accepts privilege names case-insensitively; upper-case
    // is the conventional form and matches our row vocabulary.
    let priv_upper = privilege.to_uppercase();
    Some(format!(
        "{verb} {priv} ON {on_clause} {s}.{n} {prep} {g}",
        verb = verb, priv = priv_upper, on_clause = on_clause,
        s = s, n = n, prep = prep, g = g,
    ))
}

impl MySqlPlane {
    /// What the mysql CLI's `status` (`\s`) shows, from the server.
    fn status_sql(&self) -> String {
        "SELECT \
          connection_id()               AS `Connection id`, \
          user()                        AS `Current user`, \
          IFNULL(database(), '(none)')  AS `Current database`, \
          @@version                     AS `Server version`, \
          @@version_comment             AS `Server version comment`, \
          @@hostname                    AS `Server host`, \
          @@port                        AS `TCP port`, \
          @@character_set_client        AS `Client characterset`, \
          @@character_set_server        AS `Server characterset`, \
          @@collation_server            AS `Server collation`, \
          (SELECT VARIABLE_VALUE FROM performance_schema.global_status WHERE VARIABLE_NAME = 'Uptime') AS `Uptime (seconds)`, \
          (SELECT VARIABLE_VALUE FROM performance_schema.global_status WHERE VARIABLE_NAME = 'Threads_connected') AS `Threads connected`"
        .into()
    }

    /// What the mysql CLI's command `\letter` runs with `arguments`
    /// (`name` is how it was typed, `\W` or `warnings`, for what this
    /// shell says of one it doesn't run yet). `\g` / `\G` end a
    /// statement: the splitter's (None).
    fn letter_command(&self, letter: char, arguments: &str, name: &str) -> Option<MetaAction> {
        // `get_arg`'s first word, a `;` in it its own (`\u sakila;` is
        // database `sakila;`).
        let word = arguments.split(char::is_whitespace).next().unwrap_or_default();
        match letter {
            'q' => Some(MetaAction::Disconnect),
            '?' | 'h' if arguments.is_empty() => Some(MetaAction::Output(self.help_lines())),
            // The server's help on the topic (`com_server_help`).
            '?' | 'h' => Some(MetaAction::Staged(vec![server_help_query(arguments)])),
            's' => Some(MetaAction::Introspect { sql: self.status_sql(), tag: "pretty_kv".into() }),
            // \c — clear the input buffer (native mysql behavior).
            'c' => Some(MetaAction::ClearInput(None)),
            'd' => Some(set_delimiter(arguments)),
            'u' if word.is_empty() => Some(MetaAction::Output(USE_WITHOUT_DATABASE.iter().map(|l| l.to_string()).collect())),
            // `mysql_select_db` on the word as it is: quoted, so a `;` in
            // it is the name's. One typed with backticks goes as typed.
            'u' if word.contains('`') => Some(MetaAction::RunSql(format!("USE {word}"), false)),
            'u' => Some(MetaAction::RunSql(format!("USE `{word}`"), false)),
            // \e — edit the buffer in $EDITOR. It takes no arguments:
            // what follows it is the buffer's again.
            'e' => Some(MetaAction::EditExternal { seed: None, line: None }),
            'g' | 'G' => None,
            // `nopager`: a pane pages nothing — stdout is all it has.
            'n' => Some(MetaAction::Output(vec!["PAGER set to stdout".into()])),
            // `notee`: no outfile here to close; said as the CLI says it.
            't' => Some(MetaAction::Output(vec!["Outfile disabled.".into()])),
            // `rehash`: completion's names fetched afresh, silently.
            '#' => Some(MetaAction::RebuildCompletion),
            // `warnings` / `nowarning`: the server's warnings after each
            // statement (`print_warnings`), or not.
            'W' => Some(MetaAction::ShowWarnings { on: true, said: "Show warnings enabled.".into() }),
            'w' => Some(MetaAction::ShowWarnings { on: false, said: "Show warnings disabled.".into() }),
            // `print`: the buffer between rules (`print_buffer`).
            'p' => Some(MetaAction::PrintBuffer),
            // `connect [db [host]]`: a new session (`com_connect`), its
            // arguments read as `get_arg` reads them.
            'r' => {
                let (database, rest) = get_arg(arguments).map_or((None, ""), |(database, rest)| (Some(database), rest));
                let host = database.as_ref().and_then(|_| get_arg(rest)).map(|(host, _)| host);
                Some(MetaAction::Connect { database, host })
            }
            // Its other commands (`\R`, `\T`, `\C` …) this shell doesn't
            // run yet; anything else isn't one of its
            // (`parser::mysql_unknown_commands`).
            _ if parser::is_mysql_command(letter) => Some(not_supported(name)),
            _ => None,
        }
    }

    /// What the mysql CLI's named command `name` runs: what its letter
    /// runs, with the `arguments` after its name. `use` with a backtick
    /// sends the text it was typed in (`com_use`'s `normalize_dbname`:
    /// ``use `my db` ``).
    fn named(&self, name: &str, letter: Option<char>, text: &str, arguments: &str) -> Option<MetaAction> {
        match letter {
            Some('u') if text.contains('`') => Some(MetaAction::RunSql(text.trim_end().to_string(), false)),
            Some(letter) => self.letter_command(letter, arguments, name),
            None => Some(not_supported(name)),
        }
    }
}

/// The mysql CLI's `com_server_help`: the server's `HELP` statement for
/// `topic` — quoted as the CLI quotes it (`help 'select'`, one typed
/// with its quote as it is), the topic as typed, and cut to the CLI's
/// 101 bytes (`cmd_buf`: a longer topic loses its closing quote).
fn server_help_query(topic: &str) -> String {
    let mut query = if topic.starts_with('\'') { format!("help {topic}") } else { format!("help '{}'", topic.trim_end()) };
    if query.len() > 101 {
        let mut cut = 101;
        while !query.is_char_boundary(cut) { cut -= 1; }
        query.truncate(cut);
    }
    query
}

/// Whether `command` asks the server for help on a topic, as the mysql
/// CLI's `com_help` hands it to `com_server_help`: `help select`, `\h
/// select`, `? select` — not `help` alone, which lists the commands.
fn asks_server_help(command: &str) -> bool {
    let text = past_comments_and_space(command);
    let letter = match text.strip_prefix('\\') {
        Some(rest) => rest.chars().next(),
        None => find_named(text).and_then(|(_, letter, _)| letter),
    };
    matches!(letter, Some('h' | '?')) && !help_topic(text).is_empty()
}

/// What `com_help` takes for the topic of `command` (`\h select`, `help
/// select`, `? select`, past the comments it may start with): what
/// follows its first space.
fn help_topic(command: &str) -> &str {
    past_comments_and_space(command).split_once(char::is_whitespace).map_or("", |(_, topic)| topic.trim())
}

/// What the mysql CLI prints of the server's answer to `query`
/// (`com_server_help`): one topic — its name, description and any
/// examples; several items, or a category's contents — a list, under
/// `categories:` / `topics:` as they change (`print_help_item`); none —
/// "Nothing found" and where to look.
fn server_help_lines(query: &str, set: Option<&shared::ResultSet>) -> Vec<String> {
    let rows = set.map_or(&[][..], |set| &set.rows[..]);
    let fields = set.map_or(0, |set| set.columns.len());
    let cell = |row: &[Option<String>], i: usize| row.get(i).cloned().flatten().unwrap_or_default();
    let mut text = String::new();
    if fields == 3 && rows.len() == 1 {
        let row = &rows[0];
        text.push_str(&format!("Name: '{}'\nDescription:\n{}", cell(row, 0), cell(row, 1)));
        let examples = cell(row, 2);
        if !examples.is_empty() {
            text.push_str(&format!("Examples:\n{examples}"));
        }
        text.push('\n');
    } else if fields >= 2 && !rows.is_empty() {
        let (name, category) = if fields == 2 {
            text.push_str("Many help items for your request exist.\n");
            text.push_str("To make a more specific request, please type 'help <item>',\nwhere <item> is one of the following\n");
            (0, 1)
        } else {
            text.push_str(&format!("You asked for help about help category: \"{}\"\n", cell(&rows[0], 0)));
            text.push_str("For more information, type 'help <item>', where <item> is one of the following\n");
            (1, 2)
        };
        let mut last = '\0';
        for row in rows {
            let kind = cell(row, category).chars().next().unwrap_or('\0');
            if kind != last {
                text.push_str(if kind == 'Y' { "categories:\n" } else { "topics:\n" });
                last = kind;
            }
            text.push_str(&format!("   {}\n", cell(row, name)));
        }
        text.push('\n');
    } else {
        text.push_str("\nNothing found\n");
        if query.get(..15).is_some_and(|q| q.eq_ignore_ascii_case("help 'contents'")) {
            text.push_str("\nPlease check if 'help tables' are loaded.\n\n");
        } else {
            text.push_str("Please try to run 'help contents' for a list of all accessible topics\n\n");
        }
    }
    let text = text.strip_suffix('\n').unwrap_or(&text);
    text.split('\n').map(String::from).collect()
}

/// MySQL keywords — must stay sorted (binary_search depends on it).
/// Meta-command names recognized by the MySQL plane's `meta_cmd`,
/// sorted lexicographically (ASCII). See the PG plane's note — same
/// contract: leading `\` included, MUST stay in sync with match arms.
static MYSQL_META_COMMANDS: &[&str] = &[
    "\\?",
    "\\c",
    "\\d",
    "\\e",
    "\\h",
    "\\q",
    "\\s", "\\safe-mode",
    "\\u",
];

static MYSQL_KEYWORDS: &[&str] = &[
    "ABS", "ADD", "ALL", "ALTER", "ANALYZE", "AND", "ANY",
    "AS", "ASC", "AUTO_INCREMENT",
    "BEGIN", "BETWEEN", "BIGINT", "BIT", "BLOB", "BOOL", "BOOLEAN",
    "BY",
    "CALL", "CASE", "CHANGE", "CHAR", "CHARACTER", "CHARSET", "CHECK",
    "COLLATE", "COLUMN", "COLUMNS", "COMMENT", "COMMIT", "CONSTRAINT",
    "CONVERT", "CREATE", "CROSS", "CURRENT",
    "DATA", "DATABASE", "DATABASES", "DATE", "DATETIME", "DECIMAL",
    "DECLARE", "DEFAULT", "DELAYED", "DELETE", "DESC", "DESCRIBE",
    "DISTINCT", "DOUBLE", "DROP",
    "ELSE", "ELSEIF", "END", "ENGINE", "ENUM", "EXCEPT", "EXISTS",
    "EXPLAIN",
    "FALSE", "FETCH", "FILTER", "FIRST", "FLOAT", "FOLLOWING", "FOR",
    "FOREIGN", "FROM", "FULL", "FUNCTION",
    "GEOMETRY", "GRANT", "GROUP",
    "HAVING", "HIGH_PRIORITY",
    "IF", "IGNORE", "IN", "INDEX", "INFILE", "INNER", "INOUT",
    "INSERT", "INT", "INTEGER", "INTERSECT", "INTO", "IS", "ITERATE",
    "JOIN", "JSON",
    "KEY", "KILL",
    "LATERAL", "LEAVE", "LEFT", "LIKE", "LIMIT", "LOAD", "LOCK",
    "LONGBLOB", "LONGTEXT", "LOOP", "LOW_PRIORITY",
    "MATCH", "MEDIUMBLOB", "MEDIUMINT", "MEDIUMTEXT", "MODIFY",
    "NOT", "NOW", "NULL", "NUMERIC",
    "OF", "ON", "ONLY", "OPTIMIZE", "OR", "ORDER", "OUT", "OUTER",
    "OUTFILE", "OVER",
    "PARTITION", "PRECEDING", "PRIMARY", "PROCEDURE",
    "READ", "REAL", "RECURSIVE", "REFERENCES", "RELEASE", "RENAME",
    "REPAIR", "REPEAT", "REPLACE", "RETURN", "RETURNS", "REVOKE",
    "RIGHT", "RLIKE", "ROLLBACK", "ROW", "ROWS",
    "SAVEPOINT", "SCHEMA", "SCHEMAS", "SELECT", "SET", "SHOW",
    "SIGNED", "SMALLINT", "SOME", "START", "STRAIGHT_JOIN",
    "TABLE", "TABLES", "TEMPORARY", "TEXT", "THEN", "TIME",
    "TIMESTAMP", "TINYBLOB", "TINYINT", "TINYTEXT", "TO",
    "TRANSACTION", "TRIGGER", "TRUE", "TRUNCATE",
    "UNBOUNDED", "UNION", "UNIQUE", "UNKNOWN", "UNSIGNED", "UPDATE",
    "USE", "USING",
    "VALUES", "VARBINARY", "VARCHAR", "VIEW",
    "WHEN", "WHERE", "WHILE", "WINDOW", "WITH", "WRITE",
    "YEAR",
    "ZEROFILL",
];

/// MySQL built-in functions — must stay sorted.
static MYSQL_FUNCTIONS: &[&str] = &[
    "ADDDATE", "ADDTIME", "ASCII", "AVG",
    "CAST", "CEIL", "CEILING", "CHAR_LENGTH", "COALESCE", "CONCAT",
    "CONCAT_WS", "COUNT", "CUME_DIST", "CURDATE", "CURTIME",
    "DATE_FORMAT", "DATEDIFF", "DAY", "DAYNAME", "DAYOFMONTH",
    "DAYOFWEEK", "DAYOFYEAR", "DENSE_RANK",
    "ELT", "EXTRACT",
    "FIELD", "FIND_IN_SET", "FLOOR", "FORMAT", "FOUND_ROWS",
    "FROM_UNIXTIME",
    "GREATEST", "GROUP_CONCAT",
    "HEX", "HOUR",
    "INET_ATON", "INET_NTOA", "INSTR", "ISNULL",
    "JSON_ARRAY", "JSON_ARRAYAGG", "JSON_CONTAINS", "JSON_EXTRACT",
    "JSON_OBJECT", "JSON_OBJECTAGG", "JSON_SET",
    "LAG", "LAST_INSERT_ID", "LAST_VALUE", "LEAD", "LEAST",
    "LENGTH", "LOCATE", "LOWER", "LPAD", "LTRIM",
    "MAKEDATE", "MAKETIME", "MAX", "MD5", "MIN", "MINUTE", "MOD",
    "MONTH", "MONTHNAME",
    "NOW", "NTH_VALUE", "NTILE", "NULLIF",
    "ORD",
    "PERCENT_RANK", "PERIOD_ADD", "PERIOD_DIFF", "POWER",
    "QUARTER",
    "RAND", "RANK", "REPEAT", "REVERSE", "ROUND", "ROW_COUNT",
    "ROW_NUMBER", "RPAD", "RTRIM",
    "SEC_TO_TIME", "SECOND", "SHA1", "SHA2", "SQRT", "STRCMP",
    "STR_TO_DATE", "SUBDATE", "SUBSTR", "SUBSTRING", "SUBTIME",
    "SUM",
    "TIME_TO_SEC", "TIMESTAMPDIFF", "TRIM",
    "UNHEX", "UNIX_TIMESTAMP", "UPPER", "UUID",
    "WEEK", "WEEKDAY",
    "YEAR",
];

#[cfg(test)]
mod tests {
    #[test]
    fn errors_read_like_the_mysql_cli() {
        use crate::plane::{DbmsPlane, ErrorStyle, ShowContext, Verbosity};
        let style = ErrorStyle { verbosity: Verbosity::Default, show_context: ShowContext::Errors };
        let e = shared::DbError {
            number: Some(1146), sqlstate: Some("42S02".into()),
            message: "Table 'sakila.nosuch' doesn't exist".into(), ..Default::default()
        };
        assert_eq!(super::MySqlPlane.format_error(&e, None, style), ["ERROR 1146 (42S02): Table 'sakila.nosuch' doesn't exist"]);
    }

    use super::*;

    #[test]
    fn vertical_output_is_the_clis() {
        let strings = |cells: &[&str]| cells.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        // mysql 9.6, `select actor_id, first_name, null as nothing,
        // 'a\nb' as two_lines, 'é' as `é` from actor where actor_id = 1\G`:
        // names right-justified by bytes, values as they are.
        let columns = strings(&["actor_id", "first_name", "nothing", "two_lines", "é"]);
        let rows = vec![strings(&["1", "PENELOPE", "NULL", "a\nb", "é"])];
        assert_eq!(MySqlPlane.print_vertical(&columns, &rows, &crate::types::PrintedWith::default()), [
            "*************************** 1. row ***************************",
            "  actor_id: 1",
            "first_name: PENELOPE",
            "   nothing: NULL",
            " two_lines: a",
            "b",
            "        é: é",
        ]);
        assert!(MySqlPlane.print_vertical(&columns, &[], &crate::types::PrintedWith::default()).is_empty());
        let two = MySqlPlane.print_vertical(&strings(&["a"]), &[strings(&["1"]), strings(&["2"])], &crate::types::PrintedWith::default());
        assert_eq!(two[2], "*************************** 2. row ***************************");
    }

    #[test]
    fn numeric_columns_line_up_right_as_the_cli_lines_them() {
        use crate::types::Align;
        // `IS_NUM`, by sqlx's names: integers (signed or not, `tinyint(1)`
        // as BOOLEAN), DECIMAL, FLOAT, DOUBLE, YEAR, and NULL's own type.
        for t in ["TINYINT", "TINYINT UNSIGNED", "BOOLEAN", "SMALLINT", "MEDIUMINT UNSIGNED", "INT", "BIGINT UNSIGNED",
                  "FLOAT", "DOUBLE", "DECIMAL", "YEAR", "NULL"] {
            assert_eq!(MySqlPlane.column_align(t), Align::Right, "{t}");
        }
        for t in ["VARCHAR", "CHAR", "TEXT", "DATETIME", "TIMESTAMP", "DATE", "TIME", "BIT", "ENUM", "SET", "JSON", "BLOB", ""] {
            assert_eq!(MySqlPlane.column_align(t), Align::Left, "{t}");
        }
        // Its headers start at the left.
        assert!(!MySqlPlane.centres_headers());
    }

    /// The mysql CLI's `use`, typed or in a batch: "Database changed",
    /// and the database it names (backticks off).
    #[test]
    fn use_switches_the_database() {
        let p = MySqlPlane;
        let switched = |sql: &str| p.switches_database(sql).map(|(db, said)| (db, said.to_string()));
        let to = |db: &str| Some((db.to_string(), "Database changed".to_string()));
        assert_eq!(switched("USE sakila"), to("sakila"));
        assert_eq!(switched("use `my db`"), to("my db"));
        assert_eq!(switched("  Use mysql  "), to("mysql"));
        assert_eq!(switched("USE `a``b`"), to("a`b"));
        assert_eq!(switched("USE sakila # back to work"), to("sakila"));
        assert_eq!(switched("USE\tsakila"), to("sakila"));
        assert_eq!(switched("USER_TABLES"), None);
        assert_eq!(switched("SELECT 1"), None);
        assert_eq!(switched("USE `unterminated"), None);
        assert_eq!(switched("USE"), None);
        let usage = |cmd: &str| match p.named_command(cmd, ";").or_else(|| p.meta_cmd(cmd)) {
            Some(MetaAction::Output(lines)) => lines,
            _ => Vec::new(),
        };
        assert_eq!(usage("use"), ["ERROR: ", "USE must be followed by a database name"]);
        // A line with the delimiter in it is statements, `use sakila` the
        // first — a command.
        assert!(p.named_command("use sakila; select 1 as a;", ";").is_none());
        assert!(matches!(p.statement_command("use sakila"), Some(MetaAction::RunSql(sql, false)) if sql == "USE `sakila`"));
        assert_eq!(usage("\\u"), ["ERROR: ", "USE must be followed by a database name"]);
    }

    #[test]
    fn edit_and_clear_work_on_the_sql_before_them() {
        // The mysql CLI runs a command wherever it's typed outside quotes
        // and comments; `\c` clears the buffer without a word.
        let plane = MySqlPlane;
        assert!(matches!(plane.meta_cmd("select 1 \\c"), Some(MetaAction::ClearInput(None))));
        assert!(matches!(plane.meta_cmd("select 1\n\\c"), Some(MetaAction::ClearInput(None))));
        assert!(matches!(plane.meta_cmd("select 1 \\e"), Some(MetaAction::EditExternal { seed: None, line: None })));
        assert!(plane.meta_cmd("select '\\c'").is_none());
        assert!(plane.meta_cmd("select 1 # \\c").is_none());
        // `\g` and `\G` end the statement: the splitter's.
        assert!(plane.meta_cmd("select 1 \\G").is_none());
        assert!(plane.meta_cmd("select 1 \\g").is_none());
        // Past a command it doesn't know (`\i`) and `\N` (NULL), as it
        // reads the line.
        assert!(matches!(plane.meta_cmd("\\if 1 \\c"), Some(MetaAction::ClearInput(None))));
        assert!(matches!(plane.meta_cmd("select \\N \\e"), Some(MetaAction::EditExternal { .. })));
    }

    #[test]
    fn connect_reads_its_arguments_as_the_mysql_clis_get_arg() {
        let p = MySqlPlane;
        let connect = |input: &str| match p.meta_cmd(input).or_else(|| p.named_command(input, ";")) {
            Some(MetaAction::Connect { database, host }) => (database, host),
            _ => panic!("{input}: not a connect"),
        };
        let s = |text: &str| Some(text.to_string());
        // None: the database it's on, a quick reconnect.
        assert_eq!(connect("\\r"), (None, None));
        assert_eq!(connect("connect"), (None, None));
        assert_eq!(connect("connect mysql"), (s("mysql"), None));
        assert_eq!(connect("\\r sakila 127.0.0.1"), (s("sakila"), s("127.0.0.1")));
        // Quoted as typed, a backslash keeping the next character, the
        // whole line its arguments — a `;` too.
        assert_eq!(connect("\\r 'my db'"), (s("my db"), None));
        assert_eq!(connect("\\r `x`"), (s("x"), None));
        assert_eq!(connect("\\r my\\ db"), (s("my db"), None));
        assert_eq!(connect("\\r sakila;"), (s("sakila;"), None));
        assert_eq!(connect("\\r ''"), (None, None));
        // Inside quotes a backslash stays — the quote after it ends the
        // database, the rest is the host — and a quote left open runs to
        // the line's end (`get_arg`).
        assert_eq!(connect("\\r 'a\\'b'"), (s("a\\"), s("b'")));
        assert_eq!(connect("\\r 'abc"), (s("abc"), None));
        // What the CLI prints: `com_connect`'s lines, `reconnect`'s when it
        // fails, the server's error as it prints one.
        assert_eq!(p.reconnected_notice(true, "7306", "sakila"), ["Connection id:    7306", "Current database: sakila", ""]);
        assert_eq!(p.reconnected_notice(true, "1", ""), ["Connection id:    1", "Current database: *** NONE ***", ""]);
        assert_eq!(p.reconnected_notice(false, "", ""), ["ERROR: ", "Can't connect to the server"]);
        let unknown = shared::DbError {
            number: Some(1049), sqlstate: Some("42000".into()), message: "Unknown database 'nosuchdb'".into(), ..Default::default()
        };
        assert_eq!(p.connect_failed("", "", Some(&unknown), "", false), ["ERROR 1049 (42000): Unknown database 'nosuchdb'"]);
        assert_eq!(p.completion_notice()[0], "Reading table information for completion of table and column names");
    }

    #[test]
    fn print_shows_the_buffer_as_the_mysql_cli_does() {
        const RULE: &str = "--------------";
        let p = MySqlPlane;
        let ctx = crate::plane::MetaContext::default();
        // No previous query: the buffer is empty once a statement has run.
        let print = |typed: &str| p.print_buffer(typed, Some("select 0;"), &ctx);
        assert!(matches!(p.meta_cmd("\\p"), Some(MetaAction::PrintBuffer)));
        assert!(matches!(p.meta_cmd("select 1 \\p"), Some(MetaAction::PrintBuffer)));
        assert!(matches!(p.named_command("print", ";"), Some(MetaAction::PrintBuffer)));
        assert!(matches!(p.statement_command("print"), Some(MetaAction::PrintBuffer)));
        assert_eq!(print(""), [RULE, "", RULE, ""]);
        assert_eq!(print("select 1 as a "), [RULE, "select 1 as a ", RULE, ""]);
        // Every line kept — blank ones and comments too — the last one's
        // line break not doubled.
        assert_eq!(print("select 3\n\n  as c "), [RULE, "select 3", "", "  as c ", RULE, ""]);
        assert_eq!(print("/* c */ select 8 -- x\n"), [RULE, "/* c */ select 8 -- x", RULE, ""]);
        assert_eq!(print("select 1\n\n"), [RULE, "select 1", "", RULE, ""]);
        // The space after the last statement's delimiter went with it; a
        // new line's indent is the buffer's.
        assert_eq!(print(" select 5 as e "), [RULE, "select 5 as e ", RULE, ""]);
        assert_eq!(print("\n  select 5 as e "), [RULE, "  select 5 as e ", RULE, ""]);
        // Run as a statement: that statement is the buffer.
        assert_eq!(print("print"), [RULE, "print", RULE, ""]);
        // What `\e` edits with nothing typed: the statement alone, as its
        // `old_buffer` keeps it.
        assert_eq!(p.previous_buffer("select 1", ";"), "select 1");
        assert_eq!(p.previous_buffer("select 1", "\\G"), "select 1");
    }

    #[test]
    fn a_command_after_sql_runs_there_as_the_mysql_cli_runs_it() {
        let plane = MySqlPlane;
        // Its commands run after a buffer; the shell keeps the buffer for
        // those that don't work on it (`MetaAction::works_on_buffer`).
        assert!(matches!(plane.meta_cmd("select 1 as a \\s"), Some(MetaAction::Introspect { .. })));
        assert!(matches!(plane.meta_cmd("select 1 \\u sakila"), Some(MetaAction::RunSql(sql, false)) if sql == "USE `sakila`"));
        assert!(matches!(plane.meta_cmd("select 1\n\\d //"), Some(MetaAction::SetDelimiter(d)) if d == "//"));
        assert!(matches!(plane.meta_cmd("select 1 \\h"), Some(MetaAction::Output(_))));
        assert!(matches!(plane.meta_cmd("select 1 \\q"), Some(MetaAction::Disconnect)));
        assert!(matches!(plane.meta_cmd("select 1 \\safe-mode on"), Some(MetaAction::SafeMode(true))));
        // One without arguments takes `\` and its letter: the line reads
        // on after it (`command_extent`).
        assert_eq!(plane.command_extent("\\s from dual;", ";"), 2);
        assert_eq!(plane.command_extent("\\c select 2;", ";"), 2);
        assert_eq!(plane.command_extent("\\u sakila\nselect 1;", ";"), "\\u sakila".len());
        assert_eq!(plane.command_extent("\\safe-mode on", ";"), "\\safe-mode on".len());
        // `\sf` is `\s`, then `f` reads on, as the CLI reads it.
        assert_eq!(plane.command_extent("\\sf f\nx", ";"), 2);
        assert_eq!(plane.command_extent("delimiter //\ncreate", ";"), "delimiter //".len());
        // Named commands and a command typed first read only their line.
        assert!(matches!(plane.meta_cmd("\\u sakila\nselect 1;"), Some(MetaAction::RunSql(sql, false)) if sql == "USE `sakila`"));
        assert!(matches!(plane.named_command("delimiter //", ";"), Some(MetaAction::SetDelimiter(d)) if d == "//"));
        // Its commands this shell doesn't run yet say so; `\g` / `\G`
        // end statements; an unknown `\i` stays text.
        for (input, name) in [("\\C utf8", "\\C"), ("select 1 \\x", "\\x"), ("\\R x", "\\R"), ("\\T out.txt", "\\T")] {
            assert!(matches!(plane.meta_cmd(input), Some(MetaAction::Fail(lines)) if lines == [format!("{name} isn't supported here yet.")]), "{input}");
        }
        assert!(plane.meta_cmd("select 1 \\G").is_none());
        assert!(plane.meta_cmd("\\g").is_none());
        assert!(plane.meta_cmd("\\if 1").is_none());
    }

    #[test]
    fn a_semicolon_after_a_command_is_read_as_the_mysql_cli_reads_it() {
        let p = MySqlPlane;
        // Without arguments the command is its letter: the `;` reads on,
        // an empty statement (mysql 9.6: "No query specified").
        assert_eq!(p.command_extent("\\s;", ";"), 2);
        assert_eq!(p.command_extent("\\c;", ";"), 2);
        assert!(matches!(p.meta_cmd("\\s;"), Some(MetaAction::Introspect { .. })));
        // With arguments, it runs to the delimiter, that included, and
        // the line reads on after it ...
        assert_eq!(p.command_extent("\\u sakila;", ";"), "\\u sakila;".len());
        assert_eq!(p.command_extent("\\u sakila ; select 1;", ";"), "\\u sakila ;".len());
        assert_eq!(p.command_extent("\\h select; select 2;", ";"), "\\h select;".len());
        assert_eq!(p.command_extent("\\d // select 4 //", ";"), "\\d //".len());
        // ... but reads its arguments from the line: a `;` in a word is
        // its (`Unknown database 'sakila;'`), and so is the rest of the
        // line for `\h`'s topic (`Nothing found`).
        assert!(matches!(p.meta_cmd("\\u sakila;"), Some(MetaAction::RunSql(sql, false)) if sql == "USE `sakila;`"));
        assert!(matches!(p.meta_cmd("\\usakila;select 7;"), Some(MetaAction::RunSql(sql, false)) if sql == "USE `sakila;select`"));
        assert!(matches!(p.meta_cmd("\\u sakila ; select 1;"), Some(MetaAction::RunSql(sql, false)) if sql == "USE `sakila`"));
        assert!(matches!(p.meta_cmd("\\h select;"), Some(MetaAction::Staged(q)) if q == ["help 'select;'"]));
        assert!(matches!(p.meta_cmd("\\d //;"), Some(MetaAction::SetDelimiter(d)) if d == "//;"));
        // `\h;` has no topic: the whole help.
        assert!(matches!(p.meta_cmd("\\h;"), Some(MetaAction::Output(lines)) if lines == p.help_lines()));
        // An unknown command after the delimiter is said, as it's read.
        assert_eq!(p.line_notes("\\u sakila ; select 1 \\i;", ";").len(), 1);
    }

    #[test]
    fn a_command_is_its_letter_as_the_mysql_cli_reads_it() {
        let p = MySqlPlane;
        // Its arguments follow the letter, space or not — `get_arg`'s
        // first word.
        assert!(matches!(p.meta_cmd("\\usakila"), Some(MetaAction::RunSql(sql, false)) if sql == "USE `sakila`"));
        assert!(matches!(p.meta_cmd("\\u sakila extra"), Some(MetaAction::RunSql(sql, false)) if sql == "USE `sakila`"));
        // One typed with backticks goes as typed.
        assert!(matches!(p.meta_cmd("\\u `sakila`"), Some(MetaAction::RunSql(sql, false)) if sql == "USE `sakila`"));
        assert!(matches!(p.meta_cmd("\\d//"), Some(MetaAction::SetDelimiter(d)) if d == "//"));
        // `\h`'s topic follows a space: `\hselect` is plain `\h`.
        assert!(matches!(p.meta_cmd("\\hselect"), Some(MetaAction::Output(lines)) if lines == p.help_lines()));
        // `\endif` is its `\e`, `\sf f` its `\s`, `\cxx` its `\c` — the
        // rest reads on (`command_extent`).
        assert!(matches!(p.meta_cmd("\\endif"), Some(MetaAction::EditExternal { seed: None, line: None })));
        assert!(matches!(p.meta_cmd("\\sf f"), Some(MetaAction::Introspect { .. })));
        assert!(matches!(p.meta_cmd("\\cxx"), Some(MetaAction::ClearInput(None))));
        assert_eq!(p.command_extent("\\endif", ";"), 2);
        // Parliament's own `\safe-mode` is a word.
        assert!(matches!(p.meta_cmd("\\safe-mode on"), Some(MetaAction::SafeMode(true))));
        assert!(matches!(p.meta_cmd("\\safe-mode off;"), Some(MetaAction::SafeMode(false))));
        assert!(matches!(p.meta_cmd("\\safe-mode;"), Some(MetaAction::Output(_))));
        assert_eq!(p.command_extent("\\safe-mode;", ";"), "\\safe-mode;".len());
        assert!(matches!(p.meta_cmd("\\safe-modex"), Some(MetaAction::Introspect { .. })));
        // An unchanged recall of the previous query stays, as the CLI keeps it.
        assert!(p.keeps_unchanged_recall());
    }

    /// The mysql CLI's `com_server_help`: the server's `HELP` statement,
    /// quoted and cut as the CLI quotes and cuts it, and its answer
    /// printed in each of the CLI's shapes.
    #[test]
    fn help_is_the_servers_as_the_cli_prints_it() {
        assert_eq!(server_help_query("select"), "help 'select'");
        assert_eq!(server_help_query("'Data Types'"), "help 'Data Types'");
        assert_eq!(server_help_query("o'x"), "help 'o'x'");
        // 101 bytes: a 94-letter topic fits with its quote; 95 loses it.
        assert_eq!(server_help_query(&"a".repeat(94)).len(), 101);
        assert!(!server_help_query(&"a".repeat(95)).ends_with('\''));
        assert_eq!(help_topic("\\h  create index "), "create index");
        assert_eq!(help_topic("help"), "");
        assert_eq!(help_topic("/* c */ help contents"), "contents");
        let set = |columns: &[&str], rows: &[&[&str]]| shared::ResultSet {
            columns: columns.iter().map(|c| c.to_string()).collect(),
            rows: rows.iter().map(|r| r.iter().map(|c| Some(c.to_string())).collect()).collect(),
        };
        // One topic.
        let topic = set(&["name", "description", "example"], &[&["SHOW", "Syntax:\nSHOW …\n\nURL: u\n\n", ""]]);
        assert_eq!(server_help_lines("help 'show'", Some(&topic)),
                   ["Name: 'SHOW'", "Description:", "Syntax:", "SHOW …", "", "URL: u", "", ""]);
        // A category's contents: its name, then the items under each kind.
        let category = set(&["source_category_name", "name", "is_it_category"],
                           &[&["Contents", "Account Management", "Y"], &["Contents", "Data Types", "Y"]]);
        assert_eq!(server_help_lines("help 'contents'", Some(&category)), [
            "You asked for help about help category: \"Contents\"",
            "For more information, type 'help <item>', where <item> is one of the following",
            "categories:", "   Account Management", "   Data Types", "",
        ]);
        // Several items.
        let many = set(&["name", "is_it_category"], &[&["CREATE INDEX", "N"], &["Data Definition", "Y"]]);
        assert_eq!(server_help_lines("help 'create'", Some(&many)), [
            "Many help items for your request exist.",
            "To make a more specific request, please type 'help <item>',",
            "where <item> is one of the following",
            "topics:", "   CREATE INDEX", "categories:", "   Data Definition", "",
        ]);
        // Nothing — and for `contents`, the tables may not be loaded.
        let none = set(&["name", "is_it_category"], &[]);
        assert_eq!(server_help_lines("help 'nosuch'", Some(&none)),
                   ["", "Nothing found", "Please try to run 'help contents' for a list of all accessible topics", ""]);
        assert_eq!(server_help_lines("help 'Contents'", Some(&none)),
                   ["", "Nothing found", "", "Please check if 'help tables' are loaded.", ""]);
    }

    #[test]
    fn psql_conditionals_are_the_mysql_clis_unknown_and_edit_commands() {
        // No `\if` blocks: `\if 1` is the CLI's unknown `\i` — it says
        // so and the text stays in the statement, which the server gets.
        let plane = MySqlPlane;
        assert!(plane.meta_cmd("\\if 1").is_none());
        let said: Vec<Vec<String>> = plane.line_notes("\\if 1", ";").into_iter()
            .map(|note| match note { crate::ShellEntry::CliError { lines, .. } => lines, _ => Vec::new() })
            .collect();
        assert_eq!(said, [["ERROR: ", "Unknown command '\\i'."]]);
        assert_eq!(plane.line_notes("select 4 \\i \\j;", ";").len(), 2);
        // Only the line just entered: the `\if` above was said when it was.
        assert!(plane.line_notes("\\if 1\nselect 1;", ";").is_empty());
        assert!(plane.line_notes("select '\\if'", ";").is_empty());
        // `\elif` / `\else` / `\endif` start with `\e`, one of its commands.
        assert!(plane.line_notes("\\endif", ";").is_empty());
    }

    #[test]
    fn edit_ignores_arguments_like_the_mysql_cli() {
        let plane = MySqlPlane;
        for input in ["\\e", "\\e;", "\\e 5", "SELECT 1\n\\e", "SELECT 1\n\\e 5"] {
            assert!(matches!(plane.meta_cmd(input), Some(MetaAction::EditExternal { seed: None, line: None })), "{input}");
        }
    }

    #[test]
    fn delimiter_takes_the_first_word_after_it_as_the_mysql_cli_does() {
        let p = MySqlPlane;
        let set = |input: &str| match p.named_command(input, ";").or_else(|| p.meta_cmd(input)) {
            Some(MetaAction::SetDelimiter(d)) => Ok(d),
            Some(MetaAction::Output(lines)) => Err(lines.concat()),
            _ => Err("not delimiter".into()),
        };
        assert_eq!(set("delimiter //"), Ok("//".into()));
        // `;` is a delimiter too: how DELIMITER // ends.
        assert_eq!(set("delimiter ;"), Ok(";".into()));
        assert_eq!(set("DELIMITER ;"), Ok(";".into()));
        assert_eq!(set("\\d ;"), Ok(";".into()));
        assert_eq!(set("delimiter $$ extra"), Ok("$$".into()));
        assert_eq!(set("delimiter $X$"), Ok("$X$".into()));
        assert_eq!(set("delimiter"), Err("ERROR: DELIMITER must be followed by a 'delimiter' character or string".into()));
        // The delimiter in its line is its argument's (`is_delimiter_command`).
        assert_eq!(set("delimiter //;"), Ok("//;".into()));
        // What follows `use` keeps its case: database names may not fold.
        assert!(matches!(p.named_command("USE MyDB", ";"), Some(MetaAction::RunSql(sql, false)) if sql == "USE `MyDB`"));
    }

    #[test]
    fn edit_is_the_long_form_of_backslash_e() {
        let plane = MySqlPlane;
        assert!(matches!(plane.named_command("edit", ";"), Some(MetaAction::EditExternal { seed: None, line: None })));
        // `EDIT;`, `  Edit ;` are statements its delimiter ends.
        for statement in ["EDIT", "  Edit "] {
            assert!(matches!(plane.statement_command(statement), Some(MetaAction::EditExternal { seed: None, line: None })), "{statement}");
        }
        for input in ["SELECT edit FROM t", "edit x", "editor"] {
            assert!(plane.named_command(input, ";").is_none(), "{input}");
            assert!(plane.statement_command(input).is_none(), "{input}");
        }
    }

    #[test]
    fn named_commands_are_read_as_the_mysql_clis_find_command_reads_them() {
        let p = MySqlPlane;
        let line = |text: &str| p.named_command(text, ";");
        let status = |action: Option<MetaAction>| matches!(action, Some(MetaAction::Introspect { tag, .. }) if tag == "pretty_kv");
        // Its name in any case, space after it, past `/* */` comments.
        assert!(status(line("STATUS")));
        assert!(status(line("  status  ")));
        assert!(status(line("/* c */ status")));
        assert!(status(p.statement_command(" Status ")));
        // Arguments only for a command that takes them (`status foo;`
        // goes to the server); a line break after the name isn't a space.
        assert!(line("status foo").is_none());
        assert!(p.statement_command("status\n").is_none());
        let topic = |action: Option<MetaAction>| match action {
            Some(MetaAction::Staged(queries)) => queries.first().cloned(),
            _ => None,
        };
        assert_eq!(topic(line("help select")).as_deref(), Some("help 'select'"));
        assert_eq!(topic(line("? select")).as_deref(), Some("help 'select'"));
        assert_eq!(topic(p.statement_command("help select")).as_deref(), Some("help 'select'"));
        assert!(matches!(line("help"), Some(MetaAction::Output(lines)) if lines == p.help_lines()));
        assert!(matches!(line("clear"), Some(MetaAction::ClearInput(None))));
        assert!(matches!(line("Quit"), Some(MetaAction::Disconnect)));
        assert!(matches!(p.statement_command("exit"), Some(MetaAction::Disconnect)));
        // A line with `\g` or the delimiter in it is read as statements.
        assert!(line("status;").is_none());
        assert!(line("status \\g").is_none());
        assert!(p.named_command("status//", "//").is_none());
        // `go` / `ego` alone run the empty buffer; as a statement, their
        // name goes to the server.
        for name in ["go", "EGO"] {
            assert!(matches!(line(name), Some(MetaAction::Fail(lines)) if lines == NO_QUERY), "{name}");
            assert!(p.statement_command(name).is_none(), "{name}");
        }
        // The ones this shell doesn't run say so, by the name typed.
        assert!(matches!(line("resetconnection"), Some(MetaAction::Fail(lines)) if lines == ["resetconnection isn't supported here yet."]));
        assert!(matches!(p.statement_command("query_attributes n v"), Some(MetaAction::Fail(lines)) if lines == ["query_attributes isn't supported here yet."]));
        // `use` with a backtick sends the text typed (`normalize_dbname`).
        assert!(matches!(line("use `my db`"), Some(MetaAction::RunSql(sql, false)) if sql == "use `my db`"));
        // `nopager` / `notee` say what the CLI says (a pane pages nothing,
        // has no outfile); `rehash` fetches completion's names afresh.
        let said = |action: Option<MetaAction>| match action {
            Some(MetaAction::Output(lines)) => lines,
            _ => Vec::new(),
        };
        for input in ["\\n", "nopager", "NoPager"] {
            assert_eq!(said(p.meta_cmd(input).or_else(|| line(input))), ["PAGER set to stdout"], "{input}");
        }
        for input in ["\\t", "notee"] {
            assert_eq!(said(p.meta_cmd(input).or_else(|| line(input))), ["Outfile disabled."], "{input}");
        }
        assert!(matches!(p.meta_cmd("\\#"), Some(MetaAction::RebuildCompletion)));
        // `warnings` / `nowarning`: the server's warnings after each
        // statement, or not, said as the CLI says it.
        for input in ["\\W", "warnings", "Warnings"] {
            assert!(matches!(p.meta_cmd(input).or_else(|| line(input)),
                Some(MetaAction::ShowWarnings { on: true, said }) if said == "Show warnings enabled."), "{input}");
        }
        for input in ["\\w", "nowarning"] {
            assert!(matches!(p.meta_cmd(input).or_else(|| line(input)),
                Some(MetaAction::ShowWarnings { on: false, said }) if said == "Show warnings disabled."), "{input}");
        }
        assert_eq!(p.warnings_query(), Some("show warnings"));
        let rows = |rows: &[[&str; 3]]| shared::ResultSet {
            columns: vec!["Level".into(), "Code".into(), "Message".into()],
            rows: rows.iter().map(|r| r.iter().map(|c| Some(c.to_string())).collect()).collect(),
        };
        let two = rows(&[["Warning", "1292", "Truncated incorrect INTEGER value: 'a'"], ["Warning", "1365", "Division by 0"]]);
        assert_eq!(p.warning_lines(&two, None), [
            "Warning (Code 1292): Truncated incorrect INTEGER value: 'a'", "Warning (Code 1365): Division by 0",
        ]);
        // The statement's own error, alone: not again.
        let own = rows(&[["Error", "1146", "Table 'sakila.nosuch' doesn't exist"]]);
        assert!(p.warning_lines(&own, Some(1146)).is_empty());
        assert_eq!(p.warning_lines(&own, Some(1064)).len(), 1);
        assert_eq!(p.warning_lines(&two, Some(1292)).len(), 2);
        assert!(matches!(line("rehash"), Some(MetaAction::RebuildCompletion)));
        assert!(matches!(p.statement_command("rehash"), Some(MetaAction::RebuildCompletion)));
    }
}
