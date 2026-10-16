import mysql from 'mysql2/promise';
import { Client as PgClient } from 'pg';

// Admin clients for staging scenarios that the UI-under-test can't set
// up by itself (e.g. a held lock from outside the UI) and for test
// cleanup. Connection params match tests/fixtures/setup.sh defaults;
// override via env.
//
// Password resolution: PW_*_PASSWORD (Playwright-specific override) takes
// precedence, then the standard MYSQL_PWD / PGPASSWORD env vars used by
// the mysql / psql CLIs. With ~/.parliament/secrets.env sourced from
// ~/.zshrc, the standard vars are pre-populated and Playwright connects
// without any per-spec setup. See tests/fixtures/secrets.env.template.

const MYSQL_HOST = process.env.PW_MYSQL_HOST ?? '127.0.0.1';
const MYSQL_PORT = Number(process.env.PW_MYSQL_PORT ?? '3306');
const MYSQL_USER = process.env.PW_MYSQL_USER ?? 'root';
const MYSQL_PASSWORD = process.env.PW_MYSQL_PASSWORD ?? process.env.MYSQL_PWD ?? '';
const MYSQL_DB = process.env.PW_MYSQL_DB ?? 'sakila';

const PG_HOST = process.env.PW_PG_HOST ?? '127.0.0.1';
const PG_PORT = Number(process.env.PW_PG_PORT ?? '5432');
const PG_USER = process.env.PW_PG_USER ?? 'postgres';
const PG_PASSWORD = process.env.PW_PG_PASSWORD ?? process.env.PGPASSWORD ?? '';
const PG_DB = process.env.PW_PG_DB ?? 'pagila';

export async function openMySql(): Promise<mysql.Connection> {
  return mysql.createConnection({
    host: MYSQL_HOST,
    port: MYSQL_PORT,
    user: MYSQL_USER,
    password: MYSQL_PASSWORD,
    database: MYSQL_DB,
    multipleStatements: false,
  });
}

export async function openPg(): Promise<PgClient> {
  const client = new PgClient({
    host: PG_HOST,
    port: PG_PORT,
    user: PG_USER,
    password: PG_PASSWORD,
    database: PG_DB,
  });
  await client.connect();
  return client;
}

// Start a transaction that row-locks `actor_id = 1`. Returns the open
// connection so the caller can COMMIT to release.
export async function lockActorRow(): Promise<mysql.Connection> {
  const conn = await openMySql();
  await conn.beginTransaction();
  await conn.query('SELECT * FROM actor WHERE actor_id = 1 FOR UPDATE');
  return conn;
}
