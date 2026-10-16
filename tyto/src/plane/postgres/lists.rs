//! psql's catalog list commands (describe.c), each building psql's own
//! SQL — the same columns, filters and order, and through `pattern.rs`
//! the same name patterns and refusals — for the shell to run and show
//! as a result table: `\d` `\dt` `\di` `\dv` `\dm` `\ds` `\dE`
//! (listTables), `\dn` (listSchemas), `\df` (describeFunctions), `\du`
//! / `\dg` (describeRoles), `\l` (listAllDbs), `\dx` (listExtensions),
//! `\dT` (describeTypes), `\dp` / `\z` (permissionsList), `\db`
//! (describeTablespaces), `\dD` (listDomains), `\da`
//! (describeAggregates), `\do` (describeOperators), `\dP`
//! (listPartitionedTables), `\dX` (listExtendedStats), `\dconfig`
//! (describeConfigurationParameters), `\ddp` (listDefaultACLs), `\drg`
//! (describeRoleGrants), `\dc` (listConversions), `\dC` (listCasts),
//! `\dd` (objectDescription), `\dL` (listLanguages), `\dO`
//! (listCollations), `\dy` (listEventTriggers), `\drds`
//! (listDbRoleSettings), the text search lists `\dF`, `\dFp`, `\dFd`,
//! `\dFt`, the access method lists `\dA`, `\dAc`, `\dAf`, `\dAo`, `\dAp`,
//! the foreign data lists `\des`, `\deu`, `\dew`, `\det`, the replication
//! lists `\dRp` (listPublications), `\dRs` (describeSubscriptions), and
//! `\dl` / `\lo_list` (listLargeObjects).
//!
//! `\dx+`, `\dF+`, `\dFp+` and `\dRp+` print titled tables per object, so
//! they're staged instead (`Described`): psql's lookup, then each
//! object's queries, printed as psql prints them (`print.rs`).
//!
//! A column psql's SQL leaves NULL stays NULL, so it prints as `\pset
//! null` says — nothing by default — as psql's does.
//! An `x` among the options shows the result expanded, as psql 18 does.

use shared::{BatchResult, ResultSet};

use super::describe::batch_error;
use super::pattern::{self, NameMatch};
use super::print::{print_table, Align, PrintTable};
use crate::plane::{MetaContext, MetaStep};
use crate::types::{FooterQuery, MetaAction, ShellEntry};

/// The newest server psql 18 knows: its branches when the session's
/// version is unknown. An unknown server is likelier new than old, and
/// a wrong guess fails with the server's error instead of showing a
/// wrong value (as the oldest branch's `'libc'` would).
const NEWEST: u32 = 180000;

/// The list command `cmd` (no backslash) with argument text `rest`, or
/// None when `cmd` isn't one of psql's list commands handled here.
pub(super) fn command(cmd: &str, rest: &str, ctx: &MetaContext) -> Option<MetaAction> {
    let list = List::of(cmd)?;
    let (args, unterminated) = pattern::slash_args(rest, &ctx.variables, true);
    if unterminated.is_some() {
        return Some(MetaAction::Output(vec!["unterminated quoted string".into()]));
    }
    let pattern = args.first().map(String::as_str);
    let verbose = cmd.contains('+');
    let system = cmd.contains('S');
    // psql: `x` after a `\d` command's name (`\dx` is extensions), or
    // anywhere in `\l` / `\z`.
    let expanded = match cmd.strip_prefix('d') {
        Some(_) => cmd.get(2..).is_some_and(|options| options.contains('x')),
        None => cmd.contains('x'),
    };
    // psql prints some verbose lists per object: staged.
    if let Some(described) = verbose.then(|| described(list, pattern, ctx)).flatten() {
        return Some(described.first());
    }
    let sql = match list {
        List::Relations(types) => return Some(list_tables(types, pattern, verbose, system, expanded, ctx)),
        List::Schemas => list_schemas(pattern, verbose, system, ctx),
        List::Functions(types) => describe_functions(types, pattern, args.get(1..).unwrap_or_default(), verbose, system, ctx),
        List::Roles => describe_roles(pattern, verbose, system),
        List::Databases => list_all_dbs(pattern, verbose, sversion(ctx)),
        List::Extensions => list_extensions(pattern),
        List::TsConfigs => list_ts_configs(pattern, ctx),
        List::TsParsers => list_ts_parsers(pattern, ctx),
        List::TsDictionaries => list_ts_dictionaries(pattern, verbose, ctx),
        List::TsTemplates => list_ts_templates(pattern, verbose, ctx),
        List::AccessMethods => describe_access_methods(pattern, verbose),
        // A second pattern: the operand type, or the operator family.
        List::OpClasses => list_operator_classes(pattern, args.get(1).map(String::as_str), verbose, ctx),
        List::OpFamilies => list_operator_families(pattern, args.get(1).map(String::as_str), verbose, ctx),
        List::OpFamilyOperators => list_opfamily_operators(pattern, args.get(1).map(String::as_str), verbose, ctx),
        List::OpFamilyFunctions => list_opfamily_functions(pattern, args.get(1).map(String::as_str), verbose, ctx),
        List::ForeignServers => list_foreign_servers(pattern, verbose),
        List::UserMappings => list_user_mappings(pattern, verbose),
        List::ForeignDataWrappers => list_foreign_data_wrappers(pattern, verbose),
        List::ForeignTables => list_foreign_tables(pattern, verbose, ctx),
        List::Publications => list_publications(pattern, sversion(ctx)),
        List::Subscriptions => describe_subscriptions(pattern, verbose, sversion(ctx)),
        List::LargeObjects => Ok(list_large_objects(verbose)),
        List::Types => describe_types(pattern, verbose, system, ctx),
        List::Privileges => permissions_list(pattern, system, ctx),
        List::Tablespaces => describe_tablespaces(pattern, verbose),
        List::Domains => list_domains(pattern, verbose, system, ctx),
        List::Aggregates => describe_aggregates(pattern, system, ctx),
        List::Operators => describe_operators(pattern, args.get(1..).unwrap_or_default(), verbose, system, ctx),
        List::Partitioned(types) => list_partitioned_tables(types, pattern, verbose, ctx),
        List::ExtendedStats => list_extended_stats(pattern, sversion(ctx), ctx),
        List::Config => Ok(describe_configuration_parameters(pattern, verbose, sversion(ctx))),
        List::DefaultAcls => list_default_acls(pattern, ctx),
        List::RoleGrants => describe_role_grants(pattern, system, sversion(ctx)),
        List::Conversions => list_conversions(pattern, verbose, system, ctx),
        List::Casts => list_casts(pattern, verbose, ctx),
        List::ObjectDescriptions => object_descriptions(pattern, system, ctx),
        List::Languages => list_languages(pattern, verbose, system, ctx),
        List::Collations => list_collations(pattern, verbose, system, sversion(ctx), ctx),
        List::EventTriggers => list_event_triggers(pattern, verbose),
        List::DbRoleSettings => {
            return Some(match list_db_role_settings(pattern, args.get(1).map(String::as_str)) {
                Ok((sql, none)) => MetaAction::List {
                    sql, vertical: expanded, title: list.title(pattern).into(), none: unless_quiet(none, ctx), counts_rows: list.counts_rows(), footers: None,
                },
                Err(refusal) => MetaAction::Fail(vec![refusal]),
            });
        }
    };
    // psql's second query, for the lines under the list.
    let footers = match list {
        List::Schemas => schema_publications(pattern, sversion(ctx)),
        _ => None,
    };
    Some(match sql {
        Ok(sql) => MetaAction::List { sql, vertical: expanded, title: list.title(pattern).into(), none: None, counts_rows: list.counts_rows(), footers },
        Err(refusal) => MetaAction::Fail(vec![refusal]),
    })
}

/// Which list a command name asks for, read the way psql's
/// `exec_command_d` / `exec_command_list` read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum List<'a> {
    /// listTables, with the relation-type letters.
    Relations(&'a str),
    Schemas,
    /// describeFunctions, with the options after `df`.
    Functions(&'a str),
    Roles,
    Databases,
    Extensions,
    Types,
    Privileges,
    Tablespaces,
    Domains,
    Aggregates,
    Operators,
    /// listPartitionedTables, with the options after `dP`.
    Partitioned(&'a str),
    ExtendedStats,
    Config,
    DefaultAcls,
    RoleGrants,
    Conversions,
    Casts,
    ObjectDescriptions,
    Languages,
    Collations,
    EventTriggers,
    DbRoleSettings,
    TsConfigs,
    TsParsers,
    TsDictionaries,
    TsTemplates,
    AccessMethods,
    OpClasses,
    OpFamilies,
    OpFamilyOperators,
    OpFamilyFunctions,
    ForeignServers,
    UserMappings,
    ForeignDataWrappers,
    ForeignTables,
    Publications,
    Subscriptions,
    LargeObjects,
}

impl<'a> List<'a> {
    /// Whether psql counts the list's rows under it (`default_footer`):
    /// all but `\du`'s — describeRoles turns it off.
    fn counts_rows(self) -> bool {
        !matches!(self, List::Roles)
    }

    /// psql's title over the list (`myopt.title` in describe.c).
    fn title(self, pattern: Option<&str>) -> &'static str {
        match self {
            List::Relations(types) => relations_title(types),
            List::Schemas => "List of schemas",
            List::Functions(_) => "List of functions",
            List::Roles => "List of roles",
            List::Databases => "List of databases",
            List::Extensions => "List of installed extensions",
            List::Types => "List of data types",
            List::Privileges => "Access privileges",
            List::Tablespaces => "List of tablespaces",
            List::Domains => "List of domains",
            List::Aggregates => "List of aggregate functions",
            List::Operators => "List of operators",
            List::Partitioned(options) => match (options.contains('t'), options.contains('i')) {
                (false, true) => "List of partitioned indexes",
                (true, false) => "List of partitioned tables",
                _ => "List of partitioned relations",
            },
            List::ExtendedStats => "List of extended statistics",
            List::Config if pattern.is_some() => "List of configuration parameters",
            List::Config => "List of non-default configuration parameters",
            List::DefaultAcls => "Default access privileges",
            List::RoleGrants => "List of role grants",
            List::Conversions => "List of conversions",
            List::Casts => "List of casts",
            List::ObjectDescriptions => "Object descriptions",
            List::Languages => "List of languages",
            List::Collations => "List of collations",
            List::EventTriggers => "List of event triggers",
            List::DbRoleSettings => "List of settings",
            List::TsConfigs => "List of text search configurations",
            List::TsParsers => "List of text search parsers",
            List::TsDictionaries => "List of text search dictionaries",
            List::TsTemplates => "List of text search templates",
            List::AccessMethods => "List of access methods",
            List::OpClasses => "List of operator classes",
            List::OpFamilies => "List of operator families",
            List::OpFamilyOperators => "List of operators of operator families",
            List::OpFamilyFunctions => "List of support functions of operator families",
            List::ForeignServers => "List of foreign servers",
            List::UserMappings => "List of user mappings",
            List::ForeignDataWrappers => "List of foreign-data wrappers",
            List::ForeignTables => "List of foreign tables",
            List::Publications => "List of publications",
            List::Subscriptions => "List of subscriptions",
            List::LargeObjects => "Large objects",
        }
    }

    fn of(cmd: &'a str) -> Option<List<'a>> {
        if matches!(cmd, "l" | "list" | "lx" | "listx" | "l+" | "list+" | "lx+" | "listx+" | "l+x" | "list+x") {
            return Some(List::Databases);
        }
        if matches!(cmd, "z" | "zS" | "zx" | "zSx" | "zxS") {
            return Some(List::Privileges);
        }
        // `\lol` is Parliament's short form.
        if cmd.starts_with("lo_list") || cmd == "lol" {
            return Some(List::LargeObjects);
        }
        if let Some(types) = list_tables_types(cmd) {
            return Some(List::Relations(types));
        }
        let rest = cmd.strip_prefix('d')?;
        match rest.chars().next()? {
            'n' => Some(List::Schemas),
            'u' | 'g' => Some(List::Roles),
            'x' => Some(List::Extensions),
            'T' => Some(List::Types),
            'p' => Some(List::Privileges),
            'b' => Some(List::Tablespaces),
            'D' => Some(List::Domains),
            'a' => Some(List::Aggregates),
            'o' => Some(List::Operators),
            'P' => {
                let options = &rest[1..];
                matches!(options.chars().next(), None | Some('+' | 't' | 'i' | 'n' | 'x'))
                    .then_some(List::Partitioned(options))
            }
            'X' => Some(List::ExtendedStats),
            'c' => Some(if cmd.starts_with("dconfig") { List::Config } else { List::Conversions }),
            'C' => Some(List::Casts),
            'd' => Some(if cmd.starts_with("ddp") { List::DefaultAcls } else { List::ObjectDescriptions }),
            'L' => Some(List::Languages),
            'O' => Some(List::Collations),
            'y' => Some(List::EventTriggers),
            'F' => match rest[1..].chars().next() {
                None | Some('+' | 'x') => Some(List::TsConfigs),
                Some('p') => Some(List::TsParsers),
                Some('d') => Some(List::TsDictionaries),
                Some('t') => Some(List::TsTemplates),
                _ => None,
            },
            'A' => match rest[1..].chars().next() {
                None | Some('+' | 'x') => Some(List::AccessMethods),
                Some('c') => Some(List::OpClasses),
                Some('f') => Some(List::OpFamilies),
                Some('o') => Some(List::OpFamilyOperators),
                Some('p') => Some(List::OpFamilyFunctions),
                _ => None,
            },
            'e' => match rest[1..].chars().next() {
                Some('s') => Some(List::ForeignServers),
                Some('u') => Some(List::UserMappings),
                Some('w') => Some(List::ForeignDataWrappers),
                Some('t') => Some(List::ForeignTables),
                _ => None,
            },
            'R' => match rest[1..].chars().next() {
                Some('p') => Some(List::Publications),
                Some('s') => Some(List::Subscriptions),
                _ => None,
            },
            'l' => Some(List::LargeObjects),
            'r' if rest[1..].starts_with("ds") => Some(List::DbRoleSettings),
            'r' => rest[1..].starts_with('g').then_some(List::RoleGrants),
            'f' => {
                let options = &rest[1..];
                // psql reads the letter after `df`; the rest is checked
                // when the list is built.
                matches!(options.chars().next(), None | Some('+' | 'S' | 'a' | 'n' | 'p' | 't' | 'w' | 'x'))
                    .then_some(List::Functions(options))
            }
            _ => None,
        }
    }
}

/// The relation-type letters of a command psql's `listTables` serves —
/// `\dt` `\di` `\dv` `\dm` `\ds` `\dE` and their combinations, the rest
/// of the name read as psql reads it: the letters it looks for (and `S` /
/// `+` / `x`), anything else ignored (`\dt;`, `\dtq`) — or "" for `\d`,
/// `\dS`, `\d+` (which list relations only without a pattern; with one
/// they describe, see `describe.rs`). None for any other command —
/// `\dx` is extensions, `\d;` unknown — as `exec_command_d` switches on
/// the name's second letter.
pub(super) fn list_tables_types(cmd: &str) -> Option<&str> {
    let rest = cmd.strip_prefix('d')?;
    match rest.chars().next() {
        None | Some('S' | '+') => Some(""),
        Some('t' | 'i' | 'v' | 'm' | 's' | 'E') => Some(rest),
        _ => None,
    }
}

/// psql's `pset.sversion` for the session's server.
fn sversion(ctx: &MetaContext) -> u32 {
    server_version_num(&ctx.server_version).unwrap_or(NEWEST)
}

/// psql's numeric server version (`PQserverVersion`) from the server's
/// `version()` text: "PostgreSQL 18.4 (Postgres.app) on …" → 180004,
/// "PostgreSQL 9.6.24 on …" → 90624. None when it isn't one.
pub(super) fn server_version_num(version: &str) -> Option<u32> {
    let number: String = version.trim().strip_prefix("PostgreSQL ")?
        .chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut parts = number.split('.').map(|p| p.parse::<u32>().ok());
    let major = parts.next().flatten()?;
    let minor = parts.next().flatten().unwrap_or(0);
    Some(if major >= 10 {
        major * 10_000 + minor
    } else {
        major * 10_000 + minor * 100 + parts.next().flatten().unwrap_or(0)
    })
}

/// psql's `printACLColumn`, NULL (default privileges) read as blank.
fn acl_column(column: &str) -> String {
    format!(
        "CASE WHEN pg_catalog.array_length({column}, 1) = 0 THEN '(none)' \
         ELSE pg_catalog.array_to_string({column}, E'\\n') END AS \"Access privileges\""
    )
}

/// psql's `listTables` (describe.c): list relations of the kinds in
/// `tabtypes` (`t` tables, `i` indexes, `v` views, `m` materialized
/// views, `s` sequences, `E` foreign tables), matching `pattern`.
/// Without `S` or a pattern, system schemas are left out. psql says
/// "Did not find …" when nothing matches.
fn list_tables(tabtypes: &str, pattern: Option<&str>, verbose: bool, show_system: bool, expanded: bool, ctx: &MetaContext) -> MetaAction {
    let mut show_tables  = tabtypes.contains('t');
    let show_indexes     = tabtypes.contains('i');
    let mut show_views   = tabtypes.contains('v');
    let mut show_matviews = tabtypes.contains('m');
    let mut show_seq     = tabtypes.contains('s');
    let mut show_foreign = tabtypes.contains('E');
    let ntypes = [show_tables, show_indexes, show_views, show_matviews, show_seq, show_foreign]
        .iter().filter(|&&b| b).count();
    if ntypes == 0 {
        (show_tables, show_views, show_matviews, show_seq, show_foreign) = (true, true, true, true, true);
    }
    let with_am = show_tables || show_matviews || show_indexes;

    let mut buf = String::from(
        "SELECT n.nspname as \"Schema\",\n  \
           c.relname as \"Name\",\n  \
           CASE c.relkind WHEN 'r' THEN 'table' WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized view' \
           WHEN 'i' THEN 'index' WHEN 'S' THEN 'sequence' WHEN 't' THEN 'TOAST table' \
           WHEN 'f' THEN 'foreign table' WHEN 'p' THEN 'partitioned table' WHEN 'I' THEN 'partitioned index' \
           END as \"Type\",\n  \
           pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\"",
    );
    if show_indexes {
        buf.push_str(",\n  c2.relname as \"Table\"");
    }
    if verbose {
        buf.push_str(",\n  CASE c.relpersistence WHEN 'p' THEN 'permanent' WHEN 't' THEN 'temporary' \
                      WHEN 'u' THEN 'unlogged' END as \"Persistence\"");
        if with_am {
            buf.push_str(",\n  am.amname as \"Access method\"");
        }
        buf.push_str(",\n  pg_catalog.pg_size_pretty(pg_catalog.pg_table_size(c.oid)) as \"Size\"\
                      ,\n  pg_catalog.obj_description(c.oid, 'pg_class') as \"Description\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_class c\
                  \n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace");
    if with_am {
        buf.push_str("\n     LEFT JOIN pg_catalog.pg_am am ON am.oid = c.relam");
    }
    if show_indexes {
        buf.push_str("\n     LEFT JOIN pg_catalog.pg_index i ON i.indexrelid = c.oid\
                      \n     LEFT JOIN pg_catalog.pg_class c2 ON i.indrelid = c2.oid");
    }
    buf.push_str("\nWHERE c.relkind IN (");
    if show_tables {
        buf.push_str("'r','p',");
        // With `S` or a pattern, `t` matches TOAST tables too.
        if show_system || pattern.is_some() { buf.push_str("'t',"); }
    }
    if show_views    { buf.push_str("'v',"); }
    if show_matviews { buf.push_str("'m',"); }
    if show_indexes  { buf.push_str("'i','I',"); }
    if show_seq      { buf.push_str("'S',"); }
    if show_system || pattern.is_some() { buf.push_str("'s',"); }
    if show_foreign  { buf.push_str("'f',"); }
    buf.push_str("'')\n");
    if !show_system && pattern.is_none() {
        buf.push_str("      AND n.nspname <> 'pg_catalog'\n      \
                      AND n.nspname !~ '^pg_toast'\n      \
                      AND n.nspname <> 'information_schema'\n");
    }
    let relations = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("c.relname"),
        visibility_rule: Some("pg_catalog.pg_table_is_visible(c.oid)"),
        ..NameMatch::default()
    };
    if let Err(e) = relations.validate(&mut buf, pattern, true, 3, &ctx.database) {
        return MetaAction::Output(vec![e.to_string()]);
    }
    buf.push_str("ORDER BY 1,2;");

    let what = if ntypes != 1 { "relations" }
        else if show_tables   { "tables" }
        else if show_indexes  { "indexes" }
        else if show_views    { "views" }
        else if show_matviews { "materialized views" }
        else if show_seq      { "sequences" }
        else                  { "foreign tables" };
    let none = match pattern {
        Some(p) => format!("Did not find any {what} named \"{p}\"."),
        None    => format!("Did not find any {what}."),
    };
    MetaAction::List { sql: buf, vertical: expanded, title: relations_title(tabtypes).into(), none: unless_quiet(none, ctx), counts_rows: true, footers: None }
}

/// What psql says instead of an empty list — but under `QUIET`, where it
/// prints the empty table, as its other lists do (`listTables`,
/// `listDbRoleSettings`).
fn unless_quiet(none: String, ctx: &MetaContext) -> Option<String> {
    (!super::quiet(ctx)).then_some(none)
}

/// listTables' title: the one kind of relation asked for, else all of
/// them (`\d`, `\dtv`).
fn relations_title(tabtypes: &str) -> &'static str {
    let kinds: Vec<char> = "tivmsE".chars().filter(|&kind| tabtypes.contains(kind)).collect();
    match kinds[..] {
        ['t'] => "List of tables",
        ['i'] => "List of indexes",
        ['v'] => "List of views",
        ['m'] => "List of materialized views",
        ['s'] => "List of sequences",
        ['E'] => "List of foreign tables",
        _ => "List of relations",
    }
}

/// psql's `listSchemas` (`\dn`). Without `S` or a pattern, `pg_*` and
/// `information_schema` are left out. With a pattern, psql 15+ asks
/// after it for the schema's publications (`schema_publications`).
fn list_schemas(pattern: Option<&str>, verbose: bool, show_system: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT n.nspname AS \"Name\",\n  pg_catalog.pg_get_userbyid(n.nspowner) AS \"Owner\"",
    );
    if verbose {
        buf.push_str(",\n  ");
        buf.push_str(&acl_column("n.nspacl"));
        buf.push_str(",\n  pg_catalog.obj_description(n.oid, 'pg_namespace') AS \"Description\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_namespace n\n");
    let hide_system = !show_system && pattern.is_none();
    if hide_system {
        buf.push_str("WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'\n");
    }
    let schemas = NameMatch { name_var: Some("n.nspname"), ..NameMatch::default() };
    schemas.validate(&mut buf, pattern, hide_system, 2, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// psql's second query for `\dn <pattern>` on a PG 15+ server: the
/// publications of the schema the pattern names — put in as typed, as
/// psql puts it (`'%s'`), so `\dn pub*` asks for a schema named `pub*`
/// and a `'` in it breaks the query, psql's and this one alike — which
/// print under the list as its `Publications:` footer.
fn schema_publications(pattern: Option<&str>, sversion: u32) -> Option<FooterQuery> {
    let pattern = pattern.filter(|_| sversion >= 150000)?;
    Some(FooterQuery {
        sql: format!(
            "SELECT pubname \n\
             FROM pg_catalog.pg_publication p\n     \
             JOIN pg_catalog.pg_publication_namespace pn ON p.oid = pn.pnpubid\n     \
             JOIN pg_catalog.pg_namespace n ON n.oid = pn.pnnspid \n\
             WHERE n.nspname = '{pattern}'\n\
             ORDER BY 1",
        ),
        heading: "Publications:".into(),
    })
}

/// psql's `describeFunctions` (`\df[anptw][Sx+] [FUNCPTRN [TYPEPTRN …]]`):
/// `options` are the letters after `df` — `a` aggregates, `n` normal,
/// `p` procedures, `t` trigger and `w` window functions, any mix, none
/// meaning all. Each argument-type pattern matches that argument's
/// type (`-` for no such argument).
fn describe_functions(
    options: &str, pattern: Option<&str>, arg_patterns: &[String], verbose: bool, show_system: bool, ctx: &MetaContext,
) -> Result<String, String> {
    if !options.chars().all(|c| "anptwSx+".contains(c)) {
        return Err("\\df only takes [anptwSx+] as options".into());
    }
    let mut show_aggregate = options.contains('a');
    let mut show_normal    = options.contains('n');
    let mut show_procedure = options.contains('p');
    let mut show_trigger   = options.contains('t');
    let mut show_window    = options.contains('w');
    if !(show_aggregate || show_normal || show_procedure || show_trigger || show_window) {
        (show_aggregate, show_normal, show_procedure, show_trigger, show_window) = (true, true, true, true, true);
    }
    // psql reads at most FUNC_MAX_ARGS argument patterns.
    let arg_patterns = &arg_patterns[..arg_patterns.len().min(100)];

    let mut buf = String::from(
        "SELECT n.nspname as \"Schema\",\n  \
           p.proname as \"Name\",\n  \
           pg_catalog.pg_get_function_result(p.oid) as \"Result data type\",\n  \
           pg_catalog.pg_get_function_arguments(p.oid) as \"Argument data types\",\n \
         CASE p.prokind\n  WHEN 'a' THEN 'agg'\n  WHEN 'w' THEN 'window'\n  WHEN 'p' THEN 'proc'\n  ELSE 'func'\n \
         END as \"Type\"",
    );
    if verbose {
        buf.push_str(
            ",\n CASE\n  WHEN p.provolatile = 'i' THEN 'immutable'\n  WHEN p.provolatile = 's' THEN 'stable'\n  \
             WHEN p.provolatile = 'v' THEN 'volatile'\n END as \"Volatility\"\
             ,\n CASE\n  WHEN p.proparallel = 'r' THEN 'restricted'\n  WHEN p.proparallel = 's' THEN 'safe'\n  \
             WHEN p.proparallel = 'u' THEN 'unsafe'\n END as \"Parallel\"\
             ,\n pg_catalog.pg_get_userbyid(p.proowner) as \"Owner\"\
             ,\n CASE WHEN prosecdef THEN 'definer' ELSE 'invoker' END AS \"Security\"\
             ,\n CASE WHEN p.proleakproof THEN 'yes' ELSE 'no' END as \"Leakproof?\",\n ",
        );
        buf.push_str(&acl_column("p.proacl"));
        buf.push_str(
            ",\n l.lanname as \"Language\"\
             ,\n CASE WHEN l.lanname IN ('internal', 'c') THEN p.prosrc END as \"Internal name\"\
             ,\n pg_catalog.obj_description(p.oid, 'pg_proc') as \"Description\"",
        );
    }
    buf.push_str("\nFROM pg_catalog.pg_proc p\n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace\n");
    for i in 0..arg_patterns.len() {
        buf.push_str(&format!(
            "     LEFT JOIN pg_catalog.pg_type t{i} ON t{i}.oid = p.proargtypes[{i}]\n     \
             LEFT JOIN pg_catalog.pg_namespace nt{i} ON nt{i}.oid = t{i}.typnamespace\n"
        ));
    }
    if verbose {
        buf.push_str("     LEFT JOIN pg_catalog.pg_language l ON l.oid = p.prolang\n");
    }

    // Filter by kind, psql's way.
    let mut have_where = false;
    let mut condition = |buf: &mut String, text: &str| {
        buf.push_str(if have_where { "      AND " } else { "WHERE " });
        have_where = true;
        buf.push_str(text);
        buf.push('\n');
    };
    if show_normal && show_aggregate && show_procedure && show_trigger && show_window {
        // All of them: no filter.
    } else if show_normal {
        if !show_aggregate { condition(&mut buf, "p.prokind <> 'a'"); }
        if !show_procedure { condition(&mut buf, "p.prokind <> 'p'"); }
        if !show_trigger   { condition(&mut buf, "p.prorettype <> 'pg_catalog.trigger'::pg_catalog.regtype"); }
        if !show_window    { condition(&mut buf, "p.prokind <> 'w'"); }
    } else {
        let kinds: Vec<&str> = [
            (show_aggregate, "p.prokind = 'a'"),
            (show_trigger,   "p.prorettype = 'pg_catalog.trigger'::pg_catalog.regtype"),
            (show_procedure, "p.prokind = 'p'"),
            (show_window,    "p.prokind = 'w'"),
        ].into_iter().filter_map(|(shown, test)| shown.then_some(test)).collect();
        buf.push_str("WHERE (\n       ");
        buf.push_str(&kinds.join("\n       OR "));
        buf.push_str("\n      )\n");
        have_where = true;
    }

    let functions = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("p.proname"),
        visibility_rule: Some("pg_catalog.pg_function_is_visible(p.oid)"),
        ..NameMatch::default()
    };
    functions.validate(&mut buf, pattern, have_where, 3, &ctx.database).map_err(|e| e.to_string())?;

    for (i, arg) in arg_patterns.iter().enumerate() {
        if arg == "-" {
            // No such argument.
            buf.push_str(&format!("  AND t{i}.typname IS NULL\n"));
            continue;
        }
        // Like \dT, the type's internal or external name.
        let (nspname, typname) = (format!("nt{i}.nspname"), format!("t{i}.typname"));
        let (format_type, visible) = (
            format!("pg_catalog.format_type(t{i}.oid, NULL)"),
            format!("pg_catalog.pg_type_is_visible(t{i}.oid)"),
        );
        let types = NameMatch {
            schema_var:      Some(nspname.as_str()),
            name_var:        Some(typname.as_str()),
            alt_name_var:    Some(format_type.as_str()),
            visibility_rule: Some(visible.as_str()),
            ..NameMatch::default()
        };
        types.validate(&mut buf, Some(map_typename_pattern(arg)), true, 3, &ctx.database).map_err(|e| e.to_string())?;
    }

    if !show_system && pattern.is_none() {
        buf.push_str("      AND n.nspname <> 'pg_catalog'\n      AND n.nspname <> 'information_schema'\n");
    }
    buf.push_str("ORDER BY 1, 2, 4;");
    Ok(buf)
}

/// psql's `map_typename_pattern`: type names the grammar accepts that
/// are neither pg_type's name nor format_type's.
fn map_typename_pattern(pattern: &str) -> &str {
    const MAP: &[(&str, &str)] = &[
        ("decimal", "numeric"), ("float", "double precision"), ("int", "integer"),
        ("bool[]", "boolean[]"), ("decimal[]", "numeric[]"), ("float[]", "double precision[]"),
        ("float4[]", "real[]"), ("float8[]", "double precision[]"), ("int[]", "integer[]"),
        ("int2[]", "smallint[]"), ("int4[]", "integer[]"), ("int8[]", "bigint[]"),
        ("time[]", "time without time zone[]"), ("timetz[]", "time with time zone[]"),
        ("timestamp[]", "timestamp without time zone[]"), ("timestamptz[]", "timestamp with time zone[]"),
        ("varbit[]", "bit varying[]"), ("varchar[]", "character varying[]"),
    ];
    MAP.iter().find(|(from, _)| from.eq_ignore_ascii_case(pattern)).map_or(pattern, |(_, to)| to)
}

/// psql's `describeRoles` (`\du` / `\dg`). psql builds the Attributes
/// column itself from the role's flags; this builds the same text in
/// the query, from the same flags in the same order.
fn describe_roles(pattern: Option<&str>, verbose: bool, show_system: bool) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT r.rolname AS \"Role name\",\n  \
           concat_ws(E'\\n',\n    \
             NULLIF(concat_ws(', ',\n      \
               CASE WHEN r.rolsuper THEN 'Superuser' END,\n      \
               CASE WHEN NOT r.rolinherit THEN 'No inheritance' END,\n      \
               CASE WHEN r.rolcreaterole THEN 'Create role' END,\n      \
               CASE WHEN r.rolcreatedb THEN 'Create DB' END,\n      \
               CASE WHEN NOT r.rolcanlogin THEN 'Cannot login' END,\n      \
               CASE WHEN r.rolreplication THEN 'Replication' END,\n      \
               CASE WHEN r.rolbypassrls THEN 'Bypass RLS' END), ''),\n    \
             CASE WHEN r.rolconnlimit = 0 THEN 'No connections'\n         \
                  WHEN r.rolconnlimit = 1 THEN '1 connection'\n         \
                  WHEN r.rolconnlimit > 1 THEN r.rolconnlimit || ' connections' END,\n    \
             'Password valid until ' || r.rolvaliduntil) AS \"Attributes\"",
    );
    if verbose {
        // psql builds this table cell by cell from PQgetvalue, which reads
        // NULL as '' — `\pset null` never shows here.
        buf.push_str(",\n  COALESCE(pg_catalog.shobj_description(r.oid, 'pg_authid'), '') AS \"Description\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_roles r\n");
    if !show_system && pattern.is_none() {
        buf.push_str("WHERE r.rolname !~ '^pg_'\n");
    }
    let roles = NameMatch { name_var: Some("r.rolname"), ..NameMatch::default() };
    roles.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// psql's `listAllDbs` (`\l`, `\list`), by server version as psql
/// branches: locale provider from 15, ICU rules from 16, `datlocale`
/// from 17.
fn list_all_dbs(pattern: Option<&str>, verbose: bool, sversion: u32) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT\n  d.datname as \"Name\",\n  \
         pg_catalog.pg_get_userbyid(d.datdba) as \"Owner\",\n  \
         pg_catalog.pg_encoding_to_char(d.encoding) as \"Encoding\",\n",
    );
    buf.push_str(if sversion >= 150000 {
        "  CASE d.datlocprovider WHEN 'b' THEN 'builtin' WHEN 'c' THEN 'libc' WHEN 'i' THEN 'icu' END AS \"Locale Provider\",\n"
    } else {
        "  'libc' AS \"Locale Provider\",\n"
    });
    buf.push_str("  d.datcollate as \"Collate\",\n  d.datctype as \"Ctype\",\n");
    buf.push_str(if sversion >= 170000 {
        "  d.datlocale as \"Locale\",\n"
    } else if sversion >= 150000 {
        "  d.daticulocale as \"Locale\",\n"
    } else {
        "  NULL as \"Locale\",\n"
    });
    buf.push_str(if sversion >= 160000 {
        "  d.daticurules as \"ICU Rules\",\n"
    } else {
        "  NULL as \"ICU Rules\",\n"
    });
    buf.push_str("  ");
    buf.push_str(&acl_column("d.datacl"));
    if verbose {
        buf.push_str(
            ",\n  CASE WHEN pg_catalog.has_database_privilege(d.datname, 'CONNECT')\n       \
             THEN pg_catalog.pg_size_pretty(pg_catalog.pg_database_size(d.datname))\n       \
             ELSE 'No Access'\n  END as \"Size\"\
             ,\n  t.spcname as \"Tablespace\"\
             ,\n  pg_catalog.shobj_description(d.oid, 'pg_database') as \"Description\"",
        );
    }
    buf.push_str("\nFROM pg_catalog.pg_database d\n");
    if verbose {
        buf.push_str("  JOIN pg_catalog.pg_tablespace t on d.dattablespace = t.oid\n");
    }
    let databases = NameMatch { name_var: Some("d.datname"), ..NameMatch::default() };
    databases.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// psql's `listExtensions` (`\dx`).
fn list_extensions(pattern: Option<&str>) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT e.extname AS \"Name\", e.extversion AS \"Version\", \
         ae.default_version AS \"Default version\",\
         n.nspname AS \"Schema\", d.description AS \"Description\"\n\
         FROM pg_catalog.pg_extension e \
         LEFT JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace \
         LEFT JOIN pg_catalog.pg_description d ON d.objoid = e.oid \
         AND d.classoid = 'pg_catalog.pg_extension'::pg_catalog.regclass \
         LEFT JOIN pg_catalog.pg_available_extensions() ae(name, default_version, comment) ON ae.name = e.extname\n",
    );
    let extensions = NameMatch { name_var: Some("e.extname"), ..NameMatch::default() };
    extensions.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// psql's `listExtensionContents` lookup (`\dx+`): the extensions to list.
fn extension_lookup(pattern: Option<&str>) -> Result<String, String> {
    let mut buf = String::from("SELECT e.extname, e.oid\nFROM pg_catalog.pg_extension e\n");
    let extensions = NameMatch { name_var: Some("e.extname"), ..NameMatch::default() };
    extensions.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// A staged list that psql prints as titled tables per object (`\dx+`,
/// `\dF+`, `\dFp+`): its lookup, then every object's queries in one
/// batch, printed in order — or psql's message when nothing matched. A
/// failed query prints after the tables that completed.
struct Described {
    lookup: Result<String, String>,
    none:   String,
    queries: ObjectQueries,
    tables:  ObjectTables,
}

/// One object's queries, from its lookup row and psql's server version
/// (None skips a row that isn't what the lookup returns).
type ObjectQueries = fn(&[Option<String>], u32) -> Option<Vec<String>>;

/// One object's tables, from its lookup row, the results so far (fewer
/// than its queries when one failed) and the server version.
type ObjectTables = fn(&[Option<String>], &[ResultSet], u32) -> Vec<PrintTable>;

impl Described {
    fn first(&self) -> MetaAction {
        match &self.lookup {
            Ok(lookup) => MetaAction::Staged(vec![lookup.clone()]),
            Err(refusal) => MetaAction::Fail(vec![refusal.clone()]),
        }
    }

    fn next(&self, ctx: &MetaContext, replies: &[BatchResult]) -> MetaStep {
        let (Ok(lookup), Some(found)) = (&self.lookup, replies.first()) else {
            return MetaStep::Done(Vec::new());
        };
        if let Some(error) = batch_error(found, std::slice::from_ref(lookup), ctx) {
            return MetaStep::Failed(vec![error]);
        }
        let version = sversion(ctx);
        let objects: Vec<(&[Option<String>], Vec<String>)> = found.sets.first()
            .map(|set| set.rows.iter().filter_map(|row| Some((row.as_slice(), (self.queries)(row, version)?))).collect())
            .unwrap_or_default();
        // Nothing found: psql's word for it, unless `QUIET`.
        if objects.is_empty() {
            return MetaStep::Failed(if super::quiet(ctx) { Vec::new() } else { vec![ShellEntry::MetaOutput(vec![self.none.clone()])] });
        }
        let queries: Vec<String> = objects.iter().flat_map(|(_, q)| q.iter().cloned()).collect();
        // Objects with nothing to fetch (a publication of all tables)
        // print straight away.
        let nothing = BatchResult::default();
        let results = match replies.get(1) {
            Some(results) => results,
            None if queries.is_empty() => &nothing,
            None => return MetaStep::Run(queries),
        };

        let mut lines = Vec::new();
        let mut sets = results.sets.as_slice();
        for (row, mine) in &objects {
            let (done, rest) = sets.split_at(mine.len().min(sets.len()));
            sets = rest;
            for table in (self.tables)(row, done, version) {
                // psql's `\x` applies here (only `\d` turns it off).
                lines.extend(print_table(&table, &ctx.print));
            }
            if done.len() < mine.len() { break; }
        }
        let mut out = Vec::new();
        if !lines.is_empty() {
            out.push(ShellEntry::Printed { lines, links: Vec::new() });
        }
        out.extend(batch_error(results, &queries, ctx));
        MetaStep::Done(out)
    }
}

/// The staged form of a list, when psql prints it per object.
fn described(list: List, pattern: Option<&str>, ctx: &MetaContext) -> Option<Described> {
    let none = |one: &str, many: &str| match pattern {
        Some(p) => format!("Did not find any {one} named \"{p}\"."),
        None => format!("Did not find any {many}."),
    };
    Some(match list {
        List::Extensions => Described {
            lookup:  extension_lookup(pattern),
            none:    none("extension", "extensions"),
            queries: |row, _| Some(vec![extension_contents_sql(oid(row, 1)?)]),
            tables:  |row, results, _| results.first().map(|set| PrintTable {
                title: Some(format!("Objects in extension \"{}\"", text(row, 0))),
                ..table(&["Object description"], set, true)
            }).into_iter().collect(),
        },
        List::TsConfigs => Described {
            lookup:  ts_config_lookup(pattern, ctx),
            none:    none("text search configuration", "text search configurations"),
            queries: |row, _| Some(vec![ts_config_map_sql(oid(row, 0)?)]),
            tables:  |row, results, _| results.first().map(|set| PrintTable {
                title: Some(format!(
                    "Text search configuration \"{}\"\nParser: \"{}\"",
                    qualified(row, 2, 1), qualified(row, 4, 3),
                )),
                ..table(&["Token", "Dictionaries"], set, false)
            }).into_iter().collect(),
        },
        List::TsParsers => Described {
            lookup:  ts_parser_lookup(pattern, ctx),
            none:    none("text search parser", "text search parsers"),
            queries: |row, _| {
                let oid = oid(row, 0)?;
                Some(vec![ts_parser_methods_sql(oid), ts_parser_tokens_sql(oid)])
            },
            tables:  |row, results, _| {
                let parser = qualified(row, 1, 2);
                let mut tables = Vec::new();
                if let Some(set) = results.first() {
                    tables.push(PrintTable {
                        title: Some(format!("Text search parser \"{parser}\"")),
                        ..table(&["Method", "Function", "Description"], set, false)
                    });
                }
                if let Some(set) = results.get(1) {
                    tables.push(PrintTable {
                        title: Some(format!("Token types for parser \"{parser}\"")),
                        ..table(&["Token name", "Description"], set, true)
                    });
                }
                tables
            },
        },
        List::Publications => Described {
            lookup:  publication_lookup(pattern, sversion(ctx)),
            none:    none("publication", "publications"),
            queries: |row, version| {
                let oid = oid(row, 0)?;
                // A publication of all tables lists none.
                if text(row, 3) == "t" { return Some(Vec::new()); }
                let mut queries = vec![publication_tables_sql(oid, version)];
                if version >= 150000 { queries.push(publication_schemas_sql(oid)); }
                Some(queries)
            },
            tables:  |row, results, version| vec![publication_table(row, results, version)],
        },
        _ => return None,
    })
}

/// Column `i` of a lookup row as text (SQL NULL as '').
fn text(row: &[Option<String>], i: usize) -> &str {
    row.get(i).and_then(Option::as_deref).unwrap_or_default()
}

/// Column `i` of a lookup row as an OID, when it is one.
fn oid(row: &[Option<String>], i: usize) -> Option<&str> {
    Some(text(row, i)).filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
}

/// `schema.name` from a lookup row, or `name` when the schema is NULL —
/// psql's titles.
fn qualified(row: &[Option<String>], schema: usize, name: usize) -> String {
    match row.get(schema).and_then(Option::as_deref) {
        Some(schema) => format!("{schema}.{}", text(row, name)),
        None => text(row, name).to_string(),
    }
}

/// A result as psql's `printQuery` lays it out: left-aligned text, SQL
/// NULL blank, and with `footer` its "(N rows)".
fn table(headers: &[&str], set: &ResultSet, footer: bool) -> PrintTable {
    let rows: Vec<Vec<String>> = set.rows.iter()
        .map(|row| row.iter().map(|v| v.clone().unwrap_or_default()).collect())
        .collect();
    PrintTable {
        title:   None,
        headers: headers.iter().map(|h| h.to_string()).collect(),
        aligns:  vec![Align::Left; headers.len()],
        rows,
        footers: Vec::new(),
        count_rows: footer,
    }
}

/// psql's `listOneExtensionContents`: the objects extension `oid` owns.
fn extension_contents_sql(oid: &str) -> String {
    format!(
        "SELECT pg_catalog.pg_describe_object(classid, objid, 0) AS \"Object description\"\n\
         FROM pg_catalog.pg_depend\n\
         WHERE refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass AND refobjid = '{oid}' AND deptype = 'e'\n\
         ORDER BY 1;"
    )
}

/// The next step of a staged list (`\dx+`, `\dF+`, `\dFp+`), or None when
/// `cmd` isn't one. Re-reads the command; nothing is kept between steps.
pub(super) fn next(cmd: &str, rest: &str, ctx: &MetaContext, replies: &[BatchResult]) -> Option<MetaStep> {
    if !cmd.contains('+') { return None; }
    let (args, _) = pattern::slash_args(rest, &ctx.variables, true);
    Some(described(List::of(cmd)?, args.first().map(String::as_str), ctx)?.next(ctx, replies))
}

/// psql's `listTSConfigs` (`\dF`).
fn list_ts_configs(pattern: Option<&str>, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT\n   n.nspname as \"Schema\",\n   c.cfgname as \"Name\",\n   \
         pg_catalog.obj_description(c.oid, 'pg_ts_config') as \"Description\"\n\
         FROM pg_catalog.pg_ts_config c\nLEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.cfgnamespace\n",
    );
    ts_configs().validate(&mut buf, pattern, false, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

fn ts_configs() -> NameMatch<'static> {
    NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("c.cfgname"),
        visibility_rule: Some("pg_catalog.pg_ts_config_is_visible(c.oid)"),
        ..NameMatch::default()
    }
}

/// psql's `listTSConfigsVerbose` lookup (`\dF+`): each configuration with
/// its parser.
fn ts_config_lookup(pattern: Option<&str>, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT c.oid, c.cfgname,\n   n.nspname,\n   p.prsname,\n   np.nspname as pnspname\n\
         FROM pg_catalog.pg_ts_config c\n   LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.cfgnamespace,\n \
         pg_catalog.pg_ts_parser p\n   LEFT JOIN pg_catalog.pg_namespace np ON np.oid = p.prsnamespace\n\
         WHERE  p.oid = c.cfgparser\n",
    );
    ts_configs().validate(&mut buf, pattern, true, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 3, 2;");
    Ok(buf)
}

/// psql's `describeOneTSConfig`: configuration `oid`'s token → dictionaries map.
fn ts_config_map_sql(oid: &str) -> String {
    format!(
        "SELECT\n  ( SELECT t.alias FROM\n    pg_catalog.ts_token_type(c.cfgparser) AS t\n    \
         WHERE t.tokid = m.maptokentype ) AS \"Token\",\n  pg_catalog.btrim(\n    \
         ARRAY( SELECT mm.mapdict::pg_catalog.regdictionary\n           FROM pg_catalog.pg_ts_config_map AS mm\n           \
         WHERE mm.mapcfg = m.mapcfg AND mm.maptokentype = m.maptokentype\n           \
         ORDER BY mapcfg, maptokentype, mapseqno\n    ) :: pg_catalog.text,\n  '{{}}') AS \"Dictionaries\"\n\
         FROM pg_catalog.pg_ts_config AS c, pg_catalog.pg_ts_config_map AS m\n\
         WHERE c.oid = '{oid}' AND m.mapcfg = c.oid\n\
         GROUP BY m.mapcfg, m.maptokentype, c.cfgparser\nORDER BY 1;"
    )
}

/// psql's `listTSParsers` (`\dFp`).
fn list_ts_parsers(pattern: Option<&str>, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT\n  n.nspname as \"Schema\",\n  p.prsname as \"Name\",\n  \
         pg_catalog.obj_description(p.oid, 'pg_ts_parser') as \"Description\"\n\
         FROM pg_catalog.pg_ts_parser p\nLEFT JOIN pg_catalog.pg_namespace n ON n.oid = p.prsnamespace\n",
    );
    ts_parsers().validate(&mut buf, pattern, false, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

fn ts_parsers() -> NameMatch<'static> {
    NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("p.prsname"),
        visibility_rule: Some("pg_catalog.pg_ts_parser_is_visible(p.oid)"),
        ..NameMatch::default()
    }
}

/// psql's `listTSParsersVerbose` lookup (`\dFp+`).
fn ts_parser_lookup(pattern: Option<&str>, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT p.oid,\n  n.nspname,\n  p.prsname\nFROM pg_catalog.pg_ts_parser p\n\
         LEFT JOIN pg_catalog.pg_namespace n ON n.oid = p.prsnamespace\n",
    );
    ts_parsers().validate(&mut buf, pattern, false, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `describeOneTSParser`, first table: parser `oid`'s methods.
fn ts_parser_methods_sql(oid: &str) -> String {
    let method = |label: &str, column: &str| format!(
        "SELECT '{label}',\n   p.{column}::pg_catalog.regproc,\n   pg_catalog.obj_description(p.{column}, 'pg_proc')\n \
         FROM pg_catalog.pg_ts_parser p\n WHERE p.oid = '{oid}'\n"
    );
    format!(
        "SELECT 'Start parse' AS \"Method\",\n   p.prsstart::pg_catalog.regproc AS \"Function\",\n   \
         pg_catalog.obj_description(p.prsstart, 'pg_proc') as \"Description\"\n FROM pg_catalog.pg_ts_parser p\n \
         WHERE p.oid = '{oid}'\nUNION ALL\n{}UNION ALL\n{}UNION ALL\n{}UNION ALL\n{}",
        method("Get next token", "prstoken"), method("End parse", "prsend"),
        method("Get headline", "prsheadline"), method("Get token types", "prslextype").trim_end_matches('\n').to_string() + ";",
    )
}

/// psql's `describeOneTSParser`, second table: parser `oid`'s token types.
fn ts_parser_tokens_sql(oid: &str) -> String {
    format!(
        "SELECT t.alias as \"Token name\",\n  t.description as \"Description\"\n\
         FROM pg_catalog.ts_token_type( '{oid}'::pg_catalog.oid ) as t\nORDER BY 1;"
    )
}

/// psql's `listTSDictionaries` (`\dFd`).
fn list_ts_dictionaries(pattern: Option<&str>, verbose: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from("SELECT\n  n.nspname as \"Schema\",\n  d.dictname as \"Name\",\n");
    if verbose {
        buf.push_str(
            "  ( SELECT COALESCE(nt.nspname, '(null)')::pg_catalog.text || '.' || t.tmplname FROM\n    \
             pg_catalog.pg_ts_template t\n    LEFT JOIN pg_catalog.pg_namespace nt ON nt.oid = t.tmplnamespace\n    \
             WHERE d.dicttemplate = t.oid ) AS  \"Template\",\n  \
             d.dictinitoption as \"Init options\",\n",
        );
    }
    buf.push_str(
        "  pg_catalog.obj_description(d.oid, 'pg_ts_dict') as \"Description\"\n\
         FROM pg_catalog.pg_ts_dict d\nLEFT JOIN pg_catalog.pg_namespace n ON n.oid = d.dictnamespace\n",
    );
    let dictionaries = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("d.dictname"),
        visibility_rule: Some("pg_catalog.pg_ts_dict_is_visible(d.oid)"),
        ..NameMatch::default()
    };
    dictionaries.validate(&mut buf, pattern, false, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `listTSTemplates` (`\dFt`).
fn list_ts_templates(pattern: Option<&str>, verbose: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from("SELECT\n  n.nspname AS \"Schema\",\n  t.tmplname AS \"Name\",\n");
    if verbose {
        buf.push_str("  t.tmplinit::pg_catalog.regproc AS \"Init\",\n  t.tmpllexize::pg_catalog.regproc AS \"Lexize\",\n");
    }
    buf.push_str(
        "  pg_catalog.obj_description(t.oid, 'pg_ts_template') AS \"Description\"\n\
         FROM pg_catalog.pg_ts_template t\nLEFT JOIN pg_catalog.pg_namespace n ON n.oid = t.tmplnamespace\n",
    );
    let templates = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("t.tmplname"),
        visibility_rule: Some("pg_catalog.pg_ts_template_is_visible(t.oid)"),
        ..NameMatch::default()
    };
    templates.validate(&mut buf, pattern, false, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `describeTypes` (`\dT`): types matched by internal or external
/// name; array types only when the pattern names one (`[]`), composite
/// types only when they stand alone.
fn describe_types(pattern: Option<&str>, verbose: bool, show_system: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT n.nspname as \"Schema\",\n  pg_catalog.format_type(t.oid, NULL) AS \"Name\",\n",
    );
    if verbose {
        buf.push_str(
            "  t.typname AS \"Internal name\",\n  \
             CASE WHEN t.typrelid != 0\n      THEN CAST('tuple' AS pg_catalog.text)\n    \
             WHEN t.typlen < 0\n      THEN CAST('var' AS pg_catalog.text)\n    \
             ELSE CAST(t.typlen AS pg_catalog.text)\n  END AS \"Size\",\n  \
             pg_catalog.array_to_string(\n      ARRAY(\n          SELECT e.enumlabel\n          \
             FROM pg_catalog.pg_enum e\n          WHERE e.enumtypid = t.oid\n          \
             ORDER BY e.enumsortorder\n      ),\n      E'\\n'\n  ) AS \"Elements\",\n  \
             pg_catalog.pg_get_userbyid(t.typowner) AS \"Owner\",\n",
        );
        buf.push_str(&acl_column("t.typacl"));
        buf.push_str(",\n  ");
    }
    buf.push_str("  pg_catalog.obj_description(t.oid, 'pg_type') as \"Description\"\n");
    buf.push_str("FROM pg_catalog.pg_type t\n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace\n");
    buf.push_str(
        "WHERE (t.typrelid = 0 OR (SELECT c.relkind = 'c' FROM pg_catalog.pg_class c WHERE c.oid = t.typrelid))\n",
    );
    if pattern.is_none_or(|p| !p.contains("[]")) {
        buf.push_str("  AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_type el WHERE el.oid = t.typelem AND el.typarray = t.oid)\n");
    }
    if !show_system && pattern.is_none() {
        buf.push_str("      AND n.nspname <> 'pg_catalog'\n      AND n.nspname <> 'information_schema'\n");
    }
    let types = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("t.typname"),
        alt_name_var:    Some("pg_catalog.format_type(t.oid, NULL)"),
        visibility_rule: Some("pg_catalog.pg_type_is_visible(t.oid)"),
        ..NameMatch::default()
    };
    types.validate(&mut buf, pattern.map(map_typename_pattern), true, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `permissionsList` (`\dp` / `\z`): tables, views, sequences …
/// with their privileges, column privileges and row-security policies.
fn permissions_list(pattern: Option<&str>, show_system: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT n.nspname as \"Schema\",\n  c.relname as \"Name\",\n  \
         CASE c.relkind WHEN 'r' THEN 'table' WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized view' \
         WHEN 'S' THEN 'sequence' WHEN 'f' THEN 'foreign table' WHEN 'p' THEN 'partitioned table' \
         END as \"Type\",\n  ",
    );
    buf.push_str(&acl_column("c.relacl"));
    buf.push_str(
        ",\n  pg_catalog.array_to_string(ARRAY(\n    \
         SELECT attname || E':\\n  ' || pg_catalog.array_to_string(attacl, E'\\n  ')\n    \
         FROM pg_catalog.pg_attribute a\n    \
         WHERE attrelid = c.oid AND NOT attisdropped AND attacl IS NOT NULL\n  \
         ), E'\\n') AS \"Column privileges\"\
         ,\n  pg_catalog.array_to_string(ARRAY(\n    SELECT polname\n    \
         || CASE WHEN NOT polpermissive THEN\n       E' (RESTRICTIVE)'\n       ELSE '' END\n    \
         || CASE WHEN polcmd != '*' THEN\n           E' (' || polcmd::pg_catalog.text || E'):'\n       \
         ELSE E':'\n       END\n    \
         || CASE WHEN polqual IS NOT NULL THEN\n           \
         E'\\n  (u): ' || pg_catalog.pg_get_expr(polqual, polrelid)\n       ELSE E''\n       END\n    \
         || CASE WHEN polwithcheck IS NOT NULL THEN\n           \
         E'\\n  (c): ' || pg_catalog.pg_get_expr(polwithcheck, polrelid)\n       ELSE E''\n       END    \
         || CASE WHEN polroles <> '{0}' THEN\n           \
         E'\\n  to: ' || pg_catalog.array_to_string(\n               ARRAY(\n                   \
         SELECT rolname\n                   FROM pg_catalog.pg_roles\n                   \
         WHERE oid = ANY (polroles)\n                   ORDER BY 1\n               ), E', ')\n       \
         ELSE E''\n       END\n    \
         FROM pg_catalog.pg_policy pol\n    WHERE polrelid = c.oid), E'\\n')\n    AS \"Policies\"",
    );
    buf.push_str(
        "\nFROM pg_catalog.pg_class c\n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n\
         WHERE c.relkind IN ('r','v','m','S','f','p')\n",
    );
    if !show_system && pattern.is_none() {
        buf.push_str("      AND n.nspname <> 'pg_catalog'\n      AND n.nspname <> 'information_schema'\n");
    }
    let relations = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("c.relname"),
        visibility_rule: Some("pg_catalog.pg_table_is_visible(c.oid)"),
        ..NameMatch::default()
    };
    relations.validate(&mut buf, pattern, true, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `describeTablespaces` (`\db`).
fn describe_tablespaces(pattern: Option<&str>, verbose: bool) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT spcname AS \"Name\",\n  pg_catalog.pg_get_userbyid(spcowner) AS \"Owner\",\n  \
         pg_catalog.pg_tablespace_location(oid) AS \"Location\"",
    );
    if verbose {
        buf.push_str(",\n  ");
        buf.push_str(&acl_column("spcacl"));
        buf.push_str(
            ",\n  spcoptions AS \"Options\"\
             ,\n  pg_catalog.pg_size_pretty(pg_catalog.pg_tablespace_size(oid)) AS \"Size\"\
             ,\n  pg_catalog.shobj_description(oid, 'pg_tablespace') AS \"Description\"",
        );
    }
    buf.push_str("\nFROM pg_catalog.pg_tablespace\n");
    let tablespaces = NameMatch { name_var: Some("spcname"), ..NameMatch::default() };
    tablespaces.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// psql's `listDomains` (`\dD`).
fn list_domains(pattern: Option<&str>, verbose: bool, show_system: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT n.nspname as \"Schema\",\n       t.typname as \"Name\",\n       \
         pg_catalog.format_type(t.typbasetype, t.typtypmod) as \"Type\",\n       \
         (SELECT c.collname FROM pg_catalog.pg_collation c, pg_catalog.pg_type bt\n        \
         WHERE c.oid = t.typcollation AND bt.oid = t.typbasetype AND t.typcollation <> bt.typcollation) as \"Collation\",\n       \
         CASE WHEN t.typnotnull THEN 'not null' END as \"Nullable\",\n       \
         t.typdefault as \"Default\",\n       \
         pg_catalog.array_to_string(ARRAY(\n         \
         SELECT pg_catalog.pg_get_constraintdef(r.oid, true) FROM pg_catalog.pg_constraint r \
         WHERE t.oid = r.contypid AND r.contype = 'c' ORDER BY r.conname\n       ), ' ') as \"Check\"",
    );
    if verbose {
        buf.push_str(",\n  ");
        buf.push_str(&acl_column("t.typacl"));
        buf.push_str(",\n       d.description as \"Description\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_type t\n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace\n");
    if verbose {
        buf.push_str("     LEFT JOIN pg_catalog.pg_description d ON d.classoid = t.tableoid AND d.objoid = t.oid AND d.objsubid = 0\n");
    }
    buf.push_str("WHERE t.typtype = 'd'\n");
    if !show_system && pattern.is_none() {
        buf.push_str("      AND n.nspname <> 'pg_catalog'\n      AND n.nspname <> 'information_schema'\n");
    }
    let domains = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("t.typname"),
        visibility_rule: Some("pg_catalog.pg_type_is_visible(t.oid)"),
        ..NameMatch::default()
    };
    domains.validate(&mut buf, pattern, true, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `describeAggregates` (`\da`).
fn describe_aggregates(pattern: Option<&str>, show_system: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT n.nspname as \"Schema\",\n  p.proname AS \"Name\",\n  \
         pg_catalog.format_type(p.prorettype, NULL) AS \"Result data type\",\n  \
         CASE WHEN p.pronargs = 0\n    THEN CAST('*' AS pg_catalog.text)\n    \
         ELSE pg_catalog.pg_get_function_arguments(p.oid)\n  END AS \"Argument data types\",\n  \
         pg_catalog.obj_description(p.oid, 'pg_proc') as \"Description\"\n\
         FROM pg_catalog.pg_proc p\n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace\n\
         WHERE p.prokind = 'a'\n",
    );
    if !show_system && pattern.is_none() {
        buf.push_str("      AND n.nspname <> 'pg_catalog'\n      AND n.nspname <> 'information_schema'\n");
    }
    let aggregates = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("p.proname"),
        visibility_rule: Some("pg_catalog.pg_function_is_visible(p.oid)"),
        ..NameMatch::default()
    };
    aggregates.validate(&mut buf, pattern, true, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2, 4;");
    Ok(buf)
}

/// psql's `describeOperators` (`\do[Sx+] [OPPTRN [TYPEPTRN [TYPEPTRN]]]`):
/// one type pattern matches a prefix operator's operand, two match the
/// left and right operands (`-` for none).
fn describe_operators(
    pattern: Option<&str>, arg_patterns: &[String], verbose: bool, show_system: bool, ctx: &MetaContext,
) -> Result<String, String> {
    const LEFT: &str = "CASE WHEN o.oprkind='l' THEN NULL ELSE pg_catalog.format_type(o.oprleft, NULL) END";
    const RIGHT: &str = "CASE WHEN o.oprkind='r' THEN NULL ELSE pg_catalog.format_type(o.oprright, NULL) END";
    let arg_patterns = &arg_patterns[..arg_patterns.len().min(2)];
    let mut buf = format!(
        "SELECT n.nspname as \"Schema\",\n  o.oprname AS \"Name\",\n  \
         {LEFT} AS \"Left arg type\",\n  \
         {RIGHT} AS \"Right arg type\",\n  \
         pg_catalog.format_type(o.oprresult, NULL) AS \"Result type\",\n"
    );
    if verbose {
        buf.push_str("  o.oprcode AS \"Function\",\n  CASE WHEN p.proleakproof THEN 'yes' ELSE 'no' END AS \"Leakproof?\",\n");
    }
    buf.push_str(
        "  coalesce(pg_catalog.obj_description(o.oid, 'pg_operator'),\n           \
         pg_catalog.obj_description(o.oprcode, 'pg_proc')) AS \"Description\"\n\
         FROM pg_catalog.pg_operator o\n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = o.oprnamespace\n",
    );
    match arg_patterns.len() {
        2 => buf.push_str(
            "     LEFT JOIN pg_catalog.pg_type t0 ON t0.oid = o.oprleft\n     \
             LEFT JOIN pg_catalog.pg_namespace nt0 ON nt0.oid = t0.typnamespace\n     \
             LEFT JOIN pg_catalog.pg_type t1 ON t1.oid = o.oprright\n     \
             LEFT JOIN pg_catalog.pg_namespace nt1 ON nt1.oid = t1.typnamespace\n",
        ),
        1 => buf.push_str(
            "     LEFT JOIN pg_catalog.pg_type t0 ON t0.oid = o.oprright\n     \
             LEFT JOIN pg_catalog.pg_namespace nt0 ON nt0.oid = t0.typnamespace\n",
        ),
        _ => {}
    }
    if verbose {
        buf.push_str("     LEFT JOIN pg_catalog.pg_proc p ON p.oid = o.oprcode\n");
    }
    let hide_system = !show_system && pattern.is_none();
    if hide_system {
        buf.push_str("WHERE n.nspname <> 'pg_catalog'\n      AND n.nspname <> 'information_schema'\n");
    }
    // Operator names are mostly regex characters: always literal.
    let operators = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("o.oprname"),
        visibility_rule: Some("pg_catalog.pg_operator_is_visible(o.oid)"),
        force_escape:    true,
        ..NameMatch::default()
    };
    operators.validate(&mut buf, pattern, hide_system, 3, &ctx.database).map_err(|e| e.to_string())?;
    if arg_patterns.len() == 1 {
        buf.push_str("  AND o.oprleft = 0\n");
    }
    for (i, arg) in arg_patterns.iter().enumerate() {
        if arg == "-" {
            buf.push_str(&format!("  AND t{i}.typname IS NULL\n"));
            continue;
        }
        let (nspname, typname) = (format!("nt{i}.nspname"), format!("t{i}.typname"));
        let (format_type, visible) = (
            format!("pg_catalog.format_type(t{i}.oid, NULL)"),
            format!("pg_catalog.pg_type_is_visible(t{i}.oid)"),
        );
        let types = NameMatch {
            schema_var:      Some(nspname.as_str()),
            name_var:        Some(typname.as_str()),
            alt_name_var:    Some(format_type.as_str()),
            visibility_rule: Some(visible.as_str()),
            ..NameMatch::default()
        };
        types.validate(&mut buf, Some(map_typename_pattern(arg)), true, 3, &ctx.database).map_err(|e| e.to_string())?;
    }
    // Its NULL operand types last.
    buf.push_str("ORDER BY 1, 2, 3, 4;");
    Ok(buf)
}

/// psql's `listPartitionedTables` (`\dP[itn][x+] [PATTERN]`): `t`
/// partitioned tables, `i` partitioned indexes (both when neither), `n`
/// nested ones too, with their parent.
fn list_partitioned_tables(options: &str, pattern: Option<&str>, verbose: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut show_tables = options.contains('t');
    let mut show_indexes = options.contains('i');
    let show_nested = options.contains('n');
    if !show_tables && !show_indexes {
        (show_tables, show_indexes) = (true, true);
    }
    let mixed = show_tables && show_indexes;
    let with_parent = show_nested || pattern.is_some();

    let mut buf = String::from(
        "SELECT n.nspname as \"Schema\",\n  c.relname as \"Name\",\n  pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\"",
    );
    if mixed {
        buf.push_str(",\n  CASE c.relkind WHEN 'p' THEN 'partitioned table' WHEN 'I' THEN 'partitioned index' END as \"Type\"");
    }
    if with_parent {
        buf.push_str(",\n  inh.inhparent::pg_catalog.regclass as \"Parent name\"");
    }
    if show_indexes {
        buf.push_str(",\n c2.oid::pg_catalog.regclass as \"Table\"");
    }
    if verbose {
        buf.push_str(",\n  am.amname as \"Access method\"");
        if show_nested {
            buf.push_str(",\n  s.dps as \"Leaf partition size\"");
        }
        buf.push_str(",\n  s.tps as \"Total size\"");
        buf.push_str(",\n  pg_catalog.obj_description(c.oid, 'pg_class') as \"Description\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_class c\n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace");
    if show_indexes {
        buf.push_str("\n     LEFT JOIN pg_catalog.pg_index i ON i.indexrelid = c.oid\n     \
                      LEFT JOIN pg_catalog.pg_class c2 ON i.indrelid = c2.oid");
    }
    if with_parent {
        buf.push_str("\n     LEFT JOIN pg_catalog.pg_inherits inh ON c.oid = inh.inhrelid");
    }
    if verbose {
        buf.push_str(
            "\n     LEFT JOIN pg_catalog.pg_am am ON c.relam = am.oid\
             ,\n     LATERAL (SELECT pg_catalog.pg_size_pretty(sum(\
             \n                 CASE WHEN ppt.isleaf AND ppt.level = 1\
             \n                      THEN pg_catalog.pg_table_size(ppt.relid) ELSE 0 END)) AS dps\
             ,\n                     pg_catalog.pg_size_pretty(sum(pg_catalog.pg_table_size(ppt.relid))) AS tps\
             \n              FROM pg_catalog.pg_partition_tree(c.oid) ppt) s",
        );
    }
    buf.push_str("\nWHERE c.relkind IN (");
    if show_tables { buf.push_str("'p',"); }
    if show_indexes { buf.push_str("'I',"); }
    buf.push_str("'')\n");
    if !with_parent {
        buf.push_str(" AND NOT c.relispartition\n");
    }
    if pattern.is_none() {
        buf.push_str("      AND n.nspname <> 'pg_catalog'\n      AND n.nspname !~ '^pg_toast'\n      AND n.nspname <> 'information_schema'\n");
    }
    let relations = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("c.relname"),
        visibility_rule: Some("pg_catalog.pg_table_is_visible(c.oid)"),
        ..NameMatch::default()
    };
    relations.validate(&mut buf, pattern, true, 3, &ctx.database).map_err(|e| e.to_string())?;
    // psql orders by "Parent name" (a regclass: by OID, roots first).
    buf.push_str(&format!(
        "ORDER BY \"Schema\", {}{}\"Name\";",
        if mixed { "\"Type\" DESC, " } else { "" },
        if with_parent { "\"Parent name\" NULLS FIRST, " } else { "" },
    ));
    Ok(buf)
}

/// psql's `listExtendedStats` (`\dX`), by server version as psql
/// branches (expression statistics from 14).
fn list_extended_stats(pattern: Option<&str>, sversion: u32, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT \nes.stxnamespace::pg_catalog.regnamespace::pg_catalog.text AS \"Schema\", \nes.stxname AS \"Name\", \n",
    );
    buf.push_str(if sversion >= 140000 {
        "pg_catalog.format('%s FROM %s', \n  pg_catalog.pg_get_statisticsobjdef_columns(es.oid), \n  \
         es.stxrelid::pg_catalog.regclass) AS \"Definition\""
    } else {
        "pg_catalog.format('%s FROM %s', \n  (SELECT pg_catalog.string_agg(pg_catalog.quote_ident(a.attname),', ') \n   \
         FROM pg_catalog.unnest(es.stxkeys) s(attnum) \n   JOIN pg_catalog.pg_attribute a \n   \
         ON (es.stxrelid = a.attrelid \n   AND a.attnum = s.attnum \n   AND NOT a.attisdropped)), \n\
         es.stxrelid::pg_catalog.regclass) AS \"Definition\""
    });
    buf.push_str(
        ",\nCASE WHEN 'd' = any(es.stxkind) THEN 'defined' \nEND AS \"Ndistinct\", \n\
         CASE WHEN 'f' = any(es.stxkind) THEN 'defined' \nEND AS \"Dependencies\"\
         ,\nCASE WHEN 'm' = any(es.stxkind) THEN 'defined' \nEND AS \"MCV\" ",
    );
    buf.push_str(" \nFROM pg_catalog.pg_statistic_ext es \n");
    let statistics = NameMatch {
        schema_var:      Some("es.stxnamespace::pg_catalog.regnamespace::pg_catalog.text"),
        name_var:        Some("es.stxname"),
        visibility_rule: Some("pg_catalog.pg_statistics_obj_is_visible(es.oid)"),
        ..NameMatch::default()
    };
    statistics.validate(&mut buf, pattern, false, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `describeConfigurationParameters` (`\dconfig`): parameters
/// matching the pattern (case-insensitively, dots and all), or without
/// one those changed from their defaults.
fn describe_configuration_parameters(pattern: Option<&str>, verbose: bool, sversion: u32) -> String {
    let mut buf = String::from("SELECT s.name AS \"Parameter\", pg_catalog.current_setting(s.name) AS \"Value\"");
    if verbose {
        buf.push_str(", s.vartype AS \"Type\", s.context AS \"Context\", ");
        if sversion >= 150000 {
            buf.push_str(&acl_column("p.paracl"));
        } else {
            buf.push_str("NULL AS \"Access privileges\"");
        }
    }
    buf.push_str("\nFROM pg_catalog.pg_settings s\n");
    if verbose && sversion >= 150000 {
        buf.push_str("  LEFT JOIN pg_catalog.pg_parameter_acl p\n  ON pg_catalog.lower(s.name) = p.parname\n");
    }
    match pattern {
        // psql takes the pattern as it is: no dotted-name checks.
        Some(_) => {
            let parameters = NameMatch { name_var: Some("pg_catalog.lower(s.name)"), ..NameMatch::default() };
            parameters.process(&mut buf, pattern, false);
        }
        None => buf.push_str("WHERE s.source <> 'default' AND\n      s.setting IS DISTINCT FROM s.boot_val\n"),
    }
    buf.push_str("ORDER BY 1;");
    buf
}

/// psql's `listDefaultACLs` (`\ddp`): the pattern matches the schema or
/// the owning role.
fn list_default_acls(pattern: Option<&str>, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT pg_catalog.pg_get_userbyid(d.defaclrole) AS \"Owner\",\n  \
         n.nspname AS \"Schema\",\n  \
         CASE d.defaclobjtype     WHEN 'r' THEN 'table' WHEN 'S' THEN 'sequence' WHEN 'f' THEN 'function'    \
         WHEN 'T' THEN 'type' WHEN 'n' THEN 'schema' WHEN 'L' THEN 'large object' END AS \"Type\",\n  ",
    );
    buf.push_str(&acl_column("d.defaclacl"));
    buf.push_str("\nFROM pg_catalog.pg_default_acl d\n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = d.defaclnamespace\n");
    let defaults = NameMatch {
        name_var:     Some("n.nspname"),
        alt_name_var: Some("pg_catalog.pg_get_userbyid(d.defaclrole)"),
        ..NameMatch::default()
    };
    defaults.validate(&mut buf, pattern, false, 3, &ctx.database).map_err(|e| e.to_string())?;
    // Its global defaults (no schema) last.
    buf.push_str("ORDER BY 1, 2, 3;");
    Ok(buf)
}

/// psql's `describeRoleGrants` (`\drg`): role memberships with their
/// options, by server version (per-grant INHERIT / SET from 16).
fn describe_role_grants(pattern: Option<&str>, show_system: bool, sversion: u32) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT m.rolname AS \"Role name\", r.rolname AS \"Member of\",\n  pg_catalog.concat_ws(', ',\n",
    );
    buf.push_str(if sversion >= 160000 {
        "    CASE WHEN pam.admin_option THEN 'ADMIN' END,\n    CASE WHEN pam.inherit_option THEN 'INHERIT' END,\n    \
         CASE WHEN pam.set_option THEN 'SET' END\n"
    } else {
        "    CASE WHEN pam.admin_option THEN 'ADMIN' END,\n    CASE WHEN m.rolinherit THEN 'INHERIT' END,\n    'SET'\n"
    });
    buf.push_str(
        "  ) AS \"Options\",\n  g.rolname AS \"Grantor\"\n\
         FROM pg_catalog.pg_roles m\n     JOIN pg_catalog.pg_auth_members pam ON (pam.member = m.oid)\n     \
         LEFT JOIN pg_catalog.pg_roles r ON (pam.roleid = r.oid)\n     \
         LEFT JOIN pg_catalog.pg_roles g ON (pam.grantor = g.oid)\n",
    );
    if !show_system && pattern.is_none() {
        buf.push_str("WHERE m.rolname !~ '^pg_'\n");
    }
    let roles = NameMatch { name_var: Some("m.rolname"), ..NameMatch::default() };
    roles.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2, 4;\n");
    Ok(buf)
}

/// psql's `listConversions` (`\dc`).
fn list_conversions(pattern: Option<&str>, verbose: bool, show_system: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT n.nspname AS \"Schema\",\n       c.conname AS \"Name\",\n       \
         pg_catalog.pg_encoding_to_char(c.conforencoding) AS \"Source\",\n       \
         pg_catalog.pg_encoding_to_char(c.contoencoding) AS \"Destination\",\n       \
         CASE WHEN c.condefault THEN 'yes'\n       ELSE 'no' END AS \"Default?\"",
    );
    if verbose {
        buf.push_str(",\n       d.description AS \"Description\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_conversion c\n     JOIN pg_catalog.pg_namespace n ON n.oid = c.connamespace\n");
    if verbose {
        buf.push_str("LEFT JOIN pg_catalog.pg_description d ON d.classoid = c.tableoid\n          AND d.objoid = c.oid AND d.objsubid = 0\n");
    }
    buf.push_str("WHERE true\n");
    if !show_system && pattern.is_none() {
        buf.push_str("  AND n.nspname <> 'pg_catalog'\n  AND n.nspname <> 'information_schema'\n");
    }
    let conversions = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("c.conname"),
        visibility_rule: Some("pg_catalog.pg_conversion_is_visible(c.oid)"),
        ..NameMatch::default()
    };
    conversions.validate(&mut buf, pattern, true, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `listCasts` (`\dC`): the pattern matches either type's internal
/// or external name.
fn list_casts(pattern: Option<&str>, verbose: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT pg_catalog.format_type(castsource, NULL) AS \"Source type\",\n       \
         pg_catalog.format_type(casttarget, NULL) AS \"Target type\",\n       \
         CASE WHEN c.castmethod = 'b' THEN '(binary coercible)'\n            \
         WHEN c.castmethod = 'i' THEN '(with inout)'\n            ELSE p.proname\n       END AS \"Function\",\n       \
         CASE WHEN c.castcontext = 'e' THEN 'no'\n            WHEN c.castcontext = 'a' THEN 'in assignment'\n            \
         ELSE 'yes'\n       END AS \"Implicit?\"",
    );
    if verbose {
        buf.push_str(
            ",\n       CASE WHEN p.proleakproof THEN 'yes'\n            ELSE 'no'\n       END AS \"Leakproof?\",\n       \
             d.description AS \"Description\"",
        );
    }
    buf.push_str(
        "\nFROM pg_catalog.pg_cast c LEFT JOIN pg_catalog.pg_proc p\n     ON c.castfunc = p.oid\n     \
         LEFT JOIN pg_catalog.pg_type ts\n     ON c.castsource = ts.oid\n     \
         LEFT JOIN pg_catalog.pg_namespace ns\n     ON ns.oid = ts.typnamespace\n     \
         LEFT JOIN pg_catalog.pg_type tt\n     ON c.casttarget = tt.oid\n     \
         LEFT JOIN pg_catalog.pg_namespace nt\n     ON nt.oid = tt.typnamespace\n",
    );
    if verbose {
        buf.push_str("     LEFT JOIN pg_catalog.pg_description d\n     ON d.classoid = c.tableoid AND d.objoid = c.oid AND d.objsubid = 0\n");
    }
    let side = |prefix: &str| (format!("n{prefix}.nspname"), format!("t{prefix}.typname"),
                               format!("pg_catalog.format_type(t{prefix}.oid, NULL)"),
                               format!("pg_catalog.pg_type_is_visible(t{prefix}.oid)"));
    buf.push_str("WHERE ( (true");
    for (i, prefix) in ["s", "t"].into_iter().enumerate() {
        if i == 1 { buf.push_str(") OR (true"); }
        let (nspname, typname, format_type, visible) = side(prefix);
        let types = NameMatch {
            schema_var:      Some(nspname.as_str()),
            name_var:        Some(typname.as_str()),
            alt_name_var:    Some(format_type.as_str()),
            visibility_rule: Some(visible.as_str()),
            ..NameMatch::default()
        };
        types.validate(&mut buf, pattern, true, 3, &ctx.database).map_err(|e| e.to_string())?;
    }
    buf.push_str(") )\nORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `objectDescription` (`\dd`): comments on the objects no other
/// list shows them for — table and domain constraints, operator classes
/// and families, rules, triggers. The pattern applies to each kind.
fn object_descriptions(pattern: Option<&str>, show_system: bool, ctx: &MetaContext) -> Result<String, String> {
    const SYSTEM_WHERE: &str = "WHERE n.nspname <> 'pg_catalog'\n      AND n.nspname <> 'information_schema'\n";
    const SYSTEM_AND: &str = "      AND n.nspname <> 'pg_catalog'\n      AND n.nspname <> 'information_schema'\n";
    let hide_system = !show_system && pattern.is_none();
    // Each kind: its rows, where psql filters system schemas (opening a
    // WHERE, or continuing its own), its name column and visibility.
    let kinds: [(&str, &str, &str, &str); 6] = [
        ("  SELECT pgc.oid as oid, pgc.tableoid AS tableoid,\n  n.nspname as nspname,\n  \
          CAST(pgc.conname AS pg_catalog.text) as name,  CAST('table constraint' AS pg_catalog.text) as object\n  \
          FROM pg_catalog.pg_constraint pgc\n    JOIN pg_catalog.pg_class c ON c.oid = pgc.conrelid\n    \
          LEFT JOIN pg_catalog.pg_namespace n     ON n.oid = c.relnamespace\n",
         SYSTEM_WHERE, "pgc.conname", "pg_catalog.pg_table_is_visible(c.oid)"),
        ("UNION ALL\n  SELECT pgc.oid as oid, pgc.tableoid AS tableoid,\n  n.nspname as nspname,\n  \
          CAST(pgc.conname AS pg_catalog.text) as name,  CAST('domain constraint' AS pg_catalog.text) as object\n  \
          FROM pg_catalog.pg_constraint pgc\n    JOIN pg_catalog.pg_type t ON t.oid = pgc.contypid\n    \
          LEFT JOIN pg_catalog.pg_namespace n     ON n.oid = t.typnamespace\n",
         SYSTEM_WHERE, "pgc.conname", "pg_catalog.pg_type_is_visible(t.oid)"),
        ("UNION ALL\n  SELECT o.oid as oid, o.tableoid as tableoid,\n  n.nspname as nspname,\n  \
          CAST(o.opcname AS pg_catalog.text) as name,\n  CAST('operator class' AS pg_catalog.text) as object\n  \
          FROM pg_catalog.pg_opclass o\n    JOIN pg_catalog.pg_am am ON o.opcmethod = am.oid\n    \
          JOIN pg_catalog.pg_namespace n ON n.oid = o.opcnamespace\n",
         SYSTEM_AND, "o.opcname", "pg_catalog.pg_opclass_is_visible(o.oid)"),
        ("UNION ALL\n  SELECT opf.oid as oid, opf.tableoid as tableoid,\n  n.nspname as nspname,\n  \
          CAST(opf.opfname AS pg_catalog.text) AS name,\n  CAST('operator family' AS pg_catalog.text) as object\n  \
          FROM pg_catalog.pg_opfamily opf\n    JOIN pg_catalog.pg_am am ON opf.opfmethod = am.oid\n    \
          JOIN pg_catalog.pg_namespace n ON opf.opfnamespace = n.oid\n",
         SYSTEM_AND, "opf.opfname", "pg_catalog.pg_opfamily_is_visible(opf.oid)"),
        ("UNION ALL\n  SELECT r.oid as oid, r.tableoid as tableoid,\n  n.nspname as nspname,\n  \
          CAST(r.rulename AS pg_catalog.text) as name,  CAST('rule' AS pg_catalog.text) as object\n  \
          FROM pg_catalog.pg_rewrite r\n       JOIN pg_catalog.pg_class c ON c.oid = r.ev_class\n       \
          LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n  WHERE r.rulename != '_RETURN'\n",
         SYSTEM_AND, "r.rulename", "pg_catalog.pg_table_is_visible(c.oid)"),
        ("UNION ALL\n  SELECT t.oid as oid, t.tableoid as tableoid,\n  n.nspname as nspname,\n  \
          CAST(t.tgname AS pg_catalog.text) as name,  CAST('trigger' AS pg_catalog.text) as object\n  \
          FROM pg_catalog.pg_trigger t\n       JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid\n       \
          LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n",
         SYSTEM_WHERE, "t.tgname", "pg_catalog.pg_table_is_visible(c.oid)"),
    ];
    let mut buf = String::from(
        "SELECT DISTINCT tt.nspname AS \"Schema\", tt.name AS \"Name\", tt.object AS \"Object\", \
         d.description AS \"Description\"\nFROM (\n",
    );
    for (rows, system_filter, name_var, visibility_rule) in kinds {
        buf.push_str(rows);
        if hide_system { buf.push_str(system_filter); }
        // A filter that opened the WHERE leaves it to the pattern otherwise.
        let have_where = system_filter == SYSTEM_AND || hide_system;
        let objects = NameMatch {
            schema_var:      Some("n.nspname"),
            name_var:        Some(name_var),
            visibility_rule: Some(visibility_rule),
            ..NameMatch::default()
        };
        objects.validate(&mut buf, pattern, have_where, 3, &ctx.database).map_err(|e| e.to_string())?;
    }
    buf.push_str(
        ") AS tt\n  JOIN pg_catalog.pg_description d ON (tt.oid = d.objoid AND tt.tableoid = d.classoid AND d.objsubid = 0)\n\
         ORDER BY 1, 2, 3;",
    );
    Ok(buf)
}

/// psql's `listLanguages` (`\dL`). Without `S` or a pattern, internal
/// languages (no call handler) are left out.
fn list_languages(pattern: Option<&str>, verbose: bool, show_system: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT l.lanname AS \"Name\",\n       pg_catalog.pg_get_userbyid(l.lanowner) as \"Owner\",\n       \
         l.lanpltrusted AS \"Trusted\"",
    );
    if verbose {
        buf.push_str(
            ",\n       NOT l.lanispl AS \"Internal language\",\n       \
             l.lanplcallfoid::pg_catalog.regprocedure AS \"Call handler\",\n       \
             l.lanvalidator::pg_catalog.regprocedure AS \"Validator\",\n       \
             l.laninline::pg_catalog.regprocedure AS \"Inline handler\",\n       ",
        );
        buf.push_str(&acl_column("l.lanacl"));
    }
    buf.push_str(
        ",\n       d.description AS \"Description\"\nFROM pg_catalog.pg_language l\n\
         LEFT JOIN pg_catalog.pg_description d\n  ON d.classoid = l.tableoid AND d.objoid = l.oid\n  AND d.objsubid = 0\n",
    );
    if pattern.is_some() {
        let languages = NameMatch { name_var: Some("l.lanname"), ..NameMatch::default() };
        languages.validate(&mut buf, pattern, false, 2, &ctx.database).map_err(|e| e.to_string())?;
    } else if !show_system {
        buf.push_str("WHERE l.lanplcallfoid != 0\n");
    }
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// psql's `listCollations` (`\dO`), by server version as psql branches
/// (ICU locale from 15, ICU rules from 16, `colllocale` from 17); only
/// collations usable in the database's encoding.
fn list_collations(
    pattern: Option<&str>, verbose: bool, show_system: bool, sversion: u32, ctx: &MetaContext,
) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT\n  n.nspname AS \"Schema\",\n  c.collname AS \"Name\",\n  \
         CASE c.collprovider WHEN 'd' THEN 'default' WHEN 'b' THEN 'builtin' WHEN 'c' THEN 'libc' WHEN 'i' THEN 'icu' END \
         AS \"Provider\",\n  c.collcollate AS \"Collate\",\n  c.collctype AS \"Ctype\",\n",
    );
    buf.push_str(if sversion >= 170000 {
        "  c.colllocale AS \"Locale\",\n"
    } else if sversion >= 150000 {
        "  c.colliculocale AS \"Locale\",\n"
    } else {
        "  c.collcollate AS \"Locale\",\n"
    });
    buf.push_str(if sversion >= 160000 {
        "  c.collicurules AS \"ICU Rules\",\n"
    } else {
        "  NULL AS \"ICU Rules\",\n"
    });
    buf.push_str("  CASE WHEN c.collisdeterministic THEN 'yes' ELSE 'no' END AS \"Deterministic?\"");
    if verbose {
        buf.push_str(",\n  pg_catalog.obj_description(c.oid, 'pg_collation') AS \"Description\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_collation c, pg_catalog.pg_namespace n\nWHERE n.oid = c.collnamespace\n");
    if !show_system && pattern.is_none() {
        buf.push_str("      AND n.nspname <> 'pg_catalog'\n      AND n.nspname <> 'information_schema'\n");
    }
    buf.push_str("      AND c.collencoding IN (-1, pg_catalog.pg_char_to_encoding(pg_catalog.getdatabaseencoding()))\n");
    let collations = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("c.collname"),
        visibility_rule: Some("pg_catalog.pg_collation_is_visible(c.oid)"),
        ..NameMatch::default()
    };
    collations.validate(&mut buf, pattern, true, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `listEventTriggers` (`\dy`).
fn list_event_triggers(pattern: Option<&str>, verbose: bool) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT evtname as \"Name\", evtevent as \"Event\", pg_catalog.pg_get_userbyid(e.evtowner) as \"Owner\",\n \
         case evtenabled when 'O' then 'enabled'  when 'R' then 'replica'  when 'A' then 'always'  \
         when 'D' then 'disabled' end as \"Enabled\",\n \
         e.evtfoid::pg_catalog.regproc as \"Function\", \
         pg_catalog.array_to_string(array(select x from pg_catalog.unnest(evttags) as t(x)), ', ') as \"Tags\"",
    );
    if verbose {
        buf.push_str(",\npg_catalog.obj_description(e.oid, 'pg_event_trigger') as \"Description\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_event_trigger e ");
    let triggers = NameMatch { name_var: Some("evtname"), ..NameMatch::default() };
    triggers.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1");
    Ok(buf)
}

/// psql's `listDbRoleSettings` (`\drds [ROLEPTRN [DBPTRN]]`), with the
/// message psql prints instead of an empty table.
fn list_db_role_settings(role: Option<&str>, database: Option<&str>) -> Result<(String, String), String> {
    let mut buf = String::from(
        "SELECT rolname AS \"Role\", datname AS \"Database\",\n\
         pg_catalog.array_to_string(setconfig, E'\\n') AS \"Settings\"\nFROM pg_catalog.pg_db_role_setting s\n\
         LEFT JOIN pg_catalog.pg_database d ON d.oid = setdatabase\nLEFT JOIN pg_catalog.pg_roles r ON r.oid = setrole\n",
    );
    // psql reads a database pattern only after a role pattern.
    let database = database.filter(|_| role.is_some());
    let roles = NameMatch { name_var: Some("r.rolname"), ..NameMatch::default() };
    let have_where = roles.validate(&mut buf, role, false, 1, "").map_err(|e| e.to_string())?;
    let databases = NameMatch { name_var: Some("d.datname"), ..NameMatch::default() };
    databases.validate(&mut buf, database, have_where, 1, "").map_err(|e| e.to_string())?;
    // Settings for every role or database last.
    buf.push_str("ORDER BY 1, 2;");
    let none = match (role, database) {
        (Some(r), Some(d)) => format!("Did not find any settings for role \"{r}\" and database \"{d}\"."),
        (Some(r), None) => format!("Did not find any settings for role \"{r}\"."),
        _ => "Did not find any settings.".into(),
    };
    Ok((buf, none))
}

/// psql's `describeAccessMethods` (`\dA`).
fn describe_access_methods(pattern: Option<&str>, verbose: bool) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT amname AS \"Name\",\n  CASE amtype WHEN 'i' THEN 'Index' WHEN 't' THEN 'Table' END AS \"Type\"",
    );
    if verbose {
        buf.push_str(",\n  amhandler AS \"Handler\",\n  pg_catalog.obj_description(oid, 'pg_am') AS \"Description\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_am\n");
    let methods = NameMatch { name_var: Some("amname"), ..NameMatch::default() };
    methods.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// The access-method pattern of the `\dA` subcommands, opening the WHERE;
/// whether it added a clause.
fn match_access_method(buf: &mut String, pattern: Option<&str>) -> Result<bool, String> {
    let methods = NameMatch { name_var: Some("am.amname"), ..NameMatch::default() };
    methods.validate(buf, pattern, false, 1, "").map_err(|e| e.to_string())
}

/// A type pattern on `t` / `tn`, internal or external name.
fn type_names() -> NameMatch<'static> {
    NameMatch {
        schema_var:      Some("tn.nspname"),
        name_var:        Some("t.typname"),
        alt_name_var:    Some("pg_catalog.format_type(t.oid, NULL)"),
        visibility_rule: Some("pg_catalog.pg_type_is_visible(t.oid)"),
        ..NameMatch::default()
    }
}

/// psql's `listOperatorClasses` (`\dAc [AMPTRN [TYPEPTRN]]`).
fn list_operator_classes(
    am_pattern: Option<&str>, type_pattern: Option<&str>, verbose: bool, ctx: &MetaContext,
) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT\n  am.amname AS \"AM\",\n  pg_catalog.format_type(c.opcintype, NULL) AS \"Input type\",\n  \
         CASE\n    WHEN c.opckeytype <> 0 AND c.opckeytype <> c.opcintype\n    \
         THEN pg_catalog.format_type(c.opckeytype, NULL)\n    ELSE NULL\n  END AS \"Storage type\",\n  \
         CASE\n    WHEN pg_catalog.pg_opclass_is_visible(c.oid)\n    THEN pg_catalog.format('%I', c.opcname)\n    \
         ELSE pg_catalog.format('%I.%I', n.nspname, c.opcname)\n  END AS \"Operator class\",\n  \
         (CASE WHEN c.opcdefault\n    THEN 'yes'\n    ELSE 'no'\n  END) AS \"Default?\"",
    );
    if verbose {
        buf.push_str(
            ",\n  CASE\n    WHEN pg_catalog.pg_opfamily_is_visible(of.oid)\n    THEN pg_catalog.format('%I', of.opfname)\n    \
             ELSE pg_catalog.format('%I.%I', ofn.nspname, of.opfname)\n  END AS \"Operator family\",\n \
             pg_catalog.pg_get_userbyid(c.opcowner) AS \"Owner\"\n",
        );
    }
    buf.push_str(
        "\nFROM pg_catalog.pg_opclass c\n  LEFT JOIN pg_catalog.pg_am am on am.oid = c.opcmethod\n  \
         LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.opcnamespace\n  \
         LEFT JOIN pg_catalog.pg_type t ON t.oid = c.opcintype\n  \
         LEFT JOIN pg_catalog.pg_namespace tn ON tn.oid = t.typnamespace\n",
    );
    if verbose {
        buf.push_str(
            "  LEFT JOIN pg_catalog.pg_opfamily of ON of.oid = c.opcfamily\n  \
             LEFT JOIN pg_catalog.pg_namespace ofn ON ofn.oid = of.opfnamespace\n",
        );
    }
    let have_where = match_access_method(&mut buf, am_pattern)?;
    if type_pattern.is_some() {
        type_names().validate(&mut buf, type_pattern, have_where, 3, &ctx.database).map_err(|e| e.to_string())?;
    }
    buf.push_str("ORDER BY 1, 2, 4;");
    Ok(buf)
}

/// psql's `listOperatorFamilies` (`\dAf [AMPTRN [TYPEPTRN]]`): the type
/// pattern matches any of a family's operator classes.
fn list_operator_families(
    am_pattern: Option<&str>, type_pattern: Option<&str>, verbose: bool, ctx: &MetaContext,
) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT\n  am.amname AS \"AM\",\n  CASE\n    WHEN pg_catalog.pg_opfamily_is_visible(f.oid)\n    \
         THEN pg_catalog.format('%I', f.opfname)\n    ELSE pg_catalog.format('%I.%I', n.nspname, f.opfname)\n  \
         END AS \"Operator family\",\n  (SELECT\n     \
         pg_catalog.string_agg(pg_catalog.format_type(oc.opcintype, NULL), ', ')\n   \
         FROM pg_catalog.pg_opclass oc\n   WHERE oc.opcfamily = f.oid) \"Applicable types\"",
    );
    if verbose {
        buf.push_str(",\n  pg_catalog.pg_get_userbyid(f.opfowner) AS \"Owner\"\n");
    }
    buf.push_str(
        "\nFROM pg_catalog.pg_opfamily f\n  LEFT JOIN pg_catalog.pg_am am on am.oid = f.opfmethod\n  \
         LEFT JOIN pg_catalog.pg_namespace n ON n.oid = f.opfnamespace\n",
    );
    let have_where = match_access_method(&mut buf, am_pattern)?;
    if type_pattern.is_some() {
        buf.push_str(&format!(
            "  {} EXISTS (\n    SELECT 1\n    FROM pg_catalog.pg_type t\n    \
             JOIN pg_catalog.pg_opclass oc ON oc.opcintype = t.oid\n    \
             LEFT JOIN pg_catalog.pg_namespace tn ON tn.oid = t.typnamespace\n    WHERE oc.opcfamily = f.oid\n",
            if have_where { "AND" } else { "WHERE" },
        ));
        type_names().validate(&mut buf, type_pattern, true, 3, &ctx.database).map_err(|e| e.to_string())?;
        buf.push_str("  )\n");
    }
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// An operator-family pattern (`\dAo`, `\dAp`), schema-qualified by `ns`.
fn match_family(buf: &mut String, pattern: Option<&str>, have_where: bool, ns: &str, ctx: &MetaContext) -> Result<(), String> {
    let schema = format!("{ns}.nspname");
    let families = NameMatch { schema_var: Some(&schema), name_var: Some("of.opfname"), ..NameMatch::default() };
    families.validate(buf, pattern, have_where, 3, &ctx.database).map(|_| ()).map_err(|e| e.to_string())
}

/// psql's `listOpFamilyOperators` (`\dAo [AMPTRN [OPFPTRN]]`).
fn list_opfamily_operators(
    am_pattern: Option<&str>, family_pattern: Option<&str>, verbose: bool, ctx: &MetaContext,
) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT\n  am.amname AS \"AM\",\n  CASE\n    WHEN pg_catalog.pg_opfamily_is_visible(of.oid)\n    \
         THEN pg_catalog.format('%I', of.opfname)\n    ELSE pg_catalog.format('%I.%I', nsf.nspname, of.opfname)\n  \
         END AS \"Operator family\",\n  o.amopopr::pg_catalog.regoperator AS \"Operator\"\n,  \
         o.amopstrategy AS \"Strategy\",\n  CASE o.amoppurpose\n    WHEN 'o' THEN 'ordering'\n    \
         WHEN 's' THEN 'search'\n  END AS \"Purpose\"\n",
    );
    if verbose {
        buf.push_str(
            ", ofs.opfname AS \"Sort opfamily\",\n  CASE\n    WHEN p.proleakproof THEN 'yes'\n    \
             ELSE 'no'\n  END AS \"Leakproof?\"\n",
        );
    }
    buf.push_str(
        "FROM pg_catalog.pg_amop o\n  LEFT JOIN pg_catalog.pg_opfamily of ON of.oid = o.amopfamily\n  \
         LEFT JOIN pg_catalog.pg_am am ON am.oid = of.opfmethod AND am.oid = o.amopmethod\n  \
         LEFT JOIN pg_catalog.pg_namespace nsf ON of.opfnamespace = nsf.oid\n",
    );
    if verbose {
        buf.push_str(
            "  LEFT JOIN pg_catalog.pg_opfamily ofs ON ofs.oid = o.amopsortfamily\n  \
             LEFT JOIN pg_catalog.pg_operator op ON op.oid = o.amopopr\n  \
             LEFT JOIN pg_catalog.pg_proc p ON p.oid = op.oprcode\n",
        );
    }
    let have_where = match_access_method(&mut buf, am_pattern)?;
    if family_pattern.is_some() {
        match_family(&mut buf, family_pattern, have_where, "nsf", ctx)?;
    }
    buf.push_str(
        "ORDER BY 1, 2,\n  o.amoplefttype = o.amoprighttype DESC,\n  pg_catalog.format_type(o.amoplefttype, NULL),\n  \
         pg_catalog.format_type(o.amoprighttype, NULL),\n  o.amopstrategy;",
    );
    Ok(buf)
}

/// psql's `listOpFamilyFunctions` (`\dAp [AMPTRN [OPFPTRN]]`).
fn list_opfamily_functions(
    am_pattern: Option<&str>, family_pattern: Option<&str>, verbose: bool, ctx: &MetaContext,
) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT\n  am.amname AS \"AM\",\n  CASE\n    WHEN pg_catalog.pg_opfamily_is_visible(of.oid)\n    \
         THEN pg_catalog.format('%I', of.opfname)\n    ELSE pg_catalog.format('%I.%I', ns.nspname, of.opfname)\n  \
         END AS \"Operator family\",\n  pg_catalog.format_type(ap.amproclefttype, NULL) AS \"Registered left type\",\n  \
         pg_catalog.format_type(ap.amprocrighttype, NULL) AS \"Registered right type\",\n  ap.amprocnum AS \"Number\"\n",
    );
    buf.push_str(if verbose {
        ", ap.amproc::pg_catalog.regprocedure AS \"Function\"\n"
    } else {
        ", p.proname AS \"Function\"\n"
    });
    buf.push_str(
        "FROM pg_catalog.pg_amproc ap\n  LEFT JOIN pg_catalog.pg_opfamily of ON of.oid = ap.amprocfamily\n  \
         LEFT JOIN pg_catalog.pg_am am ON am.oid = of.opfmethod\n  \
         LEFT JOIN pg_catalog.pg_namespace ns ON of.opfnamespace = ns.oid\n  \
         LEFT JOIN pg_catalog.pg_proc p ON ap.amproc = p.oid\n",
    );
    let have_where = match_access_method(&mut buf, am_pattern)?;
    if family_pattern.is_some() {
        match_family(&mut buf, family_pattern, have_where, "ns", ctx)?;
    }
    buf.push_str("ORDER BY 1, 2,\n  ap.amproclefttype = ap.amprocrighttype DESC,\n  3, 4, 5;");
    Ok(buf)
}

/// psql's display of a foreign object's options: `(name 'value', …)`,
/// blank without any.
fn fdw_options(column: &str) -> String {
    format!(
        "CASE WHEN {column} IS NULL THEN '' ELSE   '(' || pg_catalog.array_to_string(ARRAY(SELECT   \
         pg_catalog.quote_ident(option_name) ||  ' ' ||   pg_catalog.quote_literal(option_value)  FROM   \
         pg_catalog.pg_options_to_table({column})),  ', ') || ')'   END AS \"FDW options\""
    )
}

/// psql's `listForeignServers` (`\des`).
fn list_foreign_servers(pattern: Option<&str>, verbose: bool) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT s.srvname AS \"Name\",\n  pg_catalog.pg_get_userbyid(s.srvowner) AS \"Owner\",\n  \
         f.fdwname AS \"Foreign-data wrapper\"",
    );
    if verbose {
        buf.push_str(",\n  ");
        buf.push_str(&acl_column("s.srvacl"));
        buf.push_str(",\n  s.srvtype AS \"Type\",\n  s.srvversion AS \"Version\",\n  ");
        buf.push_str(&fdw_options("srvoptions"));
        buf.push_str(",\n  d.description AS \"Description\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_foreign_server s\n     JOIN pg_catalog.pg_foreign_data_wrapper f ON f.oid=s.srvfdw\n");
    if verbose {
        buf.push_str("LEFT JOIN pg_catalog.pg_description d\n       ON d.classoid = s.tableoid AND d.objoid = s.oid AND d.objsubid = 0\n");
    }
    let servers = NameMatch { name_var: Some("s.srvname"), ..NameMatch::default() };
    servers.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// psql's `listUserMappings` (`\deu`): the pattern matches the server or
/// the user.
fn list_user_mappings(pattern: Option<&str>, verbose: bool) -> Result<String, String> {
    let mut buf = String::from("SELECT um.srvname AS \"Server\",\n  um.usename AS \"User name\"");
    if verbose {
        buf.push_str(",\n ");
        buf.push_str(&fdw_options("umoptions"));
    }
    buf.push_str("\nFROM pg_catalog.pg_user_mappings um\n");
    let mappings = NameMatch { name_var: Some("um.srvname"), alt_name_var: Some("um.usename"), ..NameMatch::default() };
    mappings.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `listForeignDataWrappers` (`\dew`).
fn list_foreign_data_wrappers(pattern: Option<&str>, verbose: bool) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT fdw.fdwname AS \"Name\",\n  pg_catalog.pg_get_userbyid(fdw.fdwowner) AS \"Owner\",\n  \
         fdw.fdwhandler::pg_catalog.regproc AS \"Handler\",\n  fdw.fdwvalidator::pg_catalog.regproc AS \"Validator\"",
    );
    if verbose {
        buf.push_str(",\n  ");
        buf.push_str(&acl_column("fdwacl"));
        buf.push_str(",\n ");
        buf.push_str(&fdw_options("fdwoptions"));
        buf.push_str(",\n  d.description AS \"Description\" ");
    }
    buf.push_str("\nFROM pg_catalog.pg_foreign_data_wrapper fdw\n");
    if verbose {
        buf.push_str("LEFT JOIN pg_catalog.pg_description d\n       ON d.classoid = fdw.tableoid AND d.objoid = fdw.oid AND d.objsubid = 0\n");
    }
    let wrappers = NameMatch { name_var: Some("fdwname"), ..NameMatch::default() };
    wrappers.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// psql's `listForeignTables` (`\det`).
fn list_foreign_tables(pattern: Option<&str>, verbose: bool, ctx: &MetaContext) -> Result<String, String> {
    let mut buf = String::from("SELECT n.nspname AS \"Schema\",\n  c.relname AS \"Table\",\n  s.srvname AS \"Server\"");
    if verbose {
        buf.push_str(",\n ");
        buf.push_str(&fdw_options("ftoptions"));
        buf.push_str(",\n  d.description AS \"Description\"");
    }
    buf.push_str(
        "\nFROM pg_catalog.pg_foreign_table ft\n  INNER JOIN pg_catalog.pg_class c ON c.oid = ft.ftrelid\n  \
         INNER JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n  \
         INNER JOIN pg_catalog.pg_foreign_server s ON s.oid = ft.ftserver\n",
    );
    if verbose {
        buf.push_str("   LEFT JOIN pg_catalog.pg_description d\n          ON d.classoid = c.tableoid AND d.objoid = c.oid AND d.objsubid = 0\n");
    }
    let tables = NameMatch {
        schema_var:      Some("n.nspname"),
        name_var:        Some("c.relname"),
        visibility_rule: Some("pg_catalog.pg_table_is_visible(c.oid)"),
        ..NameMatch::default()
    };
    tables.validate(&mut buf, pattern, false, 3, &ctx.database).map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// psql's `listPublications` (`\dRp`), by server version (Generated
/// columns from 18).
fn list_publications(pattern: Option<&str>, sversion: u32) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT pubname AS \"Name\",\n  pg_catalog.pg_get_userbyid(pubowner) AS \"Owner\",\n  \
         puballtables AS \"All tables\",\n  pubinsert AS \"Inserts\",\n  pubupdate AS \"Updates\",\n  \
         pubdelete AS \"Deletes\",\n  pubtruncate AS \"Truncates\"",
    );
    if sversion >= 180000 {
        buf.push_str(",\n (CASE pubgencols\n    WHEN 'n' THEN 'none'\n    WHEN 's' THEN 'stored'\n   END) AS \"Generated columns\"");
    }
    buf.push_str(",\n  pubviaroot AS \"Via root\"\nFROM pg_catalog.pg_publication\n");
    let publications = NameMatch { name_var: Some("pubname"), ..NameMatch::default() };
    publications.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// psql's `describePublications` lookup (`\dRp+`): each publication's
/// settings, in psql's column order whatever the server version.
fn publication_lookup(pattern: Option<&str>, sversion: u32) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT oid, pubname,\n  pg_catalog.pg_get_userbyid(pubowner) AS owner,\n  \
         puballtables, pubinsert, pubupdate, pubdelete, pubtruncate",
    );
    buf.push_str(if sversion >= 180000 {
        ", (CASE pubgencols\n    WHEN 'n' THEN 'none'\n    WHEN 's' THEN 'stored'\n   END) AS \"Generated columns\"\n"
    } else {
        ", 'none' AS pubgencols"
    });
    buf.push_str(", pubviaroot\nFROM pg_catalog.pg_publication\n");
    let publications = NameMatch { name_var: Some("pubname"), ..NameMatch::default() };
    publications.validate(&mut buf, pattern, false, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 2;");
    Ok(buf)
}

/// The tables publication `oid` lists, with their row filter and
/// columns from 15.
fn publication_tables_sql(oid: &str, sversion: u32) -> String {
    let filters = if sversion >= 150000 {
        ", pg_get_expr(pr.prqual, c.oid), (CASE WHEN pr.prattrs IS NOT NULL THEN\n     \
         pg_catalog.array_to_string(      ARRAY(SELECT attname\n              FROM\n                \
         pg_catalog.generate_series(0, pg_catalog.array_upper(pr.prattrs::pg_catalog.int2[], 1)) s,\n                \
         pg_catalog.pg_attribute\n        WHERE attrelid = c.oid AND attnum = prattrs[s]), ', ')\n       ELSE NULL END)"
    } else {
        ", NULL, NULL"
    };
    format!(
        "SELECT n.nspname, c.relname{filters}\nFROM pg_catalog.pg_class c,\n     pg_catalog.pg_namespace n,\n     \
         pg_catalog.pg_publication_rel pr\nWHERE c.relnamespace = n.oid\n  AND c.oid = pr.prrelid\n  \
         AND pr.prpubid = '{oid}'\nORDER BY 1,2"
    )
}

/// The schemas publication `oid` lists (15+).
fn publication_schemas_sql(oid: &str) -> String {
    format!(
        "SELECT n.nspname\nFROM pg_catalog.pg_namespace n\n     \
         JOIN pg_catalog.pg_publication_namespace pn ON n.oid = pn.pnnspid\nWHERE pn.pnpubid = '{oid}'\nORDER BY 1"
    )
}

/// psql's per-publication table: its settings in one row, its tables and
/// schemas as footers ("Tables:", "Tables from schemas:") — or, with
/// neither, the row count.
fn publication_table(row: &[Option<String>], results: &[ResultSet], sversion: u32) -> PrintTable {
    let mut columns: Vec<(&str, usize)> = vec![
        ("Owner", 2), ("All tables", 3), ("Inserts", 4), ("Updates", 5), ("Deletes", 6), ("Truncates", 7),
    ];
    if sversion >= 180000 { columns.push(("Generated columns", 8)); }
    columns.push(("Via root", 9));
    let mut footers = Vec::new();
    if let Some(tables) = results.first().filter(|set| !set.rows.is_empty()) {
        footers.push("Tables:".to_string());
        for t in &tables.rows {
            let mut line = format!("    \"{}.{}\"", text(t, 0), text(t, 1));
            if let Some(columns) = t.get(3).and_then(Option::as_deref) { line.push_str(&format!(" ({columns})")); }
            if let Some(filter) = t.get(2).and_then(Option::as_deref) { line.push_str(&format!(" WHERE {filter}")); }
            footers.push(line);
        }
    }
    if let Some(schemas) = results.get(1).filter(|set| !set.rows.is_empty()) {
        footers.push("Tables from schemas:".to_string());
        footers.extend(schemas.rows.iter().map(|s| format!("    \"{}\"", text(s, 0))));
    }
    // psql's printer counts the row only when nothing else is under it
    // (`count_rows`).
    PrintTable {
        title:   Some(format!("Publication {}", text(row, 1))),
        headers: columns.iter().map(|(h, _)| h.to_string()).collect(),
        aligns:  vec![Align::Left; columns.len()],
        rows:    vec![columns.iter().map(|&(_, i)| text(row, i).to_string()).collect()],
        footers,
        count_rows: true,
    }
}

/// psql's `describeSubscriptions` (`\dRs`): the current database's
/// subscriptions, `+` adding the settings each server version has.
fn describe_subscriptions(pattern: Option<&str>, verbose: bool, sversion: u32) -> Result<String, String> {
    let mut buf = String::from(
        "SELECT subname AS \"Name\"\n,  pg_catalog.pg_get_userbyid(subowner) AS \"Owner\"\n,  \
         subenabled AS \"Enabled\"\n,  subpublications AS \"Publication\"\n",
    );
    if verbose {
        if sversion >= 140000 {
            buf.push_str(", subbinary AS \"Binary\"\n");
            buf.push_str(if sversion >= 160000 {
                ", (CASE substream\n    WHEN 'f' THEN 'off'\n    WHEN 't' THEN 'on'\n    WHEN 'p' THEN 'parallel'\n   END) AS \"Streaming\"\n"
            } else {
                ", substream AS \"Streaming\"\n"
            });
        }
        if sversion >= 150000 {
            buf.push_str(", subtwophasestate AS \"Two-phase commit\"\n, subdisableonerr AS \"Disable on error\"\n");
        }
        if sversion >= 160000 {
            buf.push_str(", suborigin AS \"Origin\"\n, subpasswordrequired AS \"Password required\"\n, subrunasowner AS \"Run as owner?\"\n");
        }
        if sversion >= 170000 {
            buf.push_str(", subfailover AS \"Failover\"\n");
        }
        buf.push_str(",  subsynccommit AS \"Synchronous commit\"\n,  subconninfo AS \"Conninfo\"\n");
        if sversion >= 150000 {
            buf.push_str(", subskiplsn AS \"Skip LSN\"\n");
        }
    }
    buf.push_str(
        "FROM pg_catalog.pg_subscription\nWHERE subdbid = (SELECT oid\n                 FROM pg_catalog.pg_database\n                 \
         WHERE datname = pg_catalog.current_database())",
    );
    let subscriptions = NameMatch { name_var: Some("subname"), ..NameMatch::default() };
    subscriptions.validate(&mut buf, pattern, true, 1, "").map_err(|e| e.to_string())?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// psql's `listLargeObjects` (`\dl`, `\lo_list`); no pattern.
fn list_large_objects(verbose: bool) -> String {
    let mut buf = String::from("SELECT oid as \"ID\",\n  pg_catalog.pg_get_userbyid(lomowner) as \"Owner\",\n  ");
    if verbose {
        buf.push_str(&acl_column("lomacl"));
        buf.push_str(",\n  ");
    }
    buf.push_str(
        "pg_catalog.obj_description(oid, 'pg_largeobject') as \"Description\"\n\
         FROM pg_catalog.pg_largeobject_metadata\nORDER BY oid",
    );
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plane::DbmsPlane;
    use crate::plane::postgres::{live, PostgresPlane};

    fn ctx(version: &str) -> MetaContext {
        MetaContext { database: "pagila".into(), server_version: version.into(), ..MetaContext::default() }
    }

    fn sql(input: &str, version: &str) -> String {
        match PostgresPlane.meta_cmd_in(input, &ctx(version)) {
            Some(MetaAction::List { sql, .. }) => sql,
            _ => panic!("{input}: unexpected action"),
        }
    }

    fn refusal(input: &str) -> String {
        match PostgresPlane.meta_cmd_in(input, &ctx("")) {
            Some(MetaAction::Fail(lines)) => lines.join("\n"),
            _ => panic!("{input}: unexpected action"),
        }
    }

    #[test]
    fn server_versions_read_as_psql_numbers() {
        assert_eq!(server_version_num("PostgreSQL 18.4 (Postgres.app) on aarch64-apple-darwin23.6.0, compiled by …"), Some(180004));
        assert_eq!(server_version_num("PostgreSQL 16.4 on aarch64-unknown-linux-gnu, compiled by gcc"), Some(160004));
        assert_eq!(server_version_num("PostgreSQL 18beta1 on x86_64-pc-linux-gnu"), Some(180000));
        assert_eq!(server_version_num("PostgreSQL 9.6.24 on x86_64-pc-linux-gnu"), Some(90624));
        assert_eq!(server_version_num("8.0.36"), None);
        assert_eq!(server_version_num(""), None);
    }

    #[test]
    fn lists_carry_psqls_titles() {
        let title = |input: &str| match PostgresPlane.meta_cmd_in(input, &ctx("")) {
            Some(MetaAction::List { title, .. }) => title,
            _ => panic!("{input}: not a list"),
        };
        // listTables: one kind asked for, or all of them.
        for (input, expected) in [("\\d", "List of relations"), ("\\dt", "List of tables"), ("\\dtv", "List of relations"),
                                  ("\\diS+", "List of indexes"), ("\\dvx", "List of views"), ("\\dm", "List of materialized views"),
                                  ("\\ds", "List of sequences"), ("\\dE", "List of foreign tables")] {
            assert_eq!(title(input), expected, "{input}");
        }
        for (input, expected) in [("\\l", "List of databases"), ("\\dn", "List of schemas"), ("\\du", "List of roles"),
                                  ("\\dp", "Access privileges"), ("\\z", "Access privileges"), ("\\ddp", "Default access privileges"),
                                  ("\\dx", "List of installed extensions"), ("\\df", "List of functions"), ("\\dPi", "List of partitioned indexes"),
                                  ("\\dPt", "List of partitioned tables"), ("\\dP", "List of partitioned relations"),
                                  ("\\dconfig", "List of non-default configuration parameters"), ("\\dconfig work*", "List of configuration parameters"),
                                  ("\\drds", "List of settings"), ("\\dd", "Object descriptions"), ("\\dl", "Large objects"),
                                  ("\\dew", "List of foreign-data wrappers"), ("\\dAp", "List of support functions of operator families")] {
            assert_eq!(title(input), expected, "{input}");
        }
    }

    #[test]
    fn command_names_read_as_psql_reads_them() {
        assert_eq!(List::of("dn"), Some(List::Schemas));
        assert_eq!(List::of("dnS+x"), Some(List::Schemas));
        assert_eq!(List::of("du+"), Some(List::Roles));
        assert_eq!(List::of("dg"), Some(List::Roles));
        assert_eq!(List::of("df"), Some(List::Functions("")));
        assert_eq!(List::of("dfanS+"), Some(List::Functions("anS+")));
        assert_eq!(List::of("dfz"), None);
        assert_eq!(List::of("l"), Some(List::Databases));
        assert_eq!(List::of("list+x"), Some(List::Databases));
        assert_eq!(List::of("l++"), None);
        assert_eq!(List::of("dtv"), Some(List::Relations("tv")));
        // \dx is extensions; x after the first letter is expanded output.
        assert_eq!(List::of("dx"), Some(List::Extensions));
        assert_eq!(List::of("dtx"), Some(List::Relations("tx")));
        assert_eq!((List::of("dT+"), List::of("dpS"), List::of("db+"), List::of("dDS")),
                   (Some(List::Types), Some(List::Privileges), Some(List::Tablespaces), Some(List::Domains)));
        assert_eq!((List::of("z"), List::of("zxS"), List::of("zz")), (Some(List::Privileges), Some(List::Privileges), None));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\dxx", &ctx("")), Some(MetaAction::List { vertical: true, .. })));
        // psql counts every list's rows but `\du`'s.
        assert!(matches!(PostgresPlane.meta_cmd_in("\\du+", &ctx("")), Some(MetaAction::List { counts_rows: false, .. })));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\dg", &ctx("")), Some(MetaAction::List { counts_rows: false, .. })));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\dt", &ctx("")), Some(MetaAction::List { counts_rows: true, .. })));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\dn", &ctx("")), Some(MetaAction::List { counts_rows: true, .. })));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\zSx", &ctx("")), Some(MetaAction::List { vertical: true, .. })));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\dnx", &ctx("")), Some(MetaAction::List { vertical: true, .. })));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\lx", &ctx("")), Some(MetaAction::List { vertical: true, .. })));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\dtx", &ctx("")), Some(MetaAction::List { vertical: true, .. })));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\list", &ctx("")), Some(MetaAction::List { vertical: false, .. })));
    }

    #[test]
    fn schemas_are_psql_s_list_schemas() {
        let s = sql("\\dn", "");
        assert!(s.contains("WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'\nORDER BY 1;"), "{s}");
        let s = sql("\\dn+ pub*", "");
        assert!(s.contains("AS \"Access privileges\"") && s.contains("AS \"Description\""), "{s}");
        assert!(s.contains("WHERE n.nspname OPERATOR(pg_catalog.~) '^(pub.*)$' COLLATE pg_catalog.default\n"), "{s}");
        assert!(!sql("\\dnS", "").contains("WHERE"));
        // psql counts the dot but has no database to compare: always refused.
        assert_eq!(refusal("\\dn pagila.public"), "cross-database references are not implemented: pagila.public");
        assert_eq!(refusal("\\dn a.b.c"), "improper qualified name (too many dotted names): a.b.c");
    }

    #[test]
    fn quiet_lists_print_their_empty_table() {
        let quiet = MetaContext {
            variables: crate::parser::Variables::from([("QUIET".to_string(), "on".to_string())]),
            ..ctx("")
        };
        for input in ["\\dt nosuch", "\\d", "\\drds nosuch"] {
            assert!(matches!(PostgresPlane.meta_cmd_in(input, &ctx("")), Some(MetaAction::List { none: Some(_), .. })), "{input}");
            assert!(matches!(PostgresPlane.meta_cmd_in(input, &quiet), Some(MetaAction::List { none: None, .. })), "{input}");
        }
    }

    #[test]
    fn schema_patterns_ask_for_publications_as_psql_does() {
        let footers = |input: &str, version: &str| match PostgresPlane.meta_cmd_in(input, &ctx(version)) {
            Some(MetaAction::List { footers, .. }) => footers,
            other => panic!("{input}: {}", other.is_some()),
        };
        // psql 18's text, word for word (`psql -E`) — the pattern as
        // typed, a `"` kept; and the footers its rows make.
        let query = footers("\\dn \"Pub\"", "PostgreSQL 15.0 on x").expect("PG 15+: a second query");
        assert_eq!(query.sql, "SELECT pubname \n\
                               FROM pg_catalog.pg_publication p\n     \
                               JOIN pg_catalog.pg_publication_namespace pn ON p.oid = pn.pnpubid\n     \
                               JOIN pg_catalog.pg_namespace n ON n.oid = pn.pnnspid \n\
                               WHERE n.nspname = '\"Pub\"'\n\
                               ORDER BY 1");
        let rows = vec![vec![Some("a".to_string())], vec![None]];
        assert_eq!(query.footers(&rows), ["Publications:", "    \"a\"", "    \"\""]);
        assert!(query.footers(&[]).is_empty());
        // No pattern, or a server before 15: the list alone.
        assert_eq!(footers("\\dn", ""), None);
        assert_eq!(footers("\\dn+ public", "PostgreSQL 14.10 on x"), None);
        assert!(footers("\\dn+ public", "").is_some_and(|q| q.sql.contains("WHERE n.nspname = 'public'\n")));
        // Only `\dn`.
        assert_eq!(footers("\\dt public", ""), None);
    }

    #[test]
    fn functions_are_psql_s_describe_functions() {
        let s = sql("\\df", "");
        assert!(s.contains("WHERE pg_catalog.pg_function_is_visible(p.oid)\n      AND n.nspname <> 'pg_catalog'\n"), "{s}");
        assert!(s.ends_with("ORDER BY 1, 2, 4;"), "{s}");
        // Kinds: \dfn excludes the others; \dfat lists aggregates or trigger functions.
        let s = sql("\\dfn", "");
        assert!(s.contains("WHERE p.prokind <> 'a'\n      AND p.prokind <> 'p'\n      AND p.prorettype <> 'pg_catalog.trigger'::pg_catalog.regtype\n      AND p.prokind <> 'w'\n"), "{s}");
        let s = sql("\\dfat", "");
        assert!(s.contains("WHERE (\n       p.prokind = 'a'\n       OR p.prorettype = 'pg_catalog.trigger'::pg_catalog.regtype\n      )\n  AND pg_catalog.pg_function_is_visible(p.oid)\n"), "{s}");
        // Argument-type patterns, psql's type-name aliases, and `-`.
        let s = sql("\\df f int -", "");
        assert!(s.contains("LEFT JOIN pg_catalog.pg_type t0 ON t0.oid = p.proargtypes[0]"), "{s}");
        assert!(s.contains("OR pg_catalog.format_type(t0.oid, NULL) OPERATOR(pg_catalog.~) '^(integer)$'"), "{s}");
        assert!(s.contains("  AND t1.typname IS NULL\n"), "{s}");
        assert!(sql("\\df+", "").contains("as \"Leakproof?\""));
        assert_eq!(refusal("\\dfaz"), "\\df only takes [anptwSx+] as options");
        assert_eq!(refusal("\\df nope.public.f"), "cross-database references are not implemented: nope.public.f");
        assert!(sql("\\df pagila.public.f", "").contains("'^(f)$'"));
    }

    #[test]
    fn roles_are_psql_s_describe_roles() {
        let s = sql("\\du", "");
        assert!(s.contains("AS \"Attributes\"") && s.contains("WHERE r.rolname !~ '^pg_'\nORDER BY 1;"), "{s}");
        assert!(sql("\\dg+ post*", "").contains("AS \"Description\"\nFROM pg_catalog.pg_roles r\nWHERE r.rolname OPERATOR(pg_catalog.~) '^(post.*)$'"));
        assert!(!sql("\\duS", "").contains("WHERE"));
        assert_eq!(refusal("\\du a.b"), "improper qualified name (too many dotted names): a.b");
    }

    #[test]
    fn databases_follow_psql_s_version_branches() {
        let s = sql("\\l", "PostgreSQL 18.4 on x");
        assert!(s.contains("d.datlocprovider WHEN 'b' THEN 'builtin'") && s.contains("  d.datlocale as \"Locale\""), "{s}");
        assert!(s.contains("  d.daticurules as \"ICU Rules\""), "{s}");
        let s = sql("\\l", "PostgreSQL 15.8 on x");
        assert!(s.contains("  d.daticulocale as \"Locale\"") && s.contains("NULL as \"ICU Rules\""), "{s}");
        let s = sql("\\l+ pag*", "PostgreSQL 14.13 on x");
        assert!(s.contains("'libc' AS \"Locale Provider\"") && s.contains("NULL as \"Locale\""), "{s}");
        assert!(s.contains("JOIN pg_catalog.pg_tablespace t") && s.contains("WHERE d.datname OPERATOR(pg_catalog.~) '^(pag.*)$'"), "{s}");
        assert_eq!(refusal("\\l a.b"), "improper qualified name (too many dotted names): a.b");
    }

    #[test]
    fn types_privileges_tablespaces_domains_are_psql_s() {
        let s = sql("\\dT", "");
        assert!(s.contains("  AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_type el WHERE el.oid = t.typelem AND el.typarray = t.oid)\n"), "{s}");
        assert!(s.contains("AND n.nspname <> 'information_schema'\n  AND pg_catalog.pg_type_is_visible(t.oid)\nORDER BY 1, 2;"), "{s}");
        // A pattern naming an array type lets arrays in; psql's aliases apply.
        let s = sql("\\dT int[]", "");
        assert!(!s.contains("NOT EXISTS") && s.contains("OR pg_catalog.format_type(t.oid, NULL) OPERATOR(pg_catalog.~) E'^(integer"), "{s}");
        assert!(sql("\\dT+", "").contains("AS \"Elements\""));
        let s = sql("\\dp", "");
        assert!(s.contains("WHERE c.relkind IN ('r','v','m','S','f','p')\n") && s.contains("AS \"Policies\""), "{s}");
        assert_eq!(sql("\\z", ""), s);
        assert!(!sql("\\dpS", "").contains("<> 'pg_catalog'"));
        let s = sql("\\db+ pg_*", "");
        assert!(s.contains("AS \"Options\"") && s.contains("WHERE spcname OPERATOR(pg_catalog.~) '^(pg_.*)$'"), "{s}");
        let s = sql("\\dD+", "");
        assert!(s.contains("WHERE t.typtype = 'd'\n") && s.contains("LEFT JOIN pg_catalog.pg_description d"), "{s}");
        assert_eq!(refusal("\\db a.b"), "improper qualified name (too many dotted names): a.b");
        assert_eq!(refusal("\\dT a.b.c.d"), "improper qualified name (too many dotted names): a.b.c.d");
    }

    #[test]
    fn aggregates_operators_partitions_stats_config_defaults_and_grants_are_psql_s() {
        assert_eq!((List::of("daS"), List::of("do+"), List::of("dX")),
                   (Some(List::Aggregates), Some(List::Operators), Some(List::ExtendedStats)));
        assert_eq!((List::of("dP"), List::of("dPtn+"), List::of("dPz")),
                   (Some(List::Partitioned("")), Some(List::Partitioned("tn+")), None));
        assert_eq!((List::of("dconfig+"), List::of("dc"), List::of("ddp"), List::of("dd")),
                   (Some(List::Config), Some(List::Conversions), Some(List::DefaultAcls), Some(List::ObjectDescriptions)));
        assert_eq!((List::of("drg"), List::of("drgS"), List::of("drds")), (Some(List::RoleGrants), Some(List::RoleGrants), Some(List::DbRoleSettings)));
        // Sorted as psql sorts, NULLs last.
        assert!(sql("\\doS", "").ends_with("ORDER BY 1, 2, 3, 4;"));
        assert!(sql("\\ddp", "").ends_with("ORDER BY 1, 2, 3;"));
        assert!(sql("\\dPn", "").ends_with("ORDER BY \"Schema\", \"Type\" DESC, \"Parent name\" NULLS FIRST, \"Name\";"));
        // Operator names are literal; one type pattern is a prefix operator's operand.
        let s = sql("\\do + int", "");
        assert!(s.contains("o.oprname OPERATOR(pg_catalog.~) E'^(\\\\+)$'") && s.contains("  AND o.oprleft = 0\n"), "{s}");
        // Version branches.
        assert!(sql("\\dX", "PostgreSQL 13.16 on x").contains("unnest(es.stxkeys)"));
        assert!(sql("\\dX", "PostgreSQL 18.4 on x").contains("pg_get_statisticsobjdef_columns(es.oid)"));
        assert!(sql("\\drg", "PostgreSQL 15.8 on x").contains("CASE WHEN m.rolinherit THEN 'INHERIT' END,\n    'SET'\n"));
        let s = sql("\\dconfig+", "PostgreSQL 14.13 on x");
        assert!(s.contains("NULL AS \"Access privileges\"") && !s.contains("pg_parameter_acl"), "{s}");
        assert!(sql("\\dconfig", "").contains("WHERE s.source <> 'default' AND\n"));
        assert!(sql("\\dconfig WORK*", "").contains("WHERE pg_catalog.lower(s.name) OPERATOR(pg_catalog.~) '^(work.*)$'"));
        assert_eq!(refusal("\\drg a.b"), "improper qualified name (too many dotted names): a.b");
    }

    #[test]
    fn conversions_casts_comments_languages_collations_event_triggers_and_settings_are_psql_s() {
        assert_eq!((List::of("dc"), List::of("dconfig"), List::of("dcS+")),
                   (Some(List::Conversions), Some(List::Config), Some(List::Conversions)));
        assert_eq!((List::of("dC+"), List::of("dd"), List::of("ddS"), List::of("ddp")),
                   (Some(List::Casts), Some(List::ObjectDescriptions), Some(List::ObjectDescriptions), Some(List::DefaultAcls)));
        assert_eq!((List::of("dL"), List::of("dO"), List::of("dy+"), List::of("drds"), List::of("drg"), List::of("dr")),
                   (Some(List::Languages), Some(List::Collations), Some(List::EventTriggers),
                    Some(List::DbRoleSettings), Some(List::RoleGrants), None));
        // \dC matches either side's type.
        let s = sql("\\dC int*", "");
        assert!(s.contains("WHERE ( (true  AND (ts.typname OPERATOR(pg_catalog.~)") && s.contains(") OR (true  AND (tt.typname"), "{s}");
        // \dd: each kind takes the pattern; system filters open or continue a WHERE as psql's do.
        let s = sql("\\dd", "");
        assert_eq!(s.matches("WHERE n.nspname <> 'pg_catalog'").count(), 3, "{s}");
        assert!(s.ends_with("ORDER BY 1, 2, 3;"), "{s}");
        assert_eq!(sql("\\dd foo", "").matches("OPERATOR(pg_catalog.~) '^(foo)$'").count(), 6);
        // \dL hides internal languages only without S or a pattern.
        assert!(sql("\\dL", "").contains("WHERE l.lanplcallfoid != 0\nORDER BY 1;"));
        assert!(!sql("\\dLS", "").contains("lanplcallfoid != 0"));
        assert_eq!(refusal("\\dL a.b"), "cross-database references are not implemented: a.b");
        // \dO by version.
        assert!(sql("\\dO", "PostgreSQL 18.4 on x").contains("  c.colllocale AS \"Locale\""));
        let s = sql("\\dO", "PostgreSQL 14.13 on x");
        assert!(s.contains("  c.collcollate AS \"Locale\",\n  NULL AS \"ICU Rules\""), "{s}");
        // \drds: two patterns, psql's messages, NULL roles and databases sort last.
        match PostgresPlane.meta_cmd_in("\\drds postgres pagila", &ctx("")) {
            Some(MetaAction::List { sql, vertical: false, none: Some(none), .. }) => {
                assert!(sql.contains("WHERE r.rolname OPERATOR(pg_catalog.~) '^(postgres)$' COLLATE pg_catalog.default\n  AND d.datname"), "{sql}");
                assert!(sql.ends_with("ORDER BY 1, 2;"), "{sql}");
                assert_eq!(none, "Did not find any settings for role \"postgres\" and database \"pagila\".");
            }
            _ => panic!("\\drds: unexpected action"),
        }
        assert!(matches!(PostgresPlane.meta_cmd_in("\\drds", &ctx("")), Some(MetaAction::List { none: Some(n), .. }) if n == "Did not find any settings."));
    }

    #[test]
    fn text_search_lists_are_psql_s() {
        assert_eq!((List::of("dF"), List::of("dF+"), List::of("dFx"), List::of("dFS")),
                   (Some(List::TsConfigs), Some(List::TsConfigs), Some(List::TsConfigs), None));
        assert_eq!((List::of("dFp+"), List::of("dFd"), List::of("dFt+"), List::of("dFz")),
                   (Some(List::TsParsers), Some(List::TsDictionaries), Some(List::TsTemplates), None));
        assert!(sql("\\dF", "").contains("WHERE pg_catalog.pg_ts_config_is_visible(c.oid)\nORDER BY 1, 2;"));
        assert!(sql("\\dFd+", "").contains("AS  \"Template\""));
        assert!(sql("\\dF pagila.pg_catalog.english", "").contains("'^(english)$'"));
        assert_eq!(refusal("\\dF nope.pg_catalog.english"), "cross-database references are not implemented: nope.pg_catalog.english");
        // \dFd+ and \dFt+ are plain lists; \dF+ and \dFp+ are staged.
        assert!(matches!(PostgresPlane.meta_cmd_in("\\dFt+", &ctx("")), Some(MetaAction::List { .. })));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\dF+", &ctx("")), Some(MetaAction::Staged(_))));

        let set = |rows: &[&[Option<&str>]]| shared::ResultSet {
            columns: Vec::new(),
            rows: rows.iter().map(|r| r.iter().map(|v| v.map(String::from)).collect()).collect(),
        };
        let reply = |sets| BatchResult { sets, error: None };
        // \dF+: a two-line title, no row count.
        let found = reply(vec![set(&[&[Some("3748"), Some("simple"), Some("pg_catalog"), Some("default"), Some("pg_catalog")]])]);
        let map = reply(vec![set(&[&[Some("word"), Some("simple")]])]);
        match PostgresPlane.meta_continue("\\dF+ simple", &ctx(""), &[found, map]) {
            MetaStep::Done(out) => match &out[..] {
                [ShellEntry::Printed { lines, .. }] => assert_eq!(lines, &[
                    "Text search configuration \"pg_catalog.simple\"",
                    "Parser: \"pg_catalog.default\"",
                    " Token | Dictionaries ",
                    "-------+--------------",
                    " word  | simple",
                    "",
                ]),
                _ => panic!("not one printed block"),
            },
            MetaStep::Run(_) | MetaStep::Call(_) | MetaStep::Failed(_) => panic!("ran on"),
        }
        // \dFp+: two queries per parser, two tables, the second counted.
        let found = reply(vec![set(&[&[Some("3722"), None, Some("default")]])]);
        match PostgresPlane.meta_continue("\\dFp+", &ctx(""), std::slice::from_ref(&found)) {
            MetaStep::Run(q) => {
                assert_eq!(q.len(), 2);
                assert!(q[0].contains("UNION ALL") && q[0].ends_with("WHERE p.oid = '3722';") && q[1].contains("ts_token_type( '3722'"), "{q:?}");
            }
            MetaStep::Done(_) | MetaStep::Call(_) | MetaStep::Failed(_) => panic!("stopped after the lookup"),
        }
        let results = reply(vec![set(&[&[Some("Start parse"), Some("prsd_start"), None]]), set(&[&[Some("word"), Some("Word")]])]);
        match PostgresPlane.meta_continue("\\dFp+", &ctx(""), &[found, results]) {
            MetaStep::Done(out) => match &out[..] {
                [ShellEntry::Printed { lines, .. }] => {
                    assert_eq!(lines.iter().filter(|l| l.starts_with('(')).count(), 1, "{lines:?}");
                    assert!(lines.iter().any(|l| l.trim() == "Text search parser \"default\""), "{lines:?}");
                    assert!(lines.iter().any(|l| l.trim() == "Token types for parser \"default\""), "{lines:?}");
                }
                _ => panic!("not one printed block"),
            },
            MetaStep::Run(_) | MetaStep::Call(_) | MetaStep::Failed(_) => panic!("ran on"),
        }
    }

    #[test]
    fn access_method_lists_are_psql_s() {
        assert_eq!((List::of("dA"), List::of("dA+"), List::of("dAx"), List::of("dAS")),
                   (Some(List::AccessMethods), Some(List::AccessMethods), Some(List::AccessMethods), None));
        assert_eq!((List::of("dAc"), List::of("dAf+"), List::of("dAo"), List::of("dAp"), List::of("dAz")),
                   (Some(List::OpClasses), Some(List::OpFamilies), Some(List::OpFamilyOperators), Some(List::OpFamilyFunctions), None));
        // \dAc: the access method opens the WHERE, the type continues it.
        let s = sql("\\dAc btree int4", "");
        assert!(s.contains("WHERE am.amname OPERATOR(pg_catalog.~) '^(btree)$'") && s.contains("  AND (t.typname OPERATOR"), "{s}");
        // \dAf: the type through any of the family's classes.
        let s = sql("\\dAf * int4", "");
        assert!(s.contains("  WHERE EXISTS (\n") && s.ends_with("  )\nORDER BY 1, 2;"), "{s}");
        // \dAo / \dAp: the family pattern, by its schema.
        assert!(sql("\\dAo btree integer_ops", "").contains("  AND of.opfname OPERATOR(pg_catalog.~) '^(integer_ops)$'"));
        assert!(sql("\\dAp+", "").contains("ap.amproc::pg_catalog.regprocedure AS \"Function\""));
        assert_eq!(refusal("\\dA a.b"), "improper qualified name (too many dotted names): a.b");
    }

    #[test]
    fn foreign_data_lists_are_psql_s() {
        assert_eq!((List::of("des+"), List::of("deu"), List::of("dew"), List::of("det"), List::of("de"), List::of("dE")),
                   (Some(List::ForeignServers), Some(List::UserMappings), Some(List::ForeignDataWrappers),
                    Some(List::ForeignTables), None, Some(List::Relations("E"))));
        assert!(sql("\\deu+ x", "").contains("WHERE (um.srvname OPERATOR(pg_catalog.~) '^(x)$' COLLATE pg_catalog.default\n        OR um.usename"));
        assert!(sql("\\des+", "").contains("pg_catalog.pg_options_to_table(srvoptions)"));
        assert!(sql("\\det", "").contains("WHERE pg_catalog.pg_table_is_visible(c.oid)\nORDER BY 1, 2;"));
        assert_eq!(refusal("\\det nope.public.x"), "cross-database references are not implemented: nope.public.x");
    }

    #[test]
    fn replication_and_large_object_lists_are_psql_s() {
        assert_eq!((List::of("dRp"), List::of("dRs+"), List::of("dR"), List::of("dRz")),
                   (Some(List::Publications), Some(List::Subscriptions), None, None));
        assert_eq!((List::of("dl+"), List::of("lo_list"), List::of("lo_listx"), List::of("lol"), List::of("lo_unlink")),
                   (Some(List::LargeObjects), Some(List::LargeObjects), Some(List::LargeObjects), Some(List::LargeObjects), None));
        assert!(matches!(PostgresPlane.meta_cmd_in("\\lo_listx", &ctx("")), Some(MetaAction::List { vertical: true, .. })));
        assert!(sql("\\dRp", "PostgreSQL 18.4 on x").contains("AS \"Generated columns\""));
        assert!(!sql("\\dRp", "PostgreSQL 17.6 on x").contains("Generated columns"));
        let s = sql("\\dRs+", "PostgreSQL 15.8 on x");
        assert!(s.contains("substream AS \"Streaming\"") && s.contains("subskiplsn") && !s.contains("subfailover"), "{s}");
        assert!(sql("\\dRs", "").ends_with("WHERE datname = pg_catalog.current_database())ORDER BY 1;"));
        assert!(sql("\\dl+", "").contains("AS \"Access privileges\""));

        // \dRp+: a publication of all tables needs no second batch.
        let set = |rows: &[&[&str]]| shared::ResultSet {
            columns: Vec::new(),
            rows: rows.iter().map(|r| r.iter().map(|v| Some(v.to_string())).collect()).collect(),
        };
        let found = BatchResult {
            sets: vec![set(&[&["16400", "everything", "postgres", "t", "t", "t", "t", "t", "none", "f"]])],
            error: None,
        };
        match PostgresPlane.meta_continue("\\dRp+", &ctx("PostgreSQL 17.6 on x"), &[found]) {
            MetaStep::Done(out) => match &out[..] {
                [ShellEntry::Printed { lines, .. }] => {
                    assert_eq!(lines[0].trim(), "Publication everything");
                    assert!(lines[1].contains("Via root") && !lines[1].contains("Generated columns"), "{lines:?}");
                    assert_eq!(&lines[lines.len() - 2..], ["(1 row)", ""]);
                }
                _ => panic!("not one printed block"),
            },
            MetaStep::Run(_) | MetaStep::Call(_) | MetaStep::Failed(_) => panic!("asked for an empty batch"),
        }
    }

    #[test]
    fn extension_contents_are_staged_and_printed_like_psql() {
        use shared::ResultSet;
        let rows = |rows: &[&[&str]]| ResultSet {
            columns: Vec::new(),
            rows: rows.iter().map(|r| r.iter().map(|v| Some(v.to_string())).collect()).collect(),
        };
        let reply = |sets: Vec<ResultSet>| BatchResult { sets, error: None };
        let step = |cmd: &str, replies: &[BatchResult]| PostgresPlane.meta_continue(cmd, &ctx(""), replies);
        match PostgresPlane.meta_cmd_in("\\dx+ plpgsql", &ctx("")) {
            Some(MetaAction::Staged(q)) => assert!(q[0].contains("FROM pg_catalog.pg_extension e\nWHERE e.extname OPERATOR(pg_catalog.~) '^(plpgsql)$'"), "{q:?}"),
            _ => panic!("\\dx+ isn't staged"),
        }
        // Nothing found: psql's message — and the command failed, as
        // psql's `listExtensionContents` returns false.
        match step("\\dx+ nosuch", &[reply(vec![rows(&[])])]) {
            MetaStep::Failed(out) => assert!(matches!(&out[..], [ShellEntry::MetaOutput(l)] if l == &["Did not find any extension named \"nosuch\"."])),
            _ => panic!("didn't fail with nothing found"),
        }
        // Found: each one's objects, then a titled table each.
        let found = reply(vec![rows(&[&["plpgsql", "13579"]])]);
        match step("\\dx+", std::slice::from_ref(&found)) {
            MetaStep::Run(q) => assert!(q[0].contains("refobjid = '13579'"), "{q:?}"),
            MetaStep::Done(_) | MetaStep::Call(_) | MetaStep::Failed(_) => panic!("stopped after the lookup"),
        }
        let contents = reply(vec![rows(&[&["function plpgsql_call_handler()"], &["language plpgsql"]])]);
        match step("\\dx+", &[found, contents]) {
            MetaStep::Done(out) => match &out[..] {
                [ShellEntry::Printed { lines, .. }] => assert_eq!(lines, &[
                    " Objects in extension \"plpgsql\"",
                    "       Object description        ",
                    "---------------------------------",
                    " function plpgsql_call_handler()",
                    " language plpgsql",
                    "(2 rows)",
                    "",
                ]),
                _ => panic!("not one printed block"),
            },
            MetaStep::Run(_) | MetaStep::Call(_) | MetaStep::Failed(_) => panic!("ran a third step"),
        }
    }

    /// The list commands the live tests run through psql and the plane.
    const LIST_COMMANDS: &[&str] = &[
        "\\dn", "\\dn+", "\\dnS", "\\dn pub*", "\\dn PUBLIC", "\\dn \"public\"", "\\dn pagila.public", "\\dn a.b.c",
        "\\df", "\\df+", "\\dfS film*", "\\df film_in_stock int int", "\\df film_in_stock - -", "\\df+ film_in_stock",
        "\\dfa", "\\dfn", "\\dfp", "\\dft", "\\dfw", "\\dfaS sum", "\\dfanS max", "\\dfaz", "\\df public.*",
        "\\df pagila.public.film_in_stock", "\\df nope.public.f", "\\dfS+ pg_catalog.lower text",
        "\\du", "\\du+", "\\duS", "\\du postgres", "\\du pg_*", "\\dg", "\\du a.b",
        "\\l", "\\l+", "\\l pagila", "\\l a.b", "\\l+ pag*", "\\list", "\\lx",
        "\\dt", "\\dtx film*", "\\dt nosuch",
        "\\dx", "\\dx plpgsql", "\\dxx", "\\dx a.b",
        "\\dT", "\\dT+", "\\dTS int*", "\\dTS int[]", "\\dT mpaa_rating", "\\dT+ mpaa_rating", "\\dT a.b.c.d",
        "\\dp", "\\dp film*", "\\z", "\\zS pg_class", "\\dpS pg_catalog.pg_class", "\\z nope.public.x",
        "\\db", "\\db+", "\\db pg_*", "\\db a.b",
        "\\dD", "\\dD+", "\\dDS", "\\dD year", "\\dDS+ information_schema.*",
        "\\da", "\\daS", "\\daS max", "\\daS pg_catalog.array_*", "\\da a.b.c.d",
        "\\do", "\\doS", "\\doS +", "\\doS+ ||", "\\doS - int", "\\doS = int int", "\\doS * - int", "\\doS ~ text text",
        "\\dP", "\\dP+", "\\dPt", "\\dPi", "\\dPn", "\\dPn+", "\\dP pay*", "\\dPi+ pay*",
        "\\dX", "\\dX a.b.c.d",
        "\\dconfig", "\\dconfig+", "\\dconfig *work_mem*", "\\dconfig+ log_*", "\\dconfig WORK_MEM", "\\dconfig a.b",
        "\\ddp", "\\ddp postgres", "\\ddp a.b.c", "\\do a.b.c.d", "\\doS - -",
        "\\drg", "\\drgS", "\\drg pg_*", "\\drg a.b", "\\drg a.b.c",
        "\\dc", "\\dcS", "\\dcS+ utf8_to*", "\\dc a.b.c.d",
        "\\dC", "\\dC+", "\\dC int*", "\\dC+ text", "\\dC pg_catalog.bool",
        "\\dd", "\\ddS", "\\ddS pg_catalog.*", "\\ddS *pkey",
        "\\dL", "\\dL+", "\\dLS", "\\dLS+", "\\dL plpgsql", "\\dL a.b",
        "\\dO", "\\dO+", "\\dOS", "\\dOS+ en*", "\\dO \"C\"", "\\dOS \"C\"",
        "\\dy", "\\dy+", "\\dy a.b",
        "\\drds", "\\drds postgres", "\\drds * pagila", "\\drds a.b",
        "\\dF", "\\dF eng*", "\\dF pg_catalog.*", "\\dF pagila.pg_catalog.english", "\\dF nope.pg_catalog.english",
        "\\dFx", "\\dF a.b.c.d", "\\dFp", "\\dFd", "\\dFd+", "\\dFd+ english*", "\\dFt", "\\dFt+",
        "\\dA", "\\dA+", "\\dA b*", "\\dA a.b",
        "\\dAc", "\\dAc+", "\\dAc btree", "\\dAc btree int4", "\\dAc * text", "\\dAc+ gist *",
        "\\dAf", "\\dAf+ hash", "\\dAf btree int4", "\\dAf * text",
        "\\dAo btree", "\\dAo+ btree integer_ops", "\\dAo * pg_catalog.text_ops", "\\dAo gist",
        "\\dAp btree", "\\dAp+ btree integer_ops", "\\dAp hash", "\\dAp nope.public.x",
        "\\des", "\\des+", "\\deu", "\\deu+", "\\dew", "\\dew+", "\\det", "\\det+", "\\des a.b",
        "\\dRp", "\\dRp a.b", "\\dRs", "\\dRs+", "\\dl", "\\dl+", "\\lo_list", "\\lo_list+", "\\lo_listx",
    ];

    // ── Live parity with psql ───────────────────────────────────────────
    //
    // Opt-in: needs psql on PATH and the local Pagila database
    // (PGPASSWORD from ~/.parliament/secrets.env).
    //   cargo test -p tyto -- --ignored lists_match_psql

    /// Each command through psql, and our SQL for it through psql, both
    /// as CSV: the same columns and rows, or the same refusal. Staged
    /// lists (`\dx+`) compare as printed text.
    #[test]
    #[ignore]
    fn lists_match_psql() {
        let text = |out: std::process::Output| {
            String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
        };
        let version = text(live::psql(&["-A", "-t", "-c", "SELECT version()"])).trim().to_string();
        let ctx = ctx(&version);
        let commands = LIST_COMMANDS;
        let mut failures = Vec::new();
        for cmd in commands {
            // An expanded result is psql's -x. SQL NULL is marked, apart
            // from '', on both sides: NULLs where psql's are.
            let null = format!("null={}", live::NULL);
            let run = |sql: &str, expanded: bool| {
                text(live::psql(&["--csv", "-P", &null, if expanded { "-x" } else { "-q" }, "-c", sql]))
            };
            let ours = match PostgresPlane.meta_cmd_in(cmd, &ctx) {
                Some(MetaAction::RunSql(sql, expanded)) => run(&sql, expanded),
                Some(MetaAction::List { sql, vertical: expanded, none, .. }) => {
                    let out = run(&sql, expanded);
                    match none {
                        Some(none) if out.lines().count() <= 1 => none + "\n",
                        _ => out,
                    }
                }
                // psql's refusals, said as psql says them.
                Some(MetaAction::Output(lines) | MetaAction::Fail(lines)) => lines.join("\n") + "\n",
                _ => panic!("{cmd}: unexpected action"),
            };
            let theirs = text(live::psql(&["--csv", "-P", &null, "-c", cmd]));
            if ours != theirs {
                failures.push(format!("{cmd}\n  ours: {ours:?}\n  psql: {theirs:?}"));
            }
        }
        for cmd in ["\\dx+", "\\dx+ plpgsql", "\\dx+ nosuch", "\\dx+ a.b", "\\dF+", "\\dF+ english", "\\dF+ nosuch",
                    "\\dF+ a.b.c.d", "\\dFp+", "\\dFp+ default", "\\dFp+ nosuch", "\\dRp+", "\\dRp+ nosuch"] {
            let ours = live::staged_output(&ctx, cmd, &mut live::query);
            let theirs = text(live::psql(&["-P", "pager=off", "-c", cmd]));
            if ours != theirs {
                failures.push(format!("{cmd}\n  ours: {ours:?}\n  psql: {theirs:?}"));
            }
        }
        assert!(failures.is_empty(), "{} differ:\n{}", failures.len(), failures.join("\n"));
    }

    /// psql's output for `cmd`, then ours (our SQL through psql), both as
    /// CSV from one session in which `setup` ran inside a transaction
    /// that is rolled back afterwards.
    fn in_rolled_back_fixture(setup: &[&str], cmd: &str) -> (String, String) {
        const MARK: &str = "--8<--";
        let ours = match PostgresPlane.meta_cmd_in(cmd, &ctx("")) {
            Some(MetaAction::RunSql(sql, false) | MetaAction::List { sql, vertical: false, .. }) => sql,
            _ => panic!("{cmd}: unexpected action"),
        };
        let echo = format!("\\echo {MARK}");
        // SQL NULL is marked, apart from '', on both sides: NULLs where
        // psql's are.
        let null_on = format!("\\pset null '{}'", live::NULL);
        let mut args = vec!["--csv", "-q"];
        for step in std::iter::once("BEGIN").chain(setup.iter().copied())
            .chain([null_on.as_str(), cmd, echo.as_str(), ours.as_str(), "ROLLBACK"])
        {
            args.extend(["-c", step]);
        }
        let out = live::psql(&args);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (theirs, ours) = text.split_once(&format!("{MARK}\n")).expect("marker");
        (theirs.to_string(), ours.to_string())
    }

    /// psql's `ECHO_HIDDEN`: what a list command sends is the query psql
    /// sends — `psql -E` prints it between its lines — word for word, and
    /// the query psql sends after it for its footers (`\dn <pattern>`'s
    /// publications). But `\du`, whose Attributes psql builds client-side
    /// from another query (ours builds them in its SQL).
    #[test]
    #[ignore]
    fn echo_hidden_shows_psqls_queries() {
        let text = |out: std::process::Output| String::from_utf8_lossy(&out.stdout).into_owned();
        let version = text(live::psql(&["-A", "-t", "-c", "SELECT version()"])).trim().to_string();
        let ctx = ctx(&version);
        let mut failures = Vec::new();
        let differs = |cmd: &str| cmd.starts_with("\\du") || cmd.starts_with("\\dg");
        for cmd in LIST_COMMANDS.iter().filter(|cmd| !differs(cmd)) {
            let ours = match PostgresPlane.meta_cmd_in(cmd, &ctx) {
                Some(MetaAction::RunSql(sql, _)) => vec![sql],
                Some(MetaAction::List { sql, footers, .. }) => std::iter::once(sql).chain(footers.map(|f| f.sql)).collect(),
                _ => Vec::new(),
            };
            let printed = text(live::psql(&["-E", "-P", "pager=off", "-c", cmd]));
            let theirs: Vec<String> = printed.split("/******** QUERY *********/\n").skip(1)
                .filter_map(|block| block.split_once("\n/************************/\n").map(|(query, _)| query.to_string()))
                .collect();
            if ours != theirs {
                failures.push(format!("{cmd}\n  ours: {ours:?}\n  psql: {theirs:?}"));
            }
        }
        assert!(failures.is_empty(), "{} of {} differ:\n{}", failures.len(), LIST_COMMANDS.len(), failures.join("\n"));
    }

    /// `\du` / `\du+` over roles with every attribute line psql writes —
    /// connection limits of 0, 1 and more, a password expiry, no
    /// inheritance, a comment.
    #[test]
    #[ignore]
    fn role_attributes_match_psql() {
        let setup = [
            "CREATE ROLE parl_probe_a NOLOGIN NOINHERIT CREATEDB CONNECTION LIMIT 1 VALID UNTIL '2030-01-01 00:00:00+00'",
            "CREATE ROLE parl_probe_b LOGIN CONNECTION LIMIT 0",
            "CREATE ROLE parl_probe_c LOGIN CREATEROLE CONNECTION LIMIT 5",
            "CREATE ROLE parl_probe_d LOGIN",
            "COMMENT ON ROLE parl_probe_a IS 'probe role'",
        ];
        for cmd in ["\\du parl_probe_*", "\\du+ parl_probe_*"] {
            let (theirs, ours) = in_rolled_back_fixture(&setup, cmd);
            assert!(theirs.contains("1 connection\nPassword valid until "), "{theirs}");
            assert_eq!(ours, theirs, "{cmd}");
        }
    }

    /// `\dp` with column privileges and row-security policies (permissive
    /// and restrictive, per command, with roles and checks), `\dD` with a
    /// collation, NOT NULL, a default, two checks and a comment, `\dT+`
    /// over an enum and a standalone composite — none of which Pagila has.
    #[test]
    #[ignore]
    fn privileges_domains_and_types_match_psql() {
        let setup = [
            "CREATE TABLE parl_probe_t (id int, secret text)",
            "GRANT SELECT ON parl_probe_t TO PUBLIC",
            "GRANT SELECT (id) ON parl_probe_t TO PUBLIC",
            "GRANT UPDATE (secret) ON parl_probe_t TO postgres",
            "ALTER TABLE parl_probe_t ENABLE ROW LEVEL SECURITY",
            "CREATE POLICY p_all ON parl_probe_t USING (id > 0)",
            "CREATE POLICY p_restrict ON parl_probe_t AS RESTRICTIVE FOR UPDATE TO postgres, PUBLIC USING (true) WITH CHECK (id < 100)",
            "CREATE DOMAIN parl_probe_d AS text COLLATE \"C\" NOT NULL DEFAULT 'x' CHECK (VALUE <> '') CHECK (length(VALUE) < 10)",
            "COMMENT ON DOMAIN parl_probe_d IS 'probe domain'",
            "CREATE TYPE parl_probe_e AS ENUM ('a', 'b')",
            "CREATE TYPE parl_probe_c AS (x int)",
        ];
        for cmd in ["\\dp parl_probe_*", "\\z parl_probe_t", "\\dD parl_probe_*", "\\dD+ parl_probe_*",
                    "\\dT parl_probe_*", "\\dT+ parl_probe_*"] {
            let (theirs, ours) = in_rolled_back_fixture(&setup, cmd);
            assert!(theirs.lines().count() > 1, "{cmd}: the fixture didn't build: {theirs}");
            assert_eq!(ours, theirs, "{cmd}");
        }
    }

    /// `\dP` over nested partitions (parent order, sizes), `\dX` with
    /// column and expression statistics, `\do` with a prefix and an infix
    /// operator, `\ddp` with schema and global defaults (global sort
    /// last), `\drg` with a membership's options — none in Pagila.
    #[test]
    #[ignore]
    fn partitions_operators_stats_defaults_and_grants_match_psql() {
        let setup = [
            "CREATE TABLE parl_probe_p (id int, k int) PARTITION BY RANGE (id)",
            "CREATE TABLE parl_probe_p1 PARTITION OF parl_probe_p FOR VALUES FROM (0) TO (10) PARTITION BY LIST (k)",
            "CREATE TABLE parl_probe_p1a PARTITION OF parl_probe_p1 FOR VALUES IN (1)",
            "CREATE TABLE parl_probe_p2 PARTITION OF parl_probe_p FOR VALUES FROM (10) TO (20)",
            "CREATE INDEX parl_probe_p_idx ON parl_probe_p (k)",
            "COMMENT ON TABLE parl_probe_p IS 'probe partitioned'",
            "CREATE STATISTICS parl_probe_s (ndistinct, mcv) ON id, k FROM parl_probe_p2",
            "CREATE STATISTICS parl_probe_sx ON (id + k) FROM parl_probe_p2",
            "CREATE FUNCTION parl_probe_neg(int) RETURNS int LANGUAGE sql IMMUTABLE AS 'SELECT -$1'",
            "CREATE FUNCTION parl_probe_add(int, int) RETURNS int LANGUAGE sql IMMUTABLE AS 'SELECT $1 + $2'",
            "CREATE OPERATOR ~~~ (RIGHTARG = int, FUNCTION = parl_probe_neg)",
            "CREATE OPERATOR #@# (LEFTARG = int, RIGHTARG = int, FUNCTION = parl_probe_add)",
            "COMMENT ON OPERATOR #@# (int, int) IS 'probe operator'",
            "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO PUBLIC",
            "ALTER DEFAULT PRIVILEGES GRANT EXECUTE ON FUNCTIONS TO postgres",
            "CREATE ROLE parl_probe_r1",
            "CREATE ROLE parl_probe_r2",
            "GRANT parl_probe_r1 TO parl_probe_r2 WITH ADMIN OPTION, INHERIT FALSE",
        ];
        for cmd in ["\\dP parl_probe_*", "\\dPn", "\\dPn+ parl_probe_*", "\\dPi parl_probe_*", "\\dPtn+",
                    "\\dX parl_probe_*", "\\do ~~~", "\\do+ #@#", "\\do #@# int int", "\\do ~~~ int", "\\do ~~~ - int",
                    "\\ddp", "\\drg parl_probe_*"] {
            let (theirs, ours) = in_rolled_back_fixture(&setup, cmd);
            assert!(theirs.lines().count() > 1, "{cmd}: the fixture didn't build: {theirs}");
            assert_eq!(ours, theirs, "{cmd}");
        }
    }

    /// `\dd` over a commented table constraint, domain constraint, rule
    /// and trigger; `\dO` with a nondeterministic ICU collation; `\dC`
    /// with an assignment cast; `\dy` with an always-enabled, tagged
    /// event trigger; `\drds` with a role's settings everywhere and in
    /// one database — none in Pagila.
    #[test]
    #[ignore]
    fn comments_collations_casts_event_triggers_and_settings_match_psql() {
        let setup = [
            "CREATE TABLE parl_probe_t (id int CONSTRAINT parl_probe_ck CHECK (id > 0))",
            "COMMENT ON CONSTRAINT parl_probe_ck ON parl_probe_t IS 'probe check'",
            "CREATE DOMAIN parl_probe_d AS int CONSTRAINT parl_probe_dck CHECK (VALUE > 0)",
            "COMMENT ON CONSTRAINT parl_probe_dck ON DOMAIN parl_probe_d IS 'probe domain check'",
            "CREATE RULE parl_probe_rule AS ON DELETE TO parl_probe_t DO INSTEAD NOTHING",
            "COMMENT ON RULE parl_probe_rule ON parl_probe_t IS 'probe rule'",
            "CREATE FUNCTION parl_probe_tf() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$",
            "CREATE TRIGGER parl_probe_tg BEFORE INSERT ON parl_probe_t FOR EACH ROW EXECUTE FUNCTION parl_probe_tf()",
            "COMMENT ON TRIGGER parl_probe_tg ON parl_probe_t IS 'probe trigger'",
            "CREATE COLLATION parl_probe_coll (provider = icu, locale = 'und-u-ks-level2', deterministic = false)",
            "COMMENT ON COLLATION parl_probe_coll IS 'probe collation'",
            "CREATE TYPE parl_probe_pt AS (a int)",
            "CREATE FUNCTION parl_probe_cast(parl_probe_pt) RETURNS int LANGUAGE sql AS $$SELECT ($1).a$$",
            "CREATE CAST (parl_probe_pt AS int) WITH FUNCTION parl_probe_cast(parl_probe_pt) AS ASSIGNMENT",
            "COMMENT ON CAST (parl_probe_pt AS int) IS 'probe cast'",
            "CREATE ROLE parl_probe_r",
            "ALTER ROLE parl_probe_r SET work_mem = '8MB'",
            "ALTER ROLE parl_probe_r SET statement_timeout = '5s'",
            "ALTER ROLE parl_probe_r IN DATABASE pagila SET search_path = public, pg_catalog",
            "CREATE FUNCTION parl_probe_et() RETURNS event_trigger LANGUAGE plpgsql AS $$BEGIN END$$",
            "CREATE EVENT TRIGGER parl_probe_ev ON ddl_command_start WHEN TAG IN ('CREATE TABLE', 'DROP TABLE') \
             EXECUTE FUNCTION parl_probe_et()",
            "ALTER EVENT TRIGGER parl_probe_ev ENABLE ALWAYS",
            "COMMENT ON EVENT TRIGGER parl_probe_ev IS 'probe event trigger'",
        ];
        for cmd in ["\\dd parl_probe_*", "\\dd", "\\dO parl_probe_*", "\\dO+ parl_probe_*", "\\dC parl_probe_*",
                    "\\dC+ parl_probe_*", "\\dy", "\\dy+ parl_probe_*", "\\drds parl_probe_*", "\\drds parl_probe_r pagila"] {
            let (theirs, ours) = in_rolled_back_fixture(&setup, cmd);
            assert!(theirs.lines().count() > 1, "{cmd}: the fixture didn't build: {theirs}");
            assert_eq!(ours, theirs, "{cmd}");
        }
    }

    /// The foreign data lists over a wrapper (no handler, with options), a
    /// server with type, version and options, user mappings for a role and
    /// for PUBLIC, and a foreign table with options — none in Pagila.
    #[test]
    #[ignore]
    fn foreign_data_matches_psql() {
        let setup = [
            "CREATE FOREIGN DATA WRAPPER parl_probe_fdw OPTIONS (debug 'true')",
            "COMMENT ON FOREIGN DATA WRAPPER parl_probe_fdw IS 'probe wrapper'",
            "CREATE SERVER parl_probe_srv TYPE 'probe' VERSION '1.0' FOREIGN DATA WRAPPER parl_probe_fdw \
             OPTIONS (host 'nowhere', port '1')",
            "COMMENT ON SERVER parl_probe_srv IS 'probe server'",
            "CREATE USER MAPPING FOR postgres SERVER parl_probe_srv OPTIONS (user 'u')",
            "CREATE USER MAPPING FOR PUBLIC SERVER parl_probe_srv",
            "CREATE FOREIGN TABLE parl_probe_ft (a int) SERVER parl_probe_srv OPTIONS (table_name 'o''brien')",
            "COMMENT ON FOREIGN TABLE parl_probe_ft IS 'probe foreign table'",
        ];
        for cmd in ["\\dew parl_probe_*", "\\dew+ parl_probe_*", "\\des parl_probe_*", "\\des+ parl_probe_*",
                    "\\deu parl_probe_*", "\\deu+ parl_probe_*", "\\deu postgres", "\\det parl_probe_*", "\\det+ parl_probe_*"] {
            let (theirs, ours) = in_rolled_back_fixture(&setup, cmd);
            assert!(theirs.lines().count() > 1, "{cmd}: the fixture didn't build: {theirs}");
            assert_eq!(ours, theirs, "{cmd}");
        }
    }

    /// `\dRp` / `\dRp+` over publications of all tables, of a table with a
    /// column list and row filter, and of a schema; `\dRs` with an
    /// unconnected subscription; `\dl` with a commented, granted large
    /// object — none in Pagila. `\dRp+` is staged, so it's compared inside
    /// one session holding the fixture.
    #[test]
    #[ignore]
    // Its query callback's error is a whole DbError, as live::staged_output takes it.
    #[allow(clippy::result_large_err)]
    fn publications_subscriptions_and_large_objects_match_psql() {
        let setup = [
            "CREATE TABLE parl_probe_pt (id int, note text, secret text)",
            "CREATE PUBLICATION parl_probe_all FOR ALL TABLES",
            "CREATE PUBLICATION parl_probe_cols FOR TABLE parl_probe_pt (id, note) WHERE (id > 0) \
             WITH (publish = 'insert, update')",
            "CREATE PUBLICATION parl_probe_schema FOR TABLES IN SCHEMA public",
            "CREATE SUBSCRIPTION parl_probe_sub CONNECTION 'dbname=nowhere' PUBLICATION parl_probe_all WITH (connect = false)",
            "DO $$BEGIN PERFORM lo_create(424242); END$$",
            "COMMENT ON LARGE OBJECT 424242 IS 'probe large object'",
            "GRANT SELECT ON LARGE OBJECT 424242 TO PUBLIC",
        ];
        for cmd in ["\\dRp parl_probe_*", "\\dRs parl_probe_*", "\\dRs+ parl_probe_*", "\\dl", "\\dl+", "\\lo_list+"] {
            let (theirs, ours) = in_rolled_back_fixture(&setup, cmd);
            assert!(theirs.lines().count() > 1, "{cmd}: the fixture didn't build: {theirs}");
            assert_eq!(ours, theirs, "{cmd}");
        }
        let version = String::from_utf8_lossy(&live::psql(&["-A", "-t", "-c", "SELECT version()"]).stdout).trim().to_string();
        let ctx = ctx(&version);
        let mut psql = live::Session::open("pagila");
        psql.step("BEGIN;");
        for step in setup {
            psql.step(&format!("{step};"));
        }
        let psql = std::cell::RefCell::new(psql);
        for cmd in ["\\dRp+ parl_probe_*", "\\dRp+ parl_probe_cols"] {
            let theirs = psql.borrow_mut().printed(cmd);
            assert!(theirs.contains("Publication parl_probe_cols"), "{cmd}: the fixture didn't build: {theirs}");
            let ours = live::staged_output(&ctx, cmd, &mut |sql| Ok(psql.borrow_mut().rows(sql)));
            assert_eq!(ours, theirs, "{cmd}");
        }
        psql.borrow_mut().step("ROLLBACK;");
    }
}
