//! Errors as the database server reported them, for the browser: every
//! field a Postgres ErrorResponse carries (psql shows them by
//! `VERBOSITY`, all of them on `\errverbose`), or a MySQL error's number
//! and SQLSTATE — instead of sqlx's one-line Display ("error returned
//! from database: …").

use shared::DbError;
use sqlx::mysql::MySqlDatabaseError;
use sqlx::postgres::{PgDatabaseError, PgErrorPosition, PgSeverity};

/// The server's error fields, when `e` is an error the server reported.
pub(crate) fn fields(e: &sqlx::Error) -> Option<DbError> {
    let db = e.as_database_error()?;
    if let Some(pg) = db.try_downcast_ref::<PgDatabaseError>() {
        return Some(postgres(pg));
    }
    if let Some(my) = db.try_downcast_ref::<MySqlDatabaseError>() {
        return Some(DbError {
            sqlstate: my.code().map(String::from),
            number:   Some(my.number()),
            message:  my.message().to_string(),
            ..DbError::default()
        });
    }
    Some(DbError { message: db.message().to_string(), ..DbError::default() })
}

/// What to show for `e`: the primary message of a server error, or
/// sqlx's description of anything else (a lost connection, …).
pub(crate) fn message(e: &sqlx::Error) -> String {
    match e.as_database_error() {
        Some(db) => db.message().to_string(),
        None => e.to_string(),
    }
}

fn postgres(pg: &PgDatabaseError) -> DbError {
    let own = |v: Option<&str>| v.map(String::from);
    let (position, internal_position, internal_query) = match pg.position() {
        Some(PgErrorPosition::Original(at)) => (Some(at as u32), None, None),
        Some(PgErrorPosition::Internal { position, query }) => (None, Some(position as u32), Some(query.to_string())),
        None => (None, None, None),
    };
    DbError {
        severity: Some(severity(pg.severity()).into()),
        sqlstate: Some(pg.code().to_string()),
        number: None,
        message: pg.message().to_string(),
        detail: own(pg.detail()),
        hint: own(pg.hint()),
        position,
        internal_position,
        internal_query,
        context: own(pg.r#where()),
        schema: own(pg.schema()),
        table: own(pg.table()),
        column: own(pg.column()),
        datatype: own(pg.data_type()),
        constraint: own(pg.constraint()),
        file: own(pg.file()),
        line: pg.line().map(|l| l as u32),
        routine: own(pg.routine()),
    }
}

/// The severity as the server spells it (psql prints it verbatim).
fn severity(s: PgSeverity) -> &'static str {
    match s {
        PgSeverity::Panic   => "PANIC",
        PgSeverity::Fatal   => "FATAL",
        PgSeverity::Error   => "ERROR",
        PgSeverity::Warning => "WARNING",
        PgSeverity::Notice  => "NOTICE",
        PgSeverity::Debug   => "DEBUG",
        PgSeverity::Info    => "INFO",
        PgSeverity::Log     => "LOG",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LiveConnection;

    /// Live check that a Postgres error arrives with the fields psql
    /// shows — SQLSTATE, position, DETAIL, HINT, CONTEXT, LOCATION, and
    /// the internal query of an error raised inside a function. Needs
    /// the local Pagila database (PGPASSWORD from
    /// ~/.parliament/secrets.env):
    /// `cargo test -p noctua -- --ignored pg_error_fields_contract`
    #[tokio::test]
    #[ignore]
    async fn pg_error_fields_contract() {
        use sqlx::postgres::PgConnectOptions;

        let mut opts = PgConnectOptions::new();
        if std::env::var("PGUSER").is_err()     { opts = opts.username("postgres"); }
        if std::env::var("PGDATABASE").is_err() { opts = opts.database("pagila"); }
        let mut live = LiveConnection::Postgres(crate::connect_pg(opts, None).await.expect("connect"));
        let e = live.fetch_all_strings("SELECT * FROM nosuch").await.expect_err("missing table");
        let f = fields(&e).expect("server error");
        assert_eq!((f.severity.as_deref(), f.sqlstate.as_deref(), f.position), (Some("ERROR"), Some("42P01"), Some(15)));
        assert_eq!(f.message, "relation \"nosuch\" does not exist");
        assert_eq!(message(&e), f.message);
        assert!(f.file.is_some() && f.line.is_some() && f.routine.is_some(), "{f:?}");

        let e = live.fetch_all_strings(
            "DO $$ BEGIN RAISE EXCEPTION 'boom' USING DETAIL = 'the detail', HINT = 'a hint', ERRCODE = 'P0042'; END $$",
        ).await.expect_err("raise");
        let f = fields(&e).expect("server error");
        assert_eq!((f.sqlstate.as_deref(), f.message.as_str()), (Some("P0042"), "boom"));
        assert_eq!((f.detail.as_deref(), f.hint.as_deref()), (Some("the detail"), Some("a hint")));
        assert!(f.context.as_deref().is_some_and(|c| c.contains("PL/pgSQL function inline_code_block")), "{f:?}");

        let e = live.fetch_all_strings("DO $$ BEGIN PERFORM * FROM nosuch_internal; END $$").await.expect_err("internal");
        let f = fields(&e).expect("server error");
        assert_eq!(f.sqlstate.as_deref(), Some("42P01"));
        assert_eq!((f.internal_query.as_deref(), f.internal_position, f.position), (Some("SELECT * FROM nosuch_internal"), Some(15), None));

        let e = live.fetch_all_strings(
            "CREATE TEMP TABLE errors_probe (n int CONSTRAINT positive CHECK (n > 0)); INSERT INTO errors_probe VALUES (0)",
        ).await.expect_err("check violation");
        let f = fields(&e).expect("server error");
        assert_eq!((f.sqlstate.as_deref(), f.constraint.as_deref(), f.table.as_deref()), (Some("23514"), Some("positive"), Some("errors_probe")));
        assert!(f.detail.as_deref().is_some_and(|d| d.starts_with("Failing row contains")), "{f:?}");
    }

    /// The same for MySQL: number and SQLSTATE, as the mysql CLI shows
    /// them (`ERROR 1146 (42S02): …`). Needs the local sakila database.
    /// `cargo test -p noctua -- --ignored mysql_error_fields_contract`
    #[tokio::test]
    #[ignore]
    async fn mysql_error_fields_contract() {
        use sqlx::mysql::MySqlConnectOptions;

        let mut opts = MySqlConnectOptions::new().username("root").database("sakila");
        if let Ok(pwd) = std::env::var("MYSQL_PWD") { opts = opts.password(&pwd); }
        let mut live = LiveConnection::MySql(crate::connect_mysql(opts).await.expect("connect"));
        let e = live.fetch_all_strings("SELECT * FROM nosuch").await.expect_err("missing table");
        let f = fields(&e).expect("server error");
        assert_eq!((f.number, f.sqlstate.as_deref()), (Some(1146), Some("42S02")));
        assert_eq!(f.message, "Table 'sakila.nosuch' doesn't exist");
    }
}
