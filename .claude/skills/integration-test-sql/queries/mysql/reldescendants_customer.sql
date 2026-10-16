-- Representative plane query for table_relationship_descendants_sql('sakila', 'customer') on MySQL.
-- Column-alias contract: Depth / Ref_schema / Ref_table / From_columns / To_columns / Via_schema / Via_table.
-- Returns children (depth 1, FKs into customer) and grand-children (depth 2).
-- One row per FK constraint × bridge — composite FKs collapse via GROUP_CONCAT,
-- but distinct bridges to the same grand-child appear as separate rows.
SELECT * FROM (
    SELECT 1 AS `Depth`,
           TABLE_SCHEMA AS `Ref_schema`,
           TABLE_NAME   AS `Ref_table`,
           GROUP_CONCAT(COLUMN_NAME ORDER BY ORDINAL_POSITION SEPARATOR ',') AS `From_columns`,
           GROUP_CONCAT(REFERENCED_COLUMN_NAME ORDER BY ORDINAL_POSITION SEPARATOR ',') AS `To_columns`,
           '' AS `Via_schema`,
           '' AS `Via_table`
    FROM information_schema.KEY_COLUMN_USAGE
    WHERE REFERENCED_TABLE_SCHEMA = 'sakila' AND REFERENCED_TABLE_NAME = 'customer'
    GROUP BY CONSTRAINT_SCHEMA, CONSTRAINT_NAME, TABLE_SCHEMA, TABLE_NAME
    UNION ALL
    SELECT 2,
           kcu.TABLE_SCHEMA,
           kcu.TABLE_NAME,
           GROUP_CONCAT(kcu.COLUMN_NAME ORDER BY kcu.ORDINAL_POSITION SEPARATOR ','),
           GROUP_CONCAT(kcu.REFERENCED_COLUMN_NAME ORDER BY kcu.ORDINAL_POSITION SEPARATOR ','),
           kcu.REFERENCED_TABLE_SCHEMA,
           kcu.REFERENCED_TABLE_NAME
    FROM information_schema.KEY_COLUMN_USAGE kcu
    WHERE kcu.REFERENCED_TABLE_NAME IS NOT NULL
      AND EXISTS (
        SELECT 1 FROM information_schema.KEY_COLUMN_USAGE k1
        WHERE k1.REFERENCED_TABLE_SCHEMA = 'sakila' AND k1.REFERENCED_TABLE_NAME = 'customer'
          AND k1.TABLE_SCHEMA = kcu.REFERENCED_TABLE_SCHEMA
          AND k1.TABLE_NAME   = kcu.REFERENCED_TABLE_NAME
      )
    GROUP BY kcu.CONSTRAINT_SCHEMA, kcu.CONSTRAINT_NAME,
             kcu.TABLE_SCHEMA, kcu.TABLE_NAME,
             kcu.REFERENCED_TABLE_SCHEMA, kcu.REFERENCED_TABLE_NAME
) t
ORDER BY `Depth`, `Ref_schema`, `Ref_table`, `Via_schema`, `Via_table`;
