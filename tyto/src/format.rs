use std::collections::HashMap;

use crate::types::{ExplorerColumn, IndexInfo};

/// Insert thousands-commas into a numeric string.
pub(crate) fn format_int_with_commas(s: &str) -> String {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) { out.push(','); }
        out.push(*b as char);
    }
    out
}

/// Thin `i64`-typed sibling for callers that already parsed the value.
/// Negatives pass through with a leading `-` and thousands commas on
/// the magnitude. Zero renders as `"0"`. Used by the tooltip builders
/// where the input is guaranteed numeric already.
pub(crate) fn format_int_with_commas_i64(n: i64) -> String {
    if n < 0 {
        // -1234567 → "-1,234,567". `abs()` would overflow on i64::MIN;
        // work in unsigned magnitude instead so the edge case is
        // representable.
        let mag = (n as i128).unsigned_abs().to_string();
        return format!("-{}", format_int_with_commas(&mag));
    }
    format_int_with_commas(&n.to_string())
}

/// Human-readable magnitude compression for signals where the DBA
/// wants "rough temperature", not an exact value. Uses SI-ish
/// suffixes:
///
///   999           → "999"
///   1_234         → "1.2K"
///   105_000_000   → "105M"
///   1_500_000_000 → "1.5G"
///   4_500_000_000_000 → "4.5T"
///
/// Values under 1,000 pass through as-is (readable at that scale).
/// Values ≥1,000 keep one decimal until the magnitude reaches 100 in
/// the unit — `1.5K`, `12K`, `105K`, `1.5M`, … — so the display width
/// stays roughly constant. Negatives sink into the same formatter
/// with a leading minus.
pub(crate) fn format_int_condensed(n: i64) -> String {
    if n < 0 {
        let mag = (n as i128).unsigned_abs();
        return format!("-{}", format_u128_condensed(mag));
    }
    format_u128_condensed(n as u128)
}

/// Magnitude-only condensation shared by the i64 wrapper. Extracted
/// so `i64::MIN.unsigned_abs()` (a u64 value) can flow through the
/// same logic without going back through a signed roundtrip.
fn format_u128_condensed(n: u128) -> String {
    const K: u128 = 1_000;
    const M: u128 = 1_000_000;
    const G: u128 = 1_000_000_000;
    const T: u128 = 1_000_000_000_000;
    const P: u128 = 1_000_000_000_000_000;
    if n < K {
        return n.to_string();
    }
    let (unit_value, suffix) = if n < M {
        (K, 'K')
    } else if n < G {
        (M, 'M')
    } else if n < T {
        (G, 'G')
    } else if n < P {
        (T, 'T')
    } else {
        (P, 'P')
    };
    let scaled = n as f64 / unit_value as f64;
    // One decimal below 100 in the unit (1.5K, 12.3K), no decimal at
    // or above 100 (105K, 999M) so the display width stays roughly
    // 3-4 chars regardless of magnitude.
    if scaled < 100.0 {
        format!("{:.1}{}", scaled, suffix)
    } else {
        format!("{:.0}{}", scaled, suffix)
    }
}

#[cfg(test)]
mod format_int_tests {
    use super::*;

    #[test]
    fn commas_i64_positive() {
        assert_eq!(format_int_with_commas_i64(0), "0");
        assert_eq!(format_int_with_commas_i64(1), "1");
        assert_eq!(format_int_with_commas_i64(999), "999");
        assert_eq!(format_int_with_commas_i64(1_000), "1,000");
        assert_eq!(format_int_with_commas_i64(1_234_567), "1,234,567");
        assert_eq!(format_int_with_commas_i64(1_000_000_000), "1,000,000,000");
    }

    #[test]
    fn commas_i64_negative() {
        assert_eq!(format_int_with_commas_i64(-1), "-1");
        assert_eq!(format_int_with_commas_i64(-1_234), "-1,234");
        assert_eq!(format_int_with_commas_i64(-1_234_567), "-1,234,567");
    }

    #[test]
    fn commas_i64_min_edge_case() {
        // i64::MIN.abs() would panic on i64; the impl casts to i128
        // first so unsigned_abs works. Verify the exact string.
        let expected_mag = "9,223,372,036,854,775,808"; // |i64::MIN|
        assert_eq!(
            format_int_with_commas_i64(i64::MIN),
            format!("-{}", expected_mag),
        );
    }

    #[test]
    fn commas_i64_max_edge_case() {
        assert_eq!(
            format_int_with_commas_i64(i64::MAX),
            "9,223,372,036,854,775,807",
        );
    }

    #[test]
    fn condensed_small_values_pass_through() {
        assert_eq!(format_int_condensed(0), "0");
        assert_eq!(format_int_condensed(1), "1");
        assert_eq!(format_int_condensed(999), "999");
    }

    #[test]
    fn condensed_thousands() {
        assert_eq!(format_int_condensed(1_000), "1.0K");
        assert_eq!(format_int_condensed(1_234), "1.2K");
        assert_eq!(format_int_condensed(99_999), "100.0K");
        // 100_000 crosses the "drop decimal" threshold at ≥100 in-unit.
        assert_eq!(format_int_condensed(100_000), "100K");
        assert_eq!(format_int_condensed(343_000), "343K");
        assert_eq!(format_int_condensed(999_999), "1000K");
    }

    #[test]
    fn condensed_millions() {
        assert_eq!(format_int_condensed(1_500_000), "1.5M");
        assert_eq!(format_int_condensed(12_300_000), "12.3M");
        assert_eq!(format_int_condensed(105_000_000), "105M");
    }

    #[test]
    fn condensed_billions_and_up() {
        assert_eq!(format_int_condensed(1_500_000_000), "1.5G");
        assert_eq!(format_int_condensed(105_000_000_000), "105G");
        assert_eq!(format_int_condensed(4_500_000_000_000), "4.5T");
        assert_eq!(format_int_condensed(1_500_000_000_000_000), "1.5P");
    }

    #[test]
    fn condensed_negatives() {
        assert_eq!(format_int_condensed(-1_234), "-1.2K");
        assert_eq!(format_int_condensed(-105_000_000), "-105M");
    }

    #[test]
    fn condensed_i64_min() {
        // i64::MIN → about -9.2E (>P suffix); we cap suffix at P.
        // Verify no panic and reasonable-looking output.
        let s = format_int_condensed(i64::MIN);
        assert!(s.starts_with('-'), "should be negative: {}", s);
        assert!(s.ends_with('P'), "expected P suffix: {}", s);
    }
}

/// Natural ("version") compare: digit runs compare as numbers so
/// `part_2 < part_10 < part_100`. Non-digit runs compare case-insensitive
/// as ASCII. Used by the Schema Explorer when sorting partition lists
/// where lexicographic order produces the classic `part_1, part_10,
/// part_2` annoyance. Non-ASCII bytes fall through to byte compare —
/// fine for PG identifier names which are practically always ASCII;
/// would need full Unicode handling if we ever start sorting arbitrary
/// user-facing labels.
pub(crate) fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let aa = a.as_bytes();
    let bb = b.as_bytes();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < aa.len() && j < bb.len() {
        let ac = aa[i];
        let bc = bb[j];
        if ac.is_ascii_digit() && bc.is_ascii_digit() {
            let i_end = i + aa[i..].iter().position(|c| !c.is_ascii_digit()).unwrap_or(aa.len() - i);
            let j_end = j + bb[j..].iter().position(|c| !c.is_ascii_digit()).unwrap_or(bb.len() - j);
            // Skip leading zeros for value compare so 002 == 2.
            let i_val = i + aa[i..i_end].iter().take_while(|&&c| c == b'0').count();
            let j_val = j + bb[j..j_end].iter().take_while(|&&c| c == b'0').count();
            let i_len = i_end - i_val;
            let j_len = j_end - j_val;
            match i_len.cmp(&j_len) {
                Ordering::Equal => {}
                ord => return ord,
            }
            // Same significant length → lexical compare is numeric compare
            // (ASCII digits sort the same way as their numeric values).
            match aa[i_val..i_end].cmp(&bb[j_val..j_end]) {
                Ordering::Equal => {}
                ord => return ord,
            }
            // Same value, possibly different leading-zero count. Shorter
            // total wins so `2` precedes `02`.
            match (i_end - i).cmp(&(j_end - j)) {
                Ordering::Equal => {}
                ord => return ord,
            }
            i = i_end;
            j = j_end;
        } else {
            match ac.to_ascii_lowercase().cmp(&bc.to_ascii_lowercase()) {
                Ordering::Equal => {}
                ord => return ord,
            }
            i += 1;
            j += 1;
        }
    }
    match (i < aa.len(), j < bb.len()) {
        (false, false) => Ordering::Equal,
        (false, true)  => Ordering::Less,
        (true, false)  => Ordering::Greater,
        (true, true)   => Ordering::Equal, // unreachable; loop preserves invariant
    }
}

#[cfg(test)]
mod natural_cmp_tests {
    use super::natural_cmp;
    use std::cmp::Ordering::*;

    #[test]
    fn part_numbers_sort_naturally() {
        assert_eq!(natural_cmp("part_2",  "part_10"), Less);
        assert_eq!(natural_cmp("part_10", "part_100"), Less);
        assert_eq!(natural_cmp("part_9",  "part_10"), Less);
        assert_eq!(natural_cmp("part_10", "part_2"), Greater);
    }

    #[test]
    fn equal_when_identical() {
        assert_eq!(natural_cmp("events_p_03", "events_p_03"), Equal);
    }

    #[test]
    fn purely_alphabetic() {
        assert_eq!(natural_cmp("Apple",  "banana"), Less);
        assert_eq!(natural_cmp("BANANA", "apple"),  Greater);
    }

    #[test]
    fn leading_zero_tiebreaker() {
        // Same numeric value; shorter (fewer leading zeros) wins so the
        // sort is stable across mixed-zero-padding sources.
        assert_eq!(natural_cmp("p_2", "p_02"), Less);
    }

    #[test]
    fn prefix_shorter_is_less() {
        assert_eq!(natural_cmp("part",   "part_1"), Less);
        assert_eq!(natural_cmp("part_1", "part"),   Greater);
    }
}

/// Format a raw byte count as a human-friendly string.
pub(crate) fn human_bytes(raw: &str) -> String {
    let Ok(n) = raw.parse::<u64>() else { return raw.to_string(); };
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    if n < KB       { format!("{} B", n) }
    else if n < MB  { format!("{:.1} KB", n as f64 / KB as f64) }
    else if n < GB  { format!("{:.1} MB", n as f64 / MB as f64) }
    else if n < TB  { format!("{:.2} GB", n as f64 / GB as f64) }
    else            { format!("{:.2} TB", n as f64 / TB as f64) }
}

/// Split a raw SQL column type into a pretty type name and its size string.
pub(crate) fn parse_column_type(raw: &str) -> (String, String) {
    if raw.is_empty() { return (String::new(), String::new()); }

    let (head, args) = match raw.find('(') {
        Some(i) => {
            let end = raw.rfind(')').unwrap_or(raw.len());
            (&raw[..i], &raw[i + 1..end])
        }
        None => (raw, ""),
    };
    let bare = head.trim();
    let lower = bare.to_lowercase();
    let mut stripped = lower.as_str();
    for suffix in [" unsigned zerofill", " unsigned", " zerofill"] {
        if let Some(s) = stripped.strip_suffix(suffix) { stripped = s; }
    }

    let pretty = match stripped {
        // MySQL types
        "tinyint"    => "TinyInt",
        "smallint"   => "SmallInt",
        "mediumint"  => "MediumInt",
        "int"        => "Int",
        "integer"    => "Int",
        "bigint"     => "BigInt",
        "decimal"    => "Decimal",
        "numeric"    => "Numeric",
        "float"      => "Float",
        "double"     => "Double",
        "bit"        => "Bit",
        "char"       => "Char",
        "varchar"    => "VarChar",
        "tinytext"   => "TinyText",
        "text"       => "Text",
        "mediumtext" => "MediumText",
        "longtext"   => "LongText",
        "binary"     => "Binary",
        "varbinary"  => "VarBinary",
        "tinyblob"   => "TinyBlob",
        "blob"       => "Blob",
        "mediumblob" => "MediumBlob",
        "longblob"   => "LongBlob",
        "date"       => "Date",
        "datetime"   => "DateTime",
        "timestamp"  => "TimeStamp",
        "time"       => "Time",
        "year"       => "Year",
        "enum"       => "Enum",
        "set"        => "Set",
        "json"       => "JSON",
        "geometry"   => "Geometry",
        "point"      => "Point",
        "linestring" => "LineString",
        "polygon"    => "Polygon",

        // Postgres aliases
        "int2"                     => "SmallInt",
        "int4"                     => "Int",
        "int8"                     => "BigInt",
        "smallserial"              => "SmallSerial",
        "serial"                   => "Serial",
        "bigserial"                => "BigSerial",
        "float4" | "real"          => "Real",
        "float8" | "double precision" => "Double",
        "bpchar"                   => "Char",
        "character varying"        => "VarChar",
        "character"                => "Char",
        "timestamptz" | "timestamp with time zone"    => "TimeStampTz",
        "timetz"      | "time with time zone"         => "TimeTz",
        "timestamp without time zone" => "TimeStamp",
        "time without time zone"      => "Time",
        "interval"                 => "Interval",
        "bytea"                    => "ByteA",
        "jsonb"                    => "JSONB",
        "uuid"                     => "UUID",
        "oid"                      => "Oid",
        "name"                     => "Name",
        "citext"                   => "CiText",
        "money"                    => "Money",
        "inet"                     => "Inet",
        "cidr"                     => "Cidr",
        "macaddr"                  => "MacAddr",

        _            => stripped,
    };
    let suffix = if raw.to_lowercase().contains("unsigned") { " Unsigned" } else { "" };

    let is_int = matches!(stripped,
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint"
      | "int2" | "int4" | "int8"
      | "smallserial" | "serial" | "bigserial");
    let size = if is_int { String::new() } else { args.to_string() };
    (format!("{}{}", pretty, suffix), size)
}

/// Compute superscript index tags for each column name.
pub(crate) fn compute_column_index_tags(
    indexes: &[IndexInfo],
    columns: &[ExplorerColumn],
) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut next_letter: u8 = b'a';

    for idx in indexes {
        let label = if idx.name.eq_ignore_ascii_case("PRIMARY") {
            "pk".to_string()
        } else {
            let l = next_letter;
            if next_letter < b'z' { next_letter += 1; }
            (l as char).to_string()
        };
        let composite = idx.columns.len() > 1;
        for (pos, col) in idx.columns.iter().enumerate() {
            let tag = if composite {
                format!("{}{}", label, pos + 1)
            } else {
                label.clone()
            };
            out.entry(col.clone()).or_default().push(tag);
        }
    }

    for c in columns {
        out.entry(c.name.clone()).or_default();
    }
    out
}

/// Produce the index's display tag per-index.
pub(crate) fn compute_index_tags(indexes: &[IndexInfo]) -> Vec<String> {
    let mut out = Vec::with_capacity(indexes.len());
    let mut next_letter: u8 = b'a';
    for idx in indexes {
        if idx.name.eq_ignore_ascii_case("PRIMARY") {
            out.push("pk".to_string());
        } else {
            let l = next_letter;
            if next_letter < b'z' { next_letter += 1; }
            out.push((l as char).to_string());
        }
    }
    out
}

/// Format a column default for display.
pub(crate) fn format_default(col_type: &str, default: &str, _is_nullable: bool) -> String {
    if default.is_empty() {
        return String::new();
    }
    let lower = col_type.to_lowercase();
    let is_numeric = matches!(lower.as_str(),
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint"
      | "decimal" | "numeric" | "float" | "double" | "bit")
      || lower.starts_with("int")
      || lower.starts_with("bigint")
      || lower.starts_with("decimal")
      || lower.starts_with("numeric")
      || lower.starts_with("float")
      || lower.starts_with("double")
      || lower.starts_with("tinyint")
      || lower.starts_with("smallint")
      || lower.starts_with("mediumint");
    let is_expr = matches!(default.to_uppercase().as_str(),
        "CURRENT_TIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIME" | "NULL");
    if is_numeric || is_expr {
        default.to_string()
    } else {
        format!("'{}'", default)
    }
}

pub(crate) fn pretty_index_kind(raw: &str) -> String {
    match raw.to_uppercase().as_str() {
        "BTREE"    => "B-Tree".into(),
        "HASH"     => "Hash".into(),
        "FULLTEXT" => "FullText".into(),
        "SPATIAL"  => "Spatial".into(),
        other      => other.to_string(),
    }
}

pub(crate) fn format_help(title: &str, items: &[(&str, &str)]) -> Vec<String> {
    let max = items.iter().map(|(cmd, _)| cmd.len()).max().unwrap_or(0);
    let mut lines = vec![format!("{}:", title), String::new()];
    for (cmd, desc) in items {
        lines.push(format!("  {:<width$}  {}", cmd, desc, width = max));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_int_with_commas ───────────────────────────────────────────

    #[test]
    fn test_commas_zero() {
        assert_eq!(format_int_with_commas("0"), "0");
    }

    #[test]
    fn test_commas_hundreds() {
        assert_eq!(format_int_with_commas("999"), "999");
    }

    #[test]
    fn test_commas_thousands() {
        assert_eq!(format_int_with_commas("1000"), "1,000");
    }

    #[test]
    fn test_commas_millions() {
        assert_eq!(format_int_with_commas("1234567"), "1,234,567");
    }

    #[test]
    fn test_commas_negative() {
        // The function only handles pure-digit strings; a leading '-' causes
        // it to return the input unchanged.
        assert_eq!(format_int_with_commas("-1234"), "-1234");
    }

    // ── human_bytes ──────────────────────────────────────────────────────

    #[test]
    fn test_bytes_zero() {
        assert_eq!(human_bytes("0"), "0 B");
    }

    #[test]
    fn test_bytes_small() {
        assert_eq!(human_bytes("500"), "500 B");
    }

    #[test]
    fn test_bytes_kb() {
        assert_eq!(human_bytes("1024"), "1.0 KB");
    }

    #[test]
    fn test_bytes_mb() {
        assert_eq!(human_bytes("1048576"), "1.0 MB");
    }

    #[test]
    fn test_bytes_gb() {
        assert_eq!(human_bytes("1073741824"), "1.00 GB");
    }

    // ── parse_column_type ────────────────────────────────────────────────

    #[test]
    fn test_varchar() {
        assert_eq!(parse_column_type("varchar(255)"), ("VarChar".into(), "255".into()));
    }

    #[test]
    fn test_int() {
        assert_eq!(parse_column_type("int"), ("Int".into(), "".into()));
    }

    #[test]
    fn test_bigint_unsigned() {
        assert_eq!(parse_column_type("bigint unsigned"), ("BigInt Unsigned".into(), "".into()));
    }

    #[test]
    fn test_numeric_precision() {
        assert_eq!(parse_column_type("numeric(10,2)"), ("Numeric".into(), "10,2".into()));
    }

    #[test]
    fn test_timestamp_tz() {
        assert_eq!(
            parse_column_type("timestamp with time zone"),
            ("TimeStampTz".into(), "".into()),
        );
    }

    #[test]
    fn test_character_varying() {
        assert_eq!(
            parse_column_type("character varying(100)"),
            ("VarChar".into(), "100".into()),
        );
    }

}
