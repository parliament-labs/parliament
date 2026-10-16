-- Representative plane query for ObjectKind::Tables on MySQL (Sakila).
-- VARBINARY text columns (ENGINE, TABLE_COMMENT) are the dialect gotcha.
SELECT TABLE_SCHEMA AS `Schema`,
       TABLE_NAME   AS `Name`,
       TABLE_TYPE   AS `Table_type`,
       TABLE_ROWS   AS `Rows`,
       ENGINE       AS `Engine`
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = 'sakila'
  AND TABLE_TYPE = 'BASE TABLE'
ORDER BY 1, 2;
