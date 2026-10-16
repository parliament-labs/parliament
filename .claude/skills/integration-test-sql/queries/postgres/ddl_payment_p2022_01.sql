-- Locks down partition reconstruction for a PARTITION (child of payment).
-- Replaces the column body with `PARTITION OF parent <bound>` since the
-- columns are inherited.
WITH
tbl AS (
    SELECT c.oid, c.relname, n.nspname, c.relpersistence, c.relkind
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public' AND c.relname = 'payment_p2022_01'
),
parts AS (
    SELECT t.relispartition                       AS is_partition,
           t.relkind = 'p'                        AS is_partitioned_parent,
           pg_get_partkeydef(t.oid)               AS partkey,
           pg_get_expr(t.relpartbound, t.oid)     AS bound,
           parent_n.nspname                       AS parent_schema,
           parent_c.relname                       AS parent_name
    FROM pg_class t
    LEFT JOIN pg_inherits inh ON inh.inhrelid = t.oid AND t.relispartition
    LEFT JOIN pg_class    parent_c ON parent_c.oid = inh.inhparent
    LEFT JOIN pg_namespace parent_n ON parent_n.oid = parent_c.relnamespace
    WHERE t.oid = (SELECT oid FROM tbl)
),
cols AS (
    SELECT a.attname,
           pg_catalog.format_type(a.atttypid, a.atttypmod) AS col_type,
           CASE WHEN a.attnotnull THEN ' NOT NULL' ELSE '' END AS nn,
           CASE WHEN a.atthasdef
                THEN ' DEFAULT ' || pg_catalog.pg_get_expr(d.adbin, d.adrelid)
                ELSE '' END AS col_default,
           '' AS identity, '' AS generated,
           a.attnum
    FROM pg_attribute a
    LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
    WHERE a.attrelid = (SELECT oid FROM tbl)
      AND a.attnum > 0 AND NOT a.attisdropped
    ORDER BY a.attnum
),
cons AS (
    SELECT '    CONSTRAINT ' || quote_ident(c.conname) || ' ' ||
           pg_catalog.pg_get_constraintdef(c.oid, true) AS def,
           CASE c.contype WHEN 'p' THEN 0 WHEN 'u' THEN 1
                WHEN 'f' THEN 2 WHEN 'c' THEN 3 ELSE 4 END AS sort_order,
           c.conname
    FROM pg_constraint c
    WHERE c.conrelid = (SELECT oid FROM tbl)
      AND c.contype <> 'n'
    ORDER BY sort_order, c.conname
),
col_lines AS (
    SELECT string_agg(
        ('    ' || quote_ident(attname) || ' ' || col_type ||
         col_default || nn),
        ',' || chr(10) ORDER BY attnum
    ) AS body FROM cols
),
con_lines AS (
    SELECT string_agg(def, ',' || chr(10) ORDER BY sort_order, conname) AS body FROM cons
)
SELECT 'CREATE TABLE ' ||
       quote_ident((SELECT nspname FROM tbl)) || '.' ||
       quote_ident((SELECT relname FROM tbl)) ||
       CASE WHEN (SELECT is_partition FROM parts) THEN
           ' PARTITION OF ' || quote_ident((SELECT parent_schema FROM parts)) || '.' ||
           quote_ident((SELECT parent_name FROM parts)) || chr(10) || '    ' ||
           COALESCE((SELECT bound FROM parts), '') ||
           CASE WHEN (SELECT is_partitioned_parent FROM parts)
                THEN chr(10) || '    PARTITION BY ' || (SELECT partkey FROM parts)
                ELSE '' END ||
           ';'
       ELSE
           ' (' || chr(10) ||
           (SELECT body FROM col_lines) ||
           CASE WHEN (SELECT body FROM con_lines) IS NOT NULL
                THEN ',' || chr(10) || (SELECT body FROM con_lines)
                ELSE '' END ||
           chr(10) || ')' ||
           CASE WHEN (SELECT is_partitioned_parent FROM parts)
                THEN chr(10) || '    PARTITION BY ' || (SELECT partkey FROM parts)
                ELSE '' END ||
           ';'
       END AS "Create Table";
