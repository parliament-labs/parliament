-- Representative plane query for table_columns_bulk_sql on MySQL.
-- Used by the ERD tab's "All Fields" mode to fetch every column of N
-- tables in a single round trip. Column-alias contract: Schema_, Table_,
-- Field, Ord, Is_pk. The case-insensitive parser in tyto/src/explorer.rs
-- (parse_erd_columns) reads these aliases by name, then groups by
-- (Schema_, Table_) and orders by Ord.
--
-- Two-table fixture intentionally exercises the tuple IN clause and
-- confirms PK detection across multiple tables in one query.
SELECT c.TABLE_SCHEMA AS `Schema_`,
       c.TABLE_NAME   AS `Table_`,
       c.COLUMN_NAME  AS `Field`,
       c.ORDINAL_POSITION AS `Ord`,
       CASE WHEN k.COLUMN_NAME IS NULL THEN 0 ELSE 1 END AS `Is_pk`
FROM information_schema.COLUMNS c
LEFT JOIN information_schema.KEY_COLUMN_USAGE k
  ON k.TABLE_SCHEMA   = c.TABLE_SCHEMA
 AND k.TABLE_NAME     = c.TABLE_NAME
 AND k.COLUMN_NAME    = c.COLUMN_NAME
 AND k.CONSTRAINT_NAME = 'PRIMARY'
WHERE (c.TABLE_SCHEMA, c.TABLE_NAME) IN (('sakila','city'),('sakila','country'))
ORDER BY c.TABLE_SCHEMA, c.TABLE_NAME, c.ORDINAL_POSITION;
