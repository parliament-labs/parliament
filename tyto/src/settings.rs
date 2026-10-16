//! Settings dialog. The external editor for `\e` / `\ef` / `\ev`, and
//! the terminal it opens in, are app-wide preferences noctua keeps in
//! `~/.sqlclient/settings.json`. The time zone Postgres sessions ask for
//! is this browser's own choice, kept in its localStorage
//! ([`TimeZoneChoice`]) — the user is at the browser, which may not be
//! on noctua's machine.
//!
//! The dialog edits a draft; Save stores it. Save & Test saves, then has
//! noctua open the *saved* editor on a sample, so no command ever
//! travels from the browser.

use leptos::*;
use shared::settings::{AppSettings, EditorPreset, EditorSettings, LaunchMode, TerminalApp};
use shared::{ClientMessage, DbType};
use web_sys::WebSocket;

use crate::dom::ws_send;
use crate::plane::plane_for;

/// The time zone this browser asks Postgres sessions for. The closer to
/// the user, the stronger: this choice beats `PGTZ` in noctua's
/// environment, which beats the server's own setting — as psql's `PGTZ`
/// beats the server's. Kept in this browser, not in noctua's settings,
/// so each browser keeps its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TimeZoneChoice {
    /// What psql gets: `PGTZ` in noctua's environment, else the server's.
    Default,
    /// This browser's zone, as it is when a session opens.
    Browser,
    /// A zone by name, as Postgres knows it (`UTC`, `Europe/Berlin`).
    Named(String),
}

/// localStorage key of [`TimeZoneChoice`].
const TIME_ZONE_KEY: &str = "session:timezone";

impl TimeZoneChoice {
    /// From what's stored: nothing (or a blank name) is `Default`.
    fn from_stored(stored: Option<&str>) -> Self {
        match stored.map(str::trim) {
            None | Some("") => TimeZoneChoice::Default,
            Some("browser") => TimeZoneChoice::Browser,
            Some(zone) => TimeZoneChoice::Named(zone.to_string()),
        }
    }

    fn stored(&self) -> Option<&str> {
        match self {
            TimeZoneChoice::Default => None,
            TimeZoneChoice::Browser => Some("browser"),
            TimeZoneChoice::Named(zone) => Some(zone.trim()).filter(|z| !z.is_empty()),
        }
    }

    pub(crate) fn load() -> Self {
        let stored = local_storage().and_then(|s| s.get_item(TIME_ZONE_KEY).ok().flatten());
        Self::from_stored(stored.as_deref())
    }

    fn save(&self) {
        let Some(storage) = local_storage() else { return };
        let _ = match self.stored() {
            Some(value) => storage.set_item(TIME_ZONE_KEY, value),
            None => storage.remove_item(TIME_ZONE_KEY),
        };
    }

    /// The zone a session asks for — None: left to `PGTZ`, else the server.
    fn zone(&self) -> Option<String> {
        match self {
            TimeZoneChoice::Default => None,
            TimeZoneChoice::Browser => browser_time_zone(),
            TimeZoneChoice::Named(zone) => Some(zone.trim().to_string()).filter(|z| !z.is_empty()),
        }
    }
}

/// The time zone a Postgres session opened now asks for, by this
/// browser's choice (`ClientMessage::Connect.timezone`).
pub(crate) fn session_time_zone() -> Option<String> {
    TimeZoneChoice::load().zone()
}

/// This browser's time zone, as its system has it (`America/Los_Angeles`).
fn browser_time_zone() -> Option<String> {
    let format = js_sys::Intl::DateTimeFormat::new(&js_sys::Array::new(), &js_sys::Object::new());
    js_sys::Reflect::get(&format.resolved_options(), &"timeZone".into()).ok()?.as_string()
}

/// The zones this browser knows, for the name field's suggestions — none
/// from a browser without `Intl.supportedValuesOf` (before Chrome 99,
/// Safari 15.4).
fn known_time_zones() -> Vec<String> {
    let intl = js_sys::Reflect::get(&js_sys::global(), &"Intl".into()).unwrap_or_default();
    if !js_sys::Reflect::has(&intl, &"supportedValuesOf".into()).unwrap_or(false) {
        return Vec::new();
    }
    js_sys::Intl::supported_values_of(js_sys::Intl::SupportedValuesKey::TimeZone)
        .iter().filter_map(|zone| zone.as_string()).collect()
}

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

/// Where a save or test stands.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Status {
    Idle,
    Saving { then_test: bool },
    /// Waiting for the test editor to close.
    Testing,
    Done { ok: bool, message: String },
}

/// Settings state the app owns; the socket handler feeds it.
#[derive(Clone, Copy)]
pub(crate) struct SettingsState {
    pub(crate) open:   RwSignal<bool>,
    /// As last loaded or saved.
    pub(crate) saved:  RwSignal<AppSettings>,
    /// Why settings.json couldn't be used, if it couldn't.
    pub(crate) notice: RwSignal<Option<String>>,
    pub(crate) status: RwSignal<Status>,
}

impl SettingsState {
    pub(crate) fn new() -> Self {
        SettingsState {
            open:   create_rw_signal(false),
            saved:  create_rw_signal(AppSettings::default()),
            notice: create_rw_signal(None),
            status: create_rw_signal(Status::Idle),
        }
    }

    pub(crate) fn show(self) {
        self.status.set(Status::Idle);
        self.open.set(true);
    }

    fn close(self) {
        self.open.set(false);
        self.status.set(Status::Idle);
    }

    pub(crate) fn loaded(self, settings: AppSettings, load_error: Option<String>) {
        self.saved.set(settings);
        self.notice.set(load_error);
    }

    pub(crate) fn saved(self, ws: RwSignal<Option<WebSocket>>, settings: AppSettings) {
        self.saved.set(settings);
        self.notice.set(None);
        match self.status.get_untracked() {
            Status::Saving { then_test: true } => {
                self.status.set(Status::Testing);
                ws_send(ws, &test_message());
            }
            Status::Saving { then_test: false } => self.close(),
            _ => {}
        }
    }

    /// Replies only count while this dialog is waiting for one: a reply
    /// to a dialog that was closed meanwhile is dropped.
    pub(crate) fn failed(self, message: String) {
        if matches!(self.status.get_untracked(), Status::Saving { .. }) {
            self.status.set(Status::Done { ok: false, message });
        }
    }

    pub(crate) fn tested(self, ok: bool, message: String) {
        if self.status.get_untracked() == Status::Testing {
            self.status.set(Status::Done { ok, message: format!("Saved. {message}") });
        }
    }
}

fn connected(ws: RwSignal<Option<WebSocket>>) -> bool {
    ws.with_untracked(|s| s.as_ref().is_some_and(|s| s.ready_state() == WebSocket::OPEN))
}

/// The plane whose environment variables stand in for "Environment"
/// when testing — there's no connection to take them from.
const TEST_PLANE: DbType = DbType::Postgres;

fn test_message() -> ClientMessage {
    let plane = plane_for(TEST_PLANE);
    ClientMessage::TestEditor {
        env_chain:     plane.editor_env_chain().to_vec(),
        line_flag_var: plane.editor_line_flag_var(),
    }
}

/// One line saying what the chosen editor will run.
fn describe_editor(e: &EditorSettings) -> String {
    match e.preset {
        EditorPreset::Environment => {
            let chains: Vec<String> = [DbType::Postgres, DbType::MySql].iter().map(|&t| {
                let plane = plane_for(t);
                let vars: Vec<String> = plane.editor_env_chain().iter().map(|v| format!("${}", v.name())).collect();
                format!("{}: {}", plane.name(), vars.join(" → "))
            }).collect();
            format!("Like the database's own CLI — {}. If none is set, vi.", chains.join("; "))
        }
        EditorPreset::Custom => "Your command, with the file (and line) added.".into(),
        p => {
            let spec = p.spec();
            let place = if spec.in_terminal { "in a terminal window" } else { "directly; it waits for you to close the file" };
            format!("Runs `{} {}` {place}.", spec.argv.join(" "), spec.line_args.replace("{file}", "FILE").replace("{line}", "N"))
        }
    }
}

#[component]
pub(crate) fn SettingsDialog(state: SettingsState, ws: RwSignal<Option<WebSocket>>) -> impl IntoView {
    let draft = create_rw_signal(state.saved.get_untracked().editor);
    let zone_draft = create_rw_signal(TimeZoneChoice::load());
    let browser_zone = browser_time_zone().unwrap_or_else(|| "unknown".into());
    let busy = move || matches!(state.status.get(), Status::Saving { .. } | Status::Testing);
    let close = move || state.close();
    let save = move |then_test: bool| {
        // The time zone is this browser's own: kept here, noctua or not.
        zone_draft.get_untracked().save();
        if !connected(ws) {
            state.status.set(Status::Done {
                ok: false,
                message: "Not connected to noctua — the editor settings weren't saved (the time zone was, in this browser).".into(),
            });
            return;
        }
        let settings = AppSettings { editor: draft.get_untracked(), ..state.saved.get_untracked() };
        state.status.set(Status::Saving { then_test });
        ws_send(ws, &ClientMessage::SaveSettings { settings });
    };
    let set = move |f: &dyn Fn(&mut EditorSettings)| draft.update(|d| f(d));
    let dialog_ref = create_node_ref::<html::Div>();
    // Focus the dialog so Esc closes it without a click first.
    dialog_ref.on_load(|el| request_animation_frame(move || { let _ = el.focus(); }));

    view! {
        <div class="app-dialog-backdrop" on:mousedown=move |_| close()></div>
        <div class="app-dialog app-dialog-wide settings-dialog" tabindex="-1" node_ref=dialog_ref
             role="dialog" aria-modal="true" aria-labelledby="settings-title"
             on:keydown=move |e: web_sys::KeyboardEvent| {
                 if e.key() == "Escape" {
                     close();
                 }
             }>
            <div class="app-dialog-title" id="settings-title">"Settings"</div>
            {move || state.notice.get().map(|n| view! { <div class="settings-notice">{n}</div> })}

            <div class="settings-section">"Editor"</div>
            <div class="settings-lede">
                "Opened by \\e, \\ef and \\ev. A connection's own editor override, if set, wins over this."
            </div>
            <div class="form-row">
                <label class="form-label" for="settings-editor">"Open with"</label>
                <select id="settings-editor" class="form-input"
                    on:change=move |e| { let v = event_target_value(&e); set(&|d| d.preset = EditorPreset::parse(&v)); }>
                    {EditorPreset::ALL.iter().map(|&p| view! {
                        <option value=p.as_str() selected=move || draft.get().preset == p>{p.spec().label}</option>
                    }).collect_view()}
                </select>
                <div class="settings-hint">{move || describe_editor(&draft.get())}</div>
            </div>
            {move || (draft.get().preset == EditorPreset::Custom).then(|| view! {
                <div class="form-row">
                    <label class="form-label" for="settings-command">"Command"</label>
                    <input id="settings-command" class="form-input" type="text" placeholder="e.g. code -w, or /path/to/editor --wait"
                        prop:value=move || draft.get().command
                        on:input=move |e| { let v = event_target_value(&e); set(&|d| d.command = v.clone()); } />
                </div>
                <div class="form-row">
                    <label class="form-label" for="settings-line">"Open at line"</label>
                    <input id="settings-line" class="form-input" type="text" placeholder="+{line} {file}"
                        prop:value=move || draft.get().line_args
                        on:input=move |e| { let v = event_target_value(&e); set(&|d| d.line_args = v.clone()); } />
                    <div class="settings-hint">"{line} and {file} are filled in. Blank: open at the top."</div>
                </div>
                <div class="form-row">
                    <label class="form-label" for="settings-launch">"Launch"</label>
                    <select id="settings-launch" class="form-input"
                        on:change=move |e| { let v = event_target_value(&e); set(&|d| d.launch = LaunchMode::parse(&v)); }>
                        {LaunchMode::ALL.iter().map(|&m| view! {
                            <option value=m.as_str() selected=move || draft.get().launch == m>{m.label()}</option>
                        }).collect_view()}
                    </select>
                </div>
            })}
            <div class="form-row">
                <label class="form-label" for="settings-terminal">"Terminal"</label>
                <select id="settings-terminal" class="form-input"
                    on:change=move |e| { let v = event_target_value(&e); set(&|d| d.terminal = TerminalApp::parse(&v)); }>
                    {TerminalApp::ALL.iter().map(|&t| view! {
                        <option value=t.as_str() selected=move || draft.get().terminal == t>{t.label()}</option>
                    }).collect_view()}
                </select>
                <div class="settings-hint">"Where editors that need a terminal (vi, nano, …) open."</div>
            </div>
            {move || (draft.get().terminal == TerminalApp::Custom).then(|| view! {
                <div class="form-row">
                    <label class="form-label" for="settings-terminal-command">"Terminal command"</label>
                    <input id="settings-terminal-command" class="form-input" type="text" placeholder="open -na Ghostty --args -e {cmd}"
                        prop:value=move || draft.get().terminal_template
                        on:input=move |e| { let v = event_target_value(&e); set(&|d| d.terminal_template = v.clone()); } />
                    <div class="settings-hint">"{cmd} is replaced by the script that runs the editor."</div>
                </div>
            })}

            <div class="settings-section">"Time zone"</div>
            <div class="settings-lede">
                "For Postgres sessions this browser opens from now on — a new tab, \\c, a reconnect. \
                 Kept in this browser. SET TimeZone in a shell still changes its own session."
            </div>
            <div class="form-row">
                <label class="form-label" for="settings-timezone">"Sessions use"</label>
                <select id="settings-timezone" class="form-input"
                    on:change=move |e| {
                        let choice = match event_target_value(&e).as_str() {
                            "browser" => TimeZoneChoice::Browser,
                            "other" => TimeZoneChoice::Named(String::new()),
                            _ => TimeZoneChoice::Default,
                        };
                        zone_draft.set(choice);
                    }>
                    <option value="default" selected=move || zone_draft.get() == TimeZoneChoice::Default>"Default"</option>
                    <option value="browser" selected=move || zone_draft.get() == TimeZoneChoice::Browser>
                        {format!("This browser's ({browser_zone})")}
                    </option>
                    <option value="other" selected=move || matches!(zone_draft.get(), TimeZoneChoice::Named(_))>"Another zone"</option>
                </select>
                <div class="settings-hint">{move || match zone_draft.get() {
                    TimeZoneChoice::Default => "As psql: $PGTZ in noctua's environment, else the server's setting.",
                    TimeZoneChoice::Browser => "Wherever this browser is — above $PGTZ and the server's setting.",
                    TimeZoneChoice::Named(_) => "Above $PGTZ and the server's setting. The server checks the name: one it doesn't know refuses the session.",
                }}</div>
            </div>
            {move || matches!(zone_draft.get(), TimeZoneChoice::Named(_)).then(|| view! {
                <div class="form-row">
                    <label class="form-label" for="settings-timezone-name">"Zone"</label>
                    <input id="settings-timezone-name" class="form-input" type="text" list="settings-timezones"
                        placeholder="e.g. UTC, Europe/Berlin"
                        prop:value=move || match zone_draft.get() { TimeZoneChoice::Named(zone) => zone, _ => String::new() }
                        on:input=move |e| zone_draft.set(TimeZoneChoice::Named(event_target_value(&e))) />
                    <datalist id="settings-timezones">
                        {known_time_zones().into_iter().map(|zone| view! { <option value=zone/> }).collect_view()}
                    </datalist>
                </div>
            })}

            {move || match state.status.get() {
                Status::Idle => None,
                Status::Saving { .. } => Some(view! { <div class="settings-status">"Saving…"</div> }),
                Status::Testing => Some(view! {
                    <div class="settings-status">"Saved. Waiting for the editor — make a change and save, or just quit."</div>
                }),
                Status::Done { ok, message } => {
                    // "Environment" was tried with one CLI's variables;
                    // say which, since MySQL skips $PSQL_EDITOR.
                    let note = if draft.get_untracked().preset == EditorPreset::Environment
                        && message.starts_with("Saved.")
                    {
                        format!(" (Tested with {}'s variables.)", plane_for(TEST_PLANE).name())
                    } else {
                        String::new()
                    };
                    Some(view! {
                        <div class="settings-status" class:settings-status-ok=ok class:settings-status-err=!ok>
                            {message}{note}
                        </div>
                    })
                }
            }}

            <div class="app-dialog-actions">
                <button class="btn btn-ghost" on:click=move |_| close()>"Cancel"</button>
                <button class="btn btn-ghost" disabled=busy on:click=move |_| save(true)>"Save & Test"</button>
                <button class="btn btn-primary" disabled=busy on:click=move |_| save(false)>"Save"</button>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_only_land_while_the_dialog_waits_for_them() {
        let _rt = create_runtime();
        let ws = create_rw_signal(None);
        let state = SettingsState::new();
        state.show();
        // Save & Test: saved → testing → result.
        state.status.set(Status::Saving { then_test: true });
        state.saved(ws, AppSettings::default());
        assert_eq!(state.status.get_untracked(), Status::Testing);
        state.tested(true, "`vim` opened.".into());
        assert_eq!(state.status.get_untracked(), Status::Done { ok: true, message: "Saved. `vim` opened.".into() });
        // Save closes the dialog.
        state.status.set(Status::Saving { then_test: false });
        state.saved(ws, AppSettings::default());
        assert!(!state.open.get_untracked());
        assert_eq!(state.status.get_untracked(), Status::Idle);
        // A late test result or error for a dialog closed meanwhile is dropped.
        state.show();
        state.tested(false, "late".into());
        state.failed("late".into());
        assert_eq!(state.status.get_untracked(), Status::Idle);
        // A save from another tab reloads but doesn't close this dialog.
        state.saved(ws, AppSettings::default());
        assert!(state.open.get_untracked());
    }

    #[test]
    fn the_time_zone_choice_is_stored_as_typed() {
        for (stored, choice) in [
            (None, TimeZoneChoice::Default),
            (Some(" "), TimeZoneChoice::Default),
            (Some("browser"), TimeZoneChoice::Browser),
            (Some("Europe/Berlin"), TimeZoneChoice::Named("Europe/Berlin".into())),
        ] {
            assert_eq!(TimeZoneChoice::from_stored(stored), choice);
        }
        assert_eq!(TimeZoneChoice::Named(" UTC ".into()).stored(), Some("UTC"));
        // A blank name is the default: nothing stored, nothing asked.
        assert_eq!(TimeZoneChoice::Named("  ".into()).stored(), None);
        assert_eq!(TimeZoneChoice::Named("  ".into()).zone(), None);
        assert_eq!(TimeZoneChoice::Default.zone(), None);
        assert_eq!(TimeZoneChoice::Named("UTC".into()).zone(), Some("UTC".into()));
    }

    #[test]
    fn describes_presets_and_environment() {
        let mut e = EditorSettings { preset: EditorPreset::Vim, ..Default::default() };
        assert_eq!(describe_editor(&e), "Runs `vim +N FILE` in a terminal window.");
        e.preset = EditorPreset::VsCode;
        assert_eq!(describe_editor(&e), "Runs `code -w -g FILE:N` directly; it waits for you to close the file.");
        e.preset = EditorPreset::Environment;
        let d = describe_editor(&e);
        assert!(d.contains("PostgreSQL: $PSQL_EDITOR → $EDITOR → $VISUAL"), "{d}");
        assert!(d.contains("MySQL: $EDITOR → $VISUAL"), "{d}");
    }
}
