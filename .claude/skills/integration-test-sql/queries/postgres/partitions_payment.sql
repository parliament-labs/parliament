-- Representative plane query for table_partitions_sql('public', 'payment') on Postgres.
-- Column-alias contract: Partition_schema / Partition_name / Partition_bound.
-- Lists direct partitions of a partitioned parent. Empty for non-partitioned tables.
-- `child_c.relispartition` guard restricts to declarative partitions; classical
-- INHERITS children would surface here with a NULL bound otherwise.
SELECT child_n.nspname                          AS "Partition_schema",
       child_c.relname                          AS "Partition_name",
       pg_get_expr(child_c.relpartbound, child_c.oid) AS "Partition_bound",
       child_c.reltuples::bigint                AS "Partition_rows",
       pg_relation_size(child_c.oid)::bigint    AS "Partition_data_length"
FROM pg_inherits inh
JOIN pg_class    parent_c ON parent_c.oid = inh.inhparent
JOIN pg_namespace parent_n ON parent_n.oid = parent_c.relnamespace
JOIN pg_class    child_c  ON child_c.oid  = inh.inhrelid
JOIN pg_namespace child_n ON child_n.oid  = child_c.relnamespace
WHERE parent_n.nspname = 'public' AND parent_c.relname = 'payment'
  AND child_c.relispartition
ORDER BY child_c.relname;
