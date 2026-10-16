//! psql's table printers — fe_utils/print.c as psql 18.4 has it, under
//! `\pset border`, `linestyle`, the separators and table attributes
//! (`PrintOptions.text`) — so a table psql composes itself, like
//! `\d <table>`, and a result under `\pset format`, read exactly as psql
//! prints them. Each printer writes
//! the text psql would, as psql's does; `print_table` hands it back a
//! line at a time.

use unicode_width::UnicodeWidthChar;

pub(super) use crate::types::Align;
use shared::NumericLocale;

use crate::types::{Expanded, LineStyle, OutputFormat, PrintOptions, Rules, Separator, XHeader};

/// The letter psql writes for an alignment (latex, troff-ms).
fn letter(align: Align) -> char {
    match align {
        Align::Left => 'l',
        Align::Right => 'r',
    }
}

/// How psql lines up a result's column (`column_type_alignment`):
/// numbers right — by the type's OID, as the server sends it
/// (`QueryResult.types`) — everything else left.
pub(super) fn type_alignment(type_oid: &str) -> Align {
    // int8, int2, int4, oid, xid, cid, float4, float8, money, numeric, xid8
    match type_oid {
        "20" | "21" | "23" | "26" | "28" | "29" | "700" | "701" | "790" | "1700" | "5069" => Align::Right,
        _ => Align::Left,
    }
}

/// A table for [`print_table`]: psql's `printTableContent`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PrintTable {
    pub title:   Option<String>,
    pub headers: Vec<String>,
    /// One per column; a missing one is `Left`.
    pub aligns:  Vec<Align>,
    pub rows:    Vec<Vec<String>>,
    /// Lines printed under the table (each may hold newlines).
    pub footers: Vec<String>,
    /// psql's default footer, `(2 rows)`, when there are no others and
    /// `\pset footer` is on (`footers_with_default`) — which the expanded
    /// printers leave out.
    pub count_rows: bool,
}

impl PrintTable {
    fn align(&self, col: usize) -> Align {
        self.aligns.get(col).copied().unwrap_or(Align::Left)
    }

    /// The cell at `row`, `col` (a short row reads as empty there).
    fn cell(&self, row: usize, col: usize) -> &str {
        self.rows[row].get(col).map_or("", String::as_str)
    }

    /// Its footers, or else, when it counts its rows, psql's count.
    fn footers_with_default(&self, o: &PrintOptions) -> Vec<String> {
        if self.footers.is_empty() && self.count_rows && o.footer {
            match self.rows.len() {
                1 => vec!["(1 row)".into()],
                n => vec![format!("({n} rows)")],
            }
        } else {
            self.footers.clone()
        }
    }
}

/// The table as psql prints it with these options (`printTable`), one
/// entry per line: in the format `\pset format` says, expanded under
/// `\x on` — and, aligned or wrapped, under `\x auto` when it's wider
/// than the pane. (Parliament's own `json` and `markdown` print psql's
/// tables aligned.)
pub(super) fn print_table(t: &PrintTable, o: &PrintOptions) -> Vec<String> {
    let mut out = String::new();
    let vertical = o.expanded == Expanded::On;
    match o.format {
        OutputFormat::Unaligned if vertical => unaligned_vertical(t, o, &mut out),
        OutputFormat::Unaligned => unaligned_text(t, o, &mut out),
        OutputFormat::Csv if vertical => csv_vertical(t, o, &mut out),
        OutputFormat::Csv => csv_text(t, o, &mut out),
        OutputFormat::Html if vertical => html_vertical(t, o, &mut out),
        OutputFormat::Html => html_text(t, o, &mut out),
        OutputFormat::Asciidoc if vertical => asciidoc_vertical(t, o, &mut out),
        OutputFormat::Asciidoc => asciidoc_text(t, o, &mut out),
        OutputFormat::Latex | OutputFormat::LatexLongtable if vertical => latex_vertical(t, o, &mut out),
        OutputFormat::Latex => latex_text(t, o, &mut out),
        OutputFormat::LatexLongtable => latex_longtable_text(t, o, &mut out),
        OutputFormat::TroffMs if vertical => troff_ms_vertical(t, o, &mut out),
        OutputFormat::TroffMs => troff_ms_text(t, o, &mut out),
        OutputFormat::Aligned | OutputFormat::Wrapped | OutputFormat::Json | OutputFormat::Markdown if vertical => {
            aligned_vertical(t, o, &mut out)
        }
        OutputFormat::Aligned | OutputFormat::Wrapped | OutputFormat::Json | OutputFormat::Markdown => {
            aligned_text(t, o, &mut out)
        }
    }
    lines(&out)
}

/// Printed text a line at a time — each line ended by its newline, so
/// psql's blank line after a table is an empty last line.
fn lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n').unwrap_or(text).split('\n').map(String::from).collect()
}

fn pad(out: &mut String, n: usize) {
    out.extend(std::iter::repeat_n(' ', n));
}

// ── Numbers in the client's locale ──────────────────────────────────

/// psql's `setDecimalLocale`: the decimal point, thousands separator and
/// group size its printer writes numbers with, from the client's
/// `localeconv()` — `.` for an empty decimal point; for an empty
/// separator `,`, or `.` where the decimal point is `,`; groups of 3
/// unless the locale's first is 1–6 (only the first counts: Indian
/// grouping prints in threes, as psql's does).
fn decimal_locale(locale: &NumericLocale) -> (&str, &str, usize) {
    let decimal_point = if locale.decimal_point.is_empty() { "." } else { locale.decimal_point.as_str() };
    let thousands_sep = match locale.thousands_sep.as_str() {
        "" if decimal_point == "," => ".",
        "" => ",",
        sep => sep,
    };
    let group = usize::try_from(locale.grouping).ok().filter(|g| (1..=6).contains(g)).unwrap_or(3);
    (decimal_point, thousands_sep, group)
}

/// A number as psql writes it under `\pset numericlocale`
/// (`format_numeric_locale`): its integer digits grouped with the
/// locale's thousands separator, its `.` the locale's decimal point —
/// anything but digits, a sign, `.` and an exponent (money, `NaN`,
/// `Infinity`) left as it is.
pub(super) fn format_numeric_locale(text: &str, locale: &NumericLocale) -> String {
    if !text.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E')) {
        return text.to_string();
    }
    let (decimal_point, thousands_sep, group) = decimal_locale(locale);
    let (sign, rest) = match text.as_bytes().first() {
        Some(b'+' | b'-') => text.split_at(1),
        _ => ("", text),
    };
    let int_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    let mut out = String::from(sign);
    // Digits before the first separator.
    let mut leading = match int_len % group { 0 => group, n => n };
    for (i, digit) in rest[..int_len].chars().enumerate() {
        if i > 0 {
            leading -= 1;
            if leading == 0 {
                out.push_str(thousands_sep);
                leading = group;
            }
        }
        out.push(digit);
    }
    let rest = &rest[int_len..];
    match rest.strip_prefix('.') {
        Some(fraction) => {
            out.push_str(decimal_point);
            out.push_str(fraction);
        }
        None => out.push_str(rest),
    }
    out
}

/// What psql's `printQuery` does to a result's cells before printing
/// them: under `\pset numericlocale`, each in a right-aligned (number)
/// column written as the client's locale writes numbers.
pub(super) fn localize_numbers(t: &mut PrintTable, o: &PrintOptions) {
    if !o.text.numeric_locale {
        return;
    }
    for row in &mut t.rows {
        for (c, cell) in row.iter_mut().enumerate() {
            if t.aligns.get(c) == Some(&Align::Right) {
                *cell = format_numeric_locale(cell, &o.locale);
            }
        }
    }
}

// ── Line styles ─────────────────────────────────────────────────────

/// Where a rule is drawn (`printTextRule`): over the table, under its
/// headers (and between expanded records), under it, and the lines down
/// a row of data.
const TOP: usize = 0;
const MIDDLE: usize = 1;
const BOTTOM: usize = 2;
const DATA: usize = 3;

/// A rule's characters (`printTextLineFormat`): the line itself, and
/// where it meets the left edge, a column line, the right edge.
#[derive(Clone, Copy)]
struct RuleFormat {
    hrule:      &'static str,
    leftvrule:  &'static str,
    midvrule:   &'static str,
    rightvrule: &'static str,
}

/// A line style (`printTextFormat`): its rules; the column line beside
/// a value continued after a line break, after a wrap, and beside one
/// with nothing more to show; the marks either side of a header or value
/// continued after a line break or a wrap.
struct TextFormat {
    lrule:             [RuleFormat; 4],
    midvrule_nl:       &'static str,
    midvrule_wrap:     &'static str,
    midvrule_blank:    &'static str,
    header_nl_left:    &'static str,
    header_nl_right:   &'static str,
    nl_left:           &'static str,
    nl_right:          &'static str,
    wrap_left:         &'static str,
    wrap_right:        &'static str,
    /// Room for a mark at the right of the last column under border 0.
    wrap_right_border: bool,
    /// old-ascii, whose marks some layouts make room for or leave out
    /// (`format == &pg_asciiformat_old`).
    old_ascii:         bool,
}

const ASCII_RULE: RuleFormat = RuleFormat { hrule: "-", leftvrule: "+", midvrule: "+", rightvrule: "+" };
const ASCII_DATA: RuleFormat = RuleFormat { hrule: "", leftvrule: "|", midvrule: "|", rightvrule: "|" };

/// `pg_asciiformat`: `-+|`, a line break marked `+` at the right, a wrap
/// `.` either side.
const ASCII: TextFormat = TextFormat {
    lrule: [ASCII_RULE, ASCII_RULE, ASCII_RULE, ASCII_DATA],
    midvrule_nl: "|", midvrule_wrap: "|", midvrule_blank: "|",
    header_nl_left: " ", header_nl_right: "+", nl_left: " ", nl_right: "+", wrap_left: ".", wrap_right: ".",
    wrap_right_border: true, old_ascii: false,
};

/// `pg_asciiformat_old`, psql 8.4's: a continued value marked in the
/// column line (`:` after a line break, `;` after a wrap), a continued
/// header `+` at its left.
const OLD_ASCII: TextFormat = TextFormat {
    lrule: [ASCII_RULE, ASCII_RULE, ASCII_RULE, ASCII_DATA],
    midvrule_nl: ":", midvrule_wrap: ";", midvrule_blank: " ",
    header_nl_left: "+", header_nl_right: " ", nl_left: " ", nl_right: " ", wrap_left: " ", wrap_right: " ",
    wrap_right_border: false, old_ascii: true,
};

/// The unicode style's box-drawing characters, single and double
/// (`unicode_style`). A row's: its line, where it meets a single / double
/// left edge, right edge.
struct UnicodeRow {
    horizontal:         &'static str,
    vertical_and_right: [&'static str; 2],
    vertical_and_left:  [&'static str; 2],
}

/// A column line's: itself, and where it crosses / ends on / starts from
/// a single / double rule.
struct UnicodeColumn {
    vertical:                &'static str,
    vertical_and_horizontal: [&'static str; 2],
    up_and_horizontal:       [&'static str; 2],
    down_and_horizontal:     [&'static str; 2],
}

/// The frame's: its corners and lines.
struct UnicodeBorder {
    up_and_right:   &'static str,
    vertical:       &'static str,
    down_and_right: &'static str,
    horizontal:     &'static str,
    down_and_left:  &'static str,
    up_and_left:    &'static str,
}

const UNICODE_ROWS: [UnicodeRow; 2] = [
    UnicodeRow { horizontal: "─", vertical_and_right: ["├", "╟"], vertical_and_left: ["┤", "╢"] },
    UnicodeRow { horizontal: "═", vertical_and_right: ["╞", "╠"], vertical_and_left: ["╡", "╣"] },
];
const UNICODE_COLUMNS: [UnicodeColumn; 2] = [
    UnicodeColumn { vertical: "│", vertical_and_horizontal: ["┼", "╪"], up_and_horizontal: ["┴", "╧"], down_and_horizontal: ["┬", "╤"] },
    UnicodeColumn { vertical: "║", vertical_and_horizontal: ["╫", "╬"], up_and_horizontal: ["╨", "╩"], down_and_horizontal: ["╥", "╦"] },
];
const UNICODE_BORDERS: [UnicodeBorder; 2] = [
    UnicodeBorder { up_and_right: "└", vertical: "│", down_and_right: "┌", horizontal: "─", down_and_left: "┐", up_and_left: "┘" },
    UnicodeBorder { up_and_right: "╚", vertical: "║", down_and_right: "╔", horizontal: "═", down_and_left: "╗", up_and_left: "╝" },
];

/// The line style `\pset linestyle` names; unicode's built from its
/// border, column and header lines (`refresh_utf8format`): a line break
/// marked `↵` at the right, a wrap `…` either side.
fn text_format(rules: &Rules) -> TextFormat {
    match rules.style {
        LineStyle::Ascii => ASCII,
        LineStyle::OldAscii => OLD_ASCII,
        LineStyle::Unicode => {
            let b = rules.unicode_border as usize;
            let border = &UNICODE_BORDERS[b];
            let header = &UNICODE_ROWS[rules.unicode_header as usize];
            let column = &UNICODE_COLUMNS[rules.unicode_column as usize];
            TextFormat {
                lrule: [
                    RuleFormat {
                        hrule: border.horizontal, leftvrule: border.down_and_right,
                        midvrule: column.down_and_horizontal[b], rightvrule: border.down_and_left,
                    },
                    RuleFormat {
                        hrule: header.horizontal, leftvrule: header.vertical_and_right[b],
                        midvrule: column.vertical_and_horizontal[rules.unicode_header as usize],
                        rightvrule: header.vertical_and_left[b],
                    },
                    RuleFormat {
                        hrule: border.horizontal, leftvrule: border.up_and_right,
                        midvrule: column.up_and_horizontal[b], rightvrule: border.up_and_left,
                    },
                    RuleFormat { hrule: "", leftvrule: border.vertical, midvrule: column.vertical, rightvrule: border.vertical },
                ],
                midvrule_nl: column.vertical, midvrule_wrap: column.vertical, midvrule_blank: column.vertical,
                header_nl_left: " ", header_nl_right: "↵", nl_left: " ", nl_right: "↵", wrap_left: "…", wrap_right: "…",
                wrap_right_border: true, old_ascii: false,
            }
        }
    }
}

// ── Aligned and wrapped ─────────────────────────────────────────────

/// What a display line of a column carries on into the next
/// (`printTextLineWrap`): nothing, its text wrapped, or a newline.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wrap {
    None,
    Wrapped,
    Newline,
}

/// The table's width besides its columns' (`width_total` before them):
/// under border 0 a space after each column, under 1 ` | ` between them
/// and a space at each end, under 2 `| ` … ` |` around them too.
fn rules_width(ncols: usize, border: u16) -> usize {
    match border {
        0 => ncols,
        1 => (ncols * 3).saturating_sub(1),
        _ => ncols * 3 + 1,
    }
}

/// A rule across the table (`_print_horizontal_line`): under border 0
/// dashes under each column, a space between; under 1 a line through
/// the column lines; under 2 to the edges as well.
fn horizontal_line(widths: &[usize], border: u16, rule: &RuleFormat, out: &mut String) {
    if border == 1 {
        out.push_str(rule.hrule);
    } else if border == 2 {
        out.push_str(rule.leftvrule);
        out.push_str(rule.hrule);
    }
    for (c, width) in widths.iter().enumerate() {
        out.push_str(&rule.hrule.repeat(*width));
        if c + 1 < widths.len() {
            if border == 0 {
                out.push(' ');
            } else {
                out.push_str(rule.hrule);
                out.push_str(rule.midvrule);
                out.push_str(rule.hrule);
            }
        }
    }
    if border == 2 {
        out.push_str(rule.hrule);
        out.push_str(rule.rightvrule);
    } else if border == 1 {
        out.push_str(rule.hrule);
    }
    out.push('\n');
}

/// `print_aligned_text`: centred headers over a rule, each column as
/// wide as its widest line, a cell holding newlines continued with the
/// line style's mark; drawn as `\pset border` and `linestyle` say.
/// `wrapped` narrows the columns that gain most until the table fits
/// the pane, marking each break; `\x auto` prints it expanded instead
/// when it doesn't fit.
fn aligned_text(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let border = o.text.rules.border.min(2);
    let format = text_format(&o.text.rules);
    let dformat = &format.lrule[DATA];
    let ncols = t.headers.len();
    let headers: Vec<Vec<Line>> = t.headers.iter().map(|h| format_text(h)).collect();
    let cells: Vec<Vec<Vec<Line>>> = (0..t.rows.len())
        .map(|r| (0..ncols).map(|c| format_text(t.cell(r, c))).collect())
        .collect();
    let width_header: Vec<usize> = headers.iter().map(|h| max_width(h)).collect();
    let mut widest = width_header.clone();
    let mut width_average = vec![0usize; ncols];
    for row in &cells {
        for (c, cell) in row.iter().enumerate() {
            let width = max_width(cell);
            widest[c] = widest[c].max(width);
            width_average[c] += width;
        }
    }
    if !cells.is_empty() {
        for average in &mut width_average {
            *average /= cells.len();
        }
    }
    let rules = rules_width(ncols, border);
    let mut width_total = rules + widest.iter().sum::<usize>();
    let total_header_width = rules + width_header.iter().sum::<usize>();
    let mut width_wrap = widest.clone();
    let output_columns = o.columns.unwrap_or(0);

    if o.format == OutputFormat::Wrapped && output_columns > 0 && output_columns >= total_header_width {
        // Narrow the column whose widest value is furthest above its
        // average — the one fewest values wrap in — a character at a
        // time, slightly against wide ones, never below its header.
        while width_total > output_columns {
            let mut max_ratio = 0.0;
            let mut worst = None;
            for c in 0..ncols {
                if width_average[c] != 0 && width_wrap[c] > width_header[c] {
                    let ratio = width_wrap[c] as f64 / width_average[c] as f64 + widest[c] as f64 * 0.01;
                    if ratio > max_ratio {
                        max_ratio = ratio;
                        worst = Some(c);
                    }
                }
            }
            let Some(worst) = worst else { break };
            width_wrap[worst] -= 1;
            width_total -= 1;
        }
    }

    // `\x auto`: expanded when it doesn't fit — never a single column,
    // which would be wider still.
    if o.expanded == Expanded::Auto && output_columns > 0 && ncols > 1
        && (output_columns < total_header_width || output_columns < width_total)
    {
        return aligned_vertical(t, o, out);
    }

    if !o.tuples_only {
        if let Some(title) = &t.title {
            let width = max_width(&format_text(title));
            if width < width_total {
                pad(out, (width_total - width) / 2);
            }
            out.push_str(title);
            out.push('\n');
        }

        if border == 2 {
            horizontal_line(&width_wrap, border, &format.lrule[TOP], out);
        }
        let mut done = vec![false; ncols];
        let mut remaining = ncols;
        let mut line = 0;
        while remaining > 0 {
            if border == 2 {
                out.push_str(dformat.leftvrule);
            }
            for c in 0..ncols {
                if border != 0 || (!format.wrap_right_border && c > 0) {
                    out.push_str(if line == 0 { " " } else { format.header_nl_left });
                }
                if !done[c] {
                    let text = &headers[c][line];
                    let spare = width_wrap[c].saturating_sub(text.width);
                    pad(out, spare / 2);
                    out.push_str(&text.text);
                    pad(out, spare.div_ceil(2));
                    if headers[c].get(line + 1).is_none() {
                        remaining -= 1;
                        done[c] = true;
                    }
                } else {
                    pad(out, width_wrap[c]);
                }
                if border != 0 || format.wrap_right_border {
                    out.push_str(if done[c] { " " } else { format.header_nl_right });
                }
                if border != 0 && c + 1 < ncols {
                    out.push_str(dformat.midvrule);
                }
            }
            line += 1;
            if border == 2 {
                out.push_str(dformat.rightvrule);
            }
            out.push('\n');
        }
        horizontal_line(&width_wrap, border, &format.lrule[MIDDLE], out);
    }

    let mut wrap = vec![Wrap::None; ncols];
    // psql walks the cells: a row of no columns prints nothing.
    for row in cells.iter().filter(|_| ncols > 0) {
        let mut line = vec![0usize; ncols];
        let mut offset = vec![0usize; ncols];
        loop {
            let mut more = false;
            if border == 2 {
                out.push_str(dformat.leftvrule);
            }
            for c in 0..ncols {
                let right = t.align(c) == Align::Right;
                // Spaces to the column's width: all but the last column's,
                // under border 2 that too.
                let finalspaces = border == 2 || c + 1 < ncols;
                let mut printed = width_wrap[c];
                if border != 0 {
                    out.push_str(match wrap[c] {
                        Wrap::Wrapped => format.wrap_left,
                        Wrap::Newline => format.nl_left,
                        Wrap::None => " ",
                    });
                }
                match row[c].get(line[c]) {
                    None => {
                        if finalspaces {
                            pad(out, printed);
                        }
                    }
                    Some(text) => {
                        let rest = &text.text[offset[c]..];
                        let (bytes, width) = strlen_max_width(rest, width_wrap[c]);
                        printed = width.min(width_wrap[c]);
                        if right {
                            pad(out, width_wrap[c] - printed);
                        }
                        out.push_str(&rest[..bytes]);
                        offset[c] += bytes;
                        if offset[c] < text.text.len() {
                            more = true;
                        } else {
                            line[c] += 1;
                            more |= row[c].get(line[c]).is_some();
                            offset[c] = 0;
                        }
                    }
                }
                wrap[c] = match row[c].get(line[c]) {
                    Some(_) if offset[c] != 0 => Wrap::Wrapped,
                    Some(_) if line[c] != 0 => Wrap::Newline,
                    _ => Wrap::None,
                };
                if !right && (finalspaces || wrap[c] != Wrap::None) {
                    pad(out, width_wrap[c] - printed);
                }
                out.push_str(match wrap[c] {
                    Wrap::Wrapped => format.wrap_right,
                    Wrap::Newline => format.nl_right,
                    Wrap::None if finalspaces => " ",
                    Wrap::None => "",
                });
                // The line before the next column: as that column goes
                // on — its mark for this line, or nothing left to show.
                if border != 0 && c + 1 < ncols {
                    out.push_str(match wrap[c + 1] {
                        Wrap::Wrapped => format.midvrule_wrap,
                        Wrap::Newline => format.midvrule_nl,
                        Wrap::None if row[c + 1].get(line[c + 1]).is_none() => format.midvrule_blank,
                        Wrap::None => dformat.midvrule,
                    });
                }
            }
            if border == 2 {
                out.push_str(dformat.rightvrule);
            }
            out.push('\n');
            if !more {
                break;
            }
        }
    }

    if border == 2 {
        horizontal_line(&width_wrap, border, &format.lrule[BOTTOM], out);
    }
    if !o.tuples_only {
        for footer in t.footers_with_default(o) {
            out.push_str(&footer);
            out.push('\n');
        }
    }
    out.push('\n');
}

/// `print_aligned_vertical` (`\x`): a record line over each row, then
/// each column's header and value, a value holding newlines continued
/// with the line style's mark; drawn as `\pset border` and `linestyle`
/// say. `wrapped` fits the values to the pane, marking each break. Its
/// own footers follow; no row count, but for a table with no rows.
fn aligned_vertical(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let ncols = t.headers.len();
    if t.rows.is_empty() || ncols == 0 {
        if !o.tuples_only {
            for footer in t.footers_with_default(o) {
                out.push_str(&footer);
                out.push('\n');
            }
        }
        out.push('\n');
        return;
    }
    let border = o.text.rules.border.min(2);
    let format = text_format(&o.text.rules);
    let dformat = &format.lrule[DATA];
    let headers: Vec<Vec<Line>> = t.headers.iter().map(|h| format_text(h)).collect();
    let cells: Vec<Vec<Vec<Line>>> = (0..t.rows.len())
        .map(|r| (0..ncols).map(|c| format_text(t.cell(r, c))).collect())
        .collect();
    let hwidth = headers.iter().map(|h| max_width(h)).max().unwrap_or(0);
    let hmultiline = headers.iter().any(|h| h.len() > 1);
    let mut dwidth = cells.iter().flatten().map(|cell| max_width(cell)).max().unwrap_or(0);
    let mut dmultiline = cells.iter().flatten().any(|cell| cell.len() > 1);
    // Old-ascii marks a continued header at its left, in a column of
    // its own under borders 0 and 1.
    let old_header_marks = hmultiline && format.old_ascii;

    if !o.tuples_only {
        if let Some(title) = &t.title {
            out.push_str(title);
            out.push('\n');
        }
    }

    // `wrapped`: the values as wide as the pane leaves them — at least
    // three characters, and as wide as the record line needs.
    if o.format == OutputFormat::Wrapped {
        let output_columns = o.columns.unwrap_or(0);
        // What isn't header or value: the spaces and line between them
        // (and the frame), a column for a header's line-break marks.
        let mut swidth = match border {
            0 => 1 + usize::from(hmultiline),
            1 => 3 + usize::from(old_header_marks),
            _ => 7,
        };
        // A value's marks get a column of their own but under border 2
        // (the frame's spaces) or old-ascii (in the column line).
        let marks_column = border < 2 && !format.old_ascii;
        if dmultiline && marks_column {
            swidth += 1;
        }
        let rwidth = if o.tuples_only {
            0
        } else {
            t.rows.len().to_string().len() + match border { 0 => 9, 1 => 12, _ => 15 }
        };
        dwidth = loop {
            let width = (hwidth + swidth + dwidth).max(rwidth);
            let new_width = if output_columns > 0 {
                let min_width = (hwidth + swidth + 3).max(rwidth);
                if output_columns >= width {
                    width - hwidth - swidth
                } else if output_columns < min_width {
                    min_width - hwidth - swidth
                } else {
                    output_columns - hwidth - swidth
                }
            } else {
                width - hwidth - swidth
            };
            // Wrapping needs a column for its marks.
            if new_width < dwidth && !dmultiline && marks_column {
                dmultiline = true;
                swidth += 1;
            } else {
                break new_width;
            }
        };
    }

    for (r, row) in cells.iter().enumerate() {
        let rule = &format.lrule[if r == 0 { TOP } else { MIDDLE }];
        let lhwidth = hwidth + usize::from(border < 2 && old_header_marks);
        // The record line reads `\pset border` as set, not clamped — as
        // psql's does.
        if !o.tuples_only {
            vertical_line(Some(r + 1), lhwidth, dwidth, rule, o, out);
        } else if r != 0 || border == 2 {
            vertical_line(None, lhwidth, dwidth, rule, o, out);
        }
        for (header, data) in headers.iter().zip(row) {
            let (mut hline, mut dline, mut offset) = (0, 0, 0);
            let (mut hcomplete, mut dcomplete) = (false, false);
            let mut unprinted = data[0].width;
            while !dcomplete || !hcomplete {
                if border == 2 {
                    out.push_str(dformat.leftvrule);
                }
                if !hcomplete {
                    if border == 2 || old_header_marks {
                        out.push_str(if hline == 0 { " " } else { format.header_nl_left });
                    }
                    let text = &header[hline];
                    out.push_str(&text.text);
                    pad(out, hwidth.saturating_sub(text.width));
                    let marks = border > 0 || (hmultiline && !format.old_ascii);
                    if header.get(hline + 1).is_some() {
                        if marks {
                            out.push_str(format.header_nl_right);
                        }
                        hline += 1;
                    } else {
                        if marks {
                            out.push(' ');
                        }
                        hcomplete = true;
                    }
                } else {
                    let mut swidth = hwidth + usize::from(border);
                    if border < 2 && old_header_marks {
                        swidth += 1;
                    }
                    if border == 0 && hmultiline && !format.old_ascii {
                        swidth += 1;
                    }
                    // `%*s` of a space: one at least.
                    pad(out, swidth.max(1));
                }
                if border > 0 {
                    out.push_str(if offset != 0 {
                        format.midvrule_wrap
                    } else if dline == 0 {
                        dformat.midvrule
                    } else {
                        format.midvrule_nl
                    });
                }
                if dcomplete {
                    // The value's done; the header goes on.
                    if border == 2 {
                        pad(out, dwidth + 2);
                        out.push_str(dformat.rightvrule);
                    }
                    out.push('\n');
                    continue;
                }
                out.push_str(if offset == 0 { " " } else { format.wrap_left });
                let rest = &data[dline].text[offset..];
                let (bytes, width) = strlen_max_width(rest, dwidth);
                out.push_str(&rest[..bytes]);
                unprinted = unprinted.saturating_sub(width);
                offset += bytes;
                let spare = dwidth.saturating_sub(width);
                let marks = border > 1 || (dmultiline && !format.old_ascii);
                if unprinted > 0 {
                    if marks {
                        pad(out, spare);
                        out.push_str(format.wrap_right);
                    }
                } else if data.get(dline + 1).is_some() {
                    if marks {
                        pad(out, spare);
                        out.push_str(format.nl_right);
                    }
                    dline += 1;
                    offset = 0;
                    unprinted = data[dline].width;
                } else {
                    if border > 1 {
                        pad(out, spare);
                        out.push(' ');
                    }
                    dcomplete = true;
                }
                if border == 2 {
                    out.push_str(dformat.rightvrule);
                }
                out.push('\n');
            }
        }
    }

    if border == 2 {
        vertical_line(None, hwidth, dwidth, &format.lrule[BOTTOM], o, out);
    }
    if !o.tuples_only && !t.footers.is_empty() {
        if border < 2 {
            out.push('\n');
        }
        for footer in &t.footers {
            out.push_str(footer);
            out.push('\n');
        }
    }
    out.push('\n');
}

/// psql's line over an expanded record, or under the last
/// (`print_aligned_vertical_line`): under border 0 `* Record 3` and
/// spaces; else `[ RECORD 3 ]` in the rule, where the column line meets
/// it if the label leaves room; under `\t`, no label. As far as
/// `\pset xheader_width` says: the data's full width, to the column
/// line (`column`), or no further than the page's width (`page`:
/// `\pset columns` or the pane's) or so many characters allow. The
/// `\pset border` as set, as psql reads it here: past 2 the line has
/// no ends.
fn vertical_line(record: Option<usize>, hwidth: usize, dwidth: usize, rule: &RuleFormat, o: &PrintOptions, out: &mut String) {
    let border = o.text.rules.border;
    let xheader = o.text.rules.xheader;
    if border == 2 {
        out.push_str(rule.leftvrule);
        out.push_str(rule.hrule);
    } else if border == 1 {
        out.push_str(rule.hrule);
    }
    let fill = if border > 0 { rule.hrule } else { " " };
    let mut reclen: isize = 0;
    if let Some(n) = record {
        let label = if border == 0 { format!("* Record {n}") } else { format!("[ RECORD {n} ]") };
        reclen = label.len() as isize;
        out.push_str(&label);
    }
    if border != 2 {
        reclen += 1;
    }
    let hwidth = hwidth as isize;
    for _ in reclen..hwidth {
        out.push_str(fill);
    }
    reclen -= hwidth;
    // Where the column line meets it — its end, under `column`.
    let marks: &[&str] = match (border, xheader) {
        (0, _) => &[" "],
        (_, XHeader::Column) => &[rule.hrule, rule.rightvrule],
        _ => &[rule.hrule, rule.midvrule, rule.hrule],
    };
    for mark in marks {
        if reclen <= 0 {
            out.push_str(mark);
        }
        reclen -= 1;
    }
    if xheader != XHeader::Column {
        let output_columns = match xheader {
            XHeader::Exact(width) => isize::try_from(width).unwrap_or(0),
            XHeader::Page => o.columns.map_or(0, |c| c as isize),
            _ => 0,
        };
        let mut dwidth = dwidth as isize;
        if matches!(xheader, XHeader::Page | XHeader::Exact(_)) && output_columns > 0 {
            let room = output_columns - hwidth - match border { 0 => 0, 1 => 3, _ => 7 };
            if border <= 2 {
                dwidth = dwidth.min(room.max(0));
            }
        }
        let reclen = reclen.max(0);
        for _ in reclen..dwidth.max(reclen) {
            out.push_str(fill);
        }
        if border == 2 {
            out.push_str(rule.hrule);
            out.push_str(rule.rightvrule);
        }
    }
    out.push('\n');
}

// ── Unaligned and csv ───────────────────────────────────────────────

/// The end of psql's unaligned output: a newline, whatever the record
/// separator — but a zero byte under `recordsep_zero`, for `xargs -0`.
fn end_unaligned(o: &PrintOptions, out: &mut String) {
    out.push(if o.text.recordsep == Separator::Zero { '\0' } else { '\n' });
}

/// `print_unaligned_text`: the title, the headers and each row a record
/// of its own, `\pset recordsep` between them (a newline) and `fieldsep`
/// between fields (`|`), then the footers.
fn unaligned_text(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let (fieldsep, recordsep) = (o.text.fieldsep.as_str(), o.text.recordsep.as_str());
    let mut need_recordsep = false;
    if !o.tuples_only {
        if let Some(title) = &t.title {
            out.push_str(title);
            out.push_str(recordsep);
        }
        out.push_str(&t.headers.join(fieldsep));
        need_recordsep = true;
    }
    let ncols = t.headers.len();
    for r in 0..t.rows.len() {
        for c in 0..ncols {
            if need_recordsep {
                out.push_str(recordsep);
                need_recordsep = false;
            }
            out.push_str(t.cell(r, c));
            if c + 1 < ncols {
                out.push_str(fieldsep);
            } else {
                need_recordsep = true;
            }
        }
    }
    if !o.tuples_only {
        for footer in t.footers_with_default(o) {
            if need_recordsep {
                out.push_str(recordsep);
            }
            out.push_str(&footer);
            need_recordsep = true;
        }
    }
    if need_recordsep {
        end_unaligned(o, out);
    }
}

/// `print_unaligned_vertical`: `header`, `fieldsep`, `value`, a record
/// each, two record separators between rows; the table's own footers,
/// no row count.
fn unaligned_vertical(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let (fieldsep, recordsep) = (o.text.fieldsep.as_str(), o.text.recordsep.as_str());
    let mut need_recordsep = false;
    if !o.tuples_only {
        if let Some(title) = &t.title {
            out.push_str(title);
            need_recordsep = true;
        }
    }
    let ncols = t.headers.len();
    for r in 0..t.rows.len() {
        for c in 0..ncols {
            if need_recordsep {
                out.push_str(recordsep);
                out.push_str(recordsep);
                need_recordsep = false;
            }
            out.push_str(&t.headers[c]);
            out.push_str(fieldsep);
            out.push_str(t.cell(r, c));
            if c + 1 < ncols {
                out.push_str(recordsep);
            } else {
                need_recordsep = true;
            }
        }
    }
    if !o.tuples_only && !t.footers.is_empty() {
        out.push_str(recordsep);
        for footer in &t.footers {
            out.push_str(recordsep);
            out.push_str(footer);
        }
    }
    if need_recordsep {
        end_unaligned(o, out);
    }
}

/// A csv field as psql writes it (`csv_print_field`): quoted, quotes
/// doubled, when it holds the separator, a quote or a line break — or is
/// `\.`, or the separator is `\` or `.`: what COPY could read as the end
/// of the data.
fn csv_field(text: &str, sep: char, out: &mut String) {
    if text.contains(sep) || text.contains(['"', '\r', '\n']) || text == "\\." || matches!(sep, '\\' | '.') {
        out.push('"');
        out.push_str(&text.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(text);
    }
}

/// `print_csv_text`: the headers (but under `\t`) and the rows, fields
/// `\pset csv_fieldsep` apart (`,`) — never a title or footers.
fn csv_text(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let sep = o.text.csv_fieldsep;
    let ncols = t.headers.len();
    if !o.tuples_only {
        for (c, header) in t.headers.iter().enumerate() {
            if c != 0 {
                out.push(sep);
            }
            csv_field(header, sep, out);
        }
        out.push('\n');
    }
    for r in 0..t.rows.len() {
        for c in 0..ncols {
            csv_field(t.cell(r, c), sep, out);
            out.push(if c + 1 < ncols { sep } else { '\n' });
        }
    }
}

/// `print_csv_vertical`: a `header,value` line for every cell.
fn csv_vertical(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let sep = o.text.csv_fieldsep;
    for r in 0..t.rows.len() {
        for (c, header) in t.headers.iter().enumerate() {
            csv_field(header, sep, out);
            out.push(sep);
            csv_field(t.cell(r, c), sep, out);
            out.push('\n');
        }
    }
}

// ── HTML ────────────────────────────────────────────────────────────

/// `html_escaped_print`: entities for `& < > "`, `<br />` for a newline,
/// `&nbsp;` for a leading space.
pub(super) fn html_escaped(text: &str, out: &mut String) {
    let mut leading_space = true;
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\n' => out.push_str("<br />\n"),
            '"' => out.push_str("&quot;"),
            ' ' if leading_space => out.push_str("&nbsp;"),
            ch => out.push(ch),
        }
        if ch != ' ' {
            leading_space = false;
        }
    }
}

/// Nothing but spaces and tabs, or nothing at all.
fn blank(text: &str) -> bool {
    text.chars().all(|c| c == ' ' || c == '\t')
}

fn html_cell(t: &PrintTable, r: usize, c: usize, out: &mut String) {
    out.push_str(if t.align(c) == Align::Right { "    <td align=\"right\">" } else { "    <td align=\"left\">" });
    let cell = t.cell(r, c);
    if blank(cell) {
        out.push_str("&nbsp; ");
    } else {
        html_escaped(cell, out);
    }
    out.push_str("</td>\n");
}

/// `<table border="1">` — `\pset border`, as it is — and `\pset
/// tableattr` in it when set.
fn html_table_start(o: &PrintOptions, out: &mut String) {
    out.push_str(&format!("<table border=\"{}\"", o.text.rules.border));
    if let Some(attrs) = &o.text.tableattr {
        out.push(' ');
        out.push_str(attrs);
    }
    out.push_str(">\n");
}

fn html_footers(footers: &[String], out: &mut String) {
    if footers.is_empty() {
        return;
    }
    out.push_str("<p>");
    for footer in footers {
        html_escaped(footer, out);
        out.push_str("<br />\n");
    }
    out.push_str("</p>");
}

/// `print_html_text`: a `<table>` with the title as its caption.
fn html_text(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    html_table_start(o, out);
    if !o.tuples_only {
        if let Some(title) = &t.title {
            out.push_str("  <caption>");
            html_escaped(title, out);
            out.push_str("</caption>\n");
        }
        out.push_str("  <tr>\n");
        for header in &t.headers {
            out.push_str("    <th align=\"center\">");
            html_escaped(header, out);
            out.push_str("</th>\n");
        }
        out.push_str("  </tr>\n");
    }
    for r in 0..t.rows.len() {
        if t.headers.is_empty() {
            break;
        }
        out.push_str("  <tr valign=\"top\">\n");
        for c in 0..t.headers.len() {
            html_cell(t, r, c, out);
        }
        out.push_str("  </tr>\n");
    }
    out.push_str("</table>\n");
    if !o.tuples_only {
        html_footers(&t.footers_with_default(o), out);
    }
    out.push('\n');
}

/// `print_html_vertical`: a `Record n` row over each record's
/// header / value rows.
fn html_vertical(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    html_table_start(o, out);
    if !o.tuples_only {
        if let Some(title) = &t.title {
            out.push_str("  <caption>");
            html_escaped(title, out);
            out.push_str("</caption>\n");
        }
    }
    for r in 0..t.rows.len() {
        if t.headers.is_empty() {
            break;
        }
        if !o.tuples_only {
            out.push_str(&format!("\n  <tr><td colspan=\"2\" align=\"center\">Record {}</td></tr>\n", r + 1));
        } else {
            out.push_str("\n  <tr><td colspan=\"2\">&nbsp;</td></tr>\n");
        }
        for (c, header) in t.headers.iter().enumerate() {
            out.push_str("  <tr valign=\"top\">\n    <th>");
            html_escaped(header, out);
            out.push_str("</th>\n");
            // The cell, and the row's end on its line.
            html_cell(t, r, c, out);
            out.push_str("  </tr>\n");
        }
    }
    out.push_str("</table>\n");
    if !o.tuples_only {
        html_footers(&t.footers, out);
    }
    out.push('\n');
}

// ── AsciiDoc ────────────────────────────────────────────────────────

/// `asciidoc_escaped_print`: `|` as `\|`.
fn asciidoc_escaped(text: &str, out: &mut String) {
    out.push_str(&text.replace('|', "\\|"));
}

fn asciidoc_footers(footers: &[String], out: &mut String) {
    if footers.is_empty() {
        return;
    }
    out.push_str("\n....\n");
    for footer in footers {
        out.push_str(footer);
        out.push('\n');
    }
    out.push_str("....\n");
}

/// The table's frame and grid as `\pset border` draws them — none
/// past 2 — closing its attributes and opening it.
fn asciidoc_frame(o: &PrintOptions, out: &mut String) {
    out.push_str(match o.text.rules.border {
        0 => ",frame=\"none\",grid=\"none\"",
        1 => ",frame=\"none\"",
        2 => ",frame=\"all\",grid=\"all\"",
        _ => "",
    });
    out.push_str("]\n|====\n");
}

/// `print_asciidoc_text`: a `|====` table, in a paragraph of its own.
fn asciidoc_text(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let ncols = t.headers.len();
    out.push('\n');
    if !o.tuples_only {
        if let Some(title) = &t.title {
            out.push('.');
            out.push_str(title);
            out.push('\n');
        }
    }
    out.push_str(if o.tuples_only { "[cols=\"" } else { "[options=\"header\",cols=\"" });
    for c in 0..ncols {
        if c != 0 {
            out.push(',');
        }
        out.push_str(if t.align(c) == Align::Right { ">l" } else { "<l" });
    }
    out.push('"');
    asciidoc_frame(o, out);
    if !o.tuples_only {
        for (c, header) in t.headers.iter().enumerate() {
            if c != 0 {
                out.push(' ');
            }
            out.push_str("^l|");
            asciidoc_escaped(header, out);
        }
        out.push('\n');
    }
    for r in 0..t.rows.len() {
        for c in 0..ncols {
            if c != 0 {
                out.push(' ');
            }
            out.push('|');
            let cell = t.cell(r, c);
            if blank(cell) {
                if c + 1 != ncols {
                    out.push(' ');
                }
            } else {
                asciidoc_escaped(cell, out);
            }
            if c + 1 == ncols {
                out.push('\n');
            }
        }
    }
    out.push_str("|====\n");
    if !o.tuples_only {
        asciidoc_footers(&t.footers_with_default(o), out);
    }
}

/// `print_asciidoc_vertical`: a `Record n` row over each record's
/// header / value rows.
fn asciidoc_vertical(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    out.push('\n');
    if !o.tuples_only {
        if let Some(title) = &t.title {
            out.push('.');
            out.push_str(title);
            out.push('\n');
        }
    }
    out.push_str("[cols=\"h,l\"");
    asciidoc_frame(o, out);
    for r in 0..t.rows.len() {
        if t.headers.is_empty() {
            break;
        }
        if !o.tuples_only {
            out.push_str(&format!("2+^|Record {}\n", r + 1));
        } else {
            out.push_str("2+|\n");
        }
        for (c, header) in t.headers.iter().enumerate() {
            out.push_str("<l|");
            asciidoc_escaped(header, out);
            out.push_str(if t.align(c) == Align::Right { " >l|" } else { " <l|" });
            let cell = t.cell(r, c);
            if blank(cell) {
                out.push(' ');
            } else {
                asciidoc_escaped(cell, out);
            }
            out.push('\n');
        }
    }
    out.push_str("|====\n");
    if !o.tuples_only {
        asciidoc_footers(&t.footers, out);
    }
}

// ── LaTeX ───────────────────────────────────────────────────────────

/// `latex_escaped_print`: LaTeX's special characters as it writes them
/// (Pakin's symbol list), a newline as `\\`.
fn latex_escaped(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '#' => out.push_str("\\#"),
            '$' => out.push_str("\\$"),
            '%' => out.push_str("\\%"),
            '&' => out.push_str("\\&"),
            '<' => out.push_str("\\textless{}"),
            '>' => out.push_str("\\textgreater{}"),
            '\\' => out.push_str("\\textbackslash{}"),
            '^' => out.push_str("\\^{}"),
            '_' => out.push_str("\\_"),
            '{' => out.push_str("\\{"),
            '|' => out.push_str("\\textbar{}"),
            '}' => out.push_str("\\}"),
            '~' => out.push_str("\\~{}"),
            '\n' => out.push_str("\\\\"),
            ch => out.push(ch),
        }
    }
}

fn latex_title(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    if !o.tuples_only {
        if let Some(title) = &t.title {
            out.push_str("\\begin{center}\n");
            latex_escaped(title, out);
            out.push_str("\n\\end{center}\n\n");
        }
    }
}

/// The column spec of a `tabular` or `longtable`: each column's, with
/// lines between them but under border 0, and at the edges from border
/// 2 on.
fn latex_columns(columns: &[String], border: u16, out: &mut String) {
    if border >= 2 {
        out.push_str("| ");
    }
    for (c, column) in columns.iter().enumerate() {
        out.push_str(column);
        if border != 0 && c + 1 < columns.len() {
            out.push_str(" | ");
        }
    }
    if border >= 2 {
        out.push_str(" |");
    }
}

fn latex_footers(footers: &[String], out: &mut String) {
    for footer in footers {
        latex_escaped(footer, out);
        out.push_str(" \\\\\n");
    }
}

/// `print_latex_text`: a `tabular` environment, the title centred over
/// it, the footers after; `\pset border` 2 frames it, 3 rules each row.
fn latex_text(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let ncols = t.headers.len();
    let border = o.text.rules.border.min(3);
    latex_title(t, o, out);
    out.push_str("\\begin{tabular}{");
    let columns: Vec<String> = (0..ncols).map(|c| letter(t.align(c)).to_string()).collect();
    latex_columns(&columns, border, out);
    out.push_str("}\n");
    if !o.tuples_only && border >= 2 {
        out.push_str("\\hline\n");
    }
    if !o.tuples_only {
        for (c, header) in t.headers.iter().enumerate() {
            if c != 0 {
                out.push_str(" & ");
            }
            out.push_str("\\textit{");
            latex_escaped(header, out);
            out.push('}');
        }
        out.push_str(" \\\\\n\\hline\n");
    }
    for r in 0..t.rows.len() {
        for c in 0..ncols {
            latex_escaped(t.cell(r, c), out);
            if c + 1 < ncols {
                out.push_str(" & ");
            } else {
                out.push_str(" \\\\\n");
                if border == 3 {
                    out.push_str("\\hline\n");
                }
            }
        }
    }
    if border == 2 {
        out.push_str("\\hline\n");
    }
    out.push_str("\\end{tabular}\n\n\\noindent ");
    if !o.tuples_only {
        latex_footers(&t.footers_with_default(o), out);
    }
    out.push('\n');
}

/// `print_latex_longtable_text`: a `longtable`, its header repeated on
/// each page, the title as its caption; no footers. `\pset border` 2
/// frames it with book rules, 3 rules each row.
fn latex_longtable_text(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let ncols = t.headers.len();
    let border = o.text.rules.border.min(3);
    out.push_str("\\begin{longtable}{");
    // A left-aligned column takes the next of `\pset tableattr`'s widths
    // (fractions of the text width) — the last one again once they run
    // out; without them, `l`.
    let widths: Vec<&str> = o.text.tableattr.as_deref().map(|a| a.split([' ', '\t', '\n']).filter(|w| !w.is_empty()).collect())
        .unwrap_or_default();
    let mut next = widths.iter();
    let mut last: Option<&str> = None;
    let mut columns = Vec::new();
    for c in 0..ncols {
        columns.push(match t.align(c) {
            Align::Left if o.text.tableattr.is_some() => match next.next().copied().or(last) {
                Some(width) => {
                    last = Some(width);
                    format!("p{{{width}\\textwidth}}")
                }
                None => "l".into(),
            },
            align => letter(align).to_string(),
        });
    }
    latex_columns(&columns, border, out);
    out.push_str("}\n");
    if !o.tuples_only {
        let head = |out: &mut String| {
            if border >= 2 {
                out.push_str("\\toprule\n");
            }
            for (c, header) in t.headers.iter().enumerate() {
                if c != 0 {
                    out.push_str(" & ");
                }
                out.push_str("\\small\\textbf{\\textit{");
                latex_escaped(header, out);
                out.push_str("}}");
            }
            out.push_str(" \\\\\n");
        };
        head(out);
        out.push_str("\\midrule\n\\endfirsthead\n");
        head(out);
        // Under border 3 each row's rule is under the header already.
        if border != 3 {
            out.push_str("\\midrule\n");
        }
        out.push_str("\\endhead\n");
        if let Some(title) = &t.title {
            if border == 2 {
                out.push_str("\\bottomrule\n");
            }
            out.push_str("\\caption[");
            latex_escaped(title, out);
            out.push_str(" (Continued)]{");
            latex_escaped(title, out);
            out.push_str("}\n\\endfoot\n");
            if border == 2 {
                out.push_str("\\bottomrule\n");
            }
            out.push_str("\\caption[");
            latex_escaped(title, out);
            out.push_str("]{");
            latex_escaped(title, out);
            out.push_str("}\n\\endlastfoot\n");
        } else if border >= 2 {
            out.push_str("\\bottomrule\n\\endfoot\n\\bottomrule\n\\endlastfoot\n");
        }
    }
    for r in 0..t.rows.len() {
        for c in 0..ncols {
            if c != 0 {
                out.push_str("\n&\n");
            }
            out.push_str("\\raggedright{");
            latex_escaped(t.cell(r, c), out);
            out.push('}');
            if c + 1 == ncols {
                out.push_str(" \\tabularnewline\n");
                if border == 3 {
                    out.push_str(" \\hline\n");
                }
            }
        }
    }
    out.push_str("\\end{longtable}\n");
}

/// `print_latex_vertical`: a two-column `tabular`, a `Record n` row and
/// a rule over each record; `\pset border` 0 draws no rules, 2 frames it.
fn latex_vertical(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let border = o.text.rules.border.min(2);
    latex_title(t, o, out);
    out.push_str(match border {
        0 => "\\begin{tabular}{cl}\n",
        1 => "\\begin{tabular}{c|l}\n",
        _ => "\\begin{tabular}{|c|l|}\n",
    });
    for r in 0..t.rows.len() {
        if t.headers.is_empty() {
            break;
        }
        if !o.tuples_only {
            if border == 2 {
                out.push_str(&format!("\\hline\n\\multicolumn{{2}}{{|c|}}{{\\textit{{Record {}}}}} \\\\\n", r + 1));
            } else {
                out.push_str(&format!("\\multicolumn{{2}}{{c}}{{\\textit{{Record {}}}}} \\\\\n", r + 1));
            }
        }
        if border >= 1 {
            out.push_str("\\hline\n");
        }
        for (c, header) in t.headers.iter().enumerate() {
            latex_escaped(header, out);
            out.push_str(" & ");
            latex_escaped(t.cell(r, c), out);
            out.push_str(" \\\\\n");
        }
    }
    if border == 2 {
        out.push_str("\\hline\n");
    }
    out.push_str("\\end{tabular}\n\n\\noindent ");
    if !o.tuples_only {
        latex_footers(&t.footers, out);
    }
    out.push('\n');
}

// ── troff -ms ───────────────────────────────────────────────────────

/// `troff_ms_escaped_print`: a backslash as `\(rs`.
fn troff_ms_escaped(text: &str, out: &mut String) {
    out.push_str(&text.replace('\\', "\\(rs"));
}

/// The title in a display of its own, then the table's start — boxed
/// under `\pset border` 2.
fn troff_ms_title(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    if !o.tuples_only {
        if let Some(title) = &t.title {
            out.push_str(".LP\n.DS C\n");
            troff_ms_escaped(title, out);
            out.push_str("\n.DE\n");
        }
    }
    out.push_str(if o.text.rules.border >= 2 { ".LP\n.TS\ncenter box;\n" } else { ".LP\n.TS\ncenter;\n" });
}

fn troff_ms_footers(footers: &[String], out: &mut String) {
    out.push_str(".TE\n.DS L\n");
    for footer in footers {
        troff_ms_escaped(footer, out);
        out.push('\n');
    }
    out.push_str(".DE\n");
}

/// `print_troff_ms_text`: a `tbl` table, cells a tab apart, lines
/// between columns but under border 0, the title and the footers in
/// displays around it.
fn troff_ms_text(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    let ncols = t.headers.len();
    troff_ms_title(t, o, out);
    for c in 0..ncols {
        out.push(letter(t.align(c)));
        if o.text.rules.border > 0 && c + 1 < ncols {
            out.push_str(" | ");
        }
    }
    out.push_str(".\n");
    if !o.tuples_only {
        for (c, header) in t.headers.iter().enumerate() {
            if c != 0 {
                out.push('\t');
            }
            out.push_str("\\fI");
            troff_ms_escaped(header, out);
            out.push_str("\\fP");
        }
        out.push_str("\n_\n");
    }
    for r in 0..t.rows.len() {
        for c in 0..ncols {
            troff_ms_escaped(t.cell(r, c), out);
            out.push(if c + 1 == ncols { '\n' } else { '\t' });
        }
    }
    troff_ms_footers(&if o.tuples_only { Vec::new() } else { t.footers_with_default(o) }, out);
}

/// `print_troff_ms_vertical`: a `Record n` row over each record's
/// header / value rows, `.T&` where the row format changes; a rule over
/// each record's rows but under border 0, between records under 2.
fn troff_ms_vertical(t: &PrintTable, o: &PrintOptions, out: &mut String) {
    #[derive(PartialEq)]
    enum Rows { None, Record, Body }
    let border = o.text.rules.border.min(2);
    troff_ms_title(t, o, out);
    if o.tuples_only {
        out.push_str("c l;\n");
    }
    let mut current = Rows::None;
    for r in 0..t.rows.len() {
        if t.headers.is_empty() {
            break;
        }
        if !o.tuples_only {
            if current != Rows::Record {
                if border == 2 && r > 0 {
                    out.push_str("_\n");
                }
                if current != Rows::None {
                    out.push_str(".T&\n");
                }
                out.push_str("c s.\n");
                current = Rows::Record;
            }
            out.push_str(&format!("\\fIRecord {}\\fP\n", r + 1));
        }
        if border >= 1 {
            out.push_str("_\n");
        }
        for (c, header) in t.headers.iter().enumerate() {
            if !o.tuples_only && current != Rows::Body {
                if current != Rows::None {
                    out.push_str(".T&\n");
                }
                out.push_str(if border == 1 { "c | l.\n" } else { "c l.\n" });
                current = Rows::Body;
            }
            troff_ms_escaped(header, out);
            out.push('\t');
            troff_ms_escaped(t.cell(r, c), out);
            out.push('\n');
        }
    }
    troff_ms_footers(&if o.tuples_only { Vec::new() } else { t.footers.clone() }, out);
}

// ── Text as psql lays it out ────────────────────────────────────────

/// One display line of a cell, and its width in terminal columns.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    text:  String,
    width: usize,
}

/// How wide psql's aligned printer would make a result under `\pset
/// border` (`width_total`): each column as wide as its widest header or
/// cell line, laid out as psql lays text out, and the border's lines and
/// spaces.
pub(super) fn aligned_width(headers: &[String], rows: &[Vec<String>], border: u16) -> usize {
    let widths = headers.iter().enumerate().map(|(i, header)| {
        let cells = rows.iter().filter_map(|row| row.get(i));
        std::iter::once(header).chain(cells).map(|text| max_width(&format_text(text))).max().unwrap_or(0)
    });
    rules_width(headers.len(), border.min(2)) + widths.sum::<usize>()
}

fn max_width(lines: &[Line]) -> usize {
    lines.iter().map(|l| l.width).max().unwrap_or(0)
}

/// How much of `text` fits `target` columns (`strlen_max_width`): its
/// length in bytes and its width — always at least one character, even
/// one wider than that.
fn strlen_max_width(text: &str, target: usize) -> (usize, usize) {
    let (mut bytes, mut width) = (0, 0);
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if target < width + ch_width && width != 0 {
            break;
        }
        width += ch_width;
        bytes += ch.len_utf8();
    }
    (bytes, width)
}

/// Text as psql lays it out (mbprint.c `pg_wcsformat`): split at
/// newlines, tabs expanded to the next multiple of 8, `\r` and other
/// control characters escaped (`\r`, `\x01`, `\u0085`), wide characters
/// two columns.
fn format_text(text: &str) -> Vec<Line> {
    let mut lines = vec![Line { text: String::new(), width: 0 }];
    for c in text.chars() {
        if c == '\n' {
            lines.push(Line { text: String::new(), width: 0 });
            continue;
        }
        let line = lines.last_mut().expect("never empty");
        match c {
            '\r' => { line.text.push_str("\\r"); line.width += 2; }
            '\t' => loop {
                line.text.push(' ');
                line.width += 1;
                if line.width % 8 == 0 { break; }
            },
            c if c.is_control() && c.is_ascii() => {
                line.text.push_str(&format!("\\x{:02X}", c as u32));
                line.width += 4;
            }
            c if c.is_control() => {
                line.text.push_str(&format!("\\u{:04X}", c as u32));
                line.width += 6;
            }
            c => {
                line.text.push(c);
                line.width += c.width().unwrap_or(0);
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(headers: &[&str], rows: &[&[&str]]) -> PrintTable {
        PrintTable {
            headers: headers.iter().map(|h| h.to_string()).collect(),
            rows:    rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect(),
            ..PrintTable::default()
        }
    }

    fn opts(format: OutputFormat, expanded: Expanded) -> PrintOptions {
        PrintOptions { format, expanded, ..PrintOptions::default() }
    }

    fn aligned(t: &PrintTable, tuples_only: bool) -> Vec<String> {
        print_table(t, &PrintOptions { tuples_only, ..PrintOptions::default() })
    }

    fn expanded(t: &PrintTable, tuples_only: bool) -> Vec<String> {
        print_table(t, &PrintOptions { tuples_only, ..opts(OutputFormat::Aligned, Expanded::On) })
    }

    /// Each expected block was captured from psql 18 (`psql -X`).
    #[test]
    fn prints_what_psql_prints() {
        let t = PrintTable {
            title:   Some("Table \"public.t\"".into()),
            count_rows: true,
            ..table(&["Column", "Type", "Nullable"], &[&["film_id", "integer", "not null"], &["description", "text", ""]])
        };
        assert_eq!(aligned(&t, false), [
            "         Table \"public.t\"",
            "   Column    |  Type   | Nullable ",
            "-------------+---------+----------",
            " film_id     | integer | not null",
            " description | text    | ",
            "(2 rows)",
            "",
        ]);

        let t = PrintTable { count_rows: true, ..table(&["a", "b", "c"], &[&["line1\nline22", "x", "p\nq\nr"]]) };
        assert_eq!(aligned(&t, false), [
            "   a    | b | c ",
            "--------+---+---",
            " line1 +| x | p+",
            " line22 |   | q+",
            "        |   | r",
            "(1 row)",
            "",
        ]);

        let t = PrintTable {
            aligns:  vec![Align::Right, Align::Left, Align::Right],
            count_rows: true,
            ..table(&["n", "t", "m"], &[&["1", "abc", "12345"], &["22", "de", "3"]])
        };
        assert_eq!(aligned(&t, false), [
            " n  |  t  |   m   ",
            "----+-----+-------",
            "  1 | abc | 12345",
            " 22 | de  |     3",
            "(2 rows)",
            "",
        ]);

        let t = PrintTable { count_rows: true, ..table(&["t", "c", "r"], &[&["a\tb", "x\u{1}y", "r\rs"]]) };
        assert_eq!(aligned(&t, false), [
            "     t     |   c    |  r   ",
            "-----------+--------+------",
            " a       b | x\\x01y | r\\rs",
            "(1 row)",
            "",
        ]);

        let t = PrintTable { count_rows: true, ..table(&["w", "v", "z"], &[&["日本語", "é", "x"]]) };
        assert_eq!(aligned(&t, false), [
            "   w    | v | z ",
            "--------+---+---",
            " 日本語 | é | x",
            "(1 row)",
            "",
        ]);

        // A title wider than the table isn't centred.
        let t = PrintTable {
            title:   Some("A much longer title than the table is wide".into()),
            count_rows: true,
            ..table(&["n"], &[&["1"]])
        };
        assert_eq!(aligned(&t, false)[..2], ["A much longer title than the table is wide", " n "]);
        // `\pset footer off`: no count.
        let t = PrintTable { count_rows: true, ..table(&["n"], &[&["1"]]) };
        assert_eq!(print_table(&t, &PrintOptions { footer: false, ..PrintOptions::default() }), [" n ", "---", " 1", ""]);
    }

    /// Each expected block was captured from psql 18.4 with `\x on`.
    #[test]
    fn prints_what_psql_prints_expanded() {
        // A multi-line value continues `+` at its own width; the record
        // line runs past the data when its label is wider.
        let t = table(&["a", "bb"], &[&["line1\nline22", "x"]]);
        assert_eq!(expanded(&t, false), ["-[ RECORD 1 ]", "a  | line1 +", "   | line22", "bb | x", ""]);
        let t = table(&["x"], &[&["a\nbbbbbbbbbbbbbbbbbbbb"]]);
        assert_eq!(expanded(&t, false), ["-[ RECORD 1 ]-----------", "x | a                   +", "  | bbbbbbbbbbbbbbbbbbbb", ""]);
        let t = table(&["a", "bb"], &[&["abc", "p\nq"]]);
        assert_eq!(expanded(&t, false), ["-[ RECORD 1 ]", "a  | abc", "bb | p  +", "   | q", ""]);
        // `+` where the columns meet, when the label leaves room; a
        // title on its own line; no row count.
        let t = PrintTable {
            title: Some("Objects in extension \"plpgsql\"".into()),
            count_rows: true,
            ..table(&["Object description"], &[&["function plpgsql_call_handler()"], &["function plpgsql_inline_handler(internal)"]])
        };
        assert_eq!(expanded(&t, false), [
            "Objects in extension \"plpgsql\"",
            "-[ RECORD 1 ]------+------------------------------------------",
            "Object description | function plpgsql_call_handler()",
            "-[ RECORD 2 ]------+------------------------------------------",
            "Object description | function plpgsql_inline_handler(internal)",
            "",
        ]);
        let t = table(&["averyveryverylongheadername"], &[&["1"]]);
        assert_eq!(expanded(&t, false)[0], "-[ RECORD 1 ]---------------+--");
        // No rows: the count alone.
        let t = PrintTable { count_rows: true, ..table(&["a"], &[]) };
        assert_eq!(expanded(&t, false), ["(0 rows)", ""]);
        // `\t`: a plain line between records, none before the first.
        let t = table(&["a", "b"], &[&["1", "2"], &["3", "4"]]);
        assert_eq!(expanded(&t, true), ["a | 1", "b | 2", "--+--", "a | 3", "b | 4", ""]);
        // A header on two lines (`select 1 as "a\nbb", 2 as c`); an empty
        // value keeps the space before it.
        let t = table(&["a\nbb", "c"], &[&["1", "2"]]);
        assert_eq!(expanded(&t, false), ["-[ RECORD 1 ]", "a +| 1", "bb |", "c  | 2", ""]);
        let t = table(&["x", "yy"], &[&["", "y"]]);
        assert_eq!(expanded(&t, false), ["-[ RECORD 1 ]", "x  | ", "yy | y", ""]);
        // A table's own footers follow its records after a blank line, and
        // stand in for the count (`\dRp+` of a publication of `actor`).
        let t = PrintTable {
            title: Some("Publication parliament_probe".into()),
            footers: vec!["Tables:".into(), "    \"public.actor\"".into()],
            count_rows: true,
            ..table(&["Owner", "All tables", "Inserts", "Updates", "Deletes", "Truncates", "Generated columns", "Via root"],
                    &[&["postgres", "f", "t", "t", "t", "t", "none", "f"]])
        };
        assert_eq!(expanded(&t, false), [
            "Publication parliament_probe",
            "-[ RECORD 1 ]-----+---------",
            "Owner             | postgres",
            "All tables        | f",
            "Inserts           | t",
            "Updates           | t",
            "Deletes           | t",
            "Truncates         | t",
            "Generated columns | none",
            "Via root          | f",
            "",
            "Tables:",
            "    \"public.actor\"",
            "",
        ]);
    }

    #[test]
    fn x_picks_the_expanded_printer() {
        let t = PrintTable { count_rows: true, ..table(&["Parameter", "Value"], &[&["Database", "pagila"]]) };
        let options = |expanded, columns| PrintOptions { expanded, columns, ..PrintOptions::default() };
        assert_eq!(print_table(&t, &options(Expanded::Off, None)), aligned(&t, false));
        assert_eq!(print_table(&t, &options(Expanded::On, None)), expanded(&t, false));
        // `auto`: expanded only when the aligned table is wider than the
        // pane (it's 20 wide); not measured, never.
        assert_eq!(print_table(&t, &options(Expanded::Auto, Some(19))), expanded(&t, false));
        assert_eq!(print_table(&t, &options(Expanded::Auto, Some(20))), aligned(&t, false));
        assert_eq!(print_table(&t, &options(Expanded::Auto, None)), aligned(&t, false));
        // Never a single column (psql 18.4, `\pset columns 10`).
        let t = PrintTable { count_rows: true, ..table(&["a"], &[&["abcdefghijklmnopqrstuvwxyz"]]) };
        assert_eq!(print_table(&t, &options(Expanded::Auto, Some(10))), [
            "             a              ",
            "----------------------------",
            " abcdefghijklmnopqrstuvwxyz",
            "(1 row)",
            "",
        ]);
    }

    #[test]
    fn tuples_only_prints_just_the_rows() {
        let t = PrintTable { title: Some("T".into()), count_rows: true, ..table(&["x", "y"], &[&["a", "b"]]) };
        assert_eq!(aligned(&t, true), [" a | b", ""]);
    }

    #[test]
    fn footers_may_span_lines() {
        let t = PrintTable {
            footers: vec!["View definition:".into(), " SELECT 1\n   FROM t;".into()],
            ..table(&["a"], &[])
        };
        assert_eq!(aligned(&t, false), [" a ", "---", "View definition:", " SELECT 1", "   FROM t;", ""]);
    }

    #[test]
    fn numbers_line_up_right() {
        assert_eq!(type_alignment("23"), Align::Right); // int4
        assert_eq!(type_alignment("1700"), Align::Right); // numeric
        assert_eq!(type_alignment("28"), Align::Right); // xid
        assert_eq!(type_alignment("25"), Align::Left); // text
        assert_eq!(type_alignment("1007"), Align::Left); // int4[]
        assert_eq!(type_alignment(""), Align::Left);
    }

    /// psql 18.4's capture of
    /// `select 1 as n, 'a|b "c"' as t, null::text as z, E'line1\nline2' as m,
    ///  ' lead & <x>' as h, E'back\\slash_%$#{}~^' as l, 1.5::numeric as d
    ///  union all select 22, '', 'x', '', '  ', '', 2`
    /// under `\pset title 'My Title'`, its default null display ('').
    fn sample() -> PrintTable {
        let r = Align::Right;
        let l = Align::Left;
        PrintTable {
            title: Some("My Title".into()),
            aligns: vec![r, l, l, l, l, l, r],
            count_rows: true,
            ..table(&["n", "t", "z", "m", "h", "l", "d"], &[
                &["1", "a|b \"c\"", "", "line1\nline2", " lead & <x>", "back\\slash_%$#{}~^", "1.5"],
                &["22", "", "x", "", "  ", "", "2"],
            ])
        }
    }

    fn printed(format: OutputFormat, expanded: Expanded, tuples_only: bool) -> String {
        let lines = print_table(&sample(), &PrintOptions { tuples_only, ..opts(format, expanded) });
        lines.iter().map(|l| format!("{l}\n")).collect()
    }

    fn empty(format: OutputFormat, expanded: Expanded) -> String {
        let t = PrintTable { aligns: vec![Align::Right], count_rows: true, ..table(&["a"], &[]) };
        print_table(&t, &opts(format, expanded)).iter().map(|l| format!("{l}\n")).collect()
    }

    #[test]
    fn unaligned_is_psqls() {
        use OutputFormat::Unaligned;
        assert_eq!(printed(Unaligned, Expanded::Off, false),
            "My Title\nn|t|z|m|h|l|d\n1|a|b \"c\"||line1\nline2| lead & <x>|back\\slash_%$#{}~^|1.5\n22||x||  ||2\n(2 rows)\n");
        assert_eq!(printed(Unaligned, Expanded::On, false),
            "My Title\n\nn|1\nt|a|b \"c\"\nz|\nm|line1\nline2\nh| lead & <x>\nl|back\\slash_%$#{}~^\nd|1.5\n\n\
             n|22\nt|\nz|x\nm|\nh|  \nl|\nd|2\n");
        assert_eq!(printed(Unaligned, Expanded::Off, true),
            "1|a|b \"c\"||line1\nline2| lead & <x>|back\\slash_%$#{}~^|1.5\n22||x||  ||2\n");
        assert_eq!(empty(Unaligned, Expanded::Off), "a\n(0 rows)\n");
        assert_eq!(empty(Unaligned, Expanded::On), "");
        // `\x auto` is aligned's alone.
        assert_eq!(printed(Unaligned, Expanded::Auto, false), printed(Unaligned, Expanded::Off, false));
    }

    #[test]
    fn csv_is_psqls() {
        use OutputFormat::Csv;
        assert_eq!(printed(Csv, Expanded::Off, false),
            "n,t,z,m,h,l,d\n1,\"a|b \"\"c\"\"\",,\"line1\nline2\", lead & <x>,back\\slash_%$#{}~^,1.5\n22,,x,,  ,,2\n");
        assert_eq!(printed(Csv, Expanded::On, false),
            "n,1\nt,\"a|b \"\"c\"\"\"\nz,\nm,\"line1\nline2\"\nh, lead & <x>\nl,back\\slash_%$#{}~^\nd,1.5\n\
             n,22\nt,\nz,x\nm,\nh,  \nl,\nd,2\n");
        assert_eq!(printed(Csv, Expanded::Off, true),
            "1,\"a|b \"\"c\"\"\",,\"line1\nline2\", lead & <x>,back\\slash_%$#{}~^,1.5\n22,,x,,  ,,2\n");
        assert_eq!(empty(Csv, Expanded::Off), "a\n");
        assert_eq!(empty(Csv, Expanded::On), "");
        // A field that is `\.` is quoted, COPY's end of data.
        let t = table(&["a"], &[&["\\."]]);
        assert_eq!(print_table(&t, &opts(Csv, Expanded::Off)), ["a", "\"\\.\""]);
    }

    #[test]
    fn html_is_psqls() {
        use OutputFormat::Html;
        assert_eq!(printed(Html, Expanded::Off, false), concat!(
            "<table border=\"1\">\n",
            "  <caption>My Title</caption>\n",
            "  <tr>\n",
            "    <th align=\"center\">n</th>\n",
            "    <th align=\"center\">t</th>\n",
            "    <th align=\"center\">z</th>\n",
            "    <th align=\"center\">m</th>\n",
            "    <th align=\"center\">h</th>\n",
            "    <th align=\"center\">l</th>\n",
            "    <th align=\"center\">d</th>\n",
            "  </tr>\n",
            "  <tr valign=\"top\">\n",
            "    <td align=\"right\">1</td>\n",
            "    <td align=\"left\">a|b &quot;c&quot;</td>\n",
            "    <td align=\"left\">&nbsp; </td>\n",
            "    <td align=\"left\">line1<br />\n",
            "line2</td>\n",
            "    <td align=\"left\">&nbsp;lead &amp; &lt;x&gt;</td>\n",
            "    <td align=\"left\">back\\slash_%$#{}~^</td>\n",
            "    <td align=\"right\">1.5</td>\n",
            "  </tr>\n",
            "  <tr valign=\"top\">\n",
            "    <td align=\"right\">22</td>\n",
            "    <td align=\"left\">&nbsp; </td>\n",
            "    <td align=\"left\">x</td>\n",
            "    <td align=\"left\">&nbsp; </td>\n",
            "    <td align=\"left\">&nbsp; </td>\n",
            "    <td align=\"left\">&nbsp; </td>\n",
            "    <td align=\"right\">2</td>\n",
            "  </tr>\n",
            "</table>\n",
            "<p>(2 rows)<br />\n",
            "</p>\n",
        ));
        let on = printed(Html, Expanded::On, false);
        assert!(on.starts_with(concat!(
            "<table border=\"1\">\n",
            "  <caption>My Title</caption>\n",
            "\n",
            "  <tr><td colspan=\"2\" align=\"center\">Record 1</td></tr>\n",
            "  <tr valign=\"top\">\n",
            "    <th>n</th>\n",
            "    <td align=\"right\">1</td>\n",
            "  </tr>\n",
            "  <tr valign=\"top\">\n",
            "    <th>t</th>\n",
            "    <td align=\"left\">a|b &quot;c&quot;</td>\n",
            "  </tr>\n",
        )), "{on}");
        assert!(on.contains(concat!(
            "    <td align=\"right\">1.5</td>\n",
            "  </tr>\n",
            "\n",
            "  <tr><td colspan=\"2\" align=\"center\">Record 2</td></tr>\n",
        )), "{on}");
        assert!(on.ends_with("    <th>d</th>\n    <td align=\"right\">2</td>\n  </tr>\n</table>\n\n"), "{on}");
        assert!(printed(Html, Expanded::Off, true).starts_with("<table border=\"1\">\n  <tr valign=\"top\">\n    <td align=\"right\">1</td>\n"));
        assert!(printed(Html, Expanded::Off, true).ends_with("  </tr>\n</table>\n\n"));
        assert_eq!(empty(Html, Expanded::Off),
            "<table border=\"1\">\n  <tr>\n    <th align=\"center\">a</th>\n  </tr>\n</table>\n<p>(0 rows)<br />\n</p>\n");
        assert_eq!(empty(Html, Expanded::On), "<table border=\"1\">\n</table>\n\n");
    }

    #[test]
    fn asciidoc_is_psqls() {
        use OutputFormat::Asciidoc;
        assert_eq!(printed(Asciidoc, Expanded::Off, false), concat!(
            "\n",
            ".My Title\n",
            "[options=\"header\",cols=\">l,<l,<l,<l,<l,<l,>l\",frame=\"none\"]\n",
            "|====\n",
            "^l|n ^l|t ^l|z ^l|m ^l|h ^l|l ^l|d\n",
            "|1 |a\\|b \"c\" |  |line1\n",
            "line2 | lead & <x> |back\\slash_%$#{}~^ |1.5\n",
            "|22 |  |x |  |  |  |2\n",
            "|====\n",
            "\n",
            "....\n",
            "(2 rows)\n",
            "....\n",
        ));
        assert_eq!(printed(Asciidoc, Expanded::On, false), concat!(
            "\n",
            ".My Title\n",
            "[cols=\"h,l\",frame=\"none\"]\n",
            "|====\n",
            "2+^|Record 1\n",
            "<l|n >l|1\n",
            "<l|t <l|a\\|b \"c\"\n",
            "<l|z <l| \n",
            "<l|m <l|line1\n",
            "line2\n",
            "<l|h <l| lead & <x>\n",
            "<l|l <l|back\\slash_%$#{}~^\n",
            "<l|d >l|1.5\n",
            "2+^|Record 2\n",
            "<l|n >l|22\n",
            "<l|t <l| \n",
            "<l|z <l|x\n",
            "<l|m <l| \n",
            "<l|h <l| \n",
            "<l|l <l| \n",
            "<l|d >l|2\n",
            "|====\n",
        ));
        assert_eq!(printed(Asciidoc, Expanded::Off, true), concat!(
            "\n",
            "[cols=\">l,<l,<l,<l,<l,<l,>l\",frame=\"none\"]\n",
            "|====\n",
            "|1 |a\\|b \"c\" |  |line1\n",
            "line2 | lead & <x> |back\\slash_%$#{}~^ |1.5\n",
            "|22 |  |x |  |  |  |2\n",
            "|====\n",
        ));
        assert_eq!(empty(Asciidoc, Expanded::Off),
            "\n[options=\"header\",cols=\">l\",frame=\"none\"]\n|====\n^l|a\n|====\n\n....\n(0 rows)\n....\n");
        assert_eq!(empty(Asciidoc, Expanded::On), "\n[cols=\"h,l\",frame=\"none\"]\n|====\n|====\n");
    }

    #[test]
    fn latex_is_psqls() {
        use OutputFormat::{Latex, LatexLongtable};
        assert_eq!(printed(Latex, Expanded::Off, false), concat!(
            "\\begin{center}\n",
            "My Title\n",
            "\\end{center}\n",
            "\n",
            "\\begin{tabular}{r | l | l | l | l | l | r}\n",
            "\\textit{n} & \\textit{t} & \\textit{z} & \\textit{m} & \\textit{h} & \\textit{l} & \\textit{d} \\\\\n",
            "\\hline\n",
            "1 & a\\textbar{}b \"c\" &  & line1\\\\line2 &  lead \\& \\textless{}x\\textgreater{} & ",
            "back\\textbackslash{}slash\\_\\%\\$\\#\\{\\}\\~{}\\^{} & 1.5 \\\\\n",
            "22 &  & x &  &    &  & 2 \\\\\n",
            "\\end{tabular}\n",
            "\n",
            "\\noindent (2 rows) \\\\\n",
            "\n",
        ));
        let vertical = concat!(
            "\\begin{center}\n",
            "My Title\n",
            "\\end{center}\n",
            "\n",
            "\\begin{tabular}{c|l}\n",
            "\\multicolumn{2}{c}{\\textit{Record 1}} \\\\\n",
            "\\hline\n",
            "n & 1 \\\\\n",
            "t & a\\textbar{}b \"c\" \\\\\n",
            "z &  \\\\\n",
            "m & line1\\\\line2 \\\\\n",
            "h &  lead \\& \\textless{}x\\textgreater{} \\\\\n",
            "l & back\\textbackslash{}slash\\_\\%\\$\\#\\{\\}\\~{}\\^{} \\\\\n",
            "d & 1.5 \\\\\n",
            "\\multicolumn{2}{c}{\\textit{Record 2}} \\\\\n",
            "\\hline\n",
            "n & 22 \\\\\n",
            "t &  \\\\\n",
            "z & x \\\\\n",
            "m &  \\\\\n",
            "h &    \\\\\n",
            "l &  \\\\\n",
            "d & 2 \\\\\n",
            "\\end{tabular}\n",
            "\n",
            "\\noindent \n",
        );
        assert_eq!(printed(Latex, Expanded::On, false), vertical);
        assert_eq!(printed(LatexLongtable, Expanded::On, false), vertical);
        assert_eq!(printed(Latex, Expanded::Off, true), concat!(
            "\\begin{tabular}{r | l | l | l | l | l | r}\n",
            "1 & a\\textbar{}b \"c\" &  & line1\\\\line2 &  lead \\& \\textless{}x\\textgreater{} & ",
            "back\\textbackslash{}slash\\_\\%\\$\\#\\{\\}\\~{}\\^{} & 1.5 \\\\\n",
            "22 &  & x &  &    &  & 2 \\\\\n",
            "\\end{tabular}\n",
            "\n",
            "\\noindent \n",
        ));
        assert_eq!(empty(Latex, Expanded::Off), "\\begin{tabular}{r}\n\\textit{a} \\\\\n\\hline\n\\end{tabular}\n\n\\noindent (0 rows) \\\\\n\n");
        assert_eq!(empty(Latex, Expanded::On), "\\begin{tabular}{c|l}\n\\end{tabular}\n\n\\noindent \n");

        let head = "\\small\\textbf{\\textit{n}} & \\small\\textbf{\\textit{t}} & \\small\\textbf{\\textit{z}} & \\small\\textbf{\\textit{m}} & \
                    \\small\\textbf{\\textit{h}} & \\small\\textbf{\\textit{l}} & \\small\\textbf{\\textit{d}} \\\\\n";
        let cells = concat!(
            "\\raggedright{1}\n&\n\\raggedright{a\\textbar{}b \"c\"}\n&\n\\raggedright{}\n&\n\\raggedright{line1\\\\line2}\n&\n",
            "\\raggedright{ lead \\& \\textless{}x\\textgreater{}}\n&\n",
            "\\raggedright{back\\textbackslash{}slash\\_\\%\\$\\#\\{\\}\\~{}\\^{}}\n&\n\\raggedright{1.5} \\tabularnewline\n",
            "\\raggedright{22}\n&\n\\raggedright{}\n&\n\\raggedright{x}\n&\n\\raggedright{}\n&\n\\raggedright{  }\n&\n",
            "\\raggedright{}\n&\n\\raggedright{2} \\tabularnewline\n",
            "\\end{longtable}\n",
        );
        assert_eq!(printed(LatexLongtable, Expanded::Off, false), format!(
            "\\begin{{longtable}}{{r | l | l | l | l | l | r}}\n{head}\\midrule\n\\endfirsthead\n{head}\\midrule\n\\endhead\n\
             \\caption[My Title (Continued)]{{My Title}}\n\\endfoot\n\\caption[My Title]{{My Title}}\n\\endlastfoot\n{cells}"
        ));
        assert_eq!(printed(LatexLongtable, Expanded::Off, true), format!("\\begin{{longtable}}{{r | l | l | l | l | l | r}}\n{cells}"));
        assert_eq!(empty(LatexLongtable, Expanded::Off), concat!(
            "\\begin{longtable}{r}\n\\small\\textbf{\\textit{a}} \\\\\n\\midrule\n\\endfirsthead\n",
            "\\small\\textbf{\\textit{a}} \\\\\n\\midrule\n\\endhead\n\\end{longtable}\n",
        ));
    }

    #[test]
    fn troff_ms_is_psqls() {
        use OutputFormat::TroffMs;
        assert_eq!(printed(TroffMs, Expanded::Off, false), concat!(
            ".LP\n.DS C\nMy Title\n.DE\n",
            ".LP\n.TS\ncenter;\n",
            "r | l | l | l | l | l | r.\n",
            "\\fIn\\fP\t\\fIt\\fP\t\\fIz\\fP\t\\fIm\\fP\t\\fIh\\fP\t\\fIl\\fP\t\\fId\\fP\n",
            "_\n",
            "1\ta|b \"c\"\t\tline1\nline2\t lead & <x>\tback\\(rsslash_%$#{}~^\t1.5\n",
            "22\t\tx\t\t  \t\t2\n",
            ".TE\n.DS L\n(2 rows)\n.DE\n",
        ));
        assert_eq!(printed(TroffMs, Expanded::On, false), concat!(
            ".LP\n.DS C\nMy Title\n.DE\n",
            ".LP\n.TS\ncenter;\n",
            "c s.\n\\fIRecord 1\\fP\n_\n.T&\nc | l.\n",
            "n\t1\nt\ta|b \"c\"\nz\t\nm\tline1\nline2\nh\t lead & <x>\nl\tback\\(rsslash_%$#{}~^\nd\t1.5\n",
            ".T&\nc s.\n\\fIRecord 2\\fP\n_\n.T&\nc | l.\n",
            "n\t22\nt\t\nz\tx\nm\t\nh\t  \nl\t\nd\t2\n",
            ".TE\n.DS L\n.DE\n",
        ));
        assert_eq!(printed(TroffMs, Expanded::Off, true), concat!(
            ".LP\n.TS\ncenter;\n",
            "r | l | l | l | l | l | r.\n",
            "1\ta|b \"c\"\t\tline1\nline2\t lead & <x>\tback\\(rsslash_%$#{}~^\t1.5\n",
            "22\t\tx\t\t  \t\t2\n",
            ".TE\n.DS L\n.DE\n",
        ));
        assert_eq!(empty(TroffMs, Expanded::Off), ".LP\n.TS\ncenter;\nr.\n\\fIa\\fP\n_\n.TE\n.DS L\n(0 rows)\n.DE\n");
        assert_eq!(empty(TroffMs, Expanded::On), ".LP\n.TS\ncenter;\n.TE\n.DS L\n.DE\n");
    }

    /// psql 18.4 under `\pset fieldsep ';'`, `recordsep '#'` and the
    /// `_zero` forms, of `select 1 as n, 'a;b' as t, null::text as z
    /// union all select 2, 'c', 'd'`.
    #[test]
    fn unaligned_separators_are_psqls() {
        use crate::types::TextOptions;
        let t = PrintTable {
            count_rows: true,
            aligns: vec![Align::Right],
            ..table(&["n", "t", "z"], &[&["1", "a;b", ""], &["2", "c", "d"]])
        };
        let with = |fieldsep: Separator, recordsep: Separator, expanded, tuples_only| {
            let text = TextOptions { fieldsep, recordsep, ..TextOptions::default() };
            let o = PrintOptions { format: OutputFormat::Unaligned, expanded, tuples_only, text, ..PrintOptions::default() };
            print_table(&t, &o).join("\n")
        };
        let semi = || Separator::Text(";".into());
        let hash = || Separator::Text("#".into());
        assert_eq!(with(semi(), hash(), Expanded::Off, false), "n;t;z#1;a;b;#2;c;d#(2 rows)");
        assert_eq!(with(semi(), hash(), Expanded::On, false), "n;1#t;a;b#z;##n;2#t;c#z;d");
        // Zero bytes, and a zero byte to end.
        assert_eq!(with(Separator::Zero, Separator::Zero, Expanded::Off, false), "n\0t\0z\x001\0a;b\0\x002\0c\0d\0(2 rows)\0");
        assert_eq!(with(Separator::Zero, Separator::Zero, Expanded::Off, true), "1\0a;b\0\x002\0c\0d\0");
        assert_eq!(with(Separator::Zero, Separator::Zero, Expanded::On, false), "n\x001\0t\0a;b\0z\0\0\0n\x002\0t\0c\0z\0d\0");
        let titled = PrintTable { title: Some("T".into()), ..t.clone() };
        let o = PrintOptions {
            format: OutputFormat::Unaligned, expanded: Expanded::On,
            text: TextOptions { fieldsep: semi(), recordsep: hash(), ..TextOptions::default() }, ..PrintOptions::default()
        };
        assert_eq!(print_table(&titled, &o).join("\n"), "T##n;1#t;a;b#z;##n;2#t;c#z;d");
    }

    /// psql 18.4 under `\pset csv_fieldsep`, of `select 1 as n, 'a;b' as t,
    /// 'x.y' as u`: a field holding the separator quoted — every field
    /// when it's `.` or `\`.
    #[test]
    fn csv_separators_are_psqls() {
        use crate::types::TextOptions;
        let t = table(&["n", "t", "u"], &[&["1", "a;b", "x.y"]]);
        let with = |sep, expanded| {
            let o = PrintOptions { format: OutputFormat::Csv, expanded, text: TextOptions { csv_fieldsep: sep, ..TextOptions::default() }, ..PrintOptions::default() };
            print_table(&t, &o).join("\n")
        };
        assert_eq!(with(';', Expanded::Off), "n;t;u\n1;\"a;b\";x.y");
        assert_eq!(with(';', Expanded::On), "n;1\nt;\"a;b\"\nu;x.y");
        assert_eq!(with('.', Expanded::Off), "\"n\".\"t\".\"u\"\n\"1\".\"a;b\".\"x.y\"");
        assert_eq!(with('\\', Expanded::On), "\"n\"\\\"1\"\n\"t\"\\\"a;b\"\n\"u\"\\\"x.y\"");
    }

    /// psql 18.4 under `\pset tableattr`: the html table's attributes, and
    /// latex-longtable's column widths.
    #[test]
    fn tableattr_is_psqls() {
        use crate::types::TextOptions;
        let attrs = |format, expanded, attrs: &str| PrintOptions {
            format, expanded, text: TextOptions { tableattr: Some(attrs.into()), ..TextOptions::default() }, ..PrintOptions::default()
        };
        let t = PrintTable { aligns: vec![Align::Right], ..table(&["n", "t", "u"], &[&["1", "a", "b"]]) };
        assert_eq!(print_table(&t, &attrs(OutputFormat::Html, Expanded::Off, "class=\"x\""))[0], "<table border=\"1\" class=\"x\">");
        assert_eq!(print_table(&t, &attrs(OutputFormat::Html, Expanded::On, "class=\"x\""))[0], "<table border=\"1\" class=\"x\">");
        assert_eq!(print_table(&t, &attrs(OutputFormat::LatexLongtable, Expanded::Off, "0.2 0.3"))[0],
                   "\\begin{longtable}{r | p{0.2\\textwidth} | p{0.3\\textwidth}}");
        // The last width again once they run out.
        let wide = PrintTable { aligns: vec![Align::Right], ..table(&["n", "t", "u", "v"], &[&["1", "a", "b", "c"]]) };
        assert_eq!(print_table(&wide, &attrs(OutputFormat::LatexLongtable, Expanded::Off, "0.2"))[0],
                   "\\begin{longtable}{r | p{0.2\\textwidth} | p{0.2\\textwidth} | p{0.2\\textwidth}}");
    }

    /// psql 18.4 under `\pset format wrapped` and `\pset columns N`, of
    /// `select 1 as id, 'the quick brown fox jumps over the lazy dog' as
    /// words, 12345 as num, E'short\nsecond line that is rather long' as
    /// multi union all select 2, 'tiny', 7, 'x'`.
    #[test]
    fn wrapped_is_psqls() {
        let t = PrintTable {
            title: Some("Wrapped title here".into()),
            aligns: vec![Align::Right, Align::Left, Align::Right, Align::Left],
            count_rows: true,
            ..table(&["id", "words", "num", "multi"], &[
                &["1", "the quick brown fox jumps over the lazy dog", "12345", "short\nsecond line that is rather long"],
                &["2", "tiny", "7", "x"],
            ])
        };
        let wrapped = |columns| print_table(&t, &PrintOptions { format: OutputFormat::Wrapped, columns: Some(columns), ..PrintOptions::default() });
        assert_eq!(wrapped(40), [
            "           Wrapped title here",
            " id |     words     | num |    multi    ",
            "----+---------------+-----+-------------",
            "  1 | the quick bro.| 123.| short      +",
            "    |.wn fox jumps .|. 45 | second line.",
            "    |.over the lazy.|     |. that is ra.",
            "    |. dog          |     |.ther long",
            "  2 | tiny          |   7 | x",
            "(2 rows)",
            "",
        ]);
        assert_eq!(wrapped(30), [
            "      Wrapped title here",
            " id |  words  | num |  multi  ",
            "----+---------+-----+---------",
            "  1 | the qui.| 123.| short  +",
            "    |.ck brow.|. 45 | second .",
            "    |.n fox j.|     |.line th.",
            "    |.umps ov.|     |.at is r.",
            "    |.er the .|     |.ather l.",
            "    |.lazy do.|     |.ong",
            "    |.g       |     | ",
            "  2 | tiny    |   7 | x",
            "(2 rows)",
            "",
        ]);
        // Narrower than its headers: as aligned prints it.
        assert_eq!(wrapped(20), print_table(&t, &PrintOptions::default()));
        // Not measured: no wrapping.
        assert_eq!(print_table(&t, &opts(OutputFormat::Wrapped, Expanded::Off)), print_table(&t, &PrintOptions::default()));
        // `\x auto` keeps it when the wrapped table fits.
        assert_eq!(print_table(&t, &PrintOptions { format: OutputFormat::Wrapped, expanded: Expanded::Auto, columns: Some(40), ..PrintOptions::default() })[1..],
                   wrapped(40)[1..]);
    }

    /// psql 18.4, `\pset format wrapped`, `\x on`, `\pset columns N`.
    #[test]
    fn wrapped_expanded_is_psqls() {
        let t = PrintTable {
            aligns: vec![Align::Right, Align::Left],
            ..table(&["id", "words"], &[&["1", "the quick brown fox jumps over the lazy dog"], &["2", "tiny"]])
        };
        let wrapped = |columns| print_table(&t, &PrintOptions {
            format: OutputFormat::Wrapped, expanded: Expanded::On, columns: Some(columns), ..PrintOptions::default()
        });
        assert_eq!(wrapped(30), [
            "-[ RECORD 1 ]----------------",
            "id    | 1",
            "words | the quick brown fox j.",
            "      |.umps over the lazy do.",
            "      |.g",
            "-[ RECORD 2 ]----------------",
            "id    | 2",
            "words | tiny",
            "",
        ]);
        // Never under three characters, nor the record line's width.
        let narrow = wrapped(12);
        assert_eq!(narrow[..5], ["-[ RECORD 1 ]", "id    | 1", "words | the .", "      |.quic.", "      |.k br."]);
        assert_eq!(narrow[narrow.len() - 5..], ["      |.dog", "-[ RECORD 2 ]", "id    | 2", "words | tiny", ""]);
        // Not measured: the values as they are.
        let t = table(&["id", "w"], &[&["1", "ab"]]);
        assert_eq!(print_table(&t, &opts(OutputFormat::Wrapped, Expanded::On)), ["-[ RECORD 1 ]", "id | 1", "w  | ab", ""]);
    }

    #[test]
    fn borders_and_line_styles_print_as_psqls() {
        use crate::types::{LineStyle, Rules, TextOptions, UnicodeLine};
        use UnicodeLine::{Double, Single};
        // psql 18.4, `\pset title Tt` and `columns 16`, for:
        //   select 1 as n, E'a\nbc' as "multi
        //   head", 'xyz' as t union all select 22, 'd', E'p\nq'
        let s = |cells: &[&str]| cells.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        let t = PrintTable {
            title: Some("Tt".into()),
            headers: s(&["n", "multi\nhead", "t"]),
            aligns: vec![Align::Right, Align::Left, Align::Left],
            rows: vec![s(&["1", "a\nbc", "xyz"]), s(&["22", "d", "p\nq"])],
            footers: Vec::new(),
            count_rows: true,
        };
        let printed = |format, border, style, [unicode_border, unicode_column, unicode_header]: [UnicodeLine; 3], expanded| {
            let rules = Rules { border, style, unicode_border, unicode_column, unicode_header, ..Rules::default() };
            let o = PrintOptions { format, expanded, columns: Some(16), text: TextOptions { rules, ..TextOptions::default() }, ..PrintOptions::default() };
            print_table(&t, &o).iter().map(|line| format!("{line}\n")).collect::<String>()
        };
        // Border 0: no lines between columns, dashes under each.
        assert_eq!(printed(OutputFormat::Aligned, 0, LineStyle::Ascii, [Single, Single, Single], Expanded::Off), r#"     Tt
n  multi+ t  
   head      
-- ----- ---
 1 a    +xyz
   bc    
22 d     p  +
         q
(2 rows)

"#);
        // Old-ascii: a continued header marked at its left, a continued value in the column line.
        assert_eq!(printed(OutputFormat::Aligned, 2, LineStyle::OldAscii, [Single, Single, Single], Expanded::Off), r#"         Tt
+----+-------+-----+
| n  | multi |  t  |
|+   |+head  |+    |
+----+-------+-----+
|  1 | a     | xyz |
|    : bc          |
| 22 | d     | p   |
|            : q   |
+----+-------+-----+
(2 rows)

"#);
        // Unicode, a double frame and header rule.
        assert_eq!(printed(OutputFormat::Wrapped, 2, LineStyle::Unicode, [Double, Single, Double], Expanded::Off), r#"         Tt
╔════╤═══════╤═════╗
║ n  │ multi↵│  t  ║
║    │ head  │     ║
╠════╪═══════╪═════╣
║  1 │ a    ↵│ xyz ║
║    │ bc    │     ║
║ 22 │ d     │ p  ↵║
║    │       │ q   ║
╚════╧═══════╧═════╝
(2 rows)

"#);
        // Expanded old-ascii — `;` once a value is done, as psql leaves it.
        assert_eq!(printed(OutputFormat::Aligned, 1, LineStyle::OldAscii, [Single, Single, Single], Expanded::On), r#"Tt
-[ RECORD 1 ]
 n     | 1
 multi | a
+head  : bc
 t     | xyz
-[ RECORD 2 ]
 n     | 22
 multi | d
+head  ;
 t     | p
       : q

"#);
        // Expanded under border 0.
        assert_eq!(printed(OutputFormat::Aligned, 0, LineStyle::Unicode, [Single, Single, Single], Expanded::On), r#"Tt
* Record 1
n      1
multi↵ a  ↵
head   bc
t      xyz
* Record 2
n      22
multi↵ d
head  
t      p  ↵
       q

"#);
        // Expanded, framed.
        assert_eq!(printed(OutputFormat::Aligned, 2, LineStyle::Unicode, [Single, Single, Single], Expanded::On), r#"Tt
┌─[ RECORD 1 ]─┐
│ n     │ 1   │
│ multi↵│ a  ↵│
│ head  │ bc  │
│ t     │ xyz │
├─[ RECORD 2 ]─┤
│ n     │ 22  │
│ multi↵│ d   │
│ head  │     │
│ t     │ p  ↵│
│       │ q   │
└───────┴─────┘

"#);
        // html writes the border as it is.
        assert_eq!(printed(OutputFormat::Html, 0, LineStyle::Ascii, [Single, Single, Single], Expanded::Off), r#"<table border="0">
  <caption>Tt</caption>
  <tr>
    <th align="center">n</th>
    <th align="center">multi<br />
head</th>
    <th align="center">t</th>
  </tr>
  <tr valign="top">
    <td align="right">1</td>
    <td align="left">a<br />
bc</td>
    <td align="left">xyz</td>
  </tr>
  <tr valign="top">
    <td align="right">22</td>
    <td align="left">d</td>
    <td align="left">p<br />
q</td>
  </tr>
</table>
<p>(2 rows)<br />
</p>
"#);
        // asciidoc frames and grids it.
        assert_eq!(printed(OutputFormat::Asciidoc, 2, LineStyle::Ascii, [Single, Single, Single], Expanded::Off), r#"
.Tt
[options="header",cols=">l,<l,<l",frame="all",grid="all"]
|====
^l|n ^l|multi
head ^l|t
|1 |a
bc |xyz
|22 |d |p
q
|====

....
(2 rows)
....
"#);
        // latex rules each row under border 3.
        assert_eq!(printed(OutputFormat::Latex, 3, LineStyle::Ascii, [Single, Single, Single], Expanded::Off), r#"\begin{center}
Tt
\end{center}

\begin{tabular}{| r | l | l |}
\hline
\textit{n} & \textit{multi\\head} & \textit{t} \\
\hline
1 & a\\bc & xyz \\
\hline
22 & d & p\\q \\
\hline
\end{tabular}

\noindent (2 rows) \\

"#);
        // longtable with book rules.
        assert_eq!(printed(OutputFormat::LatexLongtable, 2, LineStyle::Ascii, [Single, Single, Single], Expanded::Off), r#"\begin{longtable}{| r | l | l |}
\toprule
\small\textbf{\textit{n}} & \small\textbf{\textit{multi\\head}} & \small\textbf{\textit{t}} \\
\midrule
\endfirsthead
\toprule
\small\textbf{\textit{n}} & \small\textbf{\textit{multi\\head}} & \small\textbf{\textit{t}} \\
\midrule
\endhead
\bottomrule
\caption[Tt (Continued)]{Tt}
\endfoot
\bottomrule
\caption[Tt]{Tt}
\endlastfoot
\raggedright{1}
&
\raggedright{a\\bc}
&
\raggedright{xyz} \tabularnewline
\raggedright{22}
&
\raggedright{d}
&
\raggedright{p\\q} \tabularnewline
\end{longtable}
"#);
        // troff-ms boxes it, a rule between records.
        assert_eq!(printed(OutputFormat::TroffMs, 2, LineStyle::Ascii, [Single, Single, Single], Expanded::On), r#".LP
.DS C
Tt
.DE
.LP
.TS
center box;
c s.
\fIRecord 1\fP
_
.T&
c l.
n	1
multi
head	a
bc
t	xyz
_
.T&
c s.
\fIRecord 2\fP
_
.T&
c l.
n	22
multi
head	d
t	p
q
.TE
.DS L
.DE
"#);
    }

    #[test]
    fn xheader_width_is_psqls() {
        use crate::types::{Rules, TextOptions, XHeader};
        // psql 18.4, `\x`, `\pset columns 30`, for
        //   select 1 as id, 'the quick brown fox jumps over the lazy dog' as words
        let t = PrintTable {
            headers: vec!["id".into(), "words".into()],
            aligns: vec![Align::Right, Align::Left],
            rows: vec![vec!["1".into(), "the quick brown fox jumps over the lazy dog".into()]],
            ..PrintTable::default()
        };
        let record_lines = |border: u16, xheader: XHeader| -> Vec<String> {
            let o = PrintOptions {
                expanded: Expanded::On, columns: Some(30),
                text: TextOptions { rules: Rules { border, xheader, ..Rules::default() }, ..TextOptions::default() },
                ..PrintOptions::default()
            };
            print_table(&t, &o).into_iter().filter(|l| l.contains("RECORD") || l.contains("Record") || l.starts_with('+')).collect()
        };
        let dashes = |n: usize| "-".repeat(n);
        assert_eq!(record_lines(1, XHeader::Full), [format!("-[ RECORD 1 ]{}", dashes(38))]);
        assert_eq!(record_lines(1, XHeader::Column), ["-[ RECORD 1 ]"]);
        assert_eq!(record_lines(1, XHeader::Page), [format!("-[ RECORD 1 ]{}", dashes(17))]);
        assert_eq!(record_lines(1, XHeader::Exact(20)), [format!("-[ RECORD 1 ]{}", dashes(7))]);
        // A width of 0 or less: the full width.
        assert_eq!(record_lines(1, XHeader::Exact(-3)), record_lines(1, XHeader::Full));
        assert_eq!(record_lines(0, XHeader::Column), ["* Record 1"]);
        assert_eq!(record_lines(0, XHeader::Exact(20)), [format!("* Record 1{}", " ".repeat(10))]);
        // Framed: the bottom rule ends where the record lines do.
        assert_eq!(record_lines(2, XHeader::Column), ["+-[ RECORD 1 ]", "+-------+"]);
        assert_eq!(record_lines(2, XHeader::Page), [
            format!("+-[ RECORD 1 ]{}+", dashes(15)), format!("+-------+{}+", dashes(20)),
        ]);
        assert_eq!(record_lines(2, XHeader::Exact(20)), [
            format!("+-[ RECORD 1 ]{}+", dashes(5)), format!("+-------+{}+", dashes(10)),
        ]);
    }

    #[test]
    fn numbers_in_the_clients_locale_are_psqls() {
        use shared::NumericLocale;
        let locale = |decimal_point: &str, thousands_sep: &str, grouping: i32| NumericLocale {
            decimal_point: decimal_point.into(), thousands_sep: thousands_sep.into(), grouping,
        };
        // localeconv() as macOS gives it, and psql 18.4's cells under each
        // (`LC_ALL=… psql -P numericlocale=on`).
        let c = locale(".", "", 0);
        let german = locale(",", ".", 3);
        let french = locale(",", "\u{202f}", 3);
        let swiss = locale(".", "’", 3);
        let cases: [(&str, [&str; 4]); 11] = [
            ("1234567.891", ["1,234,567.891", "1.234.567,891", "1\u{202f}234\u{202f}567,891", "1’234’567.891"]),
            ("-1234567", ["-1,234,567", "-1.234.567", "-1\u{202f}234\u{202f}567", "-1’234’567"]),
            ("1234", ["1,234", "1.234", "1\u{202f}234", "1’234"]),
            ("123", ["123", "123", "123", "123"]),
            ("10000000000", ["10,000,000,000", "10.000.000.000", "10\u{202f}000\u{202f}000\u{202f}000", "10’000’000’000"]),
            ("-0.5", ["-0.5", "-0,5", "-0,5", "-0.5"]),
            ("1e+20", ["1e+20", "1e+20", "1e+20", "1e+20"]),
            // Not a plain number: as it is.
            ("$1,234,567.00", ["$1,234,567.00"; 4]),
            ("NaN", ["NaN"; 4]),
            ("Infinity", ["Infinity"; 4]),
            ("", [""; 4]),
        ];
        for (value, expected) in cases {
            for (locale, expected) in [&c, &german, &french, &swiss].into_iter().zip(expected) {
                assert_eq!(format_numeric_locale(value, locale), expected, "{value} in {locale:?}");
            }
        }
        // psql's fallbacks: no separator — `,`, or `.` beside a `,` point;
        // groups of 3 unless the first is 1–6; Indian grouping in threes.
        assert_eq!(format_numeric_locale("1234567,5", &locale(",", "", 0)), "1234567,5");
        assert_eq!(format_numeric_locale("1234567.5", &locale(",", "", 0)), "1.234.567,5");
        assert_eq!(format_numeric_locale("1234567", &locale(".", ",", 2)), "1,23,45,67");
        assert_eq!(format_numeric_locale("1234567", &locale(".", ",", 127)), "1,234,567");
        assert_eq!(format_numeric_locale("1234567", &locale(".", ",", -1)), "1,234,567");

        // Only the right-aligned (number) columns, and only when on.
        let mut t = PrintTable {
            headers: vec!["n".into(), "t".into()],
            aligns: vec![Align::Right, Align::Left],
            rows: vec![vec!["1234".into(), "1234".into()]],
            ..PrintTable::default()
        };
        let mut o = PrintOptions { locale: german, ..PrintOptions::default() };
        localize_numbers(&mut t, &o);
        assert_eq!(t.rows[0], ["1234", "1234"]);
        o.text.numeric_locale = true;
        localize_numbers(&mut t, &o);
        assert_eq!(t.rows[0], ["1.234", "1234"]);
    }

    /// Every format under every `\pset border` (past 3 too) and line
    /// style — unicode with each mix of single and double lines — with
    /// `\x` off, on and auto and `\t` off and on, printed byte for byte as
    /// psql 18 prints the same queries: multi-line headers and values,
    /// wrapping, no rows, no columns. Opt-in: psql on PATH, Pagila.
    #[test]
    #[ignore]
    fn every_border_and_line_style_prints_as_psql_does() {
        use crate::types::{LineStyle, Rules, TextOptions, UnicodeLine};
        use super::super::live::psql;
        use UnicodeLine::{Double, Single};
        let s = |cells: &[&str]| cells.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        let titled = |headers: &[&str], aligns: Vec<Align>, rows: Vec<Vec<String>>| PrintTable {
            title: Some("Tt".into()), headers: s(headers), aligns, rows, footers: Vec::new(), count_rows: true,
        };
        let (l, r) = (Align::Left, Align::Right);
        let tables = [
            ("select 1 as n, E'a\\nbc' as \"multi\nhead\", 'xyz' as t union all select 22, 'd', E'p\\nq'",
             titled(&["n", "multi\nhead", "t"], vec![r, l, l], vec![s(&["1", "a\nbc", "xyz"]), s(&["22", "d", "p\nq"])])),
            ("select 1 as id, 'the quick brown fox jumps over' as words, E'ab\\ncd' as m",
             titled(&["id", "words", "m"], vec![r, l, l], vec![s(&["1", "the quick brown fox jumps over", "ab\ncd"])])),
            ("select 1 as a where false", titled(&["a"], vec![r], Vec::new())),
            ("select", titled(&[], Vec::new(), vec![Vec::new()])),
        ];
        let styles = [
            (LineStyle::Ascii, [Single; 3]),
            (LineStyle::OldAscii, [Single; 3]),
            (LineStyle::Unicode, [Single; 3]),
            (LineStyle::Unicode, [Double; 3]),
            (LineStyle::Unicode, [Double, Single, Double]),
            (LineStyle::Unicode, [Single, Double, Single]),
        ];
        let formats = [OutputFormat::Aligned, OutputFormat::Wrapped, OutputFormat::Html, OutputFormat::Asciidoc,
                       OutputFormat::Latex, OutputFormat::LatexLongtable, OutputFormat::TroffMs];
        let mut failures = Vec::new();
        let mut cases = 0;
        for (sql, table) in &tables {
            for format in formats {
                // Line styles draw only aligned and wrapped tables.
                let styles = if matches!(format, OutputFormat::Aligned | OutputFormat::Wrapped) { &styles[..] } else { &styles[..1] };
                for border in [0u16, 1, 2, 3, 7] {
                    for &(style, [unicode_border, unicode_column, unicode_header]) in styles {
                        for expanded in [Expanded::Off, Expanded::On, Expanded::Auto] {
                            for tuples_only in [false, true] {
                                let rules = Rules { border, style, unicode_border, unicode_column, unicode_header, ..Rules::default() };
                                let options = PrintOptions {
                                    format, expanded, tuples_only, footer: true, columns: Some(24),
                                    text: TextOptions { rules, ..TextOptions::default() }, ..PrintOptions::default()
                                };
                                // Each line ended by its newline.
                                let ours: String = print_table(table, &options).iter().map(|line| format!("{line}\n")).collect();
                                let args = [
                                    format!("format={}", format.name()),
                                    format!("border={border}"),
                                    format!("linestyle={}", style.name()),
                                    format!("unicode_border_linestyle={}", unicode_border.name()),
                                    format!("unicode_column_linestyle={}", unicode_column.name()),
                                    format!("unicode_header_linestyle={}", unicode_header.name()),
                                    format!("expanded={}", match expanded { Expanded::Off => "off", Expanded::On => "on", Expanded::Auto => "auto" }),
                                    format!("tuples_only={}", if tuples_only { "on" } else { "off" }),
                                    "title=Tt".into(), "columns=24".into(), "pager=off".into(),
                                ];
                                let mut argv: Vec<&str> = args.iter().flat_map(|a| ["-P", a.as_str()]).collect();
                                argv.extend(["-c", sql]);
                                let theirs = String::from_utf8_lossy(&psql(&argv).stdout).into_owned();
                                cases += 1;
                                if ours != theirs {
                                    failures.push(format!("{} | {sql}\n  ours: {ours:?}\n  psql: {theirs:?}", args[..8].join(" ")));
                                }
                            }
                        }
                    }
                }
            }
        }
        let cases_failing: Vec<&str> = failures.iter().filter_map(|f| f.lines().next()).collect();
        assert!(failures.is_empty(), "{} of {cases} differ:\n{}\n\n{}", failures.len(), cases_failing.join("\n"),
                failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n"));
    }

    /// `\pset numericlocale` in every locale's conventions — as macOS's
    /// `localeconv()` gives them, which noctua reads the same way — and
    /// format, with `\x` off and on, printed byte for byte as psql 18
    /// prints the same query run under that locale (`LC_ALL`). Opt-in:
    /// psql on PATH, Pagila.
    #[test]
    #[ignore]
    fn numeric_locale_prints_as_psql_does() {
        use shared::NumericLocale;
        let sql = "select 1234567.891::numeric as n, -1234567::int as i, 1234::int2 as s, 12345678901::int8 as b, \
                   1e10::float8 as f, -0.5::float4 as h, 1234567::money as m, null::int as z, 'NaN'::float8 as nan, \
                   'Infinity'::numeric as inf, 12345::oid as o, '1234'::text as t";
        let headers = ["n", "i", "s", "b", "f", "h", "m", "z", "nan", "inf", "o", "t"];
        let raw = ["1234567.891", "-1234567", "1234", "12345678901", "10000000000", "-0.5", "$1,234,567.00", "", "NaN",
                   "Infinity", "12345", "1234"];
        let locales = [
            ("C", ".", "", 0), ("en_US.UTF-8", ".", ",", 3), ("de_DE.UTF-8", ",", ".", 3),
            ("fr_FR.UTF-8", ",", "\u{202f}", 3), ("de_CH.UTF-8", ".", "’", 3), ("hi_IN.UTF-8", ".", ",", 3),
        ];
        let mut failures = Vec::new();
        for (name, decimal_point, thousands_sep, grouping) in locales {
            let locale = NumericLocale { decimal_point: decimal_point.into(), thousands_sep: thousands_sep.into(), grouping };
            for format in [OutputFormat::Aligned, OutputFormat::Unaligned, OutputFormat::Csv, OutputFormat::Html] {
                for expanded in [Expanded::Off, Expanded::On] {
                    let mut options = PrintOptions { format, expanded, locale: locale.clone(), ..PrintOptions::default() };
                    options.text.numeric_locale = true;
                    let mut table = PrintTable {
                        headers: headers.iter().map(|h| h.to_string()).collect(),
                        aligns: headers.iter().map(|h| if *h == "t" { Align::Left } else { Align::Right }).collect(),
                        rows: vec![raw.iter().map(|v| v.to_string()).collect()],
                        count_rows: true,
                        ..PrintTable::default()
                    };
                    // psql leaves SQL NULL be: it prints its null display.
                    let null = 7;
                    let cell = std::mem::take(&mut table.rows[0][null]);
                    localize_numbers(&mut table, &options);
                    table.rows[0][null] = cell;
                    let ours: String = print_table(&table, &options).iter().map(|line| format!("{line}\n")).collect();
                    let x = if expanded == Expanded::On { "expanded=on" } else { "expanded=off" };
                    let out = std::process::Command::new("psql")
                        .env("LC_ALL", name)
                        .args(["-X", "-d", "pagila", "-U", "postgres", "-P", "pager=off", "-P", "numericlocale=on",
                               "-P", &format!("format={}", format.name()), "-P", x, "-c", sql])
                        .output().expect("psql on PATH");
                    let theirs = String::from_utf8_lossy(&out.stdout).into_owned();
                    if ours != theirs {
                        failures.push(format!("{name} {} {x}:\n  ours: {ours:?}\n  psql: {theirs:?}", format.name()));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// `\pset xheader_width` — full, column, page (`\pset columns`), a
    /// width, one psql ignores — on expanded output, aligned and wrapped,
    /// under every border and two line styles, with `\t` off and on,
    /// printed byte for byte as psql 18 prints the same queries. Opt-in:
    /// psql on PATH, Pagila.
    #[test]
    #[ignore]
    fn xheader_width_prints_as_psql_does() {
        use crate::types::{LineStyle, Rules, TextOptions, XHeader};
        use super::super::live::psql;
        let s = |cells: &[&str]| cells.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        let tables = [
            ("select 1 as id, 'the quick brown fox jumps over the lazy dog' as words union all select 22, 'x'",
             PrintTable {
                 title: Some("Tt".into()), headers: s(&["id", "words"]), aligns: vec![Align::Right, Align::Left],
                 rows: vec![s(&["1", "the quick brown fox jumps over the lazy dog"]), s(&["22", "x"])],
                 footers: Vec::new(), count_rows: true,
             }),
            ("select 1 as a_rather_long_header, E'x\\ny' as b",
             PrintTable {
                 title: None, headers: s(&["a_rather_long_header", "b"]), aligns: vec![Align::Right, Align::Left],
                 rows: vec![s(&["1", "x\ny"])], footers: Vec::new(), count_rows: true,
             }),
        ];
        let xheaders = [
            (XHeader::Full, "full"), (XHeader::Column, "column"), (XHeader::Page, "page"),
            (XHeader::Exact(20), "20"), (XHeader::Exact(3), "3"), (XHeader::Exact(-3), "-3"),
        ];
        let mut failures = Vec::new();
        let mut cases = 0;
        for (sql, table) in &tables {
            for format in [OutputFormat::Aligned, OutputFormat::Wrapped] {
                for border in [0u16, 1, 2, 3] {
                    for style in [LineStyle::Ascii, LineStyle::Unicode] {
                        for (xheader, name) in xheaders {
                            for tuples_only in [false, true] {
                                let rules = Rules { border, style, xheader, ..Rules::default() };
                                let options = PrintOptions {
                                    format, expanded: Expanded::On, tuples_only, columns: Some(30),
                                    text: TextOptions { rules, ..TextOptions::default() }, ..PrintOptions::default()
                                };
                                let ours: String = print_table(table, &options).iter().map(|line| format!("{line}\n")).collect();
                                let args = [
                                    format!("format={}", format.name()), format!("border={border}"),
                                    format!("linestyle={}", style.name()), format!("xheader_width={name}"),
                                    format!("tuples_only={}", if tuples_only { "on" } else { "off" }),
                                    "expanded=on".into(), "columns=30".into(), "pager=off".into(),
                                ];
                                let title = table.title.as_deref().map(|t| format!("title={t}"));
                                let mut argv: Vec<&str> = args.iter().chain(title.iter()).flat_map(|a| ["-P", a.as_str()]).collect();
                                argv.extend(["-c", sql]);
                                let theirs = String::from_utf8_lossy(&psql(&argv).stdout).into_owned();
                                cases += 1;
                                if ours != theirs {
                                    failures.push(format!("{} | {sql}\n  ours: {ours:?}\n  psql: {theirs:?}", args[..5].join(" ")));
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{} of {cases} differ:\n{}", failures.len(), failures.iter().take(8).cloned().collect::<Vec<_>>().join("\n"));
    }
}
