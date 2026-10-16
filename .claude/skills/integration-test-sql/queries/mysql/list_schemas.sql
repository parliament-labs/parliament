-- Mirrors MySqlPlane::list_schemas_sql()
SELECT SCHEMA_NAME AS `Schema`
FROM information_schema.SCHEMATA
WHERE SCHEMA_NAME NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')
ORDER BY 1;
