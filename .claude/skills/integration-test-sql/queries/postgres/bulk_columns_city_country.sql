-- Representative plane query for table_columns_bulk_sql on Postgres.
-- Used by the ERD tab's "All Fields" mode to fetch every column of N
-- tables in a single round trip. Column-alias contract: Schema_, Table_,
-- Field, Ord, Is_pk (case-insensitive parser lookup in
-- tyto/src/explorer.rs::parse_erd_columns).
--
-- The Is_pk check uses pg_index.indisprimary AND attnum = ANY(indkey)
-- to handle composite PKs (any column whose attnum is in the PK index's
-- indkey vector → 1; everything else → 0 via the LEFT JOIN's NULL).
SELECT n.nspname AS "Schema_",
       c.relname AS "Table_",
       a.attname AS "Field",
       a.attnum  AS "Ord",
       CASE WHEN i.indisprimary AND a.attnum = ANY(i.indkey) THEN 1 ELSE 0 END AS "Is_pk"
FROM pg_attribute a
JOIN pg_class     c ON c.oid = a.attrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_index i ON i.indrelid = c.oid AND i.indisprimary
WHERE NOT a.attisdropped AND a.attnum > 0
  AND (n.nspname, c.relname) IN (('public','city'),('public','country'))
ORDER BY n.nspname, c.relname, a.attnum;
