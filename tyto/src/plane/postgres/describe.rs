//! psql's `\d <pattern>` — describe.c `describeTableDetails` and
//! `describeOneTableDetails` — so a Postgres shell prints exactly what
//! psql prints: the same catalog queries (psql 18, every branch it
//! takes for PostgreSQL 13 and later), the same title, columns and
//! footers, laid out by psql's printer.
//!
//! psql runs its queries one at a time; here they run as batches, one
//! round trip each (`MetaAction::Staged` → `IntrospectBatch`):
//!
//! 1. the server version and psql's lookup of the pattern;
//! 2. each matching relation's `pg_class` row;
//! 3. every section each relation needs, chosen from that row as psql
//!    chooses — columns, indexes, constraints, triggers, partitions …;
//! 4. only when an index lives in a non-default tablespace: its name.
//!
//! Every step re-reads the command and the replies so far; nothing is
//! kept between them. Results are read by position, as psql's
//! `PQgetvalue` reads them, with SQL NULL distinct from ''.

use shared::{BatchResult, ResultSet};

use super::pattern::NameMatch;
use super::print::{localize_numbers, print_table, Align, PrintTable};
use crate::plane::{MetaContext, MetaStep};
use crate::types::{Expanded, ExplorerInit, PrintOptions, PrintedLink};
use crate::{ObjectKind, ShellEntry};

/// psql's describe.c follows server versions back to 9.2; Parliament
/// supports PostgreSQL 13 and later.
const MIN_VERSION: i32 = 130_000;

/// One `\d` with a pattern (`S` only matters without one).
pub(super) struct Describe<'a> {
    pub pattern: &'a str,
    pub verbose: bool,
}

impl Describe<'_> {
    /// The first batch: the server version and the relations matching
    /// the pattern — or psql's refusal of the pattern.
    pub(super) fn first(&self, ctx: &MetaContext) -> Result<Vec<String>, String> {
        Ok(vec![
            "SELECT pg_catalog.current_setting('server_version_num')".into(),
            self.lookup_sql(ctx)?,
        ])
    }

    /// psql's `describeTableDetails` query.
    fn lookup_sql(&self, ctx: &MetaContext) -> Result<String, String> {
        // psql leaves system schemas out only without a pattern, which
        // `\d` lists instead (`listTables`), so never here.
        let mut buf = String::from(
            "SELECT c.oid,\n  n.nspname,\n  c.relname\nFROM pg_catalog.pg_class c\n     \
             LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n",
        );
        let relations = NameMatch {
            schema_var:      Some("n.nspname"),
            name_var:        Some("c.relname"),
            visibility_rule: Some("pg_catalog.pg_table_is_visible(c.oid)"),
            ..NameMatch::default()
        };
        relations.validate(&mut buf, Some(self.pattern), false, 3, &ctx.database).map_err(|e| e.to_string())?;
        buf.push_str("ORDER BY 2, 3;");
        Ok(buf)
    }

    /// The step after `replies` — another batch, or the output.
    pub(super) fn next(&self, ctx: &MetaContext, replies: &[BatchResult]) -> MetaStep {
        let Some(first) = replies.first() else { return MetaStep::Done(Vec::new()) };
        if let Some(error) = batch_error(first, &self.first(ctx).unwrap_or_default(), ctx) {
            return MetaStep::Failed(vec![error]);
        }
        let (Some(version), Some(found)) = (first.sets.first(), first.sets.get(1)) else {
            return MetaStep::Done(Vec::new());
        };
        let version: i32 = cell(version, 0, 0).parse().unwrap_or(0);
        if version < MIN_VERSION {
            return MetaStep::Failed(vec![ShellEntry::MetaOutput(vec![
                "\\d: describing relations needs PostgreSQL 13 or later.".into(),
            ])]);
        }
        // Nothing found: psql's word for it, unless `QUIET`.
        if found.rows.is_empty() {
            return MetaStep::Failed(if super::quiet(ctx) { Vec::new() } else { vec![ShellEntry::MetaOutput(vec![
                format!("Did not find any relation named \"{}\".", self.pattern),
            ])] });
        }
        let found: Vec<Found> = (0..found.rows.len()).map(|i| Found {
            oid:    cell(found, i, 0).to_string(),
            schema: cell(found, i, 1).to_string(),
            name:   cell(found, i, 2).to_string(),
        }).collect();

        // Stage 2: each relation's pg_class row.
        let stage2: Vec<String> = found.iter().map(|f| self.relinfo_sql(&f.oid)).collect();
        let Some(infos) = replies.get(1) else { return MetaStep::Run(stage2) };
        let rels: Vec<Option<RelInfo>> = found.iter().zip(&infos.sets)
            .map(|(f, set)| RelInfo::read(f, set))
            .collect();

        // Stage 3: every section of every relation.
        let plans: Vec<Vec<Section>> = rels.iter()
            .map(|rel| rel.as_ref().map(|r| plan(r, self.verbose)).unwrap_or_default())
            .collect();
        let stage3: Vec<String> = rels.iter().zip(&plans)
            .filter_map(|(rel, plan)| rel.as_ref().map(|r| (r, plan)))
            .flat_map(|(rel, plan)| plan.iter().map(|s| s.sql(rel, version, self.verbose)).collect::<Vec<_>>())
            .collect();
        let Some(sections) = replies.get(2) else {
            if let Some(error) = batch_error(infos, &stage2, ctx) {
                return MetaStep::Failed(vec![error]);
            }
            return MetaStep::Run(stage3);
        };

        // Each relation's results, in plan order.
        let mut results = sections.sets.iter();
        let mut described: Vec<(&RelInfo, Results)> = Vec::new();
        for (rel, plan) in rels.iter().zip(&plans) {
            let Some(rel) = rel else { continue };
            let sets: Vec<&ResultSet> = results.by_ref().take(plan.len()).collect();
            if sets.len() < plan.len() { break; } // the error stopped the batch here
            described.push((rel, Results { plan: plan.clone(), sets }));
        }

        // Stage 4, rarely: the names of non-default tablespaces indexes
        // live in. One psql can't find (dropped meanwhile) is left out,
        // as psql leaves it out.
        let mut index_spaces: Vec<String> = described.iter()
            .flat_map(|(_, r)| r.index_tablespaces())
            .collect();
        index_spaces.sort();
        index_spaces.dedup();
        let stage4: Vec<String> = index_spaces.iter().map(|oid| tablespace_sql(oid)).collect();
        let names: Vec<(String, String)> = match replies.get(3) {
            None if !stage4.is_empty() && sections.error.is_none() => return MetaStep::Run(stage4),
            Some(reply) => index_spaces.iter().zip(&reply.sets)
                .filter(|(_, set)| !set.rows.is_empty())
                .map(|(oid, set)| (oid.clone(), cell(set, 0, 0).to_string()))
                .collect(),
            None => Vec::new(),
        };

        let mut lines = Vec::new();
        let mut links = Vec::new();
        for (rel, results) in &described {
            let start = lines.len();
            let (mut table, link) = render(rel, results, &names, self.verbose, version);
            // In the format `\pset format` says; expanded only for a
            // sequence — psql finds the rest "confusing in expanded mode"
            // — which psql prints as a result (`printQuery`): its numbers
            // in the client's locale under `\pset numericlocale`.
            let options = match rel.relkind {
                'S' => {
                    localize_numbers(&mut table, &ctx.print);
                    ctx.print.clone()
                }
                _ => PrintOptions { expanded: Expanded::Off, ..ctx.print.clone() },
            };
            lines.extend(print_table(&table, &options));
            if let Some(link) = link.filter(|_| !ctx.print.tuples_only) {
                links.extend(link.place(&lines[start..], start, &ctx.database));
            }
        }
        let mut out = Vec::new();
        if !lines.is_empty() {
            out.push(ShellEntry::Printed { lines, links });
        }
        let error = batch_error(sections, &stage3, ctx)
            .or_else(|| replies.get(3).and_then(|reply| batch_error(reply, &stage4, ctx)));
        if let Some(error) = error {
            out.push(error);
        } else if let Some(missing) = found.iter().zip(&rels).find(|(_, r)| r.is_none()).filter(|_| !super::quiet(ctx)) {
            out.push(ShellEntry::MetaOutput(vec![format!("Did not find any relation with OID {}.", missing.0.oid)]));
        }
        MetaStep::Done(out)
    }

    fn relinfo_sql(&self, oid: &str) -> String {
        let reloptions = if self.verbose {
            "pg_catalog.array_to_string(c.reloptions || array(select 'toast.' || x from pg_catalog.unnest(tc.reloptions) x), ', ')\n"
        } else {
            "''"
        };
        format!(
            "SELECT c.relchecks, c.relkind, c.relhasindex, c.relhasrules, c.relhastriggers, c.relrowsecurity, \
             c.relforcerowsecurity, false AS relhasoids, c.relispartition, {reloptions}, c.reltablespace, \
             CASE WHEN c.reloftype = 0 THEN '' ELSE c.reloftype::pg_catalog.regtype::pg_catalog.text END, \
             c.relpersistence, c.relreplident, am.amname\n\
             FROM pg_catalog.pg_class c\n \
             LEFT JOIN pg_catalog.pg_class tc ON (c.reltoastrelid = tc.oid)\n\
             LEFT JOIN pg_catalog.pg_am am ON (c.relam = am.oid)\n\
             WHERE c.oid = '{oid}';"
        )
    }
}

/// A batch's error as psql prints it (`VERBOSITY` honoured), its
/// position counted into the query that failed — the one after those
/// that completed.
pub(super) fn batch_error(reply: &BatchResult, queries: &[String], ctx: &MetaContext) -> Option<ShellEntry> {
    let error = reply.error.as_ref()?;
    let failed = queries.get(reply.sets.len()).map(String::as_str);
    Some(ShellEntry::CliError {
        lines:   super::error::psql_error(error, failed, ctx.error_style),
        message: error.message.clone(),
    })
}

/// A relation the lookup found.
struct Found {
    oid:    String,
    schema: String,
    name:   String,
}

/// psql's `tableinfo`: a relation's `pg_class` row.
#[derive(Clone, Debug)]
struct RelInfo {
    oid:              String,
    schema:           String,
    name:             String,
    checks:           i32,
    relkind:          char,
    hasindex:         bool,
    hasrules:         bool,
    hastriggers:      bool,
    rowsecurity:      bool,
    forcerowsecurity: bool,
    ispartition:      bool,
    reloptions:       String,
    tablespace:       String,
    reloftype:        Option<String>,
    relpersistence:   char,
    relreplident:     char,
    relam:            Option<String>,
}

impl RelInfo {
    fn read(found: &Found, set: &ResultSet) -> Option<RelInfo> {
        if set.rows.is_empty() { return None; }
        let ch = |col| cell(set, 0, col).chars().next().unwrap_or('\0');
        Some(RelInfo {
            oid:              found.oid.clone(),
            schema:           found.schema.clone(),
            name:             found.name.clone(),
            checks:           cell(set, 0, 0).parse().unwrap_or(0),
            relkind:          ch(1),
            hasindex:         is_true(set, 0, 2),
            hasrules:         is_true(set, 0, 3),
            hastriggers:      is_true(set, 0, 4),
            rowsecurity:      is_true(set, 0, 5),
            forcerowsecurity: is_true(set, 0, 6),
            ispartition:      is_true(set, 0, 8),
            reloptions:       cell(set, 0, 9).to_string(),
            tablespace:       cell(set, 0, 10).to_string(),
            reloftype:        Some(cell(set, 0, 11).to_string()).filter(|t| !t.is_empty()),
            relpersistence:   ch(12),
            relreplident:     ch(13),
            relam:            (!is_null(set, 0, 14)).then(|| cell(set, 0, 14).to_string()),
        })
    }

    fn kind_is(&self, kinds: &str) -> bool { kinds.contains(self.relkind) }

    /// psql's `show_column_details`: Collation / Nullable / Default.
    fn column_details(&self) -> bool { self.kind_is("rvmfcp") }

    fn has_tablespace(&self) -> bool { !self.tablespace.is_empty() && self.tablespace != "0" }
}

/// One catalog query a relation's description needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Sequence,
    SequenceOwner,
    Columns,
    PartitionOf,
    PartitionKey,
    ToastOwner,
    IndexInfo,
    /// Not psql's: the kind of an index's table, for the link to it.
    IndexTableKind,
    Indexes,
    Checks,
    ForeignKeys,
    ReferencedBy,
    Policies,
    StatsObjects,
    Rules,
    Publications,
    NotNull,
    ViewDef,
    ViewRules,
    Triggers,
    ForeignServer,
    Inherits,
    Children,
    Tablespace,
}

/// The sections psql queries for `rel`, in its order.
fn plan(rel: &RelInfo, verbose: bool) -> Vec<Section> {
    use Section::*;
    if rel.relkind == 'S' {
        return vec![Sequence, SequenceOwner];
    }
    let mut plan = vec![Columns];
    if rel.ispartition { plan.push(PartitionOf); }
    if rel.relkind == 'p' { plan.push(PartitionKey); }
    if rel.relkind == 't' { plan.push(ToastOwner); }
    if rel.kind_is("iI") {
        plan.extend([IndexInfo, IndexTableKind]);
        if rel.relkind == 'i' && rel.has_tablespace() { plan.push(Tablespace); }
    } else if rel.kind_is("rmfpt") {
        if rel.hasindex { plan.push(Indexes); }
        if rel.checks > 0 { plan.push(Checks); }
        plan.extend([ForeignKeys, ReferencedBy, Policies, StatsObjects]);
        if rel.hasrules && rel.relkind != 'm' { plan.push(Rules); }
        plan.push(Publications);
        if verbose { plan.push(NotNull); }
    }
    if rel.kind_is("vm") && verbose {
        plan.push(ViewDef);
        if rel.hasrules { plan.push(ViewRules); }
    }
    if rel.hastriggers { plan.push(Triggers); }
    if rel.kind_is("rmfpIt") {
        if rel.relkind == 'f' { plan.push(ForeignServer); }
        plan.extend([Inherits, Children]);
        if rel.has_tablespace() { plan.push(Tablespace); }
    }
    plan
}

/// Which optional columns the columns query fetches, in psql's order.
struct ColumnLayout {
    details:     bool,
    is_key:      bool,
    indexdef:    bool,
    fdwopts:     bool,
    storage:     bool,
    compression: bool,
    stattarget:  bool,
    descr:       bool,
}

impl ColumnLayout {
    fn new(rel: &RelInfo, verbose: bool, version: i32) -> ColumnLayout {
        let index = rel.kind_is("iI");
        ColumnLayout {
            details:     rel.column_details(),
            is_key:      index,
            indexdef:    index,
            fdwopts:     rel.relkind == 'f',
            storage:     verbose,
            compression: verbose && version >= 140_000 && rel.kind_is("rpm"),
            stattarget:  verbose && rel.kind_is("riImfp"),
            descr:       verbose && rel.kind_is("rvmfcp"),
        }
    }
}

impl Section {
    /// psql's query for this section of `rel`.
    fn sql(self, rel: &RelInfo, version: i32, verbose: bool) -> String {
        let oid = &rel.oid;
        match self {
            Section::Sequence => format!(
                "SELECT pg_catalog.format_type(seqtypid, NULL) AS \"Type\",\n       \
                 seqstart AS \"Start\",\n       seqmin AS \"Minimum\",\n       seqmax AS \"Maximum\",\n       \
                 seqincrement AS \"Increment\",\n       \
                 CASE WHEN seqcycle THEN 'yes' ELSE 'no' END AS \"Cycles?\",\n       \
                 seqcache AS \"Cache\"\nFROM pg_catalog.pg_sequence\nWHERE seqrelid = '{oid}';"
            ),
            Section::SequenceOwner => format!(
                "SELECT pg_catalog.quote_ident(nspname) || '.' ||\n   pg_catalog.quote_ident(relname) || '.' ||\n   \
                 pg_catalog.quote_ident(attname),\n   d.deptype\nFROM pg_catalog.pg_class c\n\
                 INNER JOIN pg_catalog.pg_depend d ON c.oid=d.refobjid\n\
                 INNER JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace\n\
                 INNER JOIN pg_catalog.pg_attribute a ON (\n a.attrelid=c.oid AND\n a.attnum=d.refobjsubid)\n\
                 WHERE d.classid='pg_catalog.pg_class'::pg_catalog.regclass\n \
                 AND d.refclassid='pg_catalog.pg_class'::pg_catalog.regclass\n \
                 AND d.objid='{oid}'\n AND d.deptype IN ('a', 'i')"
            ),
            Section::Columns => columns_sql(rel, version, verbose),
            Section::PartitionOf => format!(
                "SELECT inhparent::pg_catalog.regclass,\n  pg_catalog.pg_get_expr(c.relpartbound, c.oid),\n  {}{}\n\
                 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_inherits i ON c.oid = inhrelid\nWHERE c.oid = '{oid}';",
                if version >= 140_000 { "inhdetachpending" } else { "false as inhdetachpending" },
                if verbose { ",\n  pg_catalog.pg_get_partition_constraintdef(c.oid)" } else { "" },
            ),
            Section::PartitionKey => format!("SELECT pg_catalog.pg_get_partkeydef('{oid}'::pg_catalog.oid);"),
            Section::ToastOwner => format!(
                "SELECT n.nspname, c.relname\nFROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n \
                 ON n.oid = c.relnamespace\nWHERE reltoastrelid = '{oid}';"
            ),
            Section::IndexInfo => format!(
                "SELECT i.indisunique, i.indisprimary, i.indisclustered, i.indisvalid,\n  \
                 (NOT i.indimmediate) AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid = i.indrelid \
                 AND conindid = i.indexrelid AND contype IN ('p','u','x') AND condeferrable) AS condeferrable,\n  \
                 (NOT i.indimmediate) AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid = i.indrelid \
                 AND conindid = i.indexrelid AND contype IN ('p','u','x') AND condeferred) AS condeferred,\n\
                 i.indisreplident,\n{}  a.amname, c2.relname, pg_catalog.pg_get_expr(i.indpred, i.indrelid, true)\n\
                 FROM pg_catalog.pg_index i, pg_catalog.pg_class c, pg_catalog.pg_class c2, pg_catalog.pg_am a\n\
                 WHERE i.indexrelid = c.oid AND c.oid = '{oid}' AND c.relam = a.oid\nAND i.indrelid = c2.oid;",
                if version >= 150_000 { "i.indnullsnotdistinct,\n" } else { "false AS indnullsnotdistinct,\n" },
            ),
            Section::IndexTableKind => format!(
                "SELECT c2.relkind FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class c2 ON c2.oid = i.indrelid \
                 WHERE i.indexrelid = '{oid}';"
            ),
            Section::Indexes => format!(
                "SELECT c2.relname, i.indisprimary, i.indisunique, i.indisclustered, i.indisvalid, \
                 pg_catalog.pg_get_indexdef(i.indexrelid, 0, true),\n  pg_catalog.pg_get_constraintdef(con.oid, true), \
                 contype, condeferrable, condeferred, i.indisreplident, c2.reltablespace, {}\n\
                 FROM pg_catalog.pg_class c, pg_catalog.pg_class c2, pg_catalog.pg_index i\n  \
                 LEFT JOIN pg_catalog.pg_constraint con ON (conrelid = i.indrelid AND conindid = i.indexrelid \
                 AND contype IN ('p','u','x'))\n\
                 WHERE c.oid = '{oid}' AND c.oid = i.indrelid AND i.indexrelid = c2.oid\n\
                 ORDER BY i.indisprimary DESC, c2.relname;",
                if version >= 180_000 { "con.conperiod" } else { "false AS conperiod" },
            ),
            Section::Checks => format!(
                "SELECT r.conname, pg_catalog.pg_get_constraintdef(r.oid, true)\nFROM pg_catalog.pg_constraint r\n\
                 WHERE r.conrelid = '{oid}' AND r.contype = 'c'\nORDER BY 1;"
            ),
            Section::ForeignKeys if rel.ispartition || rel.relkind == 'p' => format!(
                "SELECT conrelid = '{oid}'::pg_catalog.regclass AS sametable,\n       conname,\n       \
                 pg_catalog.pg_get_constraintdef(oid, true) AS condef,\n       conrelid::pg_catalog.regclass AS ontable\n  \
                 FROM pg_catalog.pg_constraint,\n       pg_catalog.pg_partition_ancestors('{oid}')\n \
                 WHERE conrelid = relid AND contype = 'f' AND conparentid = 0\nORDER BY sametable DESC, conname;"
            ),
            Section::ForeignKeys => format!(
                "SELECT true as sametable, conname,\n  pg_catalog.pg_get_constraintdef(r.oid, true) as condef,\n  \
                 conrelid::pg_catalog.regclass AS ontable\nFROM pg_catalog.pg_constraint r\n\
                 WHERE r.conrelid = '{oid}' AND r.contype = 'f'\n     AND conparentid = 0\nORDER BY conname"
            ),
            Section::ReferencedBy => format!(
                "SELECT conname, conrelid::pg_catalog.regclass AS ontable,\n       \
                 pg_catalog.pg_get_constraintdef(oid, true) AS condef\n  FROM pg_catalog.pg_constraint c\n \
                 WHERE confrelid IN (SELECT pg_catalog.pg_partition_ancestors('{oid}')\n                     \
                 UNION ALL VALUES ('{oid}'::pg_catalog.regclass))\n       AND contype = 'f' AND conparentid = 0\n\
                 ORDER BY conname;"
            ),
            Section::Policies => format!(
                "SELECT pol.polname, pol.polpermissive,\n  CASE WHEN pol.polroles = '{{0}}' THEN NULL ELSE \
                 pg_catalog.array_to_string(array(select rolname from pg_catalog.pg_roles where oid = any (pol.polroles) \
                 order by 1),',') END,\n  pg_catalog.pg_get_expr(pol.polqual, pol.polrelid),\n  \
                 pg_catalog.pg_get_expr(pol.polwithcheck, pol.polrelid),\n  CASE pol.polcmd\n    WHEN 'r' THEN 'SELECT'\n    \
                 WHEN 'a' THEN 'INSERT'\n    WHEN 'w' THEN 'UPDATE'\n    WHEN 'd' THEN 'DELETE'\n    END AS cmd\n\
                 FROM pg_catalog.pg_policy pol\nWHERE pol.polrelid = '{oid}' ORDER BY 1;"
            ),
            Section::StatsObjects if version >= 140_000 => format!(
                "SELECT oid, stxrelid::pg_catalog.regclass, stxnamespace::pg_catalog.regnamespace::pg_catalog.text AS nsp, \
                 stxname,\npg_catalog.pg_get_statisticsobjdef_columns(oid) AS columns,\n  'd' = any(stxkind) AS ndist_enabled,\n  \
                 'f' = any(stxkind) AS deps_enabled,\n  'm' = any(stxkind) AS mcv_enabled,\nstxstattarget\n\
                 FROM pg_catalog.pg_statistic_ext\nWHERE stxrelid = '{oid}'\nORDER BY nsp, stxname;"
            ),
            Section::StatsObjects => format!(
                "SELECT oid, stxrelid::pg_catalog.regclass, stxnamespace::pg_catalog.regnamespace AS nsp, stxname,\n  \
                 (SELECT pg_catalog.string_agg(pg_catalog.quote_ident(attname),', ')\n   \
                 FROM pg_catalog.unnest(stxkeys) s(attnum)\n   JOIN pg_catalog.pg_attribute a ON (stxrelid = a.attrelid AND\n        \
                 a.attnum = s.attnum AND NOT attisdropped)) AS columns,\n  'd' = any(stxkind) AS ndist_enabled,\n  \
                 'f' = any(stxkind) AS deps_enabled,\n  'm' = any(stxkind) AS mcv_enabled,\n  stxstattarget\n\
                 FROM pg_catalog.pg_statistic_ext\nWHERE stxrelid = '{oid}'\nORDER BY 1;"
            ),
            Section::Rules => format!(
                "SELECT r.rulename, trim(trailing ';' from pg_catalog.pg_get_ruledef(r.oid, true)), ev_enabled\n\
                 FROM pg_catalog.pg_rewrite r\nWHERE r.ev_class = '{oid}' ORDER BY 1;"
            ),
            Section::Publications if version >= 150_000 => format!(
                "SELECT pubname\n     , NULL\n     , NULL\nFROM pg_catalog.pg_publication p\n     \
                 JOIN pg_catalog.pg_publication_namespace pn ON p.oid = pn.pnpubid\n     \
                 JOIN pg_catalog.pg_class pc ON pc.relnamespace = pn.pnnspid\n\
                 WHERE pc.oid ='{oid}' and pg_catalog.pg_relation_is_publishable('{oid}')\nUNION\n\
                 SELECT pubname\n     , pg_get_expr(pr.prqual, c.oid)\n     , (CASE WHEN pr.prattrs IS NOT NULL THEN\n         \
                 (SELECT string_agg(attname, ', ')\n           \
                 FROM pg_catalog.generate_series(0, pg_catalog.array_upper(pr.prattrs::pg_catalog.int2[], 1)) s,\n                \
                 pg_catalog.pg_attribute\n          WHERE attrelid = pr.prrelid AND attnum = prattrs[s])\n        ELSE NULL END) \
                 FROM pg_catalog.pg_publication p\n     JOIN pg_catalog.pg_publication_rel pr ON p.oid = pr.prpubid\n     \
                 JOIN pg_catalog.pg_class c ON c.oid = pr.prrelid\nWHERE pr.prrelid = '{oid}'\nUNION\n\
                 SELECT pubname\n     , NULL\n     , NULL\nFROM pg_catalog.pg_publication p\n\
                 WHERE p.puballtables AND pg_catalog.pg_relation_is_publishable('{oid}')\nORDER BY 1;"
            ),
            Section::Publications => format!(
                "SELECT pubname\n     , NULL\n     , NULL\nFROM pg_catalog.pg_publication p\n\
                 JOIN pg_catalog.pg_publication_rel pr ON p.oid = pr.prpubid\nWHERE pr.prrelid = '{oid}'\nUNION ALL\n\
                 SELECT pubname\n     , NULL\n     , NULL\nFROM pg_catalog.pg_publication p\n\
                 WHERE p.puballtables AND pg_catalog.pg_relation_is_publishable('{oid}')\nORDER BY 1;"
            ),
            Section::NotNull => format!(
                "SELECT c.conname, a.attname, c.connoinherit,\n  c.conislocal, c.coninhcount <> 0,\n  c.convalidated\n\
                 FROM pg_catalog.pg_constraint c JOIN\n  pg_catalog.pg_attribute a ON\n    \
                 (a.attrelid = c.conrelid AND a.attnum = c.conkey[1])\nWHERE c.contype = 'n' AND\n  \
                 c.conrelid = '{oid}'::pg_catalog.regclass\nORDER BY a.attnum"
            ),
            Section::ViewDef => format!("SELECT pg_catalog.pg_get_viewdef('{oid}'::pg_catalog.oid, true);"),
            Section::ViewRules => format!(
                "SELECT r.rulename, trim(trailing ';' from pg_catalog.pg_get_ruledef(r.oid, true))\n\
                 FROM pg_catalog.pg_rewrite r\nWHERE r.ev_class = '{oid}' AND r.rulename != '_RETURN' ORDER BY 1;"
            ),
            Section::Triggers => format!(
                "SELECT t.tgname, pg_catalog.pg_get_triggerdef(t.oid, true), t.tgenabled, t.tgisinternal,\n  \
                 CASE WHEN t.tgparentid != 0 THEN\n    (SELECT u.tgrelid::pg_catalog.regclass\n     \
                 FROM pg_catalog.pg_trigger AS u,\n          \
                 pg_catalog.pg_partition_ancestors(t.tgrelid) WITH ORDINALITY AS a(relid, depth)\n     \
                 WHERE u.tgname = t.tgname AND u.tgrelid = a.relid\n           AND u.tgparentid = 0\n     \
                 ORDER BY a.depth LIMIT 1)\n  END AS parent\nFROM pg_catalog.pg_trigger t\nWHERE t.tgrelid = '{oid}' AND {}\n\
                 ORDER BY 1;",
                // Servers before 15 mark partitions' inherited triggers
                // internal; psql still shows them.
                if version < 150_000 {
                    "(NOT t.tgisinternal OR (t.tgisinternal AND t.tgenabled = 'D') \n    \
                     OR EXISTS (SELECT 1 FROM pg_catalog.pg_depend WHERE objid = t.oid \n        \
                     AND refclassid = 'pg_catalog.pg_trigger'::pg_catalog.regclass))"
                } else {
                    "(NOT t.tgisinternal OR (t.tgisinternal AND t.tgenabled = 'D'))"
                },
            ),
            Section::ForeignServer => format!(
                "SELECT s.srvname,\n  pg_catalog.array_to_string(ARRAY(\n    SELECT pg_catalog.quote_ident(option_name) \
                 || ' ' || pg_catalog.quote_literal(option_value)\n    FROM pg_catalog.pg_options_to_table(ftoptions)),  ', ')\n\
                 FROM pg_catalog.pg_foreign_table f,\n     pg_catalog.pg_foreign_server s\n\
                 WHERE f.ftrelid = '{oid}' AND s.oid = f.ftserver;"
            ),
            Section::Inherits => format!(
                "SELECT c.oid::pg_catalog.regclass\nFROM pg_catalog.pg_class c, pg_catalog.pg_inherits i\n\
                 WHERE c.oid = i.inhparent AND i.inhrelid = '{oid}'\n  AND c.relkind != 'p' AND c.relkind != 'I'\n\
                 ORDER BY inhseqno;"
            ),
            Section::Children => format!(
                "SELECT c.oid::pg_catalog.regclass, c.relkind, {}, pg_catalog.pg_get_expr(c.relpartbound, c.oid)\n\
                 FROM pg_catalog.pg_class c, pg_catalog.pg_inherits i\nWHERE c.oid = i.inhrelid AND i.inhparent = '{oid}'\n\
                 ORDER BY pg_catalog.pg_get_expr(c.relpartbound, c.oid) = 'DEFAULT', c.oid::pg_catalog.regclass::pg_catalog.text;",
                if version >= 140_000 { "inhdetachpending" } else { "false AS inhdetachpending" },
            ),
            Section::Tablespace => tablespace_sql(&rel.tablespace),
        }
    }
}

fn tablespace_sql(oid: &str) -> String {
    format!("SELECT spcname FROM pg_catalog.pg_tablespace\nWHERE oid = '{oid}';")
}

/// psql's per-column query, its columns chosen by [`ColumnLayout`].
fn columns_sql(rel: &RelInfo, version: i32, verbose: bool) -> String {
    let layout = ColumnLayout::new(rel, verbose, version);
    let oid = &rel.oid;
    let mut buf = String::from("SELECT a.attname,\n  pg_catalog.format_type(a.atttypid, a.atttypmod)");
    if layout.details {
        // "Pretty" expressions, without excess parentheses.
        buf.push_str(",\n  (SELECT pg_catalog.pg_get_expr(d.adbin, d.adrelid, true)\n   FROM pg_catalog.pg_attrdef d\n   \
                      WHERE d.adrelid = a.attrelid AND d.adnum = a.attnum AND a.atthasdef),\n  a.attnotnull");
        buf.push_str(",\n  (SELECT c.collname FROM pg_catalog.pg_collation c, pg_catalog.pg_type t\n   \
                      WHERE c.oid = a.attcollation AND t.oid = a.atttypid AND a.attcollation <> t.typcollation) AS attcollation");
        buf.push_str(",\n  a.attidentity,\n  a.attgenerated");
    }
    if layout.is_key {
        buf.push_str(&format!(",\n  CASE WHEN a.attnum <= (SELECT i.indnkeyatts FROM pg_catalog.pg_index i \
                               WHERE i.indexrelid = '{oid}') THEN 'yes' ELSE 'no' END AS is_key"));
    }
    if layout.indexdef {
        buf.push_str(",\n  pg_catalog.pg_get_indexdef(a.attrelid, a.attnum, TRUE) AS indexdef");
    }
    if layout.fdwopts {
        buf.push_str(",\n  CASE WHEN attfdwoptions IS NULL THEN '' ELSE   '(' || pg_catalog.array_to_string(ARRAY(\
                      SELECT pg_catalog.quote_ident(option_name) || ' ' || pg_catalog.quote_literal(option_value)  FROM   \
                      pg_catalog.pg_options_to_table(attfdwoptions)), ', ') || ')' END AS attfdwoptions");
    }
    if layout.storage {
        buf.push_str(",\n  a.attstorage");
    }
    if layout.compression {
        buf.push_str(",\n  a.attcompression AS attcompression");
    }
    if layout.stattarget {
        buf.push_str(",\n  CASE WHEN a.attstattarget=-1 THEN NULL ELSE a.attstattarget END AS attstattarget");
    }
    if layout.descr {
        buf.push_str(",\n  pg_catalog.col_description(a.attrelid, a.attnum)");
    }
    buf.push_str(&format!("\nFROM pg_catalog.pg_attribute a\nWHERE a.attrelid = '{oid}' AND a.attnum > 0 \
                           AND NOT a.attisdropped\nORDER BY a.attnum;"));
    buf
}

/// A relation's section results, in plan order.
struct Results<'a> {
    plan: Vec<Section>,
    sets: Vec<&'a ResultSet>,
}

impl<'a> Results<'a> {
    fn get(&self, section: Section) -> Option<&'a ResultSet> {
        self.plan.iter().position(|s| *s == section).map(|i| self.sets[i])
    }

    /// The non-default tablespaces this relation's indexes live in.
    fn index_tablespaces(&self) -> Vec<String> {
        self.get(Section::Indexes)
            .map(|set| (0..set.rows.len()).map(|i| cell(set, i, 11).to_string()).filter(|t| t != "0" && !t.is_empty()).collect())
            .unwrap_or_default()
    }
}

/// Where the explorer link goes in a relation's printed block.
enum LinkAt {
    /// The title (tables, partitioned tables, TOAST tables, views).
    Title { title: String, kind: ObjectKind, schema: String, name: String },
    /// `s.t` in an index's `for table "s.t"` footer.
    Footer { schema: String, name: String },
}

impl LinkAt {
    /// The link's position in `lines` (one relation's block, starting
    /// at `offset` in the entry).
    fn place(self, lines: &[String], offset: usize, database: &str) -> Option<PrintedLink> {
        let (needle, skip, kind, schema, name) = match self {
            LinkAt::Title { title, kind, schema, name } => (title, 0, kind, schema, name),
            LinkAt::Footer { schema, name } => {
                let prefix = "for table \"";
                (format!("{prefix}{schema}.{name}\""), prefix.len(), ObjectKind::Tables, schema, name)
            }
        };
        let (line, at) = lines.iter().enumerate()
            .find_map(|(i, l)| l.find(&needle).map(|at| (i, at)))?;
        // A footer link covers `s.t`, not the words or quotes around it.
        let start = at + skip;
        let end = if skip == 0 { at + needle.len() } else { at + needle.len() - 1 };
        Some(PrintedLink {
            line: offset + line,
            start,
            end,
            open: ExplorerInit {
                database: Some(database.to_string()).filter(|d| !d.is_empty()),
                kind,
                schema,
                name,
            },
        })
    }
}

/// psql's printed table for one relation, and its explorer link.
fn render(rel: &RelInfo, results: &Results, tablespaces: &[(String, String)], verbose: bool, version: i32) -> (PrintTable, Option<LinkAt>) {
    if rel.relkind == 'S' {
        return (render_sequence(rel, results), None);
    }
    let qualified = format!("{}.{}", rel.schema, rel.name);
    let unlogged = rel.relpersistence == 'u';
    let what = match rel.relkind {
        'r' if unlogged => "Unlogged table",
        'r' => "Table",
        'v' => "View",
        'm' => "Materialized view",
        'i' if unlogged => "Unlogged index",
        'i' => "Index",
        'I' if unlogged => "Unlogged partitioned index",
        'I' => "Partitioned index",
        't' => "TOAST table",
        'c' => "Composite type",
        'f' => "Foreign table",
        'p' if unlogged => "Unlogged partitioned table",
        'p' => "Partitioned table",
        _ => "",
    };
    let title = if what.is_empty() {
        format!("?{}? \"{qualified}\"", rel.relkind)
    } else {
        format!("{what} \"{qualified}\"")
    };
    let mut link = match rel.relkind {
        'r' | 'p' | 't' => Some(ObjectKind::Tables),
        'v' => Some(ObjectKind::Views),
        _ => None,
    }.map(|kind| LinkAt::Title { title: title.clone(), kind, schema: rel.schema.clone(), name: rel.name.clone() });

    let mut table = PrintTable { title: Some(title), ..PrintTable::default() };
    let layout = ColumnLayout::new(rel, verbose, version);
    let compression = layout.compression;
    table.headers = ["Column", "Type"].iter().map(|h| h.to_string()).collect();
    let optional = [
        (layout.details, &["Collation", "Nullable", "Default"][..]),
        (layout.is_key, &["Key?"][..]),
        (layout.indexdef, &["Definition"][..]),
        (layout.fdwopts, &["FDW options"][..]),
        (layout.storage, &["Storage"][..]),
        (compression, &["Compression"][..]),
        (layout.stattarget, &["Stats target"][..]),
        (layout.descr, &["Description"][..]),
    ];
    for (on, names) in optional {
        if on { table.headers.extend(names.iter().map(|h| h.to_string())); }
    }
    if let Some(cols) = results.get(Section::Columns) {
        for i in 0..cols.rows.len() {
            let mut col = 0;
            let mut next = || { col += 1; col - 1 };
            let mut row = vec![cell(cols, i, next()).to_string(), cell(cols, i, next()).to_string()];
            if layout.details {
                let (default, notnull, collation, identity, generated) = (next(), next(), next(), next(), next());
                row.push(cell(cols, i, collation).to_string());
                row.push(if is_true(cols, i, notnull) { "not null".into() } else { String::new() });
                let expr = cell(cols, i, default);
                row.push(match (cell(cols, i, identity), cell(cols, i, generated)) {
                    ("a", _) => "generated always as identity".into(),
                    ("d", _) => "generated by default as identity".into(),
                    (_, "s") => format!("generated always as ({expr}) stored"),
                    (_, "v") => format!("generated always as ({expr})"),
                    _ => expr.to_string(),
                });
            }
            if layout.is_key { row.push(cell(cols, i, next()).to_string()); }
            if layout.indexdef { row.push(cell(cols, i, next()).to_string()); }
            if layout.fdwopts { row.push(cell(cols, i, next()).to_string()); }
            if layout.storage {
                row.push(match cell(cols, i, next()) {
                    "p" => "plain", "m" => "main", "x" => "extended", "e" => "external", _ => "???",
                }.into());
            }
            if compression {
                row.push(match cell(cols, i, next()) { "p" => "pglz", "l" => "lz4", "" => "", _ => "???" }.into());
            }
            if layout.stattarget { row.push(cell(cols, i, next()).to_string()); }
            if layout.descr { row.push(cell(cols, i, next()).to_string()); }
            table.rows.push(row);
        }
    }
    table.aligns = vec![Align::Left; table.headers.len()];

    let mut footers: Vec<String> = Vec::new();
    let footer = &mut footers;

    if let Some(set) = results.get(Section::PartitionOf).filter(|s| !s.rows.is_empty()) {
        footer.push(format!(
            "Partition of: {} {}{}",
            cell(set, 0, 0), cell(set, 0, 1),
            if is_true(set, 0, 2) { " DETACH PENDING" } else { "" },
        ));
        if verbose {
            let constraint = cell(set, 0, 3);
            footer.push(if constraint.is_empty() {
                "No partition constraint".into()
            } else {
                format!("Partition constraint: {constraint}")
            });
        }
    }
    if let Some(set) = results.get(Section::PartitionKey).filter(|s| s.rows.len() == 1) {
        footer.push(format!("Partition key: {}", cell(set, 0, 0)));
    }
    if let Some(set) = results.get(Section::ToastOwner).filter(|s| s.rows.len() == 1) {
        footer.push(format!("Owning table: \"{}.{}\"", cell(set, 0, 0), cell(set, 0, 1)));
    }
    if let Some(set) = results.get(Section::IndexInfo).filter(|s| s.rows.len() == 1) {
        let mut line = if is_true(set, 0, 1) {
            "primary key, ".to_string()
        } else if is_true(set, 0, 0) {
            format!("unique{}, ", if is_true(set, 0, 7) { " nulls not distinct" } else { "" })
        } else {
            String::new()
        };
        let table_name = cell(set, 0, 9);
        line.push_str(&format!("{}, for table \"{}.{table_name}\"", cell(set, 0, 8), rel.schema));
        let predicate = cell(set, 0, 10);
        if !predicate.is_empty() { line.push_str(&format!(", predicate ({predicate})")); }
        if is_true(set, 0, 2) { line.push_str(", clustered"); }
        if !is_true(set, 0, 3) { line.push_str(", invalid"); }
        if is_true(set, 0, 4) { line.push_str(", deferrable"); }
        if is_true(set, 0, 5) { line.push_str(", initially deferred"); }
        if is_true(set, 0, 6) { line.push_str(", replica identity"); }
        footer.push(line);
        if results.get(Section::IndexTableKind).is_some_and(|k| matches!(cell(k, 0, 0), "r" | "p")) {
            link = Some(LinkAt::Footer { schema: rel.schema.clone(), name: table_name.to_string() });
        }
        if rel.relkind == 'i' {
            tablespace_footer(footer, results);
        }
    }
    if let Some(set) = results.get(Section::Indexes).filter(|s| !s.rows.is_empty()) {
        footer.push("Indexes:".into());
        for i in 0..set.rows.len() {
            let mut line = format!("    \"{}\"", cell(set, i, 0));
            if cell(set, i, 7) == "x" || is_true(set, i, 12) {
                line.push_str(&format!(" {}", cell(set, i, 6)));
            } else {
                if is_true(set, i, 1) {
                    line.push_str(" PRIMARY KEY,");
                } else if is_true(set, i, 2) {
                    line.push_str(if cell(set, i, 7) == "u" { " UNIQUE CONSTRAINT," } else { " UNIQUE," });
                }
                // Everything after USING, verbatim.
                let def = cell(set, i, 5);
                let def = def.find(" USING ").map_or(def, |at| &def[at + 7..]);
                line.push_str(&format!(" {def}"));
                if is_true(set, i, 8) { line.push_str(" DEFERRABLE"); }
                if is_true(set, i, 9) { line.push_str(" INITIALLY DEFERRED"); }
            }
            if is_true(set, i, 3) { line.push_str(" CLUSTER"); }
            if !is_true(set, i, 4) { line.push_str(" INVALID"); }
            if is_true(set, i, 10) { line.push_str(" REPLICA IDENTITY"); }
            let space = cell(set, i, 11);
            if let Some((_, name)) = tablespaces.iter().find(|(oid, _)| oid == space) {
                line.push_str(&format!(", tablespace \"{name}\""));
            }
            footer.push(line);
        }
    }
    if let Some(set) = results.get(Section::Checks).filter(|s| !s.rows.is_empty()) {
        footer.push("Check constraints:".into());
        for i in 0..set.rows.len() {
            footer.push(format!("    \"{}\" {}", cell(set, i, 0), cell(set, i, 1)));
        }
    }
    if let Some(set) = results.get(Section::ForeignKeys).filter(|s| !s.rows.is_empty()) {
        footer.push("Foreign-key constraints:".into());
        for i in 0..set.rows.len() {
            footer.push(if cell(set, i, 0) == "f" {
                format!("    TABLE \"{}\" CONSTRAINT \"{}\" {}", cell(set, i, 3), cell(set, i, 1), cell(set, i, 2))
            } else {
                format!("    \"{}\" {}", cell(set, i, 1), cell(set, i, 2))
            });
        }
    }
    if let Some(set) = results.get(Section::ReferencedBy).filter(|s| !s.rows.is_empty()) {
        footer.push("Referenced by:".into());
        for i in 0..set.rows.len() {
            footer.push(format!("    TABLE \"{}\" CONSTRAINT \"{}\" {}", cell(set, i, 1), cell(set, i, 0), cell(set, i, 2)));
        }
    }
    if let Some(set) = results.get(Section::Policies) {
        let any = !set.rows.is_empty();
        match (rel.rowsecurity, rel.forcerowsecurity, any) {
            (true, false, true)  => footer.push("Policies:".into()),
            (true, true, true)   => footer.push("Policies (forced row security enabled):".into()),
            (true, false, false) => footer.push("Policies (row security enabled): (none)".into()),
            (true, true, false)  => footer.push("Policies (forced row security enabled): (none)".into()),
            (false, _, true)     => footer.push("Policies (row security disabled):".into()),
            (false, _, false)    => {}
        }
        for i in 0..set.rows.len() {
            let mut line = format!("    POLICY \"{}\"", cell(set, i, 0));
            if cell(set, i, 1).starts_with('f') { line.push_str(" AS RESTRICTIVE"); }
            if !is_null(set, i, 5) { line.push_str(&format!(" FOR {}", cell(set, i, 5))); }
            if !is_null(set, i, 2) { line.push_str(&format!("\n      TO {}", cell(set, i, 2))); }
            if !is_null(set, i, 3) { line.push_str(&format!("\n      USING ({})", cell(set, i, 3))); }
            if !is_null(set, i, 4) { line.push_str(&format!("\n      WITH CHECK ({})", cell(set, i, 4))); }
            footer.push(line);
        }
    }
    if let Some(set) = results.get(Section::StatsObjects).filter(|s| !s.rows.is_empty()) {
        footer.push("Statistics objects:".into());
        for i in 0..set.rows.len() {
            let kinds: Vec<&str> = [(5, "ndistinct"), (6, "dependencies"), (7, "mcv")].iter()
                .filter(|(col, _)| is_true(set, i, *col))
                .map(|(_, name)| *name)
                .collect();
            let mut line = format!("    \"{}.{}\"", cell(set, i, 2), cell(set, i, 3));
            // PG 14+ leaves the kinds out when there are none (a single
            // expression) or all three (CREATE STATISTICS's default);
            // PG 13 always lists them.
            if version < 140_000 || (!kinds.is_empty() && kinds.len() < 3) {
                line.push_str(&format!(" ({})", kinds.join(", ")));
            }
            line.push_str(&format!(" ON {} FROM {}", cell(set, i, 4), cell(set, i, 1)));
            let target = cell(set, i, 8);
            if !is_null(set, i, 8) && target != "-1" { line.push_str(&format!("; STATISTICS {target}")); }
            footer.push(line);
        }
    }
    if let Some(set) = results.get(Section::Rules).filter(|s| !s.rows.is_empty()) {
        for (enabled, heading) in [("O", "Rules:"), ("D", "Disabled rules:"), ("A", "Rules firing always:"),
                                   ("R", "Rules firing on replica only:")] {
            let rules: Vec<usize> = (0..set.rows.len()).filter(|&i| cell(set, i, 2).starts_with(enabled)).collect();
            if rules.is_empty() { continue; }
            footer.push(heading.into());
            for i in rules {
                footer.push(format!("    {}", after_create_rule(cell(set, i, 1))));
            }
        }
    }
    if let Some(set) = results.get(Section::Publications).filter(|s| !s.rows.is_empty()) {
        footer.push("Publications:".into());
        for i in 0..set.rows.len() {
            let mut line = format!("    \"{}\"", cell(set, i, 0));
            if !is_null(set, i, 2) { line.push_str(&format!(" ({})", cell(set, i, 2))); }
            if !is_null(set, i, 1) { line.push_str(&format!(" WHERE {}", cell(set, i, 1))); }
            footer.push(line);
        }
    }
    if let Some(set) = results.get(Section::NotNull).filter(|s| !s.rows.is_empty()) {
        footer.push("Not-null constraints:".into());
        for i in 0..set.rows.len() {
            let (local, inherited) = (is_true(set, i, 3), is_true(set, i, 4));
            footer.push(format!(
                "    \"{}\" NOT NULL \"{}\"{}{}",
                cell(set, i, 0), cell(set, i, 1),
                if is_true(set, i, 2) { " NO INHERIT" }
                else if local && inherited { " (local, inherited)" }
                else if inherited { " (inherited)" }
                else { "" },
                if is_true(set, i, 5) { "" } else { " NOT VALID" },
            ));
        }
    }
    if let Some(set) = results.get(Section::ViewDef).filter(|s| !s.rows.is_empty()) {
        footer.push("View definition:".into());
        footer.push(cell(set, 0, 0).to_string());
        if let Some(rules) = results.get(Section::ViewRules).filter(|s| !s.rows.is_empty()) {
            footer.push("Rules:".into());
            for i in 0..rules.rows.len() {
                footer.push(format!(" {}", after_create_rule(cell(rules, i, 1))));
            }
        }
    }
    if let Some(set) = results.get(Section::Triggers).filter(|s| !s.rows.is_empty()) {
        for (category, heading) in TRIGGER_HEADINGS.iter().enumerate() {
            let triggers: Vec<usize> = (0..set.rows.len())
                .filter(|&i| trigger_category(cell(set, i, 2), cell(set, i, 3)) == Some(category))
                .collect();
            if triggers.is_empty() { continue; }
            footer.push(heading.to_string());
            for i in triggers {
                // Everything after TRIGGER, verbatim.
                let def = cell(set, i, 1);
                let def = def.find(" TRIGGER ").map_or(def, |at| &def[at + 9..]);
                let mut line = format!("    {def}");
                if !is_null(set, i, 4) { line.push_str(&format!(", ON TABLE {}", cell(set, i, 4))); }
                footer.push(line);
            }
        }
    }
    if rel.kind_is("rmfpIt") {
        if let Some(set) = results.get(Section::ForeignServer).filter(|s| s.rows.len() == 1) {
            footer.push(format!("Server: {}", cell(set, 0, 0)));
            let options = cell(set, 0, 1);
            if !options.is_empty() { footer.push(format!("FDW options: ({options})")); }
        }
        if let Some(set) = results.get(Section::Inherits) {
            list_footer(footer, "Inherits", (0..set.rows.len()).map(|i| cell(set, i, 0).to_string()).collect());
        }
        if let Some(set) = results.get(Section::Children) {
            let partitioned = rel.kind_is("pI");
            let n = set.rows.len();
            if partitioned && n == 0 {
                footer.push("Number of partitions: 0".into());
            } else if !verbose {
                if n > 0 {
                    footer.push(if partitioned {
                        format!("Number of partitions: {n} (Use \\d+ to list them.)")
                    } else {
                        format!("Number of child tables: {n} (Use \\d+ to list them.)")
                    });
                }
            } else {
                let children = (0..n).map(|i| {
                    let mut child = cell(set, i, 0).to_string();
                    if !is_null(set, i, 3) { child.push_str(&format!(" {}", cell(set, i, 3))); }
                    match cell(set, i, 1) {
                        "p" | "I" => child.push_str(", PARTITIONED"),
                        "f" => child.push_str(", FOREIGN"),
                        _ => {}
                    }
                    if is_true(set, i, 2) { child.push_str(" (DETACH PENDING)"); }
                    child
                }).collect();
                list_footer(footer, if partitioned { "Partitions" } else { "Child tables" }, children);
            }
        }
        if let Some(of_type) = &rel.reloftype {
            footer.push(format!("Typed table of type: {of_type}"));
        }
        let pg_catalog = rel.schema == "pg_catalog";
        if verbose && rel.kind_is("rm") && rel.relreplident != 'i'
            && ((!pg_catalog && rel.relreplident != 'd') || (pg_catalog && rel.relreplident != 'n'))
        {
            footer.push(format!("Replica Identity: {}", match rel.relreplident { 'f' => "FULL", 'n' => "NOTHING", _ => "???" }));
        }
        tablespace_footer(footer, results);
        if verbose {
            if let Some(am) = &rel.relam { footer.push(format!("Access method: {am}")); }
        }
    }
    if verbose && !rel.reloptions.is_empty() {
        footer.push(format!("Options: {}", rel.reloptions));
    }
    table.footers = footers;
    (table, link)
}

/// psql's trigger footers, in order: enabled, disabled (user, then
/// internal), ALWAYS, REPLICA.
const TRIGGER_HEADINGS: [&str; 5] = [
    "Triggers:",
    "Disabled user triggers:",
    "Disabled internal triggers:",
    "Triggers firing always:",
    "Triggers firing on replica only:",
];

/// Which of [`TRIGGER_HEADINGS`] a trigger goes under, from its
/// `tgenabled` and `tgisinternal`.
fn trigger_category(enabled: &str, internal: &str) -> Option<usize> {
    match enabled {
        "O" | "t" => Some(0),
        "D" | "f" if internal == "f" => Some(1),
        "D" | "f" if internal == "t" => Some(2),
        "A" => Some(3),
        "R" => Some(4),
        _ => None,
    }
}

/// `Tablespace: "x"`, when the relation isn't in the database default.
fn tablespace_footer(footer: &mut Vec<String>, results: &Results) {
    if let Some(set) = results.get(Section::Tablespace).filter(|s| !s.rows.is_empty()) {
        footer.push(format!("Tablespace: \"{}\"", cell(set, 0, 0)));
    }
}

/// psql's `Label: first,` / aligned `second,` … `last` footer list.
fn list_footer(footer: &mut Vec<String>, label: &str, items: Vec<String>) {
    let n = items.len();
    let indent = " ".repeat(label.chars().count());
    for (i, item) in items.into_iter().enumerate() {
        let mut line = if i == 0 { format!("{label}: {item}") } else { format!("{indent}  {item}") };
        if i + 1 < n { line.push(','); }
        footer.push(line);
    }
}

/// A rule definition after its `CREATE RULE ` (psql skips 12 bytes).
fn after_create_rule(def: &str) -> &str {
    def.get(12..).unwrap_or("")
}

/// A sequence: one row of its settings, as psql's `printQuery` prints
/// it (numbers right-aligned), and who owns it.
fn render_sequence(rel: &RelInfo, results: &Results) -> PrintTable {
    let title = format!(
        "{} \"{}.{}\"",
        if rel.relpersistence == 'u' { "Unlogged sequence" } else { "Sequence" },
        rel.schema, rel.name,
    );
    let mut table = PrintTable {
        title:   Some(title),
        headers: ["Type", "Start", "Minimum", "Maximum", "Increment", "Cycles?", "Cache"].iter().map(|h| h.to_string()).collect(),
        aligns:  vec![Align::Left, Align::Right, Align::Right, Align::Right, Align::Right, Align::Left, Align::Right],
        ..PrintTable::default()
    };
    if let Some(set) = results.get(Section::Sequence) {
        table.rows = (0..set.rows.len()).map(|i| (0..7).map(|c| cell(set, i, c).to_string()).collect()).collect();
    }
    if let Some(set) = results.get(Section::SequenceOwner).filter(|s| s.rows.len() == 1) {
        match cell(set, 0, 1) {
            "a" => table.footers.push(format!("Owned by: {}", cell(set, 0, 0))),
            "i" => table.footers.push(format!("Sequence for identity column: {}", cell(set, 0, 0))),
            _ => {}
        }
    }
    table
}

/// A value as psql's `PQgetvalue` reads it: '' for SQL NULL.
fn cell(set: &ResultSet, row: usize, col: usize) -> &str {
    set.rows.get(row).and_then(|r| r.get(col)).and_then(|v| v.as_deref()).unwrap_or("")
}

fn is_null(set: &ResultSet, row: usize, col: usize) -> bool {
    set.rows.get(row).and_then(|r| r.get(col)).is_none_or(|v| v.is_none())
}

fn is_true(set: &ResultSet, row: usize, col: usize) -> bool {
    cell(set, row, col) == "t"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plane::DbmsPlane;
    use crate::plane::postgres::{live, PostgresPlane};
    use crate::MetaAction;

    fn rel(relkind: char) -> RelInfo {
        RelInfo {
            oid: "1".into(), schema: "public".into(), name: "t".into(), checks: 0, relkind,
            hasindex: false, hasrules: false, hastriggers: false, rowsecurity: false, forcerowsecurity: false,
            ispartition: false, reloptions: String::new(), tablespace: "0".into(), reloftype: None,
            relpersistence: 'p', relreplident: 'd', relam: None,
        }
    }

    #[test]
    fn sections_follow_psql_s_choices() {
        use Section::*;
        assert_eq!(plan(&rel('S'), false), [Sequence, SequenceOwner]);
        assert_eq!(plan(&rel('v'), false), [Columns]);
        assert_eq!(plan(&RelInfo { hasrules: true, ..rel('v') }, true), [Columns, ViewDef, ViewRules]);
        assert_eq!(plan(&rel('r'), false), [Columns, ForeignKeys, ReferencedBy, Policies, StatsObjects, Publications, Inherits, Children]);
        let busy = RelInfo { hasindex: true, checks: 2, hasrules: true, hastriggers: true, ..rel('r') };
        assert_eq!(plan(&busy, true), [Columns, Indexes, Checks, ForeignKeys, ReferencedBy, Policies, StatsObjects,
                                       Rules, Publications, NotNull, Triggers, Inherits, Children]);
        assert_eq!(plan(&RelInfo { ispartition: true, ..rel('p') }, false)[..3], [Columns, PartitionOf, PartitionKey]);
        assert_eq!(plan(&rel('i'), false), [Columns, IndexInfo, IndexTableKind]);
        assert_eq!(plan(&RelInfo { tablespace: "1663".into(), ..rel('i') }, false), [Columns, IndexInfo, IndexTableKind, Tablespace]);
        assert_eq!(plan(&rel('I'), false), [Columns, IndexInfo, IndexTableKind, Inherits, Children]);
        // A matview has a _RETURN rule but no table-rules footer.
        assert!(!plan(&RelInfo { hasrules: true, ..rel('m') }, false).contains(&Rules));
    }

    #[test]
    fn list_footers_align_like_psql() {
        let mut footer = Vec::new();
        list_footer(&mut footer, "Partitions", vec!["a FOR VALUES IN (1)".into(), "b DEFAULT".into()]);
        assert_eq!(footer, ["Partitions: a FOR VALUES IN (1),", "            b DEFAULT"]);
    }

    #[test]
    fn the_first_batch_is_the_version_and_the_lookup() {
        let ctx = MetaContext { database: "pagila".into(), ..MetaContext::default() };
        let Some(MetaAction::Staged(queries)) = PostgresPlane.meta_cmd_in("\\d+ film*", &ctx) else { panic!("not staged") };
        assert_eq!(queries.len(), 2);
        assert!(queries[1].contains("c.relname OPERATOR(pg_catalog.~) '^(film.*)$'"), "{}", queries[1]);
        assert!(queries[1].ends_with("ORDER BY 2, 3;"), "{}", queries[1]);
        // psql's refusals fail the command instead.
        assert!(matches!(PostgresPlane.meta_cmd_in("\\d a.b.c.d", &ctx), Some(MetaAction::Fail(_))));
        // No pattern: psql lists relations instead.
        assert!(matches!(PostgresPlane.meta_cmd_in("\\d", &ctx), Some(MetaAction::List { .. })));
    }

    // ── Live parity with psql ───────────────────────────────────────────
    //
    // Opt-in: they need psql on PATH and the local Pagila database
    // (PGPASSWORD from ~/.parliament/secrets.env).
    //   cargo test -p tyto -- --ignored describe_matches_psql

    /// `cmd` run through the plane on database `db`, its queries by `query`.
    fn ours(db: &str, cmd: &str, query: &mut dyn FnMut(&str) -> Result<ResultSet, shared::DbError>) -> String {
        live::staged_output(&MetaContext { database: db.into(), ..MetaContext::default() }, cmd, query)
    }

    /// `\d` and `\d+` of each pattern: ours, then psql's own.
    fn compare(patterns: &[&str], mut ours: impl FnMut(&str) -> String, mut theirs: impl FnMut(&str) -> String) {
        let cases: Vec<String> = patterns.iter().flat_map(|p| [format!("\\d {p}"), format!("\\d+ {p}")]).collect();
        let mut failures = Vec::new();
        for cmd in &cases {
            let (ours, theirs) = (ours(cmd), theirs(cmd));
            if ours != theirs {
                let first = ours.lines().zip(theirs.lines()).position(|(a, b)| a != b)
                    .unwrap_or(ours.lines().count().min(theirs.lines().count()));
                failures.push(format!(
                    "{cmd}: first difference at line {}\n  ours:   {:?}\n  psql:   {:?}",
                    first + 1, ours.lines().nth(first), theirs.lines().nth(first),
                ));
            }
        }
        assert!(failures.is_empty(), "{} of {} differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
    }

    /// Pagila relations of every kind, each query and each `\d` in a
    /// psql of its own (so psql's own messages — "Did not find …" — are
    /// compared too).
    #[test]
    #[ignore]
    fn describe_matches_psql() {
        compare(
            &["film", "payment", "payment_p2022_01", "actor_info", "rental_by_category", "film_pkey",
              "actor_actor_id_seq", "film*", "public.film_*", "nosuch", "a.b.c.d", "pg_catalog.pg_class",
              "pg_toast.pg_toast_2619", "pg_class_oid_index", "(film|actor)", "\"Film\"", "(film"],
            |cmd| ours("pagila", cmd, &mut live::query),
            |cmd| {
                let out = live::psql(&["-P", "pager=off", "-c", cmd]);
                String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
            },
        );
    }

    /// `\d` in each of psql's formats, `\x` on and off — expanded only for
    /// a sequence, as psql prints it.
    #[test]
    #[ignore]
    fn describe_prints_every_format_as_psql_does() {
        use crate::types::{Expanded, OutputFormat, PrintOptions};
        let formats = [OutputFormat::Unaligned, OutputFormat::Csv, OutputFormat::Html, OutputFormat::Asciidoc,
                       OutputFormat::Latex, OutputFormat::LatexLongtable, OutputFormat::TroffMs, OutputFormat::Wrapped];
        let mut failures = Vec::new();
        for format in formats {
            for expanded in [Expanded::Off, Expanded::On] {
                let ctx = MetaContext {
                    database: "pagila".into(),
                    print: PrintOptions { format, expanded, ..PrintOptions::default() },
                    ..MetaContext::default()
                };
                for cmd in ["\\d film", "\\d+ film_pkey", "\\d actor_actor_id_seq", "\\d rental_by_category"] {
                    let ours = live::staged_output(&ctx, cmd, &mut live::query);
                    let x = if expanded == Expanded::On { "expanded=on" } else { "expanded=off" };
                    let out = live::psql(&["-P", "pager=off", "-P", &format!("format={}", format.name()), "-P", x, "-c", cmd]);
                    let theirs = String::from_utf8_lossy(&out.stdout).into_owned();
                    if ours != theirs {
                        failures.push(format!("{} {x} {cmd}:\n  ours: {ours:?}\n  psql: {theirs:?}", format.name()));
                    }
                }
            }
        }
        // Aligned and wrapped under each border and line style.
        use crate::types::{LineStyle, Rules, TextOptions};
        for format in [OutputFormat::Aligned, OutputFormat::Wrapped] {
            for border in [0u16, 2] {
                for style in [LineStyle::Ascii, LineStyle::OldAscii, LineStyle::Unicode] {
                    let rules = Rules { border, style, ..Rules::default() };
                    let ctx = MetaContext {
                        database: "pagila".into(),
                        print: PrintOptions { format, columns: Some(60), text: TextOptions { rules, ..TextOptions::default() }, ..PrintOptions::default() },
                        ..MetaContext::default()
                    };
                    for cmd in ["\\d film", "\\d+ film_pkey", "\\d actor_actor_id_seq"] {
                        let ours = live::staged_output(&ctx, cmd, &mut live::query);
                        let args = [format!("format={}", format.name()), format!("border={border}"), format!("linestyle={}", style.name())];
                        let out = live::psql(&["-P", "pager=off", "-P", "columns=60", "-P", &args[0], "-P", &args[1], "-P", &args[2], "-c", cmd]);
                        let theirs = String::from_utf8_lossy(&out.stdout).into_owned();
                        if ours != theirs {
                            failures.push(format!("{} {cmd}:\n  ours: {ours:?}\n  psql: {theirs:?}", args.join(" ")));
                        }
                    }
                }
            }
        }
        // `\pset numericlocale`: a sequence's numbers in the client's
        // locale, as psql prints them under it; a table's as they are.
        let mut print = PrintOptions {
            locale: shared::NumericLocale { decimal_point: ",".into(), thousands_sep: ".".into(), grouping: 3 },
            ..PrintOptions::default()
        };
        print.text.numeric_locale = true;
        let ctx = MetaContext { database: "pagila".into(), print, ..MetaContext::default() };
        for cmd in ["\\d actor_actor_id_seq", "\\d film"] {
            let ours = live::staged_output(&ctx, cmd, &mut live::query);
            let out = std::process::Command::new("psql").env("LC_ALL", "de_DE.UTF-8")
                .args(["-X", "-d", "pagila", "-U", "postgres", "-P", "pager=off", "-P", "numericlocale=on", "-c", cmd])
                .output().expect("psql on PATH");
            let theirs = String::from_utf8_lossy(&out.stdout).into_owned();
            if ours != theirs {
                failures.push(format!("numericlocale {cmd}:\n  ours: {ours:?}\n  psql: {theirs:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Every footer psql prints, on objects Pagila doesn't have. They're
    /// built inside a transaction that is never committed — psql's `\d`
    /// and the plane's queries run in that same session and see them;
    /// nothing outlives the test.
    #[test]
    #[ignore]
    fn describe_matches_psql_on_every_footer() {
        let mut psql = live::Session::open("pagila");
        psql.step("BEGIN;\nCREATE SCHEMA parliament_fixture;\nSET LOCAL search_path = parliament_fixture;");
        psql.step(FIXTURE);
        let built = psql.step("SELECT count(*) FROM pg_class WHERE relname = 'ft';");
        assert!(built.contains('1'), "fixture didn't build (postgres_fdw missing?): {built}");
        let psql = std::cell::RefCell::new(psql);
        compare(
            &["parent_t", "child_t", "gadgets", "gadgets_pkey", "gadgets_lower_name", "gadgets_code_key",
              "gadgets_id_seq", "pairs", "pair", "scratch", "owned_seq", "loose_seq", "ranges", "ranges_r_excl",
              "events", "events_2024", "events_rest", "events_sub", "events_pkey", "\"Mixed Case\"", "v_rules",
              "mv", "nn", "child_nn", "ft", "gadgets*"],
            |cmd| ours("pagila", cmd, &mut |sql| Ok(psql.borrow_mut().rows(sql))),
            |cmd| psql.borrow_mut().printed(cmd),
        );
        psql.borrow_mut().step("ROLLBACK;");
    }

    const FIXTURE: &str = r#"
        CREATE TABLE parent_t (id int PRIMARY KEY, note text);
        CREATE TABLE child_t (extra int CHECK (extra > 0)) INHERITS (parent_t);
        CREATE TABLE gadgets (
            id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
            serial_no int GENERATED BY DEFAULT AS IDENTITY,
            name text COLLATE "C" NOT NULL,
            price numeric(10,2) CHECK (price >= 0),
            price_with_tax numeric GENERATED ALWAYS AS (price * 1.2) STORED,
            kind text DEFAULT CASE WHEN random() > 0.5 THEN 'a'
                                   ELSE 'b' END,
            code text UNIQUE DEFERRABLE INITIALLY DEFERRED,
            parent int REFERENCES parent_t (id) ON DELETE CASCADE
        ) WITH (fillfactor = 70, toast.autovacuum_enabled = false);
        COMMENT ON COLUMN gadgets.name IS 'line one
line two';
        CREATE INDEX gadgets_lower_name ON gadgets (lower(name)) INCLUDE (price) WHERE price > 10;
        ALTER TABLE gadgets ALTER COLUMN name SET STATISTICS 500;
        ALTER TABLE gadgets ALTER COLUMN kind SET STORAGE EXTERNAL;
        ALTER TABLE gadgets ALTER COLUMN kind SET COMPRESSION pglz;
        ALTER TABLE gadgets REPLICA IDENTITY FULL;
        ALTER TABLE gadgets CLUSTER ON gadgets_pkey;
        ALTER TABLE gadgets ENABLE ROW LEVEL SECURITY;
        ALTER TABLE gadgets FORCE ROW LEVEL SECURITY;
        CREATE POLICY cheap ON gadgets AS RESTRICTIVE FOR SELECT TO PUBLIC USING (price < 100);
        CREATE POLICY own ON gadgets FOR UPDATE TO postgres USING (true) WITH CHECK (price > 0);
        CREATE POLICY anyone ON parent_t USING (true);
        CREATE STATISTICS gadgets_stats (ndistinct, dependencies) ON name, kind FROM gadgets;
        CREATE STATISTICS gadgets_all ON name, kind FROM gadgets;
        ALTER STATISTICS gadgets_all SET STATISTICS 50;
        CREATE STATISTICS gadgets_expr ON (lower(name)) FROM gadgets;
        CREATE PUBLICATION pub_parent FOR TABLE parent_t (id, note) WHERE (id > 0) WITH (publish = 'insert');
        CREATE PUBLICATION pub_schema FOR TABLES IN SCHEMA parliament_fixture;
        CREATE RULE gadgets_log AS ON DELETE TO gadgets DO ALSO NOTHING;
        CREATE RULE parent_off AS ON INSERT TO parent_t DO INSTEAD NOTHING;
        ALTER TABLE parent_t DISABLE RULE parent_off;
        CREATE FUNCTION trg() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;
        CREATE TRIGGER t_on BEFORE INSERT ON gadgets FOR EACH ROW EXECUTE FUNCTION trg();
        CREATE TRIGGER t_off AFTER UPDATE ON gadgets FOR EACH ROW EXECUTE FUNCTION trg();
        ALTER TABLE gadgets DISABLE TRIGGER t_off;
        CREATE TRIGGER t_always BEFORE DELETE ON gadgets FOR EACH ROW EXECUTE FUNCTION trg();
        ALTER TABLE gadgets ENABLE ALWAYS TRIGGER t_always;
        CREATE TRIGGER t_replica AFTER INSERT ON gadgets FOR EACH STATEMENT EXECUTE FUNCTION trg();
        ALTER TABLE gadgets ENABLE REPLICA TRIGGER t_replica;
        CREATE TYPE pair AS (a int, b text);
        CREATE TABLE pairs OF pair;
        CREATE UNLOGGED TABLE scratch (x int);
        CREATE SEQUENCE owned_seq OWNED BY scratch.x;
        CREATE UNLOGGED SEQUENCE loose_seq INCREMENT BY -2 MINVALUE -100 MAXVALUE -1 CYCLE CACHE 5;
        CREATE TABLE ranges (r int4range, EXCLUDE USING gist (r WITH &&));
        CREATE TABLE events (id int, at date, PRIMARY KEY (id, at)) PARTITION BY RANGE (at);
        CREATE TABLE events_2024 PARTITION OF events FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');
        CREATE TABLE events_rest PARTITION OF events DEFAULT;
        CREATE TABLE events_sub PARTITION OF events FOR VALUES FROM ('2025-01-01') TO ('2026-01-01') PARTITION BY LIST (id);
        CREATE TABLE events_sub_1 PARTITION OF events_sub FOR VALUES IN (1);
        CREATE TRIGGER t_events AFTER INSERT ON events FOR EACH ROW EXECUTE FUNCTION trg();
        CREATE TABLE "Mixed Case" ("Col" int);
        CREATE VIEW v_rules AS SELECT * FROM parent_t;
        CREATE RULE v_ins AS ON INSERT TO v_rules DO INSTEAD INSERT INTO parent_t VALUES (NEW.id, NEW.note);
        CREATE MATERIALIZED VIEW mv AS SELECT 1 AS one;
        CREATE TABLE nn (a int NOT NULL, b int NOT NULL NO INHERIT, v int GENERATED ALWAYS AS (a * 2) VIRTUAL);
        CREATE TABLE child_nn (a int NOT NULL) INHERITS (nn);
        CREATE EXTENSION postgres_fdw SCHEMA parliament_fixture;
        CREATE SERVER loopback FOREIGN DATA WRAPPER postgres_fdw OPTIONS (dbname 'nowhere');
        CREATE FOREIGN TABLE ft (a int OPTIONS (column_name 'aa'), b text) SERVER loopback OPTIONS (table_name 't');
    "#;
}
