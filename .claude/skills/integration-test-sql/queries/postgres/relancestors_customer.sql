-- Representative plane query for table_relationship_ancestors_sql('public', 'customer') on Postgres.
-- Column-alias contract: Depth / Ref_schema / Ref_table / From_columns / To_columns / Via_schema / Via_table.
-- Returns parents (depth 1, FKs out of customer) and grand-parents (depth 2).
-- Partition-aware via the `fks` CTE: both ends of every FK row are normalised
-- to their partition root. FK column names harvested via inline scalar
-- subqueries that unnest con.conkey / con.confkey WITH ORDINALITY and
-- string_agg pg_attribute.attname in declaration order.
WITH t AS (
    SELECT c.oid FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relname = 'customer' AND n.nspname = 'public'
), fks AS (
    SELECT
        COALESCE(pg_partition_root(con.conrelid),  con.conrelid)  AS owner_root,
        COALESCE(pg_partition_root(con.confrelid), con.confrelid) AS ref_root,
        ( SELECT string_agg(a.attname, ',' ORDER BY ck.ord)
          FROM unnest(con.conkey) WITH ORDINALITY AS ck(attnum, ord)
          JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = ck.attnum
        ) AS from_cols,
        ( SELECT string_agg(a.attname, ',' ORDER BY ck.ord)
          FROM unnest(con.confkey) WITH ORDINALITY AS ck(attnum, ord)
          JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = ck.attnum
        ) AS to_cols
    FROM pg_constraint con
    WHERE con.contype = 'f'
)
SELECT * FROM (
    SELECT DISTINCT 1 AS "Depth",
           n.nspname AS "Ref_schema",
           c.relname AS "Ref_table",
           COALESCE(fks.from_cols, '') AS "From_columns",
           COALESCE(fks.to_cols, '')   AS "To_columns",
           ''::text                    AS "Via_schema",
           ''::text                    AS "Via_table"
    FROM fks
    JOIN pg_class     c ON c.oid = fks.ref_root
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE fks.owner_root = (SELECT oid FROM t)
    UNION ALL
    SELECT DISTINCT 2,
           n2.nspname,
           c2.relname,
           COALESCE(p2.from_cols, ''),
           COALESCE(p2.to_cols, ''),
           n_via.nspname,
           c_via.relname
    FROM fks p1
    JOIN fks p2 ON p2.owner_root = p1.ref_root
    JOIN pg_class     c2    ON c2.oid    = p2.ref_root
    JOIN pg_namespace n2    ON n2.oid    = c2.relnamespace
    JOIN pg_class     c_via ON c_via.oid = p1.ref_root
    JOIN pg_namespace n_via ON n_via.oid = c_via.relnamespace
    WHERE p1.owner_root = (SELECT oid FROM t)
) x
ORDER BY "Depth", "Ref_schema", "Ref_table", "Via_schema", "Via_table";
