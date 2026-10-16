-- Representative plane query for table_indexes_sql('public', 'payment_p2022_01') on Postgres.
-- Column-alias contract: Key_name / Non_unique / Index_type / Index_size /
-- Column_name / Seq_in_index / Inherited.
-- Locks down partition-index inheritance: payment_p2022_04's PRIMARY index is
-- propagated from payment's partitioned-parent pkey (Inherited=1); the FK and
-- *_idx indexes are partition-local (Inherited=0).
SELECT CASE WHEN ix.indisprimary THEN 'PRIMARY' ELSE i.relname END AS "Key_name",
       CASE WHEN ix.indisunique THEN '0' ELSE '1' END            AS "Non_unique",
       UPPER(am.amname)                                          AS "Index_type",
       a.attname                                                 AS "Column_name",
       kpos.ord                                                  AS "Seq_in_index",
       EXISTS (SELECT 1 FROM pg_inherits WHERE inhrelid = i.oid)::int AS "Inherited"
FROM pg_index ix
JOIN pg_class i  ON i.oid = ix.indexrelid
JOIN pg_class t  ON t.oid = ix.indrelid
JOIN pg_namespace n ON n.oid = t.relnamespace
JOIN pg_am am ON am.oid = i.relam
JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS kpos(attnum, ord) ON TRUE
JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = kpos.attnum
WHERE n.nspname = 'public' AND t.relname = 'payment_p2022_01'
ORDER BY (CASE WHEN ix.indisprimary THEN 0 ELSE 1 END), i.relname, kpos.ord;
