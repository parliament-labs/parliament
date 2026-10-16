-- Representative plane query for table_columns_sql('public', 'film') on Postgres.
-- Column-alias contract: Field / Type / Null / Default / Extra / Comment.
SELECT a.attname::text                         AS "Field",
       format_type(a.atttypid, a.atttypmod)    AS "Type",
       CASE WHEN a.attnotnull THEN 'NO' ELSE 'YES' END AS "Null",
       COALESCE(pg_get_expr(d.adbin, d.adrelid), '') AS "Default",
       ''                                      AS "Extra",
       COALESCE(col_description(a.attrelid, a.attnum), '') AS "Comment"
FROM pg_attribute a
JOIN pg_class c ON c.oid = a.attrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
WHERE n.nspname = 'public' AND c.relname = 'film' AND a.attnum > 0 AND NOT a.attisdropped
ORDER BY a.attnum;
