-- Mirrors PostgresPlane::list_schemas_sql()
SELECT nspname AS "Schema"
FROM pg_namespace
WHERE nspname NOT IN ('pg_catalog', 'information_schema')
  AND nspname NOT LIKE 'pg_toast%'
  AND nspname NOT LIKE 'pg_temp%'
ORDER BY 1;
