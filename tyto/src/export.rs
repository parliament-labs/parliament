use leptos::*;
use wasm_bindgen::JsCast;

pub(crate) fn trigger_download(filename: &str, content: &str, mime: &str) {
    let Some(window) = web_sys::window() else { return };
    let Some(document) = window.document() else { return };
    let array = js_sys::Array::new();
    array.push(&wasm_bindgen::JsValue::from_str(content));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(
        &array, &opts,
    ) else { return };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else { return };
    if let Ok(a) = document.create_element("a") {
        let _ = a.set_attribute("href", &url);
        let _ = a.set_attribute("download", filename);
        a.unchecked_ref::<web_sys::HtmlElement>().click();
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}

/// A result as CSV: SQL NULL an empty field, an empty string `""`, as
/// PostgreSQL's CSV writes them apart.
pub(crate) fn to_csv(columns: &[String], rows: &[Vec<Option<String>>]) -> String {
    let mut out = String::new();
    out.push_str(&columns.iter().map(|c| csv_quote(c)).collect::<Vec<_>>().join(","));
    out.push('\n');
    for row in rows {
        let cells: Vec<String> = row.iter().map(|cell| match cell.as_deref() {
            None => String::new(),
            Some("") => "\"\"".into(),
            Some(value) => csv_quote(value),
        }).collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

pub(crate) fn csv_quote(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// A result as JSON objects, SQL NULL as `null`.
pub(crate) fn to_json(columns: &[String], rows: &[Vec<Option<String>>]) -> String {
    let objects: Vec<String> = rows.iter().map(|row| {
        let fields: Vec<String> = columns.iter().zip(row.iter()).map(|(col, val)| {
            format!("    {}: {}", json_str(col), val.as_deref().map_or_else(|| "null".to_string(), json_str))
        }).collect();
        format!("  {{\n{}\n  }}", fields.join(",\n"))
    }).collect();
    format!("[\n{}\n]\n", objects.join(",\n"))
}

pub(crate) fn json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t"))
}

/// A result as INSERT statements, SQL NULL a bare `NULL`.
pub(crate) fn to_sql_inserts(columns: &[String], rows: &[Vec<Option<String>>]) -> String {
    let cols = columns.iter().map(|c| format!("`{}`", c.replace('`', "``"))).collect::<Vec<_>>().join(", ");
    let mut out = String::new();
    for row in rows {
        let vals = row.iter().map(|v| match v {
            None => "NULL".to_string(),
            Some(v) => format!("'{}'", v.replace('\'', "''")),
        }).collect::<Vec<_>>().join(", ");
        out.push_str(&format!("INSERT INTO table_name ({}) VALUES ({});\n", cols, vals));
    }
    out
}

pub(crate) fn render_save_links(columns: Vec<String>, rows: Vec<Vec<Option<String>>>) -> impl IntoView {
    let (c1, r1) = (columns.clone(), rows.clone());
    let (c2, r2) = (columns.clone(), rows.clone());
    let (c3, r3) = (columns, rows);
    view! {
        <span class="shell-save-links">
            " · Save as "
            <a class="shell-save-link" href="#" on:click=move |e| {
                e.prevent_default();
                trigger_download("result.csv", &to_csv(&c1, &r1), "text/csv");
            }>"CSV"</a>
            " · "
            <a class="shell-save-link" href="#" on:click=move |e| {
                e.prevent_default();
                trigger_download("result.json", &to_json(&c2, &r2), "application/json");
            }>"JSON"</a>
            " · "
            <a class="shell-save-link" href="#" on:click=move |e| {
                e.prevent_default();
                trigger_download("result.sql", &to_sql_inserts(&c3, &r3), "text/sql");
            }>"SQL"</a>
        </span>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── csv_quote ────────────────────────────────────────────────────────

    #[test]
    fn test_csv_plain() {
        assert_eq!(csv_quote("hello"), "hello");
    }

    #[test]
    fn test_csv_comma() {
        assert_eq!(csv_quote("a,b"), "\"a,b\"");
    }

    #[test]
    fn test_csv_quotes() {
        assert_eq!(csv_quote(r#"say "hi""#), r#""say ""hi""""#);
    }

    #[test]
    fn test_csv_newline() {
        assert_eq!(csv_quote("a\nb"), "\"a\nb\"");
    }

    // ── to_csv ───────────────────────────────────────────────────────────

    #[test]
    fn test_csv_output() {
        let cols = vec!["a".into(), "b".into()];
        let rows = vec![vec![Some("1".into()), Some("2".into())], vec![Some("3".into()), Some("4".into())]];
        assert_eq!(to_csv(&cols, &rows), "a,b\n1,2\n3,4\n");
    }

    #[test]
    fn test_csv_null() {
        let cols = vec!["x".into()];
        // SQL NULL an empty field, an empty string `""`, the text 'NULL'
        // as written.
        let rows = vec![vec![None], vec![Some(String::new())], vec![Some("NULL".into())]];
        assert_eq!(to_csv(&cols, &rows), "x\n\n\"\"\nNULL\n");
    }

    // ── to_json ──────────────────────────────────────────────────────────

    #[test]
    fn test_json_output() {
        let cols = vec!["name".into()];
        let rows = vec![vec![Some("Alice".into())]];
        let out = to_json(&cols, &rows);
        assert!(out.contains("\"name\""));
        assert!(out.contains("\"Alice\""));
    }

    #[test]
    fn test_json_null() {
        let cols = vec!["x".into()];
        let rows = vec![vec![None], vec![Some("NULL".into())]];
        let out = to_json(&cols, &rows);
        // SQL NULL a bare null; the text 'NULL' a string.
        assert!(out.contains("\"x\": null"));
        assert!(out.contains("\"x\": \"NULL\""));
    }

    // ── to_sql_inserts ───────────────────────────────────────────────────

    #[test]
    fn test_sql_insert() {
        let cols = vec!["id".into(), "name".into()];
        let rows = vec![vec![Some("1".into()), Some("Alice".into())]];
        let out = to_sql_inserts(&cols, &rows);
        assert!(out.contains("INSERT INTO"));
        assert!(out.contains("'Alice'"));
    }

    #[test]
    fn test_sql_null() {
        let cols = vec!["x".into()];
        let rows = vec![vec![None], vec![Some("NULL".into())]];
        let out = to_sql_inserts(&cols, &rows);
        // SQL NULL a bare NULL; the text 'NULL' a literal.
        assert!(out.contains("VALUES (NULL);"));
        assert!(out.contains("VALUES ('NULL');"));
    }

    #[test]
    fn test_sql_escape() {
        let cols = vec!["name".into()];
        let rows = vec![vec![Some("O'Brien".into())]];
        let out = to_sql_inserts(&cols, &rows);
        assert!(out.contains("'O''Brien'"));
    }
}
