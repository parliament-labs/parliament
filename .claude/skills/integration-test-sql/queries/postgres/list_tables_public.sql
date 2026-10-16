-- Representative plane query for ObjectKind::Tables on Postgres.
-- Check column aliases match `plane.md` contract.
SELECT n.nspname::text AS "Schema",
       c.relname::text AS "Name",
       CASE c.relkind WHEN 'r' THEN 'BASE TABLE' ELSE 'OTHER' END AS "Table_type",
       c.reltuples::bigint AS "Rows"
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public'
  AND c.relkind = 'r'
ORDER BY 1, 2;
