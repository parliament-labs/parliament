//! Shell pane.
//!
//! Owns the SQL input + history rendering, the multi-statement
//! pasting + queueing logic, the safe-mode confirm dialog (both
//! single-statement and batch shapes), the `\watch N` auto-repeat
//! plumbing, the query buffer a backslash command typed after SQL works
//! on (`select 1 \gx`, read with [`crate::parser::trailing_command`]),
//! the per-pane TXN badge, the readline-style keybindings, and the
//! persistent-history bridge to [`crate::history`].
//!
//! Meta-command dispatch (`\d`, `\dt`, `\timing`, `\set`, …) routes
//! through the active plane — see [`crate::plane::DbmsPlane::meta_cmd`].
//! SQL inspection for safe-mode + TXN-badge routes through
//! [`crate::sql_inspect::inspect_statement`] against the active plane's
//! [`sqlparser::dialect::Dialect`].
//!
//! [`render_shell_leaf`] is the entry point for shell-kind panes;
//! Schema Explorer and Activity panes route to their own renderers
//! in [`crate::explorer`] / [`crate::activity`].

use leptos::*;
use shared::{ClientMessage, FormatStyle};
use std::collections::HashMap;
use wasm_bindgen::prelude::*;
use web_sys::WebSocket;

use crate::dom::*;
use crate::export::render_save_links;
use crate::history::persist_command;
use crate::parser::Lexicon;
use crate::plane::{DbmsPlane, LostConnection, Timing, plane_for};
use crate::types::*;
use crate::activity::render_activity_pane;

/// Map a cursor byte offset from the *old* text to the *new* text after a
/// reformat.
///
/// Special-cased: if the cursor was at the end of `old` (the dominant case
/// while typing — debounce fires shortly after the last keystroke, cursor
/// is naturally at the end), the new cursor lands at the end of `new`. This
/// preserves things like a trailing newline the user just typed so they
/// stay "ready to type the next clause."
///
/// Otherwise — for mid-statement edits — we count alphanumeric characters
/// before the cursor in `old`, then walk `new` until we've passed the same
/// count. That position survives whitespace / newline / indent changes
/// (which is most of what a beautifier does). Best-effort approximation.
///
/// UTF-16 vs byte: textareas hand back UTF-16 offsets, but River output is
/// ASCII-only — equivalent for SQL.
fn map_cursor_after_format(old: &str, new: &str, old_byte: usize) -> usize {
    if old_byte >= old.len() {
        return new.len();
    }
    let n = old[..old_byte].chars().filter(|c| c.is_alphanumeric()).count();
    if n == 0 {
        return 0;
    }
    let mut count = 0;
    for (i, c) in new.char_indices() {
        if c.is_alphanumeric() {
            count += 1;
            if count == n {
                return i + c.len_utf8();
            }
        }
    }
    new.len()
}

/// Schedule a debounced beautifier pass on `pane_id`'s textarea. Called from
/// Apply `style` to the current input, writing the result back to
/// `t_input` and restoring the cursor on the next animation frame.
/// Fires synchronously — no debounce, no timer. Driven by the shell-
/// input right-click menu's Format items. `Unformatted` is a no-op
/// (there's no canonical inverse of River reflow); the menu still
/// offers it for symmetry with the connection form's style list.
pub(crate) fn apply_format_to_input(
    pane_id: &str,
    t_input: RwSignal<String>,
    plane: &'static dyn DbmsPlane,
    style: FormatStyle,
) {
    let sql = t_input.get_untracked();
    if sql.is_empty() { return; }

    let dialect = plane.sql_dialect();
    let new_sql = match style {
        // Unformatted is a no-op — the menu offers it for parity with
        // the connection form's style list, not because it does anything.
        FormatStyle::Unformatted => return,
        FormatStyle::River => match crate::format_sql::format_river(&sql, dialect) {
            Some(s) => s,
            None    => return,
        },
        FormatStyle::Block => match crate::format_sql::format_block(&sql, dialect) {
            Some(s) => s,
            None    => return,
        },
        FormatStyle::Inline => match crate::format_sql::format_inline(&sql, dialect) {
            Some(s) => s,
            None    => return,
        },
    };
    if new_sql == sql { return; }

    // Cursor preservation across the rewrite so the user lands roughly
    // where they were before the reflow.
    let pid = pane_id.to_string();
    let old_cursor: usize = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid)))
        .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        .and_then(|ta| ta.selection_start().ok().flatten())
        .map(|n| n as usize)
        .unwrap_or(0);
    let new_cursor = map_cursor_after_format(&sql, &new_sql, old_cursor);
    t_input.set(new_sql);

    // Restore cursor + recompute auto-grow height after Leptos paints.
    let pid_paint = pid;
    let paint_cb = Closure::once(move || {
        if let Some(ta) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid_paint)))
            .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        {
            let pos = new_cursor as u32;
            let _ = ta.set_selection_range(pos, pos);
            let _ = ta.style().set_property("height", "auto");
            let _ = ta
                .style()
                .set_property("height", &format!("{}px", ta.scroll_height()));
        }
    });
    if let Some(win) = web_sys::window() {
        let _ = win.request_animation_frame(paint_cb.as_ref().unchecked_ref());
    }
    paint_cb.forget();
}

/// Render one token as a `<span>` carrying its highlight class.
fn render_sql_token(tok: crate::highlight::Token) -> View {
    let class = tok.kind.css_class();
    view! { <span class=class>{tok.text}</span> }.into_view()
}

pub(crate) fn icon_new_shell() -> impl IntoView {
    view! {
        <svg width="20" height="18" viewBox="0 0 20 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <rect x="1" y="1" width="18" height="16" rx="2"/>
            <path d="M5 5.5l3.5 3.5-3.5 3.5"/>
            <line x1="11" y1="12.5" x2="15" y2="12.5"/>
        </svg>
    }
}

pub(crate) fn icon_split_v() -> impl IntoView {
    view! {
        <svg width="20" height="18" viewBox="0 0 20 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round">
            <rect x="1" y="1" width="18" height="16" rx="2"/>
            <line x1="10" y1="1" x2="10" y2="17"/>
        </svg>
    }
}

pub(crate) fn icon_split_h() -> impl IntoView {
    view! {
        <svg width="20" height="18" viewBox="0 0 20 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round">
            <rect x="1" y="1" width="18" height="16" rx="2"/>
            <line x1="1" y1="9" x2="19" y2="9"/>
        </svg>
    }
}

pub(crate) fn icon_panes() -> impl IntoView {
    view! {
        <svg width="20" height="18" viewBox="0 0 20 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <rect x="1" y="1" width="12" height="11" rx="1.5"/>
            <rect x="7" y="6" width="12" height="11" rx="1.5"/>
        </svg>
    }
}

pub(crate) fn pane_kind_icon(kind: PaneKind) -> leptos::View {
    match kind {
        PaneKind::Shell          => icon_new_shell().into_view(),
        PaneKind::SchemaExplorer => view! { <span>"⊞"</span> }.into_view(),
        PaneKind::Activity       => view! { <span>"⚡"</span> }.into_view(),
    }
}

// Renders the DB connection id as a small `#NNN` pill when present.
// Matches `SHOW PROCESSLIST.ID` (MySQL) / `pg_stat_activity.pid` (Postgres)
// so a DBA can correlate the pane with server-side session state.
//
// When `stale` is true the pill renders nothing — the appropriate
// affordance is already on screen elsewhere:
//   - WS drop → the top-of-app "Disconnected from Noctua. Auto-
//     reconnecting…" banner.
//   - SessionClosed → the in-pane "Session ended — Reconnect" banner.
// The pre-fix pill used to collapse to a red ✕ with a tooltip saying
// "close this pane and re-open from the sidebar," which (a) contradicted
// the auto-reconnect flow that restores sessions for the WS-drop case
// and (b) duplicated the in-pane banner's Reconnect button for the
// SessionClosed case. Hiding it removes the redundant noise; the slot
// reappears with the new connection id once SessionOpened arrives.
pub(crate) fn session_id_pill(
    id:    RwSignal<Option<String>>,
    stale: RwSignal<bool>,
) -> impl IntoView {
    view! {
        {move || {
            if stale.get() {
                return None;
            }
            id.get().filter(|s| !s.is_empty()).map(|s| view! {
                <span class="session-id-pill"
                      title="Database connection id — matches SHOW PROCESSLIST / pg_stat_activity">
                    {format!("#{}", s)}
                </span>
            })
        }}
    }
}

/// Estimate the memory cost of one `ShellEntry` in bytes. Counts the
/// String payloads inside the variant; the enum discriminant and
/// fixed-size scalars (`u32`, `u64`, `bool`) round to negligible at
/// the scale this indicator is for (kilobytes-to-megabytes of result
/// rows). Each `String` is `s.len()` for the UTF-8 bytes on the heap;
/// the `String`'s own 24-byte header isn't counted because for the
/// typical result-row payload it's noise compared to the cell text.
fn shell_entry_bytes(entry: &crate::types::ShellEntry) -> usize {
    use crate::types::ShellEntry as E;
    match entry {
        E::Query    { sql, .. } => sql.len(),
        E::Table    { columns, rows, .. }
        | E::Vertical { columns, rows, .. } => {
            columns.iter().map(|c| c.len()).sum::<usize>()
                + rows.iter().flat_map(|r| r.iter().map(|c| c.as_ref().map_or(0, String::len))).sum::<usize>()
        }
        E::Info     { .. }      => 0,
        E::Error    (msg)       => msg.len(),
        E::Notice   { severity, message } => severity.len() + message.len(),
        E::MetaOutput(lines)    => lines.iter().map(|l| l.len()).sum(),
        E::KeyValue (pairs)     => pairs.iter().map(|(k, v)| k.len() + v.len()).sum(),
        E::Printed  { lines, .. }
        | E::CliError { lines, .. } => lines.iter().map(|l| l.len()).sum(),
    }
}

/// Sum the byte cost of every entry in a shell's history. Drives the
/// `buf:N B` pill in the shell header so a DBA can see at a glance how
/// much memory a long-running pane has accumulated, and decide when to
/// `Clear shell buffer` from the right-click menu.
pub(crate) fn shell_buffer_bytes(history: &[crate::types::ShellEntry]) -> usize {
    history.iter().map(shell_entry_bytes).sum()
}

/// Render the shell-buffer-size indicator. Sits next to the
/// [`session_id_pill`] in the shell pane header. Always rendered
/// (unlike the session-id pill which hides when stale) — the buffer
/// belongs to the FRONTEND state and isn't affected by upstream
/// session lifecycle. Right-clicking anywhere in the shell area
/// (handled by the broader contextmenu wiring in `render_shell_leaf`)
/// surfaces the menu's `Clear shell buffer` action.
pub(crate) fn shell_buffer_pill(
    history: RwSignal<Vec<crate::types::ShellEntry>>,
) -> impl IntoView {
    view! {
        {move || {
            let bytes = history.with(|h| shell_buffer_bytes(h));
            let label = format!("buf:{}", crate::format::human_bytes(&bytes.to_string()));
            view! {
                <span class="shell-buffer-pill"
                      title="Shell buffer size (queries + results + meta output).\nRight-click in the shell area to clear.">
                    {label}
                </span>
            }
        }}
    }
}

/// What the right-click "Copy …" menu items in a query block should
/// stuff into the clipboard. Inspired by Warp Terminal's block
/// abstraction — each block is one SQL statement + the entries that
/// followed it until the next statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockCopyMode {
    /// Just the SQL the user typed (the Query's `sql`).
    Command,
    /// The output of the command — every entry that followed the
    /// Query until the next Query (Table / Vertical / Info / Error
    /// / MetaOutput / KeyValue) stringified in the order they
    /// arrived.
    Output,
    /// Command + output. Prefixed with the prompt sequence (`N SQL>`)
    /// so the clipboard payload reads like the on-screen scrollback.
    WholeBlock,
}

/// Stringify one `ShellEntry` for clipboard payload. Tabular shapes
/// (Table / Vertical) serialize as TSV — easy to paste into a
/// spreadsheet, terminal, or text editor; not committed to one
/// dialect's quoting rules (matches the existing CSV-export's
/// "minimal escaping" stance). MetaOutput / KeyValue / Error / Info
/// emit their human-readable shape.
fn entry_for_clipboard(entry: &crate::types::ShellEntry, plane: &dyn DbmsPlane) -> String {
    use crate::types::ShellEntry as E;
    match entry {
        E::Query { sql, .. } => sql.clone(),
        // As shown: its title, the header and rows tab-separated (none
        // of either for mysql's empty result, its footer alone), NULLs
        // as the null display, the footer and time — each as the result
        // printed with them.
        E::Table { columns, aligns, rows, exec_time_us, warnings, timed, style, .. } => {
            let mut lines: Vec<String> = style.title().map(String::from).into_iter().collect();
            if !columns.is_empty() && !style.tuples_only {
                lines.push(columns.join("\t"));
            }
            lines.extend(rows.iter().map(|row| style.row(plane, row, aligns).join("\t")));
            lines.extend(style.footers().iter().cloned());
            lines.extend(shown_footer(plane, rows.len(), *warnings, false, *exec_time_us, *timed, style));
            lines.join("\n")
        }
        // As the CLI printed it, then its footer.
        E::Vertical { columns, aligns, rows, exec_time_us, warnings, timed, style } => {
            let shown: Vec<Vec<String>> = rows.iter().map(|row| style.row(plane, row, aligns)).collect();
            plane.print_vertical(columns, &shown, style).into_iter()
                .chain(shown_footer(plane, rows.len(), *warnings, true, *exec_time_us, *timed, style))
                .collect::<Vec<_>>()
                .join("\n")
        }
        E::Info { exec_time_us, said, timed } => command_lines(plane, said, *exec_time_us, *timed).join("\n"),
        E::Error(msg) => format!("Error: {}", msg),
        E::Notice { severity, message } => format!("{severity}:  {message}"),
        E::MetaOutput(lines) => lines.join("\n"),
        E::Printed { lines, .. }
        | E::CliError { lines, .. } => lines.join("\n"),
        E::KeyValue(pairs) => pairs.iter()
            .map(|(k, v)| format!("{}: {}", k, v))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Format one block (a Query + its trailing output entries) for
/// clipboard payload according to the requested `mode`. Returns
/// empty string if no Query in `history` carries the given
/// `block_num` (e.g., the user cleared the buffer between
/// right-click and click — better to write nothing than the wrong
/// content).
///
/// Block boundary rule: walk forward from the Query with `num ==
/// block_num` until either the next Query or end-of-history. Every
/// entry along the way belongs to the block.
pub(crate) fn format_block_for_clipboard(
    history:   &[crate::types::ShellEntry],
    block_num: u32,
    mode:      BlockCopyMode,
    plane:     &dyn DbmsPlane,
) -> String {
    use crate::types::ShellEntry as E;
    // Find the Query that owns this block.
    let start = history.iter().position(|e| matches!(e, E::Query { num, .. } if *num == block_num));
    let Some(start) = start else { return String::new() };

    // Sequence of entries belonging to this block: the Query at
    // `start` plus every following entry up to (but not including)
    // the next Query.
    let mut end = history.len();
    for (i, e) in history.iter().enumerate().skip(start + 1) {
        if matches!(e, E::Query { .. }) { end = i; break; }
    }
    let block = &history[start..end];

    let query = match &block[0] {
        E::Query { sql, terminator, .. } => format!("{sql}{terminator}"),
        _ => return String::new(), // unreachable per the `start` selector
    };

    match mode {
        BlockCopyMode::Command => query,
        BlockCopyMode::Output => block.iter().skip(1)
            .map(|entry| entry_for_clipboard(entry, plane))
            .collect::<Vec<_>>()
            .join("\n"),
        BlockCopyMode::WholeBlock => {
            // Clipboard format uses a fixed `N SQL>` prompt rather
            // than the user's `PROMPT1` template. The clipboard is
            // a portable text representation meant to paste cleanly
            // into a chat / bug report / doc; embedding the user's
            // prompt customisations (`%n@%/`, ANSI escapes, etc.)
            // would leak local state and could break renderers that
            // don't understand the customisations.
            let mut out = format!("{} SQL> {}\n", block_num, query.trim_end());
            for entry in block.iter().skip(1) {
                out.push_str(&entry_for_clipboard(entry, plane));
                out.push('\n');
            }
            out.trim_end().to_string()
        }
    }
}

use crate::explorer::render_explorer_pane;

/// Header text for the single-statement safe-mode confirm dialog.
/// Carries the specific reason the inspector flagged the statement so
/// the DBA sees *why* it's gated — pre-fix the dialog said only
/// "destructive command" no matter the cause; with AST classification
/// we can name the actual hazard.
fn single_header(r: &crate::sql_inspect::DestructiveReason) -> &'static str {
    use crate::sql_inspect::DestructiveReason as D;
    match r {
        D::Drop                   => "⚠ safe-mode: DROP",
        D::Truncate               => "⚠ safe-mode: TRUNCATE",
        D::DeleteWithoutWhere     => "⚠ safe-mode: DELETE without WHERE",
        D::UpdateWithoutWhere     => "⚠ safe-mode: UPDATE without WHERE",
        D::AlterTableDestructive  => "⚠ safe-mode: destructive ALTER TABLE",
        D::DropRole               => "⚠ safe-mode: DROP ROLE",
        D::ParseFailed            => "⚠ safe-mode: couldn't fully parse this statement — confirm if it's safe",
    }
}

// ── Pane-type dropdown (shared by every pane's ⊡ button) ─────────────────────

pub(crate) fn pane_type_menu<E>(
    pane_id: String,
    plane: &'static dyn DbmsPlane,
    menu_open: RwSignal<bool>,
    add_pane: E,
) -> impl IntoView
where
    E: Fn(String, PaneKind, Option<ExplorerInit>) + Copy + 'static,
{
    view! {
        {move || {
            if !menu_open.get() { return view! { <span/> }.into_view(); }
            let items = plane.supported_panes();
            let pid_for_items = pane_id.clone();
            view! {
                <div class="pane-menu">
                    {items.iter().map(|item| {
                        let it = *item;
                        let pid = pid_for_items.clone();
                        view! {
                            <div class="pane-menu-item"
                                 on:click=move |_| {
                                     add_pane(pid.clone(), it.kind, None);
                                     menu_open.set(false);
                                 }>
                                <span class="pane-menu-icon">{it.icon}</span>
                                <span class="pane-menu-label">{it.label}</span>
                            </div>
                        }
                    }).collect_view()}
                </div>
            }.into_view()
        }}
    }
}

// ── Shell leaf renderer ───────────────────────────────────────────────────────

/// Stop a running `\watch` and clear its bounds, so the next `\watch`
/// starts from defaults rather than inheriting a previous count /
/// min_rows. Silently, as psql's ends.
pub(crate) fn stop_watch(tab: &ShellTab) {
    tab.watch_sql.set(None);
    tab.watch_interval.set(None);
    tab.watch_count.set(None);
    tab.watch_min_rows.set(None);
}

/// Start psql's `\watch` on `stmt`, repeated within `bounds`: the
/// buffer it was typed after — shown and kept like a `\g` statement's —
/// or the previous query, recalled. The first run goes at once; each
/// prints as it answers (`print_watch_run`).
pub(crate) fn start_watch(tab: &ShellTab, stmt: TypedStatement, bounds: WatchBounds) {
    let sql = if stmt.recalled {
        stmt.sql
    } else {
        let sql = tab.interpolate(&stmt.sql);
        persist_command(&tab.history_key, &stmt.history_text());
        let num = tab.query_count.get_untracked();
        tab.query_count.update(|n| *n += 1);
        tab.shell_history.update(|h| h.push(ShellEntry::Query {
            num, sql: sql.clone(), terminator: stmt.terminator, meta: false, errored: false,
        }));
        sql
    };
    tab.hist_pos.set(None);
    tab.saved_input.set(String::new());
    tab.current_input.set(String::new());
    reset_shell_input_height(&tab.id);
    // A new watch: a run of one before it, still in flight, isn't its.
    tab.watch_generation.update(|generation| *generation = generation.wrapping_add(1));
    tab.watch_sql.set(Some(sql));
    tab.watch_interval.set(Some(bounds.interval_ms));
    tab.watch_count.set(bounds.count);
    tab.watch_min_rows.set(bounds.min_rows);
    scroll_shell_to_bottom(&tab.id);
}

/// A `\watch` run answered, printed as psql prints it (`entry`: a result
/// with rows under the title its run was sent with — `watch_title`,
/// `ResultTitle::Watch` — a command's tag alone). The watch then ends,
/// without a word, after its last run (`count`) or a result with fewer
/// rows than `min_rows`.
pub(crate) fn print_watch_run(tab: &ShellTab, entries: Vec<ShellEntry>, rows: Option<usize>) {
    tab.shell_history.update(|h| h.extend(entries));
    let last_run = tab.watch_count.get_untracked().is_some_and(|left| left <= 1);
    tab.watch_count.update(|left| if let Some(n) = left { *n = n.saturating_sub(1) });
    let too_few = matches!((rows, tab.watch_min_rows.get_untracked()), (Some(rows), Some(min)) if (rows as u64) < u64::from(min));
    if last_run || too_few {
        stop_watch(tab);
    }
    scroll_shell_to_bottom(&tab.id);
}

/// psql's `\watch` title (`do_watch`): `\pset title`, if set, and a
/// tab, then the time the run started and the interval —
/// `Mon Oct 12 07:40:40 2026 (every 0.5s)`.
fn watch_title(user_title: Option<&str>, started: &str, interval_ms: u32) -> String {
    let every = c_general(f64::from(interval_ms) / 1000.0);
    match user_title {
        None => format!("{started} (every {every}s)"),
        Some(user_title) => format!("{user_title}\t{started} (every {every}s)"),
    }
}

/// `x` as C's `printf("%g")` writes it: six significant digits, trailing
/// zeros dropped, in exponent form from 1e6 up or below 1e-4
/// (`4.29497e+06`).
fn c_general(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let trim = |digits: &str| -> String {
        if digits.contains('.') { digits.trim_end_matches('0').trim_end_matches('.').to_string() } else { digits.to_string() }
    };
    // Rounded to six significant digits first: its exponent decides.
    let scientific = format!("{x:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if !(-4..6).contains(&exponent) {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exponent.abs())
    } else {
        trim(&format!("{x:.*}", (5 - exponent) as usize))
    }
}

/// A time as C's `%c` writes it in psql's default locale, `asctime`'s
/// shape: `Mon Oct  5 07:40:40 2026`. `weekday` counts from Sunday,
/// `month` from January, both from 0.
fn asctime(weekday: u32, month: u32, day: u32, hours: u32, minutes: u32, seconds: u32, year: i32) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {} {day:>2} {hours:02}:{minutes:02}:{seconds:02} {year}",
            DAYS[weekday as usize % 7], MONTHS[month as usize % 12])
}

/// Now, in the browser's time zone, as psql's `\watch` title shows it.
fn local_asctime() -> String {
    let now = js_sys::Date::new_0();
    asctime(now.get_day(), now.get_month(), now.get_date(), now.get_hours(), now.get_minutes(),
            now.get_seconds(), now.get_full_year() as i32)
}

/// Run one statement in this shell exactly as pressing Enter on it
/// would: substitute `:vars`, stop any `\watch`, number it, record it in
/// scrollback and persistent history, send it. Also how other panes run
/// SQL here (the explorer's action shell), so what they run is visible
/// and recallable like anything typed.
///
/// No-op without a live session, and during the WS-down → reconnect
/// window (`session_stale`): sending then would flip `querying` and
/// fire into a dead socket, leaving the pane stuck "Executing…" (audit
/// finding 2.7).
pub(crate) fn run_in_shell(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, stmt: TypedStatement) {
    let Some(to) = send_to(tab, ws) else { return };
    if stmt.sql.trim().is_empty() { return; }
    let sql = tab.interpolate(&stmt.sql);
    stop_watch(tab);
    let num = tab.query_count.get_untracked();
    tab.query_count.update(|n| *n += 1);
    latch_result(tab, &stmt);
    tab.hist_pos.set(None);
    tab.saved_input.set(String::new());
    // History keeps what was typed (`:var`s unexpanded, as psql's does),
    // so a variable's value — perhaps a secret — never lands in it.
    persist_command(&tab.history_key, &stmt.history_text());
    let terminator = stmt.terminator;
    tab.shell_history.update(|h| h.push(ShellEntry::Query { num, sql: sql.clone(), terminator, meta: false, errored: false }));
    say_notes(tab, stmt.notes);
    tab.current_input.set(String::new());
    reset_shell_input_height(&tab.id);
    tab.querying.set(true);
    deliver_user_statement(tab, ws, to, sql);
    scroll_shell_to_bottom(&tab.id);
}

/// The next statement of `tab`'s input for the server, taken off the
/// queue — each before it that the CLI answers itself answered on the
/// way (`cli_answer`) — and what the CLI said reading its lines, said
/// now (`TypedStatement.notes`).
fn next_statement(tab: &ShellTab) -> Option<TypedStatement> {
    let plane = plane_for(tab.db_type);
    loop {
        let mut next = tab.pending_stmts.try_update(|q| (!q.is_empty()).then(|| q.remove(0))).flatten()?;
        match cli_answer(plane, &next) {
            Some(said) => tab.shell_history.update(|h| h.push(cli_said(said.iter().map(|line| line.to_string()).collect()))),
            None => {
                say_notes(tab, std::mem::take(&mut next.notes));
                return Some(next);
            }
        }
    }
}

/// What the CLI says itself for `stmt` instead of sending it: the mysql
/// CLI's `ERROR: ` / `No query specified` for a terminator that ends
/// nothing (`TypedStatement::ends_nothing`). None: it goes to the
/// server — every statement psql's does, an empty one too.
pub(crate) fn cli_answer(plane: &dyn DbmsPlane, stmt: &TypedStatement) -> Option<&'static [&'static str]> {
    plane.empty_statement().filter(|_| stmt.ends_nothing())
}

/// What the CLI says about the lines of `text` past its first `skip`
/// bytes — read before, their notes said — each read with the lines
/// before it (`DbmsPlane::line_notes`), in order. A line read in part
/// before (a command mid-line read on after: `select 1 \i \s from
/// dual;`) loses the notes that part had.
pub(crate) fn notes_for(plane: &dyn DbmsPlane, text: &str, skip: usize, delimiter: &str) -> Vec<ShellEntry> {
    let mut notes = Vec::new();
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        let start = end;
        end += line.len();
        let read = text[..end].trim_end_matches('\n');
        if read.len() <= skip {
            continue;
        }
        let mut said = plane.line_notes(read, delimiter);
        if start < skip {
            let before = plane.line_notes(&text[..skip], delimiter).len();
            said.drain(..before.min(said.len()));
        }
        notes.extend(said);
    }
    notes
}

fn say_notes(tab: &ShellTab, notes: Vec<ShellEntry>) {
    if !notes.is_empty() {
        tab.shell_history.update(|h| h.extend(notes));
    }
}

/// What the CLI says about text left in the input — an unended
/// statement waiting for more — the lines `noted` already said aside;
/// said once what was queued before it has answered, as the CLI reads
/// it after that (`held_notes`).
pub(crate) fn say_left_notes(tab: &ShellTab, left: &str, delimiter: &str) {
    let left = left.trim_end();
    let skip = tab.noted.with_untracked(|noted| if left.starts_with(noted.as_str()) { noted.len() } else { 0 });
    let notes = notes_for(plane_for(tab.db_type), left, skip, delimiter);
    tab.noted.set(left.to_string());
    if tab.querying.get_untracked() || tab.pending_stmts.with_untracked(|q| !q.is_empty()) {
        tab.held_notes.update(|held| held.extend(notes));
    } else {
        say_notes(tab, notes);
    }
}

/// An error the CLI raises itself, as it prints it, line by line — its
/// message the last line (mysql's `ERROR: ` / `No query specified`).
fn cli_said(lines: Vec<String>) -> ShellEntry {
    let message = lines.last().cloned().unwrap_or_default();
    ShellEntry::CliError { lines, message }
}

/// Where a shell's next statement goes.
pub(crate) enum SendTo {
    /// Its live session.
    Session(String),
    /// A new session, opened first: the old one was lost, and the CLI
    /// reconnects on the next statement (mysql — `LostConnection`).
    Reopen,
    /// Nowhere: the session was lost, and this statement is how the CLI
    /// finds out — psql prints libpq's lines and resets the connection;
    /// the statement doesn't run, the rest of the input does.
    Reset,
    /// Nowhere, and the CLI says so: its session was lost and couldn't
    /// be reset (psql's "You are currently not connected to a database.").
    NotConnected(&'static str),
}

/// Where `tab`'s next statement goes; None: nowhere, silently — no
/// session yet, a lost one on a plane with no CLI behaviour for it (the
/// banner offers to reconnect), noctua itself unreachable
/// (`session_stale` with the id kept, or the socket down: a reconnect
/// request would be dropped), or a lost session already being replaced.
pub(crate) fn send_to(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>) -> Option<SendTo> {
    let stale = tab.session_stale.get_untracked();
    match tab.session_id.get_untracked() {
        Some(sid) if !stale => Some(SendTo::Session(sid)),
        None if stale && tab.reopening.with_untracked(Option::is_none) => match plane_for(tab.db_type).lost_connection() {
            LostConnection::OnNextStatement { .. } if ws_open(ws) => Some(SendTo::Reopen),
            LostConnection::ResetAtOnce { not_connected, .. } if tab.disconnected.get_untracked() =>
                Some(SendTo::NotConnected(not_connected)),
            LostConnection::ResetAtOnce { .. } if ws_open(ws) => Some(SendTo::Reset),
            _ => None,
        },
        _ => None,
    }
}

/// Send `sql` where `to` says. To a lost session's replacement as the
/// mysql CLI does: its lines (the "Lost connection" error only while it
/// doesn't know the session is gone), a new session on the same
/// database, and `sql` once it's open (SessionOpened, `reopening`).
pub(crate) fn deliver(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, to: SendTo, sql: String) {
    match to {
        SendTo::Session(sid) => send_statement(ws, tab.sent_sql, sid, sql),
        SendTo::Reopen => reopen(tab, ws, Reopening::Resend(sql)),
        SendTo::Reset => {
            forget_result_latches(tab);
            reset_lost(tab, ws);
        }
        SendTo::NotConnected(message) => {
            forget_result_latches(tab);
            tab.shell_history.update(|h| h.push(ShellEntry::CliError { lines: vec![message.into()], message: message.into() }));
            tab.querying.set(false);
        }
    }
}

/// A lost session's replacement, opened as the mysql CLI opens one: its
/// lines (the "Lost connection" error only while it doesn't know the
/// session is gone), a new session on the same database, and `then` once
/// it's open (SessionOpened, `reopening`).
fn reopen(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, then: Reopening) {
    if let LostConnection::OnNextStatement { error, notice } = plane_for(tab.db_type).lost_connection() {
        tab.shell_history.update(|h| {
            if !tab.disconnected.get_untracked() {
                h.push(ShellEntry::CliError { lines: vec![error.into()], message: error.into() });
            }
            h.push(ShellEntry::MetaOutput(vec![notice.into()]));
        });
    }
    tab.reopening.set(Some(then));
    reconnect(tab, ws);
}

/// Tag prefix of the warnings asked for after a statement
/// (`fetch_warnings`); the rest is the statement's error number, when it
/// failed.
const WARNINGS_TAG: &str = "warnings:";

pub(crate) fn is_warnings_tag(tag: &str) -> bool {
    tag.starts_with(WARNINGS_TAG)
}

/// After a statement that had `warnings`, or failed with `error`, under
/// the mysql CLI's `warnings` (`\W`): its `print_warnings` — the server
/// asked for them (`DbmsPlane::warnings_query`) — before the next
/// statement runs. Whether they were asked for: the rest of the input
/// goes on once they're printed (`print_warnings`).
pub(crate) fn fetch_warnings(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, warnings: u16, error: Option<u16>) -> bool {
    if !tab.show_warnings.get_untracked() || (warnings == 0 && error.is_none()) {
        return false;
    }
    let (Some(query), Some(sid)) = (plane_for(tab.db_type).warnings_query(), tab.session_id.get_untracked()) else {
        return false;
    };
    tab.querying.set(true);
    let tag = format!("{WARNINGS_TAG}{}", error.map(|e| e.to_string()).unwrap_or_default());
    ws_send(ws, &ClientMessage::IntrospectBatch { session_id: sid, tag, queries: vec![query.into()] });
    true
}

/// The warnings asked for after a statement, printed as the CLI prints
/// them (`DbmsPlane::warning_lines`) — then the rest of the input. A
/// session lost meanwhile is the lost-session handling's (SessionClosed
/// follows): nothing more goes to it from here.
pub(crate) fn print_warnings(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, tag: &str, result: shared::BatchResult, connection_lost: bool) {
    if connection_lost {
        tab.querying.set(false);
        return;
    }
    let error = tag.strip_prefix(WARNINGS_TAG).and_then(|e| e.parse().ok());
    let lines = result.sets.first().map(|set| plane_for(tab.db_type).warning_lines(set, error)).unwrap_or_default();
    if !lines.is_empty() {
        tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(lines)));
    }
    if !run_next_queued(tab, ws) {
        tab.querying.set(false);
    }
    scroll_shell_to_bottom(&tab.id);
}

/// What `stmt`'s result needs, latched for its reply: vertical output
/// (its `\gx` / `\G`, or `\x` on) and its `\g`-family action.
fn latch_result(tab: &ShellTab, stmt: &TypedStatement) {
    tab.pending_vertical.set(stmt.vertical || tab.expanded.get_untracked() == Expanded::On);
    tab.pending_post_action.set(stmt.post.clone());
}

/// A statement that won't run: what was latched for its result
/// (vertical output, a `\g`-family action, a meta-command's no-rows
/// message) mustn't reach the next one's.
fn forget_result_latches(tab: &ShellTab) {
    tab.pending_vertical.set(false);
    tab.pending_post_action.set(None);
    tab.pending_list.set(None);
}

/// Send the next statement waiting in `tab`'s input, its result's
/// latches set, once the one before it has answered. False when none
/// is waiting.
pub(crate) fn run_next_queued(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>) -> bool {
    // A statement psql wrapped isn't over until its savepoint is closed.
    if close_wrap(tab, ws) {
        return true;
    }
    finish_gexec(tab);
    if tab.pending_stmts.with_untracked(Vec::is_empty) {
        say_notes(tab, tab.held_notes.try_update(std::mem::take).unwrap_or_default());
    }
    let Some(next) = next_statement(tab) else { return false };
    latch_result(tab, &next);
    tab.querying.set(true);
    if let Some(sid) = tab.session_id.get_untracked() {
        send_user_statement(tab, ws, sid, WrappedSend::Query(next.sql));
    }
    true
}

/// A command found `tab`'s session lost on a plane that resets at once
/// (psql's `CheckConnection`): libpq's lines, and a new session — the
/// rest of the input (`pending_stmts`) goes on in it (SessionOpened).
pub(crate) fn reset_lost(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>) {
    mark_lost(tab);
    reconnect(tab, ws);
}

/// A command found its session lost, on a plane that resets the
/// connection at once (psql's `CheckConnection`): its error `entry`
/// gets libpq's word for a connection that closed under it — after the
/// server's FATAL, if it sent one; sqlx's own text says nothing psql
/// would — and the reset is under way (`Reopening::Reset`: the
/// SessionClosed that follows opens the new session). Whether it resets.
pub(crate) fn resets_lost(tab: &ShellTab, entry: &mut ShellEntry) -> bool {
    let LostConnection::ResetAtOnce { lost, .. } = plane_for(tab.db_type).lost_connection() else { return false };
    if tab.kind != PaneKind::Shell {
        return false;
    }
    tab.reopening.set(Some(Reopening::Reset));
    let lost = lost.iter().map(|line| line.to_string());
    *entry = match std::mem::replace(entry, ShellEntry::MetaOutput(Vec::new())) {
        ShellEntry::CliError { mut lines, message } => {
            lines.extend(lost);
            ShellEntry::CliError { lines, message }
        }
        _ => {
            let lines: Vec<String> = lost.collect();
            ShellEntry::CliError { message: lines.first().cloned().unwrap_or_default(), lines }
        }
    };
    true
}

/// [`reset_lost`] for a command whose reply already said the session
/// died: the SessionClosed that follows opens the new one (main.rs).
pub(crate) fn mark_lost(tab: &ShellTab) {
    if let LostConnection::ResetAtOnce { lost, .. } = plane_for(tab.db_type).lost_connection() {
        let lines: Vec<String> = lost.iter().map(|l| l.to_string()).collect();
        let message = lines.first().cloned().unwrap_or_default();
        tab.shell_history.update(|h| h.push(ShellEntry::CliError { lines, message }));
        tab.querying.set(true);
        tab.reopening.set(Some(Reopening::Reset));
    }
}

/// The mysql CLI's `connect` (`com_connect`): the session closed first
/// (`sql_real_connect`), then a new one on `database` — the one it's on
/// when none is named — which is the shell's database from now on, so a
/// reconnect after a failure tries it again, as the CLI's does.
pub(crate) fn connect_anew(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, database: Option<String>) {
    let named = database.is_some();
    if let Some(database) = database {
        tab.current_db.set(database);
    }
    if let Some(old) = tab.session_id.get_untracked() {
        tab.session_id.set(None);
        ws_send(ws, &ClientMessage::Disconnect { session_id: old });
    }
    tab.session_stale.set(true);
    tab.txn_state.set(TxnState::Idle);
    tab.querying.set(true);
    tab.reopening.set(Some(Reopening::Connect { named }));
    reconnect(tab, ws);
}

/// Open a new session for `tab`, on the database it was on, replacing
/// a lost one (`reopening` says what follows).
pub(crate) fn reconnect(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>) {
    ws_send(ws, &ClientMessage::Connect {
        connection_id: tab.conn_id.clone(),
        client_tab_id: tab.id.clone(),
        database:      tab.reconnect_database(),
        client_role:   Some(tab.kind.application_role().to_string()),
        timezone:   crate::settings::session_time_zone(),
    });
}

/// Send `sql` to run on a shell's session (`sent`: its `sent_sql`),
/// remembering it as what's running — the server's error positions
/// count into exactly this text, which a `\gexec` statement or a
/// re-substituted batch statement has nowhere else.
pub(crate) fn send_statement(ws: RwSignal<Option<WebSocket>>, sent: RwSignal<Option<String>>, session_id: String, sql: String) {
    sent.set(Some(sql.clone()));
    ws_send(ws, &ClientMessage::ExecuteQuery { session_id, sql });
}

/// A result's rows as they came: the columns, each column's type as the
/// server identifies it (`QueryResult.types`; none for `\gdesc`'s), and
/// the cells, SQL NULL as `None`.
pub(crate) struct Rows {
    pub columns:   Vec<String>,
    pub types:     Vec<String>,
    pub rows:      Vec<Vec<Option<String>>>,
    /// Printed as psql's `printQuery` prints a result — numbers in the
    /// client's locale under `\pset numericlocale` — rather than as
    /// `\crosstabview`'s grid, whose values print as they are.
    pub localized: bool,
}

/// The title psql's printer puts over a result: a list's (`List of
/// tables` — and whether psql counts its rows, and the footers it
/// prints instead), a `\watch` run's — `\pset title` is in that — or
/// else `\pset title`.
pub(crate) enum ResultTitle {
    Pset,
    List { title: String, counts_rows: bool, footers: Vec<String> },
    Watch(String),
}

/// Whether a result shows vertically in the grid: asked for (its `\gx` /
/// `\G`, `\x` on) — or, under psql's `\x auto`, wider than the pane (or
/// `\pset columns`), as psql's printer decides it
/// (`DbmsPlane::expands_automatically`).
fn shows_vertically(tab: &ShellTab, asked: bool, columns: &[String], rows: &[Vec<String>]) -> bool {
    asked || (tab.expanded.get_untracked() == Expanded::Auto && !columns.is_empty()
        && plane_for(tab.db_type).expands_automatically(columns, rows, &tab.print_options(false)))
}

/// A result as the shell prints it: a statement's without rows
/// (`columnless_result`), or its rows (`rows_entry`) — and under them,
/// for a RETURNING, its tag (`status_after_rows`) — timed when
/// `\timing` says, the time last.
pub(crate) fn result_entry(
    tab:          &ShellTab,
    rows:         Rows,
    status:       shared::CommandStatus,
    exec_time_us: u64,
    vertical:     bool,
    title:        ResultTitle,
) -> Vec<ShellEntry> {
    if rows.columns.is_empty() {
        return vec![columnless_result(tab, status, exec_time_us)];
    }
    let (timed, after) = after_rows(tab, &status, exec_time_us);
    std::iter::once(rows_entry(tab, rows, status.warnings, exec_time_us, timed, vertical, title)).chain(after).collect()
}

/// What follows a result's rows, as psql's `PrintQueryResult` prints it:
/// whether the rows' own entry carries the time, and the entry after
/// them — a RETURNING's tag (`DbmsPlane::status_after_rows`, worded as
/// `command_status` words it, none under `QUIET`) and then the time.
pub(crate) fn after_rows(tab: &ShellTab, status: &shared::CommandStatus, exec_time_us: u64) -> (bool, Option<ShellEntry>) {
    let plane = plane_for(tab.db_type);
    let timed = tab.show_timing.get_untracked();
    if tab.quiet() || !plane.status_after_rows(status) {
        return (timed, None);
    }
    let said = plane.command_status(status, tab.output_format.get_untracked());
    (false, Some(ShellEntry::Info { exec_time_us, said, timed }))
}

/// What follows rows that weren't printed (`\gset`'s, a pivot that
/// failed): as `after_rows` says it — the time alone when no tag comes.
pub(crate) fn after_unprinted_rows(tab: &ShellTab, status: &shared::CommandStatus, exec_time_us: u64) -> Option<ShellEntry> {
    let (timed, after) = after_rows(tab, status, exec_time_us);
    after.or_else(|| timed.then(|| ShellEntry::Info { exec_time_us, said: Vec::new(), timed }))
}

/// Rows as the shell prints them in the `\pset format` (or `\output`) it
/// is in — expanded when `vertical` (`\gx`, `\x on`) — under their
/// title, with the CLI's footer and, when `timed`, the time: aligned, as
/// the shell's grid; psql's other formats as psql prints them
/// (`DbmsPlane::print_result`, the title in the text); Parliament's json
/// and markdown.
pub(crate) fn rows_entry(
    tab:          &ShellTab,
    rows:         Rows,
    warnings:     u16,
    exec_time_us: u64,
    timed:        bool,
    vertical:     bool,
    title:        ResultTitle,
) -> ShellEntry {
    let plane = plane_for(tab.db_type);
    let Rows { columns, types, rows, localized } = rows;
    let format = tab.output_format.get_untracked();
    // A list psql prints without its row count (`\du`).
    let counts_rows = !matches!(title, ResultTitle::List { counts_rows: false, .. });
    // A list is never timed: psql's `PSQLexec` runs it, which doesn't.
    let timed = timed && !matches!(title, ResultTitle::List { .. });
    // A list's own footers, in place of the row count (`\dn`'s
    // `Publications:`).
    let footers = match &title {
        ResultTitle::List { footers, .. } => footers.clone(),
        _ => Vec::new(),
    };
    // The title psql's printer gets (`myopt.title`): a list's in place of
    // `\pset title`; a `\watch` run's — `\pset title` is in that — ending
    // its line itself (`do_watch`: `"%s (every %gs)\n"`), so a blank
    // line follows it.
    let watched = matches!(title, ResultTitle::Watch(_));
    let title = match title {
        ResultTitle::Pset => tab.pset_title.get_untracked(),
        ResultTitle::List { title, .. } => Some(title),
        ResultTitle::Watch(title) => Some(format!("{title}\n")),
    };
    match format {
        OutputFormat::Json | OutputFormat::Markdown => {
            let body = if format == OutputFormat::Json {
                crate::export::to_json(&columns, &rows)
            } else {
                let mut md = format!("| {} |\n", columns.join(" | "));
                md.push_str(&format!("| {} |\n", columns.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")));
                for row in &rows {
                    let null = tab.null_display.get_untracked();
                    let cells: Vec<&str> = row.iter().map(|cell| cell.as_deref().unwrap_or(&null)).collect();
                    md.push_str(&format!("| {} |\n", cells.join(" | ")));
                }
                md
            };
            // Parliament's own formats: the data alone — under a `\watch`
            // run's title (none under `\t`), which says which run it is.
            let mut lines: Vec<String> = title.filter(|_| watched && !tab.pset_tuples_only.get_untracked())
                .map(|title| title.trim_end_matches('\n').lines().map(String::from).chain([String::new()]).collect())
                .unwrap_or_default();
            lines.extend(body.lines().map(String::from));
            // json carries the data alone, no row count; nor does a list
            // psql prints without one. A list's own footers stand in for
            // the count.
            let footer = result_footer(plane, rows.len(), warnings, false, exec_time_us, timed);
            let footer: Vec<String> = if format == OutputFormat::Json || !counts_rows {
                footer.into_iter().filter(|l| l.starts_with("Time:")).collect()
            } else if !footers.is_empty() && !tab.pset_tuples_only.get_untracked() {
                footers
            } else {
                footer
            };
            if !footer.is_empty() { lines.push(String::new()); }
            lines.extend(footer);
            ShellEntry::MetaOutput(lines)
        }
        _ => {
            // Printed with the settings in force now, under its title.
            let mut style = tab.printed_with(title);
            style.footer &= counts_rows;
            style.footers = footers;
            if !localized {
                style.numeric_locale = None;
            }
            let mut options = tab.print_options(vertical);
            options.footer &= counts_rows;
            let aligns: Vec<Align> = types.iter().map(|t| plane.column_align(t)).collect();
            let shown: Vec<Vec<String>> = rows.iter().map(|row| style.row(plane, row, &aligns)).collect();
            if let Some(mut lines) = plane.print_result(&columns, &types, &shown, style.title.as_deref(), &style.footers, &options) {
                lines.extend(footer_and_time(plane, 0, 0, false, exec_time_us, timed).1);
                return ShellEntry::Printed { lines, links: Vec::new() };
            }
            if shows_vertically(tab, vertical, &columns, &shown) {
                ShellEntry::Vertical { columns, aligns, rows, exec_time_us, warnings, timed, style }
            } else {
                ShellEntry::Table { columns, aligns, rows_affected: rows.len() as u64, rows, exec_time_us, warnings, timed, style }
            }
        }
    }
}

/// The entry for a result without columns: a statement that returns no
/// rows, or — on MySQL, whose empty result has no columns — an empty
/// one, shown as "Empty set". (A Postgres result that returns rows
/// always has its columns.)
pub(crate) fn columnless_result(tab: &ShellTab, status: shared::CommandStatus, exec_time_us: u64) -> ShellEntry {
    let plane = plane_for(tab.db_type);
    let sent = tab.sent_sql.get_untracked().unwrap_or_default();
    let timed = tab.show_timing.get_untracked();
    if plane.empty_set().is_some() && crate::sql_inspect::returns_rows(&sent, plane.sql_dialect()) {
        ShellEntry::Table { columns: Vec::new(), aligns: Vec::new(), rows: Vec::new(), exec_time_us, rows_affected: status.rows_affected, warnings: status.warnings, timed, style: tab.printed_with(None) }
    } else {
        // psql's `PrintQueryStatus`: nothing under `QUIET`.
        let said = if tab.quiet() { Vec::new() } else { plane.command_status(&status, tab.output_format.get_untracked()) };
        ShellEntry::Info { exec_time_us, said, timed }
    }
}

/// What goes under a result, as its CLI prints it: the footer — with the
/// time at its end where the CLI puts it there ("2 rows in set, 1 warning
/// (0.002 sec)") — and the time when it gets a line of its own ("(2
/// rows)", then "Time: 0.205 ms"). `timed`: times are shown.
pub(crate) fn footer_and_time(
    plane: &dyn DbmsPlane, rows: usize, warnings: u16, vertical: bool, exec_time_us: u64, timed: bool,
) -> (Option<String>, Option<String>) {
    let footer = plane.rows_footer(rows, warnings, vertical);
    match timed.then(|| plane.timing(exec_time_us)) {
        Some(Timing::Inline(time)) => (Some(footer.map_or_else(|| time.clone(), |f| format!("{f} {time}"))), None),
        Some(Timing::OwnLine(time)) => (footer, Some(time)),
        None => (footer, None),
    }
}

/// What shows under a result's rows, as lines: its footer, unless the
/// result printed without one (`\t`, `\pset footer off`), and its time.
fn shown_footer(
    plane: &dyn DbmsPlane, rows: usize, warnings: u16, vertical: bool, exec_time_us: u64, timed: bool, style: &PrintedWith,
) -> Vec<String> {
    let (footer, time) = footer_and_time(plane, rows, warnings, vertical, exec_time_us, timed);
    footer.filter(|_| style.shows_footer()).into_iter().chain(time).collect()
}

/// [`footer_and_time`] as lines — the text formats' trailer.
pub(crate) fn result_footer(plane: &dyn DbmsPlane, rows: usize, warnings: u16, vertical: bool, exec_time_us: u64, timed: bool) -> Vec<String> {
    let (footer, time) = footer_and_time(plane, rows, warnings, vertical, exec_time_us, timed);
    footer.into_iter().chain(time).collect()
}

/// A statement's status lines as its CLI printed them, the first with
/// its time when `timed`.
fn command_lines(plane: &dyn DbmsPlane, said: &[String], exec_time_us: u64, timed: bool) -> Vec<String> {
    let mut lines = said.to_vec();
    match timed.then(|| plane.timing(exec_time_us)) {
        Some(Timing::Inline(time)) => match lines.first_mut() {
            Some(first) => { first.push(' '); first.push_str(&time); }
            None => lines.push(time),
        },
        Some(Timing::OwnLine(time)) => lines.push(time),
        None => {}
    }
    lines
}

/// A command the server refused: the scrollback entry it was (to mark),
/// the text the error's position counts into, and whether it was a
/// meta-command's own SQL rather than the user's.
#[derive(Debug, PartialEq)]
pub(crate) struct FailedCommand {
    pub entry: Option<usize>,
    pub query: Option<String>,
    pub meta:  bool,
}

/// Which command failed, given the SQL last sent (`ShellTab.sent_sql`)
/// and how many statements are still queued. A batch puts all its
/// statements in the scrollback before running them, so the candidate
/// sits `remaining` statements from the end; it's the one that ran when
/// its text is what was sent — shown with its `;`, as psql's `LINE n:`
/// shows it — or, being a meta-command (whose SQL isn't in the
/// scrollback), when nothing is queued. Otherwise it was a statement
/// `\gexec` generated, which has no entry (queued ahead of the rest of
/// the input, it throws the count off; the text check catches that).
pub(crate) fn failed_command(history: &[ShellEntry], remaining: usize, sent: Option<&str>) -> FailedCommand {
    let candidate = history.iter().enumerate().rev()
        .filter(|(_, e)| matches!(e, ShellEntry::Query { .. }))
        .nth(remaining);
    match candidate {
        Some((at, ShellEntry::Query { sql, terminator, .. })) if Some(sql.as_str()) == sent => FailedCommand {
            entry: Some(at),
            // With its `;`, and the line break before it if there was one.
            query: Some(match terminator.find(';').filter(|&i| terminator[..i].trim().is_empty()) {
                Some(i) => format!("{sql}{}", &terminator[..=i]),
                None => sql.clone(),
            }),
            meta:  false,
        },
        Some((at, ShellEntry::Query { meta: true, .. })) if remaining == 0 => FailedCommand {
            entry: Some(at),
            query: sent.map(String::from),
            meta:  true,
        },
        _ => FailedCommand { entry: None, query: sent.map(String::from), meta: false },
    }
}

/// What the shell shows for a command the server refused — the error as
/// this dialect's CLI prints it — with psql's bookkeeping: the last
/// error (`\errverbose`) and, with `result_variables`, `:ERROR`,
/// `:SQLSTATE`, `:ROW_COUNT`, `:LAST_ERROR_SQLSTATE`,
/// `:LAST_ERROR_MESSAGE` — not for a meta-command's own SQL, a `\watch`
/// run or psql's own `BEGIN` / savepoint, which psql's
/// `SetResultVariables` leaves out. `query` is the statement that
/// failed. An error without the server's fields (a lost connection)
/// shows its message.
pub(crate) fn command_error(tab: &ShellTab, message: String, error: Option<shared::DbError>, query: Option<String>, result_variables: bool) -> ShellEntry {
    if result_variables {
        let sqlstate = error.as_ref().and_then(|e| e.sqlstate.clone()).unwrap_or_default();
        let primary = error.as_ref().map_or_else(|| message.clone(), |e| e.message.clone());
        set_result_variables(tab, CommandOutcome::Failed { sqlstate: &sqlstate, message: &primary });
    }
    let Some(error) = error else {
        let error = shared::DbError { message: message.clone(), ..shared::DbError::default() };
        tab.last_error.set(Some(LastError { error, query }));
        return ShellEntry::Error(message);
    };
    let style = tab.variables.with_untracked(crate::psql_vars::error_style);
    let lines = plane_for(tab.db_type).format_error(&error, query.as_deref(), style);
    tab.last_error.set(Some(LastError { error, query }));
    ShellEntry::CliError { lines, message }
}

/// How a command ended, for psql's result variables.
pub(crate) enum CommandOutcome<'a> {
    /// It succeeded, with this many rows (returned, or affected).
    Done { row_count: u64 },
    /// It failed: its error's SQLSTATE and primary message — none when
    /// the result it's judged by has no error (a `\gexec` whose
    /// statements failed).
    Failed { sqlstate: &'a str, message: &'a str },
}

impl GexecPending {
    /// How the `\gexec` ended: as its own statement did, unless one it
    /// generated failed — failed then, with no SQLSTATE or message, as
    /// psql judges it by its own error-free result.
    pub(crate) fn outcome(&self) -> CommandOutcome<'static> {
        match self.failed {
            false => CommandOutcome::Done { row_count: self.row_count },
            true => CommandOutcome::Failed { sqlstate: "", message: "" },
        }
    }
}

/// psql's `SetResultVariables`: `:ERROR`, `:SQLSTATE`, `:ROW_COUNT` and,
/// for a failure, `:LAST_ERROR_SQLSTATE` and `:LAST_ERROR_MESSAGE`.
pub(crate) fn set_result_variables(tab: &ShellTab, outcome: CommandOutcome) {
    tab.variables.update(|m| match outcome {
        CommandOutcome::Done { row_count } => {
            m.insert("ERROR".into(), "false".into());
            m.insert("SQLSTATE".into(), "00000".into());
            m.insert("ROW_COUNT".into(), row_count.to_string());
        }
        CommandOutcome::Failed { sqlstate, message } => {
            m.insert("ERROR".into(), "true".into());
            m.insert("SQLSTATE".into(), sqlstate.into());
            m.insert("ROW_COUNT".into(), "0".into());
            m.insert("LAST_ERROR_SQLSTATE".into(), sqlstate.into());
            m.insert("LAST_ERROR_MESSAGE".into(), message.into());
        }
    });
}

/// Set psql variable `name` to `value` (`\set`, `\prompt`; None:
/// `\unset`) as psql's hooks set it (`psql_vars::assign`: `\unset
/// AUTOCOMMIT` is `off`, a bad value refused in psql's words, the old one
/// kept), and its effect on the shell with it.
pub(crate) fn set_variable(tab: &ShellTab, name: &str, value: Option<String>) -> Result<(), Vec<String>> {
    let stored = match crate::psql_vars::assign(name, value.as_deref()) {
        Some(assigned) => Some(assigned?),
        None => value,
    };
    match name {
        "ON_ERROR_STOP" => tab.on_error_stop.set(stored.as_deref().and_then(crate::psql_vars::parse_bool).unwrap_or(false)),
        "PROMPT1" => tab.prompt1.set(stored.clone().unwrap_or_else(|| crate::psql_prompt::DEFAULT_PROMPT1.into())),
        _ => {}
    }
    tab.variables.update(|m| match stored {
        Some(value) => { m.insert(name.to_string(), value); }
        None => { m.remove(name); }
    });
    Ok(())
}

/// The previous query as `plane`'s CLI keeps it — what an empty buffer's
/// `\g`, `\e`, `\watch`, `\p` … fall back to (`DbmsPlane::previous_buffer`):
/// psql's as it sent it, `;` and all. Meta-commands don't count (psql's
/// previous query is the last one sent) — but an empty statement the
/// mysql CLI answered does (`DbmsPlane::previous_buffer_after`).
fn previous_buffer(plane: &dyn DbmsPlane, history: &[ShellEntry]) -> Option<String> {
    history.iter().rev().find_map(|entry| match entry {
        ShellEntry::Query { sql, terminator, meta: true, .. } => plane.previous_buffer_after(sql, terminator),
        entry => sent_statement(entry).map(|(sql, terminator)| plane.previous_buffer(sql, terminator)),
    })
}

/// A statement sent, as `entry` records it — its SQL and terminator as
/// typed (`;`, its `\g` line) — that becomes the previous query: psql's
/// `previous_buf` is the last buffer its `MainLoop` sent, at a `;` or by
/// a `\g`-family command. Not one a `\watch` ran: psql's runs the
/// buffer itself (`exec_command_watch`) and leaves the previous query as
/// it was. A meta-command's entry is the plane's to judge
/// (`DbmsPlane::previous_buffer_after`).
fn sent_statement(entry: &ShellEntry) -> Option<(&str, &str)> {
    match entry {
        ShellEntry::Query { sql, terminator, meta: false, .. } if !terminator.trim_start().starts_with("\\watch") =>
            Some((sql.as_str(), terminator.as_str())),
        _ => None,
    }
}

/// A `\g`-family command that runs the query buffer: what it asks of
/// the result — vertical for this one (`\gx`), or an action on it
/// (`\gexec`, `\gset`, `\crosstabview`).
fn runs_buffer(action: &MetaAction) -> Option<(bool, Option<PostAction>)> {
    Some(match action {
        MetaAction::Go { expanded } => (*expanded, None),
        MetaAction::Gexec => (false, Some(PostAction::Gexec)),
        MetaAction::Gset { prefix } => (false, Some(PostAction::Gset { prefix: prefix.clone() })),
        MetaAction::Crosstabview { col_v, col_h, col_d, sortcol_h } => (false, Some(PostAction::Crosstabview {
            col_v: col_v.clone(), col_h: col_h.clone(), col_d: col_d.clone(), sortcol_h: sortcol_h.clone(),
        })),
        _ => return None,
    })
}

/// The query buffer of an input ending in a backslash command —
/// everything before the command — and the command as typed, the space
/// or newline before it included: `("select 1", " \\gx")` from
/// `"select 1 \\gx"`, `("select 1", "\n\\gx")` from `"select 1\n\\gx"`.
/// The command starts where psql and the mysql CLI read it: the first
/// backslash of the last line outside quotes and comments.
fn split_buffer(input: &str, lexicon: Lexicon) -> (&str, &str) {
    let input = input.trim_end();
    let at = crate::parser::trailing_command(input, lexicon).unwrap_or(0);
    let buffer = input[..at].trim_end();
    (buffer.trim_start(), &input[buffer.len()..])
}

/// What a `command` that works on the query buffer runs: the `buffer`,
/// or the previous query when that's empty — as `plane`'s CLI keeps it,
/// psql's `;` and all, as psql sends it again (`copy_previous_query`) —
/// or, with neither, the empty query psql sends then (`\g` in a new
/// shell: the server answers with nothing). The command stays on as
/// the statement's terminator — the block shows it, and recalling it
/// runs it again.
fn buffer_statement(plane: &dyn DbmsPlane, buffer: &str, command: &str, history: &[ShellEntry]) -> TypedStatement {
    if !buffer.is_empty() {
        return TypedStatement::new(buffer, command);
    }
    let command = command.trim_start();
    match previous_buffer(plane, history).filter(|sql| !sql.is_empty()) {
        Some(sql) => TypedStatement::recalled(sql, format!("\n{command}")),
        None => TypedStatement::recalled("", command),
    }
}

/// How the CLI found a command (`first_command`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum CommandKind {
    /// A backslash command (`DbmsPlane::meta_cmd_in`).
    Backslash,
    /// A named command typed alone on a line (`DbmsPlane::named_command`).
    Line,
    /// A statement its delimiter ended that is a named command
    /// (`DbmsPlane::statement_command`).
    Statement,
}

/// A command in an input: where it starts, where its text ends — its
/// line's end, or a statement's delimiter — where the input reads on
/// after it, and how it was found.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FoundCommand {
    at:     usize,
    end:    usize,
    resume: usize,
    kind:   CommandKind,
}

/// The first command of `text`, in reading order, as `plane`'s CLI
/// reads its input: a backslash that starts one of its commands (any,
/// for psql; the mysql CLI's letters but `\g` / `\G`, which end a
/// statement), a line typed while no statement is pending that is one of
/// its named commands (`use db`, `delimiter //`, `quit`), or a statement
/// its delimiter ends that is one (the mysql CLI's `status;`) — of the
/// places `parser::command_starts` lists, the first the plane takes for
/// a command.
fn first_command(text: &str, plane: &dyn DbmsPlane, delimiter: &str) -> Option<FoundCommand> {
    use crate::parser::CommandStart;
    let line_end = |at: usize| text[at..].find('\n').map_or(text.len(), |n| at + n);
    // Where the text from `at` to `end` starts, past whitespace.
    let past_space = |at: usize, end: usize| end - text[at..end].trim_start().len();
    crate::parser::command_starts(text, delimiter, plane.lexicon()).into_iter().find_map(|start| match start {
        CommandStart::Backslash(at) => {
            let end = line_end(at);
            plane.meta_cmd(&text[at..end]).is_some()
                .then_some(FoundCommand { at, end, resume: end, kind: CommandKind::Backslash })
        }
        CommandStart::Line(at) => {
            let end = line_end(at);
            plane.named_command(&text[at..end], delimiter).is_some()
                .then(|| FoundCommand { at: past_space(at, end), end, resume: end, kind: CommandKind::Line })
        }
        CommandStart::Statement { start, end } => plane.statement_command(&text[start..end]).is_some()
            .then(|| FoundCommand { at: past_space(start, end), end, resume: end + delimiter.len(), kind: CommandKind::Statement }),
    })
}

/// Whether `tab`'s shell is busy: waiting for a reply (a statement, a
/// command's catalog queries or lookup, a new session), for its user (a
/// prompt, the editor, a safe-mode confirm), or watching. What's left
/// of an input reads on once it isn't (`resume_input`).
pub(crate) fn shell_busy(tab: &ShellTab) -> bool {
    tab.querying.get()
        || tab.pending_stmts.with(|q| !q.is_empty())
        || tab.pending_meta.with(Option::is_some)
        || tab.switching.with(Option::is_some)
        || tab.reopening.with(Option::is_some)
        || tab.prompt_state.with(Option::is_some)
        || tab.password_prompt.with(Option::is_some)
        || tab.external_edit.with(Option::is_some)
        || tab.pending_confirm.with(Option::is_some)
        || tab.watch_sql.with(Option::is_some)
        || tab.wrap.with(Option::is_some)
}

/// The rest of `tab`'s input is dropped, unrun — psql's `ON_ERROR_STOP`
/// after an error, or a statement whose connection couldn't be had
/// back: the statements queued and what was left to read.
pub(crate) fn drop_rest(tab: &ShellTab) {
    tab.pending_stmts.set(Vec::new());
    tab.resume_input.set(None);
    finish_gexec(tab);
}

/// A statement a `\gexec` generated failed: so does the `\gexec`, once
/// they've all run (psql's `ExecQueryTuples`; `finish_gexec`). Nothing
/// while none is running.
pub(crate) fn mark_gexec_failed(tab: &ShellTab) {
    tab.gexec.update(|gexec| if let Some(gexec) = gexec { gexec.failed = true; });
}

/// A `\gexec` done — the statements it generated have run, the queue
/// down to what followed them (or dropped): its own statement's status
/// printed after them, and the result variables set from it — failed
/// when one of them failed, with no SQLSTATE or message of its own — as
/// psql's `ExecQueryAndProcessResults` does once `ExecQueryTuples`
/// returns.
fn finish_gexec(tab: &ShellTab) {
    let queued = tab.pending_stmts.with_untracked(Vec::len);
    let Some(gexec) = tab.gexec.get_untracked().filter(|gexec| queued <= gexec.rest) else { return };
    tab.gexec.set(None);
    if let Some(trailer) = gexec.trailer.clone() {
        tab.shell_history.update(|h| h.push(trailer));
    }
    set_result_variables(tab, gexec.outcome());
}

/// A statement shortened for a one-line mention (the safe-mode dialog,
/// `\watch`): at most 80 characters, cut on a character boundary so a
/// multi-byte name can't panic it.
fn sql_preview(sql: &str) -> String {
    if sql.chars().count() > 80 {
        format!("{}…", sql.chars().take(77).collect::<String>())
    } else {
        sql.to_string()
    }
}

// ── Staged meta-commands (psql's `\d <table>`) ─────────────────────────────

/// Tag prefix of a staged meta-command's batches; the rest is its nonce.
const META_TAG: &str = "meta:";

/// The tag of what psql's `SendQuery` sends around a statement.
const TXN_TAG: &str = "txn:";

/// Send a statement the user runs (`what`) on session `sid` as psql's
/// `SendQuery` sends it: first what the plane says goes before it
/// (`statement_opening`: `BEGIN`, its savepoint) as a batch of its own —
/// `continue_wrap` sends the statement on its reply — and, once it has
/// answered, what closes the savepoint (`close_wrap`). Nothing to send
/// first: the statement now.
pub(crate) fn send_user_statement(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, sid: String, what: WrappedSend) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let plane = plane_for(tab.db_type);
    let opening = tab.variables.with_untracked(|vars| plane.statement_opening(what.sql(), tab.txn_state.get_untracked(), vars));
    let Some(crate::plane::Opening { queries, savepoint }) = opening else {
        return send_now(tab, ws, sid, what);
    };
    let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
    // Its errors are this statement's.
    tab.sent_sql.set(Some(what.sql().to_string()));
    tab.wrap.set(Some(Wrap { nonce, stage: WrapStage::Opening { send: what, savepoint } }));
    ws_send(ws, &ClientMessage::IntrospectBatch { session_id: sid, tag: format!("{TXN_TAG}{nonce}"), queries });
}

/// Send a statement the user runs, as it is.
fn send_now(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, sid: String, what: WrappedSend) {
    match what {
        WrappedSend::Query(sql) => send_statement(ws, tab.sent_sql, sid, sql),
        WrappedSend::Describe(sql) => {
            tab.sent_sql.set(Some(sql.clone()));
            ws_send(ws, &ClientMessage::DescribeQuery { session_id: sid, sql });
        }
    }
}

/// Deliver a statement the user runs: on the live session as psql's
/// `SendQuery` sends it (`send_user_statement`); otherwise as `deliver`
/// does for a lost one.
pub(crate) fn deliver_user_statement(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, to: SendTo, sql: String) {
    match to {
        SendTo::Session(sid) => send_user_statement(tab, ws, sid, WrappedSend::Query(sql)),
        to => deliver(tab, ws, to, sql),
    }
}

/// A reply to what psql's `SendQuery` sends around a statement (tag
/// `txn:<nonce>`). After `BEGIN` / the savepoint: the statement — or,
/// refused, the error, and the statement not run, as psql's `goto
/// sendquery_cleanup`. After `RELEASE` / `ROLLBACK TO`: on with the
/// input — after its error, if it had one. Either error fails the
/// statement for `ON_ERROR_STOP`; one that found the session dead
/// (`connection_lost`) resets it first, as psql's does.
pub(crate) fn continue_wrap(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, tag: &str, result: shared::BatchResult, connection_lost: bool) {
    let Some(nonce) = tag.strip_prefix(TXN_TAG).and_then(|n| n.parse::<u32>().ok()) else { return };
    let Some(wrap) = tab.wrap.get_untracked().filter(|w| w.nonce == nonce) else { return };
    match (wrap.stage, result.error) {
        (WrapStage::Opening { send, savepoint }, None) => {
            tab.wrap.set(savepoint.then_some(Wrap { nonce, stage: WrapStage::Running { tag: None } }));
            match tab.session_id.get_untracked() {
                Some(sid) => send_now(tab, ws, sid, send),
                None => {
                    tab.wrap.set(None);
                    forget_result_latches(tab);
                    tab.querying.set(false);
                }
            }
        }
        (WrapStage::Opening { .. }, Some(error)) => {
            tab.wrap.set(None);
            forget_result_latches(tab);
            wrap_failed(tab, ws, error, true, connection_lost);
        }
        (WrapStage::Closing, error) => {
            tab.wrap.set(None);
            match error {
                Some(error) => wrap_failed(tab, ws, error, false, connection_lost),
                None => {
                    tab.querying.set(false);
                    run_next_queued(tab, ws);
                }
            }
        }
        (WrapStage::Running { .. }, _) => {}
    }
}

/// What psql's `SendQuery` sent around a statement failed: its error,
/// under the statement — marked failed when it didn't run (`unrun`: its
/// `BEGIN` / savepoint was refused) — kept for `\errverbose`, not in the
/// result variables, as psql's `ClearOrSaveResult` keeps it. The
/// statement counts as failed: under `ON_ERROR_STOP` the rest of the
/// input is dropped. On a session found dead, psql's reset comes first,
/// and the rest of the input runs on the new session (`mark_lost`).
fn wrap_failed(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, error: shared::DbError, unrun: bool, connection_lost: bool) {
    // A statement its `BEGIN` or savepoint kept from running failed, as
    // psql's `SendQuery` does — one a `\gexec` generated, the `\gexec`
    // too. Closing a savepoint after a statement isn't the statement's.
    if unrun {
        mark_gexec_failed(tab);
    }
    let queued = tab.pending_stmts.with_untracked(Vec::len);
    let sent = tab.sent_sql.get_untracked();
    let failed = tab.shell_history.with_untracked(|h| failed_command(h, queued, sent.as_deref()));
    let entry = command_error(tab, error.message.clone(), Some(error), None, false);
    tab.shell_history.update(|h| {
        if let Some(ShellEntry::Query { errored, .. }) = failed.entry.filter(|_| unrun).and_then(|at| h.get_mut(at)) {
            *errored = true;
        }
        h.push(entry);
    });
    tab.querying.set(false);
    let resets = connection_lost && tab.kind == PaneKind::Shell
        && matches!(plane_for(tab.db_type).lost_connection(), LostConnection::ResetAtOnce { .. });
    if resets {
        mark_lost(tab);
    } else {
        if tab.on_error_stop.get_untracked() {
            drop_rest(tab);
        }
        run_next_queued(tab, ws);
    }
    scroll_shell_to_bottom(&tab.id);
}

/// The statement psql's `SendQuery` wrapped has answered, with `tag` —
/// its command tag, which says whether it ended or replaced psql's
/// savepoint.
pub(crate) fn wrapped_answered(tab: &ShellTab, tag: Option<String>) {
    tab.wrap.update(|wrap| {
        if let Some(Wrap { stage: WrapStage::Running { tag: answered }, .. }) = wrap {
            *answered = tag;
        }
    });
}

/// A statement psql set its savepoint for has answered: send what the
/// plane closes it with (`statement_closing`: psql's `RELEASE` /
/// `ROLLBACK TO`, by the transaction state now and the statement's tag).
/// True when sent — the shell goes on once it answers (`continue_wrap`).
pub(crate) fn close_wrap(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>) -> bool {
    let Some(Wrap { nonce, stage: WrapStage::Running { tag } }) = tab.wrap.get_untracked() else { return false };
    let closing = plane_for(tab.db_type).statement_closing(tab.txn_state.get_untracked(), tag.as_deref());
    match (closing, tab.session_id.get_untracked()) {
        (Some(sql), Some(sid)) => {
            tab.wrap.set(Some(Wrap { nonce, stage: WrapStage::Closing }));
            tab.querying.set(true);
            ws_send(ws, &ClientMessage::IntrospectBatch { session_id: sid, tag: format!("{TXN_TAG}{nonce}"), queries: vec![sql] });
            true
        }
        _ => {
            tab.wrap.set(None);
            false
        }
    }
}

/// Whether `tag` is the tag of what psql's `SendQuery` sends around a
/// statement (`continue_wrap` takes its replies).
pub(crate) fn is_wrap_tag(tag: &str) -> bool {
    tag.starts_with(TXN_TAG)
}

/// psql's `ECHO_HIDDEN` (`PSQLexec`): a meta-command's queries, as they
/// go to the server, each shown first between psql's lines — under
/// `noexec` shown and not run (the command then says nothing more, as
/// psql's, whose `PSQLexec` returns no result). True: send them.
pub(crate) fn echo_hidden(tab: &ShellTab, queries: &[String]) -> bool {
    let echo = tab.variables.with_untracked(|v| v.get("ECHO_HIDDEN").cloned()).unwrap_or_default();
    let noexec = echo.eq_ignore_ascii_case("noexec");
    if !noexec && crate::psql_vars::parse_bool(&echo) != Some(true) {
        return true;
    }
    let lines: Vec<String> = queries.iter()
        .flat_map(|query| {
            std::iter::once("/******** QUERY *********/".to_string())
                .chain(query.split('\n').map(String::from))
                .chain(["/************************/".to_string(), String::new()])
        })
        .collect();
    tab.shell_history.update(|h| h.push(ShellEntry::Printed { lines, links: Vec::new() }));
    !noexec
}

/// A list psql runs a second query after, for its footers
/// (`listSchemas`' publications): its result waits in `pending_list`
/// while that query goes out as the list's did — shown first under
/// `ECHO_HIDDEN` — and comes back with it (`take_held_list`).
pub(crate) fn hold_list(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, list: ListReply, held: HeldList) {
    let Some(query) = list.footers.clone() else { return };
    // Nowhere to send it — or, under `noexec`, shown and not sent: the
    // list isn't printed, as psql's isn't when `PSQLexec` returns none.
    let to = send_to(tab, ws)
        .filter(|to| matches!(to, SendTo::NotConnected(_)) || echo_hidden(tab, std::slice::from_ref(&query.sql)));
    let Some(to) = to else {
        tab.querying.set(false);
        return;
    };
    tab.pending_list.set(Some(ListReply { held: Some(held), ..list }));
    tab.querying.set(true);
    deliver(tab, ws, to, query.sql);
}

/// The reply to a list's footer query (`hold_list`): the list's own
/// result, back in place of it, and the footers that reply's rows make.
pub(crate) fn take_held_list(list: &mut Option<ListReply>, rows: &[Vec<Option<String>>]) -> Option<(HeldList, Vec<String>)> {
    let reply = list.as_mut()?;
    let held = reply.held.take()?;
    let footers = reply.footers.take().map(|query| query.footers(rows)).unwrap_or_default();
    Some((held, footers))
}

/// Run a staged meta-command typed in this shell: send its first batch
/// (`IntrospectBatch`) — or, with none (psql's `\lo_unlink` inside a
/// transaction), go straight on to its next step. Each reply goes to
/// [`continue_staged_meta`].
pub(crate) fn start_staged_meta(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, command: String, queries: Vec<String>) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let sid = match send_to(tab, ws) {
        Some(SendTo::Session(sid)) => sid,
        Some(SendTo::NotConnected(message)) => {
            tab.shell_history.update(|h| h.push(ShellEntry::CliError { lines: vec![message.into()], message: message.into() }));
            return;
        }
        Some(SendTo::Reset) => { reset_lost(tab, ws); return; }
        // The mysql CLI reconnects, then runs it (`com_server_help`).
        Some(SendTo::Reopen) => {
            tab.querying.set(true);
            reopen(tab, ws, Reopening::Meta { command, queries });
            return;
        }
        _ => {
            tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(vec!["Not connected — no session.".into()])));
            return;
        }
    };
    if !queries.is_empty() && !echo_hidden(tab, &queries) {
        return;
    }
    let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
    let tag = format!("{META_TAG}{nonce}");
    tab.pending_meta.set(Some(PendingMeta {
        nonce, command, ctx: tab.meta_context(), replies: Vec::new(), queries: queries.clone(), fast_path: false,
    }));
    tab.querying.set(true);
    if queries.is_empty() {
        continue_staged_meta(tab, ws, &tag, shared::BatchResult::default());
    } else {
        ws_send(ws, &ClientMessage::IntrospectBatch { session_id: sid, tag, queries });
    }
}

/// A staged meta-command's batch came back: the plane says what's next
/// — another batch, which goes out, or what to show, which ends it. A
/// batch the server refused is kept for `\errverbose`, as psql keeps a
/// meta-command's failed query (the result variables stay as they are).
pub(crate) fn continue_staged_meta(tab: &ShellTab, ws: RwSignal<Option<WebSocket>>, tag: &str, result: shared::BatchResult) {
    let Some(nonce) = tag.strip_prefix(META_TAG).and_then(|n| n.parse::<u32>().ok()) else { return };
    let plane = plane_for(tab.db_type);
    let mut reply = result;
    loop {
        let mut step = None;
        let mut errored = false;
        let mut failed = None;
        tab.pending_meta.update_untracked(|pending| {
            if let Some(p) = pending.as_mut().filter(|p| p.nonce == nonce) {
                if let Some(error) = &reply.error {
                    errored = true;
                    if !p.fast_path {
                        failed = Some(LastError { error: error.clone(), query: p.queries.get(reply.sets.len()).cloned() });
                    }
                }
                p.replies.push(std::mem::take(&mut reply));
                step = Some(plane.meta_continue(&p.command, &p.ctx, &p.replies));
                match &step {
                    Some(crate::plane::MetaStep::Run(queries)) => (p.queries, p.fast_path) = (queries.clone(), false),
                    Some(crate::plane::MetaStep::Call(sql)) => (p.queries, p.fast_path) = (vec![sql.clone()], true),
                    _ => {}
                }
            }
        });
        if errored {
            // A command that failed says nothing more (`extra_arguments`).
            tab.pending_warnings.set(Vec::new());
        }
        if failed.is_some() {
            tab.last_error.set(failed);
        }
        match step {
            None => return, // the shell moved on
            // Nothing to ask the server: straight on to the step after.
            Some(crate::plane::MetaStep::Run(queries)) if queries.is_empty() => continue,
            Some(crate::plane::MetaStep::Run(queries)) => {
                match tab.session_id.get_untracked() {
                    Some(sid) if echo_hidden(tab, &queries) =>
                        ws_send(ws, &ClientMessage::IntrospectBatch { session_id: sid, tag: tag.to_string(), queries }),
                    _ => finish_staged_meta(tab, Vec::new()),
                }
                return;
            }
            // libpq's fast path: never shown, never held back.
            Some(crate::plane::MetaStep::Call(sql)) => {
                match tab.session_id.get_untracked() {
                    Some(sid) => ws_send(ws, &ClientMessage::IntrospectBatch { session_id: sid, tag: tag.to_string(), queries: vec![sql] }),
                    None => finish_staged_meta(tab, Vec::new()),
                }
                return;
            }
            Some(crate::plane::MetaStep::Done(entries)) => {
                finish_staged_meta(tab, entries);
                return;
            }
            Some(crate::plane::MetaStep::Failed(entries)) => {
                tab.pending_warnings.set(Vec::new());
                finish_staged_meta(tab, entries);
                return;
            }
        }
    }
}

/// The relations a block's printed output (psql's `\d`) links to, once
/// each, for the block's right-click menu.
pub(crate) fn block_explorer_links(history: &[ShellEntry], num: u32) -> Vec<ExplorerInit> {
    let mut opens: Vec<ExplorerInit> = Vec::new();
    let block = history.iter()
        .skip_while(|e| !matches!(e, ShellEntry::Query { num: n, .. } if *n == num))
        .skip(1)
        .take_while(|e| !matches!(e, ShellEntry::Query { .. }));
    for entry in block {
        if let ShellEntry::Printed { links, .. } = entry {
            for link in links {
                if !opens.contains(&link.open) { opens.push(link.open.clone()); }
            }
        }
    }
    opens
}

fn finish_staged_meta(tab: &ShellTab, entries: Vec<ShellEntry>) {
    tab.pending_meta.set(None);
    tab.querying.set(false);
    tab.shell_history.update(|h| h.extend(entries));
    scroll_shell_to_bottom(&tab.id);
}

// ── External editor (`\e` / `\ef` / `\ev`) ─────────────────────────────────

/// Tag prefix of this shell's `EditExternally` requests; the rest is
/// the edit's nonce.
const EDIT_TAG: &str = "edit:";

fn next_edit_nonce() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Open `seed` in the external editor for this shell, at `line` when
/// given. The input shows `restore` read-only until the edit ends (see
/// [`finish_external_edit`]); noctua picks the editor.
pub(crate) fn start_external_edit(
    tab:          &ShellTab,
    ws:           RwSignal<Option<WebSocket>>,
    seed:         String,
    line:         Option<u32>,
    restore:      String,
    discard_if_unchanged: bool,
    then:         String,
) {
    let Some(sid) = tab.session_id.get_untracked() else {
        tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(vec!["No active session.".into()])));
        return;
    };
    let plane = plane_for(tab.db_type);
    let nonce = next_edit_nonce();
    tab.hist_pos.set(None);
    tab.saved_input.set(String::new());
    tab.current_input.set(restore.clone());
    schedule_textarea_resize(&tab.id);
    tab.external_edit.set(Some(ExternalEdit { nonce, restore, seed: seed.clone(), discard_if_unchanged, then }));
    ws_send(ws, &ClientMessage::EditExternally {
        session_id:    sid,
        connection_id: Some(tab.conn_id.clone()),
        content:       seed,
        line,
        tag:           format!("{EDIT_TAG}{nonce}"),
        env_chain:     plane.editor_env_chain().to_vec(),
        line_flag_var: plane.editor_line_flag_var(),
    });
}

/// What a `\ef` / `\ev` lookup (introspect tag `edit_to_input`)
/// returned: the plane's `Src` / `Line` / `Error` columns.
#[derive(Debug, PartialEq)]
pub(crate) enum EditLookup {
    /// The plane refused, psql-style ("… is not a view").
    Refused(String),
    Found { src: String, line: Option<u32> },
    NotFound,
}

/// Read a `\ef` / `\ev` lookup by column name. noctua sends SQL NULL
/// as the text "NULL", so that counts as absent, like an empty cell.
pub(crate) fn parse_edit_lookup(columns: &[String], rows: &[Vec<String>]) -> EditLookup {
    let cell = |name: &str| columns.iter()
        .position(|c| c.eq_ignore_ascii_case(name))
        .and_then(|i| rows.first().and_then(|r| r.get(i)))
        .filter(|v| !v.is_empty() && *v != "NULL")
        .cloned();
    if let Some(error) = cell("Error") {
        return EditLookup::Refused(error);
    }
    match cell("Src") {
        Some(src) => EditLookup::Found { src, line: cell("Line").and_then(|l| l.parse().ok()) },
        None => EditLookup::NotFound,
    }
}

/// How noctua says an external edit ended.
pub(crate) enum EditEnd {
    /// The editor exited cleanly with this text.
    Text(String),
    /// No text came back (discarded, or an error); say why.
    Note(String),
}

/// Apply noctua's reply to the edit `tag` names: put the text in the
/// input with the cursor at the end, ready for Enter — never run it.
/// Returns false, changing nothing, when this shell isn't waiting for
/// that edit (Esc stopped waiting, or it's another shell's).
pub(crate) fn finish_external_edit(tab: &ShellTab, tag: &str, end: EditEnd) -> bool {
    let Some(nonce) = tag.strip_prefix(EDIT_TAG).and_then(|n| n.parse::<u32>().ok()) else { return false };
    let Some(edit) = tab.external_edit.get_untracked().filter(|e| e.nonce == nonce) else { return false };
    tab.external_edit.set(None);
    let text = match end {
        EditEnd::Text(content) => edited_input(&edit, &content),
        EditEnd::Note(message) => {
            // No text came back: the command failed, and says no more
            // (`pending_warnings`).
            tab.pending_warnings.set(Vec::new());
            tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(vec![message])));
            edit.restore.clone()
        }
    };
    tab.current_input.set(edit.followed(&text));
    focus_input_at_end(&tab.id);
    scroll_shell_to_bottom(&tab.id);
    true
}

/// Esc while the editor is open: stop waiting. The input goes back to
/// what it was; whatever the editor saves later is dropped.
fn abandon_external_edit(tab: &ShellTab) {
    let Some(edit) = tab.external_edit.get_untracked() else { return };
    tab.external_edit.set(None);
    tab.pending_warnings.set(Vec::new());
    tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
        "Stopped waiting for the editor; anything saved there won't come back here.".into(),
    ])));
    tab.current_input.set(edit.followed(&edit.restore));
    focus_input_at_end(&tab.id);
    scroll_shell_to_bottom(&tab.id);
}

/// The connection behind this shell went away mid-edit: no reply can
/// arrive, so stop waiting and put the input back.
pub(crate) fn lose_external_edit(tab: &ShellTab) {
    let Some(edit) = tab.external_edit.get_untracked() else { return };
    tab.external_edit.set(None);
    tab.pending_warnings.set(Vec::new());
    tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
        "Lost the connection while the editor was open; anything saved there won't come back here.".into(),
    ])));
    tab.current_input.set(edit.followed(&edit.restore));
    schedule_textarea_resize(&tab.id);
}

/// The input after an edit returns `content`. Trailing whitespace goes
/// (editors end files with a newline); and as in psql, the previous
/// query or a `\ef` / `\ev` definition coming back unchanged is
/// discarded — the input goes back to what it was, normally empty.
fn edited_input(edit: &ExternalEdit, content: &str) -> String {
    let text = content.trim_end();
    if edit.discard_if_unchanged && text == edit.seed.trim_end() { edit.restore.clone() } else { text.to_string() }
}

/// 1-based line of `text` holding the textarea's cursor (a UTF-16
/// offset, as the DOM reports it).
fn line_at(text: &str, cursor_utf16: u32) -> u32 {
    let mut units = 0;
    let mut line = 1;
    for c in text.chars() {
        if units >= cursor_utf16 { break; }
        units += c.len_utf16() as u32;
        if c == '\n' { line += 1; }
    }
    line
}

/// Line the cursor is on in this shell's input, if it falls within the
/// first `max_line` lines and isn't the first (opening at line 1 needs
/// no argument).
fn cursor_line(pane_id: &str, text: &str, max_line: usize) -> Option<u32> {
    use wasm_bindgen::JsCast;
    let cursor = document()
        .get_element_by_id(&format!("shell-input-{pane_id}"))
        .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        .and_then(|ta| ta.selection_start().ok().flatten())?;
    let line = line_at(text, cursor);
    (line > 1 && line as usize <= max_line).then_some(line)
}

/// Resize the input to its text, focus it and park the cursor at the
/// end — after the DOM has the new value.
fn focus_input_at_end(pane_id: &str) {
    schedule_textarea_resize(pane_id);
    let id = format!("shell-input-{pane_id}");
    request_animation_frame(move || {
        use wasm_bindgen::JsCast;
        if let Some(ta) = document().get_element_by_id(&id)
            .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        {
            let end = ta.value().encode_utf16().count() as u32;
            let _ = ta.focus();
            let _ = ta.set_selection_range(end, end);
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_shell_leaf<F, S, T, E>(
    pane_id: String,
    tab: Option<ShellTab>,
    active_pane_id: RwSignal<Option<String>>,
    ws: RwSignal<Option<WebSocket>>,
    pane_ops: PaneOps,
    connections: RwSignal<Vec<shared::ConnectionDefPublic>>,
    shell_panes: RwSignal<HashMap<String, ShellTab>>,
    shell_input_menu: RwSignal<Option<ShellInputContextMenu>>,
    completion_popup: RwSignal<Option<CompletionPopup>>,
    close_pane: F,
    do_split: S,
    add_tab: T,
    add_pane: E,
) -> impl IntoView
where
    F: Fn(String) + Copy + 'static,
    S: Fn(String, SplitDir) + Copy + 'static,
    T: Fn(String) + Copy + 'static,
    E: Fn(String, PaneKind, Option<ExplorerInit>) + Copy + 'static,
{
    let Some(tab) = tab else {
        return view! { <div class="shell"><div class="shell-connecting">"(pane not found)"</div></div> }.into_view();
    };

    if tab.kind == PaneKind::SchemaExplorer {
        return render_explorer_pane(pane_id, tab, active_pane_id, ws, pane_ops, connections, shell_panes, close_pane, do_split, add_tab, add_pane).into_view();
    }
    if tab.kind == PaneKind::Activity {
        return render_activity_pane(pane_id, tab, active_pane_id, ws, close_pane, do_split, add_tab, add_pane).into_view();
    }

    let t_connecting    = tab.connecting;
    let t_history       = tab.shell_history;
    let t_collapsed     = tab.collapsed_blocks;
    let t_session_id    = tab.session_id;
    let t_session_stale = tab.session_stale;
    let t_disconnected  = tab.disconnected;
    let t_switching     = tab.switching;
    let t_current_db    = tab.current_db;
    let t_db_conn_id    = tab.db_connection_id;

    let t_query_count = tab.query_count;
    let t_hist_pos    = tab.hist_pos;
    let t_input       = tab.current_input;
    let t_pend_vert   = tab.pending_vertical;
    let t_pend_list   = tab.pending_list;
    let t_sent_sql    = tab.sent_sql;
    let t_hist_saved  = tab.saved_input;
    let t_querying    = tab.querying;
    let t_txn_state   = tab.txn_state;
    let t_watch_sql   = tab.watch_sql;
    let t_watch_int   = tab.watch_interval;
    let t_watch_tick  = tab.watch_tick;
    let t_watch_title = tab.watch_title;
    let t_watch_pend  = tab.watch_pending;
    let t_watch_gen   = tab.watch_generation;
    let t_watch_timer = tab.watch_timer;
    let t_hist_key    = tab.history_key.clone();
    let t_delimiter     = tab.delimiter;
    let t_expanded         = tab.expanded;
    let t_pset_tuples_only = tab.pset_tuples_only;
    let t_pset_footer      = tab.pset_footer;
    let t_pset_title       = tab.pset_title;
    let t_prompt1          = tab.prompt1;
    // tab.prompt2 / tab.prompt3 are reserved (psql_vars classifies
    // them as AcceptedNoOp until tyto adds a multi-line continuation
    // mode and `\copy`, respectively). Re-capture them here when
    // those render sites land.
    let t_prompt_state     = tab.prompt_state;
    let t_password_prompt  = tab.password_prompt;
    let t_show_timing   = tab.show_timing;
    let t_safe_mode     = tab.safe_mode;
    let t_null_disp     = tab.null_display;
    let t_pset_text     = tab.pset_text;
    let t_output_fmt    = tab.output_format;
    let t_pending_cfm   = tab.pending_confirm;
    let t_pending_stmts = tab.pending_stmts;
    let t_noted         = tab.noted;
    let t_variables     = tab.variables;
    let t_search_on   = tab.search_active;
    let t_search_term = tab.search_term;
    let t_search_pos  = tab.search_pos;
    let t_persisted   = tab.persisted_cmds;
    let t_conn_id     = tab.conn_id.clone();
    let t_external_edit = tab.external_edit;
    let t_resume        = tab.resume_input;
    let t_warnings      = tab.pending_warnings;
    let t_conditionals  = tab.conditionals;

    let pid_format    = pane_id.clone();
    let t_plane: &'static dyn DbmsPlane = plane_for(tab.db_type);

    // Both delegate to the free functions below, which other panes use
    // too (the explorer's action shell). `StoredValue` keeps the
    // closures Copy while handing the functions the whole tab.
    let tab_sv = store_value(tab.clone());
    let cancel_watch = move || tab_sv.with_value(stop_watch);
    // Ctrl+C — and Stop, its button — ends a `\watch` as psql's SIGINT
    // does: what was typed or pasted after it goes with it, as the
    // terminal flushes its input, where a watch that ends itself reads
    // it on.
    let interrupt_watch = move || {
        t_resume.set(None);
        cancel_watch();
    };
    let submit_query = move |stmt: TypedStatement| {
        tab_sv.with_value(|t| run_in_shell(t, ws, stmt));
    };

    // Multi-statement submitter. Pushed to a Callback so the safe-mode
    // batch-confirm dialog can re-invoke it after the DBA signs off on
    // the whole batch (audit finding 1.1 — pre-fix the multi-statement
    // path was the only safe-mode bypass, since `is_destructive` only
    // ran for `stmts.len() == 1`).
    //
    // Shape: persist every statement as typed, push each (substituted)
    // to the scrollback as a Query entry, queue the tail in
    // `pending_stmts`, latch the head's result handling and send it.
    // The QueryResult handler in main.rs drains the queue
    // (`run_next_queued`), each statement with its own.
    let hk_multi  = t_hist_key.clone();
    let pid_multi = pane_id.clone();
    let run_multi: Callback<StatementBatch> = Callback::new(move |batch: StatementBatch| {
        if batch.run.is_empty() { return; }
        // A statement the CLI answers itself (`cli_answer`) needs no
        // session: a batch of nothing else goes without one.
        let to = if batch.run.iter().any(|st| cli_answer(t_plane, st).is_none()) {
            let Some(to) = tab_sv.with_value(|t| send_to(t, ws)) else { return };
            Some(to)
        } else {
            None
        };
        cancel_watch();
        t_hist_pos.set(None);
        t_hist_saved.set(String::new());
        t_input.set(String::new());
        reset_shell_input_height(&pid_multi);

        // History keeps each statement as typed; the scrollback and the
        // server get it with `:var`s substituted. One the CLI answers
        // itself is shown — never "the previous query" — but not kept;
        // one it sends is, even empty (psql's `;`).
        for st in batch.typed.iter().filter(|st| cli_answer(t_plane, st).is_none()) {
            persist_command(&hk_multi, &st.history_text());
        }
        let substituted = batch.run;
        let first_num = t_query_count.get();
        t_history.update(|h| {
            for (num, st) in (first_num..).zip(&substituted) {
                h.push(ShellEntry::Query {
                    num, sql: st.sql.clone(), terminator: st.terminator.clone(), meta: cli_answer(t_plane, st).is_some(), errored: false,
                });
            }
        });
        t_query_count.update(|n| *n += substituted.len() as u32);

        if let Some(SendTo::NotConnected(message)) = to {
            // psql answers every statement of the input.
            let lines = vec![message.to_string(); substituted.len()];
            t_history.update(|h| h.push(ShellEntry::CliError { lines, message: message.into() }));
            scroll_shell_to_bottom(&pid_multi);
            return;
        }
        t_pending_stmts.set(substituted);
        tab_sv.with_value(|t| {
            if let (Some(first), Some(to)) = (next_statement(t), to) {
                t_querying.set(true);
                latch_result(t, &first);
                deliver_user_statement(t, ws, to, first.sql);
            }
        });
        scroll_shell_to_bottom(&pid_multi);
    });

    let query_strings = move || -> Vec<String> {
        let persisted = t_persisted.get().unwrap_or_default();
        // An empty statement the CLI answered itself (mysql's lone `;`)
        // isn't worth recalling — nor kept; one it sent (psql's) is both.
        let session: Vec<String> = t_history.get().into_iter()
            .filter_map(|e| match e {
                ShellEntry::Query { sql, terminator, meta, .. } if !(sql.is_empty() && meta) => Some(format!("{sql}{terminator}")),
                _ => None,
            })
            .collect();
        let mut combined = persisted;
        combined.extend(session);
        combined
    };

    let pid_watch = pane_id.clone();
    setup_interval_effect(t_watch_int, 1, t_watch_tick, t_watch_timer, true);

    create_effect(move |_| {
        let _ = t_watch_tick.get();
        let Some(sql) = t_watch_sql.get_untracked() else { return };
        let Some(sid) = t_session_id.get() else { return };
        if t_watch_pend.get_untracked().is_some() { return; }
        t_watch_pend.set(Some(t_watch_gen.get_untracked()));
        // Titled when it starts, as psql's run is.
        let interval_ms = t_watch_int.get_untracked().unwrap_or_default();
        t_watch_title.set(watch_title(t_pset_title.get_untracked().as_deref(), &local_asctime(), interval_ms));
        send_statement(ws, t_sent_sql, sid, sql);
        scroll_shell_to_bottom(&pid_watch);
    });

    // When `querying` flips true → false the prompt+input block re-renders,
    // which drops browser focus from the (previously display:none) textarea.
    // Refocus on the next frame, but only if this pane is still the active
    // one — don't steal focus from a pane the user switched to.
    let pid_focus = pane_id.clone();
    create_effect(move |prev: Option<bool>| {
        let now = t_querying.get();
        if prev == Some(true) && !now
            && active_pane_id.get_untracked().as_deref() == Some(&pid_focus)
        {
            use wasm_bindgen::JsCast;
            let id = format!("shell-input-{}", pid_focus);
            if let Some(win) = web_sys::window() {
                let cb = wasm_bindgen::closure::Closure::once(move || {
                    let _ = web_sys::window()
                        .and_then(|w| w.document())
                        .and_then(|d| d.get_element_by_id(&id))
                        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
                        .map(|el| { let _ = el.focus(); });
                });
                let _ = win.request_animation_frame(cb.as_ref().unchecked_ref());
                cb.forget();
            }
        }
        now
    });

    // Helper: find the Nth match in history for Ctrl+R search
    let search_match = move |term: &str, pos: usize| -> Option<String> {
        if term.is_empty() {
            let qs = query_strings();
            return qs.into_iter().rev().nth(pos);
        }
        let lower = term.to_lowercase();
        query_strings().into_iter().rev()
            .filter(|s| s.to_lowercase().contains(&lower))
            .nth(pos)
    };

    // ── The input, read as the CLI reads it ──────────────────────────────
    // What was typed — or what's left of it — read in order, as psql and
    // the mysql CLI read their input: the statements ended before the
    // first command run first; then the command, on the query buffer
    // typed before it, which it works on or leaves; then what follows,
    // the same way. Whenever the shell must wait — for those statements'
    // results, a command's reply, a prompt, the editor, a watch — what's
    // left waits in `resume_input` and reads on once the shell is free
    // (the effect below). Returns whether the input was taken: Enter
    // mustn't add a line.
    let process_input: Callback<String, bool> = {
        let pid3 = pane_id.clone();
        let t_hist_key = t_hist_key.clone();
        Callback::new(move |typed: String| -> bool {
            // What the last command still has to say comes before this
            // (the effect that says it once the shell is free may not
            // have run yet).
            if !tab_sv.with_value(shell_busy) {
                let held = t_warnings.try_update(std::mem::take).unwrap_or_default();
                if !held.is_empty() {
                    t_history.update(|h| h.extend(held));
                }
            }
            // The command was taken: the input starts over.
            let clear_typed = || {
                t_hist_pos.set(None);
                t_hist_saved.set(String::new());
                t_input.set(String::new());
                reset_shell_input_height(&pid3);
            };
            let meta_respond = |msg: Vec<String>| {
                clear_typed();
                t_history.update(|h| h.push(ShellEntry::MetaOutput(msg)));
                scroll_shell_to_bottom(&pid3);
            };
            // Run statements, each in turn — with safe mode on, behind
            // one confirm when any is destructive (AST-based: audit
            // findings 1.1, 1.2, 1.3). A batch is inspected with its
            // `:var`s substituted, as it will run.
            let run_statements = |stmts: Vec<TypedStatement>| {
                let dialect = t_plane.sql_dialect();
                let safe_mode = t_safe_mode.get_untracked();
                match &stmts[..] {
                    [stmt] if !stmt.sql.is_empty() => {
                        let reason = safe_mode.then(|| crate::sql_inspect::inspect_statement(&stmt.sql, dialect).destructive).flatten();
                        match reason {
                            Some(reason) => t_pending_cfm.set(Some(PendingConfirm::Single { stmt: stmt.clone(), watch: None, reason })),
                            None => submit_query(stmt.clone()),
                        }
                        return;
                    }
                    // Several, or an empty one — the CLI's to answer, or
                    // psql's to send (`run_multi`).
                    _ => {}
                }
                let needs_session = stmts.iter().any(|st| cli_answer(t_plane, st).is_none());
                if needs_session && tab_sv.with_value(|t| send_to(t, ws)).is_none() { return; }
                let batch = tab_sv.with_value(|t| StatementBatch::substitute(stmts, |sql| t.interpolate(sql)));
                let destructive_indexes: Vec<usize> = if safe_mode {
                    batch.run.iter().enumerate()
                        .filter(|(_, st)| !st.sql.is_empty())
                        .filter(|(_, st)| crate::sql_inspect::inspect_statement(&st.sql, dialect).destructive.is_some())
                        .map(|(i, _)| i)
                        .collect()
                } else { Vec::new() };
                if destructive_indexes.is_empty() {
                    run_multi.call(batch);
                } else {
                    t_pending_cfm.set(Some(PendingConfirm::Batch { batch, destructive_indexes }));
                }
            };
            // The statements of `text`, run when it ends one — the
            // delimiter, or on MySQL `\g` / `\G` (vertical): the CLI's
            // statements, each with its own (`parser::split_statements`)
            // — or when one before its last ended (`SELECT 1; SELECT 2`
            // runs both). Whether they ran: otherwise `text` waits for
            // more.
            let run_text = |text: &str| -> bool {
                let lexicon = t_plane.lexicon();
                let statements = crate::parser::split_statements(text, &t_delimiter.get_untracked(), lexicon);
                if !statements.run_on_enter() {
                    return false;
                }
                let crate::parser::Statements { list: mut stmts, ended, .. } = statements;
                // What the CLI says reading each statement's lines, said
                // when it's sent — but the lines it said before, left in
                // the input (`noted`), which the first starts with.
                let noted = t_noted.try_update(std::mem::take).unwrap_or_default();
                let noted = noted.trim_start();
                let delimiter = t_delimiter.get_untracked();
                for (i, st) in stmts.iter_mut().enumerate() {
                    let skip = if i == 0 && !noted.is_empty() && st.sql.starts_with(noted) { noted.len() } else { 0 };
                    st.notes = notes_for(t_plane, &st.sql, skip, &delimiter);
                }
                // A statement of comments only goes to the server once it's
                // ended — the mysql CLI's "Query OK", psql's nothing — as
                // does one with nothing in it, but where the CLI answers
                // that itself (mysql's "No query specified", `cli_answer`).
                let last = stmts.len() - 1;
                let stmts: Vec<TypedStatement> = stmts.into_iter().enumerate()
                    .filter(|(i, st)| *i < last || ended || !crate::parser::is_comment_only(&st.sql, lexicon))
                    .map(|(_, st)| st)
                    .collect();
                if stmts.is_empty() {
                    t_hist_pos.set(None);
                    t_input.set(String::new());
                    reset_shell_input_height(&pid3);
                } else {
                    run_statements(stmts);
                }
                true
            };
            // Statements typed in an inactive branch of psql's \if: shown
            // and kept as typed, each with psql's "query ignored", not run.
            let ignore_statements = |text: &str| {
                let statements = crate::parser::split_statements(text, &t_delimiter.get_untracked(), t_plane.lexicon());
                for st in statements.list {
                    let num = t_query_count.get_untracked();
                    t_query_count.update(|n| *n += 1);
                    persist_command(&t_hist_key, &st.as_typed());
                    t_history.update(|h| {
                        h.push(ShellEntry::Query { num, sql: st.sql, terminator: st.terminator, meta: true, errored: false });
                        h.push(cli_said(vec![crate::psql_if::IGNORED_QUERY.into()]));
                    });
                }
                scroll_shell_to_bottom(&pid3);
            };
            // What's left that doesn't run yet goes back in the input —
            // what the CLI says about its lines said (`say_left_notes`).
            let note_left = |text: &str| tab_sv.with_value(|t| say_left_notes(t, text, &t_delimiter.get_untracked()));
            let leave = |text: &str| {
                note_left(text);
                t_hist_pos.set(None);
                t_hist_saved.set(String::new());
                t_input.set(text.to_string());
                schedule_textarea_resize(&pid3);
            };

            let mut text = typed;
            // Whether any of it was taken: a command run, statements sent.
            let mut taken = false;
            loop {
                // Leading whitespace stays: the mysql CLI tells an
                // indented line comment from one starting a line.
                let trimmed = text.trim_end();
                if trimmed.trim_start().is_empty() {
                    // Nothing (left) to run: the input starts over.
                    t_input.set(String::new());
                    reset_shell_input_height(&pid3);
                    scroll_shell_to_bottom(&pid3);
                    return true;
                }
                let delimiter = t_delimiter.get_untracked();
                let lexicon = t_plane.lexicon();
                let found = first_command(trimmed, t_plane, &delimiter);
                let inactive = !t_conditionals.with_untracked(|stack| crate::psql_if::active(stack));

                // In an inactive branch of psql's \if, nothing runs: each
                // statement is "query ignored" — what's left unended waits
                // for its terminator or the next branching command.
                if inactive {
                    let end = found.map_or(trimmed.len(), |found| found.at);
                    let pending = crate::parser::split_statements(&trimmed[..end], &delimiter, lexicon).pending;
                    if pending > 0 {
                        ignore_statements(&trimmed[..pending]);
                        taken = true;
                        text = trimmed[pending..].to_string();
                        continue;
                    }
                    if found.is_none() {
                        if taken {
                            leave(&text);
                        } else {
                            note_left(&text);
                        }
                        return taken;
                    }
                }

                // No command: statements, run or waiting for more — in the
                // input, as typed, when nothing was taken.
                let Some(found) = found else {
                    let ran = run_text(trimmed);
                    match (ran, taken) {
                        (false, true) => leave(&text),
                        (false, false) => note_left(&text),
                        _ => {}
                    }
                    return ran || taken;
                };
                let at = found.at;
                // The statements ended before the command run first, as
                // the CLI sends each at its terminator; the rest reads on
                // once they're done.
                let pending = crate::parser::split_statements(&trimmed[..at], &delimiter, lexicon).pending;
                if pending > 0 {
                    let rest = trimmed[pending..].to_string();
                    run_text(&trimmed[..pending]);
                    taken = true;
                    if tab_sv.with_value(shell_busy) {
                        t_resume.set(Some(rest));
                        return true;
                    }
                    text = rest;
                    continue;
                }
                // The command — on its line, with the query buffer typed
                // before it; a named one alone on its line, or a statement
                // up to its delimiter — and what reads on after it.
                let segment = &trimmed[..found.end];
                let following = &trimmed[found.resume..];
                let named = &trimmed[at..found.end];
                let action = match found.kind {
                    CommandKind::Backslash => t_plane.meta_cmd_in(segment, &tab_sv.with_value(ShellTab::meta_context)),
                    CommandKind::Line => t_plane.named_command(named, &delimiter),
                    CommandKind::Statement => t_plane.statement_command(named),
                };
                let Some(action) = action else {
                    // The plane takes it for text after all: statements —
                    // in an inactive \if branch, ignored as psql ignores them.
                    if inactive {
                        let pending = crate::parser::split_statements(trimmed, &delimiter, lexicon).pending;
                        ignore_statements(&trimmed[..pending]);
                        if pending == 0 && !taken {
                            return false;
                        }
                        leave(&trimmed[pending..]);
                        return true;
                    }
                    let ran = run_text(trimmed);
                    match (ran, taken) {
                        (false, true) => leave(&text),
                        (false, false) => note_left(&text),
                        _ => {}
                    }
                    return ran || taken;
                };
                taken = true;
                // A command goes into the scrollback and the history as
                // typed, as psql records it (`records_itself` below).
                // A statement run as one keeps its delimiter apart, as the
                // CLI's buffer holds the statement without it
                // (`DbmsPlane::previous_buffer_after`).
                let record_command = |(typed, terminator): (&str, &str)| {
                    let num = t_query_count.get_untracked();
                    t_query_count.update(|n| *n += 1);
                    t_history.update(|h| h.push(ShellEntry::Query {
                        num, sql: typed.to_string(), terminator: terminator.to_string(), meta: true, errored: false,
                    }));
                    persist_command(&t_hist_key, &format!("{typed}{terminator}"));
                };
                let (typed_buffer, command) = match found.kind {
                    CommandKind::Backslash => split_buffer(segment, lexicon),
                    // Nothing was pending before a named command.
                    CommandKind::Line | CommandKind::Statement => ("", named),
                };
                // The buffer as typed, for `\p`: the text before the
                // command — run as a statement, that statement (mysql's
                // `print;`).
                let typed_before = match found.kind {
                    CommandKind::Backslash => &segment[..segment.trim_end().len() - command.trim_start().len()],
                    CommandKind::Line => "",
                    CommandKind::Statement => named,
                };
                // `--` comments alone aren't a buffer — psql's lexer drops
                // them ahead of anything else — but a `/* */` one is, which
                // the CLI sends (psql's `/* c */ \g`).
                let buffer = if crate::parser::past_leading_line_comments(typed_buffer, lexicon).is_empty() { "" } else { typed_buffer };
                // The command takes what its CLI gives it (psql: its
                // line; mysql: `\s` its letter, `\u db;` its arguments to
                // the delimiter); what was typed after it reads on.
                // `command_as_typed`: with the space or line break before
                // it, as a `\g` command's block shows it.
                let gap = &command[..command.len() - command.trim_start().len()];
                let command = command.trim_start();
                let (command, after) = match found.kind {
                    CommandKind::Backslash => command.split_at(t_plane.command_extent(command, &delimiter)),
                    // A named command's text is all its own.
                    CommandKind::Line | CommandKind::Statement => (command, ""),
                };
                // A statement run as a command shows with its terminator,
                // as typed (`status;`).
                let typed_command = match found.kind {
                    CommandKind::Statement => trimmed[at..found.resume].trim_end(),
                    _ => command.trim_end(),
                };
                let recorded = match found.kind {
                    CommandKind::Statement => typed_command.split_at((found.end - at).min(typed_command.len())),
                    _ => (typed_command, ""),
                };
                let command_as_typed = format!("{gap}{typed_command}");
                // What reads on once the command is done: the buffer,
                // unless the command works on it, then what was typed
                // after the command — on its line (mysql's `select 1 \s
                // from dual;`) and below it — with the space or line break
                // typed before the command between them, as the CLI keeps
                // it — a `;` too: `\s;` is `\s` and then the mysql CLI's
                // "No query specified", `select 1 \s;` runs `select 1`.
                // After the editor it doesn't read on: it follows the text
                // that comes back (`ExternalEdit.then`).
                let leaves_buffer = !typed_buffer.is_empty() && !action.works_on_buffer();
                let left = if leaves_buffer { crate::parser::past_leading_line_comments(typed_buffer, lexicon) } else { "" };
                // What the CLI said reading the buffer's lines comes before
                // the command (`select 1 \i` then `\s`: the note, then the
                // status) — but the lines said before. A buffer the command
                // leaves reads on, its lines said (`noted`).
                if !typed_buffer.trim().is_empty() {
                    let noted = t_noted.try_update(std::mem::take).unwrap_or_default();
                    let skip = if typed_buffer.starts_with(noted.as_str()) { noted.len() } else { 0 };
                    let notes = notes_for(t_plane, typed_buffer.trim_end(), skip, &delimiter);
                    if !notes.is_empty() {
                        t_history.update(|h| h.extend(notes));
                    }
                    t_noted.set(left.trim_end().to_string());
                }
                let after = format!("{after}{following}");
                let reads_on = |left: &str| match (left.is_empty(), after.trim().is_empty()) {
                    (true, true) => String::new(),
                    (false, true) => format!("{left}\n"),
                    (true, false) => after.trim_start().to_string(),
                    (false, false) => format!("{left}{gap}{after}"),
                };

                // psql's \if / \elif / \else / \endif move the
                // conditional stack — in any branch — and the buffer reads
                // on as psql keeps it: an active branch's text stays, an
                // inactive one's goes back to what was saved.
                if let MetaAction::Branch(branch) = action {
                    record_command(recorded);
                    let applied = t_conditionals.try_update(|stack| crate::psql_if::apply(stack, branch, left))
                        .unwrap_or_else(|| Ok((left.to_string(), None)));
                    let buffer = match applied {
                        Ok((buffer, said)) => {
                            if let Some(said) = said {
                                t_history.update(|h| h.push(cli_said(vec![said])));
                            }
                            // Its extra arguments are said in the branch
                            // it leads to, when that's active (`\endif x`).
                            if t_conditionals.with_untracked(|stack| crate::psql_if::active(stack)) {
                                let warnings = t_plane.extra_arguments(typed_command);
                                t_history.update(|h| h.extend(warnings.into_iter().map(|w| cli_said(vec![w]))));
                            }
                            buffer
                        }
                        Err(refused) => {
                            t_history.update(|h| h.push(cli_said(vec![refused])));
                            left.to_string()
                        }
                    };
                    scroll_shell_to_bottom(&pid3);
                    text = reads_on(&buffer);
                    continue;
                }
                // In an inactive branch any other backslash command is
                // skipped, its arguments with it, as psql skips it — its
                // assistance words (`quit`) are read before its branches.
                if inactive && found.kind == CommandKind::Backslash {
                    record_command(recorded);
                    let name = typed_command.trim_start_matches('\\').split(char::is_whitespace).next().unwrap_or_default();
                    t_history.update(|h| h.push(cli_said(vec![crate::psql_if::ignored_command(name)])));
                    scroll_shell_to_bottom(&pid3);
                    text = reads_on(left);
                    continue;
                }
                let kept = match action {
                    MetaAction::Disconnect | MetaAction::EditExternal { .. } => String::new(),
                    _ => reads_on(left),
                };

                // Every meta-command goes into the scrollback and the
                // history as typed, as psql records it — the command,
                // never the SQL it expands to. Except: the \g family
                // records its buffer with the \g line itself, as does
                // \watch with one, \e and a buffer-clearing \r / \c are
                // edits, and \password is never kept.
                let records_itself = matches!(action,
                    MetaAction::ClearInput(_) | MetaAction::EditExternal { .. }
                    | MetaAction::Go { .. } | MetaAction::Gexec | MetaAction::Gdesc
                    | MetaAction::Gset { .. } | MetaAction::Crosstabview { .. }
                    | MetaAction::Password { .. } | MetaAction::Fail(_))
                    || matches!(action, MetaAction::Watch { .. } if !buffer.is_empty());
                if !records_itself {
                    record_command(recorded);
                }

                // What the CLI says about the arguments the command didn't
                // read (psql's `\x: extra argument "a" ignored`) — not
                // after a command that fails. The `\g` family's come
                // before the query it sends; a command waiting for its
                // reply or its user has them said once it's done, unless
                // it failed (`ShellTab.pending_warnings`).
                let warnings: Vec<ShellEntry> = match (&action, found.kind) {
                    (MetaAction::Fail(_), _) | (_, CommandKind::Line | CommandKind::Statement) => Vec::new(),
                    _ => t_plane.extra_arguments(typed_command).into_iter().map(|w| cli_said(vec![w])).collect(),
                };
                let sends_buffer = runs_buffer(&action).is_some() || matches!(action, MetaAction::Gdesc);

                // Each command's own handling; `return` ends it.
                let run = || {
                    if let MetaAction::Fail(lines) = action {
                        // A command that fails, as psql's does: recorded as
                        // typed, its error under it.
                        record_command(recorded);
                        let message = lines.first().cloned().unwrap_or_default();
                        t_history.update(|h| h.push(ShellEntry::CliError { lines, message }));
                        clear_typed();
                        scroll_shell_to_bottom(&pid3);
                        return;
                    }

                    if let MetaAction::ClearInput(said) = action {
                        clear_typed();
                        if let Some(said) = said {
                            t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![said.into()])));
                            scroll_shell_to_bottom(&pid3);
                        }
                        return;
                    }

                    if let MetaAction::EditExternal { seed, line } = action {
                        // psql: the buffer is everything before the `\e`;
                        // an empty buffer edits the previous query as the
                        // CLI keeps it (psql's with its `;`). A bare `\e`
                        // opens where the cursor is. A seed (the `\ef` /
                        // `\ev` templates) replaces both.
                        let raw = t_input.get_untracked();
                        let buffer = buffer.to_string();
                        let (seed, line, discard_if_unchanged) = match seed {
                            Some(template) => (template, line, true),
                            None if buffer.is_empty() => (
                                t_history.with_untracked(|h| previous_buffer(t_plane, h)).unwrap_or_default(),
                                line,
                                !t_plane.keeps_unchanged_recall(),
                            ),
                            None => {
                                let line = line.or_else(|| cursor_line(&pid3, &raw, buffer.lines().count()));
                                (buffer.clone(), line, false)
                            }
                        };
                        tab_sv.with_value(|t| start_external_edit(t, ws, seed, line, buffer, discard_if_unchanged, after.clone()));
                        return;
                    }

                    if let Some((vertical, post)) = runs_buffer(&action) {
                        // The statements ended before it have run: the
                        // buffer, or the previous query when it's empty —
                        // with neither, the empty query psql sends.
                        let stmt = t_history.with_untracked(|h| buffer_statement(t_plane, buffer, &command_as_typed, h));
                        run_statements(vec![stmt.shown(vertical, post)]);
                        return;
                    }

                    if let MetaAction::Gdesc = action {
                        // `\gdesc` — describe the buffer's output
                        // columns without executing it: the buffer
                        // before the `\gdesc`, or the previous query if
                        // that's empty (matches `\gset` / `\gexec`'s
                        // shape) — with neither, the empty query, as
                        // psql's — sent as DescribeQuery. The response
                        // lands in main.rs's DescribeResult /
                        // DescribeError handlers.
                        let stmt = t_history.with_untracked(|h| buffer_statement(t_plane, buffer, &command_as_typed, h));
                        let Some(sid) = t_session_id.get_untracked() else {
                            meta_respond(vec!["\\gdesc: not connected — no session.".into()]);
                            return;
                        };
                        if t_session_stale.get_untracked() {
                            meta_respond(vec!["\\gdesc: connection is reconnecting — try again in a moment.".into()]);
                            return;
                        }
                        // No watch runs alongside it (what's typed during
                        // one waits for it to end); should one, it stops,
                        // its timer never firing beside the DescribeQuery.
                        cancel_watch();
                        let s = tab_sv.with_value(|t| t.interpolate(&stmt.sql));
                        t_hist_pos.set(None);
                        t_hist_saved.set(String::new());
                        t_input.set(String::new());
                        reset_shell_input_height(&pid3);
                        // Shown and kept like the other \g commands:
                        // the buffer with its `\gdesc` line.
                        persist_command(&t_hist_key, &stmt.history_text());
                        let num = t_query_count.get_untracked();
                        t_query_count.update(|n| *n += 1);
                        t_history.update(|h| h.push(ShellEntry::Query {
                            num, sql: s.clone(), terminator: stmt.terminator, meta: false, errored: false,
                        }));
                        t_querying.set(true);
                        tab_sv.with_value(|t| send_user_statement(t, ws, sid, WrappedSend::Describe(s)));
                        scroll_shell_to_bottom(&pid3);
                        return;
                    }

                    if let MetaAction::ShowHistory = action {
                        // psql's `\s`: each entry as typed — this one, just
                        // recorded, last — and a blank line after.
                        let mut lines: Vec<String> = query_strings().iter()
                            .flat_map(|entry| entry.lines().map(String::from))
                            .collect();
                        lines.push(String::new());
                        meta_respond(lines);
                        return;
                    }

                    if let MetaAction::ErrVerbose = action {
                        // psql's \errverbose: the last error again, every
                        // field shown (VERBOSITY verbose, SHOW_CONTEXT
                        // always).
                        match tab_sv.with_value(|t| t.last_error.get_untracked()) {
                            Some(last) => {
                                use crate::plane::{ErrorStyle, ShowContext, Verbosity};
                                let style = ErrorStyle { verbosity: Verbosity::Verbose, show_context: ShowContext::Always };
                                let lines = t_plane.format_error(&last.error, last.query.as_deref(), style);
                                t_hist_pos.set(None);
                                t_hist_saved.set(String::new());
                                t_input.set(String::new());
                                reset_shell_input_height(&pid3);
                                t_history.update(|h| h.push(ShellEntry::CliError { lines, message: last.error.message }));
                                scroll_shell_to_bottom(&pid3);
                            }
                            None => meta_respond(vec!["There is no previous error.".into()]),
                        }
                        return;
                    }

                    if let MetaAction::Watch { interval_ms, count, min_rows } = action {
                        // psql's `\watch` repeats the buffer before it, or
                        // the previous query when that's empty. A buffer
                        // runs for the first time here: safe mode looks at
                        // it, as at a `\g`'s.
                        let bounds = WatchBounds { interval_ms, count, min_rows };
                        // An empty one it refuses (`do_watch`).
                        let stmt = t_history.with_untracked(|h| buffer_statement(t_plane, buffer, &command_as_typed, h));
                        if stmt.sql.is_empty() {
                            clear_typed();
                            let said = "\\watch cannot be used with an empty query";
                            t_history.update(|h| h.push(ShellEntry::CliError { lines: vec![said.into()], message: said.into() }));
                            scroll_shell_to_bottom(&pid3);
                            return;
                        }
                        let dialect = t_plane.sql_dialect();
                        let reason = (!stmt.recalled && t_safe_mode.get_untracked())
                            .then(|| crate::sql_inspect::inspect_statement(&stmt.sql, dialect).destructive)
                            .flatten();
                        match reason {
                            Some(reason) => t_pending_cfm.set(Some(PendingConfirm::Single { stmt, watch: Some(bounds), reason })),
                            None => tab_sv.with_value(|t| start_watch(t, stmt, bounds)),
                        }
                        return;
                    }

                    if let MetaAction::Connect { database, host } = action {
                        // With noctua out of reach nothing can connect: the
                        // input stays, as a statement's does then.
                        if !ws_open(ws) {
                            return;
                        }
                        cancel_watch();
                        clear_typed();
                        // Another host needs a connection of its own — its
                        // credentials — which this one can't lend it.
                        let connected = t_variables.with_untracked(|v| v.get("HOST").cloned().unwrap_or_default());
                        if host.is_some_and(|host| host != connected) {
                            let said = "Connecting to another host isn't supported here yet.";
                            t_history.update(|h| h.push(ShellEntry::CliError { lines: vec![said.into()], message: said.into() }));
                            scroll_shell_to_bottom(&pid3);
                            return;
                        }
                        tab_sv.with_value(|t| connect_anew(t, ws, database));
                        return;
                    }

                    if let MetaAction::ReconnectDb(db) = action {
                        cancel_watch();
                        clear_typed();
                        // psql opens the new session first and keeps the
                        // one it has until that works (SessionOpened /
                        // SessionError settle `switching`). Without one —
                        // a reset failed — a bare `\c` has nothing to reuse.
                        let previous = t_session_id.get_untracked();
                        let Some(database) = db.or_else(|| previous.as_ref().map(|_| t_current_db.get_untracked())) else {
                            t_history.update(|h| h.push(ShellEntry::CliError {
                                lines:   vec!["No database connection exists to re-use parameters from".into()],
                                message: "No database connection exists to re-use parameters from".into(),
                            }));
                            scroll_shell_to_bottom(&pid3);
                            return;
                        };
                        t_switching.set(Some(Switching { previous }));
                        t_querying.set(true);
                        // \c in the shell — hardcode the SQL Shell role
                        // rather than plumbing PaneKind here (this file is
                        // shell-only, kind is always Shell).
                        ws_send(ws, &ClientMessage::Connect {
                            connection_id: t_conn_id.clone(),
                            client_tab_id: pid3.clone(),
                            database: Some(database),
                            client_role: Some("SQL Shell".to_string()),
                            timezone: crate::settings::session_time_zone(),
                        });
                        return;
                    }

                    // What psql says of a setting it changed or was asked
                    // about (`printPsetInfo`, `\timing`'s) — nothing under
                    // `QUIET`.
                    let say_setting = |said: Vec<String>| {
                        if said.is_empty() || t_variables.with_untracked(crate::psql_vars::quiet) {
                            clear_typed();
                            scroll_shell_to_bottom(&pid3);
                        } else {
                            meta_respond(said);
                        }
                    };

                    if let MetaAction::SetExpanded(value) = action {
                        let expanded = value.unwrap_or_else(|| t_expanded.get_untracked().toggled());
                        t_expanded.set(expanded);
                        say_setting(vec![expanded.said().into()]);
                        return;
                    }

                    if let MetaAction::SetTiming(on) = action {
                        let new_val = on.unwrap_or(!t_show_timing.get());
                        t_show_timing.set(new_val);
                        say_setting(vec![format!("Timing is {}.", if new_val { "on" } else { "off" })]);
                        return;
                    }

                    if let MetaAction::ShowWarnings { on, said } = action {
                        tab_sv.with_value(|t| t.show_warnings.set(on));
                        meta_respond(vec![said]);
                        return;
                    }

                    if let MetaAction::RebuildCompletion = action {
                        tab_sv.with_value(|t| {
                            t.completion_tables.set(crate::types::LoadState::NotLoaded);
                            t.completion_columns.update(|m| m.clear());
                        });
                        clear_typed();
                        scroll_shell_to_bottom(&pid3);
                        return;
                    }

                    if let MetaAction::SafeMode(on) = action {
                        t_safe_mode.set(on);
                        meta_respond(vec![format!("Safe mode {}.", if on { "ON — destructive commands will require confirmation" } else { "off" })]);
                        return;
                    }

                    if let MetaAction::SetDelimiter(d) = action {
                        // Silently, as the mysql CLI sets it.
                        t_delimiter.set(d);
                        clear_typed();
                        scroll_shell_to_bottom(&pid3);
                        return;
                    }

                    if let MetaAction::SetTextOptions { options, said } = action {
                        t_pset_text.set(options);
                        say_setting(said);
                        return;
                    }

                    if let MetaAction::SetNullDisplay(value) = action {
                        // psql's words for it (`printPsetInfo`).
                        if let Some(value) = value {
                            t_null_disp.set(value);
                        }
                        say_setting(vec![format!("Null display is \"{}\".", t_null_disp.get_untracked())]);
                        return;
                    }

                    if let MetaAction::SetFormat { format, expanded } = action {
                        // psql's words for each (`printPsetInfo`); the
                        // format leaves `\x` alone unless asked.
                        t_output_fmt.set(format);
                        let mut said = vec![format!("Output format is {}.", format.name())];
                        if let Some(expanded) = expanded {
                            t_expanded.set(expanded);
                            said.push(expanded.said().into());
                        }
                        say_setting(said);
                        return;
                    }

                    // psql's `do_pset`: a value sets these silently — it
                    // returns before `printPsetInfo` — none toggles them
                    // and says so.
                    if let MetaAction::SetTuplesOnly(on) = action {
                        match on {
                            Some(on) => {
                                t_pset_tuples_only.set(on);
                                clear_typed();
                            }
                            None => {
                                let on = !t_pset_tuples_only.get_untracked();
                                t_pset_tuples_only.set(on);
                                say_setting(vec![format!("Tuples only is {}.", if on { "on" } else { "off" })]);
                            }
                        }
                        return;
                    }

                    if let MetaAction::SetFooter(on) = action {
                        match on {
                            Some(on) => {
                                t_pset_footer.set(on);
                                clear_typed();
                            }
                            None => {
                                let on = !t_pset_footer.get_untracked();
                                t_pset_footer.set(on);
                                say_setting(vec![format!("Default footer is {}.", if on { "on" } else { "off" })]);
                            }
                        }
                        return;
                    }

                    if let MetaAction::SetTitle(title) = action {
                        say_setting(vec![match &title {
                            Some(title) => format!("Title is \"{title}\"."),
                            None => "Title is unset.".into(),
                        }]);
                        t_pset_title.set(title);
                        return;
                    }

                    if let MetaAction::Password { user } = action {
                        let target_label = user.clone()
                            .or_else(|| {
                                let u = t_variables.with_untracked(|m|
                                    m.get("USER").cloned().unwrap_or_default());
                                if u.is_empty() { None } else { Some(u) }
                            })
                            .unwrap_or_else(|| "(unknown — set :USER)".into());
                        let hint = format!(
                            "[\\password {}] — enter new password (Esc cancels). Input will be masked.",
                            target_label,
                        );
                        t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![hint])));
                        t_input.set(String::new());
                        reset_shell_input_height(&pid3);
                        t_password_prompt.set(Some(PasswordPromptState {
                            user, first: None,
                        }));
                        scroll_shell_to_bottom(&pid3);
                        return;
                    }

                    if let MetaAction::Prompt { text, name } = action {
                        use crate::psql_vars::{classify, PsqlVarKind};
                        // Refuse writes to read-only psql vars
                        // (mirrors the \set rule) — \prompt USER
                        // shouldn't overwrite tyto's auto-populated
                        // session-identity snapshot.
                        if classify(&name) == Some(PsqlVarKind::ReadOnly) {
                            meta_respond(vec![format!(
                                "{}: read-only (can't be set with \\prompt).",
                                name,
                            )]);
                            return;
                        }
                        let hint = if text.is_empty() {
                            format!("[\\prompt {}] — type a value and press Enter (Esc to cancel).", name)
                        } else {
                            format!("{}  [\\prompt {} — Enter to set, Esc to cancel]", text, name)
                        };
                        // Push the hint to history so the user sees
                        // what they're answering, then arm the prompt
                        // state. on_keydown's prompt-mode short-
                        // circuit (top of the handler) consumes the
                        // next Enter into a SetVariable.
                        t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![hint])));
                        t_input.set(String::new());
                        reset_shell_input_height(&pid3);
                        t_prompt_state.set(Some(PromptState { text, name }));
                        scroll_shell_to_bottom(&pid3);
                        return;
                    }

                    if let MetaAction::SetVariable(name, value) = action {
                        use crate::psql_vars::{classify, PsqlVarKind};
                        // psql's \set says nothing when it works.
                        let set = |name: &str, value: &str| {
                            t_variables.update(|m| { m.insert(name.to_string(), value.to_string()); });
                            clear_typed();
                        };
                        if !crate::parser::valid_variable_name(&name) {
                            meta_respond(vec![format!("invalid variable name: \"{name}\"")]);
                            return;
                        }
                        match classify(&name) {
                            // Read-only — refuse the set. The shadow
                            // var is populated automatically by the
                            // SessionOpened / QueryError / QueryResult
                            // handlers in main.rs.
                            Some(PsqlVarKind::ReadOnly) => {
                                meta_respond(vec![format!(
                                    "{}: read-only (auto-populated by tyto; can't be set with \\set).",
                                    name,
                                )]);
                            }
                            // ON_ERROR_STOP, AUTOCOMMIT, ON_ERROR_ROLLBACK,
                            // VERBOSITY, SHOW_CONTEXT, PROMPT1: as psql's
                            // hooks set them, their effect with them.
                            Some(PsqlVarKind::Honored) if name == "PROMPT1" || crate::psql_vars::assign(&name, None).is_some() => {
                                match tab_sv.with_value(|t| set_variable(t, &name, Some(value.clone()))) {
                                    Ok(()) => clear_typed(),
                                    Err(refused) => meta_respond(refused),
                                }
                            }
                            // Future-proofing: if classify adds another
                            // Honored var but the match above doesn't
                            // know what to do with it, store + warn.
                            Some(PsqlVarKind::Honored) => {
                                t_variables.update(|m| { m.insert(name.clone(), value.clone()); });
                                meta_respond(vec![format!(
                                    "{} = '{}'  (honored but no handler wired — please report).",
                                    name, value,
                                )]);
                            }
                            // Accepted-no-op — store so `:NAME`
                            // substitution returns the value, but warn
                            // that the psql semantics aren't honored.
                            Some(PsqlVarKind::AcceptedNoOp) => {
                                t_variables.update(|m| { m.insert(name.clone(), value.clone()); });
                                meta_respond(vec![format!(
                                    "{} = '{}'  (accepted; tyto doesn't yet implement psql's behavior for this variable).",
                                    name, value,
                                )]);
                            }
                            // Anything else — ordinary user variable.
                            None => set(&name, &value),
                        }
                        return;
                    }
                    if let MetaAction::UnsetVariable(name) = action {
                        use crate::psql_vars::{classify, PsqlVarKind};
                        // Refuse unset of read-only vars — they're
                        // tyto's snapshot of session state, not user-
                        // controllable.
                        if classify(&name) == Some(PsqlVarKind::ReadOnly) {
                            meta_respond(vec![format!(
                                "{}: read-only (can't be unset).",
                                name,
                            )]);
                            return;
                        }
                        // Silently, as psql's \unset — which sets those
                        // with hooks to their unset value (`AUTOCOMMIT`
                        // `off`), their effect with them.
                        let _ = tab_sv.with_value(|t| set_variable(t, &name, None));
                        clear_typed();
                        return;
                    }
                    if let MetaAction::ListVariables = action {
                        let vars = t_variables.get();
                        let lines: Vec<String> = if vars.is_empty() {
                            vec!["No variables set.".into()]
                        } else {
                            let mut sorted: Vec<_> = vars.iter().collect();
                            sorted.sort_by_key(|(k, _)| (*k).clone());
                            sorted.iter().map(|(k, v)| format!("{} = '{}'", k, v)).collect()
                        };
                        meta_respond(lines);
                        return;
                    }

                    if let MetaAction::Introspect { sql, tag } = action {
                        t_hist_pos.set(None);
                        t_hist_saved.set(String::new());
                        t_input.set(String::new());
                        reset_shell_input_height(&pid3);
                        let sid = t_session_id.get();
                        if let Some(sid) = sid.filter(|_| tab_sv.with_value(|t| echo_hidden(t, std::slice::from_ref(&sql)))) {
                            // Busy until its reply: what follows waits.
                            t_querying.set(true);
                            ws_introspect(ws, &sid, sql, tag);
                        }
                        return;
                    }

                    t_hist_pos.set(None);
                    t_hist_saved.set(String::new());
                    t_input.set(String::new());
                    reset_shell_input_height(&pid3);
                    match action {
                        MetaAction::Disconnect => { close_pane(pid3.clone()); }
                        MetaAction::RunSql(sql, vertical) => {
                            // Same session guard as submit_query — see
                            // audit finding 2.7. Meta-commands route
                            // through a different submit path but share
                            // the same "queue against a dead session
                            // and stall" failure mode.
                            // The command's own SQL goes as built: psql
                            // expands variables in what's typed, not in
                            // what a command generates.
                            let to = tab_sv.with_value(|t| send_to(t, ws));
                            // Not connected: psql says so, and shows nothing.
                            let shown = |to: &SendTo| matches!(to, SendTo::NotConnected(_))
                                || tab_sv.with_value(|t| echo_hidden(t, std::slice::from_ref(&sql)));
                            if let Some(to) = to.filter(shown) {
                                t_pend_vert.set(vertical);
                                t_pend_list.set(None);
                                t_querying.set(true);
                                tab_sv.with_value(|t| deliver(t, ws, to, sql));
                            }
                        }
                        MetaAction::List { sql, vertical, title, none, counts_rows, footers } => {
                            let to = tab_sv.with_value(|t| send_to(t, ws));
                            // Not connected: psql says so, and shows nothing.
                            let shown = |to: &SendTo| matches!(to, SendTo::NotConnected(_))
                                || tab_sv.with_value(|t| echo_hidden(t, std::slice::from_ref(&sql)));
                            if let Some(to) = to.filter(shown) {
                                // Expanded as psql's `printQuery` prints
                                // a list: its `x` (`\dtx`), or `\x on`.
                                t_pend_vert.set(vertical || t_expanded.get_untracked() == Expanded::On);
                                // For the reply: psql's title over its
                                // rows, or `none` when there are none.
                                t_pend_list.set(Some(crate::types::ListReply { title, none, counts_rows, footers, held: None }));
                                t_querying.set(true);
                                tab_sv.with_value(|t| deliver(t, ws, to, sql));
                            }
                        }
                        // Nothing to say: psql's silent success.
                        MetaAction::Output(lines) if lines.is_empty() => {}
                        MetaAction::Output(lines) => {
                            t_history.update(|h| h.push(ShellEntry::MetaOutput(lines)));
                        }
                        // `\p`: the buffer as the CLI holds it — or, psql's,
                        // the previous query — said as its `\p` says it.
                        MetaAction::PrintBuffer => {
                            let previous = t_history.with_untracked(|h| previous_buffer(t_plane, h));
                            let lines = t_plane.print_buffer(typed_before, previous.as_deref(), &tab_sv.with_value(ShellTab::meta_context));
                            if !lines.is_empty() {
                                t_history.update(|h| h.push(ShellEntry::MetaOutput(lines)));
                            }
                        }
                        MetaAction::OutputKV(pairs) => {
                            t_history.update(|h| h.push(ShellEntry::KeyValue(pairs)));
                        }
                        // The command as the plane read it (a statement
                        // run as one without its terminator), for its
                        // next steps.
                        MetaAction::Staged(queries) => {
                            let command = command.trim_end().to_string();
                            tab_sv.with_value(|t| start_staged_meta(t, ws, command, queries));
                        }
                        MetaAction::ClearInput(_) | MetaAction::Watch { .. } | MetaAction::ReconnectDb(_) | MetaAction::Connect { .. } | MetaAction::SetDelimiter(_) | MetaAction::SetExpanded(_) | MetaAction::SetTiming(_) | MetaAction::SafeMode(_) | MetaAction::RebuildCompletion | MetaAction::ShowWarnings { .. } | MetaAction::Introspect { .. } | MetaAction::SetNullDisplay(_) | MetaAction::SetTextOptions { .. } | MetaAction::SetVariable(..) | MetaAction::UnsetVariable(_) | MetaAction::ListVariables | MetaAction::SetFormat { .. } | MetaAction::EditExternal { .. } | MetaAction::Go { .. } | MetaAction::ErrVerbose | MetaAction::ShowHistory | MetaAction::SetTuplesOnly(_) | MetaAction::SetFooter(_) | MetaAction::SetTitle(_) | MetaAction::Prompt { .. } | MetaAction::Gexec | MetaAction::Gdesc | MetaAction::Gset { .. } | MetaAction::Crosstabview { .. } | MetaAction::Password { .. } | MetaAction::Fail(_) | MetaAction::Branch(_) => unreachable!(),
                    }
                    scroll_shell_to_bottom(&pid3);
                };
                run();
                if !warnings.is_empty() {
                    if sends_buffer || !tab_sv.with_value(shell_busy) {
                        t_history.update(|h| h.extend(warnings));
                    } else {
                        t_warnings.update(|held| held.extend(warnings));
                    }
                }

                // What the command left reads on — at once, or, when it
                // made the shell wait, once the shell is free.
                if kept.is_empty() {
                    return true;
                }
                if tab_sv.with_value(shell_busy) {
                    t_resume.set(Some(kept));
                    return true;
                }
                text = kept;
            }
        })
    };

    // What's left of an input reads on once the shell is free (`shell_busy`
    // says when). Run after the change that freed it — never inside the
    // handler that's still freeing it (a prompt answered, a confirm's
    // Execute about to send).
    create_effect(move |_| {
        if tab_sv.with_value(shell_busy) || (t_resume.with(Option::is_none) && t_warnings.with(Vec::is_empty)) {
            return;
        }
        // Outside any event or effect, as a keystroke's handler is. The
        // pane may have closed since (its handles gone): then nothing.
        leptos::spawn_local(async move { untrack(|| {
            if tab_sv.try_with_value(shell_busy) != Some(false) {
                return;
            }
            // What the CLI says once the command is done comes before
            // what reads on.
            let warnings = t_warnings.try_update(std::mem::take).unwrap_or_default();
            if !warnings.is_empty() {
                t_history.update(|h| h.extend(warnings));
                tab_sv.with_value(|t| scroll_shell_to_bottom(&t.id));
            }
            let Some(rest) = t_resume.try_update(Option::take).flatten() else { return };
            // Typed meanwhile, not yet entered: it stays, after whatever
            // the input holds once this has run.
            let typed = t_input.get_untracked();
            t_input.set(String::new());
            let left = if process_input.call(rest.clone()) { t_input.get_untracked() } else { rest };
            if !(left.is_empty() && typed.is_empty()) {
                t_input.set(format!("{left}{typed}"));
                tab_sv.with_value(|t| schedule_textarea_resize(&t.id));
            }
        }) });
    });

    let pid3 = pane_id.clone();
    let on_keydown = move |e: web_sys::KeyboardEvent| {
        active_pane_id.set(Some(pid3.clone()));

        // Helper: reset input state and push a MetaOutput message.
        // The command was taken: the input starts over.
        let clear_typed = || {
            t_hist_pos.set(None);
            t_hist_saved.set(String::new());
            t_input.set(String::new());
            reset_shell_input_height(&pid3);
        };
        let meta_respond = |msg: Vec<String>| {
            clear_typed();
            t_history.update(|h| h.push(ShellEntry::MetaOutput(msg)));
            scroll_shell_to_bottom(&pid3);
        };

        // ── External edit in progress ──────────────────────────────────
        // The input is read-only until the editor returns; Esc stops
        // waiting, and Enter mustn't run the half-edited buffer.
        if t_external_edit.with_untracked(Option::is_some) {
            match e.key().as_str() {
                "Escape" => {
                    e.prevent_default();
                    tab_sv.with_value(abandon_external_edit);
                }
                "Enter" => e.prevent_default(),
                _ => {}
            }
            return;
        }

        // ── \password mode short-circuit ───────────────────────────────
        // Two-stage masked prompt: stage 1 captures the new password,
        // stage 2 captures the retype. On match, the shell SCRAM-
        // hashes browser-side and submits `ALTER USER … PASSWORD
        // '<hash>'`. The plaintext password lives only inside
        // `password_prompt` and is dropped the instant stage 2 fires
        // (match OR mismatch). The highlight overlay renders `•` for
        // every input character while this state is active.
        //
        // Submit-path bypasses `submit_query` so neither the hash
        // SQL nor the redacted form lands in localStorage history —
        // the user shouldn't be able to arrow-up to retrieve it.
        if let Some(state) = t_password_prompt.get_untracked() {
            if e.key() == "Enter" && !e.shift_key() {
                e.prevent_default();
                let value = t_input.get_untracked();
                t_input.set(String::new());
                reset_shell_input_height(&pid3);
                match state.first {
                    // Stage 1: capture the new password, advance to retype.
                    None => {
                        t_password_prompt.set(Some(PasswordPromptState {
                            user:  state.user.clone(),
                            first: Some(value),
                        }));
                        t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                            "[Retype new password — Enter to apply, Esc to cancel]".into(),
                        ])));
                        scroll_shell_to_bottom(&pid3);
                        return;
                    }
                    // Stage 2: compare; on match, hash + submit; on
                    // mismatch, abort.
                    Some(first) => {
                        t_password_prompt.set(None);
                        if first != value {
                            // Failed, as psql's does: nothing more said.
                            t_warnings.set(Vec::new());
                            t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                                "\\password: passwords don't match — no change made.".into(),
                            ])));
                            scroll_shell_to_bottom(&pid3);
                            return;
                        }
                        // Resolve target user — explicit arg wins,
                        // otherwise pull from `:USER` (populated at
                        // SessionOpened).
                        let target = state.user.clone().unwrap_or_else(|| {
                            t_variables.with_untracked(|m| m.get("USER").cloned().unwrap_or_default())
                        });
                        if target.is_empty() {
                            t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                                "\\password: no target user (:USER is empty; pass an explicit name).".into(),
                            ])));
                            scroll_shell_to_bottom(&pid3);
                            return;
                        }
                        // Compute the SCRAM hash. getrandom can fail
                        // in odd browser contexts; surface the error
                        // rather than sending a bogus ALTER USER.
                        let hash = match crate::psql_password::scram_sha256_hash(&value) {
                            Ok(h)  => h,
                            Err(e) => {
                                t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![e])));
                                scroll_shell_to_bottom(&pid3);
                                return;
                            }
                        };
                        // PG identifier + literal quoting from the
                        // plane (canonical, tested) — shell-side meta
                        // commands like \password are PG-only so the
                        // dependency is sound.
                        use crate::plane::postgres::{pg_quote_ident, pg_quote_literal};
                        let real_sql = format!(
                            "ALTER USER {} PASSWORD {}",
                            pg_quote_ident(&target), pg_quote_literal(&hash),
                        );
                        // Bypass submit_query so neither the real SQL
                        // (with the hash) nor a redacted form lands in
                        // the in-memory `t_history` Query stream — that
                        // stream is what arrow-up / Ctrl-R search read
                        // from `query_strings()`. Instead we push a
                        // MetaOutput describing what was sent; the
                        // QueryResult lands afterwards as normal so the
                        // user still gets a success / error indicator.
                        let Some(sid) = t_session_id.get_untracked() else {
                            t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                                "\\password: not connected — no session.".into(),
                            ])));
                            scroll_shell_to_bottom(&pid3);
                            return;
                        };
                        if t_session_stale.get_untracked() {
                            t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                                "\\password: connection is reconnecting — try again in a moment.".into(),
                            ])));
                            scroll_shell_to_bottom(&pid3);
                            return;
                        }
                        t_hist_pos.set(None);
                        t_hist_saved.set(String::new());
                        t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                            format!(
                                "[\\password {}] submitted ALTER USER … PASSWORD '<redacted>'.",
                                pg_quote_ident(&target),
                            ),
                        ])));
                        t_querying.set(true);
                        // Not remembered as the running SQL: an error's
                        // excerpt would show the password hash.
                        t_sent_sql.set(None);
                        ws_send(ws, &ClientMessage::ExecuteQuery {
                            session_id: sid,
                            sql:        real_sql,
                        });
                        scroll_shell_to_bottom(&pid3);
                        return;
                    }
                }
            }
            if e.key() == "Escape" {
                e.prevent_default();
                t_password_prompt.set(None);
                t_warnings.set(Vec::new());
                t_input.set(String::new());
                reset_shell_input_height(&pid3);
                t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                    "\\password cancelled.".into(),
                ])));
                scroll_shell_to_bottom(&pid3);
                return;
            }
            // Other keys (typing the password chars) fall through.
            // The highlight overlay masks the rendered chars with `•`
            // so what's on screen looks like a password field.
        }

        // ── \prompt mode short-circuit ─────────────────────────────────
        // When a `\prompt` is active, intercept Enter / Esc here
        // before any other shell handler runs. Enter takes the
        // current input as the variable's value (without invoking
        // submit_query); Esc cancels without setting. Any other key
        // falls through and types into the input normally.
        if let Some(state) = t_prompt_state.get_untracked() {
            if e.key() == "Enter" && !e.shift_key() {
                e.prevent_default();
                let value = t_input.get_untracked();
                t_input.set(String::new());
                reset_shell_input_height(&pid3);
                t_prompt_state.set(None);
                // Same routing as MetaAction::SetVariable so a
                // \prompt of a Honored / AcceptedNoOp name produces
                // the same notice the user would see from \set.
                use crate::psql_vars::{classify, PsqlVarKind};
                let name = state.name.clone();
                match classify(&name) {
                    Some(PsqlVarKind::ReadOnly) => {
                        // Defensive — the meta-action arm already
                        // rejects this; reaching here would be a bug.
                        meta_respond(vec![format!(
                            "{}: read-only (can't be set with \\prompt).",
                            name,
                        )]);
                    }
                    // As `\set` sets it — silently, as psql's \prompt,
                    // or refused in its words.
                    _ => match tab_sv.with_value(|t| set_variable(t, &name, Some(value.clone()))) {
                        Ok(()) => clear_typed(),
                        Err(refused) => meta_respond(refused),
                    },
                }
                return;
            }
            if e.key() == "Escape" {
                e.prevent_default();
                t_prompt_state.set(None);
                t_warnings.set(Vec::new());
                t_input.set(String::new());
                reset_shell_input_height(&pid3);
                t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                    format!("\\prompt {} cancelled.", state.name),
                ])));
                scroll_shell_to_bottom(&pid3);
                return;
            }
            // Other keys (typing the value) fall through to the
            // normal textarea input handling below.
        }

        // ── Ctrl+R reverse incremental search ──────────────────────────
        if e.key() == "r" && e.ctrl_key() && !e.meta_key() {
            e.prevent_default();
            if t_search_on.get() {
                t_search_pos.update(|p| *p += 1);
            } else {
                t_search_on.set(true);
                t_search_term.set(String::new());
                t_search_pos.set(0);
            }
            return;
        }

        // When search is active, intercept keys for the search bar
        if t_search_on.get() {
            match e.key().as_str() {
                "Escape" => {
                    e.prevent_default();
                    t_search_on.set(false);
                }
                "Enter" => {
                    e.prevent_default();
                    let term = t_search_term.get();
                    let pos = t_search_pos.get();
                    if let Some(cmd) = search_match(&term, pos) {
                        t_input.set(cmd);
                        schedule_textarea_resize(&pid3);
                    }
                    t_search_on.set(false);
                }
                "Backspace" => {
                    e.prevent_default();
                    t_search_term.update(|s| { s.pop(); });
                    t_search_pos.set(0);
                }
                key if key.len() == 1 && !e.ctrl_key() && !e.meta_key() && !e.alt_key() => {
                    e.prevent_default();
                    t_search_term.update(|s| s.push_str(key));
                    t_search_pos.set(0);
                }
                _ => {}
            }
            return;
        }

        // Ctrl+L — clear shell history (like terminal `clear`)
        if e.key() == "l" && e.ctrl_key() && !e.meta_key() {
            e.prevent_default();
            t_history.set(vec![]);
            scroll_shell_to_bottom(&pid3);
            return;
        }

        // ── Bracket auto-pair: `(` inserts `()`, `)` over `)` steps,
        //    Backspace inside `()` deletes both ────────────────────────────
        if !e.ctrl_key() && !e.meta_key() && !e.alt_key() {
            let key = e.key();
            let trigger = matches!(key.as_str(), "(" | ")" | "Backspace");
            if trigger {
                if let Some(ta) = web_sys::window()
                    .and_then(|w| w.document())
                    .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid3)))
                    .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                {
                    let start = ta.selection_start().ok().flatten().unwrap_or(0) as usize;
                    let end   = ta.selection_end().ok().flatten().unwrap_or(start as u32) as usize;
                    let value = ta.value();
                    let bytes = value.as_bytes();
                    let action: Option<(String, usize)> = match key.as_str() {
                        "(" => {
                            // Insert "()" (wrap selection if any), cursor between.
                            let mut new = value.clone();
                            new.replace_range(start..end, "()");
                            Some((new, start + 1))
                        }
                        ")" => {
                            // Step over an existing `)` instead of typing a second one.
                            if start == end && bytes.get(start) == Some(&b')') {
                                Some((value.clone(), start + 1))
                            } else {
                                None
                            }
                        }
                        "Backspace" => {
                            // Delete the empty `()` pair as one keystroke.
                            if start == end
                                && start > 0
                                && bytes.get(start - 1) == Some(&b'(')
                                && bytes.get(start) == Some(&b')')
                            {
                                let mut new = value.clone();
                                new.replace_range(start - 1..start + 1, "");
                                Some((new, start - 1))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some((new_value, new_cursor)) = action {
                        e.prevent_default();
                        t_input.set(new_value);
                        let pid_paint = pid3.clone();
                        let new_pos = new_cursor as u32;
                        let paint_cb = Closure::once(move || {
                            if let Some(ta) = web_sys::window()
                                .and_then(|w| w.document())
                                .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid_paint)))
                                .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                            {
                                let _ = ta.set_selection_range(new_pos, new_pos);
                                let _ = ta.style().set_property("height", "auto");
                                let _ = ta.style().set_property(
                                    "height", &format!("{}px", ta.scroll_height()));
                            }
                        });
                        if let Some(win) = web_sys::window() {
                            let _ = win.request_animation_frame(paint_cb.as_ref().unchecked_ref());
                        }
                        paint_cb.forget();
                        return;
                    }
                }
            }
        }

        // Ctrl+C stops a running `\watch`, as psql's does, and what was
        // typed meanwhile with it — unless text is selected in the input,
        // which it copies.
        let selecting = || document().get_element_by_id(&format!("shell-input-{pid3}"))
            .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
            .is_some_and(|ta| ta.selection_start().ok().flatten() != ta.selection_end().ok().flatten());
        if e.key() == "c" && e.ctrl_key() && !e.meta_key() && !e.alt_key()
            && t_watch_sql.get_untracked().is_some() && !selecting()
        {
            e.prevent_default();
            interrupt_watch();
            return;
        }
        // Inside psql's \if, Ctrl+C at the prompt — the shell waiting
        // for input, not for a reply or a prompt's answer — leaves the
        // innermost block, as psql's does, and drops the query buffer.
        if e.key() == "c" && e.ctrl_key() && !e.meta_key() && !e.alt_key()
            && t_conditionals.with_untracked(|stack| !stack.is_empty()) && !selecting()
            && !untrack(|| tab_sv.with_value(shell_busy))
        {
            e.prevent_default();
            t_conditionals.update(|stack| { stack.pop(); });
            t_history.update(|h| h.push(cli_said(vec![crate::psql_if::ESCAPED.into()])));
            clear_typed();
            scroll_shell_to_bottom(&pid3);
            return;
        }

        // ── Readline keybindings (Ctrl+A/E/W/U/K) ──────────────────
        if e.ctrl_key() && !e.meta_key() && !e.alt_key() {
            let key = e.key();
            let handled = match key.as_str() {
                "a" => {
                    // Ctrl+A: move cursor to start of line
                    with_shell_textarea(&pid3, |ta| { let _ = ta.set_selection_range(0, 0); });
                    true
                }
                "e" => {
                    // Ctrl+E: move cursor to end of line
                    with_shell_textarea(&pid3, |ta| {
                        let len = ta.value().len() as u32;
                        let _ = ta.set_selection_range(len, len);
                    });
                    true
                }
                "u" => {
                    // Ctrl+U: delete from cursor to start of line
                    with_shell_textarea(&pid3, |ta| {
                        let pos = ta.selection_start().ok().flatten().unwrap_or(0) as usize;
                        let val = ta.value();
                        let after = &val[pos..];
                        t_input.set(after.to_string());
                        let _ = ta.set_selection_range(0, 0);
                    });
                    true
                }
                "k" => {
                    // Ctrl+K: delete from cursor to end of line
                    with_shell_textarea(&pid3, |ta| {
                        let pos = ta.selection_start().ok().flatten().unwrap_or(0) as usize;
                        let val = ta.value();
                        let before = &val[..pos];
                        t_input.set(before.to_string());
                    });
                    true
                }
                "w" => {
                    // Ctrl+W: delete word backward from cursor
                    with_shell_textarea(&pid3, |ta| {
                        let pos = ta.selection_start().ok().flatten().unwrap_or(0) as usize;
                        let val = ta.value();
                        let before = &val[..pos];
                        let trimmed = before.trim_end();
                        let word_start = trimmed.rfind(|c: char| c.is_whitespace())
                            .map(|i| i + 1).unwrap_or(0);
                        let new_val = format!("{}{}", &val[..word_start], &val[pos..]);
                        t_input.set(new_val);
                        let _ = ta.set_selection_range(word_start as u32, word_start as u32);
                    });
                    true
                }
                _ => false,
            };
            if handled {
                e.prevent_default();
                return;
            }
        }

        // ── Tab — completion ──────────────────────────────────────
        // Three states:
        //   1. Popup open for this pane: Tab cycles forward, Shift+Tab back.
        //   2. Popup closed, cursor in a completable token: open the popup
        //      (or directly insert when there's exactly one candidate).
        //   3. Cursor in a string / comment / no candidates: prevent_default
        //      so Tab doesn't insert a literal tab character, then bail.
        if e.key() == "Tab" {
            e.prevent_default();
            // Cycle if already open.
            let open_for_pane = completion_popup.get_untracked()
                .as_ref()
                .map(|p| p.pane_id == pid3)
                .unwrap_or(false);
            if open_for_pane {
                let dir: i64 = if e.shift_key() { -1 } else { 1 };
                completion_popup.update(|opt| {
                    if let Some(p) = opt {
                        if !p.items.is_empty() {
                            let n = p.items.len() as i64;
                            let cur = p.selected as i64;
                            let nxt = ((cur + dir) % n + n) % n;
                            p.selected = nxt as usize;
                        }
                    }
                });
                return;
            }
            // Try to open. Read cursor + input.
            let input = t_input.get_untracked();
            let cursor: usize = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid3)))
                .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                .and_then(|ta| ta.selection_start().ok().flatten())
                .map(|n| n as usize)
                .unwrap_or_else(|| input.len());
            let hash = t_plane.lexicon().hash_comments();
            let Some(analysis) = crate::completion::analyze_context(&input, cursor, hash) else {
                return;
            };
            // For Table / TableInSchema, kick off a one-shot introspect
            // when the per-pane cache is still NotLoaded. The first Tab
            // does the fetch; the user retries Tab once the response
            // lands and the popup opens populated.
            let needs_tables = matches!(
                analysis.context,
                crate::completion::CompletionContext::Table { .. } |
                crate::completion::CompletionContext::TableInSchema { .. } |
                crate::completion::CompletionContext::TableOrKeyword { .. }
            );
            let cache_snapshot = if needs_tables {
                let snap = tab.completion_tables.get_untracked();
                if matches!(snap, crate::types::LoadState::NotLoaded) {
                    // Fire fetch and bail. Setting Loading first so a
                    // second Tab press doesn't re-fire the same introspect.
                    tab.completion_tables.set(crate::types::LoadState::Loading);
                    if let Some(sid) = tab.session_id.get_untracked() {
                        crate::dom::ws_introspect(
                            ws, &sid,
                            t_plane.completion_tables_sql(),
                            "completion_tables".into(),
                        );
                    }
                    return;
                }
                match snap {
                    crate::types::LoadState::Loaded(m) => Some(m),
                    _ => None,
                }
            } else {
                None
            };
            let current_db = tab.current_db.get_untracked();
            // Materialize the Loaded subset of the column cache so the
            // analyzer can lookup columns without seeing LoadState.
            let columns_loaded: std::collections::HashMap<(String, String), Vec<String>> =
                tab.completion_columns.get_untracked()
                    .into_iter()
                    .filter_map(|(k, v)| match v {
                        crate::types::LoadState::Loaded(cols) => Some((k, cols)),
                        _ => None,
                    })
                    .collect();
            // Build the alias map (Phase 2 + 3 + star expansion). Stars
            // referencing tables whose columns aren't loaded surface in
            // `stars_pending`; fire prefetches so the next Tab gets a
            // fully expanded projection.
            let extraction = crate::completion::extract_aliases(
                &input,
                &t_plane.default_completion_schema(&current_db),
                &columns_loaded,
            );
            for (sch, tbl) in &extraction.stars_pending {
                let key = (sch.clone(), tbl.clone());
                let already = matches!(
                    tab.completion_columns.with_untracked(|m| m.get(&key).cloned()),
                    Some(crate::types::LoadState::Loaded(_)) |
                    Some(crate::types::LoadState::Loading)
                );
                if !already {
                    tab.completion_columns.update(|m| {
                        m.insert(key.clone(), crate::types::LoadState::Loading);
                    });
                    if let Some(sid) = tab.session_id.get_untracked() {
                        crate::dom::ws_introspect(
                            ws, &sid,
                            t_plane.completion_columns_sql(sch, tbl),
                            format!("completion_columns:{}|{}", sch, tbl),
                        );
                    }
                }
            }
            let cands = crate::completion::gather_candidates(
                &analysis.context, t_plane,
                cache_snapshot.as_ref(),
                &columns_loaded,
                &extraction.aliases,
                &current_db,
            );
            if cands.is_empty() {
                return;
            }
            if cands.len() == 1 {
                // Single candidate — insert directly without showing a popup.
                let before = &input[..analysis.replace_start];
                let after  = &input[cursor..];
                let candidate = &cands[0];
                let new_sql = format!("{}{}{}", before, candidate, after);
                let new_cursor = analysis.replace_start + candidate.len();
                t_input.set(new_sql);
                let pid_paint = pid3.clone();
                let paint_cb = wasm_bindgen::closure::Closure::once(move || {
                    if let Some(ta) = web_sys::window()
                        .and_then(|w| w.document())
                        .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid_paint)))
                        .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                    {
                        let pos = new_cursor as u32;
                        let _ = ta.set_selection_range(pos, pos);
                    }
                });
                if let Some(win) = web_sys::window() {
                    let _ = win.request_animation_frame(paint_cb.as_ref().unchecked_ref());
                }
                paint_cb.forget();
                return;
            }
            // Multiple candidates — open the popup anchored below the
            // textarea. Pixel-exact cursor positioning is deferred (see plan).
            let anchor = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid3)))
                .and_then(|el| el.dyn_into::<web_sys::Element>().ok())
                .map(|el| {
                    let r = el.get_bounding_client_rect();
                    (r.left(), r.bottom() + 4.0)
                })
                .unwrap_or((100.0, 100.0));
            completion_popup.set(Some(CompletionPopup {
                pane_id:       pid3.clone(),
                x:             anchor.0,
                y:             anchor.1,
                items:         cands,
                selected:      0,
                replace_start: analysis.replace_start,
            }));
            return;
        }

        // ── Popup-open: capture navigation + accept/cancel keys ─────
        // Without this block the popup's parent textarea handles Up/Down
        // (history walk) and other navigation keys, even though the
        // user's focus intent is the popup. Arrow Left/Right and
        // Home/End walk OUT of the completable token, so they close
        // the popup and let the cursor move normally.
        if completion_popup.get_untracked()
            .as_ref()
            .map(|p| p.pane_id == pid3)
            .unwrap_or(false)
        {
            match e.key().as_str() {
                "Escape" => {
                    e.prevent_default();
                    completion_popup.set(None);
                    return;
                }
                "ArrowDown" => {
                    e.prevent_default();
                    completion_popup.update(|opt| {
                        if let Some(p) = opt {
                            if !p.items.is_empty() {
                                p.selected = (p.selected + 1) % p.items.len();
                            }
                        }
                    });
                    return;
                }
                "ArrowUp" => {
                    e.prevent_default();
                    completion_popup.update(|opt| {
                        if let Some(p) = opt {
                            if !p.items.is_empty() {
                                let n = p.items.len();
                                p.selected = (p.selected + n - 1) % n;
                            }
                        }
                    });
                    return;
                }
                "ArrowLeft" | "ArrowRight" | "Home" | "End" | "PageUp" | "PageDown" => {
                    // Close the popup but let the cursor still move —
                    // no prevent_default. The user is "walking out of
                    // the token"; popup loses relevance, browser handles
                    // the cursor motion itself.
                    completion_popup.set(None);
                    // fall through (return so the main match below
                    // doesn't see the key — these are pure cursor moves
                    // and the shell has no other handler for them).
                    return;
                }
                "Enter" => {
                    e.prevent_default();
                    if let Some(p) = completion_popup.get_untracked() {
                        if let Some(candidate) = p.items.get(p.selected).cloned() {
                            let input  = t_input.get_untracked();
                            let cursor: usize = web_sys::window()
                                .and_then(|w| w.document())
                                .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid3)))
                                .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                .and_then(|ta| ta.selection_start().ok().flatten())
                                .map(|n| n as usize)
                                .unwrap_or_else(|| input.len());
                            let before = &input[..p.replace_start];
                            let after  = &input[cursor.min(input.len())..];
                            let new_sql = format!("{}{}{}", before, candidate, after);
                            let new_cursor = p.replace_start + candidate.len();
                            t_input.set(new_sql);
                            let pid_paint = pid3.clone();
                            let paint_cb = wasm_bindgen::closure::Closure::once(move || {
                                if let Some(ta) = web_sys::window()
                                    .and_then(|w| w.document())
                                    .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid_paint)))
                                    .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                {
                                    let pos = new_cursor as u32;
                                    let _ = ta.set_selection_range(pos, pos);
                                }
                            });
                            if let Some(win) = web_sys::window() {
                                let _ = win.request_animation_frame(paint_cb.as_ref().unchecked_ref());
                            }
                            paint_cb.forget();
                        }
                    }
                    completion_popup.set(None);
                    return;
                }
                _ => {}
            }
        }

        match e.key().as_str() {
            "Enter" if !e.shift_key() => {
                let typed = t_input.get();
                // What the CLI says about text that stays in a statement
                // (mysql's `Unknown command '\i'.`, psql's `Use \q to
                // quit.`) shows as it reads each line: with the statement,
                // when it's sent — or, left waiting for more, under what
                // runs before it (`process_input`).
                if t_resume.with_untracked(Option::is_some) || untrack(|| tab_sv.with_value(shell_busy)) {
                    // Typed while the shell waits — for a statement's
                    // reply, a `\watch` (whose Ctrl+C drops it,
                    // `interrupt_watch`) — or while what's left of earlier
                    // input waits its turn: it reads on after that, as the
                    // CLI reads type-ahead once it's done.
                    e.prevent_default();
                    let typed = typed.trim_end();
                    if !typed.trim_start().is_empty() {
                        t_resume.update(|rest| match rest {
                            Some(rest) => {
                                rest.push('\n');
                                rest.push_str(typed);
                            }
                            None => *rest = Some(typed.to_string()),
                        });
                    }
                    clear_typed();
                } else if process_input.call(typed) {
                    e.prevent_default();
                }
            }
            "ArrowUp" => {
                let qs = query_strings();
                if qs.is_empty() { return; }
                let val = t_input.get();
                if val.contains('\n') && t_hist_pos.get().is_none() { return; }
                e.prevent_default();
                let prefix = match t_hist_pos.get() {
                    None    => { t_hist_saved.set(val.clone()); val }
                    Some(_) => t_hist_saved.get(),
                };
                let filtered: Vec<String> = if prefix.is_empty() {
                    qs
                } else {
                    qs.into_iter().filter(|s| s.starts_with(&prefix)).collect()
                };
                if filtered.is_empty() { return; }
                let new_pos = match t_hist_pos.get() {
                    None    => 0,
                    Some(i) => (i + 1).min(filtered.len() - 1),
                };
                t_hist_pos.set(Some(new_pos));
                t_input.set(filtered[filtered.len() - 1 - new_pos].clone());
                schedule_textarea_resize(&pid3);
            }
            "ArrowDown" => {
                let Some(pos) = t_hist_pos.get() else { return };
                e.prevent_default();
                if pos == 0 {
                    t_hist_pos.set(None);
                    t_input.set(t_hist_saved.get());
                } else {
                    let prefix = t_hist_saved.get();
                    let qs = query_strings();
                    let filtered: Vec<String> = if prefix.is_empty() {
                        qs
                    } else {
                        qs.into_iter().filter(|s| s.starts_with(&prefix)).collect()
                    };
                    let new_pos = pos - 1;
                    t_hist_pos.set(Some(new_pos));
                    if let Some(cmd) = filtered.get(filtered.len() - 1 - new_pos) {
                        t_input.set(cmd.clone());
                    }
                }
                schedule_textarea_resize(&pid3);
            }
            _ => {}
        }
    };

    let input_id  = format!("shell-input-{}", pane_id);
    let output_id = format!("shell-output-{}", pane_id);
    let iid_click    = input_id.clone();
    let pid_confirm  = pane_id.clone();
    let _pid_disc     = pane_id.clone();
    let _pid_split_h  = pane_id.clone();
    let _pid_split_v  = pane_id.clone();
    let _pid_add_tab  = pane_id.clone();
    let _pid_menu     = pane_id.clone();
    let _t_menu_open  = tab.pane_menu_open;
    let _t_plane_ref  = t_plane;

    // Capture pane_id for the broadened contextmenu handler on the
    // outer `.shell` div. Pre-fix this handler lived only on the
    // `<textarea>` — a tight click target, easy to miss. Hoisting it
    // to the wrapper means right-click anywhere in the pane (scroll
    // content, prompt, gutter) surfaces the menu. The native browser
    // context menu is suppressed via `prevent_default()` only when
    // the menu is set; right-clicks on widgets that want their own
    // menu (none today) would need to call `stop_propagation` first.
    let pid_ctx = pane_id.clone();
    // Links in psql's `\d` output open the explorer next to this shell.
    let open_here: Callback<ExplorerInit> = {
        let pid = pane_id.clone();
        Callback::new(move |init: ExplorerInit| pane_ops.open_explorer.call((pid.clone(), init)))
    };

    view! {
        <div class="shell"
             on:contextmenu=move |ev: web_sys::MouseEvent| {
                 ev.prevent_default();
                 shell_input_menu.set(Some(ShellInputContextMenu {
                     pane_id:   pid_ctx.clone(),
                     x:         ev.client_x() as f64,
                     y:         ev.client_y() as f64,
                     block_num: None,
                 }));
             }>
            // Single horizontal status-strip: TXN badge + session-id
            // pill + buffer pill all share one row instead of stacking
            // (the `.shell` container is flex-direction:column, so
            // direct children were each taking their own row and
            // wasting vertical real estate).
            <div class="shell-status-strip">
                {move || match t_txn_state.get() {
                    TxnState::Active => view! {
                        <div class="txn-badge txn-active" title="Session has an open transaction (BEGIN without COMMIT)">"TXN"</div>
                    }.into_view(),
                    TxnState::Failed => view! {
                        <div class="txn-badge txn-failed" title="Transaction failed — every command is refused until ROLLBACK (or ROLLBACK TO a savepoint)">"TXN FAILED"</div>
                    }.into_view(),
                    TxnState::Idle => view! { <span/> }.into_view(),
                }}
                {session_id_pill(t_db_conn_id, tab.session_stale)}
                {shell_buffer_pill(t_history)}
            </div>

            <div class="shell-connecting"
                style=move || if t_connecting.get() { "" } else { "display:none" }
            >
                "Connecting…"
            </div>

            <div id=output_id class="shell-output"
                style=move || if t_connecting.get() { "display:none" } else { "" }
                on:click=move |_| {
                    use wasm_bindgen::JsCast;
                    // If the user just finished a drag-select inside the
                    // shell output, the browser's selection is non-empty at
                    // click time. Skip the focus-steal so the highlight
                    // survives long enough to be copied. A normal single-
                    // click leaves the selection empty, and we fall through
                    // to the original focus-input-on-click behavior.
                    let has_text_selection = web_sys::window()
                        .and_then(|w| w.get_selection().ok().flatten())
                        .and_then(|sel| sel.to_string().as_string())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false);
                    if has_text_selection {
                        return;
                    }
                    // In place: a click while reading older output
                    // mustn't jump to the bottom.
                    if let Some(el) = web_sys::window()
                        .and_then(|w| w.document())
                        .and_then(|d| d.get_element_by_id(&iid_click))
                        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        focus_in_place(&el);
                    }
                }
            >
                // Group the flat history into BLOCKS. Each block is
                // one `ShellEntry::Query` plus the result / meta
                // entries that arrived before the next Query. Each
                // block gets its own wrapper div carrying the
                // block's sequence number (`Query.num`) and a
                // right-click handler that surfaces the
                // block-scoped Copy menu (Copy command / Copy
                // output / Copy whole block). Right-clicks landing
                // BETWEEN blocks (orphan output, the gutter)
                // bubble up to the outer `.shell` handler and see
                // only the shell-level menu items.
                {let pid_blocks = pane_id.clone(); move || {
                    // Build a snapshot of the prompt template +
                    // context once per re-render of the history.
                    // Each Query entry's prompt is computed lazily
                    // via the closure (which overrides `query_num`).
                    let pt = t_prompt1.get();
                    let vars_snap = t_variables.get();
                    let txn  = t_txn_state.get();
                    let user = vars_snap.get("USER").cloned().unwrap_or_default();
                    let db   = vars_snap.get("DBNAME").cloned().unwrap_or_default();
                    let host = vars_snap.get("HOST").cloned().unwrap_or_default();
                    let port = vars_snap.get("PORT").cloned().unwrap_or_default();
                    let pid_v = vars_snap.get("PID").cloned().unwrap_or_default();
                    let prompt_renderer = move |num: u32| -> String {
                        let ctx = crate::psql_prompt::PromptContext {
                            user:         &user,
                            dbname:       &db,
                            host:         &host,
                            port:         &port,
                            pid:          &pid_v,
                            txn_state:    txn,
                            is_superuser: false,
                            query_num:    num,
                            variables:    &vars_snap,
                            connected:    true,
                            inactive_branch: false,
                        };
                        crate::psql_prompt::format_prompt(&pt, &ctx)
                    };
                    let entries: Vec<ShellEntry> = t_history.get();
                    // Slice into blocks. Each block is (Some(num), Vec<entries>).
                    // None covers any pre-Query entries that may exist
                    // (defensive; in practice the history starts with
                    // a Query after the user's first command).
                    let mut blocks: Vec<(Option<u32>, Vec<ShellEntry>)> = Vec::new();
                    let mut current: Option<(Option<u32>, Vec<ShellEntry>)> = None;
                    for entry in entries {
                        match &entry {
                            ShellEntry::Query { num, .. } => {
                                if let Some(c) = current.take() { blocks.push(c); }
                                current = Some((Some(*num), vec![entry]));
                            }
                            _ => {
                                match current.as_mut() {
                                    Some((_, v)) => v.push(entry),
                                    None => current = Some((None, vec![entry])),
                                }
                            }
                        }
                    }
                    if let Some(c) = current { blocks.push(c); }

                    blocks.into_iter().map(|(num_opt, entries)| {
                        // For each entry: pass non-tabular through
                        // unchanged; wrap Table/Vertical entries with
                        // a per-block collapse toggle + scrollable
                        // region so a 500-row result doesn't dominate
                        // the scrollback. The collapsed/expanded state
                        // lives on `t_collapsed` keyed by block_num
                        // (Query.num), so re-renders and clear-buffer
                        // don't lose user choices unexpectedly.
                        let block_num = num_opt;
                        let inner = entries.into_iter()
                            .map(|e| {
                                let is_table_like = matches!(
                                    &e,
                                    ShellEntry::Table { .. } | ShellEntry::Vertical { .. } | ShellEntry::Printed { .. },
                                );
                                let rendered = render_entry(e, t_plane, &prompt_renderer, open_here).into_view();
                                match (is_table_like, block_num) {
                                    // Wrap Table/Vertical output in a
                                    // region that the per-block
                                    // collapsed-state signal can shrink
                                    // into a 400px scroll area. The
                                    // toggle itself lives in the
                                    // block's right-click context menu
                                    // (a fixed-position button at the
                                    // top of the output would be
                                    // useless once the user has
                                    // scrolled past it — and scrolling
                                    // back up to collapse defeats the
                                    // entire "less scrollback" point).
                                    (true, Some(num)) => view! {
                                        <div class="shell-collapsed-region"
                                             class:shell-collapsed-region-clipped=move ||
                                                 t_collapsed.with(|s| s.contains(&num))>
                                            {rendered}
                                        </div>
                                    }.into_view(),
                                    _ => rendered,
                                }
                            })
                            .collect_view();
                        match num_opt {
                            Some(num) => {
                                let pid_for_block = pid_blocks.clone();
                                view! {
                                    <div class="shell-block"
                                         on:contextmenu=move |ev: web_sys::MouseEvent| {
                                             ev.prevent_default();
                                             ev.stop_propagation();
                                             shell_input_menu.set(Some(ShellInputContextMenu {
                                                 pane_id:   pid_for_block.clone(),
                                                 x:         ev.client_x() as f64,
                                                 y:         ev.client_y() as f64,
                                                 block_num: Some(num),
                                             }));
                                         }>
                                        {inner}
                                    </div>
                                }.into_view()
                            }
                            None => view! { <>{inner}</> }.into_view(),
                        }
                    }).collect_view()
                }}

                // While `\watch` runs (its runs print above): how often,
                // and how to stop it — psql's Ctrl+C, or Stop.
                {move || {
                    let Some(interval_ms) = t_watch_int.get().filter(|_| t_watch_sql.get().is_some()) else {
                        return view! { <span/> }.into_view();
                    };
                    view! {
                        <div class="shell-watch-bar">
                            <span class="shell-watch-label">
                                {format!("⟳ \\watch every {}s — Ctrl+C to stop", f64::from(interval_ms) / 1000.0)}
                            </span>
                            <button class="shell-watch-stop" on:click=move |_| interrupt_watch()>"Stop"</button>
                        </div>
                    }.into_view()
                }}

                // Ctrl+R search bar (visible only during reverse search)
                {move || {
                    if !t_search_on.get() { return view! { <span/> }.into_view(); }
                    let term = t_search_term.get();
                    let pos = t_search_pos.get();
                    let matched = search_match(&term, pos).unwrap_or_default();
                    view! {
                        <div class="shell-search-bar">
                            <span class="shell-search-label">"(reverse-i-search)`"</span>
                            <span class="shell-search-term">{term}</span>
                            <span class="shell-search-label">"': "</span>
                            <span class="shell-search-match">{matched}</span>
                        </div>
                    }.into_view()
                }}

                // Safe-mode confirmation dialog. Renders one of two shapes
                // depending on whether the gated SQL is a single statement
                // or a batch:
                //
                //   Single → header (with the reason: DROP / TRUNCATE /
                //            DELETE-without-WHERE / parse-failed), one
                //            preview pre, Execute + Cancel.
                //   Batch  → header listing "N destructive statements in
                //            an M-statement batch", one preview pre per
                //            destructive statement (capped at 5; "+K
                //            more" footer), Run-batch + Cancel.
                //
                // Pre-fix this only handled the Single case because the
                // multi-statement path skipped safe-mode entirely. Batch
                // confirm is the meat of audit finding 1.1.
                {move || {
                    let pending = t_pending_cfm.get();
                    let Some(pending) = pending else { return view! { <span/> }.into_view(); };
                    match pending {
                        crate::types::PendingConfirm::Single { stmt, watch, reason } => {
                            let preview = sql_preview(&stmt.as_typed());
                            let header_text = single_header(&reason);
                            view! {
                                <div class="shell-confirm-block">
                                    <div class="shell-confirm-header">
                                        <span class="shell-confirm-warn">{header_text}</span>
                                    </div>
                                    <pre class="shell-confirm-sql">{preview}</pre>
                                    <div class="shell-confirm-actions">
                                        <button class="shell-confirm-execute" on:click=move |_| {
                                            t_pending_cfm.set(None);
                                            // Same path as Enter — `\G` intent included.
                                            tab_sv.with_value(|t| match watch {
                                                Some(bounds) => start_watch(t, stmt.clone(), bounds),
                                                None => run_in_shell(t, ws, stmt.clone()),
                                            });
                                        }>"Execute"</button>
                                        <button class="shell-confirm-cancel" on:click={
                                            let pc = pid_confirm.clone();
                                            move |_| {
                                                // What's left to read isn't run either: it
                                                // waits in the input. (Taken before the
                                                // dialog goes, or it would read on.)
                                                let rest = t_resume.try_update(Option::take).flatten();
                                                t_pending_cfm.set(None);
                                                t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                                                    "Cancelled.".into()
                                                ])));
                                                if let Some(rest) = rest {
                                                    t_input.update(|input| *input = format!("{rest}{input}"));
                                                    schedule_textarea_resize(&pc);
                                                }
                                                scroll_shell_to_bottom(&pc);
                                            }
                                        }>"Cancel"</button>
                                    </div>
                                </div>
                            }.into_view()
                        }
                        crate::types::PendingConfirm::Batch { batch, destructive_indexes } => {
                            // Cap at 5 destructive previews; "+K more"
                            // footer for the rest so the dialog doesn't
                            // grow unbounded.
                            const PREVIEW_CAP: usize = 5;
                            let dc = destructive_indexes.len();
                            let total = batch.run.len();
                            let previews: Vec<String> = destructive_indexes.iter()
                                .take(PREVIEW_CAP)
                                .map(|&i| sql_preview(&batch.run[i].as_typed()))
                                .collect();
                            let overflow = dc.saturating_sub(PREVIEW_CAP);
                            let header_text = format!(
                                "⚠ safe-mode: {} destructive statement{} in a {}-statement batch",
                                dc, if dc == 1 { "" } else { "s" }, total,
                            );
                            let batch_for_run = batch.clone();
                            view! {
                                <div class="shell-confirm-block">
                                    <div class="shell-confirm-header">
                                        <span class="shell-confirm-warn">{header_text}</span>
                                    </div>
                                    {previews.into_iter().map(|p| view! {
                                        <pre class="shell-confirm-sql">{p}</pre>
                                    }).collect_view()}
                                    {(overflow > 0).then(|| view! {
                                        <div class="shell-confirm-overflow">
                                            {format!("… and {} more destructive statement{}", overflow, if overflow == 1 { "" } else { "s" })}
                                        </div>
                                    })}
                                    <div class="shell-confirm-actions">
                                        <button class="shell-confirm-execute" on:click=move |_| {
                                            t_pending_cfm.set(None);
                                            run_multi.call(batch_for_run.clone());
                                        }>{format!("Run batch ({} statements)", total)}</button>
                                        <button class="shell-confirm-cancel" on:click={
                                            let pc = pid_confirm.clone();
                                            move |_| {
                                                // What's left to read isn't run either: it
                                                // waits in the input. (Taken before the
                                                // dialog goes, or it would read on.)
                                                let rest = t_resume.try_update(Option::take).flatten();
                                                t_pending_cfm.set(None);
                                                t_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                                                    "Cancelled.".into()
                                                ])));
                                                if let Some(rest) = rest {
                                                    t_input.update(|input| *input = format!("{rest}{input}"));
                                                    schedule_textarea_resize(&pc);
                                                }
                                                scroll_shell_to_bottom(&pc);
                                            }
                                        }>"Cancel"</button>
                                    </div>
                                </div>
                            }.into_view()
                        }
                    }
                }}

                {move || {
                    if !t_querying.get() { return view! { <span/> }.into_view(); }
                    view! {
                        <div class="shell-entry-pending">
                            <span class="shell-pending-glyph">"⚡"</span>
                            <span class="shell-pending-text">"Executing… (awaiting response)"</span>
                        </div>
                    }.into_view()
                }}

                // Without a session there's nowhere to type to — unless the
                // plane's CLI carries on after a lost one (mysql reconnects
                // on the next statement; psql, its reset failed, answers
                // "not connected").
                <div class="shell-entry-query"
                    style=move || {
                        let carries_on = t_session_stale.get() && t_plane.lost_connection() != LostConnection::Banner;
                        if (t_session_id.get().is_none() && !carries_on) || t_querying.get() { "display:none" } else { "" }
                    }
                >
                    <span class="shell-prompt">
                        {move || {
                            let num = match t_hist_pos.get() {
                                None => t_query_count.get(),
                                Some(pos) => {
                                    let nums: Vec<u32> = t_history.get()
                                        .into_iter()
                                        .filter_map(|e| if let ShellEntry::Query { num, .. } = e { Some(num) } else { None })
                                        .collect();
                                    let idx = nums.len().saturating_sub(1 + pos);
                                    nums.get(idx).copied().unwrap_or_else(|| t_query_count.get())
                                }
                            };
                            // Format the live prompt against the
                            // current `PROMPT1` template + session
                            // context. The historical-walk case
                            // reuses the same template but with the
                            // walked-to query number so the prompt
                            // reads "<historic N> SQL>" rather than
                            // the current count.
                            let pt = t_prompt1.get();
                            let vars_snap = t_variables.get();
                            let txn = t_txn_state.get();
                            let user = vars_snap.get("USER").cloned().unwrap_or_default();
                            let db   = vars_snap.get("DBNAME").cloned().unwrap_or_default();
                            let host = vars_snap.get("HOST").cloned().unwrap_or_default();
                            let port = vars_snap.get("PORT").cloned().unwrap_or_default();
                            let pid_v = vars_snap.get("PID").cloned().unwrap_or_default();
                            // The live prompt says when there's no connection
                            // (psql's `!` / `?` after a failed reset).
                            let ctx = crate::psql_prompt::PromptContext {
                                user: &user, dbname: &db, host: &host, port: &port, pid: &pid_v,
                                txn_state: txn, is_superuser: false,
                                query_num: num, variables: &vars_snap, connected: !t_disconnected.get(),
                                inactive_branch: !t_conditionals.with(|stack| crate::psql_if::active(stack)),
                            };
                            crate::psql_prompt::format_prompt(&pt, &ctx)
                        }}
                    </span>
                    <span class="shell-input-wrap">
                        <div class="shell-input-highlight" aria-hidden="true">
                            {move || {
                                let sql = t_input.get();
                                // In \password mode the textarea text is
                                // invisible (transparent) and the overlay
                                // paints what the user sees. Render `•`
                                // per character so the on-screen result
                                // matches a native password field. The
                                // real chars stay in `t_input` (cleared
                                // on Esc / consume).
                                if t_password_prompt.get().is_some() {
                                    let masked = "\u{2022}".repeat(sql.chars().count());
                                    return vec![crate::highlight::Token {
                                        text: masked,
                                        kind: crate::highlight::TokenKind::Plain,
                                    }].into_iter().map(render_sql_token).collect_view();
                                }
                                let mut tokens = crate::highlight::tokenize_sql(&sql, t_plane);
                                // A trailing '\n' collapses in `white-space: pre-wrap`; the
                                // textarea reserves a line for it. Pad with U+200B so the
                                // overlay reserves the same line and heights stay aligned.
                                if sql.ends_with('\n') {
                                    tokens.push(crate::highlight::Token {
                                        text: "\u{200B}".to_string(),
                                        kind: crate::highlight::TokenKind::Plain,
                                    });
                                }
                                tokens.into_iter().map(render_sql_token).collect_view()
                            }}
                        </div>
                        <textarea
                            id=input_id
                            class="shell-input-inline"
                            prop:value=move || t_input.get()
                            on:input={
                                let pid_input = pane_id.clone();
                                move |e| {
                                    use wasm_bindgen::JsCast;
                                    let value = event_target_value(&e);
                                    // What was noted is the input as it was
                                    // left: an edit that leaves it not
                                    // starting with that — cleared, retyped —
                                    // is read afresh when it's entered.
                                    if !t_noted.with_untracked(|noted| value.starts_with(noted.as_str())) {
                                        t_noted.set(String::new());
                                    }
                                    t_input.set(value);
                                    if let Some(el) = e.target()
                                        .and_then(|t| t.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                    {
                                        let _ = el.style().set_property("height", "auto");
                                        let _ = el.style().set_property(
                                            "height", &format!("{}px", el.scroll_height()));
                                    }

                                    // ── Star-table prefetch (every input change) ──────────
                                    // Stars inside inline views or CTEs reference real
                                    // tables that need their columns fetched before the
                                    // star can expand. Run extract_aliases on every
                                    // keystroke and prefetch anything its `stars_pending`
                                    // surfaces. Tokenization is ~O(input length); cheap at
                                    // typical query lengths. Without this, the user has to
                                    // Tab once (fires fetch), then Tab again (uses cache).
                                    {
                                        let new_input_for_stars = t_input.get_untracked();
                                        let columns_loaded_for_stars: std::collections::HashMap<(String, String), Vec<String>> =
                                            tab.completion_columns.get_untracked()
                                                .into_iter()
                                                .filter_map(|(k, v)| match v {
                                                    crate::types::LoadState::Loaded(cols) => Some((k, cols)),
                                                    _ => None,
                                                })
                                                .collect();
                                        let default_schema_for_stars = t_plane
                                            .default_completion_schema(&tab.current_db.get_untracked());
                                        let extraction = crate::completion::extract_aliases(
                                            &new_input_for_stars,
                                            &default_schema_for_stars,
                                            &columns_loaded_for_stars,
                                        );
                                        for (sch, tbl) in &extraction.stars_pending {
                                            let pkey = (sch.clone(), tbl.clone());
                                            let already = matches!(
                                                tab.completion_columns.with_untracked(|m| m.get(&pkey).cloned()),
                                                Some(crate::types::LoadState::Loaded(_)) |
                                                Some(crate::types::LoadState::Loading)
                                            );
                                            if !already {
                                                tab.completion_columns.update(|m| {
                                                    m.insert(pkey.clone(), crate::types::LoadState::Loading);
                                                });
                                                if let Some(sid) = tab.session_id.get_untracked() {
                                                    crate::dom::ws_introspect(
                                                        ws, &sid,
                                                        t_plane.completion_columns_sql(sch, tbl),
                                                        format!("completion_columns:{}|{}", sch, tbl),
                                                    );
                                                }
                                            }
                                        }
                                    }

                                    // ── Eager column-cache prefetch on `.` keystroke ──────
                                    // When the just-typed char is `.` AND the identifier
                                    // immediately before it is a known table name in the
                                    // default completion schema, fire an introspect for
                                    // that table's columns so the next Tab lands a
                                    // populated popup. The dot itself is the user's signal
                                    // they're about to qualify; we don't wait for Tab.
                                    {
                                        let new_input = t_input.get_untracked();
                                        let cursor: usize = web_sys::window()
                                            .and_then(|w| w.document())
                                            .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid_input)))
                                            .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                            .and_then(|ta| ta.selection_start().ok().flatten())
                                            .map(|n| n as usize)
                                            .unwrap_or(0);
                                        let bytes = new_input.as_bytes();
                                        if cursor > 0 && cursor <= bytes.len() && bytes[cursor - 1] == b'.' {
                                            // Walk back over identifier chars to find the qualifier.
                                            let mut start = cursor - 1;
                                            while start > 0 {
                                                let c = bytes[start - 1];
                                                if c.is_ascii_alphanumeric() || c == b'_' {
                                                    start -= 1;
                                                } else {
                                                    break;
                                                }
                                            }
                                            if start < cursor - 1 {
                                                let qualifier = new_input[start..cursor - 1].to_string();
                                                if let crate::types::LoadState::Loaded(tables_map) =
                                                    tab.completion_tables.get_untracked()
                                                {
                                                    // Skip if the qualifier is a schema — that path
                                                    // lists tables, no column fetch needed.
                                                    let is_schema = tables_map.keys()
                                                        .any(|k| k.eq_ignore_ascii_case(&qualifier));
                                                    if !is_schema {
                                                        let default_schema = t_plane
                                                            .default_completion_schema(&tab.current_db.get_untracked());
                                                        // Phase 1: bare-table qualifier
                                                        let phase1 = tables_map
                                                            .get(&default_schema)
                                                            .and_then(|ts| ts.iter()
                                                                .find(|t| t.eq_ignore_ascii_case(&qualifier))
                                                                .cloned())
                                                            .map(|t| (default_schema.clone(), t));
                                                        // Phase 2 / 3: alias resolution. Only Table
                                                        // resolutions need a column fetch; Projection
                                                        // aliases (CTEs / inline views) already carry
                                                        // their column list. Star-expansion pending
                                                        // tables get prefetched in the same pass.
                                                        let columns_loaded_dot: std::collections::HashMap<(String, String), Vec<String>> =
                                                            tab.completion_columns.get_untracked()
                                                                .into_iter()
                                                                .filter_map(|(k, v)| match v {
                                                                    crate::types::LoadState::Loaded(cols) => Some((k, cols)),
                                                                    _ => None,
                                                                })
                                                                .collect();
                                                        let extraction = crate::completion::extract_aliases(
                                                            &new_input,
                                                            &default_schema,
                                                            &columns_loaded_dot,
                                                        );
                                                        for (sch, tbl) in &extraction.stars_pending {
                                                            let pkey = (sch.clone(), tbl.clone());
                                                            let already = matches!(
                                                                tab.completion_columns.with_untracked(|m| m.get(&pkey).cloned()),
                                                                Some(crate::types::LoadState::Loaded(_)) |
                                                                Some(crate::types::LoadState::Loading)
                                                            );
                                                            if !already {
                                                                tab.completion_columns.update(|m| {
                                                                    m.insert(pkey.clone(), crate::types::LoadState::Loading);
                                                                });
                                                                if let Some(sid) = tab.session_id.get_untracked() {
                                                                    crate::dom::ws_introspect(
                                                                        ws, &sid,
                                                                        t_plane.completion_columns_sql(sch, tbl),
                                                                        format!("completion_columns:{}|{}", sch, tbl),
                                                                    );
                                                                }
                                                            }
                                                        }
                                                        let phase2_or_3 = if phase1.is_none() {
                                                            extraction.aliases.iter()
                                                                .find(|(k, _)| k.eq_ignore_ascii_case(&qualifier))
                                                                .and_then(|(_, res)| match res {
                                                                    crate::completion::AliasResolution::Table { schema: s, table: t } =>
                                                                        Some((s.clone(), t.clone())),
                                                                    crate::completion::AliasResolution::Projection { .. } => None,
                                                                })
                                                        } else { None };
                                                        let resolved = phase1.or(phase2_or_3);
                                                        if let Some((sch, tbl)) = resolved {
                                                            let key = (sch.clone(), tbl.clone());
                                                            let already = matches!(
                                                                tab.completion_columns.with_untracked(|m| m.get(&key).cloned()),
                                                                Some(crate::types::LoadState::Loaded(_)) |
                                                                Some(crate::types::LoadState::Loading)
                                                            );
                                                            if !already {
                                                                tab.completion_columns.update(|m| {
                                                                    m.insert(key.clone(), crate::types::LoadState::Loading);
                                                                });
                                                                if let Some(sid) = tab.session_id.get_untracked() {
                                                                    crate::dom::ws_introspect(
                                                                        ws, &sid,
                                                                        t_plane.completion_columns_sql(&sch, &tbl),
                                                                        format!("completion_columns:{}|{}", sch, tbl),
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // ── Re-filter the completion popup as the user types ──
                                    // When the popup is open for this pane, every keystroke
                                    // re-analyses context + re-gathers candidates against the
                                    // new prefix. Close when:
                                    //  - analyzer returns None (cursor walked out of any
                                    //    completable token; e.g. user typed a space),
                                    //  - replace_start shifted (we're in a different token now),
                                    //  - candidates are empty after the filter.
                                    let popup_open = completion_popup.get_untracked()
                                        .as_ref()
                                        .map(|p| p.pane_id == pid_input)
                                        .unwrap_or(false);
                                    if !popup_open { return; }
                                    let new_input = t_input.get_untracked();
                                    let cursor: usize = web_sys::window()
                                        .and_then(|w| w.document())
                                        .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid_input)))
                                        .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                        .and_then(|ta| ta.selection_start().ok().flatten())
                                        .map(|n| n as usize)
                                        .unwrap_or_else(|| new_input.len());
                                    let hash = t_plane.lexicon().hash_comments();
                                    let Some(analysis) = crate::completion::analyze_context(&new_input, cursor, hash) else {
                                        completion_popup.set(None);
                                        return;
                                    };
                                    let current_open_start = completion_popup.get_untracked()
                                        .as_ref().map(|p| p.replace_start).unwrap_or(usize::MAX);
                                    if analysis.replace_start != current_open_start {
                                        completion_popup.set(None);
                                        return;
                                    }
                                    let cache = match tab.completion_tables.get_untracked() {
                                        crate::types::LoadState::Loaded(m) => Some(m),
                                        _ => None,
                                    };
                                    let columns_loaded: std::collections::HashMap<(String, String), Vec<String>> =
                                        tab.completion_columns.get_untracked()
                                            .into_iter()
                                            .filter_map(|(k, v)| match v {
                                                crate::types::LoadState::Loaded(cols) => Some((k, cols)),
                                                _ => None,
                                            })
                                            .collect();
                                    let current_db = tab.current_db.get_untracked();
                                    let extraction = crate::completion::extract_aliases(
                                        &new_input,
                                        &t_plane.default_completion_schema(&current_db),
                                        &columns_loaded,
                                    );
                                    let cands = crate::completion::gather_candidates(
                                        &analysis.context, t_plane,
                                        cache.as_ref(),
                                        &columns_loaded,
                                        &extraction.aliases,
                                        &current_db,
                                    );
                                    if cands.is_empty() {
                                        completion_popup.set(None);
                                        return;
                                    }
                                    completion_popup.update(|opt| {
                                        if let Some(p) = opt {
                                            p.items    = cands;
                                            p.selected = 0;
                                        }
                                    });
                                }
                            }
                            on:keydown=on_keydown
                            on:dblclick={
                                // Highlight the matched bracket span by extending
                                // the native textarea selection from the
                                // double-clicked `(` / `[` / `{` to its mate (or
                                // backwards from a `)` / `]` / `}`). Uses the
                                // textarea's native selection — no extra paint
                                // layer.
                                move |_ev: web_sys::MouseEvent| {
                                    use wasm_bindgen::JsCast;
                                    let Some(ta) = web_sys::window()
                                        .and_then(|w| w.document())
                                        .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid_format)))
                                        .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                    else { return };
                                    let value = ta.value();
                                    let pos = ta.selection_start().ok().flatten().unwrap_or(0) as usize;
                                    // The native dblclick selects the surrounding
                                    // "word"; for non-word chars like brackets,
                                    // the caret lands at one edge. Try the byte
                                    // at `pos` first, then the byte just to its
                                    // left — covers both edges of the click.
                                    let candidates = [pos, pos.saturating_sub(1)];
                                    for c in candidates {
                                        if let Some((open, close)) = crate::highlight::find_bracket_match(&value, c) {
                                            let _ = ta.set_selection_range(open as u32, (close + 1) as u32);
                                            return;
                                        }
                                        if c == 0 { break; }
                                    }
                                }
                            }
                            rows="1"
                            prop:readOnly=move || t_external_edit.with(Option::is_some)
                        />
                    </span>
                </div>
                {move || t_external_edit.with(Option::is_some).then(|| view! {
                    <div class="shell-editing-note">
                        "Editing in your external editor — save and quit to bring the text back here. Esc stops waiting."
                    </div>
                })}
            </div>
        </div>
    }.into_view()
}

// ── Entry renderer ────────────────────────────────────────────────────────────

/// One history entry as the shell shows it. A result shows as it
/// printed (`PrintedWith`), whatever `\pset` says now.
pub(crate) fn render_entry(
    entry:           ShellEntry,
    plane:           &'static dyn DbmsPlane,
    prompt_renderer: &dyn Fn(u32) -> String,
    open_explorer:   Callback<ExplorerInit>,
) -> impl IntoView {
    // The title over a result (`\pset title`, a list's) — none under
    // `\t`, as psql's printer leaves it out there.
    // The title over a table, as psql's aligned printer prints it
    // (`print_aligned_text`): centred over the table — its first line
    // padded to where its widest line would centre (`"%-*s%s"` pads the
    // first alone), the rest from the left edge — and from the left edge
    // throughout when it's as wide as the table. Every line, unseen,
    // sizes the box the first line is centred in — drawn by CSS from its
    // `data-line`, so it's no part of the title's text, copy or
    // selection. An empty line, the empty title too, is a blank line.
    let title_line = |style: &PrintedWith| -> Option<View> {
        let lines: Vec<String> = style.title()?.split('\n')
            .map(|line| if line.is_empty() { "\u{a0}".to_string() } else { line.to_string() })
            .collect();
        let (first, rest) = lines.split_first()?;
        let rest = rest.to_vec();
        let first = first.clone();
        Some(view! {
            <div class="shell-table-title">
                <div class="shell-table-title-first">
                    <span>{first}</span>
                    {lines.into_iter().map(|line| view! { <span class="shell-table-title-width" data-line=line/> }).collect_view()}
                </div>
                {rest.into_iter().map(|line| view! { <div>{line}</div> }).collect_view()}
            </div>
        }.into_view())
    };
    // The line under a result's rows as the plane's CLI words it, with
    // the time where the CLI puts it there…
    let stat = move |n: usize, warnings: u16, us: u64, vertical: bool, timed: bool| -> Option<String> {
        footer_and_time(plane, n, warnings, vertical, us, timed).0
    };
    // …or on a line of its own (psql's "Time: 0.205 ms").
    let time_line = move |us: u64, timed: bool| -> View {
        match footer_and_time(plane, 0, 0, false, us, timed).1 {
            Some(time) => view! { <div class="shell-timing">{time}</div> }.into_view(),
            None => ().into_view(),
        }
    };
    match entry {
        ShellEntry::Query { num, sql, terminator, errored, .. } => {
            let prompt = prompt_renderer(num);
            let tokens = crate::highlight::tokenize_sql(&format!("{sql}{terminator}"), plane);
            view! {
                <div class="shell-entry-query">
                    <span class="shell-prompt" class:shell-prompt-error=errored>{prompt}</span>
                    <span class="shell-sql">
                        {tokens.into_iter().map(render_sql_token).collect_view()}
                    </span>
                </div>
            }.into_view()
        }

        ShellEntry::Error(msg) => view! {
            <div class="shell-entry-error">{format!("ERROR: {}", msg)}</div>
        }.into_view(),

        ShellEntry::CliError { lines, .. } => view! {
            <div class="shell-entry-error">{lines.join("\n")}</div>
        }.into_view(),

        ShellEntry::Notice { severity, message } => view! {
            <div class="shell-notice" class:shell-notice-warning=severity == "WARNING">
                {format!("{severity}:  {message}")}
            </div>
        }.into_view(),

        ShellEntry::Printed { mut lines, links } => {
            // psql ends each block with a blank line; the scrollback
            // spaces blocks itself (Copy keeps it).
            while lines.last().is_some_and(|l| l.is_empty()) { lines.pop(); }
            let last = lines.len().saturating_sub(1);
            view! {
                <pre class="shell-printed">
                    {lines.into_iter().enumerate().map(|(i, line)| {
                        let mut spans: Vec<&PrintedLink> = links.iter().filter(|l| l.line == i).collect();
                        spans.sort_by_key(|l| l.start);
                        let mut parts = Vec::new();
                        let mut at = 0;
                        for link in spans {
                            let (Some(before), Some(label)) = (line.get(at..link.start), line.get(link.start..link.end)) else { continue };
                            parts.push(before.to_string().into_view());
                            let open = link.open.clone();
                            parts.push(view! {
                                <span class="shell-printed-link"
                                      title=format!("Open {}.{} in the Schema Explorer", open.schema, open.name)
                                      on:click=move |_| open_explorer.call(open.clone())>
                                    {label.to_string()}
                                </span>
                            }.into_view());
                            at = link.end;
                        }
                        parts.push(line.get(at..).unwrap_or_default().to_string().into_view());
                        if i < last { parts.push("\n".into_view()); }
                        parts.collect_view()
                    }).collect_view()}
                </pre>
            }.into_view()
        }

        ShellEntry::MetaOutput(lines) => view! {
            <div class="shell-meta-output">
                {lines.into_iter().map(|line| view! {
                    <div class="shell-meta-line">{line}</div>
                }).collect_view()}
            </div>
        }.into_view(),

        ShellEntry::KeyValue(pairs) => {
            view! {
                <div class="shell-kv-block">
                    {pairs.into_iter().map(|(key, val)| {
                        view! {
                            <span class="shell-kv-key">{format!("{}:", key)}</span>
                            <span class="shell-kv-val">{val}</span>
                        }
                    }).collect_view()}
                </div>
            }.into_view()
        }

        ShellEntry::Info { exec_time_us, said, timed } => {
            let lines = command_lines(plane, &said, exec_time_us, timed);
            (!lines.is_empty()).then(|| view! { <div class="shell-entry-info">{lines.join("\n")}</div> }).into_view()
        }

        ShellEntry::Table { columns, aligns, rows, exec_time_us, warnings, timed, style, .. } => {
            let n = rows.len();

            let dl_cols = columns.clone();
            let dl_rows = rows.clone();

            // As it printed (`style`): its title over it (none under
            // `\t`), its header (none under `\t`), NULLs as the null
            // display, its footer (none under `\t` or `\pset footer off`)
            // — or a list's own footers in its place, straight under the
            // rows as psql's aligned printer puts them.
            let show_footer = style.shows_footer();
            let footers = style.footers().join("\n");
            let columnless = columns.is_empty();
            let title_view = title_line(&style);
            view! {
                <div>
                    // No columns: an empty result, said in the stat
                    // line below ("Empty set" in mysql). An empty
                    // result with columns keeps its header, as psql
                    // prints it.
                    {if columnless {
                        view! { <span/> }.into_view()
                    } else {
                        let hide_thead = style.tuples_only;
                        // Each column lines up as the CLI lines it up:
                        // numbers right, headers centred in psql.
                        let right = move |col: usize| aligns.get(col) == Some(&Align::Right);
                        view! {
                            <div class="shell-table-wrap">
                                <div class="shell-table-frame">
                                    {title_view}
                                    <table class="shell-table" class:shell-table-centred=plane.centres_headers()>
                                        {(!hide_thead).then(|| view! {
                                            <thead><tr>
                                                {columns.iter().map(|c| {
                                                    let c = c.clone();
                                                    view! { <th>{c}</th> }
                                                }).collect_view()}
                                            </tr></thead>
                                        })}
                                        <tbody>
                                            {rows.into_iter().map(|row| view! {
                                                <tr>
                                                    {row.into_iter().enumerate().map(|(col, v)| {
                                                        let null = v.is_none();
                                                        let display = style.cell(plane, &v, if right(col) { Align::Right } else { Align::Left });
                                                        view! { <td class:shell-null=null class:shell-num=right(col)>{display}</td> }
                                                    }).collect_view()}
                                                </tr>
                                            }).collect_view()}
                                        </tbody>
                                    </table>
                                </div>
                            </div>
                        }.into_view()
                    }}
                    {(!footers.is_empty()).then(|| view! { <div class="shell-table-footers">{footers}</div> })}
                    // Split the stat strip so `\pset footer off`
                    // (or `\pset tuples_only on`) hides only the
                    // "N rows" line. The CSV / JSON / SQL save
                    // links are a browser affordance with no psql
                    // equivalent — they stay visible regardless.
                    <div class="shell-stat">
                        {show_footer.then(|| stat(n, warnings, exec_time_us, false, timed)).flatten()}
                        {if n > 0 { render_save_links(dl_cols, dl_rows).into_view() } else { view! { <span/> }.into_view() }}
                    </div>
                    {time_line(exec_time_us, timed)}
                </div>
            }.into_view()
        }

        // As the CLI prints its vertical output (`DbmsPlane::print_vertical`:
        // psql's `-[ RECORD 1 ]` records under the title, the mysql CLI's
        // `*** 1. row ***`), NULLs as `\pset null` says, then the footer —
        // which `\t` and `\pset footer off` leave out — and the time.
        ShellEntry::Vertical { columns, aligns, rows, exec_time_us, warnings, timed, style } => {
            let n = rows.len();
            let shown: Vec<Vec<String>> = rows.iter().map(|row| style.row(plane, row, &aligns)).collect();
            let lines = plane.print_vertical(&columns, &shown, &style);
            // A value ending in a line break leaves an empty last line,
            // which a `<pre>` shows only with a break after it.
            let mut text = lines.join("\n");
            if lines.last().is_some_and(String::is_empty) {
                text.push('\n');
            }
            let show_footer = style.shows_footer();
            view! {
                <div class="shell-vertical-block">
                    {(!lines.is_empty()).then(|| view! { <pre class="shell-vertical-text">{text}</pre> })}
                    <div class="shell-stat">
                        {show_footer.then(|| stat(n, warnings, exec_time_us, true, timed)).flatten()}
                        {render_save_links(columns, rows)}
                    </div>
                    {time_line(exec_time_us, timed)}
                </div>
            }.into_view()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(seed: &str, discard_if_unchanged: bool) -> ExternalEdit {
        ExternalEdit { nonce: 1, restore: String::new(), seed: seed.into(), discard_if_unchanged, then: String::new() }
    }

    #[test]
    fn a_discarded_definition_gives_back_what_was_typed_meanwhile() {
        let e = ExternalEdit { restore: "select 9".into(), ..edit("create function f", true) };
        assert_eq!(edited_input(&e, "create function f\n"), "select 9");
    }

    #[test]
    fn what_followed_the_command_follows_the_edited_text() {
        // The mysql CLI's `select 2 as b \e from dual`: the rest of the
        // line goes after whatever comes back, as it appends it.
        let e = ExternalEdit { then: " from dual".into(), ..edit("select 2 as b", false) };
        assert_eq!(e.followed(&edited_input(&e, "select 2 as b\n")), "select 2 as b from dual");
        assert_eq!(e.followed(""), " from dual");
    }

    #[test]
    fn an_edit_returns_its_text_without_the_trailing_newline() {
        assert_eq!(edited_input(&edit("select 1", false), "select 2;\n"), "select 2;");
        assert_eq!(edited_input(&edit("select 1", false), "select 1\n"), "select 1");
    }

    #[test]
    fn an_unchanged_previous_query_or_definition_leaves_the_buffer_empty() {
        assert_eq!(edited_input(&edit("select 1", true), "select 1\n"), "");
        assert_eq!(edited_input(&edit("select 1", true), "select 2\n"), "select 2");
    }

    #[test]
    fn line_at_counts_utf16_offsets() {
        let text = "select 'é'\nfrom t\n\\e";
        assert_eq!(line_at(text, 0), 1);
        assert_eq!(line_at(text, 10), 1);
        assert_eq!(line_at(text, 11), 2);
        assert_eq!(line_at(text, 17), 2);
        assert_eq!(line_at(text, 18), 3);
        // A 2-unit character (😀) before the newline.
        assert_eq!(line_at("😀\nx", 3), 2);
    }

    #[test]
    fn edit_lookup_reads_columns_by_name_and_null_as_absent() {
        let cols = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let row = |r: &[&str]| vec![r.iter().map(|s| s.to_string()).collect::<Vec<_>>()];
        let c = cols(&["Src", "Line", "Error"]);
        assert_eq!(parse_edit_lookup(&c, &row(&["create …", "5", ""])),
                   EditLookup::Found { src: "create …".into(), line: Some(5) });
        assert_eq!(parse_edit_lookup(&c, &row(&["create …", "NULL", "NULL"])),
                   EditLookup::Found { src: "create …".into(), line: None });
        assert_eq!(parse_edit_lookup(&c, &row(&["", "NULL", "\"public.mv\" is not a view"])),
                   EditLookup::Refused("\"public.mv\" is not a view".into()));
        assert_eq!(parse_edit_lookup(&cols(&["line", "src", "error"]), &row(&["2", "x", ""])),
                   EditLookup::Found { src: "x".into(), line: Some(2) });
        assert_eq!(parse_edit_lookup(&c, &[]), EditLookup::NotFound);
        assert_eq!(parse_edit_lookup(&c, &row(&["NULL", "NULL", ""])), EditLookup::NotFound);
    }

    #[test]
    fn a_command_splits_off_where_it_starts() {
        let pg = Lexicon::Psql;
        assert_eq!(split_buffer("SELECT 1\n\\gset   p_ \n", pg), ("SELECT 1", "\n\\gset   p_"));
        assert_eq!(split_buffer("SELECT 1\nFROM t\n\\gx", pg), ("SELECT 1\nFROM t", "\n\\gx"));
        assert_eq!(split_buffer("select 1 \\gx", pg), ("select 1", " \\gx"));
        assert_eq!(split_buffer("select 1\\g", pg), ("select 1", "\\g"));
        assert_eq!(split_buffer("\\g", pg), ("", "\\g"));
        assert_eq!(split_buffer("select '\\gx'", pg), ("", "select '\\gx'"));
    }

    #[test]
    fn watch_runs_are_titled_as_psqls() {
        // C's `%c`: the day of the month padded to two.
        assert_eq!(asctime(1, 9, 12, 7, 40, 40, 2026), "Mon Oct 12 07:40:40 2026");
        assert_eq!(asctime(0, 0, 5, 23, 4, 9, 2027), "Sun Jan  5 23:04:09 2027");
        let at = "Mon Oct 12 07:40:40 2026";
        assert_eq!(watch_title(None, at, 2000), "Mon Oct 12 07:40:40 2026 (every 2s)");
        assert_eq!(watch_title(None, at, 200), "Mon Oct 12 07:40:40 2026 (every 0.2s)");
        assert_eq!(watch_title(None, at, 1500), "Mon Oct 12 07:40:40 2026 (every 1.5s)");
        assert_eq!(watch_title(None, at, 0), "Mon Oct 12 07:40:40 2026 (every 0s)");
        // `\pset title`, then a tab.
        assert_eq!(watch_title(Some("My title"), at, 2000), "My title\tMon Oct 12 07:40:40 2026 (every 2s)");
        // The interval as C's %g writes it.
        for (secs, g) in [(2.0, "2"), (0.2, "0.2"), (0.001, "0.001"), (1234.5678, "1234.57"), (999999.0, "999999"),
                          (1e6, "1e+06"), (4294967.295, "4.29497e+06"), (0.00001, "1e-05"), (100.0, "100")] {
            assert_eq!(c_general(secs), g, "{secs}");
        }
    }

    #[test]
    fn an_empty_statement_is_the_clis_to_answer() {
        // `select 1;;`: the mysql CLI answers the empty one, psql sends it.
        let stmts = crate::parser::split_statements("select 1;;", ";", Lexicon::Mysql).list;
        assert_eq!(stmts.last().map(|st| st.sql.as_str()), Some(""));
        assert_eq!(plane_for(shared::DbType::MySql).empty_statement(), Some(&["ERROR: ", "No query specified"][..]));
        assert_eq!(plane_for(shared::DbType::Postgres).empty_statement(), None);
        // A terminator that ends nothing is the mysql CLI's to answer, and
        // goes to psql's server; a blank query \gexec generated has none,
        // and goes to the server.
        let (my, pg) = (plane_for(shared::DbType::MySql), plane_for(shared::DbType::Postgres));
        assert!(stmts.last().unwrap().ends_nothing());
        assert!(cli_answer(my, stmts.last().unwrap()).is_some());
        assert!(cli_answer(pg, stmts.last().unwrap()).is_none());
        assert!(!stmts[0].ends_nothing() && cli_answer(my, &stmts[0]).is_none());
        assert!(!TypedStatement::new("", "").ends_nothing());
        assert!(cli_answer(my, &TypedStatement::new("", "")).is_none());
        assert!(!TypedStatement::new(" ", "").ends_nothing());
    }

    #[test]
    fn the_first_command_is_found_in_reading_order() {
        let pg = plane_for(shared::DbType::Postgres);
        let my = plane_for(shared::DbType::MySql);
        fn first<'a>(plane: &dyn DbmsPlane, text: &'a str) -> Option<&'a str> {
            first_command(text, plane, ";").map(|found| &text[found.at..])
        }
        assert_eq!(first(pg, "select 1; select 2 \\x"), Some("\\x"));
        assert_eq!(first(pg, "select 1;\n\\x\nselect 2;"), Some("\\x\nselect 2;"));
        assert_eq!(first(pg, "\\echo a \\x"), Some("\\echo a \\x"));
        assert_eq!(first(pg, "select '\\x';"), None);
        assert_eq!(first(pg, "select 1;"), None);
        // psql's `quit`, on a line typed with nothing pending — not
        // indented, and not after a `/* */` its buffer keeps.
        assert_eq!(first(pg, "select 1;\nQUIT;"), Some("QUIT;"));
        assert_eq!(first(pg, "select 1;\n  quit"), None);
        assert_eq!(first(pg, "select 1,\nquit"), None);
        assert_eq!(first(pg, "/* c */\nhelp"), None);
        assert_eq!(first(pg, "-- c\nhelp"), Some("help"));
        // mysql: its letters, not `\g` / `\G` or an unknown `\i`.
        assert_eq!(first(my, "select 1 \\G select 2 \\s"), Some("\\s"));
        assert_eq!(first(my, "select \\i 1;"), None);
        assert_eq!(first(my, "select 1;\nuse sakila\nselect 2;"), Some("use sakila\nselect 2;"));
        assert_eq!(first(my, "delimiter //\nselect 1 //"), Some("delimiter //\nselect 1 //"));
        // A statement its delimiter ends that's one of its named
        // commands is one, wherever it stands (mysql 9.6).
        assert_eq!(first(my, "use sakila; select 1;"), Some("use sakila; select 1;"));
        assert_eq!(first(my, "select 1; Status ; select 2;"), Some("Status ; select 2;"));
        assert_eq!(first(my, "select 1;\n/* c */ status;"), Some("/* c */ status;"));
        assert_eq!(first(my, "select 1; status foo;"), None);
        assert_eq!(first(my, "select 1; go;"), None);
        // A named command on its line takes the line; with the delimiter
        // in it, it's read as statements.
        let found = first_command("select 1; help select; select 2;", my, ";").unwrap();
        assert_eq!((found.kind, &"select 1; help select; select 2;"[found.at..found.end]), (CommandKind::Statement, "help select"));
        assert_eq!(&"select 1; help select; select 2;"[found.resume..], " select 2;");
        assert_eq!(first_command("status", my, ";").map(|found| found.kind), Some(CommandKind::Line));
        // psql sends such a statement.
        assert_eq!(first(pg, "select 1; quit;"), None);
    }

    #[test]
    fn a_g_command_runs_the_buffer_or_the_previous_query() {
        let pg = plane_for(shared::DbType::Postgres);
        let h = vec![ShellEntry::Query { num: 1, sql: "select 0".into(), terminator: ";".into(), meta: false, errored: false }];
        let run = |buffer: &str, command: &str| { let st = buffer_statement(pg, buffer, command, &h); (st.as_typed(), st.recalled) };
        assert_eq!(run("select 1", " \\gx"), ("select 1 \\gx".into(), false));
        // Nothing before it: the previous query again, as psql keeps it
        // and sends it again — its `;` too.
        assert_eq!(run("", "\\gx"), ("select 0;\n\\gx".into(), true));
        // Recalled again, it's the same: psql copies the buffer it has.
        let mut h2 = h.clone();
        h2.push(ShellEntry::Query { num: 2, sql: "select 0;".into(), terminator: "\n\\gx".into(), meta: false, errored: false });
        assert_eq!(buffer_statement(pg, "", "\\g", &h2).sql, "select 0;");
        assert_eq!(previous_buffer(pg, &h2).as_deref(), Some("select 0;"));
        // No query at all: the empty one psql sends, shown as its line.
        let none = buffer_statement(pg, "", "\\g", &[]);
        assert_eq!((none.as_typed(), none.history_text(), none.sql.as_str()), ("\\g".into(), "\\g".into(), ""));
        // A previous `;` alone: sent again, as psql sends its `;`.
        let semi = vec![ShellEntry::Query { num: 1, sql: String::new(), terminator: ";".into(), meta: false, errored: false }];
        assert_eq!(buffer_statement(pg, "", "\\g", &semi).sql, ";");
    }

    #[test]
    fn a_failed_command_is_the_one_that_ran() {
        let q = |sql: &str, terminator: &str, meta: bool| ShellEntry::Query {
            num: 0, sql: sql.into(), terminator: terminator.into(), meta, errored: false,
        };
        // A batch: all three in the scrollback, the second failed with
        // one still queued.
        let h = vec![q("select 1", ";", false), q("select * from nosuch", ";", false), q("select 3", ";", false)];
        assert_eq!(failed_command(&h, 1, Some("select * from nosuch")),
                   FailedCommand { entry: Some(1), query: Some("select * from nosuch;".into()), meta: false });
        // A statement \gexec generated: no entry, excerpted as sent.
        let h = vec![q("select 'select 1/0'", "\n\\gexec", false)];
        assert_eq!(failed_command(&h, 0, Some("select 1/0")),
                   FailedCommand { entry: None, query: Some("select 1/0".into()), meta: false });
        // A meta-command's own SQL.
        let h = vec![q("select 1", ";", false), q("\\dt foo", "", true)];
        assert_eq!(failed_command(&h, 0, Some("SELECT n.nspname …")),
                   FailedCommand { entry: Some(1), query: Some("SELECT n.nspname …".into()), meta: true });
        // A `;` on the line after a comment comes with its line break.
        let h = vec![q("select x -- why", "\n;", false)];
        assert_eq!(failed_command(&h, 0, Some("select x -- why")).query.as_deref(), Some("select x -- why\n;"));
        // \gdesc keeps its line, not a `;`.
        let h = vec![q("select x", "\n\\gdesc", false)];
        assert_eq!(failed_command(&h, 0, Some("select x")).query.as_deref(), Some("select x"));
    }

    #[test]
    fn g_runs_the_buffer_or_recalls_the_previous_query() {
        let pg = plane_for(shared::DbType::Postgres);
        let h = vec![ShellEntry::Query { num: 1, sql: "select 'hunter2'".into(), terminator: ";".into(), meta: false, errored: false }];
        let typed = buffer_statement(pg, "select :'pw'", "\n\\g", &h);
        assert_eq!((typed.as_typed(), typed.history_text()),
                   ("select :'pw'\n\\g".into(), "select :'pw'\n\\g".into()));
        // The previous query has its variables substituted: the block
        // shows it, history keeps only the `\g` the user typed.
        let recalled = buffer_statement(pg, "", "\\gx", &h);
        assert_eq!((recalled.as_typed(), recalled.history_text()),
                   ("select 'hunter2';\n\\gx".into(), "\\gx".into()));
    }

    #[test]
    fn sql_preview_cuts_on_characters_not_bytes() {
        let long = format!("DELETE FROM {}", "é".repeat(100));
        let p = sql_preview(&long);
        assert!(p.ends_with('…') && p.chars().count() == 78, "{p}");
        assert_eq!(sql_preview("DROP TABLE t;"), "DROP TABLE t;");
    }

    #[test]
    fn recalled_statements_run_again_through_enter() {
        // What recall hands back is what the Enter path dispatches on.
        let pg = plane_for(shared::DbType::Postgres);
        for (terminator, vertical) in [("\n\\g", false), ("\n\\gx", true)] {
            let recalled = TypedStatement::new("SELECT 1", terminator).as_typed();
            assert!(matches!(pg.meta_cmd(&recalled), Some(MetaAction::Go { expanded }) if expanded == vertical), "{recalled:?}");
        }
        let my = plane_for(shared::DbType::MySql);
        let recalled = TypedStatement::new("SELECT 1", "\\G").as_typed();
        assert_eq!(recalled, "SELECT 1\\G");
        assert_eq!(crate::parser::split_statements(&recalled, ";", my.lexicon()).list[0].terminator, "\\G");
    }

    #[test]
    fn copy_block_includes_the_terminator() {
        let h = vec![
            ShellEntry::Query { num: 3, sql: "SELECT 1".into(), terminator: "\n\\gx".into(), meta: false, errored: false },
            ShellEntry::MetaOutput(vec!["(1 row)".into()]),
        ];
        let pg = plane_for(shared::DbType::Postgres);
        assert_eq!(format_block_for_clipboard(&h, 3, BlockCopyMode::Command, pg), "SELECT 1\n\\gx");
        assert!(format_block_for_clipboard(&h, 3, BlockCopyMode::WholeBlock, pg).starts_with("3 SQL> SELECT 1\n\\gx\n"));
    }

    #[test]
    fn a_result_copies_as_it_printed() {
        let strings = |cells: &[&str]| cells.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        let query = ShellEntry::Query { num: 1, sql: "select 1".into(), terminator: ";".into(), meta: false, errored: false };
        let table = |style: PrintedWith| ShellEntry::Table {
            columns: strings(&["a", "b"]), aligns: Vec::new(), rows: vec![vec![Some("1".into()), None]],
            exec_time_us: 2_000, rows_affected: 1, warnings: 0, timed: false, style,
        };
        let pg = plane_for(shared::DbType::Postgres);
        let copy = |entry: ShellEntry| format_block_for_clipboard(&[query.clone(), entry], 1, BlockCopyMode::Output, pg);
        assert_eq!(copy(table(PrintedWith::default())), "a\tb\n1\tNULL\n(1 row)");
        // Its title, its null display; `\pset footer off`.
        let titled = PrintedWith { null_display: "(null)".into(), title: Some("T".into()), footer: false, ..PrintedWith::default() };
        assert_eq!(copy(table(titled.clone())), "T\na\tb\n1\t(null)");
        // `\t`: rows alone.
        assert_eq!(copy(table(PrintedWith { tuples_only: true, ..titled.clone() })), "1\t(null)");
        // Vertical: psql's records under the title, the null display.
        let vertical = |style: PrintedWith| ShellEntry::Vertical {
            columns: strings(&["a", "b"]), aligns: Vec::new(), rows: vec![vec![Some("1".into()), None]], exec_time_us: 2_000, warnings: 0, timed: false, style,
        };
        assert_eq!(copy(vertical(titled.clone())), "T\n-[ RECORD 1 ]\na | 1\nb | (null)");
        assert_eq!(copy(vertical(PrintedWith { tuples_only: true, ..titled })), "a | 1\nb | (null)");
        // A text 'NULL' is text: only SQL NULL shows as the null display.
        let text_null = ShellEntry::Table {
            columns: strings(&["a", "b"]), aligns: Vec::new(), rows: vec![vec![Some("NULL".into()), None]],
            exec_time_us: 2_000, rows_affected: 1, warnings: 0, timed: false,
            style: PrintedWith { null_display: String::new(), ..PrintedWith::default() },
        };
        assert_eq!(copy(text_null), "a\tb\nNULL\t\n(1 row)");
    }

    #[test]
    fn an_empty_result_copies_as_shown() {
        let h = vec![
            ShellEntry::Query { num: 1, sql: "SELECT a FROM t WHERE false".into(), terminator: ";".into(), meta: false, errored: false },
            ShellEntry::Table { columns: Vec::new(), aligns: Vec::new(), rows: Vec::new(), exec_time_us: 2_000, rows_affected: 0, warnings: 0, timed: true, style: PrintedWith::default() },
        ];
        let copy = |h: &[ShellEntry], db| format_block_for_clipboard(h, 1, BlockCopyMode::Output, plane_for(db));
        assert_eq!(copy(&h, shared::DbType::MySql), "Empty set (0.002 sec)");

        // A statement without rows: psql's command tag; mysql's count,
        // warnings and info line.
        let info = |db, status: &shared::CommandStatus, exec_time_us| vec![h[0].clone(), ShellEntry::Info {
            exec_time_us, said: plane_for(db).command_status(status, OutputFormat::Aligned), timed: true,
        }];
        let status = shared::CommandStatus { rows_affected: 1, tag: Some("INSERT 0 1".into()), ..Default::default() };
        assert_eq!(copy(&info(shared::DbType::Postgres, &status, 3_000), shared::DbType::Postgres), "INSERT 0 1\nTime: 3.000 ms");
        assert_eq!(copy(&info(shared::DbType::MySql, &status, 3_000), shared::DbType::MySql), "Query OK, 1 row affected (0.003 sec)");
        let status = shared::CommandStatus {
            rows_affected: 0, warnings: 1, info: Some("Rows matched: 3  Changed: 0  Warnings: 1".into()), ..Default::default()
        };
        assert_eq!(copy(&info(shared::DbType::MySql, &status, 2_000), shared::DbType::MySql),
                   "Query OK, 0 rows affected, 1 warning (0.002 sec)\nRows matched: 3  Changed: 0  Warnings: 1");
        // psql under `QUIET` said nothing: its time alone.
        let quiet = vec![h[0].clone(), ShellEntry::Info { exec_time_us: 3_000, said: Vec::new(), timed: true }];
        assert_eq!(copy(&quiet, shared::DbType::Postgres), "Time: 3.000 ms");
    }

    /// The line under a result, as each CLI words it.
    #[test]
    fn result_footers_are_the_clis() {
        let (my, pg) = (plane_for(shared::DbType::MySql), plane_for(shared::DbType::Postgres));
        assert_eq!(result_footer(my, 2, 0, false, 12_400, true), ["2 rows in set (0.012 sec)"]);
        assert_eq!(result_footer(my, 1, 1, false, 2_000, true), ["1 row in set, 1 warning (0.002 sec)"]);
        assert_eq!(result_footer(my, 1, 2, true, 3_148_000, true), ["1 row in set, 2 warnings (3.148 sec)"]);
        assert_eq!(result_footer(my, 0, 1, false, 2_000, true), ["Empty set, 1 warning (0.002 sec)"]);
        // psql: "(N rows)", the time on a line of its own; expanded
        // output has no footer unless it's empty.
        assert_eq!(result_footer(pg, 2, 0, false, 205, true), ["(2 rows)", "Time: 0.205 ms"]);
        assert_eq!(result_footer(pg, 1, 0, false, 0, true), ["(1 row)", "Time: 0.000 ms"]);
        assert_eq!(result_footer(pg, 1, 0, true, 176, true), ["Time: 0.176 ms"]);
        assert_eq!(result_footer(pg, 0, 0, true, 176, true), ["(0 rows)", "Time: 0.176 ms"]);
        // Untimed (psql's default): the footer alone.
        assert_eq!(result_footer(pg, 2, 0, false, 205, false), ["(2 rows)"]);
    }

    /// What the CLI says reading each line of a text, in order — the
    /// lines already said skipped.
    #[test]
    fn notes_cover_every_line() {
        let plane = plane_for(shared::DbType::MySql);
        let said = |text: &str, skip: usize| -> Vec<String> {
            notes_for(plane, text, skip, ";").into_iter().map(|note| match note {
                ShellEntry::CliError { message, .. } => message,
                _ => String::new(),
            }).collect()
        };
        let text = "select 1 \\i\nselect '\\x'\n \\j as c";
        assert_eq!(said(text, 0), ["Unknown command '\\i'.", "Unknown command '\\j'."]);
        assert_eq!(said(text, "select 1 \\i".len()), ["Unknown command '\\j'."]);
        assert!(said(text, text.len()).is_empty());
        // A quote opened on a line before is read as one.
        assert!(said("select '\nx \\i'", 0).is_empty());
        // A line read in part before (a command mid-line, read on after):
        // that part's notes were said.
        assert!(said("select 1 \\i  from dual", "select 1 \\i".len()).is_empty());
        assert_eq!(said("select 1 \\i \\j from dual", "select 1 \\i".len()), ["Unknown command '\\j'."]);
        // psql: a pending buffer's assistance words, each line read.
        let pg = |text: &str| notes_for(plane_for(shared::DbType::Postgres), text, 0, ";").len();
        assert_eq!(pg("select 1\nhelp\nquit"), 2);
        assert_eq!(pg("select 1\nfrom dual"), 0);
    }

    #[test]
    fn a_gexec_is_judged_by_its_own_statement_unless_one_it_ran_failed() {
        let gexec = GexecPending { rest: 0, trailer: None, row_count: 3, failed: false };
        assert!(matches!(gexec.outcome(), CommandOutcome::Done { row_count: 3 }));
        let failed = GexecPending { failed: true, ..gexec };
        assert!(matches!(failed.outcome(), CommandOutcome::Failed { sqlstate: "", message: "" }));
    }

    /// The SQL of the entry that counts as the previous query.
    fn previous_sql(history: &[ShellEntry]) -> Option<String> {
        previous_buffer(plane_for(shared::DbType::Postgres), history)
    }

    #[test]
    fn previous_query_skips_meta_commands() {
        let h = vec![
            ShellEntry::Query { num: 1, sql: "select 1".into(), terminator: ";".into(), meta: false, errored: false },
            ShellEntry::Query { num: 2, sql: "\\x".into(), terminator: String::new(), meta: true, errored: false },
            ShellEntry::Query { num: 3, sql: "\\dt".into(), terminator: String::new(), meta: true, errored: false },
        ];
        assert_eq!(previous_sql(&h).as_deref(), Some("select 1;"));
    }

    #[test]
    fn previous_query_skips_what_a_watch_ran() {
        let q = |num: u32, sql: &str, terminator: &str| ShellEntry::Query {
            num, sql: sql.into(), terminator: terminator.into(), meta: false, errored: false,
        };
        // psql's `\watch` leaves its previous query as it was; the `\g`
        // family makes theirs the previous one.
        let h = vec![q(1, "select 1 as a", ";"), q(2, "select 2 as b", " \\watch c=1"), q(3, "select 3 as c", "\n\\watch 1")];
        assert_eq!(previous_sql(&h).as_deref(), Some("select 1 as a;"));
        let h = vec![q(1, "select 1 as a", " \\watch"), q(2, "select 3 as c", " \\gx")];
        assert_eq!(previous_sql(&h).as_deref(), Some("select 3 as c "));
        assert_eq!(previous_sql(&[q(1, "select 1", " \\watch c=1")]), None);
    }

    #[test]
    fn an_empty_statement_the_mysql_cli_answered_leaves_it_nothing_to_edit() {
        let (my, pg) = (plane_for(shared::DbType::MySql), plane_for(shared::DbType::Postgres));
        let entry = |sql: &str, terminator: &str, meta: bool| ShellEntry::Query {
            num: 0, sql: sql.into(), terminator: terminator.into(), meta, errored: false,
        };
        // `select 1;` then `;` alone ("No query specified"): the mysql
        // CLI's `old_buffer` is empty — `\g` alone the same.
        for terminator in [";", "\\g"] {
            let h = vec![entry("select 1", ";", false), entry("", terminator, true)];
            assert_eq!(previous_buffer(my, &h).as_deref(), Some(""), "{terminator}");
        }
        // A meta-command doesn't touch it — but its server help, typed
        // alone (the buffer empty) or as a statement (`com_server_help`);
        // a help without a topic lists the commands.
        for (command, terminator, opened) in [
            ("\\s", "", "select 1"), ("help", "", "select 1"),
            ("help select", "", ""), ("\\h select", "", ""), ("? select", "", ""),
            ("help select", ";", "help select"), ("/* c */ HELP select", ";", "/* c */ HELP select"),
            // `go` / `ego` alone run the empty buffer (`com_go`).
            ("go", "", ""), ("EGO", "", ""), ("/* c */ go", "", ""),
        ] {
            let h = vec![entry("select 1", ";", false), entry(command, terminator, true)];
            assert_eq!(previous_buffer(my, &h).as_deref(), Some(opened), "{command}{terminator}");
        }
        // psql never answers one: an empty entry kept out (a `;` its
        // inactive `\if` branch ignored) leaves its previous query.
        let h = vec![entry("select 1", ";", false), entry("", ";", true)];
        assert_eq!(previous_buffer(pg, &h).as_deref(), Some("select 1;"));
    }

    #[test]
    fn previous_query_is_the_latest_query_entry() {
        let h = vec![
            ShellEntry::Query { num: 1, sql: "select 1".into(), terminator: ";".into(), meta: false, errored: false },
            ShellEntry::MetaOutput(vec!["note".into()]),
            ShellEntry::Query { num: 2, sql: "select 2".into(), terminator: ";".into(), meta: false, errored: true },
            ShellEntry::MetaOutput(vec!["note".into()]),
        ];
        assert_eq!(previous_sql(&h).as_deref(), Some("select 2;"));
        assert_eq!(previous_sql(&[]), None);
    }
}
