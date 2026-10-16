# Test Fixtures & Sample Databases

## Sample Databases

For development, screenshots, and integration testing, we use community-standard sample databases:

- **Pagila** (PostgreSQL) — DVD rental store. Customers, films, actors, rentals, payments. 15 tables, 7 views, 8 functions, triggers, indexes, foreign keys. The Postgres community's canonical demo database.
- **Sakila** (MySQL) — Same schema concept, MySQL-native. The MySQL community's standard.

### Quick setup

```bash
# Both databases
./tests/fixtures/setup.sh all

# Postgres only
./tests/fixtures/setup.sh postgres

# MySQL only
./tests/fixtures/setup.sh mysql
```

The script downloads the SQL files (cached in `pagila/` and `sakila/` subdirectories), creates a fresh database, and loads schema + data. Idempotent — safe to run multiple times.

### Connection details

| Database | Host | Port | User | Database name |
|----------|------|------|------|---------------|
| Pagila (PG) | 127.0.0.1 | 5432 | postgres | pagila |
| Sakila (MySQL) | 127.0.0.1 | 3306 | root | sakila |

### What's in the data

Pagila/Sakila contain:
- ~600 customers with addresses across multiple countries
- ~1,000 films with categories, actors, and rental history
- ~16,000 rental transactions with payments
- Views for sales summaries, actor filmographies, etc.
- Functions and stored procedures
- Triggers for inventory management
- Foreign key relationships across all tables

Rich enough to exercise every feature: Schema Explorer navigation, DDL reconstruction, `\d*` commands, `\sf`/`\sv` function/view source, `\watch` polling, multi-table joins, and meaningful query results for screenshots.

## Test Fixtures (future)

Integration test fixtures (for `cargo test -p noctua`) will live here alongside the sample databases:

```
tests/fixtures/
├── setup.sh              ← sample database installer
├── pagila/               ← cached Pagila SQL (downloaded by setup.sh)
├── sakila/               ← cached Sakila SQL (downloaded by setup.sh)
├── postgres_test.sql     ← (future) integration test schema
├── mysql_test.sql        ← (future) integration test schema
└── README.md             ← this file
```
