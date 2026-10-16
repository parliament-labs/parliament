//! Tyto frontend entry point.
//!
//! Owns the App component, the WebSocket lifecycle (connect / disconnect
//! / auto-reconnect / SessionClosed handling), the `ServerMessage`
//! dispatch table, the workspace-tab + pane-tree layout, and every
//! top-level reactive signal (`connections`, `shell_panes`,
//! `workspace_tabs`, `active_pane_id`, `dialog`, `notifications`, …).
//!
//! Per-feature rendering and logic live in sibling modules and are
//! reached from here via function calls or signal hand-offs:
//!
//! - Shell-pane body              → [`crate::shell::render_shell_leaf`]
//! - Schema Explorer body         → [`crate::explorer::render_explorer_pane`]
//! - Activity body                → [`crate::activity`]
//! - Tab-completion popup         → [`crate::completion`]
//! - AST SQL inspection           → [`crate::sql_inspect`]
//! - SQL formatter                → [`crate::format_sql`]
//! - Tokenizer / highlighter      → [`crate::highlight`]
//! - Per-RDBMS planes             → [`crate::plane`]
//!
//! The `ServerMessage::IntrospectResult` handler is the canonical
//! reader of the plane column-alias contract — see
//! `.claude/rules/plane.md` for the column-name table each plane must
//! emit.

use leptos::*;
use shared::{ClientMessage, ConnectionDef, ConnectionDefPublic, CredentialSource, DbType, FormatStyle, ServerMessage};
use std::collections::HashMap;
use uuid::Uuid;
use wasm_bindgen::prelude::*;
use web_sys::{MessageEvent, WebSocket};

mod highlight;
mod parser;
mod plane;
mod types;
mod pane_layout;
mod format;
mod format_sql;
mod sql_inspect;
mod export;
mod history;
mod dom;
mod activity;
mod explorer;
mod shell;
mod completion;
mod psql_crosstab;
mod psql_if;
mod psql_password;
mod psql_prompt;
mod psql_vars;
mod storage_factor;
mod autovac;
mod settings;

use plane::{LostConnection, plane_for};
use types::*;
use format::*;
use history::*;
use dom::*;
use explorer::*;
use shell::*;



// ── App ───────────────────────────────────────────────────────────────────────

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount_to_body(App);
}


// ── App ───────────────────────────────────────────────────────────────────────

/// Format a millisecond timestamp (e.g. from `js_sys::Date::now()`) as
/// `H:MM AM/PM`. If the timestamp is older than 24 hours, prepend a
/// short date (`Apr 20 · 11:42 AM`) so stale notifications don't look
/// like they happened today.
fn format_notification_time(ms: f64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    let now_ms = js_sys::Date::now();
    let h24 = d.get_hours();
    let minutes = d.get_minutes();
    let am = h24 < 12;
    let h12 = match h24 { 0 => 12, 13..=23 => h24 - 12, _ => h24 };
    let time = format!("{}:{:02} {}", h12, minutes, if am { "AM" } else { "PM" });
    if now_ms - ms > 24.0 * 60.0 * 60.0 * 1000.0 {
        const MONTHS: [&str; 12] = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
        let mo  = d.get_month() as usize;           // 0-indexed
        let day = d.get_date();                     // 1-indexed
        format!("{} {} · {}", MONTHS[mo.min(11)], day, time)
    } else {
        time
    }
}

/// Sidebar tree node built on every render from the connection list.
/// Folders are derived from each `ConnectionDef.paths` plus the explicit
/// empty-folder list. `BTreeMap` keeps sub-folders alphabetical.
struct TreeNode {
    name: String,
    /// Full slash-separated path from the root (`""` for the root).
    path: String,
    children: std::collections::BTreeMap<String, TreeNode>,
    connections: Vec<ConnectionDefPublic>,
}

impl TreeNode {
    fn new(name: String, path: String) -> Self {
        TreeNode {
            name,
            path,
            children: std::collections::BTreeMap::new(),
            connections: Vec::new(),
        }
    }
}

/// Descend into the tree along `path_str`'s slash-separated segments,
/// creating any missing nodes. Returns the leaf node at the end of the
/// path. Used both to materialize explicit empty folders and to place
/// each connection's placement.
fn ensure_folder<'a>(root: &'a mut TreeNode, path_str: &str) -> &'a mut TreeNode {
    let segments: Vec<&str> = path_str
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let mut cursor = root;
    let mut running = String::new();
    for seg in segments {
        if !running.is_empty() { running.push('/'); }
        running.push_str(seg);
        cursor = cursor
            .children
            .entry(seg.to_string())
            .or_insert_with(|| TreeNode::new(seg.to_string(), running.clone()));
    }
    cursor
}

/// Build a folder tree from the flat connection list. A connection's
/// `paths` lists every placement; a connection with more than one path
/// appears in each listed folder (links — same underlying config).
/// Connections with empty `paths` land at the root.
///
/// `explicit_folders` materializes folder nodes the user has created but
/// not yet populated, so empty folders persist across renders.
fn build_connection_tree(conns: &[ConnectionDefPublic], explicit_folders: &[String]) -> TreeNode {
    let mut root = TreeNode::new("All Connections".into(), String::new());

    // Materialize user-created empty folders first so the tree carries
    // them even when no connection lives inside.
    for folder_path in explicit_folders {
        ensure_folder(&mut root, folder_path);
    }

    // Fan each connection out across every path in its placement list.
    for conn in conns {
        if conn.paths.is_empty() {
            root.connections.push(conn.clone());
            continue;
        }
        for path in &conn.paths {
            let node = ensure_folder(&mut root, path);
            node.connections.push(conn.clone());
        }
    }

    // Sort each node's connection list by name (case-insensitive).
    fn sort_rec(node: &mut TreeNode) {
        node.connections.sort_by(|a, b| {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        });
        for child in node.children.values_mut() {
            sort_rec(child);
        }
    }
    sort_rec(&mut root);
    root
}

/// A single sidebar row, after the tree is flattened for rendering.
///
/// `source_path` on a `Connection` is the folder path the row is being
/// rendered under. It's threaded through so that gestures originating
/// from a specific placement (drag-to-move, future right-click "Remove
/// from folder") know which path in the connection's multi-path list to
/// operate on, without affecting the connection's other linked
/// placements. Empty `source_path` means the row is in the synthetic
/// "All Connections" view (the connection's actual placement is
/// elsewhere — or root, if `conn.paths` is empty).
// A few dozen rows, rebuilt per render: boxing the connection buys nothing.
#[allow(clippy::large_enum_variant)]
enum SidebarRow {
    Folder { name: String, path: String, depth: usize, is_root: bool },
    Connection {
        conn:        ConnectionDefPublic,
        depth:       usize,
        source_path: String,
        /// `true` (default) for tree / Recents / All Connections rows
        /// — clicking always mints a fresh workspace tab + session.
        /// `false` is reserved for `target_wt_id`-bearing rows where
        /// the click handler routes to a specific existing tab
        /// instead.
        force_new:   bool,
        /// When `Some`, clicking the row jumps to this specific
        /// workspace tab. Used by Active Connections rows, which are
        /// one-per-workspace-tab so two open MySQL sessions surface
        /// as two distinct rows the user can navigate to
        /// independently. `None` for tree / Recents / All Connections
        /// — those follow `force_new`.
        target_wt_id: Option<String>,
    },
}

/// Flatten the tree into a linear list respecting the collapsed-folder set.
/// Connections at this level render *before* sub-folders so a folder's own
/// connections aren't visually mistaken for children of a sibling subfolder
/// once that subfolder is expanded.
fn flatten_tree(
    node: &TreeNode,
    depth: usize,
    is_root: bool,
    collapsed: &std::collections::HashSet<String>,
    out: &mut Vec<SidebarRow>,
) {
    out.push(SidebarRow::Folder {
        name: node.name.clone(),
        path: node.path.clone(),
        depth,
        is_root,
    });
    if collapsed.contains(&node.path) {
        return;
    }
    for conn in &node.connections {
        out.push(SidebarRow::Connection {
            conn:         conn.clone(),
            depth:        depth + 1,
            source_path:  node.path.clone(),
            force_new:    true,
            target_wt_id: None,
        });
    }
    for child in node.children.values() {
        flatten_tree(child, depth + 1, false, collapsed, out);
    }
}


/// Apply a clipboard entry at a destination folder. Semantics:
///
/// - **Cut**: remove the entry's source placement from the connection's
///   path list, then add the destination. Net effect: a move.
/// - **Copy**: clone the connection with a fresh UUID, set its placement
///   to just the destination, save as a new record.
/// - **Link**: add the destination to the existing connection's path
///   list (idempotent — won't double-add).
/// - `force_link = true` forces Link semantics regardless of the verb on
///   the clipboard. Lets a user Copy-as-Link from one place but Paste at
///   another, or Cut then change their mind and Paste-as-Link to preserve
///   the source.
///
/// Destination `""` (root) drops to "no folder" — the entry's source path
/// is still removed (Cut) but nothing is added.
fn apply_clipboard_paste(
    entry: &ClipboardEntry,
    dest_path: &str,
    force_link: bool,
    connections: RwSignal<Vec<ConnectionDefPublic>>,
    ws: RwSignal<Option<WebSocket>>,
) {
    let Some(orig) = connections.get_untracked()
        .into_iter()
        .find(|c| c.id == entry.conn_id)
    else { return };

    let effective_verb = if force_link { ClipboardVerb::Link } else { entry.verb };

    match effective_verb {
        ClipboardVerb::Cut => {
            // Move: drop the source placement, add the destination
            // placement. Other linked placements survive.
            let mut def = orig;
            if !entry.source_path.is_empty() {
                def.paths.retain(|p| p != &entry.source_path);
            }
            if !dest_path.is_empty() && !def.paths.contains(&dest_path.to_string()) {
                def.paths.push(dest_path.to_string());
            }
            ws_send(ws, &ClientMessage::SaveConnection { def, password_change: None, copy_from: None });
        }
        ClipboardVerb::Copy => {
            // Independent copy: new UUID, fresh placement list. The
            // browser doesn't hold the password (see ConnectionDefPublic
            // + issue #6), so we tell noctua to look up the SOURCE
            // connection's password and copy it onto the new id. Without
            // copy_from, the new id has no on-disk record and the saved
            // password would silently become empty.
            let source_id = orig.id.clone();
            let mut def = orig;
            def.id    = Uuid::new_v4().to_string();
            def.paths = if dest_path.is_empty() { vec![] } else { vec![dest_path.to_string()] };
            ws_send(ws, &ClientMessage::SaveConnection {
                def,
                password_change: None,
                copy_from: Some(source_id),
            });
        }
        ClipboardVerb::Link => {
            // Add destination as another placement, leaving everything
            // else alone. No-op if already linked there.
            let mut def = orig;
            if !dest_path.is_empty() && !def.paths.contains(&dest_path.to_string()) {
                def.paths.push(dest_path.to_string());
                ws_send(ws, &ClientMessage::SaveConnection { def, password_change: None, copy_from: None });
            }
        }
    }
}

/// Rename a folder path everywhere it appears: in the explicit-folder
/// list (so empty folders follow), in each connection's path list (so
/// every placement follows), and including any sub-folder paths that
/// inherit the renamed prefix.
fn rename_folder_path(
    old_path: &str,
    new_path: &str,
    connections: RwSignal<Vec<ConnectionDefPublic>>,
    folders: RwSignal<Vec<String>>,
    ws: RwSignal<Option<WebSocket>>,
) {
    let old_prefix = format!("{}/", old_path);
    let rewrite = |p: &str| -> Option<String> {
        if p == old_path {
            Some(new_path.to_string())
        } else if p.starts_with(&old_prefix) {
            Some(format!("{}/{}", new_path, &p[old_prefix.len()..]))
        } else {
            None
        }
    };

    // 1. Rewrite explicit folders (preserving order).
    let new_folders: Vec<String> = folders.get_untracked()
        .into_iter()
        .map(|f| rewrite(&f).unwrap_or(f))
        .collect();
    ws_send(ws, &ClientMessage::SaveFolders { folders: new_folders });

    // 2. Rewrite placements on every affected connection.
    for conn in connections.get_untracked() {
        let mut def = conn;
        let mut touched = false;
        for path in def.paths.iter_mut() {
            if let Some(np) = rewrite(path) {
                *path = np;
                touched = true;
            }
        }
        if touched {
            ws_send(ws, &ClientMessage::SaveConnection { def, password_change: None, copy_from: None });
        }
    }
}

/// Opens the in-app "New folder" prompt anchored at `parent` (empty =
/// root). On submit, appends the full path to the explicit-folder list
/// and fires `SaveFolders`. Reused by both the + dropdown and the
/// right-click "New folder here" verb so they behave identically.
fn new_folder_at(
    parent: String,
    dialog: RwSignal<Option<DialogKind>>,
    folders: RwSignal<Vec<String>>,
    ws:      RwSignal<Option<WebSocket>>,
) {
    let message = if parent.is_empty() {
        "Folder name (top level):".to_string()
    } else {
        format!("Folder name (inside \u{201c}{}\u{201d}):", parent)
    };
    let parent_for_submit = parent.clone();
    dialog.set(Some(DialogKind::Prompt {
        title:         "New folder".to_string(),
        message,
        default_value: String::new(),
        confirm_label: "Create".to_string(),
        on_submit: Callback::new(move |name: String| {
            if name.is_empty() || name.contains('/') { return; }
            let full = if parent_for_submit.is_empty() {
                name
            } else {
                format!("{}/{}", parent_for_submit, name)
            };
            let mut current = folders.get_untracked();
            if !current.contains(&full) {
                current.push(full);
                ws_send(ws, &ClientMessage::SaveFolders { folders: current });
            }
        }),
    }));
}

/// Delete a folder and every sub-folder. Connections placed inside lose
/// just those placements (their other linked locations, if any, survive);
/// underlying connection records are kept — Delete connection is a
/// separate, more dangerous verb.
fn delete_folder_path(
    path: &str,
    connections: RwSignal<Vec<ConnectionDefPublic>>,
    folders: RwSignal<Vec<String>>,
    ws: RwSignal<Option<WebSocket>>,
) {
    let prefix = format!("{}/", path);
    let drop_path = |p: &str| -> bool {
        p == path || p.starts_with(&prefix)
    };

    // 1. Drop the folder + any sub-folders from the explicit-folder list.
    let new_folders: Vec<String> = folders.get_untracked()
        .into_iter()
        .filter(|f| !drop_path(f))
        .collect();
    ws_send(ws, &ClientMessage::SaveFolders { folders: new_folders });

    // 2. Drop matching placements from every affected connection.
    for conn in connections.get_untracked() {
        let mut def = conn;
        let before = def.paths.len();
        def.paths.retain(|p| !drop_path(p));
        if def.paths.len() != before {
            ws_send(ws, &ClientMessage::SaveConnection { def, password_change: None, copy_from: None });
        }
    }
}

/// Normalize a user-entered folder path for persistence.
///
/// Trims whitespace, trims leading/trailing `/`, collapses runs of `/`
/// into a single separator. Returns `None` if the result is empty so
/// the connection ends up at the sidebar root (`paths: vec![]`) rather
/// than at a `Some("")` ghost-folder placement.
fn normalize_folder_path(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .split('/')
        .map(str::trim)
        .filter(|seg| !seg.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if cleaned.is_empty() { None } else { Some(cleaned) }
}

/// Map a free-text environment tag to a CSS color token for the pane-tab
/// left stripe. Case-insensitive. Unknown values get no stripe.
fn env_color(env: &str) -> Option<&'static str> {
    match env.trim().to_ascii_lowercase().as_str() {
        "prod" | "production"                  => Some("var(--red)"),
        "staging" | "stage" | "stg" | "uat"    => Some("var(--yellow)"),
        "dev" | "development" | "local"        => Some("var(--green)"),
        "test" | "qa"                          => Some("var(--purple)"),
        _                                      => None,
    }
}

/// Build the multi-line tooltip shown on pane-tab hover. Lines are skipped
/// when the corresponding field is absent.
fn build_tooltip(
    pid: &str,
    shell_panes: RwSignal<HashMap<String, ShellTab>>,
    connections: RwSignal<Vec<ConnectionDefPublic>>,
) -> String {
    let tab = match shell_panes.get_untracked().get(pid).cloned() {
        Some(t) => t,
        None    => return String::new(),
    };
    let conn = connections.get_untracked().into_iter().find(|c| c.id == tab.conn_id);
    let mut lines: Vec<String> = vec![];
    if let Some(c) = conn {
        lines.push(match c.env.as_deref() {
            Some(e) if !e.is_empty() => format!("{} ({})", c.name, e),
            _                        => c.name,
        });
    }
    match tab.last_status.get_untracked() {
        PaneStatus::Ok(s)      => lines.push(s),
        PaneStatus::Err(s)     => lines.push(format!("Error: {}", s)),
        PaneStatus::NotStarted => {}
    }
    if let Some(id) = tab.db_connection_id.get_untracked() {
        if !id.is_empty() { lines.push(format!("#{}", id)); }
    }
    lines.push("Double-click to zoom / restore".into());
    lines.join("\n")
}

/// Pull the most recent submitted query's SQL out of a pane's shell
/// history, collapse whitespace, and truncate to 80 chars with an
/// ellipsis. Returns `None` if the pane has no prior query.
fn sql_snippet_for(tab: &ShellTab) -> Option<String> {
    tab.shell_history.get_untracked().iter().rev()
        .find_map(|e| match e {
            ShellEntry::Query { sql, .. } if !sql.is_empty() => Some(sql.clone()),
            _ => None,
        })
        .map(|s| {
            let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
            if collapsed.chars().count() > 80 {
                let head: String = collapsed.chars().take(79).collect();
                format!("{}…", head)
            } else {
                collapsed
            }
        })
}

/// Push a `Notification` for a completed query / query error onto the
/// activity queue, looking up the connection name from `connections` and
/// the current SQL from the pane's history. Active-pane and `\watch`
/// suppression are the caller's responsibility.
fn push_notification(
    notifications: RwSignal<Vec<Notification>>,
    connections: RwSignal<Vec<ConnectionDefPublic>>,
    tab: &ShellTab,
    event: NotificationEvent,
) {
    let sql_snippet = sql_snippet_for(tab);
    let conn_name = connections.get_untracked().iter()
        .find(|c| c.id == tab.conn_id)
        .map(|c| c.name.clone())
        .unwrap_or_default();
    notifications.update(|v| v.push(Notification {
        id:            Uuid::new_v4().to_string(),
        pane_id:       tab.id.clone(),
        pane_label:    tab.label.clone(),
        conn_id:       tab.conn_id.clone(),
        conn_name,
        db_type:       tab.db_type,
        event,
        sql_snippet,
        created_at_ms: js_sys::Date::now(),
    }));
}

/// Stable string key for an AdminArea — used as the introspect tag suffix
/// (e.g. `admin_area:databases`) so each area's response routes back to
/// the right HashMap entry without collisions.
/// A command an explorer sent to its action shell just finished there:
/// refresh that explorer's view of the table so the Storage tab shows
/// the effect (VACUUM resets dead tuples, ANALYZE the change count).
fn finish_explorer_action(
    shell:       &ShellTab,
    shell_panes: RwSignal<HashMap<String, ShellTab>>,
    ws:          RwSignal<Option<WebSocket>>,
) {
    let Some(origin) = shell.action_origin.get_untracked() else { return };
    shell.action_origin.set(None);
    let Some(explorer) = shell_panes.with_untracked(|m| m.get(&origin.explorer_pane_id).cloned())
    else { return };
    let key = (origin.schema, origin.name);
    let Some(detail) = explorer.explorer_table_details.with_untracked(|m| m.get(&key).copied())
    else { return };
    crate::explorer::refresh_table_detail(
        plane_for(explorer.db_type), &key.0, &key.1, detail, ws,
        crate::explorer::ExplorerSession::of(&explorer),
    );
}

/// Whether an explorer's action shell can take another command: live,
/// on the database the explorer is showing (the explorer can switch
/// databases on one connection), idle, outside a transaction, not
/// watching, not mid-`\password` / `\prompt` / safe-mode confirmation,
/// with no input left to read, and nothing typed that running a command
/// would clear.
fn action_shell_can_take(shell: &ShellTab, database: &str) -> bool {
    shell.session_id.get_untracked().is_some()
        && !shell.session_stale.get_untracked()
        && shell.current_db.with_untracked(|db| db == database)
        && !shell.querying.get_untracked()
        && shell.txn_state.get_untracked() == TxnState::Idle
        && shell.watch_sql.get_untracked().is_none()
        && shell.pending_stmts.with_untracked(|v| v.is_empty())
        && shell.password_prompt.with_untracked(Option::is_none)
        && shell.prompt_state.with_untracked(Option::is_none)
        && shell.external_edit.with_untracked(Option::is_none)
        && shell.pending_confirm.with_untracked(Option::is_none)
        && shell.resume_input.with_untracked(Option::is_none)
        && shell.current_input.with_untracked(|s| s.is_empty())
}

pub(crate) fn admin_area_tag(area: AdminArea) -> &'static str {
    match area {
        AdminArea::Roles        => "roles",
        AdminArea::Tablespaces  => "tablespaces",
        AdminArea::Databases    => "databases",
        AdminArea::Schemas      => "schemas",
        AdminArea::Extensions   => "extensions",
        AdminArea::ServerConfig => "server_config",
    }
}

fn parse_admin_area_tag(s: &str) -> Option<AdminArea> {
    match s {
        "roles"         => Some(AdminArea::Roles),
        "tablespaces"   => Some(AdminArea::Tablespaces),
        "databases"     => Some(AdminArea::Databases),
        "schemas"       => Some(AdminArea::Schemas),
        "extensions"    => Some(AdminArea::Extensions),
        "server_config" => Some(AdminArea::ServerConfig),
        _ => None,
    }
}

#[component]
fn App() -> impl IntoView {
    let ws: RwSignal<Option<WebSocket>>              = create_rw_signal(None);
    let ws_status: RwSignal<WsStatus>                = create_rw_signal(WsStatus::Connecting);
    // Recents — auto-populated list of recently-opened connections,
    // rendered as a virtual folder at the top of the sidebar. Persists
    // to localStorage so the list survives reload.
    let recents: RwSignal<Vec<RecentEntry>>          = create_rw_signal(load_recents());
    // Last folder header the user clicked. Drives the default placement
    // for the sidebar's + button: + click / + hover-dropdown items both
    // create the new thing inside this folder. Empty = root. Reset to
    // empty when the user clicks the "All Connections" header or the
    // Recents virtual header (those aren't real folders), or when the
    // active folder is deleted out from under us.
    let last_active_folder: RwSignal<String>         = create_rw_signal(String::new());
    // Hover state for the sidebar's + dropdown trigger. Toggled by the
    // wrapper span's mouseenter / mouseleave.
    let add_menu_open:      RwSignal<bool>           = create_rw_signal(false);
    // Screen-space position the dropdown menu is rendered at — captured
    // on mouseenter from the trigger's `getBoundingClientRect()` so the
    // menu (which is position: fixed to escape sidebar overflow clipping)
    // lands directly below the + button.
    let add_menu_pos:       RwSignal<(f64, f64)>     = create_rw_signal((0.0, 0.0));
    // Generic in-app modal dialog. Drives the prompt/confirm overlay at the
    // App view root. `None` means no dialog visible.
    let dialog:             RwSignal<Option<DialogKind>> = create_rw_signal(None);
    // Buffer for the dialog's input field (when the dialog is a Prompt).
    // Seeded with the Prompt's `default_value` on the None→Some transition
    // by the effect below. Read by the on_submit callback at confirm time.
    let dialog_input:       RwSignal<String>             = create_rw_signal(String::new());
    // Seed the input field with the Prompt's default value on the open
    // transition only — so the user can clear the input and have it stay
    // empty (re-seeding on every render would silently re-fill).
    create_effect(move |prev: Option<bool>| {
        let was_open = prev.unwrap_or(false);
        let now = dialog.get();
        let is_open = now.is_some();
        if !was_open && is_open {
            if let Some(DialogKind::Prompt { default_value, .. }) = now {
                dialog_input.set(default_value);
            } else {
                dialog_input.set(String::new());
            }
        }
        is_open
    });
    // Auto-reconnect backoff in milliseconds. Doubles on each consecutive
    // failure (capped at 30s) and resets to 1s on a successful onopen.
    // Read by the schedule_reconnect helper, written by both the close/
    // error path (advance) and the open path (reset).
    let reconnect_backoff_ms: RwSignal<u32>          = create_rw_signal(1000);
    let connections: RwSignal<Vec<ConnectionDefPublic>> = create_rw_signal(vec![]);
    // App-wide preferences from noctua (`~/.sqlclient/settings.json`),
    // requested on every WebSocket open, and the Settings dialog's state.
    let settings_ui = settings::SettingsState::new();
    // Explicit empty-folder list mirrored from the backend. Folders that
    // contain connections come for free from the connection tree builder;
    // this signal carries the folders the user has created but not yet
    // populated, so they stick across renders.
    let connection_folders: RwSignal<Vec<String>>    = create_rw_signal(vec![]);
    let shell_panes: RwSignal<HashMap<String, ShellTab>> = create_rw_signal(HashMap::new());
    let workspace_tabs: RwSignal<Vec<WorkspaceTab>>  = create_rw_signal(vec![]);
    let active_wt_id: RwSignal<Option<String>>       = create_rw_signal(None);
    let active_pane_id: RwSignal<Option<String>>     = create_rw_signal(None);
    let pane: RwSignal<Pane>                         = create_rw_signal(Pane::Welcome);
    let sidebar_collapsed: RwSignal<bool>            = create_rw_signal(false);
    // Set of folder paths currently collapsed in the sidebar tree.
    // Persisted to localStorage so the user's expand/collapse choices
    // survive page refresh. Empty path "" = the "All Connections" row.
    let collapsed_folders: RwSignal<std::collections::HashSet<String>> =
        create_rw_signal(load_collapsed_folders());
    create_effect(move |_| {
        persist_collapsed_folders(&collapsed_folders.get());
    });
    // Folder path currently under a drag-over for a connection drop. Drives
    // the .folder-drop-target class so the user sees where they'll land.
    let drop_target_path: RwSignal<Option<String>> = create_rw_signal(None);
    let notif_open: RwSignal<bool>                   = create_rw_signal(false);
    // Activity queue for the app-header notification bell. Single source
    // of truth — bell-badge total and per-connection sidebar badges both
    // derive from this list. Pushed to on query complete/error in a
    // non-active, non-watch pane; trimmed on pane focus and dismiss.
    let notifications: RwSignal<Vec<Notification>>   = create_rw_signal(Vec::new());

    // Tab-interaction state (right-click menu, inline rename, reopen stack).
    let tab_context_menu: RwSignal<Option<TabContextMenu>> = create_rw_signal(None);
    // Right-click context menu on the shell input textarea. Drives the
    // Format submenu (default style / Unformatted / River). Mounted at
    // the App view root; backdrop closes on outside-click and Esc.
    let shell_input_menu: RwSignal<Option<ShellInputContextMenu>> = create_rw_signal(None);
    // Tab-completion popup. Opens on Tab when the cursor sits in a
    // completable token. Mounted at the App view root like the other
    // overlays. None = no popup visible.
    let completion_popup: RwSignal<Option<CompletionPopup>> = create_rw_signal(None);
    // Sidebar right-click context menu — opens on connection rows and folder
    // headers, drives the cut/copy/link/paste/rename verb set. Mounted at the
    // App view root so a backdrop covers the whole window and outside-clicks
    // close the menu.
    let sidebar_context_menu: RwSignal<Option<SidebarContextMenu>> = create_rw_signal(None);
    // Connection-management clipboard. Populated by Cut / Copy / Copy-as-Link
    // on a connection row; consumed by Paste / Paste-as-Link on a folder
    // header. Cut clears itself after Paste; Copy / Link survive so the same
    // payload can be pasted into multiple folders.
    let conn_clipboard: RwSignal<Option<ClipboardEntry>> = create_rw_signal(None);
    let renaming_pane_id: RwSignal<Option<String>>         = create_rw_signal(None);
    let rename_input:     RwSignal<String>                 = create_rw_signal(String::new());
    let closed_panes_stack: RwSignal<Vec<ClosedPaneInfo>>  = create_rw_signal(Vec::new());

    // New-connection form fields
    let nc_db_type: RwSignal<DbType> = create_rw_signal(DbType::MySql);
    let nc_name: RwSignal<String> = create_rw_signal(String::new());
    let nc_host: RwSignal<String> = create_rw_signal("127.0.0.1".into());
    let nc_port: RwSignal<String> = create_rw_signal("3306".into());
    let nc_user: RwSignal<String> = create_rw_signal("root".into());
    let nc_pass: RwSignal<String> = create_rw_signal(String::new());
    let nc_db:   RwSignal<String> = create_rw_signal(String::new());
    let nc_default_pane: RwSignal<String> = create_rw_signal("shell".into());
    let nc_env: RwSignal<String> = create_rw_signal(String::new());
    let nc_folder_path: RwSignal<String> = create_rw_signal(String::new());
    let nc_format_style: RwSignal<FormatStyle> = create_rw_signal(FormatStyle::Unformatted);
    // Memory-cleanup threshold (megabytes) per shell pane opened from
    // this connection. NOT a pre-allocated buffer size — nothing is
    // reserved up-front. Empty string = no threshold = no cleanup
    // (saved as `None` on ConnectionDef). Non-empty must parse as u32
    // to apply. After each query completes on a shell pane, if the
    // pane's accumulated history exceeds this threshold, the oldest
    // block is evicted. See `shared::ConnectionDef::buffer_threshold_mb`.
    let nc_buffer_threshold_mb: RwSignal<String> = create_rw_signal(String::new());
    // This connection's editor override for `\e` / `\ef` / `\ev`.
    // Empty string → saved as None on ConnectionDef → noctua uses
    // Settings › Editor. Example values: `code -w`, `mvim -f`. Parsed
    // via shlex on noctua so quoted paths with spaces work.
    let nc_editor_command: RwSignal<String> = create_rw_signal(String::new());
    let editing_conn_id: RwSignal<Option<String>> = create_rw_signal(None);
    let nc_test_status: RwSignal<Option<(bool, String)>> = create_rw_signal(None);
    let nc_db_list: RwSignal<Vec<String>> = create_rw_signal(vec![]);
    // Default schema, PG only — empty when MySQL or "no preference."
    // Hot-loaded via the ↻ button next to the schema input, mirroring the
    // database hot-load. Empty `nc_schema_list` = no hot-load fired yet
    // (text input mode); non-empty = dropdown mode.
    let nc_schema: RwSignal<String> = create_rw_signal(String::new());
    let nc_schema_list: RwSignal<Vec<String>> = create_rw_signal(vec![]);
    // Which credential source the form uses. `Plaintext` = the typed
    // password field (historical behaviour); `Pgpass` = noctua reads
    // `~/.pgpass` at connect time. PG only — MySQL always uses Plaintext.
    let nc_credential_source: RwSignal<CredentialSource> = create_rw_signal(CredentialSource::Plaintext);

    // ── Wire up WebSocket ─────────────────────────────────────────────────────
    //
    // The connect_ws callback is invoked once on cold-start, again from the
    // Reconnect button on the disconnected banner, AND recursively by the
    // auto-reconnect path inside the close/error handlers. Each invocation
    // builds a fresh WebSocket and re-wires all four event handlers; the
    // previous handlers (already `.forget()`-ed onto the heap) become
    // unreachable when their owning socket is dropped. The
    // `connect_ws_slot` indirection lets the disconnect handlers refer to
    // connect_ws after the closure body has captured everything else by
    // value (you can't reference a Rust binding inside its own initializer).
    let connect_ws_slot: StoredValue<Option<Callback<()>>> = StoredValue::new(None);
    let connect_ws = Callback::new(move |_: ()| {
        let socket = match WebSocket::new("ws://localhost:3000/ws") {
            Ok(s) => s,
            Err(_) => {
                ws_status.set(WsStatus::Disconnected);
                return;
            }
        };

        // Centralized cleanup for in-flight per-pane state. Called from any
        // path that disrupts a session — WS drop (`on_disconnect`), server-
        // initiated `SessionClosed`, or anywhere else that leaves a pane
        // with promises that won't be kept. Clears every signal whose
        // associated round-trip is in flight and won't complete:
        //   - `querying` / `connecting` / `watch_pending` /
        //     `explorer_db_pending` flag the existence of an in-flight
        //     request the response handler will never see post-disruption
        //   - `watch_timer` / `activity_timer` JS intervals keep firing
        //     into a dead session_id otherwise; clearing them drops the
        //     Closure cleanly
        //   - `explorer_databases` / `explorer_schemas` / `explorer_listing`
        //     in `LoadState::Loading` would otherwise render a permanent
        //     spinner; resetting to `NotLoaded` lets the next view trigger
        //     a fresh fetch after recovery
        //   - per-`TableDetail` Loading states scattered inside
        //     `explorer_table_details` have the same issue, treated the
        //     same way
        //   - `pending_stmts` is the multi-statement queue; the remainder
        //     can't continue since the in-flight result won't arrive
        //
        // Caches that represent stable schema data (loaded values inside
        // `explorer_table_details`, etc.) are left alone — they remain
        // valid across reconnects against the same database. Per-session
        // caches (Activity pane data) are tied to their pane's session_id
        // which will be replaced on reconnect; the next render triggers a
        // refetch.
        //
        // Audit reference: this addresses the body of findings 2.1, 2.7,
        // 2.8, 2.9, 2.11 in `vault/Engineering/Audit Findings 2026-06.md`.
        let reset_in_flight_signals_outer = move |pane_id: &str| {
            let Some(tab) = shell_panes.get_untracked().get(pane_id).cloned() else { return };
            tab.querying.set(false);
            tab.connecting.set(false);
            tab.watch_pending.set(None);
            // A watch is over with its session, as psql's ends when its
            // next run finds the connection gone: what was typed
            // meanwhile reads on, and finds the session lost (or the
            // reopened one).
            shell::stop_watch(&tab);
            tab.pending_meta.set(None);
            tab.wrap.set(None);
            // The command it waited on didn't finish: it says no more.
            tab.pending_warnings.set(Vec::new());
            tab.pending_list.set(None);
            // A session that's gone has no transaction, and nothing
            // waits to be sent to a new one.
            tab.txn_state.set(TxnState::Idle);
            tab.reopening.set(None);
            tab.switching.set(None);
            tab.explorer_db_pending.set(false);
            tab.watch_timer.update_value(|slot| {
                if let Some((id, _cb)) = slot.take() {
                    if let Some(w) = web_sys::window() {
                        w.clear_interval_with_handle(id);
                    }
                }
            });
            tab.activity_timer.update_value(|slot| {
                if let Some((id, _cb)) = slot.take() {
                    if let Some(w) = web_sys::window() {
                        w.clear_interval_with_handle(id);
                    }
                }
            });
            if matches!(tab.explorer_databases.get_untracked(), LoadState::Loading) {
                tab.explorer_databases.set(LoadState::NotLoaded);
            }
            if matches!(tab.explorer_schemas.get_untracked(), LoadState::Loading) {
                tab.explorer_schemas.set(LoadState::NotLoaded);
            }
            if matches!(tab.explorer_listing.get_untracked(), LoadState::Loading) {
                tab.explorer_listing.set(LoadState::NotLoaded);
            }
            tab.explorer_table_details.update(|m| {
                for td in m.values() {
                    if matches!(td.overview.get_untracked(),    LoadState::Loading) { td.overview.set(LoadState::NotLoaded);    }
                    if matches!(td.columns.get_untracked(),     LoadState::Loading) { td.columns.set(LoadState::NotLoaded);     }
                    if matches!(td.indexes.get_untracked(),     LoadState::Loading) { td.indexes.set(LoadState::NotLoaded);     }
                    if matches!(td.ddl.get_untracked(),         LoadState::Loading) { td.ddl.set(LoadState::NotLoaded);         }
                    if matches!(td.ancestors.get_untracked(),   LoadState::Loading) { td.ancestors.set(LoadState::NotLoaded);   }
                    if matches!(td.descendants.get_untracked(), LoadState::Loading) { td.descendants.set(LoadState::NotLoaded); }
                    if matches!(td.partitions.get_untracked(),  LoadState::Loading) { td.partitions.set(LoadState::NotLoaded);  }
                    if matches!(td.data_page.get_untracked(),   LoadState::Loading) { td.data_page.set(LoadState::NotLoaded);   }
                }
            });
            // What was in flight is gone; what's left to read
            // (`resume_input`) isn't — the CLI reads on, and its next
            // statement finds the session lost (or the reopened one).
            tab.pending_stmts.set(Vec::new());
            tab.gexec.set(None);
            tab.held_notes.set(Vec::new());
            // An open editor's reply can't reach this shell any more.
            shell::lose_external_edit(&tab);
        };

        // After each query completes on a shell pane, check the pane's
        // shell-history against the connection's configured threshold
        // (`ConnectionDef::buffer_threshold_mb`). Over → drop the SINGLE
        // oldest block (one Query entry plus its trailing output
        // entries up to the next Query). One eviction per call — a
        // single huge result can momentarily exceed the threshold;
        // subsequent queries trim back down naturally as their checks
        // fire. Guard via the second-Query lookup: if there's only one
        // Query in history, no `second_idx` and we leave it alone —
        // never evict the only block the user just ran.
        //
        // Cap is read LIVE from the connections signal by the pane's
        // conn_id, so editing a connection's threshold applies
        // immediately to existing open panes without further plumbing.
        let apply_buffer_threshold = move |conn_id: &str, history: RwSignal<Vec<ShellEntry>>| {
            let cap_mb = connections.with_untracked(|cs|
                cs.iter().find(|c| c.id == conn_id).and_then(|c| c.buffer_threshold_mb));
            let Some(cap) = cap_mb else { return };
            let cap_bytes = (cap as usize).saturating_mul(1024 * 1024);
            let total = history.with_untracked(|h| crate::shell::shell_buffer_bytes(h));
            if total <= cap_bytes { return; }
            history.update(|h| {
                let mut query_positions = h.iter().enumerate()
                    .filter_map(|(i, e)| matches!(e, ShellEntry::Query { .. }).then_some(i));
                let _first = query_positions.next();
                let second = query_positions.next();
                if let Some(second_idx) = second {
                    h.drain(0..second_idx);
                }
            });
        };

        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let reset_in_flight_signals = reset_in_flight_signals_outer;
            let Some(text) = e.data().as_string() else { return };
            let Ok(msg) = serde_json::from_str::<ServerMessage>(&text) else { return };
            let shell_for_session = |sid: &str| shell_panes.get_untracked().values()
                .find(|t| t.kind == PaneKind::Shell && t.session_id.get_untracked().as_deref() == Some(sid))
                .cloned();
            match msg {
                ServerMessage::Settings { settings, load_error } => settings_ui.loaded(settings, load_error),
                ServerMessage::SettingsSaved { settings } => settings_ui.saved(ws, settings),
                ServerMessage::SettingsError { message } => settings_ui.failed(message),
                ServerMessage::EditorTestResult { ok, message } => settings_ui.tested(ok, message),
                ServerMessage::ConnectionList { connections: list, folders } => {
                    connections.set(list);
                    connection_folders.set(folders);
                }
                ServerMessage::FoldersSaved { folders } => {
                    connection_folders.set(folders);
                }
                ServerMessage::ConnectionSaved { def } => {
                    connections.update(|v| {
                        if let Some(e) = v.iter_mut().find(|c| c.id == def.id) { *e = def; }
                        else { v.push(def); }
                    });
                    if workspace_tabs.get().is_empty() { pane.set(Pane::Welcome); }
                    else { pane.set(Pane::Shell); }
                }
                ServerMessage::ConnectionDeleted { id } => {
                    connections.update(|v| v.retain(|c| c.id != id));
                }
                ServerMessage::SessionOpened { client_tab_id, session_id: sid, server_version: sv, database: db, db_connection_id: dbcid, extensions: exts, password_used, ssl_library, numeric_locale, .. } => {
                    // client_tab_id == pane_id
                    let is_shell = if let Some(tab) = shell_panes.get().get(&client_tab_id).cloned() {
                        // Auto-populate the read-only psql predefined
                        // variables (`:USER`, `:DBNAME`, etc.) before
                        // moving `sv` / `db` into their dedicated
                        // signals below. See psql_vars::classify for
                        // the full list.
                        let conn_info = connections.get_untracked().iter()
                            .find(|c| c.id == tab.conn_id)
                            .map(|c| (c.user.clone(), c.host.clone(), c.port));
                        let sv_clone = sv.clone();
                        let db_clone = db.clone();
                        tab.variables.update(|m| {
                            if let Some((user, host, port)) = conn_info {
                                m.insert("USER".into(), user);
                                m.insert("HOST".into(), host);
                                m.insert("PORT".into(), port.to_string());
                            }
                            m.insert("DBNAME".into(), db_clone);
                            // psql exposes VERSION as the full
                            // server-version string; VERSION_NAME /
                            // SERVER_VERSION_NAME mirror it for
                            // compatibility. VERSION_NUM (a packed
                            // integer like 150003) needs version-string
                            // parsing — deferred to a follow-up.
                            m.insert("VERSION".into(), sv_clone.clone());
                            m.insert("VERSION_NAME".into(), sv_clone.clone());
                            m.insert("SERVER_VERSION_NAME".into(), sv_clone);
                            // The client encoding the session speaks, as
                            // psql's SyncVariables sets it.
                            m.insert("ENCODING".into(), plane_for(tab.db_type).client_encoding().unwrap_or_default().into());
                            // Pre-initialise the read-only vars that
                            // don't have a populated source yet, so
                            // `:NAME` substitution returns empty
                            // (consistent) rather than passing the
                            // literal token through unchanged. As the
                            // populating handlers land (protocol
                            // extensions for SQLSTATE / VERSION_NUM /
                            // LASTOID), these defaults stay correct.
                            m.entry("ERROR".into()).or_insert_with(|| "false".into());
                            m.entry("VERSION_NUM".into()).or_insert_with(String::new);
                            m.entry("SERVER_VERSION_NUM".into()).or_insert_with(String::new);
                            m.entry("LAST_ERROR_MESSAGE".into()).or_insert_with(String::new);
                            // psql starts with "no error yet".
                            m.entry("LAST_ERROR_SQLSTATE".into()).or_insert_with(|| "00000".into());
                            m.entry("SQLSTATE".into()).or_insert_with(String::new);
                            m.entry("ROW_COUNT".into()).or_insert_with(|| "0".into());
                            m.entry("LASTOID".into()).or_insert_with(String::new);
                            // psql's variables with hooks, as it starts.
                            for (name, value) in crate::psql_vars::DEFAULTS {
                                m.entry(name.into()).or_insert_with(|| value.into());
                            }
                        });

                        // Describe the session before announcing it:
                        // effects keyed on `session_id` (an explorer
                        // opened by `\d` fetching its table) run as soon
                        // as it's set, and read the extensions.
                        // `session_extensions` is read by the explorer
                        // through `ExplorerSession` to pick
                        // extension-dependent plane SQL.
                        tab.session_extensions.set(exts);
                        tab.session_password_used.set(password_used);
                        tab.session_ssl_library.set(ssl_library);
                        tab.session_numeric_locale.set(numeric_locale);
                        tab.current_db.set(db);
                        tab.session_version.set(sv);
                        tab.db_connection_id.set(if dbcid.is_empty() { None } else { Some(dbcid) });
                        tab.session_id.set(Some(sid.clone()));
                        tab.connecting.set(false);
                        // Fresh session — clear any stale flag from a prior
                        // noctua death, since this pane just got new IDs.
                        tab.session_stale.set(false);
                        if tab.kind == PaneKind::Shell && !tab.history_key.is_empty()
                            && tab.persisted_cmds.with_untracked(Option::is_none)
                        {
                            let entries = load_history(&tab.history_key);
                            tab.persisted_cmds.set(Some(entries.iter().map(|e| e.sql.clone()).collect()));
                        }
                        if tab.kind == PaneKind::SchemaExplorer {
                            let plane = plane_for(tab.db_type);
                            ws_introspect(ws, &sid, plane.list_schemas_sql().to_string(), "schemas".into());
                            // An explorer opened focused on a relation
                            // (`\d <name>`) fetches its details now. From
                            // this event, not an effect: the detail's
                            // signals must outlive any one run or render,
                            // as they do when a click creates them.
                            if let Some((schema, name)) = tab.explorer_selected.get_untracked()
                                .filter(|key| tab.explorer_table_details.with_untracked(|m| !m.contains_key(key)))
                            {
                                let cache = tab.explorer_table_details;
                                match tab.explorer_kind.get_untracked() {
                                    ObjectKind::Tables => {
                                        let session = crate::explorer::ExplorerSession::of(&tab);
                                        crate::explorer::ensure_table_detail_loaded(plane, &schema, &name, cache, ws, session);
                                    }
                                    ObjectKind::Views => {
                                        crate::explorer::ensure_view_detail_loaded(plane, &schema, &name, cache, ws, Some(sid.clone()));
                                    }
                                    _ => {}
                                }
                            }
                        }
                        if tab.kind == PaneKind::Shell {
                            // Eagerly populate the tab-completion table cache
                            // so the first Tab in a Table context lands a
                            // populated popup. Without this the user has to
                            // press Tab twice (first fires fetch, second
                            // reads the cache). One small query at connect
                            // beats a confusing double-tap UX every time.
                            let plane = plane_for(tab.db_type);
                            tab.completion_tables.set(LoadState::Loading);
                            ws_introspect(ws, &sid, plane.completion_tables_sql(), "completion_tables".into());
                        }
                        // This session replaces a lost one: say so as the
                        // CLI does, then carry on — mysql runs the
                        // statement that found the old one lost, psql the
                        // rest of the input (unless ON_ERROR_STOP).
                        tab.disconnected.set(false);
                        // A `\c`'s new session: the one it replaces closes
                        // now (its SessionClosed matches no tab), and the
                        // shell says where it is, as psql does.
                        if let Some(Switching { previous }) = tab.switching.get_untracked() {
                            tab.switching.set(None);
                            if let Some(old) = &previous {
                                ws_send(ws, &ClientMessage::Disconnect { session_id: old.clone() });
                            }
                            tab.txn_state.set(TxnState::Idle);
                            tab.querying.set(false);
                            let var = |name: &str| tab.variables.with_untracked(|v| v.get(name).cloned().unwrap_or_default());
                            let lines = plane_for(tab.db_type).connected_notice(
                                &tab.current_db.get_untracked(), &var("USER"), &var("HOST"), &var("PORT"), previous.is_some(),
                            );
                            // psql's `do_connect` says it unless `QUIET`.
                            if !lines.is_empty() && !tab.quiet() {
                                tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(lines)));
                                scroll_shell_to_bottom(&tab.id);
                            }
                        }
                        if let Some(reopening) = tab.reopening.get_untracked() {
                            tab.reopening.set(None);
                            let plane = plane_for(tab.db_type);
                            // A `connect` that named a database reads its
                            // names for completion first, and says so.
                            let mut lines = match reopening {
                                Reopening::Connect { named: true } => plane.completion_notice(),
                                _ => Vec::new(),
                            };
                            lines.extend(plane.reconnected_notice(
                                true,
                                &tab.db_connection_id.get_untracked().unwrap_or_default(),
                                &tab.current_db.get_untracked(),
                            ));
                            if !lines.is_empty() {
                                tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(lines)));
                            }
                            match reopening {
                                Reopening::Resend(sql) => crate::shell::send_statement(ws, tab.sent_sql, sid.clone(), sql),
                                Reopening::Meta { command, queries } => crate::shell::start_staged_meta(&tab, ws, command, queries),
                                Reopening::Reset => {
                                    if tab.on_error_stop.get_untracked() { crate::shell::drop_rest(&tab); }
                                    if !crate::shell::run_next_queued(&tab, ws) { tab.querying.set(false); }
                                }
                                Reopening::Connect { .. } => tab.querying.set(false),
                            }
                        }
                        // A command this shell was opened for (an
                        // explorer's action shell) runs now, through the
                        // same path as a typed statement.
                        if let Some(sql) = tab.startup_sql.get_untracked() {
                            tab.startup_sql.set(None);
                            let stmt = TypedStatement::new(sql, tab.delimiter.get_untracked());
                            crate::shell::run_in_shell(&tab, ws, stmt);
                        }
                        tab.kind == PaneKind::Shell
                    } else { false };
                    // Whoever created the pane decided whether it's in
                    // front; connecting doesn't change that. (It used
                    // to make every connecting pane active, so a
                    // reconnect stole focus and a background action
                    // shell would have too.)
                    if pane.get_untracked() != Pane::Shell { pane.set(Pane::Shell); }
                    if is_shell && active_pane_id.get_untracked().as_deref() == Some(client_tab_id.as_str()) {
                        focus_shell_input(&client_tab_id);
                    }
                }
                ServerMessage::SessionError { client_tab_id, message, error, .. } => {
                    let tab = shell_panes.get().get(&client_tab_id).cloned();
                    // A `\c` that couldn't connect: the session it had stays
                    // (psql's "Previous connection kept").
                    let switching = tab.as_ref().and_then(|t| t.switching.get_untracked());
                    if let (Some(tab), Some(Switching { previous })) = (&tab, switching) {
                        tab.switching.set(None);
                        tab.pending_warnings.set(Vec::new());
                        tab.querying.set(false);
                        let var = |name: &str| tab.variables.with_untracked(|v| v.get(name).cloned().unwrap_or_default());
                        let lines = plane_for(tab.db_type).connect_failed(
                            &var("HOST"), &var("PORT"), error.as_ref(), &message, previous.is_some(),
                        );
                        tab.shell_history.update(|h| h.push(ShellEntry::CliError { lines, message }));
                        scroll_shell_to_bottom(&tab.id);
                        return;
                    }
                    if let Some(tab) = tab {
                        tab.connecting.set(false);
                        tab.querying.set(false);
                        tab.reopen_retries.set(0);
                        let reopening = tab.reopening.get_untracked();
                        tab.reopening.set(None);
                        let rest = tab.pending_stmts.get_untracked();
                        tab.pending_stmts.set(Vec::new());
                        let gexec = tab.gexec.get_untracked();
                        tab.gexec.set(None);
                        tab.held_notes.set(Vec::new());
                        // What's left to read goes on — its statements "not
                        // connected", as psql's — unless ON_ERROR_STOP.
                        if tab.on_error_stop.get_untracked() { tab.resume_input.set(None); }
                        // A lost session that couldn't be replaced: the
                        // shell now knows it has no connection.
                        if reopening.is_some() { tab.disconnected.set(true); }
                        let plane = plane_for(tab.db_type);
                        match (reopening, plane.lost_connection()) {
                            // psql: "Attempting reset: Failed." — not why —
                            // and the rest of the input "not connected".
                            // A `\gexec`'s statements, then its own status,
                            // then the rest — as psql's ExecQueryTuples, then
                            // PrintQueryResult.
                            (Some(Reopening::Reset), LostConnection::ResetAtOnce { not_connected, .. }) => {
                                let unrun = if tab.on_error_stop.get_untracked() { 0 } else { rest.len() };
                                let generated = gexec.as_ref().map_or(0, |gexec| rest.len().saturating_sub(gexec.rest)).min(unrun);
                                let not_connected = |n: usize| std::iter::repeat_n(not_connected.to_string(), n);
                                let mut lines = plane.reconnected_notice(false, "", "");
                                lines.extend(not_connected(generated));
                                let first = lines.first().cloned().unwrap_or_default();
                                tab.shell_history.update(|h| {
                                    h.push(ShellEntry::CliError { lines, message: first });
                                    h.extend(gexec.as_ref().and_then(|gexec| gexec.trailer.clone()));
                                    if unrun > generated {
                                        let lines: Vec<String> = not_connected(unrun - generated).collect();
                                        h.push(ShellEntry::CliError { message: lines[0].clone(), lines });
                                    }
                                });
                                // Its statements unrun — psql's SendQuery
                                // fails each, "not connected" — it failed.
                                if let Some(gexec) = gexec {
                                    let failed = gexec.failed || generated > 0;
                                    shell::set_result_variables(&tab, GexecPending { failed, ..gexec }.outcome());
                                }
                            }
                            // The mysql CLI's: the server's error — and, when
                            // it was reconnecting for a statement, its
                            // `Can't connect to the server`, the statement
                            // unrun. A connection it couldn't make at all is
                            // noctua's to word (mysql's `ERROR 2003` isn't
                            // ported yet).
                            (Some(reopening), LostConnection::OnNextStatement { .. }) => {
                                let mut lines = plane.connect_failed("", "", error.as_ref(), &message, false);
                                if !matches!(reopening, Reopening::Connect { .. }) {
                                    lines.extend(plane.reconnected_notice(false, "", ""));
                                }
                                tab.shell_history.update(|h| h.push(ShellEntry::CliError { lines, message }));
                            }
                            _ => tab.shell_history.update(|h| h.push(ShellEntry::Error(message))),
                        }
                    }
                    pane.set(Pane::Shell);
                    if let Some(ref pid) = active_pane_id.get() {
                        scroll_shell_to_bottom(pid);
                    }
                }
                ServerMessage::SessionClosed { session_id: sid } => {
                    // Server-initiated session death — `pg_terminate_backend`,
                    // MySQL `KILL`, idle-in-transaction timeout, replica
                    // failover, etc. Pre-fix this destroyed the pane: removed
                    // from `shell_panes`, ripped from the workspace layout,
                    // history / draft / schema state all vanished. A DBA
                    // accidentally killing their own session via the Activity
                    // pane lost the very workspace that was showing them
                    // WHICH PID they meant to kill.
                    //
                    // Post-fix: the SESSION is gone but the PANE survives.
                    // Clear `session_id` so a future query attempt doesn't
                    // route to noctua's now-empty session map, mark
                    // `session_stale = true` so the reconnect banner shows,
                    // and run the shared in-flight cleanup so spinners /
                    // watch timers / pending statements don't rot forever.
                    //
                    // Multi-tab leaves are now safe automatically: we don't
                    // touch `leaf.tabs`, so finding 2.3 ("dead tab pill on
                    // multi-tab leaf") goes away as a side effect of not
                    // destroying.
                    //
                    // Audit reference: findings 2.2, 2.3, 2.8.
                    let found = shell_panes.get_untracked().values()
                        .find(|t| t.session_id.get_untracked().as_deref() == Some(&sid))
                        .cloned();
                    if let Some(pane_tab) = found {
                        let pane_id = pane_tab.id.clone();
                        // A shell carries on as its CLI does: mysql runs the
                        // statement that found the session dead (its
                        // QueryError said so) again in a new one, the rest
                        // of its batch after it; psql resets the connection
                        // at once and goes on with the rest of the input.
                        let reopening = pane_tab.reopening.get_untracked();
                        let queued = pane_tab.pending_stmts.get_untracked();
                        let gexec = pane_tab.gexec.get_untracked();
                        reset_in_flight_signals(&pane_id);
                        pane_tab.session_id.set(None);
                        pane_tab.session_stale.set(true);
                        if pane_tab.kind == PaneKind::Shell {
                            match (plane_for(pane_tab.db_type).lost_connection(), reopening) {
                                (LostConnection::OnNextStatement { .. }, Some(Reopening::Resend(sql))) => {
                                    pane_tab.pending_stmts.set(queued);
                                    pane_tab.gexec.set(gexec);
                                    pane_tab.querying.set(true);
                                    shell::deliver(&pane_tab, ws, shell::SendTo::Reopen, sql);
                                }
                                // Only when a command found it lost (its
                                // QueryError marked the reset): psql learns
                                // of a lost connection from a command, not
                                // from background work — the next statement
                                // finds out then (`SendTo::Reset`).
                                (LostConnection::ResetAtOnce { .. }, Some(Reopening::Reset)) => {
                                    pane_tab.pending_stmts.set(queued);
                                    pane_tab.gexec.set(gexec);
                                    pane_tab.querying.set(true);
                                    pane_tab.reopening.set(Some(Reopening::Reset));
                                    shell::reconnect(&pane_tab, ws);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                // The session's transaction state from the server
                // (Postgres), just before its command's reply: the TXN
                // badge and psql's `%x` prompt follow it.
                ServerMessage::TransactionStatus { session_id: sid, state } => {
                    let tab = shell_panes.with_untracked(|m| m.values()
                        .find(|t| t.session_id.get_untracked().as_deref() == Some(&sid))
                        .cloned());
                    if let Some(tab) = tab.filter(|t| t.txn_state.get_untracked() != state) {
                        tab.txn_state.set(state);
                    }
                }
                // Server notice streamed while a shell query runs; lands
                // between the query's prompt line and its result, as in
                // psql — each `\watch` run's too.
                ServerMessage::Notice { session_id: sid, severity, message } => {
                    let tab = shell_panes.with_untracked(|m| m.values()
                        .find(|t| t.session_id.get_untracked().as_deref() == Some(&sid))
                        .cloned());
                    if let Some(tab) = tab {
                        tab.shell_history.update(|h| h.push(ShellEntry::Notice { severity, message }));
                        scroll_shell_to_bottom(&tab.id);
                    }
                }
                ServerMessage::QueryResult { session_id: sid, columns, types, rows, exec_time_us, status } => {
                    let rows_affected = status.rows_affected;
                    let tab = shell_panes.get().values()
                        .find(|t| t.session_id.get().as_deref() == Some(&sid))
                        .cloned();
                    if let Some(tab) = tab {
                        tab.reopen_retries.set(0);
                        // A statement psql wrapped: its tag says how its
                        // savepoint is closed.
                        shell::wrapped_answered(&tab, status.tag.clone());
                        // A `\watch` run: printed as psql prints it — if
                        // its watch is still the one running.
                        if let Some(run_of) = tab.watch_pending.get_untracked() {
                            tab.watch_pending.set(None);
                            if tab.watch_sql.get_untracked().is_some() && run_of == tab.watch_generation.get_untracked() {
                                let has_cols = !columns.is_empty();
                                // Emit a notification for this run if the
                                // pane isn't active: watch runs go through
                                // this branch, not the normal flow below.
                                if active_pane_id.get_untracked().as_deref() != Some(&tab.id) {
                                    push_notification(
                                        notifications,
                                        connections,
                                        &tab,
                                        NotificationEvent::Ok { rows_affected, exec_time_us, has_cols },
                                    );
                                }
                                let rows_returned = has_cols.then_some(rows.len());
                                let entries = shell::result_entry(
                                    &tab, shell::Rows { columns, types, rows, localized: true }, status, exec_time_us,
                                    tab.expanded.get_untracked() == Expanded::On,
                                    shell::ResultTitle::Watch(tab.watch_title.get_untracked()),
                                );
                                shell::print_watch_run(&tab, entries, rows_returned);
                                apply_buffer_threshold(&tab.conn_id, tab.shell_history);
                            }
                        } else {
                        finish_explorer_action(&tab, shell_panes, ws);
                        let vert = tab.pending_vertical.get_untracked();
                        tab.pending_vertical.set(false);
                        // A psql list command's reply: its title, or what
                        // psql says when no rows come back. Always taken —
                        // the next statement mustn't inherit it.
                        let mut list = tab.pending_list.get_untracked();
                        tab.pending_list.set(None);
                        // A list psql runs a second query after, for its
                        // footers (`\dn <pattern>`'s publications): its
                        // rows wait for that query's reply…
                        if let Some(reply) = list.clone().filter(|l| l.footers.is_some() && l.held.is_none()) {
                            let held = HeldList { columns, types, rows, status, exec_time_us, vertical: vert };
                            shell::hold_list(&tab, ws, reply, held);
                            return;
                        }
                        // …which brings them back, with the footers its
                        // rows make.
                        let (columns, types, rows, status, exec_time_us, vert, footers) = match shell::take_held_list(&mut list, &rows) {
                            Some((held, footers)) => (held.columns, held.types, held.rows, held.status, held.exec_time_us, held.vertical, footers),
                            None => (columns, types, rows, status, exec_time_us, vert, Vec::new()),
                        };
                        let rows_affected = status.rows_affected;
                        let warning_count = status.warnings;
                        let has_cols = !columns.is_empty();
                        // ROW_COUNT semantics: rows.len() for SELECT-
                        // like results; rows_affected for DDL / DML
                        // without RETURNING. Match psql's `:ROW_COUNT`.
                        let row_count = if has_cols { rows.len() as u64 } else { rows_affected };
                        // Clear ERROR (the prior query, if any,
                        // succeeded). LAST_ERROR_MESSAGE stays — psql's
                        // semantics keep the last error around.
                        // Not for a meta-command's own SQL (psql's
                        // SetResultVariables leaves those out).
                        // (An empty statement the CLI answered itself —
                        // mysql's "No query specified" — ran nothing: it's
                        // not the one.)
                        let from_meta = tab.shell_history.with_untracked(|h| h.iter().rev()
                            .find(|e| matches!(e, ShellEntry::Query { sql, meta, .. } if !(sql.is_empty() && *meta)))
                            .is_some_and(|e| matches!(e, ShellEntry::Query { meta: true, .. })));
                        if !from_meta {
                            shell::set_result_variables(&tab, shell::CommandOutcome::Done { row_count });
                        }

                        // ── \gexec / \gset post-action routing ────────────
                        // The shell sets `pending_post_action` when the
                        // user fired `\gexec` or `\gset [prefix]`. We
                        // consume it here BEFORE the normal Table/Vert/
                        // Info entry computation: for \gexec we queue
                        // each non-NULL cell into `pending_stmts` (the
                        // existing multi-statement continuation at the
                        // end of this block will submit the first one);
                        // for \gset we validate single-row + bind cols
                        // into psql variables. Either way we emit a
                        // MetaOutput summary in place of the normal
                        // result rendering and skip the rest of this
                        // branch.
                        let post = tab.pending_post_action.get_untracked();
                        tab.pending_post_action.set(None);
                        if let Some(pa) = post {
                            // Each arm pushes its own ShellEntry(s)
                            // — most produce MetaOutput, Crosstabview
                            // pushes a pivoted Table. The shared
                            // post-action cleanup (threshold check,
                            // querying flag, sidebar status, txn
                            // transition, multi-statement continuation,
                            // early return) follows the match.
                            match pa {
                                // A statement without columns: \gexec,
                                // \gset and \crosstabview have nothing to
                                // work on and say nothing of it, as psql's;
                                // it prints what it prints (its tag, the
                                // time).
                                PostAction::Gexec | PostAction::Gset { .. } | PostAction::Crosstabview { .. } if !has_cols => {
                                    let entry = shell::columnless_result(&tab, status, exec_time_us);
                                    tab.shell_history.update(|h| h.push(entry));
                                }
                                PostAction::Gexec => {
                                    // psql's \gexec says nothing of its own:
                                    // each non-NULL cell runs, row by row,
                                    // as typed — `:var`s and all (psql
                                    // doesn't substitute them) — before the
                                    // rest of the input; a blank one too,
                                    // which the server answers with nothing
                                    // (a `Time:` under \timing, a `BEGIN`
                                    // before it under AUTOCOMMIT off) …
                                    let generated: Vec<TypedStatement> = rows.iter().flatten().flatten()
                                        .map(|sql| TypedStatement::new(sql.clone(), ""))
                                        .collect();
                                    let rest = tab.pending_stmts.with_untracked(Vec::len);
                                    tab.pending_stmts.update(|q| { q.splice(0..0, generated); });
                                    // … then its own statement's status once
                                    // they've run — a RETURNING's tag, the
                                    // time — and the result variables from
                                    // it (`shell::finish_gexec`).
                                    tab.gexec.set(Some(GexecPending {
                                        rest,
                                        trailer: shell::after_unprinted_rows(&tab, &status, exec_time_us),
                                        row_count,
                                        failed: false,
                                    }));
                                }
                                PostAction::Gset { prefix } => {
                                    // psql's \gset: silent when it binds the
                                    // row, its errors when it can't.
                                    let error = |message: String| ShellEntry::CliError { lines: vec![message.clone()], message };
                                    let said: Option<ShellEntry> = if rows.is_empty() {
                                        Some(error("no rows returned for \\gset".into()))
                                    } else if rows.len() > 1 {
                                        Some(error("more than one row returned for \\gset".into()))
                                    } else {
                                        use crate::psql_vars::{classify, PsqlVarKind};
                                        let mut notes = Vec::new();
                                        let mut refused = None;
                                        for (col, val) in columns.iter().zip(rows[0].iter()) {
                                            let name = format!("{}{}", prefix, col);
                                            if !crate::parser::valid_variable_name(&name) {
                                                refused = Some(error(format!("invalid variable name: \"{name}\"")));
                                                break;
                                            }
                                            match classify(&name) {
                                                Some(PsqlVarKind::ReadOnly) => {
                                                    notes.push(format!("{}: read-only (skipped).", name));
                                                    continue;
                                                }
                                                Some(PsqlVarKind::Honored) => {
                                                    notes.push(format!(
                                                        "{}: skipped (use \\set; \\gset can't toggle behavioral signals).",
                                                        name,
                                                    ));
                                                    continue;
                                                }
                                                _ => {}
                                            }
                                            // A NULL unsets the variable, as in psql.
                                            tab.variables.update(|m| {
                                                match val {
                                                    Some(val) => { m.insert(name.clone(), val.clone()); }
                                                    None => { m.remove(&name); }
                                                }
                                            });
                                        }
                                        refused.or_else(|| (!notes.is_empty()).then_some(ShellEntry::MetaOutput(notes)))
                                    };
                                    if let Some(said) = said {
                                        tab.shell_history.update(|h| h.push(said));
                                    }
                                    // Then, stored or not, what follows the
                                    // rows (psql's `PrintQueryResult`): a
                                    // RETURNING's tag, and the time.
                                    let after = shell::after_unprinted_rows(&tab, &status, exec_time_us);
                                    tab.shell_history.update(|h| h.extend(after));
                                }
                                // `\gdesc`'s names and types, printed as any
                                // result, timed from the describe on.
                                PostAction::Gdesc { describe_us } => {
                                    let entries = shell::result_entry(
                                        &tab, shell::Rows { columns, types, rows, localized: true }, status,
                                        describe_us + exec_time_us, vert, shell::ResultTitle::Pset,
                                    );
                                    tab.shell_history.update(|h| h.extend(entries));
                                }
                                PostAction::Crosstabview { col_v, col_h, col_d, sortcol_h } => {
                                    // Pivot the result client-side
                                    // and push the transformed grid
                                    // as a Table. Pivot errors render
                                    // as a MetaOutput. Pivoted or not, a
                                    // RETURNING's tag and the time follow
                                    // (psql's `PrintQueryResult`).
                                    let pivoted = crate::psql_crosstab::pivot_table(
                                        &columns, &rows,
                                        col_v.as_deref(), col_h.as_deref(),
                                        col_d.as_deref(), sortcol_h.as_deref(),
                                        &tab.null_display.get_untracked(),
                                    );
                                    match pivoted {
                                        Err(msg) => {
                                            let after = shell::after_unprinted_rows(&tab, &status, exec_time_us);
                                            tab.shell_history.update(|h| {
                                                h.push(ShellEntry::MetaOutput(vec![msg]));
                                                h.extend(after);
                                            });
                                        }
                                        Ok((out_cols, out_rows, warnings)) => {
                                            if !warnings.is_empty() {
                                                tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(warnings)));
                                            }
                                            // Printed as psql prints any result.
                                            let (timed, after) = shell::after_rows(&tab, &status, exec_time_us);
                                            let types = crate::psql_crosstab::pivot_types(
                                                &columns, &types, col_v.as_deref(), col_h.as_deref(), col_d.as_deref(), out_cols.len(),
                                            );
                                            let entry = shell::rows_entry(
                                                &tab, shell::Rows { columns: out_cols, types, rows: out_rows, localized: false },
                                                // `\crosstabview` is psql's: no server warning count.
                                                0, exec_time_us, timed, vert, shell::ResultTitle::Pset,
                                            );
                                            tab.shell_history.update(|h| {
                                                h.push(entry);
                                                h.extend(after);
                                            });
                                        }
                                    }
                                }
                            }
                            apply_buffer_threshold(&tab.conn_id, tab.shell_history);
                            tab.querying.set(false);
                            tab.last_status.set(PaneStatus::Ok(format!("{} ms", exec_time_us / 1000)));
                            scroll_shell_to_bottom(&tab.id);

                            // Fire the multi-statement continuation
                            // (\gexec relies on this to flush the
                            // queued cells).
                            shell::run_next_queued(&tab, ws);
                            return;
                        }
                        let switched = (!has_cols).then(|| crate::plane::plane_for(tab.db_type)
                            .switches_database(&tab.sent_sql.get_untracked().unwrap_or_default())).flatten();
                        let entries = if let Some(none) = list.as_ref().and_then(|l| l.none.clone()).filter(|_| rows.is_empty()) {
                            vec![ShellEntry::MetaOutput(vec![none])]
                        } else if let Some((database, said)) = switched {
                            // The CLI's own `use`: its words, and the
                            // database it's on now — for the tab's label,
                            // `:DBNAME`, and where a reconnect goes. Onto
                            // another database it reads its names for
                            // completion first, and says so (`com_use`'s
                            // `build_completion_hash`).
                            let notice = (database != tab.current_db.get_untracked())
                                .then(|| plane_for(tab.db_type).completion_notice())
                                .filter(|lines| !lines.is_empty());
                            tab.variables.update(|m| { m.insert("DBNAME".into(), database.clone()); });
                            tab.current_db.set(database);
                            notice.map(ShellEntry::MetaOutput).into_iter()
                                .chain([ShellEntry::MetaOutput(vec![said.into()])])
                                .collect()
                        } else {
                            let title = match list {
                                Some(list) => shell::ResultTitle::List { title: list.title, counts_rows: list.counts_rows, footers },
                                None => shell::ResultTitle::Pset,
                            };
                            shell::result_entry(&tab, shell::Rows { columns, types, rows, localized: true }, status, exec_time_us, vert, title)
                        };
                        tab.shell_history.update(|h| h.extend(entries));
                        // After each query result lands, check the
                        // pane's history against the connection's
                        // buffer threshold. Over → drop the oldest
                        // block. See `apply_buffer_threshold` above.
                        apply_buffer_threshold(&tab.conn_id, tab.shell_history);
                        // Update sidebar status
                        tab.querying.set(false);
                        let summary = if !has_cols {
                            let r = if rows_affected == 1 { "1 row affected".to_string() }
                                    else { format!("{} rows affected", rows_affected) };
                            format!("{} · {}ms", r, exec_time_us / 1000)
                        } else {
                            let r = if rows_affected == 1 { "1 row".to_string() }
                                    else { format!("{} rows", rows_affected) };
                            format!("{} · {}ms", r, exec_time_us / 1000)
                        };
                        tab.last_status.set(PaneStatus::Ok(summary));
                        // Emit a notification when the pane isn't the active
                        // one. `\watch` ticks are intentionally INCLUDED so
                        // the bell reflects real ongoing activity; coalescing
                        // watch into one updating notification is future work.
                        if active_pane_id.get_untracked().as_deref() != Some(&tab.id) {
                            push_notification(
                                notifications,
                                connections,
                                &tab,
                                NotificationEvent::Ok { rows_affected, exec_time_us, has_cols },
                            );
                        }
                        scroll_shell_to_bottom(&tab.id);

                        // Schema-cache invalidation: if the query looks like DDL,
                        // refresh any Schema Explorer pane on the same connection
                        // so new/dropped objects appear without a manual refresh.
                        //
                        // Pre-fix this lived as a trim+uppercase+starts_with
                        // chain that was fooled by `/* migration */ CREATE
                        // TABLE x` (started with `/`, not `CREATE`) and
                        // misclassified `ALTER TABLE x ADD COLUMN y` as
                        // generically destructive. Now an AST-based
                        // classifier handles both. Audit finding 1.7.
                        // The statement this result is for: the SQL sent
                        // (not the scrollback's newest — a batch puts all
                        // its statements there before the first answers).
                        let last_sql = tab.sent_sql.get_untracked().unwrap_or_default();
                        let intent = crate::sql_inspect::inspect_statement(
                            &last_sql,
                            crate::plane::plane_for(tab.db_type).sql_dialect(),
                        );
                        // Parse-failure policy for schema cache: invalidate
                        // anyway. Re-introspect is cheap; the false-positive
                        // cost is one extra catalog round-trip. Cheaper than
                        // a missed invalidation that leaves the explorer
                        // staring at stale rows.
                        let is_ddl = intent.invalidates_schema || intent.parse_failed;
                        let is_role_ddl = intent.invalidates_roles;
                        if is_ddl || is_role_ddl {
                            let conn = tab.conn_id.clone();
                            for ep in shell_panes.get().values() {
                                if ep.kind == PaneKind::SchemaExplorer && ep.conn_id == conn {
                                    // Role-affecting DDL: refresh the Roles
                                    // list. Re-fire immediately if the user
                                    // is currently viewing it (the lazy-fetch
                                    // effect doesn't subscribe to admin_roles
                                    // so a state reset alone wouldn't help);
                                    // otherwise just reset to NotLoaded so
                                    // the next visit picks up fresh data.
                                    if is_role_ddl {
                                        let plane = crate::plane::plane_for(ep.db_type);
                                        let viewing_roles = ep.explorer_admin_active.get_untracked()
                                            && ep.explorer_admin_area.get_untracked() == Some(AdminArea::Roles);
                                        if viewing_roles {
                                            if let (Some(esid), Some(sql)) =
                                                (ep.session_id.get_untracked(), plane.list_roles_sql())
                                            {
                                                ep.explorer_admin_roles.set(LoadState::Loading);
                                                ws_introspect(ws, &esid, sql.to_string(), "admin_roles".into());
                                            }
                                        } else {
                                            ep.explorer_admin_roles.set(LoadState::NotLoaded);
                                        }
                                        // Privileges of the currently-selected
                                        // role may have shifted too — reset
                                        // unconditionally; the lazy-fetch
                                        // effect refires when the user revisits
                                        // the Privileges tab. Same reasoning
                                        // for the resolver result — without
                                        // this, the AffectedObjects sub-view
                                        // shows the pre-DDL object list.
                                        ep.explorer_admin_role_privileges.set(LoadState::NotLoaded);
                                        ep.explorer_admin_role_affected_objects.set(LoadState::NotLoaded);
                                    }
                                    if !is_ddl { continue; }
                                    let plane = crate::plane::plane_for(ep.db_type);
                                    let esid_opt = ep.session_id.get_untracked();
                                    if let Some(esid) = &esid_opt {
                                        ws_introspect(ws, esid, plane.list_schemas_sql().to_string(), "schemas".into());
                                    }
                                    // Invalidate cached ERD neighbour-column maps
                                    // for every loaded TableDetail in this pane.
                                    // Setting back to NotLoaded triggers the lazy
                                    // fetch effect to refire next time the user
                                    // has Keys-Only off.
                                    ep.explorer_table_details.update(|m| {
                                        for detail in m.values_mut() {
                                            detail.erd_neighbour_columns.set(LoadState::NotLoaded);
                                            // The data tab's cached page may be stale after DDL
                                            // (column added/removed/renamed); reset so the next
                                            // view refetches.
                                            detail.data_page.set(LoadState::NotLoaded);
                                            // The privileges matrix may also be stale —
                                            // GRANT / REVOKE / object rename / object drop
                                            // all invalidate it. Lazy-fetch on next tab open.
                                            detail.object_grants.set(LoadState::NotLoaded);
                                        }
                                    });
                                    // The table on screen refetches everything now,
                                    // in place; the other cached tables are marked
                                    // stale and refetch when next opened (see
                                    // `ensure_table_detail_loaded`), so one DDL
                                    // doesn't fire queries for every table ever
                                    // opened in this pane.
                                    let on_screen = ep.explorer_selected.get_untracked();
                                    let cached: Vec<((String, String), TableDetail)> = ep.explorer_table_details
                                        .with_untracked(|m| m.iter().map(|(k, v)| (k.clone(), *v)).collect());
                                    for (key, detail) in cached {
                                        if on_screen.as_ref() == Some(&key) {
                                            crate::explorer::refresh_table_detail(
                                                plane, &key.0, &key.1, detail, ws,
                                                crate::explorer::ExplorerSession::of(ep),
                                            );
                                        } else {
                                            detail.stale.set(true);
                                        }
                                    }
                                }
                                // Invalidate the tab-completion table + column
                                // caches on every Shell pane sharing this
                                // connection. The next Tab needing tables fires
                                // a fresh fetch; the next `.` keystroke after a
                                // table name fires a fresh column fetch.
                                if is_ddl && ep.kind == PaneKind::Shell && ep.conn_id == conn {
                                    ep.completion_tables.set(LoadState::NotLoaded);
                                    ep.completion_columns.update(|m| m.clear());
                                }
                            }
                        }

                        // The rest of the input, one statement at a time —
                        // after the server's warnings, when the CLI shows
                        // them (mysql's `warnings`).
                        if !shell::fetch_warnings(&tab, ws, warning_count, None) {
                            shell::run_next_queued(&tab, ws);
                        }
                        } // else (normal, non-watch result)
                    }
                }
                ServerMessage::QueryError { session_id: sid, message, error, connection_lost } => {
                    let tab = shell_panes.get().values()
                        .find(|t| t.session_id.get().as_deref() == Some(&sid))
                        .cloned();
                    // The statement found its session dead — the connection
                    // failed, not the server — on a plane that reconnects
                    // like its CLI: nothing to show yet; the SessionClosed
                    // that follows runs it again in a new session. The
                    // mysql CLI does so on its own lost-connection errors
                    // only (never after a server error such as a KILL's),
                    // and twice at most.
                    if let Some(t) = tab.as_ref().filter(|t| connection_lost && error.is_none()
                        && t.kind == PaneKind::Shell
                        && t.sent_sql.with_untracked(Option::is_some)
                        && t.watch_pending.get_untracked().is_none()
                        && t.reopen_retries.get_untracked() < 2
                        && matches!(plane_for(t.db_type).lost_connection(), LostConnection::OnNextStatement { .. }))
                    {
                        t.reopen_retries.update(|n| *n += 1);
                        t.reopening.set(t.sent_sql.get_untracked().map(Reopening::Resend));
                        return;
                    }
                    if let Some(t) = &tab {
                        t.reopen_retries.set(0);
                        shell::mark_gexec_failed(t);
                        // A command whose query failed says no more
                        // (psql's `extra argument … ignored`), and its
                        // list title isn't the next result's.
                        t.pending_warnings.set(Vec::new());
                        t.pending_list.set(None);
                    }
                    if let Some(tab) = tab {
                        // A `\watch` run that failed: the error, and the
                        // watch is over, as psql's is.
                        if let Some(run_of) = tab.watch_pending.get_untracked() {
                            tab.watch_pending.set(None);
                            if tab.watch_sql.get_untracked().is_some() && run_of == tab.watch_generation.get_untracked() {
                                if active_pane_id.get_untracked().as_deref() != Some(&tab.id) {
                                    push_notification(
                                        notifications,
                                        connections,
                                        &tab,
                                        NotificationEvent::Err { message: message.clone() },
                                    );
                                }
                                // Kept for \errverbose; the result variables
                                // stay as they were, as psql's do for a
                                // \watch run (`is_watch`).
                                let mut entry = shell::command_error(&tab, message, error, tab.watch_sql.get_untracked(), false);
                                // A run that found the session lost: psql's
                                // reset, as for any command (CheckConnection)
                                // — busy until it's done.
                                let resetting = connection_lost && shell::resets_lost(&tab, &mut entry);
                                tab.shell_history.update(|h| h.push(entry));
                                shell::stop_watch(&tab);
                                if resetting {
                                    tab.querying.set(true);
                                }
                                scroll_shell_to_bottom(&tab.id);
                            }
                        } else {
                        finish_explorer_action(&tab, shell_panes, ws);
                        // Clear any pending \gexec / \gset post-action.
                        // An errored query produced no result rows to
                        // iterate over, so the post-action would either
                        // see stale state or mis-fire on the NEXT
                        // successful query. Drop it.
                        tab.pending_post_action.set(None);

                        // The statement that failed: marked in the
                        // scrollback, its error shown as the CLI shows it.
                        let queued = tab.pending_stmts.with_untracked(Vec::len);
                        let sent = tab.sent_sql.get_untracked();
                        let failed = tab.shell_history.with_untracked(|h| shell::failed_command(h, queued, sent.as_deref()));
                        // Not the server's error: the connection itself
                        // failed, so there's nowhere to send the rest —
                        // unless the CLI resets it first (psql: the
                        // SessionClosed that follows does, and goes on).
                        let connection_failed = error.is_none();
                        let error_number = error.as_ref().map(|e| e.number.unwrap_or(0));
                        let mut entry = shell::command_error(&tab, message.clone(), error, failed.query, !failed.meta);
                        let resetting = connection_lost && shell::resets_lost(&tab, &mut entry);
                        tab.shell_history.update(|h| {
                            if let Some(ShellEntry::Query { errored, .. }) = failed.entry.and_then(|at| h.get_mut(at)) {
                                *errored = true;
                            }
                            h.push(entry);
                        });
                        // Same threshold check as the QueryResult path.
                        apply_buffer_threshold(&tab.conn_id, tab.shell_history);
                        // Update sidebar status — show first line of error
                        // (still busy while psql's reset runs).
                        tab.querying.set(resetting);
                        let snippet = message.lines().next().unwrap_or(&message).to_string();
                        tab.last_status.set(PaneStatus::Err(snippet));
                        // Emit a notification when the pane isn't active.
                        // The error message travels on the Notification.
                        if active_pane_id.get_untracked().as_deref() != Some(&tab.id) {
                            push_notification(
                                notifications,
                                connections,
                                &tab,
                                NotificationEvent::Err { message: message.clone() },
                            );
                        }
                        scroll_shell_to_bottom(&tab.id);
                        // The rest of a batch: both CLIs keep going past an
                        // error — psql's ON_ERROR_STOP is off by default,
                        // the mysql CLI has no such switch — and say
                        // nothing more (the server's errors, "current
                        // transaction is aborted" included, do). With
                        // ON_ERROR_STOP on, psql drops the rest silently —
                        // and so does a lost connection.
                        if !resetting {
                            if tab.on_error_stop.get_untracked() || connection_failed {
                                shell::drop_rest(&tab);
                            }
                            // After the server's warnings, when the CLI
                            // shows them (mysql's `warnings`): the error
                            // is one of them.
                            if !shell::fetch_warnings(&tab, ws, 0, error_number) {
                                shell::run_next_queued(&tab, ws);
                            }
                        }
                        } // else (normal, non-watch error)
                    }
                }

                ServerMessage::TestConnectionResult { success, message } => {
                    nc_test_status.set(Some((success, message)));
                }
                ServerMessage::DatabaseList { databases, client_tab_id } => {
                    // Route by `client_tab_id` echoed back from the request.
                    // Some(pid) → the Schema Explorer pane that asked.
                    // None → the connection-form modal's singleton dropdown.
                    //
                    // Previously this used "find any pane with
                    // explorer_db_pending = true" via HashMap iteration,
                    // which is order-nondeterministic. Two Schema Explorers
                    // firing concurrent ListDatabases against different
                    // connections could swap their lists silently. The
                    // explicit-id routing makes the response go to its
                    // originator deterministically.
                    match client_tab_id {
                        Some(pid) => {
                            if let Some(ep) = shell_panes.get_untracked().get(&pid).cloned() {
                                ep.explorer_databases.set(LoadState::Loaded(databases));
                                ep.explorer_db_pending.set(false);
                            }
                        }
                        None => nc_db_list.set(databases),
                    }
                }
                ServerMessage::DatabaseListError { message, client_tab_id } => {
                    // Symmetric routing to DatabaseList. On error: surface
                    // in the originating pane's dropdown via
                    // LoadState::Error (the rendering already handles this
                    // — error icon + tooltip with message), or in the
                    // connection-form modal via nc_test_status.
                    //
                    // Previously noctua emitted TestConnectionResult on
                    // ListDatabases failure, which routed only to
                    // nc_test_status — invisible to a Schema Explorer pane.
                    // The dropdown stayed in LoadState::Loading forever.
                    match client_tab_id {
                        Some(pid) => {
                            if let Some(ep) = shell_panes.get_untracked().get(&pid).cloned() {
                                ep.explorer_databases.set(LoadState::Error(message));
                                ep.explorer_db_pending.set(false);
                            }
                        }
                        None => nc_test_status.set(Some((false, message))),
                    }
                }
                ServerMessage::SchemaList { schemas, client_tab_id: _ } => {
                    // Only the connection form fires ListSchemas today; route
                    // straight to nc_schema_list. The client_tab_id field is
                    // reserved for a future per-pane schema dropdown.
                    nc_schema_list.set(schemas);
                }
                ServerMessage::SchemaListError { message, client_tab_id: _ } => {
                    // Surface in the same nc_test_status banner the database-
                    // list error uses, so the user sees one consistent
                    // error-channel inside the connection form.
                    nc_test_status.set(Some((false, message)));
                }

                ServerMessage::IntrospectResult { session_id: sid, tag, columns, rows } => {
                    // Every branch below reads `columns` / `rows` by
                    // CASE-INSENSITIVE COLUMN NAME, not position. Each
                    // plane's introspect SQL must AS-alias its output
                    // to match what the branch expects. See
                    // `.claude/rules/plane.md` for the full alias
                    // table (schemas / objects:* / status:* / columns:*
                    // / indexes:* / ddl:* / relancestors:* /
                    // reldescendants:* / partitions:* / erdcols:*).
                    // If a plane's SQL can't fit the shape, change the
                    // SQL — not this parser.
                    // ── Tab completion table cache ───────────────────
                    // Response carries two columns aliased `Schema_` and
                    // `Table_` (see plane.completion_tables_sql()). Group
                    // by schema and stash on the shell tab's cache so
                    // gather_candidates can read it on the next Tab.
                    // ── Tab completion column cache (one table at a time) ──
                    // Tag format: `completion_columns:{schema}|{table}`.
                    // Pipe separator (not dot) so schema names containing
                    // dots — unusual but legal — don't desync the parser.
                    if let Some(rest) = tag.strip_prefix("completion_columns:") {
                        if let Some((schema, table)) = rest.split_once('|') {
                            if let Some(tab) = shell_panes.get().values()
                                .find(|t| t.kind == PaneKind::Shell
                                       && t.session_id.get_untracked().as_deref() == Some(&sid))
                                .cloned()
                            {
                                let cols: Vec<String> = rows.into_iter()
                                    .filter_map(|r| r.into_iter().next())
                                    .filter(|s| !s.is_empty())
                                    .collect();
                                tab.completion_columns.update(|m| {
                                    m.insert(
                                        (schema.to_string(), table.to_string()),
                                        LoadState::Loaded(cols),
                                    );
                                });
                            }
                        }
                        return;
                    }
                    if tag == "completion_tables" {
                        if let Some(tab) = shell_panes.get().values()
                            .find(|t| t.kind == PaneKind::Shell
                                   && t.session_id.get_untracked().as_deref() == Some(&sid))
                            .cloned()
                        {
                            // Positional access — the plane's completion
                            // SQL guarantees (schema, table) in column
                            // order. Column-name matching would have
                            // failed silently on MySQL if `sqlx` or
                            // the driver rewrote the alias's casing.
                            let mut by_schema: HashMap<String, Vec<String>> = HashMap::new();
                            for r in &rows {
                                let s = r.first().cloned().unwrap_or_default();
                                let t = r.get(1).cloned().unwrap_or_default();
                                if !s.is_empty() && !t.is_empty() {
                                    by_schema.entry(s).or_default().push(t);
                                }
                            }
                            tab.completion_tables.set(LoadState::Loaded(by_schema));
                        }
                        return;
                    }
                    // ── \ef / \ev — route function/view source to textarea ──
                    // ── Pretty key-value display (the mysql CLI's \s / status) ──
                    if tag == "pretty_kv" {
                        if let Some(tab) = shell_panes.get().values()
                            .find(|t| t.kind == PaneKind::Shell
                                   && t.session_id.get_untracked().as_deref() == Some(&sid))
                            .cloned()
                        {
                            tab.querying.set(false);
                            let row = rows.first().cloned().unwrap_or_default();
                            let pairs: Vec<(String, String)> = columns.into_iter()
                                .zip(row)
                                .collect();
                            tab.shell_history.update(|h| h.push(ShellEntry::KeyValue(pairs)));
                            scroll_shell_to_bottom(&tab.id);
                        }
                        return;
                    }

                    if tag == "edit_to_input" {
                        // `\ef` / `\ev`: the definition arrived (`Src`,
                        // with the editor line in `Line`) — open it in
                        // the external editor, like `\e`. `Error` is the
                        // plane's psql-style refusal ("… is not a view").
                        if let Some(tab) = shell_for_session(&sid) {
                            tab.querying.set(false);
                            let (text, line) = match shell::parse_edit_lookup(&columns, &rows) {
                                // psql's own refusal (`"s.m" is not a
                                // view`), printed bare as psql prints it.
                                shell::EditLookup::Refused(error) => {
                                    tab.shell_history.update(|h| h.push(ShellEntry::CliError { lines: vec![error.clone()], message: error }));
                                    scroll_shell_to_bottom(&tab.id);
                                    return;
                                }
                                shell::EditLookup::Found { src, line } => (src, line),
                                shell::EditLookup::NotFound => (String::new(), None),
                            };
                            if text.is_empty() {
                                tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                                    "Object not found.".into()
                                ])));
                                scroll_shell_to_bottom(&tab.id);
                            } else if tab.external_edit.with_untracked(Option::is_some) {
                                // Another edit started while this one loaded.
                                tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                                    "An editor is already open for this shell; finish that edit first.".into()
                                ])));
                                scroll_shell_to_bottom(&tab.id);
                            } else {
                                // Whatever was typed while it loaded comes
                                // back if the edit doesn't.
                                let typed = tab.current_input.get_untracked();
                                shell::start_external_edit(&tab, ws, text, line, typed, true, String::new());
                            }
                        }
                        return;
                    }

                    // ── Activity pane routing ────────────────────────────
                    //
                    // Activity tags (sessions / blockers / locks / kill ack)
                    // target PaneKind::Activity rather than SchemaExplorer,
                    // so dispatch before the schema-explorer routing below.
                    if tag.starts_with("activity_") {
                        let ap = shell_panes.get().values()
                            .find(|t| t.kind == PaneKind::Activity
                                   && t.session_id.get_untracked().as_deref() == Some(&sid))
                            .cloned();
                        let Some(ap) = ap else { return; };
                        let idx = |name: &str| -> usize {
                            columns.iter()
                                .position(|c| c.eq_ignore_ascii_case(name))
                                .unwrap_or(usize::MAX)
                        };
                        let pick = |r: &Vec<String>, i: usize| -> String {
                            if i == usize::MAX { String::new() } else { r.get(i).cloned().unwrap_or_default() }
                        };
                        match tag.as_str() {
                            "activity_sessions" => {
                                let (ipid, iuser, iclient, idb, istate, itime, iquery) = (
                                    idx("Pid"), idx("User"), idx("Client"),
                                    idx("DB"), idx("State"), idx("Time"), idx("Query"),
                                );
                                let sessions: Vec<SessionRow> = rows.into_iter().map(|r| SessionRow {
                                    pid:    pick(&r, ipid),
                                    user:   pick(&r, iuser),
                                    client: pick(&r, iclient),
                                    db:     pick(&r, idb),
                                    state:  pick(&r, istate),
                                    time:   pick(&r, itime),
                                    query:  pick(&r, iquery),
                                }).collect();
                                ap.activity_sessions.set(LoadState::Loaded(sessions));
                                ap.activity_last_updated.set(Some(js_sys::Date::now()));
                            }
                            "activity_locks" => {
                                let (ipid, imode, iobj, igrant, idur) = (
                                    idx("Pid"), idx("Mode"), idx("Object"),
                                    idx("Granted"), idx("Duration"),
                                );
                                let locks: Vec<LockRow> = rows.into_iter().map(|r| LockRow {
                                    pid:      pick(&r, ipid),
                                    mode:     pick(&r, imode),
                                    object:   pick(&r, iobj),
                                    granted:  {
                                        let s = pick(&r, igrant).to_ascii_lowercase();
                                        matches!(s.as_str(), "1" | "true" | "t" | "yes")
                                    },
                                    duration: pick(&r, idur),
                                }).collect();
                                ap.activity_locks.set(LoadState::Loaded(locks));
                            }
                            "activity_blocks" => {
                                let (iw, ib) = (idx("Waiter_pid"), idx("Blocker_pid"));
                                let edges: Vec<BlockEdge> = rows.into_iter().map(|r| BlockEdge {
                                    waiter_pid:  pick(&r, iw),
                                    blocker_pid: pick(&r, ib),
                                }).collect();
                                ap.activity_blocks.set(LoadState::Loaded(edges));
                            }
                            "activity_kill" => {
                                // Kill-query ack: clear the confirm popover and bump
                                // the tick so the sessions list refreshes immediately.
                                ap.activity_confirm_kill.set(None);
                                ap.activity_tick.update(|t| *t += 1);
                            }
                            _ => {}
                        }
                        return;
                    }

                    // Route to the explorer pane whose session_id matches
                    let ep = shell_panes.get().values()
                        .find(|t| t.kind == PaneKind::SchemaExplorer
                               && t.session_id.get_untracked().as_deref() == Some(&sid))
                        .cloned();
                    let Some(ep) = ep else { return; };

                    if tag == "schemas" {
                        let schemas: Vec<String> = rows.into_iter()
                            .filter_map(|r| r.into_iter().next())
                            .collect();
                        // Auto-select schema, in order of preference:
                        //   1. The saved connection's `default_schema` (PG-only,
                        //      set via the New / Edit Connection form's schema
                        //      dropdown). Honoured case-insensitively.
                        //   2. The current database name (legacy fallback for
                        //      pre-default-schema connections — usually a no-op
                        //      on PG since DB and schema names differ, but kept
                        //      so existing behaviour isn't disturbed).
                        //   3. First non-system schema.
                        //   4. First schema, any.
                        if ep.explorer_schema.get_untracked().is_none() {
                            let conn = connections.get().into_iter()
                                .find(|c| c.id == ep.conn_id);
                            let default_schema = conn.as_ref()
                                .and_then(|c| c.default_schema.clone())
                                .filter(|s| !s.is_empty());
                            let conn_db = ep.current_db.get_untracked();
                            let fallback_db = conn.as_ref().map(|c| c.database.clone()).unwrap_or_default();
                            let preferred_db = if !conn_db.is_empty() { conn_db } else { fallback_db };
                            let system = plane_for(ep.db_type).system_schemas();
                            let pick = default_schema.as_deref()
                                .and_then(|ds| schemas.iter().find(|s| s.eq_ignore_ascii_case(ds)));
                            let pick = pick
                                .or_else(|| if !preferred_db.is_empty() {
                                    schemas.iter().find(|s| s.eq_ignore_ascii_case(&preferred_db))
                                } else {
                                    None
                                })
                                .or_else(|| schemas.iter().find(|s| !system.iter().any(|sys| sys.eq_ignore_ascii_case(s.as_str()))))
                                .or_else(|| schemas.first());
                            if let Some(s) = pick {
                                ep.explorer_schema.set(Some(s.clone()));
                            }
                        }
                        ep.explorer_schemas.set(LoadState::Loaded(schemas));

                    } else if let Some(rest) = tag.strip_prefix("objects:") {
                        // tag form: objects:<KindLabel>:<schema-or-*>
                        let (kind_lbl, _schema_part) = rest.split_once(':').unwrap_or((rest, ""));
                        // Ignore stale responses if the user has switched kinds.
                        if ep.explorer_kind.get_untracked().label() != kind_lbl {
                            return;
                        }
                        // First two columns are positional (schema, name) — matches
                        // every plane's list_sql. Remaining columns are peeled by
                        // case-insensitive name lookup so the PG-only Parent_schema
                        // / Parent_name partition-linkage columns can be lifted out
                        // of the user-visible `extras` (which drive the Engine /
                        // Rows / Updated / Comment header). Planes that don't emit
                        // those columns simply return `None` from the lookup.
                        let parent_schema_idx = columns.iter()
                            .position(|c| c.eq_ignore_ascii_case("parent_schema"));
                        let parent_name_idx = columns.iter()
                            .position(|c| c.eq_ignore_ascii_case("parent_name"));
                        let extras_idx: Vec<usize> = (2..columns.len())
                            .filter(|i| Some(*i) != parent_schema_idx
                                     && Some(*i) != parent_name_idx)
                            .collect();
                        let rows: Vec<ObjectRow> = rows.into_iter()
                            .map(|r| {
                                let schema = r.first().cloned().unwrap_or_default();
                                let name   = r.get(1).cloned().unwrap_or_default();
                                let parent_schema = parent_schema_idx
                                    .and_then(|i| r.get(i))
                                    .filter(|s| !s.is_empty())
                                    .cloned();
                                let parent_name = parent_name_idx
                                    .and_then(|i| r.get(i))
                                    .filter(|s| !s.is_empty())
                                    .cloned();
                                let extras: Vec<String> = extras_idx.iter()
                                    .map(|&i| r.get(i).cloned().unwrap_or_default())
                                    .collect();
                                ObjectRow { schema, name, parent_schema, parent_name, extras }
                            })
                            .collect();
                        ep.explorer_listing.set(LoadState::Loaded(rows));

                    } else if let Some(rest) = tag.strip_prefix("objddl:") {
                        // objddl:<KindLabel>:<schema>.<name>
                        if let Some((kind_lbl, qual)) = rest.split_once(':') {
                            if let Some((schema, name)) = qual.split_once('.') {
                                let kind = ObjectKind::ALL.iter().copied()
                                    .find(|k| k.label() == kind_lbl);
                                if let Some(kind) = kind {
                                    // SHOW CREATE returns the DDL in a column whose
                                    // name starts with "Create " (e.g. "Create View").
                                    // Triggers historically use "SQL Original Statement".
                                    let ddl_idx = columns.iter()
                                        .position(|c| c.starts_with("Create ")
                                                   || c == "SQL Original Statement")
                                        .unwrap_or(1);
                                    let ddl = rows.into_iter().next()
                                        .and_then(|r| r.get(ddl_idx).cloned())
                                        .unwrap_or_default();
                                    ep.explorer_object_ddls.update(|m| {
                                        m.insert(
                                            (kind, schema.to_string(), name.to_string()),
                                            LoadState::Loaded(ddl),
                                        );
                                    });
                                }
                            }
                        }

                    } else if let Some(rest) = tag.strip_prefix("columns:") {
                        if let Some((db_name, table_name)) = rest.split_once('.') {
                            let col_idx = |name: &str| -> usize {
                                columns.iter().position(|c| c.eq_ignore_ascii_case(name)).unwrap_or(0)
                            };
                            let fi = col_idx("Field");
                            let ti = col_idx("Type");
                            let ni = col_idx("Null");
                            let di = col_idx("Default");
                            let ei = col_idx("Extra");
                            let ci = col_idx("Comment");
                            let si = columns.iter().position(|c| c.eq_ignore_ascii_case("Source"));
                            let cols: Vec<ExplorerColumn> = rows.into_iter().map(|r| ExplorerColumn {
                                name:        r.get(fi).cloned().unwrap_or_default(),
                                col_type:    r.get(ti).cloned().unwrap_or_default(),
                                nullable:    r.get(ni).cloned().unwrap_or_default(),
                                default_val: r.get(di).cloned().unwrap_or_default(),
                                extra:       r.get(ei).cloned().unwrap_or_default(),
                                comment:     r.get(ci).cloned().unwrap_or_default(),
                                source:      si.and_then(|i| r.get(i).cloned()).unwrap_or_default(),
                            }).collect();
                            if let Some(detail) = find_table_detail(&ep, &format!("{}.{}", db_name, table_name)) {
                                detail.columns.set(LoadState::Loaded(cols));
                            }
                        }

                    } else if let Some(rest) = tag.strip_prefix("status:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            let col_idx = |name: &str| -> Option<usize> {
                                columns.iter().position(|c| c.eq_ignore_ascii_case(name))
                            };
                            let row = rows.into_iter().next().unwrap_or_default();
                            let pick = |name: &str| col_idx(name)
                                .and_then(|i| row.get(i).cloned())
                                .unwrap_or_default();
                            // `pick_opt` distinguishes "column wasn't
                            // emitted by the plane" (None) from
                            // "column was emitted but the value is
                            // blank" (Some("")). The four PG-only
                            // fields below (Owner, Parent_table,
                            // Parent_schema, Partition_bound) need
                            // this distinction because MySQL doesn't
                            // emit them at all — pre-fix the parser
                            // collapsed both cases to an empty string
                            // and a PG-to-MySQL user read this as
                            // "the GUI lost the Owner column" rather
                            // than "MySQL doesn't have table-level
                            // ownership." See `TableOverview` doc
                            // comments for the per-field semantics
                            // and audit cluster D finding 3.3.
                            let pick_opt = |name: &str| col_idx(name)
                                .map(|i| row.get(i).cloned().unwrap_or_default());
                            let data_len_raw  = pick("Data_length");
                            let index_len_raw = pick("Index_length");
                            let part_count_raw = pick("Partition_count");
                            // Storage factor: parse the plane's decimal
                            // string ("1.82") and derive display + tooltip
                            // + severity. None when the plane couldn't
                            // estimate (empty table / never analyzed /
                            // partitioned parent / non-InnoDB / etc).
                            // The tooltip's "actual bytes" comes from
                            // the plane's `Storage_actual_bytes` column
                            // so it matches what the factor was computed
                            // from (PG: relpages × block_size; MySQL:
                            // DATA + INDEX + DATA_FREE). Mismatching the
                            // factor against an unrelated byte count
                            // would make the breakdown lie.
                            let factor_raw  = pick("Storage_factor");
                            let actual_raw  = pick("Storage_actual_bytes");
                            let (storage_factor, storage_severity, storage_tooltip)
                                = match storage_factor::parse(&factor_raw) {
                                    None => (None, None, None),
                                    Some(f) => {
                                        let actual = actual_raw.parse::<u64>().unwrap_or(0);
                                        (
                                            Some(storage_factor::format_display(f)),
                                            Some(storage_factor::classify(f)),
                                            Some(storage_factor::build_tooltip(
                                                f, actual, storage_factor::TooltipTarget::Table,
                                            )),
                                        )
                                    }
                                };
                            // Cache hit ratio: parse the plane's decimal
                            // percentage string ("99.7") and derive display
                            // + severity + tooltip. None when the plane
                            // didn't emit a value (fresh table — no blocks
                            // accessed yet — or MySQL).
                            let cache_raw  = pick("Cache_hit_ratio");
                            let hits_raw   = pick("Heap_blks_hit");
                            let reads_raw  = pick("Heap_blks_read");
                            let (cache_hit_ratio, cache_hit_severity, cache_hit_tooltip)
                                = match cache_raw.trim().parse::<f64>().ok()
                                    .filter(|p| p.is_finite() && (0.0..=100.0).contains(p))
                                {
                                    None => (None, None, None),
                                    Some(p) => {
                                        let hits  = hits_raw.parse::<u64>().unwrap_or(0);
                                        let reads = reads_raw.parse::<u64>().unwrap_or(0);
                                        (
                                            Some(format!("{:.1}%", p)),
                                            Some(storage_factor::classify_cache_hit(p)),
                                            Some(storage_factor::build_cache_hit_tooltip(p, hits, reads)),
                                        )
                                    }
                                };
                            // Block IO — magnitude companion to Cache
                            // hit ratio. Sum heap + idx from
                            // pg_statio_user_tables (TOAST excluded
                            // for at-a-glance clarity). Display uses
                            // condensed magnitudes so a busy OLTP
                            // table doesn't overflow the cell. Tooltip
                            // breaks out heap vs index for follow-up.
                            let parse_blk = |k: &str| pick_opt(k)
                                .as_deref()
                                .and_then(|s| s.parse::<i64>().ok())
                                .unwrap_or(0);
                            let heap_hit_i  = parse_blk("Heap_blks_hit");
                            let heap_read_i = parse_blk("Heap_blks_read");
                            let idx_hit_i   = parse_blk("Idx_blks_hit");
                            let idx_read_i  = parse_blk("Idx_blks_read");
                            let block_hit_total  = heap_hit_i  + idx_hit_i;
                            let block_read_total = heap_read_i + idx_read_i;
                            let (block_io_display, block_io_tooltip) =
                                if block_hit_total == 0 && block_read_total == 0 {
                                    (None, None)
                                } else {
                                    let disp = format!(
                                        "{} hit / {} read",
                                        format_int_condensed(block_hit_total),
                                        format_int_condensed(block_read_total),
                                    );
                                    let tip = format!(
                                        "Block IO since last stats reset. \
                                         Heap: {} hit / {} read. \
                                         Index: {} hit / {} read. \
                                         Hit = served from shared_buffers; \
                                         read = fetched from OS/disk. \
                                         TOAST blocks excluded from the summary.",
                                        format_int_with_commas_i64(heap_hit_i),
                                        format_int_with_commas_i64(heap_read_i),
                                        format_int_with_commas_i64(idx_hit_i),
                                        format_int_with_commas_i64(idx_read_i),
                                    );
                                    (Some(disp), Some(tip))
                                };
                            // Dead tuples % — forward-looking vacuum-lag
                            // signal from pg_stat_user_tables. Empty pct
                            // means the table has no rows (fresh / never
                            // populated) or MySQL — renderer skips.
                            let (dead_tup_pct, dead_tup_severity, dead_tup_tooltip)
                                = match pick_opt("Dead_tup_pct")
                                    .as_deref()
                                    .and_then(|s| s.trim().parse::<f64>().ok())
                                    .filter(|p| p.is_finite() && (0.0..=100.0).contains(p))
                                {
                                    None => (None, None, None),
                                    Some(p) => {
                                        let live = pick_opt("N_live_tup")
                                            .as_deref()
                                            .and_then(|s| s.parse::<i64>().ok())
                                            .unwrap_or(0);
                                        let dead = pick_opt("N_dead_tup")
                                            .as_deref()
                                            .and_then(|s| s.parse::<i64>().ok())
                                            .unwrap_or(0);
                                        (
                                            Some(format!("{:.1}%", p)),
                                            Some(storage_factor::classify_dead_tup_pct(p)),
                                            Some(storage_factor::build_dead_tup_tooltip(p, live, dead)),
                                        )
                                    }
                                };
                            // Idx-scan percentage — mirror the cache-hit
                            // derivation. Empty pct means never accessed
                            // (both counters 0) or MySQL — renderer skips
                            // the cell via push-if-non-empty.
                            let (idx_scan_pct, idx_scan_severity, idx_scan_tooltip)
                                = match pick_opt("Idx_scan_pct")
                                    .as_deref()
                                    .and_then(|s| s.trim().parse::<f64>().ok())
                                    .filter(|p| p.is_finite() && (0.0..=100.0).contains(p))
                                {
                                    None => (None, None, None),
                                    Some(p) => {
                                        let idx = pick_opt("Idx_scan")
                                            .as_deref()
                                            .and_then(|s| s.parse::<i64>().ok())
                                            .unwrap_or(0);
                                        let seq = pick_opt("Seq_scan")
                                            .as_deref()
                                            .and_then(|s| s.parse::<i64>().ok())
                                            .unwrap_or(0);
                                        (
                                            Some(format!("{:.1}%", p)),
                                            Some(storage_factor::classify_idx_scan_pct(p)),
                                            Some(storage_factor::build_idx_scan_tooltip(p, idx, seq)),
                                        )
                                    }
                                };
                            // CRUD — four lifetime tuple counters from
                            // pg_stat_user_tables collapsed into a single
                            // `"C / R / U / D"` display cell with a
                            // spelled-out tooltip. Suppressed when every
                            // counter is 0 (fresh table or post-
                            // pg_stat_reset). None on MySQL — the plane
                            // omits the columns.
                            let parse_crud = |k: &str| pick_opt(k)
                                .as_deref()
                                .and_then(|s| s.parse::<i64>().ok())
                                .unwrap_or(0);
                            let crud_c = parse_crud("Crud_c");
                            let crud_r = parse_crud("Crud_r");
                            let crud_u = parse_crud("Crud_u");
                            let crud_d = parse_crud("Crud_d");
                            let (crud_display, crud_tooltip) =
                                if crud_c == 0 && crud_r == 0 && crud_u == 0 && crud_d == 0 {
                                    (None, None)
                                } else {
                                    // Display uses condensed magnitudes
                                    // (105M / 343K / ...) — the DBA
                                    // reads the cell as a rough
                                    // temperature. Tooltip retains
                                    // exact comma-formatted counts for
                                    // the "actually how many" follow-
                                    // up question.
                                    let disp = format!(
                                        "{} / {} / {} / {}",
                                        format_int_condensed(crud_c),
                                        format_int_condensed(crud_r),
                                        format_int_condensed(crud_u),
                                        format_int_condensed(crud_d),
                                    );
                                    let tip = format!(
                                        "CRUD tuple volume since last pg_stat_reset(). \
                                         Create (inserted): {}. \
                                         Read (tuples returned, not query count): {}. \
                                         Update: {}. Delete: {}.",
                                        format_int_with_commas_i64(crud_c),
                                        format_int_with_commas_i64(crud_r),
                                        format_int_with_commas_i64(crud_u),
                                        format_int_with_commas_i64(crud_d),
                                    );
                                    (Some(disp), Some(tip))
                                };
                            // TOAST — pre-composed "1.8 GB (42%)" from
                            // the plane's Toast_size (bytes) + Toast_pct
                            // columns. Both come back empty when the
                            // table has no TOAST relation, TOAST is
                            // empty, or on MySQL. Renderer suppresses
                            // via the push-if-non-empty guard so we
                            // just pass None through.
                            let toast_display = {
                                let bytes_s = pick_opt("Toast_size").unwrap_or_default();
                                let pct_s   = pick_opt("Toast_pct").unwrap_or_default();
                                if bytes_s.is_empty() || pct_s.is_empty() {
                                    None
                                } else {
                                    Some(format!("{} ({}%)", human_bytes(&bytes_s), pct_s))
                                }
                            };
                            // Freeze age % — anti-wraparound proximity.
                            // Empty pct means the table has no
                            // relfrozenxid (temp / unlogged / brand-new)
                            // or MySQL. Same shape used for MultiXact
                            // below; the classifier + tooltip builder
                            // are reused since the semantics are
                            // identical (ratio of age to per-mechanism
                            // GUC).
                            let derive_freeze = |pct_key: &str, raw_key: &str,
                                                 label: &str, setting: &str|
                                -> (Option<String>, Option<Severity>, Option<String>) {
                                let pct = pick_opt(pct_key)
                                    .as_deref()
                                    .and_then(|s| s.trim().parse::<f64>().ok())
                                    .filter(|p| p.is_finite() && *p >= 0.0);
                                match pct {
                                    None => (None, None, None),
                                    Some(p) => {
                                        let raw = pick_opt(raw_key)
                                            .as_deref()
                                            .and_then(|s| s.parse::<i64>().ok())
                                            .unwrap_or(0);
                                        (
                                            Some(format!("{:.0}%", p)),
                                            Some(storage_factor::classify_freeze_pct(p)),
                                            Some(storage_factor::build_freeze_tooltip(
                                                p, raw, label, setting,
                                            )),
                                        )
                                    }
                                }
                            };
                            let (freeze_age_pct, freeze_age_severity, freeze_age_tooltip) =
                                derive_freeze(
                                    "Freeze_age_pct", "Freeze_age_raw",
                                    "transactions", "autovacuum_freeze_max_age",
                                );
                            detail.refreshing.set(false);
                            detail.overview.set(LoadState::Loaded(TableOverview {
                                engine:          pick("Engine"),
                                collation:       pick("Collation"),
                                row_count:       format_int_with_commas(&pick("Rows")),
                                data_size:       human_bytes(&data_len_raw),
                                index_size:      human_bytes(&index_len_raw),
                                // Page_count + Page_count_ideal come
                                // straight from the plane SQL and align
                                // with the storage factor's numerator /
                                // denominator (PG: heap pages; MySQL:
                                // total InnoDB pages). format_int_with_commas
                                // makes large counts readable; the explorer
                                // builds the "~actual / ~ideal" display.
                                data_pages:       format_int_with_commas(&pick("Page_count")),
                                data_pages_ideal: format_int_with_commas(&pick("Page_count_ideal")),
                                avg_row_len:     human_bytes(&pick("Avg_row_length")),
                                auto_inc:        pick("Auto_increment"),
                                create_time:     pick("Create_time"),
                                update_time:     pick("Update_time"),
                                comment:         pick("Comment"),
                                // Skip rendering when zero or absent — the
                                // overview-grid push helper drops empty values.
                                partition_count: if part_count_raw.is_empty() || part_count_raw == "0" {
                                    String::new()
                                } else {
                                    format_int_with_commas(&part_count_raw)
                                },
                                parent_table:    pick_opt("Parent_table"),
                                parent_schema:   pick_opt("Parent_schema"),
                                partition_bound: pick_opt("Partition_bound"),
                                owner:           pick_opt("Owner"),
                                storage_factor,
                                storage_severity,
                                storage_tooltip,
                                last_vacuum:     pick_opt("Last_vacuum"),
                                cache_hit_ratio,
                                cache_hit_severity,
                                cache_hit_tooltip,
                                block_io_display,
                                block_io_tooltip,
                                last_analyze:    pick_opt("Last_analyze"),
                                dead_tup_pct,
                                dead_tup_severity,
                                dead_tup_tooltip,
                                idx_scan_pct,
                                idx_scan_severity,
                                idx_scan_tooltip,
                                crud_display,
                                crud_tooltip,
                                toast_display,
                                freeze_age_pct,
                                freeze_age_severity,
                                freeze_age_tooltip,
                            }));
                        }

                    } else if let Some(rest) = tag.strip_prefix("indexes:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            // SHOW INDEX columns: Key_name, Non_unique, Index_type,
                            // Seq_in_index, Column_name. Group rows into IndexInfo.
                            // `Inherited` is PG-only; MySQL planes don't return it
                            // and the lookup falls back to false.
                            let col_idx = |name: &str| -> usize {
                                columns.iter().position(|c| c.eq_ignore_ascii_case(name)).unwrap_or(0)
                            };
                            let kn = col_idx("Key_name");
                            let nu = col_idx("Non_unique");
                            let it = col_idx("Index_type");
                            let is = columns.iter().position(|c| c.eq_ignore_ascii_case("Index_size"));
                            let cn = col_idx("Column_name");
                            let ih = columns.iter().position(|c| c.eq_ignore_ascii_case("Inherited"));
                            // Per-index storage factor + actual bytes for the
                            // tooltip. PG emits these for btree indexes only
                            // (non-btree leave Storage_factor empty); MySQL
                            // doesn't emit them at all and the parser
                            // collapses to None. Same display + classify
                            // helpers as the table factor for visual
                            // consistency.
                            let sf  = columns.iter().position(|c| c.eq_ignore_ascii_case("Storage_factor"));
                            let sa  = columns.iter().position(|c| c.eq_ignore_ascii_case("Storage_actual_bytes"));
                            // Index_size_ideal: the theoretical-minimum bytes
                            // the index would use if perfectly packed. Same
                            // suppression rules as Storage_factor. Renderer
                            // pairs with the existing Index_size as
                            // "Size / Ideal".
                            let si_ideal = columns.iter().position(|c| c.eq_ignore_ascii_case("Index_size_ideal"));
                            // Scan_count: PG `pg_stat_user_indexes.idx_scan`.
                            // 0 = drop candidate (the renderer colours it
                            // red); >0 just renders the count. MySQL doesn't
                            // emit the column and the parser collapses to
                            // empty.
                            let sc = columns.iter().position(|c| c.eq_ignore_ascii_case("Scan_count"));
                            // Tup_per_scan: avg entries returned per scan
                            // (idx_tup_read / idx_scan). Empty when scan
                            // count is zero. Rendered inline with the
                            // scan count as "1,234 (2.3 avg)".
                            let tps = columns.iter().position(|c| c.eq_ignore_ascii_case("Tup_per_scan"));
                            let mut grouped: Vec<IndexInfo> = Vec::new();
                            for r in rows.into_iter() {
                                let key_name = r.get(kn).cloned().unwrap_or_default();
                                let column   = r.get(cn).cloned().unwrap_or_default();
                                if let Some(existing) = grouped.iter_mut().find(|i| i.name == key_name) {
                                    existing.columns.push(column);
                                } else {
                                    let raw_size = is.and_then(|i| r.get(i).cloned()).unwrap_or_default();
                                    let inherited = ih
                                        .and_then(|i| r.get(i))
                                        .map(|s| matches!(s.trim(), "1" | "t" | "true" | "TRUE"))
                                        .unwrap_or(false);
                                    let factor_raw = sf.and_then(|i| r.get(i).cloned()).unwrap_or_default();
                                    let actual_raw = sa.and_then(|i| r.get(i).cloned()).unwrap_or_default();
                                    let (storage_factor, storage_severity, storage_tooltip)
                                        = match storage_factor::parse(&factor_raw) {
                                            None => (None, None, None),
                                            Some(f) => {
                                                let actual = actual_raw.parse::<u64>().unwrap_or(0);
                                                (
                                                    Some(storage_factor::format_display(f)),
                                                    Some(storage_factor::classify(f)),
                                                    Some(storage_factor::build_tooltip(
                                                        f, actual, storage_factor::TooltipTarget::Index,
                                                    )),
                                                )
                                            }
                                        };
                                    let raw_size_ideal = si_ideal.and_then(|i| r.get(i).cloned()).unwrap_or_default();
                                    let raw_scan = sc.and_then(|i| r.get(i).cloned()).unwrap_or_default();
                                    let raw_tps  = tps.and_then(|i| r.get(i).cloned()).unwrap_or_default();
                                    let (scan_count, scan_severity) = if raw_scan.is_empty() {
                                        (String::new(), None)
                                    } else {
                                        let n = raw_scan.parse::<u64>().unwrap_or(0);
                                        let base = format_int_with_commas(&n.to_string());
                                        let display = if n > 0 && !raw_tps.is_empty() {
                                            // "1,234 (2.3 avg)" — the avg
                                            // entries per scan is a diagnostic
                                            // companion to the scan count.
                                            format!("{} ({} avg)", base, raw_tps.trim())
                                        } else {
                                            base
                                        };
                                        (
                                            display,
                                            if n == 0 { Some(Severity::Bad) } else { None },
                                        )
                                    };
                                    grouped.push(IndexInfo {
                                        name:       key_name,
                                        unique:     r.get(nu).map(|s| s == "0").unwrap_or(false),
                                        index_type: r.get(it).cloned().unwrap_or_default(),
                                        size:       human_bytes(&raw_size),
                                        size_ideal: if raw_size_ideal.is_empty() {
                                            String::new()
                                        } else {
                                            human_bytes(&raw_size_ideal)
                                        },
                                        columns:    vec![column],
                                        inherited,
                                        storage_factor,
                                        storage_severity,
                                        storage_tooltip,
                                        scan_count,
                                        scan_severity,
                                    });
                                }
                            }
                            detail.indexes.set(LoadState::Loaded(grouped));
                        }

                    } else if let Some(rest) = tag.strip_prefix("ddl:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            let ddl_idx = columns.iter()
                                .position(|c| c.starts_with("Create "))
                                .unwrap_or(if columns.len() == 1 { 0 } else { 1 });
                            let ddl = rows.into_iter().next()
                                .and_then(|r| r.get(ddl_idx).cloned())
                                .unwrap_or_default();
                            detail.ddl.set(LoadState::Loaded(ddl));
                        }

                    } else if let Some(rest) = tag.strip_prefix("relancestors:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.ancestors.set(LoadState::Loaded(parse_rel_rows(&columns, rows)));
                        }
                    } else if let Some(rest) = tag.strip_prefix("reldescendants:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.descendants.set(LoadState::Loaded(parse_rel_rows(&columns, rows)));
                        }
                    } else if let Some(rest) = tag.strip_prefix("partitions:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.partitions.set(LoadState::Loaded(parse_partition_rows(&columns, rows)));
                        }
                    } else if let Some(rest) = tag.strip_prefix("toast:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.toast.set(LoadState::Loaded(parse_toast_rows(&columns, rows)));
                        }
                    } else if let Some(rest) = tag.strip_prefix("vm:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.vm.set(LoadState::Loaded(parse_vm_rows(&columns, rows)));
                        }
                    } else if let Some(rest) = tag.strip_prefix("autovac:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.autovac.set(LoadState::Loaded(parse_autovac_rows(&columns, rows)));
                        }
                    } else if let Some(rest) = tag.strip_prefix("erdcols:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.erd_neighbour_columns.set(LoadState::Loaded(parse_erd_columns(&columns, rows)));
                        }
                    } else if let Some(rest) = tag.strip_prefix("data_page:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.data_page.set(LoadState::Loaded(DataPage { columns, rows }));
                        }
                    } else if let Some(rest) = tag.strip_prefix("data_count:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            // Single column "count" with one row. Strip
                            // non-digits (handles thousand separators, +
                            // sign, etc.) before parsing.
                            let n = rows.first().and_then(|r| r.first())
                                .map(|s| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
                                .and_then(|s| s.parse::<u64>().ok());
                            detail.data_exact_count.set(n);
                        }
                    } else if let Some(rest) = tag.strip_prefix("objgrants:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.object_grants.set(LoadState::Loaded(
                                parse_object_grant_rows(&columns, rows)));
                        }
                    } else if tag == "admin_roles" {
                        ep.explorer_admin_roles.set(LoadState::Loaded(parse_admin_rows(&columns, rows)));
                    } else if tag == "admin_role_privs" {
                        ep.explorer_admin_role_privileges.set(LoadState::Loaded(rows));
                    } else if tag == "admin_role_affected_objects" {
                        ep.explorer_admin_role_affected_objects.set(LoadState::Loaded(rows));
                    } else if let Some(area_str) = tag.strip_prefix("admin_area:") {
                        if let Some(area) = parse_admin_area_tag(area_str) {
                            ep.explorer_admin_area_data.update(|m| {
                                m.insert(area, LoadState::Loaded(rows));
                            });
                        }
                    } else if let Some(rest) = tag.strip_prefix("admin_row_detail:") {
                        // Format: admin_row_detail:<area>:<row_id>:<section>
                        // row_id and section can contain colons; we split on
                        // first two only.
                        if let Some((area_str, rest)) = rest.split_once(':') {
                            if let Some((row_id, section)) = rest.split_once(':') {
                                if let Some(area) = parse_admin_area_tag(area_str) {
                                    let key = (area, row_id.to_string(), section.to_string());
                                    ep.explorer_admin_row_detail.update(|m| {
                                        m.insert(key, LoadState::Loaded(rows));
                                    });
                                }
                            }
                        }
                    }
                }

                ServerMessage::IntrospectError { session_id: sid, tag, message, error } => {
                    // `\ef` / `\ev` lookups (no such function, an
                    // ambiguous name …) answer in the shell that asked,
                    // as psql does: severity and message only, whatever
                    // VERBOSITY says, and not kept for \errverbose.
                    if tag == "edit_to_input" {
                        if let Some(tab) = shell_for_session(&sid) {
                            tab.querying.set(false);
                            use crate::plane::{ErrorStyle, Verbosity};
                            let error = error.unwrap_or_else(|| shared::DbError::from_message(message.clone()));
                            let style = ErrorStyle { verbosity: Verbosity::Minimal, ..ErrorStyle::default() };
                            let lines = plane_for(tab.db_type).format_error(&error, None, style);
                            tab.shell_history.update(|h| h.push(ShellEntry::CliError { lines, message }));
                            scroll_shell_to_bottom(&tab.id);
                        }
                        return;
                    }
                    // mysql's `\s` / `status`: the shell's own status query
                    // failed — said as the CLI says a server error.
                    if tag == "pretty_kv" {
                        if let Some(tab) = shell_for_session(&sid) {
                            tab.pending_warnings.set(Vec::new());
                            tab.querying.set(false);
                            let error = error.unwrap_or_else(|| shared::DbError::from_message(message.clone()));
                            let lines = plane_for(tab.db_type).format_error(&error, None, crate::plane::ErrorStyle::default());
                            tab.shell_history.update(|h| h.push(ShellEntry::CliError { lines, message }));
                            scroll_shell_to_bottom(&tab.id);
                        }
                        return;
                    }
                    // Activity pane errors route next.
                    if tag.starts_with("activity_") {
                        let ap = shell_panes.get().values()
                            .find(|t| t.kind == PaneKind::Activity
                                   && t.session_id.get_untracked().as_deref() == Some(&sid))
                            .cloned();
                        let Some(ap) = ap else { return; };
                        match tag.as_str() {
                            "activity_sessions" => ap.activity_sessions.set(LoadState::Error(message)),
                            "activity_locks"    => ap.activity_locks.set(LoadState::Error(message)),
                            "activity_blocks"   => ap.activity_blocks.set(LoadState::Error(message)),
                            "activity_kill"     => {
                                // Kill failed — clear the confirm popover and surface
                                // the error on the sessions signal so the user sees
                                // it without a separate toast.
                                ap.activity_confirm_kill.set(None);
                                ap.activity_sessions.set(LoadState::Error(format!("Kill failed: {}", message)));
                            }
                            _ => {}
                        }
                        return;
                    }

                    let ep = shell_panes.get().values()
                        .find(|t| t.kind == PaneKind::SchemaExplorer
                               && t.session_id.get_untracked().as_deref() == Some(&sid))
                        .cloned();
                    let Some(ep) = ep else { return; };

                    if tag == "schemas" {
                        ep.explorer_schemas.set(LoadState::Error(message));
                    } else if let Some(rest) = tag.strip_prefix("objects:") {
                        let (kind_lbl, _) = rest.split_once(':').unwrap_or((rest, ""));
                        if ep.explorer_kind.get_untracked().label() == kind_lbl {
                            ep.explorer_listing.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("objddl:") {
                        if let Some((kind_lbl, qual)) = rest.split_once(':') {
                            if let Some((schema, name)) = qual.split_once('.') {
                                if let Some(kind) = ObjectKind::ALL.iter().copied()
                                    .find(|k| k.label() == kind_lbl)
                                {
                                    ep.explorer_object_ddls.update(|m| {
                                        m.insert(
                                            (kind, schema.to_string(), name.to_string()),
                                            LoadState::Error(message.clone()),
                                        );
                                    });
                                }
                            }
                        }
                    } else if let Some(rest) = tag.strip_prefix("columns:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.columns.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("status:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.overview.set(LoadState::Error(message));
                            detail.refreshing.set(false);
                        }
                    } else if let Some(rest) = tag.strip_prefix("indexes:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.indexes.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("ddl:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.ddl.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("relancestors:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.ancestors.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("reldescendants:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.descendants.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("partitions:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.partitions.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("toast:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.toast.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("vm:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.vm.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("autovac:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.autovac.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("erdcols:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.erd_neighbour_columns.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("data_page:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.data_page.set(LoadState::Error(message));
                        }
                    } else if let Some(rest) = tag.strip_prefix("objgrants:") {
                        if let Some(detail) = find_table_detail(&ep, rest) {
                            detail.object_grants.set(LoadState::Error(message));
                        }
                    } else if tag == "admin_roles" {
                        ep.explorer_admin_roles.set(LoadState::Error(message));
                    } else if tag == "admin_role_privs" {
                        ep.explorer_admin_role_privileges.set(LoadState::Error(message));
                    } else if tag == "admin_role_affected_objects" {
                        ep.explorer_admin_role_affected_objects.set(LoadState::Error(message));
                    } else if let Some(area_str) = tag.strip_prefix("admin_area:") {
                        if let Some(area) = parse_admin_area_tag(area_str) {
                            ep.explorer_admin_area_data.update(|m| {
                                m.insert(area, LoadState::Error(message));
                            });
                        }
                    } else if let Some(rest) = tag.strip_prefix("admin_row_detail:") {
                        if let Some((area_str, rest)) = rest.split_once(':') {
                            if let Some((row_id, section)) = rest.split_once(':') {
                                if let Some(area) = parse_admin_area_tag(area_str) {
                                    let key = (area, row_id.to_string(), section.to_string());
                                    ep.explorer_admin_row_detail.update(|m| {
                                        m.insert(key, LoadState::Error(message));
                                    });
                                }
                            }
                        }
                    }
                }

                ServerMessage::DescribeResult { session_id: sid, columns, exec_time_us } => {
                    // \gdesc's first step, as psql's `DescribeQuery`: the
                    // statement's columns as the server described them.
                    // Then the names of their types, asked of the server
                    // (`described_columns_sql`) and printed as any result
                    // (`PostAction::Gdesc`) — or, with no columns, psql's
                    // word for that.
                    let tab = shell_panes.get().values()
                        .find(|t| t.session_id.get().as_deref() == Some(&sid))
                        .cloned();
                    if let Some(tab) = tab {
                        let names = crate::plane::plane_for(tab.db_type).described_columns_sql(&columns)
                            .filter(|_| !columns.is_empty());
                        match names {
                            Some(sql) => match shell::send_to(&tab, ws) {
                                Some(to) => {
                                    tab.pending_vertical.set(tab.expanded.get_untracked() == Expanded::On);
                                    tab.pending_post_action.set(Some(PostAction::Gdesc { describe_us: exec_time_us }));
                                    shell::deliver(&tab, ws, to, sql);
                                }
                                // The session's gone, and its transaction
                                // with it: nothing to close.
                                None => {
                                    tab.wrap.set(None);
                                    tab.querying.set(false);
                                }
                            },
                            None => {
                                shell::set_result_variables(&tab, shell::CommandOutcome::Done { row_count: 0 });
                                tab.shell_history.update(|h| h.push(ShellEntry::MetaOutput(vec![
                                    "The command has no result, or the result has no columns.".into(),
                                ])));
                                // psql's DescribeQuery times only the query
                                // naming the types, so its time is none:
                                // `Time: 0.000 ms` under \timing.
                                if tab.show_timing.get_untracked() {
                                    tab.shell_history.update(|h| h.push(ShellEntry::Info { exec_time_us: 0, said: Vec::new(), timed: true }));
                                }
                                tab.querying.set(false);
                                shell::run_next_queued(&tab, ws);
                            }
                        }
                        scroll_shell_to_bottom(&tab.id);
                    }
                }

                ServerMessage::DescribeError { session_id: sid, message, error } => {
                    let tab = shell_panes.get().values()
                        .find(|t| t.session_id.get().as_deref() == Some(&sid))
                        .cloned();
                    if let Some(tab) = tab {
                        let sent = tab.sent_sql.get_untracked();
                        let failed = tab.shell_history.with_untracked(|h| shell::failed_command(h, 0, sent.as_deref()));
                        let entry = shell::command_error(&tab, message, error, failed.query, !failed.meta);
                        tab.shell_history.update(|h| h.push(entry));
                        tab.querying.set(false);
                        // A failed statement: under ON_ERROR_STOP the rest
                        // is dropped; its savepoint, if psql set one,
                        // closed first.
                        if tab.on_error_stop.get_untracked() {
                            shell::drop_rest(&tab);
                        }
                        shell::run_next_queued(&tab, ws);
                        scroll_shell_to_bottom(&tab.id);
                    }
                }

                // A staged meta-command's batch (psql's `\d <table>`).
                ServerMessage::IntrospectBatchResult { session_id: sid, tag, result, connection_lost } => {
                    if let Some(tab) = shell_for_session(&sid).filter(|_| shell::is_wrap_tag(&tag)) {
                        // What psql's SendQuery sends around a statement.
                        shell::continue_wrap(&tab, ws, &tag, result, connection_lost);
                    } else if let Some(tab) = shell_for_session(&sid).filter(|_| shell::is_warnings_tag(&tag)) {
                        // The server's warnings after a statement.
                        shell::print_warnings(&tab, ws, &tag, result, connection_lost);
                    } else if let Some(tab) = shell_for_session(&sid) {
                        let staged = tab.pending_meta.with_untracked(Option::is_some);
                        shell::continue_staged_meta(&tab, ws, &tag, result);
                        // A `\d` that found the session dead: psql resets it
                        // then and there (the SessionClosed that follows).
                        if connection_lost && staged && tab.kind == PaneKind::Shell {
                            shell::mark_lost(&tab);
                        }
                    }
                }

                ServerMessage::DmlResult { session_id: sid, tag, .. } => {
                    let ep = shell_panes.get().values()
                        .find(|t| t.session_id.get_untracked().as_deref() == Some(&sid))
                        .cloned();
                    let Some(ep) = ep else { return };
                    if let Some(rest) = tag.strip_prefix("data_update:") {
                        // Tag format: data_update:<schema>.<table>:<edit_id>
                        let Some((tbl_key, edit_id)) = rest.rsplit_once(':') else { return };
                        let Some(detail) = find_table_detail(&ep, tbl_key) else { return };
                        let pending = detail.data_edit_pending.with_untracked(|m| m.get(edit_id).cloned());
                        let Some((row_idx, col, new_val)) = pending else { return };
                        detail.data_edit_pending.update(|m| { m.remove(edit_id); });
                        detail.data_edit_status.update(|m| {
                            m.insert((row_idx, col.clone()), EditStatus::Saved);
                        });
                        // Patch the in-memory page so the cell reflects the new
                        // committed value without refetching. Find the column
                        // index by name (case-insensitive matches the rest of
                        // the parser conventions).
                        detail.data_page.update(|state| {
                            if let LoadState::Loaded(page) = state {
                                if let Some(col_idx) = page.columns.iter().position(|c| c.eq_ignore_ascii_case(&col)) {
                                    if let Some(row) = page.rows.get_mut(row_idx) {
                                        if let Some(cell) = row.get_mut(col_idx) {
                                            *cell = new_val;
                                        }
                                    }
                                }
                            }
                        });
                        // Auto-clear the Saved badge after ~1.5s.
                        if let Some(win) = web_sys::window() {
                            let cb = Closure::once_into_js(move || {
                                detail.data_edit_status.update(|m| {
                                    if matches!(m.get(&(row_idx, col.clone())), Some(EditStatus::Saved)) {
                                        m.remove(&(row_idx, col.clone()));
                                    }
                                });
                            });
                            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
                                cb.as_ref().unchecked_ref(), 1500);
                        }
                    } else if let Some(tbl_key) = tag.strip_prefix("data_insert:") {
                        // Tag format: data_insert:<schema>.<table>
                        let Some(detail) = find_table_detail(&ep, tbl_key) else { return };
                        // Successful insert: clear the form buffer,
                        // briefly show "Saved" in the form, and bump
                        // the per-table refresh tick so the page
                        // refetches and surfaces the new row (with its
                        // auto-generated PK / defaults).
                        //
                        // We deliberately KEEP `view_mode == Insert` so
                        // the user sees the Saved pill render — flipping
                        // to Grid here would unmount the only thing
                        // displaying that signal. The 1.5s timer below
                        // performs the mode flip alongside the status
                        // clear so the transition is one event.
                        detail.data_insert_buffer.update(|m| m.clear());
                        detail.data_insert_status.set(Some(EditStatus::Saved));
                        detail.data_refresh_tick.update(|t| *t += 1);
                        if let Some(win) = web_sys::window() {
                            let cb = Closure::once_into_js(move || {
                                if matches!(detail.data_insert_status.get_untracked(), Some(EditStatus::Saved)) {
                                    detail.data_insert_status.set(None);
                                    detail.data_view_mode.set(DataViewMode::Grid);
                                }
                            });
                            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
                                cb.as_ref().unchecked_ref(), 1500);
                        }
                    } else if let Some(rest) = tag.strip_prefix("data_delete:") {
                        // Tag format: data_delete:<schema>.<table>:<row_idx>
                        let Some((tbl_key, idx_str)) = rest.rsplit_once(':') else { return };
                        let Some(detail) = find_table_detail(&ep, tbl_key) else { return };
                        let Ok(_idx) = idx_str.parse::<usize>() else { return };
                        // Successful delete: clear the pending row,
                        // wipe the per-row delete status (indices will
                        // shift after refetch and stale entries would
                        // confuse later renders), drop selection state,
                        // and refetch the page so the gap is gone.
                        detail.data_delete_pending.set(None);
                        detail.data_delete_status.update(|m| m.clear());
                        detail.data_selected_row.set(None);
                        detail.data_selected_col.set(None);
                        detail.data_refresh_tick.update(|t| *t += 1);
                    } else if let Some(rest) = tag.strip_prefix("obj_grant:")
                        .or_else(|| tag.strip_prefix("obj_revoke:"))
                    {
                        // Tag format: obj_(grant|revoke):<schema>.<name>:<grantee>:<priv>
                        // The grantee field can contain colons (MySQL
                        // `user@host` doesn't, but PUBLIC and quoted
                        // names are robust against it). Split from the
                        // right twice so the leftmost rest stays as
                        // <schema>.<name>.
                        let Some((rest, priv_name)) = rest.rsplit_once(':') else { return };
                        let Some((tbl_key, grantee)) = rest.rsplit_once(':') else { return };
                        let Some(detail) = find_table_detail(&ep, tbl_key) else { return };
                        let key = (grantee.to_string(), priv_name.to_string());
                        ep.explorer_priv_cell_status.update(|m| {
                            m.insert(key.clone(), EditStatus::Saved);
                        });
                        // Clear the banner if the just-completed action
                        // was a REVOKE — tag prefix tells us.
                        if tag.starts_with("obj_revoke:") {
                            ep.explorer_priv_pending_revoke.set(None);
                        }
                        // Reset to NotLoaded so the lazy-fetch effect
                        // in render_privileges_tab re-fires and pulls
                        // the post-GRANT/REVOKE matrix. The effect
                        // tracks grants_sig reactively for exactly
                        // this purpose.
                        detail.object_grants.set(LoadState::NotLoaded);
                        // Auto-clear the Saved badge after ~1.5s.
                        if let Some(win) = web_sys::window() {
                            let key_for_clear = key.clone();
                            let status_sig = ep.explorer_priv_cell_status;
                            let cb = Closure::once_into_js(move || {
                                status_sig.update(|m| {
                                    if matches!(m.get(&key_for_clear), Some(EditStatus::Saved)) {
                                        m.remove(&key_for_clear);
                                    }
                                });
                            });
                            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
                                cb.as_ref().unchecked_ref(), 1500);
                        }
                    } else if let Some(rest) = tag.strip_prefix("schema_grant:")
                        .or_else(|| tag.strip_prefix("schema_revoke:"))
                    {
                        // Tag format: schema_(grant|revoke):<schema>:<grantee>:<priv>
                        let Some((rest, priv_name)) = rest.rsplit_once(':') else { return };
                        let Some((schema_name, grantee)) = rest.rsplit_once(':') else { return };
                        let key = (grantee.to_string(), priv_name.to_string());
                        ep.explorer_priv_cell_status.update(|m| {
                            m.insert(key.clone(), EditStatus::Saved);
                        });
                        if tag.starts_with("schema_revoke:") {
                            ep.explorer_priv_pending_revoke.set(None);
                        }
                        // Refetch the Schemas Privileges section by
                        // marking the cache entry NotLoaded. The
                        // lazy-fetch effect in render_explorer_pane
                        // re-fires the section's SQL the next time
                        // the renderer reads it.
                        let cache_key = (AdminArea::Schemas, schema_name.to_string(), "Privileges".to_string());
                        ep.explorer_admin_row_detail.update(|m| {
                            m.insert(cache_key, LoadState::NotLoaded);
                        });
                        // Auto-clear the Saved badge.
                        if let Some(win) = web_sys::window() {
                            let key_for_clear = key.clone();
                            let status_sig = ep.explorer_priv_cell_status;
                            let cb = Closure::once_into_js(move || {
                                status_sig.update(|m| {
                                    if matches!(m.get(&key_for_clear), Some(EditStatus::Saved)) {
                                        m.remove(&key_for_clear);
                                    }
                                });
                            });
                            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
                                cb.as_ref().unchecked_ref(), 1500);
                        }
                    }
                }

                ServerMessage::DmlError { session_id: sid, tag, message } => {
                    let ep = shell_panes.get().values()
                        .find(|t| t.session_id.get_untracked().as_deref() == Some(&sid))
                        .cloned();
                    let Some(ep) = ep else { return };
                    if let Some(rest) = tag.strip_prefix("data_update:") {
                        let Some((tbl_key, edit_id)) = rest.rsplit_once(':') else { return };
                        let Some(detail) = find_table_detail(&ep, tbl_key) else { return };
                        let pending = detail.data_edit_pending.with_untracked(|m| m.get(edit_id).cloned());
                        let Some((row_idx, col, _)) = pending else { return };
                        detail.data_edit_pending.update(|m| { m.remove(edit_id); });
                        detail.data_edit_status.update(|m| {
                            m.insert((row_idx, col), EditStatus::Failed(message));
                        });
                    } else if let Some(tbl_key) = tag.strip_prefix("data_insert:") {
                        let Some(detail) = find_table_detail(&ep, tbl_key) else { return };
                        // Failed insert: keep the form buffer intact so
                        // the user can fix the bad value and retry; show
                        // the DB's error message inline.
                        detail.data_insert_status.set(Some(EditStatus::Failed(message)));
                    } else if let Some(rest) = tag.strip_prefix("data_delete:") {
                        let Some((tbl_key, idx_str)) = rest.rsplit_once(':') else { return };
                        let Some(detail) = find_table_detail(&ep, tbl_key) else { return };
                        let Ok(idx) = idx_str.parse::<usize>() else { return };
                        // Failed delete: leave the banner up so the
                        // user can read the error and retry / cancel.
                        detail.data_delete_status.update(|m| {
                            m.insert(idx, EditStatus::Failed(message));
                        });
                    } else if let Some(rest) = tag.strip_prefix("obj_grant:")
                        .or_else(|| tag.strip_prefix("obj_revoke:"))
                        .or_else(|| tag.strip_prefix("schema_grant:"))
                        .or_else(|| tag.strip_prefix("schema_revoke:"))
                    {
                        // Common error path for all four grant/revoke
                        // tags. We don't care about the object key for
                        // error display — just the (grantee, priv)
                        // cell needs to flip to Failed so the matrix
                        // paints it red. Leave the banner up if it's a
                        // REVOKE so the user can read the error.
                        // Two rsplit_once peels off priv, then grantee.
                        // No literal-string-suffix trimming so a role
                        // literally named SELECT receiving SELECT
                        // privilege still parses correctly.
                        let Some((rest_no_priv, priv_name)) = rest.rsplit_once(':') else { return };
                        let Some((_, grantee)) = rest_no_priv.rsplit_once(':') else { return };
                        let key = (grantee.to_string(), priv_name.to_string());
                        ep.explorer_priv_cell_status.update(|m| {
                            m.insert(key, EditStatus::Failed(message));
                        });
                    }
                }
                // External-editor replies: the shell that's waiting for
                // that edit (tag carries its nonce) takes them.
                ServerMessage::EditExternallyResult { session_id: sid, tag, content } => {
                    if let Some(tab) = shell_for_session(&sid) {
                        shell::finish_external_edit(&tab, &tag, shell::EditEnd::Text(content));
                    }
                }
                ServerMessage::EditExternallyError { session_id: sid, tag, message } => {
                    if let Some(tab) = shell_for_session(&sid) {
                        shell::finish_external_edit(&tab, &tag, shell::EditEnd::Note(format!("External editor: {message}")));
                    }
                }
                ServerMessage::EditExternallyCancelled { session_id: sid, tag, message } => {
                    if let Some(tab) = shell_for_session(&sid) {
                        shell::finish_external_edit(&tab, &tag, shell::EditEnd::Note(message));
                    }
                }
            }
        });

        socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();

        let sock2 = socket.clone();
        let onopen = Closure::<dyn FnMut()>::new(move || {
            ws_status.set(WsStatus::Connected);
            // Successful open clears the backoff so the next disconnect
            // starts fresh at 1s instead of compounding from a long-ago
            // run of failures.
            reconnect_backoff_ms.set(1000);
            for msg in [ClientMessage::ListConnections, ClientMessage::GetSettings] {
                if let Ok(j) = serde_json::to_string(&msg) {
                    let _ = sock2.send_with_str(&j);
                }
            }
            // Phase C: re-establish any sessions that went stale during
            // the outage. Each pane's Connect is staggered by 50-250ms
            // additional cumulative jitter to avoid a synchronized burst
            // hitting noctua (see project_connection_storm_jitter memory).
            // SessionOpened arrival flips session_stale=false per pane, so
            // the ✕ pill self-heals as each Connect round-trips.
            let stale_panes: Vec<ShellTab> = shell_panes.get_untracked().values()
                .filter(|p| p.session_stale.get_untracked())
                .cloned()
                .collect();
            let mut delay_ms: i32 = 0;
            for pane in stale_panes {
                let jitter = (50.0 + js_sys::Math::random() * 200.0) as i32;
                delay_ms += jitter;
                let conn_id = pane.conn_id.clone();
                let pane_id = pane.id.clone();
                let role = pane.kind.application_role().to_string();
                // Back to the database it was on, not the default.
                let database = pane.reconnect_database();
                let cb = Closure::once_into_js(move || {
                    ws_send(ws, &ClientMessage::Connect {
                        connection_id: conn_id,
                        client_tab_id: pane_id,
                        database,
                        client_role: Some(role),
                        timezone: crate::settings::session_time_zone(),
                    });
                });
                if let Some(w) = web_sys::window() {
                    let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                        cb.as_ref().unchecked_ref(),
                        delay_ms,
                    );
                }
            }
        });
        socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();

        // onclose / onerror surface a silent noctua death to the user via the
        // top-of-app banner, tag every existing pane's session as stale so
        // the connection-id pill flips to a red ✕, AND schedule an
        // auto-reconnect with exponential backoff so the user doesn't have
        // to click anything.
        let on_disconnect = move || {
            // Avoid double-handling: onerror often fires alongside onclose.
            if ws_status.get_untracked() == WsStatus::Disconnected { return; }
            ws_status.set(WsStatus::Disconnected);
            // Mark every pane stale + clear its in-flight signals. Without
            // the cleanup, a long-running query in flight at the moment
            // noctua dies leaves the pane permanently "Executing…" even
            // after the auto-reconnect succeeds — the textarea stays
            // hidden, no prompt, no recovery short of closing the pane.
            let pane_ids: Vec<String> = shell_panes.get_untracked().keys().cloned().collect();
            for pid in &pane_ids {
                if let Some(pane) = shell_panes.get_untracked().get(pid).cloned() {
                    pane.session_stale.set(true);
                }
                reset_in_flight_signals_outer(pid);
            }
            let delay = reconnect_backoff_ms.get_untracked() as i32;
            let next  = (delay as u32).saturating_mul(2).min(30_000);
            reconnect_backoff_ms.set(next);
            let cb = Closure::once_into_js(move || {
                // Only retry if still disconnected — the user may have
                // clicked Reconnect manually in the meantime.
                if ws_status.get_untracked() == WsStatus::Disconnected {
                    connect_ws_slot.with_value(|c| {
                        if let Some(cb) = c { cb.call(()); }
                    });
                }
            });
            if let Some(w) = web_sys::window() {
                let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    cb.as_ref().unchecked_ref(),
                    delay,
                );
            }
        };
        let on_disconnect_close = on_disconnect;
        let on_disconnect_error = on_disconnect;

        let onclose = Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |_| {
            on_disconnect_close();
        });
        socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();

        let onerror = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            on_disconnect_error();
        });
        socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();

        ws_status.set(WsStatus::Connecting);
        ws.set(Some(socket));
    });

    // Now that connect_ws exists, register it in the slot so the disconnect
    // handlers can recursively re-invoke it via setTimeout-driven retry.
    connect_ws_slot.set_value(Some(connect_ws));

    // Cold-start: open the first WebSocket. Any failures route through
    // ws_status → the disconnected banner, so the app shell still renders.
    connect_ws.call(());


    // ── Close a pane by id ────────────────────────────────────────────────────

    // ── Autofocus the rename input when inline rename starts ─────────────────
    create_effect(move |prev: Option<Option<String>>| {
        let now = renaming_pane_id.get();
        let was_renaming = prev.flatten();
        if was_renaming.is_none() {
            if let Some(pid) = now.clone() {
                let id = format!("pane-tab-rename-input-{}", pid);
                if let Some(win) = web_sys::window() {
                    let cb = Closure::once(move || {
                        use wasm_bindgen::JsCast;
                        if let Some(el) = web_sys::window()
                            .and_then(|w| w.document())
                            .and_then(|d| d.get_element_by_id(&id))
                        {
                            if let Ok(ta) = el.clone().dyn_into::<web_sys::HtmlInputElement>() {
                                let _ = ta.focus();
                                ta.select();
                            }
                        }
                    });
                    let _ = win.request_animation_frame(cb.as_ref().unchecked_ref());
                    cb.forget();
                }
            }
        }
        now
    });

    // ── Clear notifications for a pane when it gains focus ───────────────────
    create_effect(move |_| {
        if let Some(pid) = active_pane_id.get() {
            if let Some(tab) = shell_panes.get_untracked().get(&pid).cloned() {
                notifications.update(|v| v.retain(|n| n.pane_id != pid));
                if tab.kind == PaneKind::Shell {
                    // In place: switching back to a shell keeps where
                    // its scrollback was.
                    let input_id = format!("shell-input-{}", pid);
                    if let Some(win) = web_sys::window() {
                        let cb = Closure::once(move || {
                            use wasm_bindgen::JsCast;
                            if let Some(el) = web_sys::window()
                                .and_then(|w| w.document())
                                .and_then(|d| d.get_element_by_id(&input_id))
                                .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
                            {
                                crate::dom::focus_in_place(&el);
                            }
                        });
                        let _ = win.request_animation_frame(cb.as_ref().unchecked_ref());
                        cb.forget();
                    }
                }
            }
        }
    });

    // Count in-flight DML across every TableDetail open on a pane.
    // Three signals carry "saving in flight" state per DML kind:
    //   - UPDATE: `data_edit_pending` (HashMap by edit-id) — populated
    //     when the inline cell editor commits.
    //   - INSERT: `data_insert_status == Some(Saving)` — single in-
    //     flight insert form per table.
    //   - DELETE: `data_delete_status` map — `EditStatus::Saving`
    //     entries are the in-flight per-row deletes.
    //
    // The close-time confirm dialog (audit 2.12 / issue #18) sums
    // them so the DBA sees "N pending edit(s)" before they sign off
    // on losing visibility into the outcome. The DB-side processing
    // still goes through — Disconnect doesn't cancel an already-sent
    // ExecuteDml — but the DmlResult / DmlError that arrive after
    // close would silently no-op because the pane is gone from
    // `shell_panes`. This gate gives the user one chance to wait
    // instead.
    let pending_dml_on_pane = move |pid: &str| -> usize {
        let Some(tab) = shell_panes.get_untracked().get(pid).cloned() else { return 0 };
        let mut count = 0usize;
        tab.explorer_table_details.with_untracked(|m| {
            for detail in m.values() {
                count += detail.data_edit_pending.with_untracked(|m| m.len());
                if matches!(detail.data_insert_status.get_untracked(),
                            Some(EditStatus::Saving)) {
                    count += 1;
                }
                count += detail.data_delete_status.with_untracked(|m|
                    m.values().filter(|s|
                        matches!(s, EditStatus::Saving)).count());
            }
        });
        count
    };

    // Intent-signal for the close logic. `Some(pid)` means "run the
    // actual close for this pane." The public `close_pane` writes
    // here either directly (no pending DML) or via the confirm
    // dialog's on_confirm (after the DBA acknowledges pending DML).
    // An effect below watches this signal and runs the close body.
    //
    // We use an intent signal instead of a captured `Rc<dyn Fn>`
    // closure because the public `close_pane` is passed into
    // `render_workspace_panes` (and downstream renderers) which require
    // its closure type to be `Copy` — `Rc<dyn Fn>` isn't, but a
    // signal-write closure is.
    let close_pane_arm: RwSignal<Option<String>> = create_rw_signal(None);

    create_effect(move |_| {
        let Some(pane_id) = close_pane_arm.get() else { return };
        close_pane_arm.set_untracked(None);
        // 0a. Snapshot for the Cmd+Shift+Z reopen stack. Capped at 10 —
        //     matches "recent browser tab undo" behavior.
        if let Some(p) = shell_panes.get_untracked().get(&pane_id).cloned() {
            closed_panes_stack.update(|stack| {
                stack.push(ClosedPaneInfo {
                    conn_id:      p.conn_id.clone(),
                    kind:         p.kind,
                    custom_label: p.custom_label.get_untracked(),
                    database:     p.reconnect_database(),
                });
                if stack.len() > 10 { stack.remove(0); }
            });
        }
        // 0. If this is an Activity pane, clear its refresh timer first so
        //    the interval stops firing before the signal arena drops the
        //    closure. Dropping the Closure alongside the handle releases
        //    the JS callback cleanly.
        if let Some(p) = shell_panes.get().get(&pane_id).cloned() {
            if p.kind == PaneKind::Activity {
                p.activity_timer.update_value(|slot| {
                    if let Some((id, _cb)) = slot.take() {
                        if let Some(w) = web_sys::window() {
                            w.clear_interval_with_handle(id);
                        }
                    }
                });
            }
            // Clear \watch timer if active
            p.watch_timer.update_value(|slot| {
                if let Some((id, _cb)) = slot.take() {
                    if let Some(w) = web_sys::window() {
                        w.clear_interval_with_handle(id);
                    }
                }
            });
        }
        // 1. Disconnect session
        if let Some(p) = shell_panes.get().get(&pane_id).cloned() {
            if let Some(sid) = p.session_id.get() {
                ws_send(ws, &ClientMessage::Disconnect { session_id: sid });
            }
        }
        // 2. Remove from flat map
        shell_panes.update(|m| { m.remove(&pane_id); });

        // 3. Handle removal from the pane tree
        let mut empty_wt: Option<String> = None;
        let wts = workspace_tabs.get();
        for wt in &wts {
            let tree = wt.layout.get_untracked();
            if let Some(leaf) = tree.find_leaf(&pane_id) {
                // If the closed pane was the zoomed one, unzoom so the
                // workspace returns to its full layout.
                let unzoom = wt.zoomed_pane_id.get_untracked().as_deref() == Some(&pane_id);
                let tab_count = leaf.tabs.get_untracked().len();
                if tab_count == 1 {
                    // Last tab in leaf — remove the whole leaf from the tree,
                    // in one update with the unzoom (see do_split).
                    match tree.remove_empty_leaf(&pane_id) {
                        None    => { empty_wt = Some(wt.id.clone()); }
                        Some(l) => batch(|| {
                            if unzoom { wt.zoomed_pane_id.set(None); }
                            wt.layout.set(l);
                        }),
                    }
                } else {
                    // Multi-tab leaf — just remove this tab, keep leaf in
                    // tree; one update, so no moment shows no tab.
                    batch(|| {
                        if unzoom { wt.zoomed_pane_id.set(None); }
                        let tabs_snap = leaf.tabs.get_untracked();
                        let removed_idx = tabs_snap.iter().position(|id| id == &pane_id).unwrap_or(0);
                        leaf.tabs.update(|v| v.retain(|id| id != &pane_id));
                        let new_len    = leaf.tabs.get_untracked().len();
                        let cur_active = leaf.active.get_untracked();
                        if removed_idx < cur_active {
                            leaf.active.set(cur_active - 1);
                        } else if removed_idx == cur_active {
                            leaf.active.set(cur_active.saturating_sub(1).min(new_len.saturating_sub(1)));
                        }
                    });
                }
                break;
            }
        }

        // 4. Remove empty workspace tabs
        if let Some(ref wt_id) = empty_wt {
            workspace_tabs.update(|tabs| tabs.retain(|t| &t.id != wt_id));
        }

        // 5. Update active selections
        let remaining = workspace_tabs.get();
        if remaining.is_empty() {
            active_wt_id.set(None); active_pane_id.set(None); pane.set(Pane::Welcome);
        } else if empty_wt.as_deref() == active_wt_id.get().as_deref() {
            let last = remaining.last().unwrap();
            active_wt_id.set(Some(last.id.clone()));
            active_pane_id.set(Some(last.layout.get_untracked().first_pane_id()));
        } else if active_pane_id.get().as_deref() == Some(&pane_id) {
            if let Some(wt) = remaining.iter().find(|t| active_wt_id.get().as_deref() == Some(&t.id)) {
                active_pane_id.set(Some(wt.layout.get_untracked().first_pane_id()));
            }
        }
    });

    // Public-facing `close_pane`. Gates the inner close on a
    // pending-DML scan. If anything's in flight, fires a Confirm
    // dialog naming the count; on confirm, the close runs (via the
    // intent signal above). Cancel keeps the pane open so the DBA
    // can wait for the DmlResult / DmlError to surface in the cell
    // editor. Audit 2.12 / issue #18.
    let close_pane = move |pane_id: String| {
        let pending = pending_dml_on_pane(&pane_id);
        if pending == 0 {
            close_pane_arm.set(Some(pane_id));
            return;
        }
        let pid = pane_id.clone();
        dialog.set(Some(DialogKind::Confirm {
            title:         "Close pane with pending edits?".to_string(),
            message:       format!(
                "{} pending edit{} on this pane.\n\nThe database will still process {} server-side, but the outcome (success or error) won't be visible here. Close anyway?",
                pending,
                if pending == 1 { "" } else { "s" },
                if pending == 1 { "it" } else { "them" },
            ),
            confirm_label: "Close anyway".to_string(),
            danger:        false,
            on_confirm:    Callback::new(move |_| {
                close_pane_arm.set(Some(pid.clone()));
            }),
        }));
    };

    // ── Split a specific pane ─────────────────────────────────────────────────

    let do_split = move |target_pane_id: String, dir: SplitDir| {
        let conn_id = match shell_panes.get().get(&target_pane_id).map(|p| p.conn_id.clone()) {
            Some(id) => id, None => return,
        };
        let conn = connections.get().into_iter().find(|c| c.id == conn_id);
        let (label, db_type, hkey, fmt_style) = conn.map(|c| (
            c.name.clone(), c.db_type,
            make_history_key(c.db_type, &c.host, c.port, &c.database),
            c.format_style,
        )).unwrap_or_else(|| ("Shell".to_string(), DbType::MySql, String::new(), FormatStyle::Unformatted));

        let new_pane = ShellTab::new(conn_id.clone(), label, db_type, hkey);
        new_pane.format_style.set(fmt_style);
        let new_pane_id = new_pane.id.clone();
        let role = new_pane.kind.application_role().to_string();
        ws_send(ws, &ClientMessage::Connect {
            connection_id: conn_id,
            client_tab_id: new_pane_id.clone(),
            database: None,
            client_role: Some(role),
            timezone: crate::settings::session_time_zone(),
        });
        shell_panes.update(|m| { m.insert(new_pane_id.clone(), new_pane); });

        let wts = workspace_tabs.get();
        for wt in &wts {
            if wt.layout.get_untracked().find_leaf(&target_pane_id).is_some() {
                // A new leaf appears after this split; exit zoom so the user
                // doesn't end up with an invisible new pane. One update:
                // the workspace tree re-renders on each of these signals
                // (`set` notifies even when the value is unchanged, hence
                // the guard), and panes built by a first render and dropped
                // by a second before their effects ran panic Leptos
                // (OwnerDisposed).
                let new_layout = wt.layout.get_untracked().split_at(&target_pane_id, &new_pane_id, dir);
                batch(|| {
                    if wt.zoomed_pane_id.get_untracked().is_some() { wt.zoomed_pane_id.set(None); }
                    wt.layout.set(new_layout);
                });
                break;
            }
        }
        active_pane_id.set(Some(new_pane_id));
    };

    // ── Open a new workspace tab for a connection ─────────────────────────────

    // ── Spawn a fresh workspace tab + session ─────────────────────────────────
    //
    // Single source of truth for "create a new pane bound to this credential",
    // called by the credentials tree (force_new path of open_conn_tab) and the
    // pane-add dropdown.
    let spawn_session = move |
        conn_id: String,
        kind:    PaneKind,
    | -> Option<String> {
        let conn = connections.get().into_iter().find(|c| c.id == conn_id);
        let (cred_name, db_type, hkey, fmt_style) = conn.map(|c| (
            c.name, c.db_type,
            make_history_key(c.db_type, &c.host, c.port, &c.database),
            c.format_style,
        )).unwrap_or_else(|| ("Shell".to_string(), DbType::MySql, String::new(), FormatStyle::Unformatted));

        let tab = match kind {
            PaneKind::SchemaExplorer => {
                let lbl = format!("{} — Schema", cred_name);
                ShellTab::new_explorer(conn_id.clone(), lbl, db_type)
            }
            PaneKind::Activity => {
                let lbl = format!("{} — Activity", cred_name);
                ShellTab::new_activity(conn_id.clone(), lbl, db_type)
            }
            PaneKind::Shell => ShellTab::new(conn_id.clone(), cred_name.clone(), db_type, hkey),
        };
        tab.format_style.set(fmt_style);
        let tab_id = tab.id.clone();
        let role = tab.kind.application_role().to_string();
        ws_send(ws, &ClientMessage::Connect {
            connection_id: conn_id,
            client_tab_id: tab_id.clone(),
            database: None,
            client_role: Some(role),
            timezone: crate::settings::session_time_zone(),
        });
        shell_panes.update(|m| { m.insert(tab_id.clone(), tab); });

        let wt = WorkspaceTab::new(cred_name, PaneTree::Leaf(PaneLeaf::new(tab_id.clone())));
        let wt_id = wt.id.clone();
        workspace_tabs.update(|v| v.push(wt));
        active_wt_id.set(Some(wt_id));
        active_pane_id.set(Some(tab_id.clone()));
        if pane.get_untracked() != Pane::Shell {
            pane.set(Pane::Shell);
        }
        Some(tab_id)
    };

    // `force_new = true` means "always mint a fresh workspace tab + session"
    // (used by the credentials tree — clicking a credential is a new-session
    // gesture). `force_new = false` keeps the navigate-if-existing behavior
    // used by the Active & Pinned section, where a pinned-but-not-yet-started
    // entry creates a session and a subsequent click jumps to it.
    let open_conn_tab = move |conn_id: String, force_new: bool| {
        // Stamp Recents on every open gesture (both the "reuse existing
        // tab" path and the "mint new tab" path). De-dup + cap is handled
        // by `touch_recent`. Persist so it survives reload.
        recents.update(|r| touch_recent(r, &conn_id));
        persist_recents(&recents.get_untracked());
        if !force_new {
            // If a workspace tab already has a pane for this connection,
            // switch to it instead of creating a duplicate.
            let existing = workspace_tabs.get().iter().find_map(|wt| {
                let first = wt.layout.get_untracked().first_pane_id();
                let matches = shell_panes.get().get(&first)
                    .map(|p| p.conn_id == conn_id)
                    .unwrap_or(false);
                if matches { Some((wt.id.clone(), first)) } else { None }
            });
            if let Some((wt_id, first_pid)) = existing {
                active_wt_id.set(Some(wt_id.clone()));
                // Preserve the last-active pane if it belongs to this workspace;
                // only fall back to first_pane_id if the current active pane is
                // from a different workspace.
                let current = active_pane_id.get_untracked();
                let belongs = current.as_ref().map(|pid| {
                    workspace_tabs.get().iter()
                        .find(|wt| wt.id == wt_id)
                        .map(|wt| wt.layout.get_untracked().find_leaf(pid).is_some())
                        .unwrap_or(false)
                }).unwrap_or(false);
                if !belongs {
                    active_pane_id.set(Some(first_pid));
                }
                if pane.get_untracked() != Pane::Shell {
                    pane.set(Pane::Shell);
                }
                return;
            }
        }

        let conn = connections.get().into_iter().find(|c| c.id == conn_id);
        let default_pane_pref = conn.as_ref().map(|c| c.default_pane.clone())
            .unwrap_or_else(|| "shell".to_string());
        let pane_kind = match default_pane_pref.as_str() {
            "schema" => PaneKind::SchemaExplorer,
            "activity" => PaneKind::Activity,
            _ => PaneKind::Shell,
        };
        spawn_session(conn_id, pane_kind);
    };


    // ── Open a non-shell pane in the same leaf as the triggering pane ──────
    //
    // Generic over PaneKind — dispatches to the right ShellTab constructor
    // based on `kind`. Replaces the old add_explorer_to_pane helper now that
    // the ⊡ button is a dropdown menu advertising every plane-supported
    // pane type.

    let add_pane_to_leaf = move |pane_id: String, kind: PaneKind, explorer_init: Option<ExplorerInit>| {
        let conn_id = match shell_panes.get().get(&pane_id).map(|p| p.conn_id.clone()) {
            Some(id) => id, None => return,
        };
        let conn = connections.get().into_iter().find(|c| c.id == conn_id);
        let (conn_name, db_type, hkey, fmt_style) = conn.map(|c| (
            c.name, c.db_type,
            make_history_key(c.db_type, &c.host, c.port, &c.database),
            c.format_style,
        )).unwrap_or_else(|| ("Shell".to_string(), DbType::MySql, String::new(), FormatStyle::Unformatted));

        let database = explorer_init.as_ref().and_then(|init| init.database.clone());
        let new_pane = match kind {
            PaneKind::SchemaExplorer => {
                let label = format!("{} — Schema", conn_name);
                let tab = ShellTab::new_explorer(conn_id.clone(), label, db_type);
                // Pre-select the requested table so the explorer's
                // table-detail view opens immediately when the pane
                // becomes visible. The session-open flow loads the
                // schema list and the selected entry's details
                // lazily — explorer_schema gets pinned to the
                // requested schema as well so the schema dropdown
                // doesn't default to the first one and clear the
                // selection.
                if let Some(init) = explorer_init {
                    // Opened to show one relation (`\d <name>`): give the
                    // inspector the room, as its ⤢ button does.
                    tab.explorer_detail_expanded.set(true);
                    tab.explorer_kind.set(init.kind);
                    tab.explorer_schema.set(Some(init.schema.clone()));
                    let target = (init.schema, init.name);
                    tab.explorer_selected.set(Some(target.clone()));
                    // The outer-list yellow-highlight key. Without
                    // this, the table-detail view opens correctly
                    // but the left-list row matching the pre-selected
                    // table stays un-highlighted until the listing
                    // arrives and the user clicks / arrow-keys to it.
                    tab.explorer_cursor_key.set(Some(target));
                }
                tab
            }
            PaneKind::Activity => {
                let label = format!("{} — Activity", conn_name);
                ShellTab::new_activity(conn_id.clone(), label, db_type)
            }
            PaneKind::Shell => {
                ShellTab::new(conn_id.clone(), conn_name, db_type, hkey)
            }
        };
        new_pane.format_style.set(fmt_style);
        let new_pane_id = new_pane.id.clone();
        let role = new_pane.kind.application_role().to_string();
        ws_send(ws, &ClientMessage::Connect {
            connection_id: conn_id,
            client_tab_id: new_pane_id.clone(),
            database,
            client_role: Some(role),
            timezone: crate::settings::session_time_zone(),
        });
        shell_panes.update(|m| { m.insert(new_pane_id.clone(), new_pane); });

        // Add to the same leaf as the triggering pane (mini-tab-bar)
        let wts = workspace_tabs.get();
        for wt in &wts {
            if let Some(leaf) = wt.layout.get_untracked().find_leaf(&pane_id) {
                let new_idx = leaf.tabs.get_untracked().len();
                leaf.tabs.update(|v| v.push(new_pane_id.clone()));
                leaf.active.set(new_idx);
                break;
            }
        }
        active_pane_id.set(Some(new_pane_id));
    };

    // ── Bring a pane to the front ─────────────────────────────────────────────
    //
    // The one place that knows a pane lives in a workspace tab AND a leaf's
    // tab strip, and sets both along with the active pane. Setting only
    // `active_pane_id` (as several paths used to) selects a pane that stays
    // hidden behind its leaf's current tab.
    let focus_pane = move |pane_id: String| {
        for wt in workspace_tabs.get_untracked() {
            let Some(leaf) = wt.layout.get_untracked().find_leaf(&pane_id) else { continue };
            if let Some(idx) = leaf.tabs.get_untracked().iter().position(|p| p == &pane_id) {
                leaf.active.set(idx);
            }
            // A zoom on some other leaf would keep this one off screen.
            if wt.zoomed_pane_id.get_untracked()
                .is_some_and(|z| !leaf.tabs.get_untracked().contains(&z))
            {
                wt.zoomed_pane_id.set(None);
            }
            if active_wt_id.get_untracked().as_deref() != Some(wt.id.as_str()) {
                active_wt_id.set(Some(wt.id.clone()));
            }
            break;
        }
        if pane.get_untracked() != Pane::Shell { pane.set(Pane::Shell); }
        active_pane_id.set(Some(pane_id.clone()));
        focus_shell_input(&pane_id);
    };

    // ── Run a command in an explorer's action shell ───────────────────────────
    //
    // Explorer admin commands (Storage tab → Run) never touch a user's
    // shell: they run in the explorer's own shell tab — its own session, so
    // an open transaction elsewhere is never at risk, and a shell, so the
    // SQL, timing, errors and notices stay observable. The tab opens in the
    // background next to the explorer and is reused while it's idle.
    let run_in_action_shell = move |req: ActionShellRun| -> Option<String> {
        let explorer = shell_panes.with_untracked(|m| m.get(&req.explorer_pane_id).cloned())?;
        let origin = ActionOrigin {
            explorer_pane_id: req.explorer_pane_id.clone(),
            schema:           req.schema,
            name:             req.name,
        };
        let explorer_db = explorer.current_db.get_untracked();
        let reusable = explorer.explorer_action_pane.get_untracked()
            .and_then(|id| shell_panes.with_untracked(|m| m.get(&id).cloned()))
            .filter(|shell| action_shell_can_take(shell, &explorer_db));
        if let Some(shell) = reusable {
            shell.action_origin.set(Some(origin));
            // Shown like a typed command: terminated by the shell's delimiter.
            let stmt = TypedStatement::new(req.sql, shell.delimiter.get_untracked());
            crate::shell::run_in_shell(&shell, ws, stmt);
            return Some(shell.id.clone());
        }

        // Open one on the database the explorer is showing. The command
        // runs when its session opens (SessionOpened → startup_sql).
        let conn = connections.get_untracked().into_iter().find(|c| c.id == explorer.conn_id)?;
        let hkey = make_history_key(conn.db_type, &conn.host, conn.port, &conn.database);
        let shell = ShellTab::new(conn.id.clone(), conn.name.clone(), conn.db_type, hkey);
        shell.format_style.set(conn.format_style);
        let db = explorer_db;
        shell.custom_label.set(Some(format!(
            "admin · {}", if db.is_empty() { conn.name.as_str() } else { db.as_str() },
        )));
        shell.startup_sql.set(Some(req.sql));
        shell.action_origin.set(Some(origin));
        let shell_id = shell.id.clone();
        ws_send(ws, &ClientMessage::Connect {
            connection_id: conn.id,
            client_tab_id: shell_id.clone(),
            database:      (!db.is_empty()).then_some(db),
            client_role:   Some(shell.kind.application_role().to_string()),
            timezone:   crate::settings::session_time_zone(),
        });
        shell_panes.update(|m| { m.insert(shell_id.clone(), shell); });
        for wt in workspace_tabs.get_untracked() {
            if let Some(leaf) = wt.layout.get_untracked().find_leaf(&req.explorer_pane_id) {
                // Background tab: the leaf's active tab stays the explorer.
                leaf.tabs.update(|v| v.push(shell_id.clone()));
                break;
            }
        }
        explorer.explorer_action_pane.set(Some(shell_id.clone()));
        Some(shell_id)
    };

    let pane_ops = PaneOps {
        run_in_action_shell: Callback::new(run_in_action_shell),
        focus:               Callback::new(focus_pane),
        open_explorer:       Callback::new(move |(pane_id, init): (String, ExplorerInit)| {
            add_pane_to_leaf(pane_id, PaneKind::SchemaExplorer, Some(init));
        }),
    };

    // ── Add a tab inside a specific pane's leaf ───────────────────────────────
    // This is what the per-pane ⊞ button calls.

    let add_tab = move |pane_id: String| {
        let conn_id = match shell_panes.get().get(&pane_id).map(|p| p.conn_id.clone()) {
            Some(id) => id, None => return,
        };
        let conn = connections.get().into_iter().find(|c| c.id == conn_id);
        let (label, db_type, hkey, fmt_style) = conn.map(|c| (
            c.name.clone(), c.db_type,
            make_history_key(c.db_type, &c.host, c.port, &c.database),
            c.format_style,
        )).unwrap_or_else(|| ("Shell".to_string(), DbType::MySql, String::new(), FormatStyle::Unformatted));

        let new_pane = ShellTab::new(conn_id.clone(), label, db_type, hkey);
        new_pane.format_style.set(fmt_style);
        let new_pane_id = new_pane.id.clone();
        let role = new_pane.kind.application_role().to_string();
        ws_send(ws, &ClientMessage::Connect {
            connection_id: conn_id,
            client_tab_id: new_pane_id.clone(),
            database: None,
            client_role: Some(role),
            timezone: crate::settings::session_time_zone(),
        });
        shell_panes.update(|m| { m.insert(new_pane_id.clone(), new_pane); });

        // Find the PaneLeaf that owns pane_id and push the new tab into it
        let wts = workspace_tabs.get();
        for wt in &wts {
            if let Some(leaf) = wt.layout.get_untracked().find_leaf(&pane_id) {
                let new_idx = leaf.tabs.get_untracked().len();
                leaf.tabs.update(|v| v.push(new_pane_id.clone()));
                leaf.active.set(new_idx);
                break;
            }
        }
        active_pane_id.set(Some(new_pane_id));
    };

    // ── Tab context-menu + keyboard-shortcut helpers ─────────────────────────

    // Clone same-kind pane into the same leaf. Reuses add_pane_to_leaf so the
    // existing Shell / SchemaExplorer / Activity constructors stay authoritative.
    let duplicate_pane = move |pane_id: String| {
        let kind = match shell_panes.get_untracked().get(&pane_id).map(|p| p.kind) {
            Some(k) => k,
            None    => return,
        };
        add_pane_to_leaf(pane_id, kind, None);
    };

    // Close every pane in the same leaf as `keep_pid` except `keep_pid`.
    let close_others = move |keep_pid: String| {
        let siblings: Vec<String> = workspace_tabs.get_untracked().iter()
            .find_map(|wt| wt.layout.get_untracked().find_leaf(&keep_pid)
                .map(|leaf| leaf.tabs.get_untracked()))
            .unwrap_or_default()
            .into_iter()
            .filter(|id| id != &keep_pid)
            .collect();
        for pid in siblings { close_pane(pid); }
    };

    // Close every pane in the same leaf whose index is greater than
    // `anchor_pid`'s index. Matches browser "Close to right".
    let close_to_right = move |anchor_pid: String| {
        let (tabs, anchor_idx) = {
            let mut out: Option<(Vec<String>, usize)> = None;
            for wt in workspace_tabs.get_untracked().iter() {
                if let Some(leaf) = wt.layout.get_untracked().find_leaf(&anchor_pid) {
                    let tabs = leaf.tabs.get_untracked();
                    if let Some(idx) = tabs.iter().position(|id| id == &anchor_pid) {
                        out = Some((tabs, idx));
                        break;
                    }
                }
            }
            match out { Some(x) => x, None => return }
        };
        let victims: Vec<String> = tabs.into_iter().enumerate()
            .filter_map(|(i, id)| if i > anchor_idx { Some(id) } else { None })
            .collect();
        for pid in victims { close_pane(pid); }
    };

    // Start renaming a tab — swap the label for an input seeded with the
    // current effective label. Commit / cancel handled in the input's keydown.
    let start_rename = move |pane_id: String| {
        let seed = shell_panes.get_untracked().get(&pane_id)
            .map(|t| {
                t.custom_label.get_untracked().unwrap_or_else(|| {
                    let db = t.current_db.get_untracked();
                    if db.is_empty() { t.label.clone() } else { db }
                })
            })
            .unwrap_or_default();
        rename_input.set(seed);
        renaming_pane_id.set(Some(pane_id));
    };

    // Reopen the most-recently-closed pane. Builds a fresh ShellTab using
    // the CLOSED pane's `conn_id` + `kind` (not the active selection — a
    // subtle bug in an earlier draft) and places it in the active
    // workspace's first leaf, or a new workspace if none exists.
    let reopen_last_closed = move || {
        let info = closed_panes_stack.try_update(|s| s.pop()).flatten();
        let Some(info) = info else { return };

        // Look up the connection to get name / db_type / hkey. If the
        // connection was deleted while the pane was in the stack, there's
        // nothing useful to reopen; silently drop.
        let conn = connections.get_untracked().into_iter().find(|c| c.id == info.conn_id);
        let Some(conn) = conn else { return };
        let hkey = make_history_key(conn.db_type, &conn.host, conn.port, &conn.database);

        let new_pane = match info.kind {
            PaneKind::SchemaExplorer => {
                let lbl = format!("{} — Schema", conn.name);
                ShellTab::new_explorer(info.conn_id.clone(), lbl, conn.db_type)
            }
            PaneKind::Activity => {
                let lbl = format!("{} — Activity", conn.name);
                ShellTab::new_activity(info.conn_id.clone(), lbl, conn.db_type)
            }
            PaneKind::Shell => ShellTab::new(info.conn_id.clone(), conn.name.clone(), conn.db_type, hkey),
        };
        new_pane.format_style.set(conn.format_style);
        let new_pane_id = new_pane.id.clone();
        // Restore the renamed label before inserting so the first render
        // paints with the correct name.
        if let Some(label) = info.custom_label {
            new_pane.custom_label.set(Some(label));
        }
        let role = new_pane.kind.application_role().to_string();
        ws_send(ws, &ClientMessage::Connect {
            connection_id: info.conn_id.clone(),
            client_tab_id: new_pane_id.clone(),
            database: info.database,
            client_role: Some(role),
            timezone: crate::settings::session_time_zone(),
        });
        shell_panes.update(|m| { m.insert(new_pane_id.clone(), new_pane); });

        // Place the reopened pane: active workspace's first leaf if one
        // exists, else create a fresh workspace tab.
        let active_wt = active_wt_id.get_untracked();
        let placed = active_wt.as_ref().and_then(|wt_id| {
            workspace_tabs.get_untracked().iter().find(|w| &w.id == wt_id).cloned()
        }).map(|wt| {
            let first_pid = wt.layout.get_untracked().first_pane_id();
            if first_pid.is_empty() { return false; }
            if let Some(leaf) = wt.layout.get_untracked().find_leaf(&first_pid) {
                let new_idx = leaf.tabs.get_untracked().len();
                leaf.tabs.update(|v| v.push(new_pane_id.clone()));
                leaf.active.set(new_idx);
                true
            } else { false }
        }).unwrap_or(false);

        if !placed {
            let wt = WorkspaceTab::new(conn.name.clone(), PaneTree::Leaf(PaneLeaf::new(new_pane_id.clone())));
            let wt_id = wt.id.clone();
            workspace_tabs.update(|v| v.push(wt));
            active_wt_id.set(Some(wt_id));
        }
        active_pane_id.set(Some(new_pane_id));
        if pane.get_untracked() != Pane::Shell { pane.set(Pane::Shell); }
    };

    // Jump to pane index N (0-based) in the active leaf. Silently no-ops if
    // the index is out of range.
    let jump_active_leaf_to = move |n: usize| {
        let Some(active_pid) = active_pane_id.get_untracked() else { return };
        for wt in workspace_tabs.get_untracked().iter() {
            if let Some(leaf) = wt.layout.get_untracked().find_leaf(&active_pid) {
                let tabs = leaf.tabs.get_untracked();
                if n < tabs.len() {
                    leaf.active.set(n);
                    active_pane_id.set(Some(tabs[n].clone()));
                }
                break;
            }
        }
    };

    // Cycle to the previous/next pane in the active leaf. Wraps.
    let active_leaf_step = move |delta: i32| {
        let Some(active_pid) = active_pane_id.get_untracked() else { return };
        for wt in workspace_tabs.get_untracked().iter() {
            if let Some(leaf) = wt.layout.get_untracked().find_leaf(&active_pid) {
                let tabs = leaf.tabs.get_untracked();
                if tabs.is_empty() { return; }
                let cur = leaf.active.get_untracked() as i32;
                let len = tabs.len() as i32;
                let next = ((cur + delta) % len + len) % len;
                leaf.active.set(next as usize);
                active_pane_id.set(Some(tabs[next as usize].clone()));
                break;
            }
        }
    };

    // ── Global keyboard shortcuts (Cmd on Mac) ───────────────────────────────
    // Scope and rationale documented in plan; Cmd+W/T/Shift+T are not
    // overridden because the browser reserves them reliably.
    //
    // Tracks the timestamp (ms since epoch) of the most recent unhandled
    // Escape, so we can detect a double-tap "go up a level" gesture in
    // the schema explorer. Reset to 0 after a double-tap fires.
    let last_escape_at: StoredValue<f64> = StoredValue::new(0.0);
    {
        let cb = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |e: web_sys::KeyboardEvent| {
            // The Settings dialog is modal and handles its own keys
            // (Leptos delegates its on:keydown to window too, so
            // stopping propagation there wouldn't keep this one out).
            if settings_ui.open.get_untracked() { return; }
            // Esc: dismiss context menu / abort rename regardless of modifiers.
            if e.key() == "Escape" {
                let mut handled = false;
                if tab_context_menu.get_untracked().is_some() {
                    tab_context_menu.set(None);
                    handled = true;
                }
                if renaming_pane_id.get_untracked().is_some() {
                    renaming_pane_id.set(None);
                    handled = true;
                }
                if handled {
                    e.prevent_default();
                    // A consumed Escape resets the double-tap timer so the
                    // user doesn't unexpectedly walk up a level on their
                    // very next Escape press.
                    last_escape_at.set_value(0.0);
                    return;
                }
                // Unhandled at the global level. Two-stage handling for
                // a SchemaExplorer pane with detail visible:
                //   (1) If the keyboard cursor is on the detail tab strip,
                //       Escape pops it back to the outer table list.
                //   (2) Otherwise, Escape arms / completes a double-tap
                //       gesture to close the inspector entirely.
                let now_ms = js_sys::Date::now();
                let prev = last_escape_at.get_value();
                if let Some(pid) = active_pane_id.get_untracked() {
                    if let Some(tab) = shell_panes.get_untracked().get(&pid).cloned() {
                        if tab.kind == PaneKind::SchemaExplorer
                            && tab.explorer_detail_visible.get_untracked()
                        {
                            // (1a) Pop body-level focus → outer list.
                            // Either DataBody (selected_col is Some) or
                            // SchemaBody (schema_selected_row is Some)
                            // — clearing the appropriate signal drops
                            // the body cursor, leaving the loaded row's
                            // outer-list highlight visible.
                            let active_detail: Option<TableDetail> =
                                tab.explorer_selected.get_untracked()
                                    .and_then(|k| {
                                        tab.explorer_table_details
                                            .with_untracked(|m| m.get(&k).copied())
                                    });
                            if let Some(detail) = active_detail {
                                if detail.data_selected_col.get_untracked().is_some() {
                                    detail.data_selected_col.set(None);
                                    last_escape_at.set_value(0.0);
                                    e.prevent_default();
                                    return;
                                }
                                if detail.schema_selected_row.get_untracked().is_some() {
                                    detail.schema_selected_row.set(None);
                                    last_escape_at.set_value(0.0);
                                    e.prevent_default();
                                    return;
                                }
                            }

                            // (2) Double-tap close. 500ms window matches typical OS
                            // double-click thresholds and gives slow taps room while
                            // still feeling like a deliberate gesture.
                            if prev > 0.0 && (now_ms - prev) < 500.0 {
                                tab.explorer_detail_visible.set(false);
                                tab.explorer_detail_expanded.set(false);
                                last_escape_at.set_value(0.0);
                                e.prevent_default();
                                return;
                            }
                            last_escape_at.set_value(now_ms);
                            return;
                        }
                    }
                }
                // Not an explorer with detail open — clear the timer so an
                // earlier solo Escape elsewhere doesn't combine with a
                // future explorer Escape.
                last_escape_at.set_value(0.0);
                return;
            }
            // ArrowUp / ArrowDown / ArrowLeft / ArrowRight / Enter / Tab:
            // keyboard navigation across the explorer's nested layers.
            //
            //   • Outer table list: Up/Down walks the row highlight
            //     (the existing yellow-cream + blue-text "active" style,
            //     now driven by `cursor_key`); Enter on a non-loaded
            //     cursor row loads the detail; second Enter on a
            //     loaded row whose active tab is Data descends into
            //     the body.
            //   • Detail tab strip: Left/Right activates each tab as
            //     the cursor walks (no separate cursor — the underline
            //     IS the cursor since active = walked tab).
            //   • Data tab body: Up/Down/Left/Right walk a per-cell
            //     cursor (spreadsheet style) backed by the
            //     (selected_row, selected_col) signals; Tab/Shift+Tab
            //     synonym for Right/Left; Enter opens the inline
            //     editor on the focused cell; Esc clears `selected_col`
            //     and pops back to outer-list mode.
            //
            // All implemented via DOM lookup keyed on stable data
            // attrs (`data-row-idx`/`data-col-idx`) and CSS classes
            // (`.explorer-list-row`, `.detail-tab-btn`, `.data-cell`).
            // Skipped when an input/textarea/select has focus so form
            // fields keep their native caret movement.
            let is_arrow = matches!(e.key().as_str(),
                "ArrowDown" | "ArrowUp" | "ArrowLeft" | "ArrowRight");
            let is_enter = e.key() == "Enter";
            let is_tab   = e.key() == "Tab";
            if (is_arrow || is_enter || is_tab)
                && !e.meta_key() && !e.ctrl_key() && !e.alt_key()
                // Shift is only meaningful for Tab here (Shift+Tab).
                && (!e.shift_key() || is_tab)
            {
                use wasm_bindgen::JsCast;
                if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                    // Bail when an editable element has focus — let the
                    // browser's native caret movement / list-box handling
                    // win in that context.
                    if let Some(active) = doc.active_element() {
                        let tag = active.tag_name();
                        if tag == "INPUT" || tag == "TEXTAREA" || tag == "SELECT" {
                            return;
                        }
                    }
                    let Some(pid) = active_pane_id.get_untracked() else { return; };
                    let Some(tab) = shell_panes.get_untracked().get(&pid).cloned() else { return; };
                    if tab.kind != PaneKind::SchemaExplorer { return; }
                    let container_id = format!("explorer-{}", pid);
                    let Some(container) = doc.get_element_by_id(&container_id) else { return; };

                    // Resolve the loaded table's TableDetail once — it
                    // tells us if we're in DataBody mode (selected_col
                    // is Some) and gives the row/col signals to write.
                    let active_detail: Option<TableDetail> =
                        tab.explorer_selected.get_untracked()
                            .and_then(|k| {
                                tab.explorer_table_details
                                    .with_untracked(|m| m.get(&k).copied())
                            });

                    // ── Layer C: cell-level focus inside Data body ──
                    // Spreadsheet-style nav driven through the
                    // (selected_row, selected_col) signal pair so mouse
                    // and keyboard never disagree. Tab / Shift+Tab are
                    // synonyms for Right / Left; Enter opens the inline
                    // editor; ArrowUp at row 0 stays put (Esc is the
                    // way out of DataBody mode).
                    if let Some(detail) = active_detail {
                        if let Some(cur_col) = detail.data_selected_col.get_untracked() {
                            let mut cur_row = detail.data_selected_row.get_untracked().unwrap_or(0);
                            let Ok(rows) = container.query_selector_all(".data-row") else { return; };
                            let nrows = rows.length() as usize;
                            if nrows == 0 { return; }

                            // Snapshot every visible row's
                            // `data-row-idx` in DOM order. The renderer
                            // stamps the *original* page index (not the
                            // post-filter position) so that selection
                            // and edit-pending state survive a sort or
                            // filter narrow. For arrow nav we therefore
                            // must walk by visible position (`vis_pos`)
                            // and translate to/from the stored idx.
                            let mut visible_idxs: Vec<usize> = Vec::with_capacity(nrows);
                            for i in 0..nrows as u32 {
                                let Some(node) = rows.item(i) else { continue };
                                let Ok(el) = node.dyn_into::<web_sys::Element>() else { continue };
                                let raw = el.get_attribute("data-row-idx").unwrap_or_default();
                                if let Ok(n) = raw.parse::<usize>() {
                                    visible_idxs.push(n);
                                }
                            }
                            if visible_idxs.is_empty() { return; }

                            // Column count is stable across rows, so
                            // measuring the first visible row works.
                            let Ok(first_row_cells) = container
                                .query_selector(".data-row")
                                .ok().flatten()
                                .map(|el| el.query_selector_all(".data-cell"))
                                .unwrap_or(Err(wasm_bindgen::JsValue::NULL))
                            else { return; };
                            let ncols = first_row_cells.length() as usize;
                            if ncols == 0 { return; }

                            // Resolve cursor's visible-position. If the
                            // stored row is no longer visible (e.g. the
                            // user just enabled a filter), snap to the
                            // first visible row AND write the new
                            // original-idx back to `selected_row` so
                            // the row-highlight class binding tracks
                            // the keyboard cursor.
                            let cur_vis = match visible_idxs.iter().position(|i| *i == cur_row) {
                                Some(p) => p,
                                None => {
                                    cur_row = visible_idxs[0];
                                    detail.data_selected_row.set(Some(cur_row));
                                    0
                                }
                            };

                            // Enter → open inline editor by dispatching
                            // a bubbling dblclick on the focused cell.
                            if is_enter {
                                let cell = container.query_selector(
                                    &format!(
                                        ".data-cell[data-row-idx=\"{}\"][data-col-idx=\"{}\"]",
                                        visible_idxs[cur_vis], cur_col,
                                    )
                                ).ok().flatten();
                                if let Some(cell) = cell {
                                    let init = web_sys::MouseEventInit::new();
                                    init.set_bubbles(true);
                                    init.set_cancelable(true);
                                    if let Ok(ev) = web_sys::MouseEvent::new_with_mouse_event_init_dict(
                                        "dblclick", &init,
                                    ) {
                                        let _ = cell.dispatch_event(&ev);
                                    }
                                }
                                e.prevent_default();
                                return;
                            }

                            // Compute new (vis_pos, col).
                            let (new_vis, new_col): (usize, usize) = match e.key().as_str() {
                                "ArrowDown"  => ((cur_vis + 1).min(visible_idxs.len() - 1), cur_col),
                                "ArrowUp"    => (cur_vis.saturating_sub(1),                  cur_col),
                                "ArrowLeft"  => (cur_vis, cur_col.saturating_sub(1)),
                                "ArrowRight" => (cur_vis, (cur_col + 1).min(ncols - 1)),
                                "Tab" if e.shift_key() => (cur_vis, cur_col.saturating_sub(1)),
                                "Tab"                  => (cur_vis, (cur_col + 1).min(ncols - 1)),
                                _ => return,
                            };
                            e.prevent_default();
                            let new_row = visible_idxs[new_vis];
                            if new_row == cur_row && new_col == cur_col { return; }
                            if new_row != cur_row {
                                detail.data_selected_row.set(Some(new_row));
                            }
                            if new_col != cur_col {
                                detail.data_selected_col.set(Some(new_col));
                            }
                            // Scroll the new cell into view.
                            if let Ok(Some(cell)) = container.query_selector(
                                &format!(
                                    ".data-cell[data-row-idx=\"{}\"][data-col-idx=\"{}\"]",
                                    new_row, new_col,
                                )
                            ) {
                                if let Ok(html_el) = cell.dyn_into::<web_sys::HtmlElement>() {
                                    let opts = web_sys::ScrollIntoViewOptions::new();
                                    opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
                                    opts.set_inline(web_sys::ScrollLogicalPosition::Nearest);
                                    opts.set_behavior(web_sys::ScrollBehavior::Smooth);
                                    html_el.scroll_into_view_with_scroll_into_view_options(&opts);
                                }
                            }
                            return;
                        }

                        // ── Layer C′: row cursor inside Schema-tab body ──
                        // Vertical-only (no column axis): Up/Down walks
                        // column rows; Left/Right exit SchemaBody and
                        // let Layer B's tab-strip walker take over;
                        // Tab/Enter are consumed no-ops (no edit
                        // semantics for schema rows yet).
                        if let Some(cur_row) = detail.schema_selected_row.get_untracked() {
                            // Left / Right: drop the cursor and fall
                            // through to Layer B's tab-strip walker.
                            // Done first so we still respond when the
                            // schema-cols table isn't rendered yet.
                            if e.key() == "ArrowLeft" || e.key() == "ArrowRight" {
                                detail.schema_selected_row.set(None);
                                // intentional fall-through
                            } else if matches!(e.key().as_str(), "Tab" | "Enter") {
                                e.prevent_default();
                                return;
                            } else {
                                let Ok(rows) = container
                                    .query_selector_all(".schema-cols-table tbody tr.schema-col-row")
                                else { return; };
                                let nrows = rows.length() as usize;
                                if nrows == 0 { return; }
                                let new_row = match e.key().as_str() {
                                    "ArrowDown" => (cur_row + 1).min(nrows - 1),
                                    "ArrowUp"   => cur_row.saturating_sub(1),
                                    _ => return,
                                };
                                e.prevent_default();
                                if new_row == cur_row { return; }
                                detail.schema_selected_row.set(Some(new_row));
                                if let Ok(Some(node)) = container.query_selector(
                                    &format!(
                                        ".schema-cols-table tbody tr.schema-col-row[data-row-idx=\"{}\"]",
                                        new_row,
                                    )
                                ) {
                                    if let Ok(html_el) = node.dyn_into::<web_sys::HtmlElement>() {
                                        let opts = web_sys::ScrollIntoViewOptions::new();
                                        opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
                                        opts.set_behavior(web_sys::ScrollBehavior::Smooth);
                                        html_el.scroll_into_view_with_scroll_into_view_options(&opts);
                                    }
                                }
                                return;
                            }
                        }
                    }

                    // ── Layer B: keyboard cursor on outer object list ──
                    // Tab on the outer list falls through to native
                    // focus traversal — outer-list nav is up/down only.
                    if is_tab { return; }
                    let Ok(rows) = container.query_selector_all(".explorer-list-row") else { return; };
                    let n = rows.length();
                    if n == 0 { return; }

                    // Read each row's (schema, name) from the data
                    // attributes the renderer stamps. Listing them
                    // here lets us reason about ordering without
                    // duplicating the filtered Vec into a signal.
                    let mut row_keys: Vec<(String, String)> = Vec::with_capacity(n as usize);
                    for i in 0..n {
                        let Some(node) = rows.item(i) else {
                            row_keys.push((String::new(), String::new()));
                            continue;
                        };
                        let Ok(el) = node.dyn_into::<web_sys::Element>() else {
                            row_keys.push((String::new(), String::new()));
                            continue;
                        };
                        let s = el.get_attribute("data-schema").unwrap_or_default();
                        let nm = el.get_attribute("data-name").unwrap_or_default();
                        row_keys.push((s, nm));
                    }
                    let cur_key = tab.explorer_cursor_key.get_untracked();
                    // Locate cursor row in the filtered list.
                    let cursor_idx: Option<u32> = cur_key.as_ref().and_then(|k| {
                        row_keys.iter().position(|r| r == k).map(|i| i as u32)
                    });

                    if e.key() == "Enter" {
                        // No cursor placed → deliberate no-op.
                        let Some(idx) = cursor_idx else { return; };
                        e.prevent_default();
                        // Second-Enter descent: if the cursor row's
                        // detail is already loaded AND the active tab
                        // is Data, drop into DataBody mode (cell focus
                        // on row 0, col 0). Otherwise click the row to
                        // load the detail.
                        let cursor_already_loaded = cur_key.as_ref()
                            .zip(tab.explorer_selected.get_untracked())
                            .map(|(c, s)| c == &s)
                            .unwrap_or(false);
                        if cursor_already_loaded {
                            if let Some(detail) = active_detail {
                                match detail.active_tab.get_untracked() {
                                    DetailTab::Data => {
                                        if detail.data_selected_row.get_untracked().is_none() {
                                            detail.data_selected_row.set(Some(0));
                                        }
                                        detail.data_selected_col.set(Some(0));
                                        return;
                                    }
                                    DetailTab::Schema => {
                                        detail.schema_selected_row.set(Some(0));
                                        return;
                                    }
                                    // DDL / ERD have no body cursor model — second
                                    // Enter is a no-op (the user can still ←/→ to
                                    // a tab that does have one).
                                    _ => return,
                                }
                            }
                        }
                        // First Enter (or non-Data active tab) → click
                        // the row, which loads the detail.
                        if let Some(node) = rows.item(idx) {
                            if let Ok(el) = node.dyn_into::<web_sys::HtmlElement>() {
                                el.click();
                            }
                        }
                        return;
                    }

                    // Outer-list ArrowLeft / ArrowRight: walks the
                    // detail tab strip when the pane is open. There's
                    // no separate "tab-strip cursor" — clicking the
                    // sibling tab activates it (the underline moves
                    // with focus), which is the only cue the user
                    // needs.
                    if e.key() == "ArrowLeft" || e.key() == "ArrowRight" {
                        if !tab.explorer_detail_visible.get_untracked() { return; }
                        let Ok(tab_btns) = container.query_selector_all(".detail-tab-btn") else { return; };
                        let nb = tab_btns.length();
                        if nb == 0 { return; }
                        let mut active_btn_idx: Option<u32> = None;
                        for i in 0..nb {
                            let Some(node) = tab_btns.item(i) else { continue };
                            let Ok(el) = node.dyn_into::<web_sys::Element>() else { continue };
                            if el.class_list().contains("detail-tab-active") {
                                active_btn_idx = Some(i);
                                break;
                            }
                        }
                        let Some(start) = active_btn_idx else { return; };
                        let target = match e.key().as_str() {
                            "ArrowLeft"  => start.saturating_sub(1),
                            "ArrowRight" => (start + 1).min(nb - 1),
                            _ => return,
                        };
                        if target == start { e.prevent_default(); return; }
                        let Some(target_node) = tab_btns.item(target) else { return; };
                        let Ok(target_el) = target_node.dyn_into::<web_sys::HtmlElement>() else { return; };
                        target_el.click();
                        e.prevent_default();
                        return;
                    }

                    // ArrowUp / ArrowDown: pick the next row by index
                    // and write `cursor_key` — the reactive class:
                    // binding repaints the highlight, no DOM mutation
                    // needed.
                    let target_idx: u32 = match (cursor_idx, e.key().as_str()) {
                        (None, _)              => 0,
                        (Some(i), "ArrowDown") => (i + 1).min(n - 1),
                        (Some(i), "ArrowUp")   => i.saturating_sub(1),
                        _ => return,
                    };
                    if Some(target_idx) == cursor_idx { e.prevent_default(); return; }
                    let Some(target_key) = row_keys.get(target_idx as usize) else { return; };
                    if !target_key.0.is_empty() || !target_key.1.is_empty() {
                        tab.explorer_cursor_key.set(Some(target_key.clone()));
                    }
                    e.prevent_default();
                    if let Some(target_node) = rows.item(target_idx) {
                        if let Ok(target_el) = target_node.dyn_into::<web_sys::HtmlElement>() {
                            let opts = web_sys::ScrollIntoViewOptions::new();
                            opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
                            opts.set_behavior(web_sys::ScrollBehavior::Smooth);
                            target_el.scroll_into_view_with_scroll_into_view_options(&opts);
                        }
                    }
                }
                return;
            }
            // Every other shortcut requires Cmd (meta).
            if !e.meta_key() { return; }
            // Don't fire pane-nav shortcuts while actively renaming — the
            // input handles its own keys.
            if renaming_pane_id.get_untracked().is_some() { return; }
            let key = e.key();
            match key.as_str() {
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" if !e.shift_key() => {
                    if let Ok(n) = key.parse::<usize>() {
                        e.prevent_default();
                        jump_active_leaf_to(n - 1);
                    }
                }
                "[" if !e.shift_key() => { e.prevent_default(); active_leaf_step(-1); }
                "]" if !e.shift_key() => { e.prevent_default(); active_leaf_step(1); }
                "w" | "W" if e.shift_key() => {
                    e.prevent_default();
                    if let Some(pid) = active_pane_id.get_untracked() { close_pane(pid); }
                }
                "z" | "Z" if e.shift_key() => {
                    e.prevent_default();
                    reopen_last_closed();
                }
                _ => {}
            }
        });
        if let Some(win) = web_sys::window() {
            let _ = win.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
        }
        cb.forget();
    }

    // ── New connection form actions ───────────────────────────────────────────

    let open_new_conn = move |_| {
        nc_db_type.set(DbType::MySql);
        nc_name.set(String::new());
        nc_host.set("127.0.0.1".into());
        let mp = plane_for(DbType::MySql);
        nc_port.set(mp.default_port().to_string());
        nc_user.set(mp.default_user().into());
        nc_pass.set(String::new());
        nc_db.set(String::new());
        nc_default_pane.set("shell".into());
        nc_env.set(String::new());
        // The new connection lands in whichever folder the user most
        // recently clicked the header of (empty = root). Form has no
        // explicit "folder" input; placement is sidebar-managed.
        nc_folder_path.set(last_active_folder.get_untracked());
        // Block is the new-connection default — the standard SQL
        // formatting style users coming from sqlfluff / pgFormatter /
        // DBeaver expect. Existing connections keep whatever style
        // they were saved with.
        nc_format_style.set(FormatStyle::Block);
        // New connection defaults: unlimited buffer (empty string).
        // User opts in by typing a number into the form.
        nc_buffer_threshold_mb.set(String::new());
        nc_editor_command.set(String::new());
        editing_conn_id.set(None);
        nc_test_status.set(None);
        nc_db_list.set(vec![]);
        nc_schema.set(String::new());
        nc_schema_list.set(vec![]);
        nc_credential_source.set(CredentialSource::Plaintext);
        pane.set(Pane::NewConnection);
    };

    // Same target-folder semantics as `open_new_conn`: the new folder
    // becomes a child of whichever folder header the user most recently
    // clicked. Sends `SaveFolders` so the empty folder persists across
    // reload before any connection lives inside it.
    let open_new_folder = move |_| {
        new_folder_at(last_active_folder.get_untracked(), dialog, connection_folders, ws);
    };

    // Switching db_type in the form swaps port/user defaults iff the user
    // hasn't diverged from the other plane's defaults — we never clobber
    // something the user has typed.
    let pick_db_type = move |new_type: DbType| {
        let old = nc_db_type.get_untracked();
        if old == new_type { return; }
        let old_plane = plane_for(old);
        let new_plane = plane_for(new_type);
        if nc_port.get_untracked() == old_plane.default_port().to_string() {
            nc_port.set(new_plane.default_port().to_string());
        }
        if nc_user.get_untracked() == old_plane.default_user() {
            nc_user.set(new_plane.default_user().into());
        }
        nc_db_type.set(new_type);
        nc_db_list.set(vec![]);
        // Swapping db type invalidates any loaded schema list and the
        // credential-source picker (both PG-only concepts). MySQL always
        // uses the typed Password path.
        nc_schema.set(String::new());
        nc_schema_list.set(vec![]);
        nc_credential_source.set(CredentialSource::Plaintext);
    };

    let save_conn = move |_| {
        if nc_name.get().trim().is_empty() { return; }
        let db_type = nc_db_type.get();
        let plane = plane_for(db_type);
        // Plane-driven required-field validation. The button is also disabled
        // when this predicate is true — this is a defensive second check.
        if plane.database_required() && nc_db.get().trim().is_empty() { return; }
        let id = editing_conn_id.get().unwrap_or_else(|| Uuid::new_v4().to_string());
        let typed_pass = nc_pass.get();
        // Sentinel semantics on the wire (see ClientMessage::SaveConnection):
        //   - Some(s) → set the password to `s`. Used when the user
        //     typed something in the Edit modal's password input, OR
        //     this is a brand-new connection (no prior record to
        //     preserve from).
        //   - None    → preserve the password noctua has stored on disk
        //     for this id. Used when the Edit modal's password input is
        //     left blank — the placeholder hints this means "no
        //     change".
        //
        // For new connections (editing_conn_id == None), we send
        // Some(""), which writes an empty string and is distinguishable
        // from "no change" — matters when the user genuinely wants a
        // passwordless connection.
        // When the connection is set to `~/.pgpass`, the stored password
        // is unused — force it empty so switching an existing Plaintext
        // connection over to Pgpass drops the stale plaintext from disk
        // rather than letting it linger as a shadow.
        let uses_pgpass = db_type == DbType::Postgres
            && nc_credential_source.get() == CredentialSource::Pgpass;
        let password_change = if uses_pgpass {
            Some(String::new())
        } else if editing_conn_id.get_untracked().is_some() && typed_pass.is_empty() {
            None
        } else {
            Some(typed_pass)
        };
        ws_send(ws, &ClientMessage::SaveConnection {
            def: ConnectionDefPublic {
                id,
                name:     nc_name.get().trim().to_string(),
                db_type,
                host:     nc_host.get(),
                port:     nc_port.get().parse().unwrap_or_else(|_| plane.default_port()),
                user:     nc_user.get(),
                database: nc_db.get(),
                paths: normalize_folder_path(&nc_folder_path.get())
                    .map(|p| vec![p])
                    .unwrap_or_default(),
                default_pane: nc_default_pane.get(),
                env: {
                    let v = nc_env.get().trim().to_string();
                    if v.is_empty() { None } else { Some(v) }
                },
                format_style: nc_format_style.get(),
                editor_command: {
                    let raw = nc_editor_command.get();
                    let trimmed = raw.trim();
                    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
                },
                buffer_threshold_mb: {
                    let raw = nc_buffer_threshold_mb.get();
                    let trimmed = raw.trim();
                    if trimmed.is_empty() { None } else { trimmed.parse::<u32>().ok() }
                },
                default_schema: {
                    // PG-only. Empty / MySQL → None. Trim defends against
                    // accidental whitespace the user may type into the
                    // text-input fallback path.
                    if db_type != DbType::Postgres {
                        None
                    } else {
                        let raw = nc_schema.get();
                        let trimmed = raw.trim().to_string();
                        if trimmed.is_empty() { None } else { Some(trimmed) }
                    }
                },
                credential_source: {
                    // PG-only. MySQL is always Plaintext; force the value
                    // even if the form somehow left it dangling.
                    if db_type == DbType::Postgres {
                        nc_credential_source.get()
                    } else {
                        CredentialSource::Plaintext
                    }
                },
            },
            password_change,
            // No copy_from on the form save path. The Edit modal preserves
            // by id (handled by password_change=None falling through to
            // noctua's by-id lookup); the sidebar's "Copy" verb is the
            // only producer of copy_from=Some(_).
            copy_from: None,
        });
        editing_conn_id.set(None);
    };

    let cancel_new_conn = move |_| {
        editing_conn_id.set(None);
        if workspace_tabs.get().is_empty() { pane.set(Pane::Welcome); }
        else { pane.set(Pane::Shell); }
    };

    // ── View ──────────────────────────────────────────────────────────────────
    view! {
        <div class="app">

            // ── Disconnected banner ───────────────────────────────────────────
            // Surfaces a silent noctua death so the user doesn't sit confused
            // wondering why queries hang. Hidden when the WS is Connected; the
            // initial Connecting state stays hidden too so a normal cold-start
            // doesn't flash the banner. Auto-reconnect runs in the background
            // with exponential backoff (1s → 30s cap); the Reconnect button
            // is a manual override that bypasses the wait. On successful
            // reconnect every stale pane's session is re-established with
            // randomized jitter, so the ✕ pills self-heal as Connect
            // round-trips.
            {move || match ws_status.get() {
                WsStatus::Disconnected => view! {
                    <div class="ws-banner">
                        <span class="ws-banner-dot"/>
                        <span class="ws-banner-text">
                            "Disconnected from Noctua. Auto-reconnecting — open sessions will be restored once the connection is back."
                        </span>
                        <button class="ws-banner-btn"
                            on:click=move |_| connect_ws.call(())>
                            "Reconnect now"
                        </button>
                    </div>
                }.into_view(),
                _ => view! { <span/> }.into_view(),
            }}

            // ── Full-width app header ─────────────────────────────────────────
            <div class="app-header">
                // Sidebar toggle
                <button class="app-header-btn" title="Toggle sidebar"
                    on:click=move |_| sidebar_collapsed.update(|v| *v = !*v)>
                    <svg class="hdr-icon" viewBox="0 0 16 16">
                        <rect x="1" y="1" width="4" height="14" rx="1"/>
                        <rect x="7" y="1" width="8" height="14" rx="1" opacity="0.35"/>
                    </svg>
                </button>
                // Bell notification
                <div class="notif-anchor">
                    {move || {
                        // Badge total derives directly from the notifications
                        // queue. `.get()` subscribes the closure to the vec
                        // signal, so the badge re-renders on every push,
                        // retain, or clear — no manual reactivity plumbing.
                        let total = notifications.get().len();
                        view! {
                            <button class="app-header-btn notif-btn"
                                class:notif-btn-active=move || notif_open.get()
                                title="Notifications"
                                on:click=move |_| notif_open.update(|v| *v = !*v)>
                                <svg class="hdr-icon" viewBox="0 0 16 16">
                                    <path d="M8 1.5a4.5 4.5 0 0 0-4.5 4.5v2.379l-1 1.5V11h11v-1.121l-1-1.5V6A4.5 4.5 0 0 0 8 1.5zM6 12a2 2 0 1 0 4 0H6z"/>
                                </svg>
                                {if total > 0 {
                                    view! { <span class="notif-badge">{total}</span> }.into_view()
                                } else {
                                    view! { <span/> }.into_view()
                                }}
                            </button>
                        }
                    }}
                    // Notifications dropdown — one card per queued notification,
                    // rendered newest-first. Card body click jumps to the
                    // pane and clears that pane's notifications; the hover-
                    // revealed × dismisses just the single notification.
                    {move || if notif_open.get() {
                        let items: Vec<Notification> = notifications.get().iter().rev().cloned().collect();
                        view! {
                            <div class="notif-panel">
                                <div class="notif-panel-header">
                                    <span class="notif-panel-title">"Activity"</span>
                                    <button class="notif-clear-btn"
                                        on:click=move |_| {
                                            notifications.update(|v| v.clear());
                                            notif_open.set(false);
                                        }>"Clear all"</button>
                                </div>
                                {if items.is_empty() {
                                    view! { <div class="notif-empty">"No new activity"</div> }.into_view()
                                } else {
                                    items.into_iter().map(|n| {
                                        let notif_id    = n.id.clone();
                                        let pane_id_jump = n.pane_id.clone();
                                        let pane_id_clear = n.pane_id.clone();
                                        let (db_badge, db_badge_cls) = match n.db_type {
                                            DbType::MySql    => ("MY", "conn-db-badge conn-db-badge-my"),
                                            DbType::Postgres => ("PG", "conn-db-badge conn-db-badge-pg"),
                                        };
                                        let is_err = matches!(n.event, NotificationEvent::Err { .. });
                                        let (status_glyph, status_text) = match &n.event {
                                            NotificationEvent::Ok { rows_affected, exec_time_us, has_cols } => {
                                                let rows_label = if *has_cols {
                                                    if *rows_affected == 1 { "1 row".to_string() }
                                                    else { format!("{} rows", rows_affected) }
                                                } else if *rows_affected == 1 {
                                                    "1 row affected".to_string()
                                                } else {
                                                    format!("{} rows affected", rows_affected)
                                                };
                                                ("✓", format!("{} · {}ms", rows_label, exec_time_us / 1000))
                                            }
                                            NotificationEvent::Err { message } => {
                                                let first = message.lines().next().unwrap_or(message).to_string();
                                                ("⚠", first)
                                            }
                                        };
                                        let time_str = format_notification_time(n.created_at_ms);
                                        let title = format!("{} · {}", n.pane_label, n.conn_name);
                                        let sql_line = n.sql_snippet.clone().unwrap_or_default();
                                        view! {
                                            <div class="notif-item"
                                                class:notif-item-err=is_err
                                                on:click=move |_| {
                                                    focus_pane(pane_id_jump.clone());
                                                    notifications.update(|v| v.retain(|n| n.pane_id != pane_id_clear));
                                                    notif_open.set(false);
                                                }>
                                                <span class=format!("notif-item-icon {}", db_badge_cls)>
                                                    <span class="notif-item-icon-badge">{db_badge}</span>
                                                    <span class="notif-item-icon-dot"
                                                        class:notif-dot-err=is_err></span>
                                                </span>
                                                <span class="notif-item-title">{title}</span>
                                                <span class="notif-item-time">{time_str}</span>
                                                <button class="notif-item-dismiss"
                                                    title="Dismiss"
                                                    on:click=move |ev| {
                                                        use wasm_bindgen::JsCast;
                                                        let _ = ev.dyn_ref::<web_sys::Event>().map(|e| e.stop_propagation());
                                                        let nid = notif_id.clone();
                                                        notifications.update(|v| v.retain(|x| x.id != nid));
                                                    }>"×"</button>
                                                {if sql_line.is_empty() {
                                                    view! { <span class="notif-item-sql notif-item-sql-empty"></span> }.into_view()
                                                } else {
                                                    view! { <span class="notif-item-sql">{sql_line}</span> }.into_view()
                                                }}
                                                <span class="notif-item-status"
                                                    class:notif-status-err=is_err>
                                                    <span class="notif-status-glyph">{status_glyph}</span>
                                                    {" "}
                                                    {status_text}
                                                </span>
                                            </div>
                                        }
                                    }).collect_view().into_view()
                                }}
                            </div>
                        }.into_view()
                    } else {
                        view! { <div/> }.into_view()
                    }}
                </div>
                <button class="app-header-btn" title="Settings" aria-label="Settings"
                    class:app-header-btn-active=move || settings_ui.open.get()
                    on:click=move |_| settings_ui.show()>"⚙\u{FE0E}"</button>
                // Add connection
                <button class="app-header-btn" title="New connection" on:click=open_new_conn>"+"</button>
                // Title
                <span class="app-header-sep"></span>
                <span class="app-header-icon">"🦉"</span>
                <span class="app-header-title">"Parliament · Tyto"</span>
            </div>

            // ── App body: sidebar + workspace ─────────────────────────────────
            <div class="app-body">

                // Sidebar
                <div class="sidebar" class:sidebar-collapsed=move || sidebar_collapsed.get()>
                    <div class="sidebar-section">
                        <div class="sidebar-section-header">
                            <span class="sidebar-section-label">"Connections"</span>
                            // + button is a hover-dropdown trigger: click
                            // creates a new connection (fast path), hover
                            // reveals "New connection" / "New folder" so
                            // the user can choose. Both verbs default
                            // their target to `last_active_folder`.
                            <span class="sidebar-add-trigger"
                                  on:mouseenter=move |ev: web_sys::MouseEvent| {
                                      // Capture the trigger's viewport-space
                                      // bounding rect so the fixed-positioned
                                      // menu lands directly below the + button.
                                      use wasm_bindgen::JsCast;
                                      if let Some(el) = ev.current_target()
                                          .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                                      {
                                          let r = el.get_bounding_client_rect();
                                          add_menu_pos.set((r.left(), r.bottom()));
                                      }
                                      add_menu_open.set(true);
                                  }
                                  on:mouseleave=move |_| add_menu_open.set(false)>
                                <button class="sidebar-add-btn"
                                        on:click=open_new_conn
                                        title="New connection (hover for menu)">"+"</button>
                                {move || if add_menu_open.get() {
                                    let label_conn = {
                                        let p = last_active_folder.get();
                                        if p.is_empty() { "New connection".to_string() }
                                        else { format!("New connection in \u{201c}{}\u{201d}", p) }
                                    };
                                    let label_folder = {
                                        let p = last_active_folder.get();
                                        if p.is_empty() { "New folder".to_string() }
                                        else { format!("New folder in \u{201c}{}\u{201d}", p) }
                                    };
                                    let (x, y) = add_menu_pos.get();
                                    let pos_style = format!("left:{}px; top:{}px", x, y);
                                    view! {
                                        <div class="sidebar-add-menu" style=pos_style>
                                            <div class="sidebar-add-menu-item"
                                                 on:click=open_new_conn>
                                                {label_conn}
                                            </div>
                                            <div class="sidebar-add-menu-item"
                                                 on:click=open_new_folder>
                                                {label_folder}
                                            </div>
                                        </div>
                                    }.into_view()
                                } else {
                                    view! { <span/> }.into_view()
                                }}
                            </span>
                        </div>

                        {move || {
                            let conns = connections.get();
                            if conns.is_empty() {
                                return view! {
                                    <div class="sidebar-empty">"No connections yet."<br/>"Click + to add one."</div>
                                }.into_view();
                            }

                            // Per-row renderer shared by the tree and the
                            // working-set. Defined inline so it captures all
                            // App-level signals by Copy without a long free-
                            // function argument list. Returns one `.conn-item`
                            // for a given connection, indented by depth.
                            let render_conn_row = move |conn: ConnectionDefPublic, depth: usize, force_new: bool, source_path: String, target_wt_id: Option<String>| -> leptos::View {
                                let id           = conn.id.clone();
                                let id2          = id.clone();
                                let id3          = id.clone();
                                let id_edit      = id.clone();
                                let id_dot_busy  = id.clone();
                                let id_dot_conn  = id.clone();
                                let id_header    = id.clone();
                                let id_footer    = id.clone();
                                let id_drag      = id.clone();
                                // Show a low-contrast ⛓ next to the name when this
                                // connection is linked (>1 placement). Hover-tooltip
                                // names the count so the user knows what's at stake
                                // before editing.
                                let link_count = conn.paths.len();
                                let link_indicator = if link_count > 1 {
                                    let tip = format!("Linked: appears in {} folders", link_count);
                                    view! { <span class="conn-link-indicator" title=tip>"⛓"</span> }.into_view()
                                } else {
                                    view! { <span/> }.into_view()
                                };
                                // Encode the source placement on the dragstart so the drop
                                // handler can move just this placement (instead of clobbering
                                // every linked placement). Tab-separated so the legacy
                                // `text/plain = id` shape still works as a sub-case.
                                let drag_payload = format!("{}\t{}", id_drag, source_path);
                                let name         = conn.name.clone();
                                let conn_line1   = format!("{}@{}:{}", conn.user, conn.host, conn.port);
                                let conn_line2   = if conn.database.is_empty() { String::new() } else { conn.database.clone() };
                                let (badge_text, badge_class) = match conn.db_type {
                                    DbType::MySql    => ("MY", "conn-db-badge conn-db-badge-my"),
                                    DbType::Postgres => ("PG", "conn-db-badge conn-db-badge-pg"),
                                };
                                let indent_style = format!("padding-left: {}px", depth * 12 + 16);

                                let is_active = move || {
                                    active_pane_id.get()
                                        .and_then(|pid| shell_panes.get().get(&pid).map(|p| p.conn_id.clone()))
                                        .as_deref() == Some(&id3)
                                };
                                let is_busy_dot = move || shell_panes.get().values()
                                    .filter(|p| p.conn_id == id_dot_busy && p.kind == PaneKind::Shell)
                                    .any(|p| p.querying.get());
                                let is_conn_dot = move || shell_panes.get().values()
                                    .any(|p| p.conn_id == id_dot_conn && p.kind == PaneKind::Shell);

                                view! {
                                    <div class="conn-item"
                                         style=indent_style
                                         draggable="true"
                                         class:active=is_active
                                         on:click=move |_| {
                                             // Active Connections rows carry a specific
                                             // workspace-tab target so two open sessions on
                                             // the same connection navigate to distinct
                                             // tabs. Everything else (tree, Recents, All
                                             // Connections) routes through open_conn_tab.
                                             if let Some(wt_id_target) = target_wt_id.as_ref() {
                                                 let pid_opt = workspace_tabs.get().iter()
                                                     .find(|w| &w.id == wt_id_target)
                                                     .map(|w| w.layout.get_untracked().first_pane_id());
                                                 if let Some(pid) = pid_opt {
                                                     active_wt_id.set(Some(wt_id_target.clone()));
                                                     active_pane_id.set(Some(pid));
                                                     if pane.get_untracked() != Pane::Shell {
                                                         pane.set(Pane::Shell);
                                                     }
                                                 }
                                             } else {
                                                 open_conn_tab(id2.clone(), force_new);
                                             }
                                         }
                                         on:dragstart=move |ev| {
                                             if let Some(dt) = ev.data_transfer() {
                                                 let _ = dt.set_data("text/plain", &drag_payload);
                                             }
                                         }
                                         on:contextmenu=move |ev: web_sys::MouseEvent| {
                                             ev.prevent_default();
                                             sidebar_context_menu.set(Some(SidebarContextMenu {
                                                 x: ev.client_x() as f64,
                                                 y: ev.client_y() as f64,
                                                 target: SidebarMenuTarget::Connection {
                                                     conn_id: id_edit.clone(),
                                                     source_path: source_path.clone(),
                                                 },
                                             }));
                                         }>
                                        <div class="conn-item-header">
                                            <span class="conn-status-dot"
                                                class:conn-dot-busy=is_busy_dot
                                                class:conn-dot-connected=is_conn_dot
                                            />
                                            <span class=badge_class>{badge_text}</span>
                                            {link_indicator}
                                            <span class="conn-name">{name}</span>
                                            {move || {
                                                let panes: Vec<ShellTab> = shell_panes.get().values()
                                                    .filter(|p| p.conn_id == id_header)
                                                    .cloned().collect();
                                                let count = panes.len();
                                                let unread: usize = notifications.get().iter()
                                                    .filter(|n| n.conn_id == id_header)
                                                    .count();
                                                view! {
                                                    <>
                                                        {if count > 1 {
                                                            view! { <span class="conn-pane-count">{count}</span> }.into_view()
                                                        } else { view! { <span/> }.into_view() }}
                                                        {if unread > 0 {
                                                            view! { <span class="conn-unread-badge">{unread}</span> }.into_view()
                                                        } else { view! { <span/> }.into_view() }}
                                                    </>
                                                }
                                            }}
                                        </div>

                                        {move || {
                                            let panes: Vec<ShellTab> = shell_panes.get().values()
                                                .filter(|p| p.conn_id == id_footer)
                                                .cloned().collect();
                                            if panes.is_empty() {
                                                return view! {
                                                    <div class="conn-item-meta">{conn_line1.clone()}</div>
                                                    {if !conn_line2.is_empty() {
                                                        view! { <div class="conn-item-meta">{conn_line2.clone()}</div> }.into_view()
                                                    } else {
                                                        view! { <span/> }.into_view()
                                                    }}
                                                }.into_view();
                                            }
                                            let is_busy = panes.iter().any(|p| p.querying.get());
                                            let (status_str, is_err) = if is_busy {
                                                ("⚡ Executing…".to_string(), false)
                                            } else {
                                                let mut s = String::new();
                                                let mut e = false;
                                                for p in &panes {
                                                    match p.last_status.get() {
                                                        PaneStatus::Ok(t)  => { s = t; break; }
                                                        PaneStatus::Err(t) => { s = format!("✕ {}", t); e = true; break; }
                                                        _ => {}
                                                    }
                                                }
                                                if s.is_empty() { s = "Idle".to_string(); }
                                                (s, e)
                                            };
                                            view! {
                                                <>
                                                    <div class="conn-item-meta">{conn_line1.clone()}</div>
                                                    <div class="conn-item-status"
                                                        class:conn-status-busy=is_busy
                                                        class:conn-status-err=is_err>
                                                        {status_str}
                                                    </div>
                                                </>
                                            }.into_view()
                                        }}
                                    </div>
                                }.into_view()
                            };


                            // ── Upper: synthetic "All Connections" + top-level folders
                            // ALL CONNECTIONS is a flat list of every defined
                            // connection, sibling to user-defined folders (not a
                            // parent of them). A connection with a placement
                            // path "Prod/MySQL" appears both in ALL CONNECTIONS
                            // and nested under Prod > MySQL.
                            let folders_snapshot = connection_folders.get();
                            let tree = build_connection_tree(&conns, &folders_snapshot);
                            let collapsed_snapshot = collapsed_folders.get();
                            let mut rows = Vec::new();

                            // ── Active Connections (virtual folder) ──────
                            // One row PER WORKSPACE TAB — not per connection.
                            // Two MySQL workspace tabs surface as two distinct
                            // rows so the user can navigate to either session
                            // independently. The row's identity is the
                            // workspace tab's `wt.id`; clicking it sets
                            // `active_wt_id` to that id and activates the
                            // workspace tab's first pane. We resolve the
                            // connection by looking at the first pane in each
                            // workspace tab — by convention each tab is bound
                            // to one connection. ALWAYS shown (even when
                            // empty) so the section's position is stable.
                            let panes_snap = shell_panes.get();
                            let wts_snap   = workspace_tabs.get();
                            let mut active_rows: Vec<(String, ConnectionDefPublic)> = wts_snap.iter()
                                .filter_map(|wt| {
                                    let first = wt.layout.get_untracked().first_pane_id();
                                    let conn_id = panes_snap.get(&first).map(|p| p.conn_id.clone())?;
                                    let conn = conns.iter().find(|c| c.id == conn_id).cloned()?;
                                    Some((wt.id.clone(), conn))
                                })
                                .collect();
                            // Order matches the workspace-tab strip's left-to-
                            // right order — same as wts_snap iteration, no sort.
                            // (Alphabetical-by-name would shuffle adjacent tabs
                            // of the same connection unpredictably.)
                            rows.push(SidebarRow::Folder {
                                name: "Active Connections".into(),
                                path: "@@active".into(),
                                depth: 0,
                                is_root: true,
                            });
                            if !collapsed_snapshot.contains("@@active") {
                                for (wt_id, conn) in active_rows.drain(..) {
                                    rows.push(SidebarRow::Connection {
                                        conn,
                                        depth:        1,
                                        source_path:  String::new(),
                                        force_new:    false,
                                        target_wt_id: Some(wt_id),
                                    });
                                }
                            }

                            // ── Recents (virtual folder) ─────────────────
                            // Auto-populated from open gestures, capped at
                            // RECENTS_CAP. The path "@@recents" is a sentinel
                            // — the new-folder prompt won't generate it by
                            // accident (users can technically type it but
                            // the consequence is just a styling collision,
                            // not a logic break). Hidden entirely when the
                            // resolved row list is empty (e.g., fresh user,
                            // or every recent's underlying connection was
                            // deleted).
                            let recents_snapshot = recents.get();
                            let recent_rows: Vec<ConnectionDefPublic> = recents_snapshot.iter()
                                .filter_map(|r| conns.iter().find(|c| c.id == r.conn_id).cloned())
                                .collect();
                            if !recent_rows.is_empty() {
                                rows.push(SidebarRow::Folder {
                                    name: "Recents".into(),
                                    path: "@@recents".into(),
                                    depth: 0,
                                    is_root: true,
                                });
                                if !collapsed_snapshot.contains("@@recents") {
                                    for conn in recent_rows {
                                        // Recents is a view, not a folder —
                                        // empty source_path so drag from here
                                        // adds a placement rather than moving.
                                        rows.push(SidebarRow::Connection {
                                            conn,
                                            depth:        1,
                                            source_path:  String::new(),
                                            force_new:    true,
                                            target_wt_id: None,
                                        });
                                    }
                                }
                            }

                            rows.push(SidebarRow::Folder {
                                name: "All Connections".into(),
                                path: String::new(),
                                depth: 0,
                                is_root: true,
                            });
                            if !collapsed_snapshot.contains("") {
                                let mut all = conns.clone();
                                all.sort_by_key(|a| a.name.to_lowercase());
                                for conn in all {
                                    // Rows under the synthetic "All Connections" view
                                    // carry empty source_path — drag from here ADDS a
                                    // placement (link) rather than moves one, since the
                                    // synthetic view doesn't represent a real folder.
                                    rows.push(SidebarRow::Connection {
                                        conn,
                                        depth:        1,
                                        source_path:  String::new(),
                                        force_new:    true,
                                        target_wt_id: None,
                                    });
                                }
                            }
                            for child in tree.children.values() {
                                flatten_tree(child, 0, false, &collapsed_snapshot, &mut rows);
                            }

                            let tree_views: Vec<leptos::View> = rows.into_iter().map(|row| match row {
                                SidebarRow::Folder { name, path, depth, is_root } => {
                                    let path_for_click = path.clone();
                                    let path_for_class = path.clone();
                                    let path_for_over  = path.clone();
                                    let path_for_drop  = path.clone();
                                    let path_for_ctx   = path.clone();
                                    let indent_px      = depth * 12 + 8;
                                    let header_class   = if is_root { "folder-node folder-node-root" } else { "folder-node" };
                                    // Virtual folders (Recents and Active Connections) are
                                    // views, not real folders: no right-click menu (none of
                                    // the verbs apply), no drop target (you can't move /
                                    // copy into a view), no rename / delete.
                                    let is_virtual = path == "@@recents" || path == "@@active";
                                    view! {
                                        <div class=header_class
                                             class:folder-drop-target=move ||
                                                 !is_virtual &&
                                                 drop_target_path.get().as_deref() == Some(path_for_class.as_str())
                                             style=format!("padding-left: {}px", indent_px)
                                             on:click=move |_| {
                                                 let p = path_for_click.clone();
                                                 // Track the active folder so the sidebar's +
                                                 // button can default its target. Virtual
                                                 // headers (Recents) and the synthetic "All
                                                 // Connections" root both reset to "" (root)
                                                 // — neither is a real folder you can place
                                                 // a new connection or sub-folder inside of.
                                                 if is_virtual || p.is_empty() {
                                                     last_active_folder.set(String::new());
                                                 } else {
                                                     last_active_folder.set(p.clone());
                                                 }
                                                 collapsed_folders.update(|s| {
                                                     if !s.remove(&p) { s.insert(p); }
                                                 });
                                             }
                                             on:contextmenu=move |ev: web_sys::MouseEvent| {
                                                 if is_virtual {
                                                     // Still suppress the browser-native
                                                     // context menu so it doesn't appear
                                                     // in place of our app menu.
                                                     ev.prevent_default();
                                                     return;
                                                 }
                                                 ev.prevent_default();
                                                 sidebar_context_menu.set(Some(SidebarContextMenu {
                                                     x: ev.client_x() as f64,
                                                     y: ev.client_y() as f64,
                                                     target: SidebarMenuTarget::Folder {
                                                         path: path_for_ctx.clone(),
                                                         is_root,
                                                     },
                                                 }));
                                             }
                                             on:dragover=move |ev| {
                                                 ev.prevent_default();
                                                 if drop_target_path.get_untracked().as_deref()
                                                     != Some(path_for_over.as_str())
                                                 {
                                                     drop_target_path.set(Some(path_for_over.clone()));
                                                 }
                                             }
                                             on:dragleave=move |_| {
                                                 drop_target_path.set(None);
                                             }
                                             on:drop=move |ev| {
                                                 ev.prevent_default();
                                                 drop_target_path.set(None);
                                                 // Virtual folders (Recents) are views, not real
                                                 // folders — drops on them are silent no-ops.
                                                 if is_virtual { return; }
                                                 // Drag payload is `{conn_id}\t{source_path}`.
                                                 // Rows from the synthetic "All Connections" view
                                                 // carry source_path = "" — those add a placement
                                                 // rather than move one (the synthetic view always
                                                 // reflects every connection, regardless of paths).
                                                 let raw = ev.data_transfer()
                                                     .and_then(|dt| dt.get_data("text/plain").ok())
                                                     .unwrap_or_default();
                                                 if raw.is_empty() { return; }
                                                 let mut parts = raw.splitn(2, '\t');
                                                 let dropped_id = parts.next().unwrap_or("").to_string();
                                                 let source_path = parts.next().unwrap_or("").to_string();
                                                 if dropped_id.is_empty() { return; }
                                                 let dest_path = if is_root {
                                                     String::new()
                                                 } else {
                                                     path_for_drop.clone()
                                                 };
                                                 // No-op when the source and target are the same
                                                 // folder — avoids burning a SaveConnection round-trip
                                                 // on a drag that didn't change anything.
                                                 if source_path == dest_path { return; }
                                                 if let Some(mut def) = connections.get()
                                                     .into_iter()
                                                     .find(|c| c.id == dropped_id)
                                                 {
                                                     // Move semantics: remove the source placement
                                                     // (if any), then add the dest placement (if
                                                     // any). Other linked placements stay intact.
                                                     if !source_path.is_empty() {
                                                         def.paths.retain(|p| p != &source_path);
                                                     }
                                                     if !dest_path.is_empty() && !def.paths.contains(&dest_path) {
                                                         def.paths.push(dest_path);
                                                     }
                                                     ws_send(ws, &ClientMessage::SaveConnection { def, password_change: None, copy_from: None });
                                                 }
                                             }>
                                            <span class="folder-name">{name}</span>
                                        </div>
                                    }.into_view()
                                }
                                SidebarRow::Connection { conn, depth, source_path, force_new, target_wt_id } => {
                                    render_conn_row(conn, depth, force_new, source_path, target_wt_id)
                                }
                            }).collect();


                            view! {
                                <>
                                    {tree_views.into_iter().collect_view()}
                                </>
                            }.into_view()
                        }}
                    </div>
                </div>

                // ── Workspace: tab bar + content ──────────────────────────────
                <div class="workspace">

                    // Tab bar — only shown for the New/Edit Connection form
                    {move || {
                        let cur_pane = pane.get();
                        if cur_pane != Pane::NewConnection { return view! { <div/> }.into_view(); }
                        view! {
                            <div class="tab-bar">
                                <div class="tab-list">
                                    <div class="tab tab-active">
                                        <span class="tab-label">{move || if editing_conn_id.get().is_some() { "Edit Connection" } else { "New Connection" }}</span>
                                        <span class="tab-close" on:click=move |_| {
                                            editing_conn_id.set(None);
                                            if workspace_tabs.get().is_empty() { pane.set(Pane::Welcome); }
                                            else { pane.set(Pane::Shell); }
                                        }>"×"</span>
                                    </div>
                                </div>
                            </div>
                        }.into_view()
                    }}

                    // Tab content
                    <div class="tab-content">
                        {move || match pane.get() {

                            Pane::Welcome => view! {
                                <div class="welcome">
                                    <div class="welcome-glyph">"🦉"</div>
                                    <div class="welcome-title">"Parliament · Tyto"</div>
                                    <div class="welcome-hint">"Select a connection or click + to create one"</div>
                                </div>
                            }.into_view(),

                            Pane::NewConnection => view! {
                                <div class="conn-form-pane">
                                    <div class="conn-form-title">{move || if editing_conn_id.get().is_some() { "Edit Connection" } else { "New Connection" }}</div>
                                    // When editing a linked connection (one record
                                    // placed in multiple folders), surface that the
                                    // change propagates to every placement.
                                    {move || {
                                        let Some(id) = editing_conn_id.get() else { return view! { <span/> }.into_view(); };
                                        let n = connections.get().iter()
                                            .find(|c| c.id == id)
                                            .map(|c| c.paths.len())
                                            .unwrap_or(0);
                                        if n <= 1 { return view! { <span/> }.into_view(); }
                                        view! {
                                            <div class="conn-form-link-note">
                                                {format!("⛓ Linked — appears in {} folders. Edits apply everywhere.", n)}
                                            </div>
                                        }.into_view()
                                    }}

                                    // ── DB type segmented selector ─────────
                                    <div class="form-row">
                                        <label class="form-label">"Engine"</label>
                                        {move || {
                                            if editing_conn_id.get().is_some() {
                                                // Read-only: show the type as a label
                                                let label = if nc_db_type.get() == DbType::MySql { "MySQL" } else { "PostgreSQL" };
                                                view! { <div class="form-db-type-label">{label}</div> }.into_view()
                                            } else {
                                                // Editable: show the segmented buttons
                                                view! {
                                                    <div class="dbtype-seg">
                                                        <button type="button"
                                                            class="dbtype-seg-btn"
                                                            class:dbtype-seg-active=move || nc_db_type.get() == DbType::MySql
                                                            on:click=move |_| pick_db_type(DbType::MySql)>
                                                            "MySQL"
                                                        </button>
                                                        <button type="button"
                                                            class="dbtype-seg-btn"
                                                            class:dbtype-seg-active=move || nc_db_type.get() == DbType::Postgres
                                                            on:click=move |_| pick_db_type(DbType::Postgres)>
                                                            "PostgreSQL"
                                                        </button>
                                                    </div>
                                                }.into_view()
                                            }
                                        }}
                                    </div>

                                    <div class="form-row">
                                        <label class="form-label">"Name"</label>
                                        <input class="form-input" type="text" placeholder="e.g. Local MySQL"
                                            prop:value=move || nc_name.get()
                                            on:input=move |e| nc_name.set(event_target_value(&e)) />
                                    </div>
                                    <div class="form-row-inline">
                                        <div class="form-row">
                                            <label class="form-label">"Host"</label>
                                            <input class="form-input" type="text"
                                                prop:value=move || nc_host.get()
                                                on:input=move |e| nc_host.set(event_target_value(&e)) />
                                        </div>
                                        <div class="form-row" style="max-width:90px">
                                            <label class="form-label">"Port"</label>
                                            <input class="form-input" type="text"
                                                prop:value=move || nc_port.get()
                                                on:input=move |e| nc_port.set(event_target_value(&e)) />
                                        </div>
                                    </div>
                                    <div class="form-row-inline">
                                        <div class="form-row">
                                            <label class="form-label">"User"</label>
                                            <input class="form-input" type="text"
                                                prop:value=move || nc_user.get()
                                                on:input=move |e| nc_user.set(event_target_value(&e)) />
                                        </div>
                                        // Password field disappears entirely when the PG
                                        // connection is set to source its password from
                                        // ~/.pgpass — matches the "browser holds no
                                        // credentials" invariant and prevents accidental
                                        // typed-password leaks into a Pgpass-typed save.
                                        {move || {
                                            let hide = nc_db_type.get() == DbType::Postgres
                                                && nc_credential_source.get() == CredentialSource::Pgpass;
                                            if hide { return ().into_view(); }
                                            view! {
                                                <div class="form-row">
                                                    <label class="form-label">"Password"</label>
                                                    <input class="form-input" type="password"
                                                        prop:placeholder=move || if editing_conn_id.get().is_some() {
                                                            "(unchanged — leave blank to keep current)"
                                                        } else {
                                                            ""
                                                        }
                                                        prop:value=move || nc_pass.get()
                                                        on:input=move |e| nc_pass.set(event_target_value(&e)) />
                                                </div>
                                            }.into_view()
                                        }}
                                    </div>
                                    // Auth-source segmented control — PG only. Mirrors
                                    // the db_type segmented button pattern
                                    // (.dbtype-seg / .dbtype-seg-btn / .dbtype-seg-active).
                                    // Toggling to `~/.pgpass` hides the password input
                                    // above; noctua resolves the actual password at
                                    // connect time.
                                    {move || {
                                        if nc_db_type.get() != DbType::Postgres {
                                            return ().into_view();
                                        }
                                        view! {
                                            <div class="form-row">
                                                <label class="form-label">"Auth"</label>
                                                <div class="dbtype-seg">
                                                    <button type="button" class="dbtype-seg-btn"
                                                        class:dbtype-seg-active=move || nc_credential_source.get() == CredentialSource::Plaintext
                                                        on:click=move |_| nc_credential_source.set(CredentialSource::Plaintext)>
                                                        "Password"
                                                    </button>
                                                    <button type="button" class="dbtype-seg-btn"
                                                        class:dbtype-seg-active=move || nc_credential_source.get() == CredentialSource::Pgpass
                                                        on:click=move |_| nc_credential_source.set(CredentialSource::Pgpass)>
                                                        "\u{007E}/.pgpass"
                                                    </button>
                                                </div>
                                            </div>
                                        }.into_view()
                                    }}
                                    {move || {
                                        let dbs = nc_db_list.get();
                                        if dbs.is_empty() {
                                            // Text input (current behavior) + Load button
                                            view! {
                                                <div class="form-row">
                                                    <label class="form-label">
                                                        {move || if plane_for(nc_db_type.get()).database_required() {
                                                            "Database *"
                                                        } else {
                                                            "Database"
                                                        }}
                                                    </label>
                                                    <div style="display:flex;gap:6px;flex:1">
                                                        <input class="form-input" style="flex:1" type="text"
                                                            prop:placeholder=move || if plane_for(nc_db_type.get()).database_required() {
                                                                "(required)"
                                                            } else {
                                                                "(optional)"
                                                            }
                                                            prop:value=move || nc_db.get()
                                                            on:input=move |e| nc_db.set(event_target_value(&e)) />
                                                        <button class="btn-load-dbs" title="Load database list from server"
                                                            on:click=move |_| {
                                                                let plane = plane_for(nc_db_type.get());
                                                                ws_send(ws, &ClientMessage::ListDatabases {
                                                                    def: ConnectionDef {
                                                                        // Use the editing connection's id when present so
                                                                        // noctua's blank-password sentinel can look up the
                                                                        // saved password (Edit modal starts with nc_pass
                                                                        // blank). For a brand-new connection it's empty —
                                                                        // the form's typed password is used directly.
                                                                        id: editing_conn_id.get_untracked().unwrap_or_default(),
                                                                        name: String::new(),
                                                                        db_type: nc_db_type.get(),
                                                                        host: nc_host.get(),
                                                                        port: nc_port.get().parse().unwrap_or(plane.default_port()),
                                                                        user: nc_user.get(),
                                                                        password: nc_pass.get(),
                                                                        database: String::new(),
                                                                        paths: vec![],
                                                                        default_pane: "shell".into(),
                                                                        env: None,
                                                                        format_style: FormatStyle::Unformatted,
                                                                        editor_command: None,
                                                                        buffer_threshold_mb: None,
                                                                        default_schema: None,
                                                                        credential_source: nc_credential_source.get(),
                                                                    },
                                                                    client_tab_id: None,
                                                                });
                                                            }>"\u{27F3}"</button>
                                                    </div>
                                                </div>
                                            }.into_view()
                                        } else {
                                            // Dropdown populated from server
                                            view! {
                                                <div class="form-row">
                                                    <label class="form-label">"Database"</label>
                                                    <div style="display:flex;gap:6px;flex:1">
                                                        <select class="form-input" style="flex:1"
                                                            on:change=move |e| {
                                                                nc_db.set(event_target_value(&e));
                                                                // Changing the database invalidates any previously-
                                                                // loaded schema list (pg_namespace is per-database).
                                                                nc_schema_list.set(vec![]);
                                                                nc_schema.set(String::new());
                                                            }>
                                                            <option value="">"(select a database)"</option>
                                                            {dbs.into_iter().map(|db| {
                                                                let selected = nc_db.get() == db;
                                                                let db2 = db.clone();
                                                                view! { <option value=db prop:selected=selected>{db2}</option> }
                                                            }).collect_view()}
                                                        </select>
                                                        <button class="btn-load-dbs" title="Refresh database list"
                                                            on:click=move |_| {
                                                                let plane = plane_for(nc_db_type.get());
                                                                ws_send(ws, &ClientMessage::ListDatabases {
                                                                    def: ConnectionDef {
                                                                        // Use the editing connection's id when present so
                                                                        // noctua's blank-password sentinel can look up the
                                                                        // saved password (Edit modal starts with nc_pass
                                                                        // blank). For a brand-new connection it's empty —
                                                                        // the form's typed password is used directly.
                                                                        id: editing_conn_id.get_untracked().unwrap_or_default(),
                                                                        name: String::new(),
                                                                        db_type: nc_db_type.get(),
                                                                        host: nc_host.get(),
                                                                        port: nc_port.get().parse().unwrap_or(plane.default_port()),
                                                                        user: nc_user.get(),
                                                                        password: nc_pass.get(),
                                                                        database: String::new(),
                                                                        paths: vec![],
                                                                        default_pane: "shell".into(),
                                                                        env: None,
                                                                        format_style: FormatStyle::Unformatted,
                                                                        editor_command: None,
                                                                        buffer_threshold_mb: None,
                                                                        default_schema: None,
                                                                        credential_source: nc_credential_source.get(),
                                                                    },
                                                                    client_tab_id: None,
                                                                });
                                                            }>"\u{27F3}"</button>
                                                    </div>
                                                </div>
                                            }.into_view()
                                        }
                                    }}
                                    // Schema picker — PG only. Mirrors the database row's
                                    // text-input/dropdown duality. The ↻ button fires
                                    // ListSchemas using the currently-selected database;
                                    // empty database produces a clean error from noctua.
                                    {move || {
                                        if nc_db_type.get() != DbType::Postgres {
                                            return ().into_view();
                                        }
                                        let schemas = nc_schema_list.get();
                                        if schemas.is_empty() {
                                            view! {
                                                <div class="form-row">
                                                    <label class="form-label">"Schema"</label>
                                                    <div style="display:flex;gap:6px;flex:1">
                                                        <input class="form-input" style="flex:1" type="text"
                                                            prop:placeholder="(optional — default schema on connect)"
                                                            prop:value=move || nc_schema.get()
                                                            on:input=move |e| nc_schema.set(event_target_value(&e)) />
                                                        <button class="btn-load-dbs" title="Load schema list from the chosen database"
                                                            on:click=move |_| {
                                                                let plane = plane_for(nc_db_type.get());
                                                                ws_send(ws, &ClientMessage::ListSchemas {
                                                                    def: ConnectionDef {
                                                                        id: editing_conn_id.get_untracked().unwrap_or_default(),
                                                                        name: String::new(),
                                                                        db_type: nc_db_type.get(),
                                                                        host: nc_host.get(),
                                                                        port: nc_port.get().parse().unwrap_or(plane.default_port()),
                                                                        user: nc_user.get(),
                                                                        password: nc_pass.get(),
                                                                        database: nc_db.get(),
                                                                        paths: vec![],
                                                                        default_pane: "shell".into(),
                                                                        env: None,
                                                                        format_style: FormatStyle::Unformatted,
                                                                        editor_command: None,
                                                                        buffer_threshold_mb: None,
                                                                        default_schema: None,
                                                                        credential_source: nc_credential_source.get(),
                                                                    },
                                                                    client_tab_id: None,
                                                                });
                                                            }>"\u{27F3}"</button>
                                                    </div>
                                                </div>
                                            }.into_view()
                                        } else {
                                            view! {
                                                <div class="form-row">
                                                    <label class="form-label">"Schema"</label>
                                                    <div style="display:flex;gap:6px;flex:1">
                                                        <select class="form-input" style="flex:1"
                                                            on:change=move |e| nc_schema.set(event_target_value(&e))>
                                                            <option value="">"(no preference)"</option>
                                                            {schemas.into_iter().map(|s| {
                                                                let selected = nc_schema.get() == s;
                                                                let s2 = s.clone();
                                                                view! { <option value=s prop:selected=selected>{s2}</option> }
                                                            }).collect_view()}
                                                        </select>
                                                        <button class="btn-load-dbs" title="Refresh schema list"
                                                            on:click=move |_| {
                                                                let plane = plane_for(nc_db_type.get());
                                                                ws_send(ws, &ClientMessage::ListSchemas {
                                                                    def: ConnectionDef {
                                                                        id: editing_conn_id.get_untracked().unwrap_or_default(),
                                                                        name: String::new(),
                                                                        db_type: nc_db_type.get(),
                                                                        host: nc_host.get(),
                                                                        port: nc_port.get().parse().unwrap_or(plane.default_port()),
                                                                        user: nc_user.get(),
                                                                        password: nc_pass.get(),
                                                                        database: nc_db.get(),
                                                                        paths: vec![],
                                                                        default_pane: "shell".into(),
                                                                        env: None,
                                                                        format_style: FormatStyle::Unformatted,
                                                                        editor_command: None,
                                                                        buffer_threshold_mb: None,
                                                                        default_schema: None,
                                                                        credential_source: nc_credential_source.get(),
                                                                    },
                                                                    client_tab_id: None,
                                                                });
                                                            }>"\u{27F3}"</button>
                                                    </div>
                                                </div>
                                            }.into_view()
                                        }
                                    }}
                                    <div class="form-row">
                                        <label class="form-label">"Opens with"</label>
                                        <select class="form-input"
                                            on:change=move |e| nc_default_pane.set(event_target_value(&e))>
                                            <option value="shell"    selected=move || nc_default_pane.get() == "shell">"Shell"</option>
                                            <option value="schema"   selected=move || nc_default_pane.get() == "schema">"Schema Explorer"</option>
                                            <option value="activity" selected=move || nc_default_pane.get() == "activity">"Activity"</option>
                                        </select>
                                    </div>
                                    <div class="form-row">
                                        <label class="form-label">"Format style"</label>
                                        <select class="form-input"
                                            on:change=move |e| {
                                                let v = event_target_value(&e);
                                                nc_format_style.set(match v.as_str() {
                                                    "river"  => FormatStyle::River,
                                                    "block"  => FormatStyle::Block,
                                                    "inline" => FormatStyle::Inline,
                                                    _        => FormatStyle::Unformatted,
                                                });
                                            }>
                                            <option value="unformatted" selected=move || nc_format_style.get() == FormatStyle::Unformatted>"Unformatted"</option>
                                            <option value="inline"      selected=move || nc_format_style.get() == FormatStyle::Inline>"Inline"</option>
                                            <option value="block"       selected=move || nc_format_style.get() == FormatStyle::Block>"Block"</option>
                                            <option value="river"       selected=move || nc_format_style.get() == FormatStyle::River>"River"</option>
                                        </select>
                                    </div>
                                    <div class="form-hint" style="font-size:11px;color:var(--text-muted);margin:-4px 0 8px 0">
                                        "Default style for the shell's right-click \u{201c}Format\u{201d} menu. Block: left-aligned SELECT/FROM/WHERE, content indented underneath — the most common SQL style. River: right-aligned keywords padded to col 6, one column per line. Inline: collapse the whole statement to a single line."
                                    </div>
                                    <div class="form-row">
                                        <label class="form-label">"Environment"</label>
                                        <input class="form-input" type="text"
                                            placeholder="prod, staging, dev, test (optional)"
                                            prop:value=move || nc_env.get()
                                            on:input=move |e| nc_env.set(event_target_value(&e)) />
                                    </div>
                                    <div class="form-hint" style="font-size:11px;color:var(--text-muted);margin:-4px 0 8px 0">
                                        "prod → red, staging → yellow, dev → green, test → purple"
                                    </div>
                                    <div class="form-row">
                                        <label class="form-label">"Editor override (optional)"</label>
                                        <input class="form-input" type="text"
                                            placeholder="(blank — use Settings › Editor)"
                                            prop:value=move || nc_editor_command.get()
                                            on:input=move |e| nc_editor_command.set(event_target_value(&e)) />
                                    </div>
                                    <div class="form-hint" style="font-size:11px;color:var(--text-muted);margin:-4px 0 8px 0">
                                        "For `\\e` / `\\ef` / `\\ev` on this connection only; the editor everywhere else is in Settings (⚙). Known GUI editors run directly and need their wait flag (`code -w`, `subl -w`, `mvim -f`); anything else opens in a terminal window. Quote paths with spaces."
                                    </div>
                                    <div class="form-row">
                                        <label class="form-label">"Max buffer threshold (MB)"</label>
                                        <input class="form-input" type="number"
                                            min="1"
                                            placeholder="(leave blank — no cleanup)"
                                            prop:value=move || nc_buffer_threshold_mb.get()
                                            on:input=move |e| nc_buffer_threshold_mb.set(event_target_value(&e)) />
                                    </div>
                                    <div class="form-hint" style="font-size:11px;color:var(--text-muted);margin:-4px 0 8px 0">
                                        "Not a pre-allocated buffer size — a trigger value. After each query completes on a shell pane, if the pane's accumulated history exceeds this threshold, the oldest block is evicted to reclaim memory. Each shell pane opened from this connection has its own independent buffer."
                                    </div>
                                    <div class="form-actions">
                                        <button class="btn btn-primary"
                                            disabled=move || {
                                                if nc_name.get().trim().is_empty() { return true; }
                                                plane_for(nc_db_type.get()).database_required()
                                                    && nc_db.get().trim().is_empty()
                                            }
                                            on:click=save_conn>
                                            "Save Connection"
                                        </button>
                                        <button class="btn-test-conn" on:click=move |_| {
                                            let plane = plane_for(nc_db_type.get());
                                            nc_test_status.set(Some((true, "Testing...".into())));
                                            ws_send(ws, &ClientMessage::TestConnection {
                                                def: ConnectionDef {
                                                    // Use the editing connection's id when present so noctua's
                                                    // blank-password sentinel can look up the saved password — the
                                                    // Edit modal starts with nc_pass blank, so without this the
                                                    // test always fails until the user retypes the password.
                                                    // Empty id for a brand-new connection.
                                                    id: editing_conn_id.get_untracked().unwrap_or_default(),
                                                    name: nc_name.get(),
                                                    db_type: nc_db_type.get(),
                                                    host: nc_host.get(),
                                                    port: nc_port.get().parse().unwrap_or(plane.default_port()),
                                                    user: nc_user.get(),
                                                    password: nc_pass.get(),
                                                    database: nc_db.get(),
                                                    paths: vec![],
                                                    default_pane: "shell".into(),
                                                    env: None,
                                                    format_style: FormatStyle::Unformatted,
                                                    editor_command: None,
                                                    buffer_threshold_mb: None,
                                                    default_schema: None,
                                                    credential_source: nc_credential_source.get(),
                                                },
                                            });
                                        }>"Test Connection"</button>
                                        {move || match nc_test_status.get() {
                                            Some((true, ref msg)) if msg == "Testing..." => view! { <span class="test-status test-pending">{msg.clone()}</span> }.into_view(),
                                            Some((true, msg)) => view! { <span class="test-status test-ok">{format!("\u{2713} {}", msg)}</span> }.into_view(),
                                            Some((false, msg)) => view! { <span class="test-status test-fail">{format!("\u{2717} {}", msg)}</span> }.into_view(),
                                            None => view! { <span/> }.into_view(),
                                        }}
                                        <button class="btn btn-ghost" on:click=cancel_new_conn>"Cancel"</button>
                                        {move || {
                                            let eid = editing_conn_id.get();
                                            if let Some(id) = eid {
                                                view! {
                                                    <button class="btn-delete-conn" on:click=move |_| {
                                                        ws_send(ws, &ClientMessage::DeleteConnection { id: id.clone() });
                                                        editing_conn_id.set(None);
                                                        if workspace_tabs.get().is_empty() { pane.set(Pane::Welcome); }
                                                        else { pane.set(Pane::Shell); }
                                                    }>"Delete Connection"</button>
                                                }.into_view()
                                            } else {
                                                view! { <span/> }.into_view()
                                            }
                                        }}
                                    </div>
                                </div>
                            }.into_view(),

                            // The workspaces render below, mounted whatever shows.
                            Pane::Shell => ().into_view(),
                        }}
                        // Workspaces stay mounted while the connection form
                        // shows: unmounting them rebuilt every pane (scrollback
                        // back to the top) when the form closed.
                        <div style=move || if pane.get() == Pane::Shell {
                            "display:flex;flex-direction:column;flex:1;overflow:hidden;min-width:0;min-height:0"
                        } else {
                            "display:none"
                        }>
                            <For
                                each=move || workspace_tabs.get()
                                key=|wt| wt.id.clone()
                                children=move |wt| {
                                    let wt_id = wt.id.clone();
                                    let layout = wt.layout;
                                    let zoomed = wt.zoomed_pane_id;
                                    let placement = create_memo(move |_| {
                                        zoomed.with(|z| layout.with(|tree| pane_layout::place(tree, z.as_deref())))
                                    });
                                    // The connection header follows the first leaf's
                                    // active pane.
                                    let first_pane = create_memo(move |_| {
                                        placement.with(|p| p.leaves.first().map(|(leaf, _)| *leaf))
                                            .and_then(|leaf| leaf.active_pane_id())
                                    });
                                    let header_view = move || {
                                        let first_pid = first_pane.get().unwrap_or_default();
                                        let conn_header = shell_panes.get_untracked().get(&first_pid)
                                            .and_then(|tab| {
                                                let conn_id = tab.conn_id.clone();
                                                let db_type = tab.db_type;
                                                connections.get().into_iter().find(|c| c.id == conn_id).map(|c| (c, db_type))
                                            });
                                        let Some((conn, db_type)) = conn_header else {
                                            return view! { <span/> }.into_view();
                                        };
                                        let (badge, badge_class) = match db_type {
                                            shared::DbType::MySql => ("MY", "conn-db-badge conn-db-badge-my"),
                                            shared::DbType::Postgres => ("PG", "conn-db-badge conn-db-badge-pg"),
                                        };
                                        let where_str = format!("{}:{}", conn.host, conn.port);
                                        let who_str = conn.user.clone();
                                        view! {
                                            <div class="conn-header">
                                                <span class=badge_class>{badge}</span>
                                                <span class="conn-header-name">{conn.name.clone()}</span>
                                                <span class="conn-header-where">{where_str}</span>
                                                <span class="conn-header-who">{who_str}</span>
                                            </div>
                                        }.into_view()
                                    };
                                    view! {
                                        <div style=move || {
                                            if active_wt_id.get().as_deref() == Some(&wt_id) {
                                                "display:flex;flex-direction:column;flex:1;overflow:hidden;min-width:0;min-height:0"
                                            } else {
                                                "display:none"
                                            }
                                        }>
                                            {header_view}
                                            <div class="pane-area">
                                                {render_workspace_panes(placement, shell_panes, active_pane_id, zoomed, notifications, connections, tab_context_menu, shell_input_menu, completion_popup, renaming_pane_id, rename_input, ws, pane_ops, close_pane, do_split, add_tab, add_pane_to_leaf)}
                                            </div>
                                        </div>
                                    }
                                }
                            />
                        </div>
                    </div>
                </div>
            </div>

            // ── Settings dialog ───────────────────────────────────────────────
            {move || settings_ui.open.get().then(|| view! {
                <settings::SettingsDialog state=settings_ui ws=ws/>
            })}

            // ── In-app dialog (prompt / confirm) ──────────────────────────────
            // Replaces window.prompt / window.confirm so we don't surface the
            // browser-chrome's "localhost:8080 says" framing. Two shapes:
            // Prompt has a single text input; Confirm is yes/no. Backdrop
            // click and Esc close without firing the callback; Enter submits
            // for Prompt; Confirm has its own keyed button.
            {move || dialog.get().map(|kind| {
                let kind_for_render = kind.clone();
                let close = move || { dialog.set(None); dialog_input.set(String::new()); };
                let input_id = "app-dialog-input";
                view! {
                    <div class="app-dialog-backdrop"
                         on:mousedown=move |_| close()>
                    </div>
                    <div class="app-dialog"
                         on:mousedown=move |e: web_sys::MouseEvent| {
                             use wasm_bindgen::JsCast;
                             let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                         }
                         on:keydown=move |e: web_sys::KeyboardEvent| {
                             if e.key() == "Escape" {
                                 e.prevent_default();
                                 close();
                             }
                         }>
                        {match kind_for_render {
                            DialogKind::Prompt { title, message, default_value: _, confirm_label, on_submit } => {
                                // Input is seeded by the open-transition effect
                                // (see create_effect on `dialog` below). The
                                // render block here treats `dialog_input` as
                                // read-only state — clearing the input must
                                // stay cleared.
                                let submit = move || {
                                    let v = dialog_input.get_untracked().trim().to_string();
                                    dialog.set(None);
                                    dialog_input.set(String::new());
                                    on_submit.call(v);
                                };
                                let submit_for_btn = submit;
                                view! {
                                    <div class="app-dialog-title">{title}</div>
                                    <div class="app-dialog-message">{message}</div>
                                    <input id=input_id class="app-dialog-input"
                                           prop:value=move || dialog_input.get()
                                           on:input=move |e| dialog_input.set(event_target_value(&e))
                                           on:keydown=move |e: web_sys::KeyboardEvent| {
                                               if e.key() == "Enter" {
                                                   e.prevent_default();
                                                   submit();
                                               }
                                           } />
                                    <div class="app-dialog-actions">
                                        <button class="btn btn-ghost"
                                                on:click=move |_| close()>"Cancel"</button>
                                        <button class="btn btn-primary"
                                                on:click=move |_| submit_for_btn()>{confirm_label}</button>
                                    </div>
                                }.into_view()
                            }
                            DialogKind::Confirm { title, message, confirm_label, danger, on_confirm } => {
                                let confirm = move || {
                                    dialog.set(None);
                                    dialog_input.set(String::new());
                                    on_confirm.call(());
                                };
                                let btn_class = if danger { "btn btn-danger" } else { "btn btn-primary" };
                                view! {
                                    <div class="app-dialog-title">{title}</div>
                                    <div class="app-dialog-message">{message}</div>
                                    <div class="app-dialog-actions">
                                        <button class="btn btn-ghost"
                                                on:click=move |_| close()>"Cancel"</button>
                                        <button class=btn_class
                                                on:click=move |_| confirm()>{confirm_label}</button>
                                    </div>
                                }.into_view()
                            }
                        }}
                    </div>
                }
            })}

            // Autofocus the dialog's input when a Prompt-shaped dialog opens.
            // Driven by the dialog signal so it fires on every fresh open.
            {move || {
                let _ = dialog.get();
                // Schedule on the next animation frame so the input element
                // exists in the DOM before we try to focus it.
                if matches!(dialog.get_untracked(), Some(DialogKind::Prompt { .. })) {
                    if let Some(win) = web_sys::window() {
                        let cb = Closure::once(move || {
                            use wasm_bindgen::JsCast;
                            if let Some(el) = web_sys::window()
                                .and_then(|w| w.document())
                                .and_then(|d| d.get_element_by_id("app-dialog-input"))
                            {
                                if let Ok(inp) = el.dyn_into::<web_sys::HtmlInputElement>() {
                                    let _ = inp.focus();
                                    inp.select();
                                }
                            }
                        });
                        let _ = win.request_animation_frame(cb.as_ref().unchecked_ref());
                        cb.forget();
                    }
                }
                view! { <span/> }
            }}

            // ── Sidebar context menu (fixed-position overlay) ────────────────
            // Two shapes — Connection and Folder. Each item closes the menu
            // when clicked; the backdrop closes on any outside-click.
            {move || sidebar_context_menu.get().map(|ctx| {
                let pos_style = format!("left:{}px; top:{}px", ctx.x, ctx.y);
                let body = match ctx.target {
                    // ── Connection-row menu: open / edit / cut / copy /
                    // copy-as-link / remove from folder / delete connection ─
                    SidebarMenuTarget::Connection { conn_id, source_path } => {
                        let conn_for_lookup = conn_id.clone();
                        let id_open  = conn_id.clone();
                        let id_edit  = conn_id.clone();
                        let id_cut   = conn_id.clone();
                        let id_copy  = conn_id.clone();
                        let id_link  = conn_id.clone();
                        let id_rm    = conn_id.clone();
                        let id_del   = conn_id.clone();
                        let sp_cut   = source_path.clone();
                        let sp_copy  = source_path.clone();
                        let sp_link  = source_path.clone();
                        let sp_rm    = source_path.clone();
                        // "Remove from folder" only makes sense when the row was
                        // rendered under a real folder (not the synthetic
                        // "All Connections" view, which has empty source_path).
                        let can_remove = !source_path.is_empty();
                        let rm_class = if can_remove { "tab-context-menu-item" } else { "tab-context-menu-item tab-context-menu-item-disabled" };
                        view! {
                            <>
                                <div class="tab-context-menu-item"
                                     on:click=move |_| {
                                         open_conn_tab(id_open.clone(), true);
                                         sidebar_context_menu.set(None);
                                     }>"Open in new tab"</div>
                                <div class="tab-context-menu-item"
                                     on:click=move |_| {
                                         if let Some(c) = connections.get().into_iter().find(|c| c.id == id_edit) {
                                             nc_db_type.set(c.db_type);
                                             nc_name.set(c.name);
                                             nc_host.set(c.host);
                                             nc_port.set(c.port.to_string());
                                             nc_user.set(c.user);
                                             // The browser does not hold the saved password
                                             // — by design (see ConnectionDefPublic + issue #6).
                                             // Start the input blank; the placeholder hints that
                                             // leaving it blank preserves the existing password.
                                             nc_pass.set(String::new());
                                             nc_db.set(c.database);
                                             nc_default_pane.set(c.default_pane);
                                             nc_env.set(c.env.clone().unwrap_or_default());
                                             nc_folder_path.set(c.paths.first().cloned().unwrap_or_default());
                                             nc_format_style.set(c.format_style);
                                             nc_buffer_threshold_mb.set(
                                                 c.buffer_threshold_mb.map(|n| n.to_string()).unwrap_or_default()
                                             );
                                             nc_editor_command.set(c.editor_command.clone().unwrap_or_default());
                                             nc_schema.set(c.default_schema.clone().unwrap_or_default());
                                             nc_credential_source.set(c.credential_source);
                                             editing_conn_id.set(Some(c.id));
                                             nc_test_status.set(None);
                                             nc_db_list.set(vec![]);
                                             // Schema list starts empty so the
                                             // Edit modal shows the text input
                                             // pre-filled with the saved schema;
                                             // user can hit ↻ to swap to the
                                             // dropdown.
                                             nc_schema_list.set(vec![]);
                                             pane.set(Pane::NewConnection);
                                         }
                                         sidebar_context_menu.set(None);
                                     }>"Edit…"</div>
                                <div class="tab-context-menu-sep"/>
                                <div class="tab-context-menu-item"
                                     on:click=move |_| {
                                         conn_clipboard.set(Some(ClipboardEntry {
                                             conn_id: id_cut.clone(),
                                             verb: ClipboardVerb::Cut,
                                             source_path: sp_cut.clone(),
                                         }));
                                         sidebar_context_menu.set(None);
                                     }>"Cut"</div>
                                <div class="tab-context-menu-item"
                                     on:click=move |_| {
                                         conn_clipboard.set(Some(ClipboardEntry {
                                             conn_id: id_copy.clone(),
                                             verb: ClipboardVerb::Copy,
                                             source_path: sp_copy.clone(),
                                         }));
                                         sidebar_context_menu.set(None);
                                     }>"Copy"</div>
                                <div class="tab-context-menu-item"
                                     on:click=move |_| {
                                         conn_clipboard.set(Some(ClipboardEntry {
                                             conn_id: id_link.clone(),
                                             verb: ClipboardVerb::Link,
                                             source_path: sp_link.clone(),
                                         }));
                                         sidebar_context_menu.set(None);
                                     }>"Copy as Link"</div>
                                <div class="tab-context-menu-sep"/>
                                <div class=rm_class
                                     on:click=move |_| {
                                         if !can_remove { return; }
                                         if let Some(mut def) = connections.get()
                                             .into_iter()
                                             .find(|c| c.id == id_rm)
                                         {
                                             def.paths.retain(|p| p != &sp_rm);
                                             ws_send(ws, &ClientMessage::SaveConnection { def, password_change: None, copy_from: None });
                                         }
                                         sidebar_context_menu.set(None);
                                     }>"Remove from folder"</div>
                                <div class="tab-context-menu-item tab-context-menu-item-danger"
                                     on:click=move |_| {
                                         let name = connections.get().iter()
                                             .find(|c| c.id == conn_for_lookup)
                                             .map(|c| c.name.clone())
                                             .unwrap_or_else(|| conn_for_lookup.clone());
                                         let id_for_dialog = id_del.clone();
                                         dialog.set(Some(DialogKind::Confirm {
                                             title:         "Delete connection".to_string(),
                                             message:       format!(
                                                 "Delete \u{201c}{}\u{201d}? This removes every linked placement.",
                                                 name
                                             ),
                                             confirm_label: "Delete".to_string(),
                                             danger:        true,
                                             on_confirm: Callback::new(move |_| {
                                                 ws_send(ws, &ClientMessage::DeleteConnection { id: id_for_dialog.clone() });
                                             }),
                                         }));
                                         sidebar_context_menu.set(None);
                                     }>"Delete connection"</div>
                            </>
                        }.into_view()
                    }
                    // ── Folder header menu: new folder / new connection /
                    // paste / paste as link / rename / delete ──────────────
                    SidebarMenuTarget::Folder { path, is_root } => {
                        let path_for_newfolder = path.clone();
                        let path_for_newconn   = path.clone();
                        let path_for_paste     = path.clone();
                        let path_for_pastelink = path.clone();
                        let path_for_rename    = path.clone();
                        let path_for_delete    = path.clone();
                        let has_clipboard = conn_clipboard.get_untracked().is_some();
                        let paste_class      = if has_clipboard { "tab-context-menu-item" } else { "tab-context-menu-item tab-context-menu-item-disabled" };
                        let paste_link_class = if has_clipboard { "tab-context-menu-item" } else { "tab-context-menu-item tab-context-menu-item-disabled" };
                        // Rename / delete don't apply to the synthetic "All
                        // Connections" root header; grey them out instead of
                        // hiding so the menu layout stays consistent.
                        let rename_class = if is_root { "tab-context-menu-item tab-context-menu-item-disabled" } else { "tab-context-menu-item" };
                        let delete_class = if is_root { "tab-context-menu-item tab-context-menu-item-disabled" } else { "tab-context-menu-item tab-context-menu-item-danger" };
                        view! {
                            <>
                                <div class="tab-context-menu-item"
                                     on:click=move |_| {
                                         new_folder_at(
                                             path_for_newfolder.clone(),
                                             dialog,
                                             connection_folders,
                                             ws,
                                         );
                                         sidebar_context_menu.set(None);
                                     }>"New folder here"</div>
                                <div class="tab-context-menu-item"
                                     on:click=move |_| {
                                         // Pre-populate the new-connection form so the
                                         // result lands in this folder. Engine defaults
                                         // to MySQL — user can flip it before saving.
                                         let plane = plane_for(DbType::MySql);
                                         nc_db_type.set(DbType::MySql);
                                         nc_name.set(String::new());
                                         nc_host.set("127.0.0.1".into());
                                         nc_port.set(plane.default_port().to_string());
                                         nc_user.set("root".into());
                                         nc_pass.set(String::new());
                                         nc_db.set(String::new());
                                         nc_default_pane.set("shell".into());
                                         nc_env.set(String::new());
                                         nc_folder_path.set(path_for_newconn.clone());
                                         // Block is the new-connection default (see open_new_conn).
                                         nc_format_style.set(FormatStyle::Block);
                                         nc_buffer_threshold_mb.set(String::new());
        nc_editor_command.set(String::new());
                                         editing_conn_id.set(None);
                                         nc_test_status.set(None);
                                         nc_db_list.set(vec![]);
                                         // Reset the schema picker too — otherwise a
                                         // stale dropdown from a prior PG connection
                                         // form leaks into this fresh form.
                                         nc_schema.set(String::new());
                                         nc_schema_list.set(vec![]);
                                         // Same for the credential-source picker —
                                         // a fresh connection defaults to Plaintext.
                                         nc_credential_source.set(CredentialSource::Plaintext);
                                         pane.set(Pane::NewConnection);
                                         sidebar_context_menu.set(None);
                                     }>"New connection here"</div>
                                <div class="tab-context-menu-sep"/>
                                <div class=paste_class
                                     on:click=move |_| {
                                         if !has_clipboard { return; }
                                         let Some(entry) = conn_clipboard.get_untracked() else { return };
                                         apply_clipboard_paste(
                                             &entry,
                                             &path_for_paste,
                                             /* force_link = */ false,
                                             connections,
                                             ws,
                                         );
                                         // Cut is a one-shot move; clear so the user can't
                                         // paste the same Cut twice. Copy / Link survive so
                                         // the user can drop the payload into many folders.
                                         if entry.verb == ClipboardVerb::Cut {
                                             conn_clipboard.set(None);
                                         }
                                         sidebar_context_menu.set(None);
                                     }>"Paste"</div>
                                <div class=paste_link_class
                                     on:click=move |_| {
                                         if !has_clipboard { return; }
                                         let Some(entry) = conn_clipboard.get_untracked() else { return };
                                         apply_clipboard_paste(
                                             &entry,
                                             &path_for_pastelink,
                                             /* force_link = */ true,
                                             connections,
                                             ws,
                                         );
                                         // Paste-as-Link always adds a placement; clipboard
                                         // survives regardless of original verb so the user
                                         // can keep linking elsewhere.
                                         sidebar_context_menu.set(None);
                                     }>"Paste as Link"</div>
                                <div class="tab-context-menu-sep"/>
                                <div class=rename_class
                                     on:click=move |_| {
                                         if is_root { return; }
                                         let leaf = path_for_rename
                                             .rsplit('/')
                                             .next()
                                             .unwrap_or("")
                                             .to_string();
                                         let path_for_submit = path_for_rename.clone();
                                         let leaf_for_filter = leaf.clone();
                                         dialog.set(Some(DialogKind::Prompt {
                                             title:         "Rename folder".to_string(),
                                             message:       format!("New name for \u{201c}{}\u{201d}:", leaf),
                                             default_value: leaf,
                                             confirm_label: "Rename".to_string(),
                                             on_submit: Callback::new(move |new_leaf: String| {
                                                 if new_leaf.is_empty() || new_leaf.contains('/') || new_leaf == leaf_for_filter {
                                                     return;
                                                 }
                                                 let new_path = match path_for_submit.rfind('/') {
                                                     Some(idx) => format!("{}/{}", &path_for_submit[..idx], new_leaf),
                                                     None      => new_leaf,
                                                 };
                                                 rename_folder_path(
                                                     &path_for_submit,
                                                     &new_path,
                                                     connections,
                                                     connection_folders,
                                                     ws,
                                                 );
                                                 // Keep the +-button's default in sync
                                                 // when the active folder itself (or one
                                                 // of its ancestors) just got renamed.
                                                 let active = last_active_folder.get_untracked();
                                                 let prefix = format!("{}/", path_for_submit);
                                                 if active == path_for_submit {
                                                     last_active_folder.set(new_path.clone());
                                                 } else if active.starts_with(&prefix) {
                                                     last_active_folder.set(format!(
                                                         "{}/{}", new_path, &active[prefix.len()..]
                                                     ));
                                                 }
                                             }),
                                         }));
                                         sidebar_context_menu.set(None);
                                     }>"Rename folder"</div>
                                <div class=delete_class
                                     on:click=move |_| {
                                         if is_root { return; }
                                         // Confirm only when the folder (or its
                                         // sub-folders) actually contain placements;
                                         // a truly empty folder can vanish silently.
                                         let prefix       = format!("{}/", path_for_delete);
                                         let placements_n = connections.get().iter()
                                             .flat_map(|c| c.paths.iter().cloned())
                                             .filter(|p| p == &path_for_delete || p.starts_with(&prefix))
                                             .count();
                                         let do_delete = {
                                             let path_for_run = path_for_delete.clone();
                                             move || {
                                                 delete_folder_path(
                                                     &path_for_run,
                                                     connections,
                                                     connection_folders,
                                                     ws,
                                                 );
                                                 // If the active folder vanished
                                                 // (or one of its ancestors did),
                                                 // fall the +-button default back
                                                 // to root.
                                                 let active = last_active_folder.get_untracked();
                                                 let prefix = format!("{}/", path_for_run);
                                                 if active == path_for_run || active.starts_with(&prefix) {
                                                     last_active_folder.set(String::new());
                                                 }
                                             }
                                         };
                                         if placements_n == 0 {
                                             // Truly empty folder — no confirmation
                                             // needed; nothing the user could lose.
                                             do_delete();
                                         } else {
                                             let path_in_msg = path_for_delete.clone();
                                             dialog.set(Some(DialogKind::Confirm {
                                                 title:         "Delete folder".to_string(),
                                                 message:       format!(
                                                     "Delete \u{201c}{}\u{201d}? {} connection placement(s) will move to root. Underlying connections are not deleted.",
                                                     path_in_msg, placements_n
                                                 ),
                                                 confirm_label: "Delete".to_string(),
                                                 danger:        true,
                                                 on_confirm: Callback::new(move |_| do_delete()),
                                             }));
                                         }
                                         sidebar_context_menu.set(None);
                                     }>"Delete folder"</div>
                            </>
                        }.into_view()
                    }
                };
                view! {
                    <div class="tab-context-menu-backdrop"
                         on:mousedown=move |_| sidebar_context_menu.set(None)
                         on:contextmenu=move |e: web_sys::MouseEvent| {
                             e.prevent_default();
                             sidebar_context_menu.set(None);
                         } />
                    <div class="tab-context-menu"
                         style=pos_style
                         on:mousedown=move |e: web_sys::MouseEvent| {
                             use wasm_bindgen::JsCast;
                             let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                         }>
                        {body}
                    </div>
                }
            })}

            // ── Shell input context menu (Format submenu) ────────────────────
            // Right-click on a shell input opens this. Three Format items —
            // pane's configured default plus the explicit style list. Backdrop
            // closes on outside click; Esc handled via keydown on the menu.
            // ── Completion popup ─────────────────────────────────────
            // Anchored below the shell input textarea (Phase 2; pixel-
            // exact cursor positioning is deferred). Items render from
            // `popup.items` with the selected row highlighted. Click an
            // item to accept; backdrop click closes without accepting.
            {move || completion_popup.get().map(|popup| {
                let pos_style = format!("left:{}px; top:{}px", popup.x, popup.y);
                let pane_id   = popup.pane_id.clone();
                let popup_for_each = popup.clone();
                view! {
                    <div class="completion-popup-backdrop"
                         on:mousedown=move |_| completion_popup.set(None) />
                    <div class="completion-popup"
                         style=pos_style
                         on:mousedown=move |e: web_sys::MouseEvent| {
                             use wasm_bindgen::JsCast;
                             let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                         }>
                        {popup_for_each.items.iter().enumerate().map(|(i, item)| {
                            let item_text   = item.clone();
                            let item_for_click = item.clone();
                            let pane_for_click = pane_id.clone();
                            let selected = i == popup_for_each.selected;
                            let replace_start = popup_for_each.replace_start;
                            view! {
                                <div class="completion-popup-item"
                                     class:completion-popup-item-selected=selected
                                     on:click=move |_| {
                                         // Look up the pane's input + apply the
                                         // replacement, same as Enter accept above.
                                         let Some(tab) = shell_panes.get_untracked().get(&pane_for_click).cloned() else {
                                             completion_popup.set(None);
                                             return;
                                         };
                                         let t_input = tab.current_input;
                                         let input   = t_input.get_untracked();
                                         let cursor: usize = web_sys::window()
                                             .and_then(|w| w.document())
                                             .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pane_for_click)))
                                             .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                             .and_then(|ta| ta.selection_start().ok().flatten())
                                             .map(|n| n as usize)
                                             .unwrap_or_else(|| input.len());
                                         let before = &input[..replace_start];
                                         let after  = &input[cursor.min(input.len())..];
                                         let new_sql = format!("{}{}{}", before, item_for_click, after);
                                         let new_cursor = replace_start + item_for_click.len();
                                         t_input.set(new_sql);
                                         completion_popup.set(None);
                                         let pid_paint = pane_for_click.clone();
                                         let paint_cb = wasm_bindgen::closure::Closure::once(move || {
                                             if let Some(ta) = web_sys::window()
                                                 .and_then(|w| w.document())
                                                 .and_then(|d| d.get_element_by_id(&format!("shell-input-{}", pid_paint)))
                                                 .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                             {
                                                 let pos = new_cursor as u32;
                                                 let _ = ta.set_selection_range(pos, pos);
                                                 let _ = ta.focus();
                                             }
                                         });
                                         if let Some(win) = web_sys::window() {
                                             let _ = win.request_animation_frame(paint_cb.as_ref().unchecked_ref());
                                         }
                                         paint_cb.forget();
                                     }>
                                    {item_text}
                                </div>
                            }
                        }).collect_view()}
                    </div>
                }
            })}

            {move || shell_input_menu.get().map(|ctx| {
                let pid = ctx.pane_id.clone();
                let pos_style = format!("left:{}px; top:{}px", ctx.x, ctx.y);
                // Resolve pane state for the menu's actions.
                let tab = shell_panes.get_untracked().get(&pid).cloned();
                let Some(tab) = tab else {
                    shell_input_menu.set(None);
                    return view! { <div/> }.into_view();
                };
                let plane = crate::plane::plane_for(tab.db_type);
                let default_style = tab.format_style.get_untracked();
                let t_input = tab.current_input;
                let t_history = tab.shell_history;
                let t_collapsed = tab.collapsed_blocks;
                let buffer_label = {
                    let bytes = t_history.with(|h| crate::shell::shell_buffer_bytes(h));
                    if bytes == 0 {
                        "Clear shell buffer".to_string()
                    } else {
                        format!("Clear shell buffer ({})", crate::format::human_bytes(&bytes.to_string()))
                    }
                };
                let buffer_empty = t_history.with(|h| h.is_empty());
                let block_num = ctx.block_num;
                let default_label = match default_style {
                    FormatStyle::River       => "Format with default (River)",
                    FormatStyle::Block       => "Format with default (Block)",
                    FormatStyle::Inline      => "Format with default (Inline)",
                    FormatStyle::Unformatted => "Format with default (Unformatted)",
                };
                let pid_default = pid.clone();
                let pid_river   = pid.clone();
                let pid_block   = pid.clone();
                let pid_inline  = pid.clone();
                let pid_unform  = pid.clone();
                view! {
                    <div class="tab-context-menu-backdrop"
                         on:mousedown=move |_| shell_input_menu.set(None)
                         on:contextmenu=move |e: web_sys::MouseEvent| {
                             e.prevent_default();
                             shell_input_menu.set(None);
                         } />
                    <div class="tab-context-menu"
                         style=pos_style
                         on:mousedown=move |e: web_sys::MouseEvent| {
                             use wasm_bindgen::JsCast;
                             let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                         }>
                        // ── Block copy (when right-click landed on a block) ──
                        // Inspired by Warp Terminal's block model — each
                        // SQL statement and its output form a unit a DBA
                        // commonly wants to copy whole or in part. When
                        // block_num is None (right-click outside any
                        // block — empty scroll area / textarea / gutter),
                        // these three items are skipped and the menu
                        // shows only the shell-level items below.
                        {block_num.map(|num| {
                            // Use untracked reads inside the on:click
                            // closures so we always grab the LATEST
                            // history at copy time, not the snapshot
                            // taken when the menu opened — a query that
                            // landed between menu open and click should
                            // still flow into the clipboard payload of
                            // a "WholeBlock" copy.
                            //
                            // First, the relations the block's printed
                            // output links to (psql's `\d`).
                            let opens = t_history.with_untracked(|h| crate::shell::block_explorer_links(h, num))
                                .into_iter()
                                .map(|init| {
                                    let label = format!("Open {}.{} in Schema Explorer", init.schema, init.name);
                                    let pid_open = pid.clone();
                                    view! {
                                        <div class="tab-context-menu-item"
                                             on:click=move |_| {
                                                 pane_ops.open_explorer.call((pid_open.clone(), init.clone()));
                                                 shell_input_menu.set(None);
                                             }>{label}</div>
                                    }
                                })
                                .collect_view();
                            view! {
                                {opens}
                                <div class="tab-context-menu-item"
                                     on:click=move |_| {
                                         let text = t_history.with_untracked(|h|
                                             crate::shell::format_block_for_clipboard(
                                                 h, num, crate::shell::BlockCopyMode::Command, plane));
                                         if !text.is_empty() {
                                             if let Some(nav) = web_sys::window().map(|w| w.navigator()) {
                                                 let _ = nav.clipboard().write_text(&text);
                                             }
                                         }
                                         shell_input_menu.set(None);
                                     }>"Copy command"</div>
                                <div class="tab-context-menu-item"
                                     on:click=move |_| {
                                         let text = t_history.with_untracked(|h|
                                             crate::shell::format_block_for_clipboard(
                                                 h, num, crate::shell::BlockCopyMode::Output, plane));
                                         if !text.is_empty() {
                                             if let Some(nav) = web_sys::window().map(|w| w.navigator()) {
                                                 let _ = nav.clipboard().write_text(&text);
                                             }
                                         }
                                         shell_input_menu.set(None);
                                     }>"Copy output"</div>
                                <div class="tab-context-menu-item"
                                     on:click=move |_| {
                                         let text = t_history.with_untracked(|h|
                                             crate::shell::format_block_for_clipboard(
                                                 h, num, crate::shell::BlockCopyMode::WholeBlock, plane));
                                         if !text.is_empty() {
                                             if let Some(nav) = web_sys::window().map(|w| w.navigator()) {
                                                 let _ = nav.clipboard().write_text(&text);
                                             }
                                         }
                                         shell_input_menu.set(None);
                                     }>"Copy whole block"</div>
                                // ── Collapse / Expand output ────────────
                                // Only meaningful when the block has a
                                // Table or Vertical entry — those are
                                // the only output shapes that can be
                                // collapsed into a fixed-height scroll
                                // region. Other variants (Info, Error,
                                // MetaOutput, KeyValue) are inherently
                                // short. If none qualify, the menu
                                // omits this item entirely so the user
                                // doesn't see an action that does
                                // nothing.
                                {
                                    let has_tabular = t_history.with_untracked(|h| {
                                        let mut hit_query = false;
                                        for entry in h {
                                            match entry {
                                                crate::types::ShellEntry::Query { num: n, .. } => {
                                                    if hit_query { break; }
                                                    if *n == num { hit_query = true; }
                                                }
                                                crate::types::ShellEntry::Table { .. }
                                                | crate::types::ShellEntry::Vertical { .. }
                                                | crate::types::ShellEntry::Printed { .. }
                                                    if hit_query => return true,
                                                _ => {}
                                            }
                                        }
                                        false
                                    });
                                    has_tabular.then(|| {
                                        let currently_collapsed = t_collapsed.with_untracked(|s| s.contains(&num));
                                        let label = if currently_collapsed { "Expand output" } else { "Collapse output" };
                                        view! {
                                            <div class="tab-context-menu-item"
                                                 on:click=move |_| {
                                                     t_collapsed.update(|s| {
                                                         if !s.insert(num) { s.remove(&num); }
                                                     });
                                                     shell_input_menu.set(None);
                                                 }>{label}</div>
                                        }
                                    })
                                }
                                <div class="tab-context-menu-sep"/>
                            }
                        })}
                        // ── Buffer management ─────────────────────────
                        // "Clear shell buffer" wipes the visible
                        // history. Sibling state (query_count, persisted
                        // localStorage, watch state, pending statements)
                        // is intentionally preserved — clearing the
                        // scrollback shouldn't reset the session
                        // identity. Disabled when the buffer is already
                        // empty so the menu doesn't suggest an action
                        // that does nothing.
                        <div class="tab-context-menu-item"
                             class:tab-context-menu-item-disabled=buffer_empty
                             on:click=move |_| {
                                 if buffer_empty { return; }
                                 t_history.set(Vec::new());
                                 // Also drop the collapsed-blocks set
                                 // — query nums in there no longer
                                 // exist and would otherwise leak
                                 // across clear cycles.
                                 t_collapsed.set(std::collections::HashSet::new());
                                 shell_input_menu.set(None);
                             }>{buffer_label}</div>
                        <div class="tab-context-menu-sep"/>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 crate::shell::apply_format_to_input(&pid_default, t_input, plane, default_style);
                                 shell_input_menu.set(None);
                             }>{default_label}</div>
                        <div class="tab-context-menu-sep"/>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 crate::shell::apply_format_to_input(&pid_unform, t_input, plane, FormatStyle::Unformatted);
                                 shell_input_menu.set(None);
                             }>"Format with Unformatted"</div>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 crate::shell::apply_format_to_input(&pid_inline, t_input, plane, FormatStyle::Inline);
                                 shell_input_menu.set(None);
                             }>"Format with Inline"</div>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 crate::shell::apply_format_to_input(&pid_block, t_input, plane, FormatStyle::Block);
                                 shell_input_menu.set(None);
                             }>"Format with Block"</div>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 crate::shell::apply_format_to_input(&pid_river, t_input, plane, FormatStyle::River);
                                 shell_input_menu.set(None);
                             }>"Format with River"</div>
                    </div>
                }.into_view()
            })}

            // ── Tab context menu (fixed-position overlay, workspace-scoped) ──
            {move || tab_context_menu.get().map(|ctx| {
                let pid_rn = ctx.pane_id.clone();
                let pid_dup = ctx.pane_id.clone();
                let pid_cl  = ctx.pane_id.clone();
                let pid_co  = ctx.pane_id.clone();
                let pid_cr  = ctx.pane_id.clone();
                view! {
                    <div class="tab-context-menu-backdrop"
                         on:mousedown=move |_| tab_context_menu.set(None)
                         on:contextmenu=move |e: web_sys::MouseEvent| {
                             e.prevent_default();
                             tab_context_menu.set(None);
                         } />
                    <div class="tab-context-menu"
                         style=format!("left:{}px; top:{}px", ctx.x, ctx.y)
                         on:mousedown=move |e: web_sys::MouseEvent| {
                             use wasm_bindgen::JsCast;
                             let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                         }>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 start_rename(pid_rn.clone());
                                 tab_context_menu.set(None);
                             }>"Rename"</div>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 duplicate_pane(pid_dup.clone());
                                 tab_context_menu.set(None);
                             }>"Duplicate"</div>
                        <div class="tab-context-menu-sep"/>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 close_pane(pid_cl.clone());
                                 tab_context_menu.set(None);
                             }>"Close"</div>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 close_others(pid_co.clone());
                                 tab_context_menu.set(None);
                             }>"Close others"</div>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 close_to_right(pid_cr.clone());
                                 tab_context_menu.set(None);
                             }>"Close to right"</div>
                    </div>
                }
            })}
        </div>
    }.into_view()
}

// ── Workspace panes ───────────────────────────────────────────────────────────

/// A workspace's panes and its chrome, inside its pane area (which is
/// positioned). Each pane is built once, in one flat keyed list, and
/// placed over its leaf by `pane_layout`: splitting, closing, zooming or
/// dragging a divider moves it and nothing else. (Built inside the
/// layout tree's boxes, panes were all rebuilt on every split or close —
/// a shell's scrollback went back to the top.) The tab bars and dividers
/// follow the layout.
#[allow(clippy::too_many_arguments)]
fn render_workspace_panes<F, S, T, E>(
    placement: Memo<pane_layout::Placement>,
    shell_panes: RwSignal<HashMap<String, ShellTab>>,
    active_pane_id: RwSignal<Option<String>>,
    zoomed_pane_id: RwSignal<Option<String>>,
    notifications: RwSignal<Vec<Notification>>,
    connections: RwSignal<Vec<ConnectionDefPublic>>,
    tab_context_menu: RwSignal<Option<TabContextMenu>>,
    shell_input_menu: RwSignal<Option<ShellInputContextMenu>>,
    completion_popup: RwSignal<Option<CompletionPopup>>,
    renaming_pane_id: RwSignal<Option<String>>,
    rename_input: RwSignal<String>,
    ws: RwSignal<Option<WebSocket>>,
    pane_ops: PaneOps,
    close_pane: F,
    do_split: S,
    add_tab: T,
    add_pane: E,
) -> leptos::View
where
    F: Fn(String) + Copy + 'static,
    S: Fn(String, SplitDir) + Copy + 'static,
    T: Fn(String) + Copy + 'static,
    E: Fn(String, PaneKind, Option<ExplorerInit>) + Copy + 'static,
{
    // Panes in the order first seen: a keyed list in tree order would
    // move a pane in the DOM when a split puts one before it.
    let order = store_value(Vec::<String>::new());
    let pane_ids = move || {
        let now: Vec<String> = placement.with(|p| p.panes.iter().map(|pane| pane.id.clone()).collect());
        let ids = pane_layout::stable_order(&order.get_value(), &now);
        order.set_value(ids.clone());
        ids
    };
    let leaves = move || placement.with(|p| p.leaves.iter().map(|(leaf, _)| *leaf).collect::<Vec<_>>());
    let dividers = move || placement.with(|p| p.dividers.iter().map(|d| (d.dir, d.ratio)).collect::<Vec<_>>());
    view! {
        <For each=pane_ids key=|pid| pid.clone() children=move |pid: String| {
            let tab = shell_panes.get_untracked().get(&pid).cloned();
            // The banner reads these; `tab` moves into render_shell_leaf.
            // `session_stale && session_id.is_none()` is the SessionClosed
            // shape — distinct from a WS drop (which keeps session_id and
            // recovers by itself), so the banner shows only when the user
            // has to act.
            let banner_stale = tab.as_ref().map(|t| t.session_stale);
            let banner_sid   = tab.as_ref().map(|t| t.session_id);
            // A shell whose CLI reconnects by itself (mysql) does so on
            // its next statement instead (`shell::deliver`).
            let banner_reopens = tab.as_ref().is_some_and(|t| {
                t.kind == PaneKind::Shell && plane_for(t.db_type).lost_connection() != LostConnection::Banner
            });
            let banner_conn  = tab.as_ref().map(|t| t.conn_id.clone());
            let banner_role: Option<String> = tab.as_ref()
                .map(|t| t.kind.application_role().to_string());
            let pid_for_btn  = pid.clone();
            let pid_place    = pid.clone();
            let shell_view = render_shell_leaf(pid, tab, active_pane_id, ws, pane_ops, connections, shell_panes, shell_input_menu, completion_popup, close_pane, do_split, add_tab, add_pane);
            // A pane off screen stays laid out at its place but isn't
            // painted (`content-visibility: hidden`): it keeps its scroll
            // position and state for when it's shown again.
            let style = move || placement.with(|p| {
                let place = p.panes.iter().find(|pane| pane.id == pid_place);
                let mut style = place.map_or(pane_layout::Rect::FULL, |pane| pane.rect).style();
                style.push_str("display:flex;flex-direction:column;overflow:hidden;");
                if !place.is_some_and(|pane| pane.shown) {
                    style.push_str("content-visibility:hidden;visibility:hidden;pointer-events:none;");
                }
                style
            });
            view! {
                <div class="pane-body" style=style>
                    {move || {
                        let show = !banner_reopens
                            && banner_stale.map(|s| s.get()).unwrap_or(false)
                            && banner_sid.map(|s| s.get().is_none()).unwrap_or(false);
                        if !show { return ().into_view(); }
                        let conn = banner_conn.clone().unwrap_or_default();
                        let pidc = pid_for_btn.clone();
                        let role_c = banner_role.clone();
                        view! {
                            <div class="reconnect-banner">
                                <span class="reconnect-banner-icon">"⚠"</span>
                                <span class="reconnect-banner-text">"Session ended."</span>
                                <button class="reconnect-banner-btn"
                                    on:click=move |_| {
                                        // Back to the database it was on.
                                        let database = shell_panes.with_untracked(|m|
                                            m.get(&pidc).and_then(ShellTab::reconnect_database));
                                        crate::dom::ws_send(ws, &shared::ClientMessage::Connect {
                                            connection_id: conn.clone(),
                                            client_tab_id: pidc.clone(),
                                            database,
                                            client_role: role_c.clone(),
                                            timezone: crate::settings::session_time_zone(),
                                        });
                                    }>"Reconnect"</button>
                            </div>
                        }.into_view()
                    }}
                    {shell_view}
                </div>
            }
        }/>
        // The chrome comes after the panes, so a tab bar's menus open over them.
        <For each=leaves key=|leaf| *leaf children=move |leaf: PaneLeaf| {
            let style = move || placement.with(|p| {
                p.leaves.iter().find(|(l, _)| *l == leaf)
                    .map_or_else(|| "display:none".to_string(), |(_, rect)| rect.tab_bar().style())
            });
            view! {
                <div class="pane-leaf-bar" style=style>
                    {render_leaf_tab_bar(leaf, shell_panes, active_pane_id, zoomed_pane_id, notifications, connections, tab_context_menu, renaming_pane_id, rename_input, close_pane, do_split, add_tab, add_pane)}
                </div>
            }
        }/>
        <For each=dividers key=|(_, ratio)| *ratio children=move |(dir, ratio)| {
            render_divider(dir, ratio, placement)
        }/>
    }.into_view()
}

/// A leaf's tab bar: its tabs, then New shell / pane menu / split / close.
#[allow(clippy::too_many_arguments)]
fn render_leaf_tab_bar<F, S, T, E>(
    leaf: PaneLeaf,
    shell_panes: RwSignal<HashMap<String, ShellTab>>,
    active_pane_id: RwSignal<Option<String>>,
    zoomed_pane_id: RwSignal<Option<String>>,
    notifications: RwSignal<Vec<Notification>>,
    connections: RwSignal<Vec<ConnectionDefPublic>>,
    tab_context_menu: RwSignal<Option<TabContextMenu>>,
    renaming_pane_id: RwSignal<Option<String>>,
    rename_input: RwSignal<String>,
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
    // The tabs re-render when the leaf's tabs change, not when another
    // is clicked: a click that replaced them swallowed the second click
    // of a double-click (zoom).
    move || {
        let tabs = leaf.tabs.get();
        view! {
            <div class="pane-tab-bar">
                {tabs.into_iter().enumerate().map(|(i, pid)| {
                    let tab_opt = shell_panes.get_untracked().get(&pid).cloned();
                    let (kind, t_input_opt, conn_id_for_tab) = tab_opt.as_ref()
                        .map(|t| (t.kind, Some(t.current_input), t.conn_id.clone()))
                        .unwrap_or_else(|| (PaneKind::Shell, None, String::new()));
                    // Env color → inline box-shadow so no layout shift.
                    let env_style = connections.get_untracked().iter()
                        .find(|c| c.id == conn_id_for_tab)
                        .and_then(|c| c.env.clone())
                        .as_deref()
                        .and_then(env_color)
                        .map(|c| format!("box-shadow: inset 3px 0 0 {};", c))
                        .unwrap_or_default();
                    let pid_close = pid.clone();
                    let pid_zoom  = pid.clone();
                    let pid_tip   = pid.clone();
                    let pid_dot   = pid.clone();
                    let pid_ctx   = pid.clone();
                    view! {
                        <div class="pane-tab"
                             style=env_style
                             class:pane-tab-active=move || leaf.active.get() == i
                             class:pane-tab-zoomed=move || zoomed_pane_id.get().is_some()
                             title=move || build_tooltip(&pid_tip, shell_panes, connections)
                             on:click={
                                 let tab_pid = pid.clone();
                                 move |_| {
                                     leaf.active.set(i);
                                     active_pane_id.set(Some(tab_pid.clone()));
                                 }
                             }
                             on:dblclick=move |_| {
                                 if zoomed_pane_id.get_untracked().is_some() {
                                     zoomed_pane_id.set(None);
                                 } else {
                                     zoomed_pane_id.set(Some(pid_zoom.clone()));
                                 }
                             }
                             on:contextmenu=move |e: web_sys::MouseEvent| {
                                 e.prevent_default();
                                 tab_context_menu.set(Some(TabContextMenu {
                                     pane_id: pid_ctx.clone(),
                                     x: e.client_x() as f64,
                                     y: e.client_y() as f64,
                                 }));
                             }>
                            {move || {
                                // Activity dot — red if any error notification for
                                // this pane, else green if any normal notification,
                                // else nothing. Mirrors the bell's severity vocab.
                                let nots = notifications.get();
                                let mut has_err = false;
                                let mut has_ok  = false;
                                for n in &nots {
                                    if n.pane_id != pid_dot { continue; }
                                    if matches!(n.event, NotificationEvent::Err { .. }) { has_err = true; }
                                    else { has_ok = true; }
                                }
                                let color = if has_err { "var(--red)" }
                                            else if has_ok { "var(--green)" }
                                            else { return view! { <span/> }.into_view(); };
                                view! { <span class="pane-tab-activity-dot" style=format!("background:{}", color)/> }.into_view()
                            }}
                            {
                                // ⚠ marker when this pane's session is stale —
                                // visible at-a-glance even when the toolbar is
                                // off-screen. Reactive on session_stale so the
                                // marker appears/clears without re-rendering
                                // the whole tab bar.
                                let stale_signal = tab_opt.as_ref().map(|t| t.session_stale);
                                view! {
                                    {move || stale_signal.and_then(|s| s.get().then_some(()))
                                        .map(|_| view! {
                                            <span class="pane-tab-stale"
                                                  title="Session lost — close this pane and re-open from the sidebar">
                                                "⚠"
                                            </span>
                                        })}
                                }
                            }
                            <span class="pane-tab-icon">{crate::shell::pane_kind_icon(kind)}</span>
                            {
                                let pid_ren_label = pid.clone();
                                view! {
                                    {move || {
                                        let is_renaming = renaming_pane_id.get().as_deref() == Some(&pid_ren_label);
                                        if is_renaming {
                                            let pid_commit = pid_ren_label.clone();
                                            let pid_cancel = pid_ren_label.clone();
                                            let input_id = format!("pane-tab-rename-input-{}", pid_ren_label);
                                            view! {
                                                <input id=input_id class="pane-tab-rename-input"
                                                    prop:value=move || rename_input.get()
                                                    on:input=move |e| rename_input.set(event_target_value(&e))
                                                    on:click=move |e| {
                                                        use wasm_bindgen::JsCast;
                                                        let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                                                    }
                                                    on:keydown=move |e: web_sys::KeyboardEvent| {
                                                        match e.key().as_str() {
                                                            "Enter" => {
                                                                e.prevent_default();
                                                                let v = rename_input.get().trim().to_string();
                                                                if let Some(tab) = shell_panes.get_untracked().get(&pid_commit).cloned() {
                                                                    tab.custom_label.set(if v.is_empty() { None } else { Some(v) });
                                                                }
                                                                renaming_pane_id.set(None);
                                                            }
                                                            "Escape" => {
                                                                e.prevent_default();
                                                                renaming_pane_id.set(None);
                                                            }
                                                            _ => {}
                                                        }
                                                        let _ = &pid_cancel;
                                                    }
                                                    on:blur=move |_| { renaming_pane_id.set(None); }
                                                />
                                            }.into_view()
                                        } else {
                                            // The database it's on, unless renamed —
                                            // read here, so a `\c` re-renders the label,
                                            // not the tab bar.
                                            let effective = tab_opt.as_ref().map_or_else(|| "Shell".to_string(), |t| {
                                                t.custom_label.get().unwrap_or_else(|| {
                                                    let db = t.current_db.get();
                                                    if db.is_empty() { t.label.clone() } else { db }
                                                })
                                            });
                                            view! { <span class="pane-tab-label">{effective}</span> }.into_view()
                                        }
                                    }}
                                }
                            }
                            <span class="pane-tab-close"
                                class:pane-tab-close-dirty=move || t_input_opt
                                    .map(|s| !s.get().trim().is_empty())
                                    .unwrap_or(false)
                                on:click=move |e| {
                                    use wasm_bindgen::JsCast;
                                    let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                                    close_pane(pid_close.clone());
                                }>"×"</span>
                        </div>
                    }
                }).collect_view()}
                <span class="pane-tab-spacer"/>
                // The actions apply to the active tab.
                {move || {
                    let active_pid = leaf.active_pane_id().unwrap_or_default();
                    let plane = shell_panes.get_untracked().get(&active_pid)
                        .map(|t| crate::plane::plane_for(t.db_type))
                        .unwrap_or_else(|| crate::plane::plane_for(shared::DbType::MySql));
                    let pid_new   = active_pid.clone();
                    let pid_sh    = active_pid.clone();
                    let pid_sv    = active_pid.clone();
                    let pid_cl    = active_pid.clone();
                    let menu_open = shell_panes.get_untracked().get(&active_pid)
                        .map(|t| t.pane_menu_open).unwrap_or_else(|| create_rw_signal(false));
                    let pid_menu  = active_pid.clone();
                    view! {
                        <button class="tab-action-icon" title="New shell"
                            on:click=move |_| add_tab(pid_new.clone())>{crate::shell::icon_new_shell()}</button>
                        <span class="pane-menu-trigger"
                            on:mouseenter=move |_| menu_open.set(true)
                            on:mouseleave=move |_| menu_open.set(false)>
                            <button class="tab-action-icon" title="Open a pane…">{crate::shell::icon_panes()}</button>
                            {crate::shell::pane_type_menu(pid_menu.clone(), plane, menu_open, add_pane)}
                        </span>
                        <button class="tab-action-icon" title="Split vertical"
                            on:click=move |_| do_split(pid_sh.clone(), SplitDir::H)>{crate::shell::icon_split_v()}</button>
                        <button class="tab-action-icon" title="Split horizontal"
                            on:click=move |_| do_split(pid_sv.clone(), SplitDir::V)>{crate::shell::icon_split_h()}</button>
                        <button class="tab-action-icon" title="Close pane"
                            on:click=move |_| close_pane(pid_cl.clone())>
                            <span style="font-size:14px">"×"</span>
                        </button>
                    }
                }}
            </div>
        }.into_view()
    }
}

/// The bar between a split's two sides; dragging it sets the split's
/// ratio, measured against the split's region of the pane area.
fn render_divider(dir: SplitDir, ratio: RwSignal<f64>, placement: Memo<pane_layout::Placement>) -> impl IntoView {
    use wasm_bindgen::JsCast;
    let is_h = dir == SplitDir::H;
    let dragging = create_rw_signal(false);
    let target = |e: &web_sys::PointerEvent| e.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok());

    let on_down = move |e: web_sys::PointerEvent| {
        dragging.set(true);
        if let Some(el) = target(&e) { let _ = el.set_pointer_capture(e.pointer_id()); }
    };
    let on_move = move |e: web_sys::PointerEvent| {
        if !dragging.get_untracked() { return; }
        let Some(area) = target(&e).and_then(|el| el.parent_element()) else { return };
        let Some(region) = placement.with_untracked(|p| {
            p.dividers.iter().find(|d| d.ratio == ratio).map(|d| d.region)
        }) else { return };
        let area = area.get_bounding_client_rect();
        let (pointer, start, size) = if is_h {
            (e.client_x() as f64 - area.left(), region.left.resolve(area.width()), region.width.resolve(area.width()))
        } else {
            (e.client_y() as f64 - area.top(), region.top.resolve(area.height()), region.height.resolve(area.height()))
        };
        if size > 0.0 { ratio.set(((pointer - start) / size).clamp(0.1, 0.9)); }
    };
    let on_up = move |e: web_sys::PointerEvent| {
        dragging.set(false);
        if let Some(el) = target(&e) { let _ = el.release_pointer_capture(e.pointer_id()); }
    };
    let style = move || placement.with(|p| {
        p.dividers.iter().find(|d| d.ratio == ratio)
            .map_or_else(|| "display:none".to_string(), |d| d.rect.style())
    });
    view! {
        <div class="split-divider"
             class:split-divider-h=is_h
             class:split-divider-v=!is_h
             class:split-divider-active=move || dragging.get()
             style=style
             on:pointerdown=on_down
             on:pointermove=on_move
             on:pointerup=on_up>
        </div>
    }
}

