-- Representative plane query for table_columns_sql('sakila', 'film') on MySQL.
-- Column-alias contract: Field / Type / Null / Default / Extra / Comment.
SELECT COLUMN_NAME    AS `Field`,
       COLUMN_TYPE    AS `Type`,
       IS_NULLABLE    AS `Null`,
       COALESCE(COLUMN_DEFAULT, '') AS `Default`,
       EXTRA          AS `Extra`,
       COLUMN_COMMENT AS `Comment`
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = 'sakila' AND TABLE_NAME = 'film'
ORDER BY ORDINAL_POSITION;
