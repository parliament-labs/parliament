-- Representative plane query for table_relationship_ancestors_sql('sakila', 'customer') on MySQL.
-- Column-alias contract: Depth / Ref_schema / Ref_table / From_columns / To_columns / Via_schema / Via_table.
-- Returns parents (depth 1, FKs out of customer) and grand-parents (depth 2).
-- One row per FK constraint × bridge — composite FKs collapse via GROUP_CONCAT,
-- but distinct bridges to the same grand-parent appear as separate rows.
SELECT * FROM (
    SELECT 1 AS `Depth`,
           REFERENCED_TABLE_SCHEMA AS `Ref_schema`,
           REFERENCED_TABLE_NAME   AS `Ref_table`,
           GROUP_CONCAT(COLUMN_NAME ORDER BY ORDINAL_POSITION SEPARATOR ',') AS `From_columns`,
           GROUP_CONCAT(REFERENCED_COLUMN_NAME ORDER BY ORDINAL_POSITION SEPARATOR ',') AS `To_columns`,
           '' AS `Via_schema`,
           '' AS `Via_table`
    FROM information_schema.KEY_COLUMN_USAGE
    WHERE TABLE_SCHEMA = 'sakila' AND TABLE_NAME = 'customer'
      AND REFERENCED_TABLE_NAME IS NOT NULL
    GROUP BY CONSTRAINT_SCHEMA, CONSTRAINT_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME
    UNION ALL
    SELECT 2,
           kcu.REFERENCED_TABLE_SCHEMA,
           kcu.REFERENCED_TABLE_NAME,
           GROUP_CONCAT(kcu.COLUMN_NAME ORDER BY kcu.ORDINAL_POSITION SEPARATOR ','),
           GROUP_CONCAT(kcu.REFERENCED_COLUMN_NAME ORDER BY kcu.ORDINAL_POSITION SEPARATOR ','),
           kcu.TABLE_SCHEMA,
           kcu.TABLE_NAME
    FROM information_schema.KEY_COLUMN_USAGE kcu
    WHERE kcu.REFERENCED_TABLE_NAME IS NOT NULL
      AND EXISTS (
        SELECT 1 FROM information_schema.KEY_COLUMN_USAGE k1
        WHERE k1.TABLE_SCHEMA = 'sakila' AND k1.TABLE_NAME = 'customer'
          AND k1.REFERENCED_TABLE_SCHEMA = kcu.TABLE_SCHEMA
          AND k1.REFERENCED_TABLE_NAME   = kcu.TABLE_NAME
      )
    GROUP BY kcu.CONSTRAINT_SCHEMA, kcu.CONSTRAINT_NAME,
             kcu.TABLE_SCHEMA, kcu.TABLE_NAME,
             kcu.REFERENCED_TABLE_SCHEMA, kcu.REFERENCED_TABLE_NAME
) t
ORDER BY `Depth`, `Ref_schema`, `Ref_table`, `Via_schema`, `Via_table`;
