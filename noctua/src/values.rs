//! Row values as display strings — what the shell, the explorer and the
//! data tab show for every cell. One converter per driver, keeping SQL
//! NULL as `None`: a shell's results and introspect batches tell NULL
//! from text as the CLIs do (a text 'NULL' isn't one); the explorer's
//! introspect results read it as the text "NULL" (`fetch_all_strings`).

use sqlx::{Column, Row, TypeInfo, ValueRef};

/// Extract column names and row data from a vector of sqlx rows, using
/// the provided per-value converter function.
pub(crate) fn rows_to_cells<R, F, T>(rows: &[R], convert: F) -> (Vec<String>, Vec<Vec<T>>)
where
    R: Row,
    F: Fn(&R, usize, &str) -> T,
{
    let columns = rows
        .first()
        .map(|r| r.columns().iter().map(|col| col.name().to_string()).collect())
        .unwrap_or_default();
    let data = rows
        .iter()
        .map(|r| {
            r.columns()
                .iter()
                .enumerate()
                .map(|(i, col)| convert(r, i, col.type_info().name()))
                .collect()
        })
        .collect();
    (columns, data)
}

/// A MySQL value as the mysql CLI prints it.
///
/// Queries go through the text protocol (`sqlx::raw_sql`), so MySQL
/// sends every value as the text the mysql CLI prints: DECIMAL with its
/// scale, JSON as stored, fractional seconds as declared, ENUM / SET /
/// YEAR, unsigned BIGINT. The bytes pass through — read unchecked,
/// because sqlx's typed decoders refuse DECIMAL and JSON as strings,
/// which is how those used to come out as "?". Two deliberate
/// differences from the CLI:
/// - binary data that isn't UTF-8 shows as `<binary NB>`; binary that
///   is (information_schema keeps text in VARBINARY columns) shows as
///   text;
/// - BIT(n) shows its integer value, as Workbench does, not raw bits.
///
/// `None` for SQL NULL.
pub(crate) fn mysql_value(row: &sqlx::mysql::MySqlRow, i: usize, type_name: &str) -> Option<String> {
    let bytes = match row.try_get_unchecked::<Option<&[u8]>, _>(i) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return None,
        Err(_) => return Some("?".into()),
    };
    if type_name.eq_ignore_ascii_case("BIT") {
        return Some(bit_value(bytes));
    }
    Some(match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => format!("<binary {}B>", bytes.len()),
    })
}

/// A BIT(n) value — up to 64 bits, big-endian on the wire — as its
/// integer: BIT(1) reads 0 / 1, `b'101'` reads 5.
fn bit_value(bytes: &[u8]) -> String {
    bytes.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b)).to_string()
}

/// A Postgres value exactly as psql prints it.
///
/// noctua sends every query through the simple-query protocol
/// (`sqlx::raw_sql`), so the server returns every column in text
/// format — the output of the type's own output function, which is
/// what psql prints. Passing those bytes through renders every type
/// faithfully (numeric, money, uuid, json, ranges, geometric types,
/// arrays, enums, domains, composites, extension types) under the
/// session's own TimeZone / DateStyle / IntervalStyle / bytea_output.
/// Decoding into Rust types first, as this used to, missed every type
/// without a matching decoder ("?", `<TYPE>`) and re-rendered the rest
/// unlike psql (`true` for `t`, UTC ISO timestamps, `inf`).
///
/// Binary format only arrives for prepared statements, which noctua
/// doesn't issue; such a value shows as `<binary TYPE>` rather than a
/// guess.
///
/// `None` for SQL NULL.
pub(crate) fn pg_value(row: &sqlx::postgres::PgRow, i: usize, type_name: &str) -> Option<String> {
    let Ok(raw) = row.try_get_raw(i) else { return Some("?".into()) };
    if raw.is_null() {
        return None;
    }
    Some(match (raw.format(), raw.as_bytes()) {
        (sqlx::postgres::PgValueFormat::Text, Ok(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
        _ => format!("<binary {type_name}>"),
    })
}

#[cfg(test)]
mod tests {
    use super::bit_value;

    #[test]
    fn bit_values_read_as_integers() {
        assert_eq!(bit_value(&[1]), "1");
        assert_eq!(bit_value(&[0b101]), "5");
        assert_eq!(bit_value(&[1, 0]), "256");
        assert_eq!(bit_value(&[0xff; 8]), u64::MAX.to_string());
        assert_eq!(bit_value(&[]), "0");
    }
    // ── Live contract: Postgres values render exactly as psql prints them ──

    /// Each value expression with psql's rendering of it (`psql -X -A -t`,
    /// psql 18, against the session settings in `PSQL_SESSION`). NULL is
    /// noctua's `"NULL"` sentinel, not psql's empty string.
    const PSQL_RENDERINGS: &[(&str, &str)] = &[
        ("1::int2", "1"),
        ("2::int4", "2"),
        ("3::int8", "3"),
        ("12.30::numeric", "12.30"),
        ("123456789012345678901234567890.123::numeric", "123456789012345678901234567890.123"),
        ("'NaN'::numeric", "NaN"),
        ("1.5::float4", "1.5"),
        ("0.1::float8", "0.1"),
        ("1e20::float8", "1e+20"),
        ("'Infinity'::float8", "Infinity"),
        ("12.34::money", "$12.34"),
        ("true", "t"),
        ("false", "f"),
        ("'a'::\"char\"", "a"),
        ("'hello'::text", "hello"),
        ("'ab'::char(4)", "ab  "),
        ("'pg_class'::name", "pg_class"),
        ("'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid", "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
        ("'{\"b\": 1,  \"a\": [1, 2]}'::json", "{\"b\": 1,  \"a\": [1, 2]}"),
        ("'{\"b\": 1,  \"a\": [1, 2]}'::jsonb", "{\"a\": [1, 2], \"b\": 1}"),
        ("'$.a[*]'::jsonpath", "$.\"a\"[*]"),
        ("'<a x=\"1\"/>'::xml", "<a x=\"1\"/>"),
        ("'\\xdeadbeef'::bytea", "\\xdeadbeef"),
        ("'abc'::bytea", "\\x616263"),
        ("'2026-10-02'::date", "2026-10-02"),
        ("'12:34:56.789'::time", "12:34:56.789"),
        ("'12:34:56+02'::timetz", "12:34:56+02"),
        ("'2026-10-02 12:34:56.5'::timestamp", "2026-10-02 12:34:56.5"),
        ("'2026-10-02 12:34:56.5+02'::timestamptz", "2026-10-02 10:34:56.5+00"),
        ("'1 mon 2 days 03:04:05'::interval", "1 mon 2 days 03:04:05"),
        ("'-1 hour'::interval", "-01:00:00"),
        ("'192.168.1.5/24'::inet", "192.168.1.5/24"),
        ("'10.0.0.0/8'::cidr", "10.0.0.0/8"),
        ("'08:00:2b:01:02:03'::macaddr", "08:00:2b:01:02:03"),
        ("B'101'", "101"),
        ("B'10110'::varbit", "10110"),
        ("'[1,5)'::int4range", "[1,5)"),
        ("'(1.5,2.5]'::numrange", "(1.5,2.5]"),
        ("'[2026-01-01,2026-02-01)'::daterange", "[2026-01-01,2026-02-01)"),
        ("'{[1,3), [5,7)}'::int4multirange", "{[1,3),[5,7)}"),
        ("'(1,2)'::point", "(1,2)"),
        ("'((0,0),(1,1))'::box", "(1,1),(0,0)"),
        ("'((0,0),(1,1),(1,0))'::polygon", "((0,0),(1,1),(1,0))"),
        ("'<(0,0),1>'::circle", "<(0,0),1>"),
        ("'a fat cat'::tsvector", "'a' 'cat' 'fat'"),
        ("'fat & cat'::tsquery", "'fat' & 'cat'"),
        ("1::oid", "1"),
        ("'pg_class'::regclass", "pg_class"),
        ("'int4'::regtype", "integer"),
        ("'(0,1)'::tid", "(0,1)"),
        ("'0/16B3748'::pg_lsn", "0/16B3748"),
        ("'{1,2,NULL}'::int[]", "{1,2,NULL}"),
        ("ARRAY['a b', 'c,d', 'e\"f', NULL]", "{\"a b\",\"c,d\",\"e\\\"f\",NULL}"),
        ("'{{1,2},{3,4}}'::int[]", "{{1,2},{3,4}}"),
        ("'{1.5,2.25}'::numeric[]", "{1.5,2.25}"),
        ("'{t,f}'::bool[]", "{t,f}"),
        ("'{2026-01-01}'::date[]", "{2026-01-01}"),
        ("'PG'::mpaa_rating", "PG"),           // pagila enum
        ("2006::year", "2006"),                // pagila domain
        ("ROW(1, 'a b', NULL)", "(1,\"a b\",)"),
        ("NULL::int", "NULL"),
        ("NULL::text", "NULL"),
    ];

    /// Settings that make the renderings above deterministic.
    const PSQL_SESSION: &str = "SET TimeZone = 'UTC'; SET DateStyle = 'ISO, MDY'; \
                                SET IntervalStyle = 'postgres'; SET lc_monetary = 'C'; \
                                SET extra_float_digits = 1; SET bytea_output = 'hex'";

    /// Runs on the shell's own path (`fetch_all_strings`) against a live
    /// Pagila: `cargo test -p noctua -- --ignored pg_values_render_like_psql`
    /// (connects as $PGUSER or postgres, password from $PGPASSWORD).
    #[tokio::test(flavor = "current_thread")]
    #[ignore]
    async fn pg_values_render_like_psql() {
        use sqlx::postgres::PgConnectOptions;

        let mut opts = PgConnectOptions::new();
        if std::env::var("PGUSER").is_err()     { opts = opts.username("postgres"); }
        if std::env::var("PGDATABASE").is_err() { opts = opts.database("pagila"); }
        let conn = crate::connect_pg(opts, None).await.expect("connect");
        let mut live = crate::LiveConnection::Postgres(conn);
        live.fetch_all_strings(PSQL_SESSION).await.expect("session settings");

        let exprs: Vec<&str> = PSQL_RENDERINGS.iter().map(|(e, _)| *e).collect();
        let (_, rows, _) = live.fetch_all_strings(&format!("SELECT {}", exprs.join(", ")))
            .await.expect("type matrix");
        let mismatches: Vec<String> = PSQL_RENDERINGS.iter().zip(&rows[0])
            .filter(|((_, want), got)| want != got)
            .map(|((expr, want), got)| format!("{expr}: psql {want:?}, noctua {got:?}"))
            .collect();
        assert!(mismatches.is_empty(), "{} of {} differ from psql:\n{}",
                mismatches.len(), PSQL_RENDERINGS.len(), mismatches.join("\n"));
    }

    // ── Live contract: MySQL values render as the mysql CLI prints them ──

    /// Each value expression with the mysql CLI's rendering of it
    /// (`mysql --batch --skip-column-names`, MySQL 8, `time_zone` UTC).
    /// Deliberate differences from the CLI are listed in
    /// `mysql_value`'s docs, not here.
    const MYSQL_RENDERINGS: &[(&str, &str)] = &[
        ("1", "1"),
        ("CAST(-5 AS SIGNED)", "-5"),
        ("CAST(18446744073709551615 AS UNSIGNED)", "18446744073709551615"),
        ("12.30", "12.30"),
        ("CAST(12.3 AS DECIMAL(10,2))", "12.30"),
        ("CAST(1.5 AS FLOAT)", "1.5"),
        ("CAST(0.1 AS DOUBLE)", "0.1"),
        ("1e20", "1e20"),
        ("TRUE", "1"),
        ("'hello'", "hello"),
        ("CAST('abc' AS BINARY)", "abc"),
        ("DATE('2026-10-02')", "2026-10-02"),
        ("TIME('12:34:56.789')", "12:34:56.789"),
        ("CAST('2026-10-02 12:34:56.5' AS DATETIME(1))", "2026-10-02 12:34:56.5"),
        ("TIMESTAMP('2026-10-02 12:34:56')", "2026-10-02 12:34:56"),
        ("YEAR('2026-10-02')", "2026"),
        ("JSON_OBJECT('b', 1, 'a', JSON_ARRAY(1, 2))", "{\"a\": [1, 2], \"b\": 1}"),
        ("CAST('{\"x\": 1}' AS JSON)", "{\"x\": 1}"),
        ("CAST(b'101' AS UNSIGNED)", "5"),
        ("ST_AsText(POINT(1, 2))", "POINT(1 2)"),
        ("NULL", "NULL"),
        ("(SELECT rating FROM film WHERE film_id = 1)", "PG"),                        // ENUM
        ("(SELECT special_features FROM film WHERE film_id = 1)", "Deleted Scenes,Behind the Scenes"), // SET
        ("(SELECT rental_rate FROM film WHERE film_id = 1)", "0.99"),                 // DECIMAL(4,2)
        ("(SELECT last_update FROM film WHERE film_id = 1)", "2006-02-15 13:03:42"),  // TIMESTAMP
        ("(SELECT release_year FROM film WHERE film_id = 1)", "2006"),                // YEAR
    ];

    /// Runs on the shell's own path against a live Sakila:
    /// `cargo test -p noctua -- --ignored mysql_values_render_like_the_cli`
    /// (connects as root@localhost, password from $MYSQL_PWD).
    #[tokio::test(flavor = "current_thread")]
    #[ignore]
    async fn mysql_values_render_like_the_cli() {
        use sqlx::mysql::MySqlConnectOptions;

        let mut opts = MySqlConnectOptions::new().username("root").database("sakila");
        if let Ok(pwd) = std::env::var("MYSQL_PWD") { opts = opts.password(&pwd); }
        let conn = crate::connect_mysql(opts).await.expect("connect");
        let mut live = crate::LiveConnection::MySql(conn);
        live.fetch_all_strings("SET time_zone = '+00:00'").await.expect("session settings");

        let exprs: Vec<&str> = MYSQL_RENDERINGS.iter().map(|(e, _)| *e).collect();
        let (_, rows, _) = live.fetch_all_strings(&format!("SELECT {}", exprs.join(", ")))
            .await.expect("type matrix");
        let mismatches: Vec<String> = MYSQL_RENDERINGS.iter().zip(&rows[0])
            .filter(|((_, want), got)| want != got)
            .map(|((expr, want), got)| format!("{expr}: mysql {want:?}, noctua {got:?}"))
            .collect();
        assert!(mismatches.is_empty(), "{} of {} differ from the mysql CLI:\n{}",
                mismatches.len(), MYSQL_RENDERINGS.len(), mismatches.join("\n"));
    }
}
