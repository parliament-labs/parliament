use leptos::*;
use web_sys::WebSocket;

use crate::dom::ws_introspect;
use crate::plane::{DbmsPlane, plane_for};
use crate::types::*;

// ── Activity pane: blocker tree model ────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct BlockerNode {
    pub(crate) pid:      String,
    pub(crate) depth:    usize,
    pub(crate) kind:     BlockerNodeKind,
    pub(crate) children: Vec<BlockerNode>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum BlockerNodeKind {
    Live,
    Stub,
    Cycle,
}

pub(crate) fn build_blocker_trees(edges: &[BlockEdge]) -> Vec<BlockerNode> {
    use std::collections::{BTreeMap, BTreeSet};
    if edges.is_empty() { return Vec::new(); }

    let mut blocks: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut waiters: BTreeSet<String> = BTreeSet::new();
    for e in edges {
        blocks.entry(e.blocker_pid.clone()).or_default().push(e.waiter_pid.clone());
        waiters.insert(e.waiter_pid.clone());
    }

    let roots: Vec<String> = blocks.keys()
        .filter(|b| !waiters.contains(*b))
        .cloned()
        .collect();

    let roots = if roots.is_empty() {
        blocks.keys().next().cloned().into_iter().collect()
    } else {
        roots
    };

    let mut claimed: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for root in &roots {
        let mut path: BTreeSet<String> = BTreeSet::new();
        out.push(render_subtree(root, &blocks, &mut claimed, &mut path, 0));
    }
    out
}

pub(crate) fn render_subtree(
    pid: &str,
    blocks: &std::collections::BTreeMap<String, Vec<String>>,
    claimed: &mut std::collections::BTreeSet<String>,
    path: &mut std::collections::BTreeSet<String>,
    depth: usize,
) -> BlockerNode {
    claimed.insert(pid.to_string());
    path.insert(pid.to_string());
    let mut children = Vec::new();
    if let Some(waiters) = blocks.get(pid) {
        for w in waiters {
            if path.contains(w) {
                children.push(BlockerNode {
                    pid: w.clone(), depth: depth + 1,
                    kind: BlockerNodeKind::Cycle, children: Vec::new(),
                });
            } else if claimed.contains(w) {
                children.push(BlockerNode {
                    pid: w.clone(), depth: depth + 1,
                    kind: BlockerNodeKind::Stub, children: Vec::new(),
                });
            } else {
                children.push(render_subtree(w, blocks, claimed, path, depth + 1));
            }
        }
    }
    path.remove(pid);
    BlockerNode {
        pid: pid.to_string(), depth,
        kind: BlockerNodeKind::Live, children,
    }
}

pub(crate) fn flatten_tree(nodes: &[BlockerNode], out: &mut Vec<BlockerNode>) {
    for n in nodes {
        out.push(BlockerNode {
            pid: n.pid.clone(),
            depth: n.depth,
            kind: n.kind,
            children: Vec::new(),
        });
        flatten_tree(&n.children, out);
    }
}

pub(crate) fn fmt_last_updated(ms: f64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    let h = d.get_hours();
    let m = d.get_minutes();
    let s = d.get_seconds();
    format!("{:02}:{:02}:{:02}", h, m, s)
}

// ── Activity pane renderer ────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_activity_pane<F, S, T, E>(
    pane_id: String,
    tab: ShellTab,
    active_pane_id: RwSignal<Option<String>>,
    ws: RwSignal<Option<WebSocket>>,
    _close_pane: F,
    _do_split: S,
    _add_tab: T,
    _add_pane: E,
) -> impl IntoView
where
    F: Fn(String) + Copy + 'static,
    S: Fn(String, SplitDir) + Copy + 'static,
    T: Fn(String) + Copy + 'static,
    E: Fn(String, PaneKind, Option<ExplorerInit>) + Copy + 'static,
{

    let plane: &'static dyn DbmsPlane = plane_for(tab.db_type);
    let _menu_open    = tab.pane_menu_open;

    let session_id      = tab.session_id;
    let session_version = tab.session_version;
    let db_conn_id      = tab.db_connection_id;
    // Per-pane right-click menu on a session's Query cell — (x, y, query).
    // Local to render_activity_pane so each Activity pane has its own.
    let query_menu: RwSignal<Option<(f64, f64, String)>> = create_rw_signal(None);
    let sessions_sig = tab.activity_sessions;
    let locks_sig    = tab.activity_locks;
    let blocks_sig   = tab.activity_blocks;
    let interval_sig = tab.activity_interval;
    let hide_idle    = tab.activity_hide_idle;
    let sort_sig     = tab.activity_sort;
    let last_upd     = tab.activity_last_updated;
    let tick_sig     = tab.activity_tick;
    let confirm_kill = tab.activity_confirm_kill;
    let timer_slot   = tab.activity_timer;
    let sessions_collapsed = tab.activity_sessions_collapsed;
    let locks_collapsed    = tab.activity_locks_collapsed;
    let blocks_collapsed   = tab.activity_blocks_collapsed;
    let expanded_queries   = tab.activity_expanded_queries;

    let _pid_close   = pane_id.clone();
    let _pid_split_h = pane_id.clone();
    let _pid_split_v = pane_id.clone();
    let _pid_add_tab = pane_id.clone();
    let _pid_menu    = pane_id.clone();
    let pid_focus   = pane_id.clone();

    crate::dom::setup_interval_effect(interval_sig, 1000, tick_sig, timer_slot, false);

    create_effect(move |_| {
        let _ = tick_sig.get();
        let hide = hide_idle.get();
        let Some(sid) = session_id.get() else { return; };
        // `session_version` is the raw VERSION()/server_version string
        // stashed on SessionOpened. The MySQL plane reads it to pick
        // between `performance_schema.data_locks` (8.0+) and
        // `information_schema.INNODB_LOCKS` (5.x). PG ignores it.
        // See `MySqlPlane::locks_sql` for the branch + audit cluster D
        // finding 3.6.
        let server_version = session_version.get_untracked();
        ws_introspect(ws, &sid, plane.sessions_sql(hide),       "activity_sessions".into());
        ws_introspect(ws, &sid, plane.locks_sql(&server_version), "activity_locks".into());
        ws_introspect(ws, &sid, plane.blockers_sql(),           "activity_blocks".into());
    });

    view! {
        <div class="shell explorer-shell activity-shell"
             on:click=move |_| active_pane_id.set(Some(pid_focus.clone()))>

            <div class="explorer-controls activity-control-bar">
                <div class="explorer-control-group">
                    <label class="explorer-control-label">"Refresh"</label>
                    <select class="explorer-select"
                        on:change=move |ev| {
                            let v = event_target_value(&ev);
                            interval_sig.set(match v.as_str() {
                                "manual" => None,
                                other => other.parse::<u32>().ok(),
                            });
                        }>
                        <option value="manual" selected=move || interval_sig.get().is_none()>"Manual"</option>
                        <option value="1"  selected=move || interval_sig.get() == Some(1)>"1s"</option>
                        <option value="2"  selected=move || interval_sig.get() == Some(2)>"2s"</option>
                        <option value="5"  selected=move || interval_sig.get() == Some(5)>"5s"</option>
                        <option value="10" selected=move || interval_sig.get() == Some(10)>"10s"</option>
                        <option value="30" selected=move || interval_sig.get() == Some(30)>"30s"</option>
                        <option value="60" selected=move || interval_sig.get() == Some(60)>"60s"</option>
                    </select>
                </div>

                <label class="explorer-checkbox">
                    <input type="checkbox"
                        prop:checked=move || hide_idle.get()
                        on:change=move |ev| {
                            use wasm_bindgen::JsCast;
                            let checked = ev.target()
                                .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                                .map(|i| i.checked())
                                .unwrap_or(false);
                            hide_idle.set(checked);
                        } />
                    <span>"Hide idle"</span>
                </label>

                <div class="explorer-control-group">
                    <label class="explorer-control-label">"Sort"</label>
                    <select class="explorer-select"
                        on:change=move |ev| {
                            let v = event_target_value(&ev);
                            sort_sig.set(match v.as_str() {
                                "User"    => ActivitySort::User,
                                "Client"  => ActivitySort::Client,
                                "Newest"  => ActivitySort::Newest,
                                _         => ActivitySort::Oldest,
                            });
                        }>
                        {ActivitySort::ALL.iter().map(|s| {
                            let cur = *s;
                            view! {
                                <option value=cur.label()
                                    selected=move || sort_sig.get() == cur>
                                    {cur.label()}
                                </option>
                            }
                        }).collect_view()}
                    </select>
                </div>

                <button class="explorer-refresh-btn" title="Refresh now"
                    on:click=move |_| tick_sig.update(|t| *t += 1)>"↻"</button>

                <span class="activity-last-updated">
                    {move || match last_upd.get() {
                        Some(ms) => format!("Last updated {}", fmt_last_updated(ms)),
                        None     => "Waiting for first refresh…".to_string(),
                    }}
                </span>
                {crate::shell::session_id_pill(db_conn_id, tab.session_stale)}
            </div>

            <div class="explorer-body">
                <div class="schema-scroll">
                    <div class="schema-section-header activity-section-toggle"
                         on:click=move |_| sessions_collapsed.update(|v| *v = !*v)>
                        "Sessions"
                    </div>
                    {move || if sessions_collapsed.get() {
                        view! { <div/> }.into_view()
                    } else {
                        render_sessions_table(
                            sessions_sig.get(),
                            blocks_sig.get(),
                            sort_sig.get(),
                            confirm_kill,
                            query_menu,
                            expanded_queries,
                            plane,
                            ws,
                            session_id,
                            tick_sig,
                        )
                    }}

                    {move || {
                        let edges = match blocks_sig.get() {
                            LoadState::Loaded(e) if !e.is_empty() => e,
                            _ => return view! { <div/> }.into_view(),
                        };
                        let trees = build_blocker_trees(&edges);
                        let mut flat = Vec::new();
                        flatten_tree(&trees, &mut flat);
                        view! {
                            <div class="schema-section-divider"></div>
                            <div class="schema-section-header activity-section-toggle"
                                 on:click=move |_| blocks_collapsed.update(|v| *v = !*v)>
                                "Blocking"
                            </div>
                            {move || if blocks_collapsed.get() {
                                view! { <div/> }.into_view()
                            } else {
                                view! {
                                    <div class="activity-blocker-tree">
                                        {flat.clone().into_iter().map(|node| {
                                            let indent = format!("padding-left: {}px", 12 + node.depth * 20);
                                            let label = match node.kind {
                                                BlockerNodeKind::Live  => format!("└── PID {}", node.pid),
                                                BlockerNodeKind::Stub  => format!("└── PID {} (shown above)", node.pid),
                                                BlockerNodeKind::Cycle => format!("└── PID {} (cycle)", node.pid),
                                            };
                                            let cls = match node.kind {
                                                BlockerNodeKind::Live  => "activity-blocker-node",
                                                BlockerNodeKind::Stub  => "activity-blocker-node activity-blocker-stub",
                                                BlockerNodeKind::Cycle => "activity-blocker-node activity-blocker-cycle",
                                            };
                                            view! { <div class=cls style=indent>{label}</div> }
                                        }).collect_view()}
                                    </div>
                                }.into_view()
                            }}
                        }.into_view()
                    }}

                    <div class="schema-section-divider"></div>
                    <div class="schema-section-header activity-section-toggle"
                         on:click=move |_| locks_collapsed.update(|v| *v = !*v)>
                        "Locks"
                    </div>
                    {move || if locks_collapsed.get() {
                        view! { <div/> }.into_view()
                    } else {
                        render_locks_table(locks_sig.get())
                    }}
                </div>
            </div>
            // Right-click Copy-query menu for session rows. Fixed-position
            // overlay; mousedown on backdrop dismisses; clicking Copy writes
            // to the clipboard via navigator.clipboard.writeText.
            {move || query_menu.get().map(|(x, y, q)| {
                let q_for_copy = q.clone();
                view! {
                    <div class="tab-context-menu-backdrop"
                         on:mousedown=move |_| query_menu.set(None)
                         on:contextmenu=move |e: web_sys::MouseEvent| {
                             e.prevent_default();
                             query_menu.set(None);
                         } />
                    <div class="tab-context-menu"
                         style=format!("left:{}px; top:{}px", x, y)
                         on:mousedown=move |e: web_sys::MouseEvent| {
                             use wasm_bindgen::JsCast;
                             let _ = e.dyn_ref::<web_sys::Event>().map(|ev| ev.stop_propagation());
                         }>
                        <div class="tab-context-menu-item"
                             on:click=move |_| {
                                 if let Some(nav) = web_sys::window().map(|w| w.navigator()) {
                                     let _ = nav.clipboard().write_text(&q_for_copy);
                                 }
                                 query_menu.set(None);
                             }>"Copy query"</div>
                    </div>
                }
            })}
        </div>
    }
}

// ── Activity sub-renderers ────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_sessions_table(
    sessions_state:   LoadState<Vec<SessionRow>>,
    blocks_state:     LoadState<Vec<BlockEdge>>,
    sort:             ActivitySort,
    confirm_kill:     RwSignal<Option<String>>,
    query_menu:       RwSignal<Option<(f64, f64, String)>>,
    expanded_queries: RwSignal<std::collections::HashSet<String>>,
    plane:            &'static dyn DbmsPlane,
    ws:               RwSignal<Option<WebSocket>>,
    session_id:       RwSignal<Option<String>>,
    tick_sig:         RwSignal<u32>,
) -> leptos::View {
    let rows = match sessions_state {
        LoadState::NotLoaded | LoadState::Loading => {
            return view! { <div class="detail-msg">"Loading sessions…"</div> }.into_view();
        }
        LoadState::Error(e) => {
            return view! { <div class="detail-error">{e}</div> }.into_view();
        }
        LoadState::Loaded(v) => v,
    };

    use std::collections::BTreeMap;
    let (blocked_by_waiter, blocker_set): (BTreeMap<String, Vec<String>>, std::collections::BTreeSet<String>) = {
        let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut b = std::collections::BTreeSet::new();
        if let LoadState::Loaded(edges) = blocks_state {
            for e in &edges {
                m.entry(e.waiter_pid.clone()).or_default().push(e.blocker_pid.clone());
                b.insert(e.blocker_pid.clone());
            }
        }
        (m, b)
    };

    let mut rows = rows;
    match sort {
        ActivitySort::User    => rows.sort_by_key(|a| a.user.to_lowercase()),
        ActivitySort::Client  => rows.sort_by_key(|a| a.client.to_lowercase()),
        ActivitySort::Oldest  => rows.sort_by_key(|a| std::cmp::Reverse(a.time.parse::<i64>().unwrap_or(0))),
        ActivitySort::Newest  => rows.sort_by_key(|a| a.time.parse::<i64>().unwrap_or(0)),
    };

    if rows.is_empty() {
        return view! { <div class="detail-msg">"No sessions"</div> }.into_view();
    }

    let can_kill = plane.supports_session_kill();

    view! {
        <div class="detail-table-wrap">
            <table class="detail-table activity-table">
                <thead>
                    <tr>
                        <th>"PID"</th>
                        <th>"User"</th>
                        <th>"Client"</th>
                        <th>"DB"</th>
                        <th>"State"</th>
                        <th>"Time"</th>
                        <th>"Blocked by"</th>
                        <th>"Query"</th>
                        <th class="activity-th-kill"></th>
                    </tr>
                </thead>
                <tbody>
                    {rows.into_iter().map(|r| {
                        let pid = r.pid.clone();
                        let pid_for_cls = pid.clone();
                        let pid_for_confirm = pid.clone();
                        let pid_for_kill = pid.clone();
                        let blockers = blocked_by_waiter.get(&pid).cloned().unwrap_or_default();
                        let blocked_by = blockers.join(", ");
                        let is_blocked = !blockers.is_empty();
                        let is_blocker = blocker_set.contains(&pid);
                        let query_title = r.query.clone();
                        let query_full  = r.query.clone();
                        let query_for_menu = r.query.clone();
                        let pid_for_expand = r.pid.clone();
                        let pid_for_class  = r.pid.clone();
                        view! {
                            <tr class="activity-row"
                                class:activity-row-blocked=move || is_blocked
                                class:activity-row-blocker=move || is_blocker
                                title=query_title>
                                <td class="col-name">{r.pid.clone()}</td>
                                <td>{r.user}</td>
                                <td class="col-default">{r.client}</td>
                                <td class="col-default">{r.db}</td>
                                <td class="col-null">{r.state}</td>
                                <td class="col-default">{r.time}</td>
                                <td class="activity-blocked-by">{blocked_by}</td>
                                <td class="activity-query"
                                    on:click=move |_| {
                                        expanded_queries.update(|s| {
                                            if !s.insert(pid_for_expand.clone()) {
                                                s.remove(&pid_for_expand);
                                            }
                                        });
                                    }
                                    on:contextmenu=move |e: web_sys::MouseEvent| {
                                        e.prevent_default();
                                        query_menu.set(Some((
                                            e.client_x() as f64,
                                            e.client_y() as f64,
                                            query_for_menu.clone(),
                                        )));
                                    }>
                                    <div class="activity-query-text"
                                         class:activity-query-expanded=move || {
                                             expanded_queries.with(|s| s.contains(&pid_for_class))
                                         }>
                                        {query_full}
                                    </div>
                                </td>
                                <td class="activity-td-kill">
                                    {if can_kill && (is_blocker || is_blocked) {
                                        view! {
                                            {move || if confirm_kill.get().as_deref() == Some(&pid_for_cls) {
                                                let pid_yes = pid_for_confirm.clone();
                                                view! {
                                                    <span class="activity-confirm">
                                                        <span class="activity-confirm-yes"
                                                            on:click=move |ev| {
                                                                use wasm_bindgen::JsCast;
                                                                let _ = ev.dyn_ref::<web_sys::Event>().map(|e| e.stop_propagation());
                                                                if let Some(sid) = session_id.get_untracked() {
                                                                    ws_introspect(ws, &sid, plane.kill_query_sql(&pid_yes), "activity_kill".into());
                                                                }
                                                                tick_sig.update(|t| *t += 1);
                                                            }>"Kill?"</span>
                                                        <span class="activity-confirm-no"
                                                            on:click=move |ev| {
                                                                use wasm_bindgen::JsCast;
                                                                let _ = ev.dyn_ref::<web_sys::Event>().map(|e| e.stop_propagation());
                                                                confirm_kill.set(None);
                                                            }>"✕"</span>
                                                    </span>
                                                }.into_view()
                                            } else {
                                                let pid_open = pid_for_kill.clone();
                                                view! {
                                                    <span class="activity-kill"
                                                        on:click=move |ev| {
                                                            use wasm_bindgen::JsCast;
                                                            let _ = ev.dyn_ref::<web_sys::Event>().map(|e| e.stop_propagation());
                                                            confirm_kill.set(Some(pid_open.clone()));
                                                        }>"✕"</span>
                                                }.into_view()
                                            }}
                                        }.into_view()
                                    } else {
                                        view! { <span/> }.into_view()
                                    }}
                                </td>
                            </tr>
                        }
                    }).collect_view()}
                </tbody>
            </table>
        </div>
    }.into_view()
}

pub(crate) fn render_locks_table(state: LoadState<Vec<LockRow>>) -> leptos::View {
    match state {
        LoadState::NotLoaded | LoadState::Loading => view! {
            <div class="detail-msg">"Loading locks…"</div>
        }.into_view(),
        LoadState::Error(e) => view! {
            <div class="detail-error">{e}</div>
        }.into_view(),
        LoadState::Loaded(rows) if rows.is_empty() => view! {
            <div class="detail-msg">"No locks held"</div>
        }.into_view(),
        LoadState::Loaded(rows) => view! {
            <div class="detail-table-wrap">
                <table class="detail-table activity-table">
                    <thead>
                        <tr>
                            <th>"PID"</th>
                            <th>"Mode"</th>
                            <th>"Object"</th>
                            <th>"Granted"</th>
                            <th>"Duration"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {rows.into_iter().map(|r| {
                            let granted_text = if r.granted { "Yes" } else { "No" };
                            let granted_cls  = if r.granted { "activity-granted-yes" } else { "activity-granted-no" };
                            view! {
                                <tr>
                                    <td class="col-name">{r.pid}</td>
                                    <td class="col-type">{r.mode}</td>
                                    <td class="col-default">{r.object}</td>
                                    <td class=granted_cls>{granted_text}</td>
                                    <td class="col-null">{r.duration}</td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
        }.into_view(),
    }
}
