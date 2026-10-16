-- Representative plane query for table_partitions_sql('sakila', 'test_partitions') on MySQL.
-- Column-alias contract: Partition_schema / Partition_name / Partition_bound.
-- The bound reconstructs the SHOW CREATE TABLE syntax from PARTITION_METHOD
-- + PARTITION_DESCRIPTION; HASH/KEY partitions have no per-partition values
-- so their bound is empty.
-- Filters PARTITION_NAME IS NOT NULL because INFORMATION_SCHEMA.PARTITIONS
-- returns one all-NULL row per non-partitioned table.
-- Depends on the test_partitions fixture (RANGE on YEAR(created), 3 buckets).
SELECT TABLE_SCHEMA   AS `Partition_schema`,
       PARTITION_NAME AS `Partition_name`,
       CASE PARTITION_METHOD
           WHEN 'RANGE'         THEN CONCAT('VALUES LESS THAN (', PARTITION_DESCRIPTION, ')')
           WHEN 'RANGE COLUMNS' THEN CONCAT('VALUES LESS THAN (', PARTITION_DESCRIPTION, ')')
           WHEN 'LIST'          THEN CONCAT('VALUES IN (', PARTITION_DESCRIPTION, ')')
           WHEN 'LIST COLUMNS'  THEN CONCAT('VALUES IN (', PARTITION_DESCRIPTION, ')')
           ELSE ''
       END AS `Partition_bound`,
       CAST(IFNULL(TABLE_ROWS, 0)  AS UNSIGNED) AS `Partition_rows`,
       CAST(IFNULL(DATA_LENGTH, 0) AS UNSIGNED) AS `Partition_data_length`
FROM information_schema.PARTITIONS
WHERE TABLE_SCHEMA = 'sakila' AND TABLE_NAME = 'test_partitions'
  AND PARTITION_NAME IS NOT NULL
ORDER BY PARTITION_ORDINAL_POSITION;
