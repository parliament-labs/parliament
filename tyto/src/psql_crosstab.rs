//! psql `\crosstabview` client-side pivot transform.
//!
//! Takes a normal query result `(columns, rows)` and pivots it into
//! a 2-D grid:
//!
//! - `col_v` — the column whose distinct values become the rows of
//!   the pivot.
//! - `col_h` — the column whose distinct values become the column
//!   headers of the pivot.
//! - `col_d` — the column whose values fill the grid cells.
//! - `sortcol_h` — optional column whose values rank-order the
//!   pivot's column headers. When `None`, headers come out in the
//!   order they're first encountered in the input rows.
//!
//! Column specs accept either a 1-based numeric index or a column
//! name (case-sensitive — matching psql).
//!
//! Mirrors `\crosstabview` from psql except in one small detail:
//! psql rejects duplicate `(col_v, col_h)` pairs with an error;
//! tyto keeps the FIRST cell value encountered for each pair and
//! emits a per-call warning in the caller's MetaOutput rather than
//! aborting (gentler UX for interactive use).

/// Resolve a column spec (numeric index or name) against a column
/// header list. Returns the 0-based index or an error string.
fn resolve_col(spec: &str, columns: &[String]) -> Result<usize, String> {
    // Try as a 1-based numeric index first.
    if let Ok(n) = spec.parse::<usize>() {
        if n == 0 {
            return Err(format!(
                "\\crosstabview: column index '{}' is out of range (1..={})",
                spec, columns.len(),
            ));
        }
        if n > columns.len() {
            return Err(format!(
                "\\crosstabview: column index '{}' is out of range (1..={})",
                spec, columns.len(),
            ));
        }
        return Ok(n - 1);
    }
    // Otherwise look up by name (case-sensitive).
    columns.iter().position(|c| c == spec).ok_or_else(|| {
        format!("\\crosstabview: column '{}' not found in result", spec)
    })
}

/// Resolve four optional column specs into 0-based indices,
/// applying psql's defaults (1st / 2nd / 3rd cols; no sort).
fn resolve_specs(
    columns:    &[String],
    col_v:      Option<&str>,
    col_h:      Option<&str>,
    col_d:      Option<&str>,
    sortcol_h:  Option<&str>,
) -> Result<(usize, usize, usize, Option<usize>), String> {
    let v = match col_v {
        Some(s) => resolve_col(s, columns)?,
        None    => 0,
    };
    let h = match col_h {
        Some(s) => resolve_col(s, columns)?,
        None    => {
            if columns.len() < 2 {
                return Err(format!(
                    "\\crosstabview: need at least 2 columns; query returned {}",
                    columns.len(),
                ));
            }
            1
        }
    };
    let d = match col_d {
        Some(s) => resolve_col(s, columns)?,
        None    => {
            if columns.len() < 3 {
                return Err(format!(
                    "\\crosstabview: need at least 3 columns; query returned {}",
                    columns.len(),
                ));
            }
            2
        }
    };
    if v == h {
        return Err(format!(
            "\\crosstabview: vertical and horizontal columns must differ",
        ));
    }
    if v == d || h == d {
        return Err(format!(
            "\\crosstabview: data column must differ from both row and column label columns",
        ));
    }
    let s = sortcol_h.map(|s| resolve_col(s, columns)).transpose()?;
    Ok((v, h, d, s))
}

/// A pivoted result: its columns, its rows (SQL NULL as `None`), and
/// the warnings to show over it.
pub(crate) type Pivot = (Vec<String>, Vec<Vec<Option<String>>>, Vec<String>);

/// Pivot `(columns, rows)` per the column specs. Returns the
/// pivoted `(columns, rows)` plus a list of warnings to surface to
/// the user (e.g., duplicate cells dropped). As psql's
/// `PrintResultInCrosstab`: a NULL row or column name, or value, stays
/// NULL — a column header shows it as `null_display` — and a cell no
/// row fills is empty.
pub(crate) fn pivot_table(
    columns:      &[String],
    rows:         &[Vec<Option<String>>],
    col_v:        Option<&str>,
    col_h:        Option<&str>,
    col_d:        Option<&str>,
    sortcol_h:    Option<&str>,
    null_display: &str,
) -> Result<Pivot, String> {
    let (vi, hi, di, si) = resolve_specs(columns, col_v, col_h, col_d, sortcol_h)?;
    let cell = |row: &Vec<Option<String>>, i: usize| row.get(i).cloned().flatten();

    if rows.is_empty() {
        // Just the vertical-column name — no horizontals possible
        // from zero rows.
        return Ok((vec![columns[vi].clone()], vec![], vec![]));
    }

    // Collect vertical-column distinct values in order of first
    // appearance — matches psql.
    let mut v_values: Vec<Option<String>> = Vec::new();
    let mut v_seen = std::collections::HashSet::new();
    for row in rows {
        let v = cell(row, vi);
        if v_seen.insert(v.clone()) {
            v_values.push(v);
        }
    }

    // Collect horizontal-column distinct values. When `si` is set,
    // pair each h-value with the sortcol_h value from its first
    // appearance so the pivot's columns sort by that ranking value.
    // Otherwise headers appear in first-encounter order.
    let mut h_values: Vec<Option<String>> = Vec::new();
    let mut h_seen = std::collections::HashSet::new();
    let mut h_sort_key: std::collections::HashMap<Option<String>, String> = std::collections::HashMap::new();
    for row in rows {
        let h = cell(row, hi);
        if h_seen.insert(h.clone()) {
            if let Some(s) = si {
                h_sort_key.insert(h.clone(), cell(row, s).unwrap_or_default());
            }
            h_values.push(h);
        }
    }
    if si.is_some() {
        // Stable sort by the captured sort key.
        h_values.sort_by(|a, b| {
            let ka = h_sort_key.get(a).map(|s| s.as_str()).unwrap_or("");
            let kb = h_sort_key.get(b).map(|s| s.as_str()).unwrap_or("");
            ka.cmp(kb)
        });
    }

    // Build the lookup grid: for each (v, h) pair, the first
    // encountered cell value wins. Track duplicates for a warning.
    let mut grid: std::collections::HashMap<(Option<String>, Option<String>), Option<String>> = std::collections::HashMap::new();
    let mut dupes = 0usize;
    for row in rows {
        match grid.entry((cell(row, vi), cell(row, hi))) {
            std::collections::hash_map::Entry::Occupied(_) => dupes += 1,
            std::collections::hash_map::Entry::Vacant(slot) => { slot.insert(cell(row, di)); }
        }
    }

    // Assemble output.
    let mut out_cols = Vec::with_capacity(1 + h_values.len());
    out_cols.push(columns[vi].clone());
    out_cols.extend(h_values.iter().map(|h| h.clone().unwrap_or_else(|| null_display.to_string())));

    let mut out_rows = Vec::with_capacity(v_values.len());
    for v in &v_values {
        let mut row = Vec::with_capacity(out_cols.len());
        row.push(v.clone());
        for h in &h_values {
            row.push(grid.get(&(v.clone(), h.clone())).cloned().unwrap_or_else(|| Some(String::new())));
        }
        out_rows.push(row);
    }

    let mut warnings = Vec::new();
    if dupes > 0 {
        warnings.push(format!(
            "\\crosstabview: {} duplicate (v, h) cell{} — kept the first value for each",
            dupes, if dupes == 1 { "" } else { "s" },
        ));
    }

    Ok((out_cols, out_rows, warnings))
}

/// The pivot's column types, as psql lines its columns up
/// (`PrintResultInCrosstab`): the row-header column's, then the data
/// column's for each of the `width - 1` pivoted ones.
pub(crate) fn pivot_types(
    columns: &[String],
    types:   &[String],
    col_v:   Option<&str>,
    col_h:   Option<&str>,
    col_d:   Option<&str>,
    width:   usize,
) -> Vec<String> {
    let Ok((vi, _, di, _)) = resolve_specs(columns, col_v, col_h, col_d, None) else { return Vec::new() };
    let type_of = |i: usize| types.get(i).cloned().unwrap_or_default();
    std::iter::once(type_of(vi)).chain(std::iter::repeat_n(type_of(di), width.saturating_sub(1))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pivot_types_follow_the_row_and_data_columns() {
        let cols: Vec<String> = ["v", "h", "d"].iter().map(|s| s.to_string()).collect();
        let types: Vec<String> = ["23", "25", "1700"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pivot_types(&cols, &types, None, None, None, 3), ["23", "1700", "1700"]);
        assert_eq!(pivot_types(&cols, &types, Some("h"), Some("d"), Some("v"), 2), ["25", "23"]);
        assert!(pivot_types(&cols, &[], None, None, None, 2).iter().all(String::is_empty));
    }

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A result's rows, "NULL" standing for SQL NULL.
    fn rows(table: &[&[&str]]) -> Vec<Vec<Option<String>>> {
        table.iter().map(|r| r.iter().map(|s| (*s != "NULL").then(|| s.to_string())).collect()).collect()
    }

    /// Pivoted rows as text, SQL NULL as "NULL".
    fn plain(rows: &[Vec<Option<String>>]) -> Vec<Vec<String>> {
        rows.iter().map(|r| r.iter().map(|c| c.clone().unwrap_or_else(|| "NULL".into())).collect()).collect()
    }

    #[test]
    fn defaults_use_first_three_columns() {
        let c = cols(&["year", "month", "sales"]);
        let r = rows(&[
            &["2020", "Jan", "100"],
            &["2020", "Feb", "200"],
            &["2021", "Jan", "300"],
            &["2021", "Feb", "400"],
        ]);
        let (oc, or, w) = pivot_table(&c, &r, None, None, None, None, "NULL").unwrap();
        assert_eq!(oc, vec!["year", "Jan", "Feb"]);
        assert_eq!(plain(&or), vec![
            vec!["2020", "100", "200"],
            vec!["2021", "300", "400"],
        ]);
        assert!(w.is_empty());
    }

    #[test]
    fn specs_by_numeric_index_1_based() {
        let c = cols(&["year", "month", "sales", "extra"]);
        let r = rows(&[
            &["2020", "Jan", "100", "x"],
            &["2020", "Feb", "200", "x"],
        ]);
        let (oc, or, _) = pivot_table(&c, &r, Some("1"), Some("2"), Some("3"), None, "NULL").unwrap();
        assert_eq!(oc, vec!["year", "Jan", "Feb"]);
        assert_eq!(plain(&or), vec![vec!["2020", "100", "200"]]);
    }

    #[test]
    fn specs_by_name() {
        let c = cols(&["year", "month", "sales"]);
        let r = rows(&[&["2020", "Jan", "100"]]);
        let (oc, or, _) = pivot_table(&c, &r, Some("year"), Some("month"), Some("sales"), None, "NULL").unwrap();
        assert_eq!(oc, vec!["year", "Jan"]);
        assert_eq!(plain(&or), vec![vec!["2020", "100"]]);
    }

    #[test]
    fn empty_rows_produces_only_v_column() {
        let c = cols(&["year", "month", "sales"]);
        let (oc, or, w) = pivot_table(&c, &[], None, None, None, None, "NULL").unwrap();
        assert_eq!(oc, vec!["year"]);
        assert!(or.is_empty());
        assert!(w.is_empty());
    }

    #[test]
    fn duplicate_v_h_pairs_keep_first_and_warn() {
        let c = cols(&["a", "b", "c"]);
        let r = rows(&[
            &["x", "1", "100"],
            &["x", "1", "999"],  // duplicate (x,1) — should keep 100
        ]);
        let (_oc, or, w) = pivot_table(&c, &r, None, None, None, None, "NULL").unwrap();
        assert_eq!(plain(&or), vec![vec!["x", "100"]]);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("1 duplicate"));
    }

    #[test]
    fn missing_v_h_cell_renders_empty() {
        let c = cols(&["a", "b", "c"]);
        let r = rows(&[
            &["x", "1", "100"],
            &["y", "2", "200"],
        ]);
        // (x,2) and (y,1) are missing — empty cells.
        let (oc, or, _) = pivot_table(&c, &r, None, None, None, None, "NULL").unwrap();
        assert_eq!(oc, vec!["a", "1", "2"]);
        assert_eq!(plain(&or), vec![
            vec!["x", "100", ""],
            vec!["y", "",    "200"],
        ]);
    }

    #[test]
    fn sortcol_h_orders_headers() {
        let c = cols(&["a", "b", "c", "sortby"]);
        // Headers first-encountered as 3, 1, 2 — should sort by sortby.
        let r = rows(&[
            &["x", "3", "C", "30"],
            &["x", "1", "A", "10"],
            &["x", "2", "B", "20"],
        ]);
        let (oc, or, _) = pivot_table(&c, &r, None, None, None, Some("sortby"), "NULL").unwrap();
        // After sort by sortby: 1 (10), 2 (20), 3 (30).
        assert_eq!(oc, vec!["a", "1", "2", "3"]);
        assert_eq!(plain(&or), vec![vec!["x", "A", "B", "C"]]);
    }

    #[test]
    fn unknown_column_name_errors() {
        let c = cols(&["a", "b", "c"]);
        let r = rows(&[&["x", "1", "v"]]);
        assert!(pivot_table(&c, &r, Some("nope"), None, None, None, "NULL").is_err());
    }

    #[test]
    fn out_of_range_index_errors() {
        let c = cols(&["a", "b", "c"]);
        let r = rows(&[&["x", "1", "v"]]);
        assert!(pivot_table(&c, &r, Some("0"), None, None, None, "NULL").is_err());
        assert!(pivot_table(&c, &r, Some("4"), None, None, None, "NULL").is_err());
    }

    #[test]
    fn vertical_equals_horizontal_errors() {
        let c = cols(&["a", "b", "c"]);
        let r = rows(&[&["x", "1", "v"]]);
        assert!(pivot_table(&c, &r, Some("a"), Some("1"), Some("c"), None, "NULL").is_err());
    }

    #[test]
    fn data_column_must_differ_from_v_and_h() {
        let c = cols(&["a", "b", "c"]);
        let r = rows(&[&["x", "1", "v"]]);
        // d == v
        assert!(pivot_table(&c, &r, Some("a"), Some("b"), Some("a"), None, "NULL").is_err());
        // d == h
        assert!(pivot_table(&c, &r, Some("a"), Some("b"), Some("b"), None, "NULL").is_err());
    }

    #[test]
    fn defaults_require_three_columns() {
        let c = cols(&["a", "b"]);
        let r = rows(&[&["x", "1"]]);
        // Default col_d=3rd column — but only 2 exist.
        assert!(pivot_table(&c, &r, None, None, None, None, "NULL").is_err());
    }

    #[test]
    fn null_h_value_becomes_column_header() {
        let c = cols(&["a", "b", "c"]);
        let r = rows(&[
            &["x", "NULL", "100"],
            &["y", "Jan",  "NULL"],
        ]);
        // psql's: the NULL column name as the null display; a NULL value
        // stays NULL, a cell no row fills is empty.
        let (oc, or, _) = pivot_table(&c, &r, None, None, None, None, "(null)").unwrap();
        assert_eq!(oc, vec!["a", "(null)", "Jan"]);
        assert_eq!(or, vec![
            vec![Some("x".to_string()), Some("100".into()), Some(String::new())],
            vec![Some("y".to_string()), Some(String::new()), None],
        ]);
    }

    #[test]
    fn first_encounter_ordering_preserved_without_sortcol() {
        let c = cols(&["a", "b", "c"]);
        let r = rows(&[
            &["row1", "Z", "1"],
            &["row1", "A", "2"],
            &["row1", "M", "3"],
        ]);
        let (oc, _, _) = pivot_table(&c, &r, None, None, None, None, "NULL").unwrap();
        // First-encounter order, NOT alphabetical.
        assert_eq!(oc, vec!["a", "Z", "A", "M"]);
    }
}
