use leptos::*;
use shared::ClientMessage;
use wasm_bindgen::prelude::*;
use web_sys::WebSocket;

use crate::types::ActivityTimerSlot;

/// Whether the socket to noctua is open — a message sent otherwise is
/// dropped.
pub(crate) fn ws_open(ws: RwSignal<Option<WebSocket>>) -> bool {
    ws.with_untracked(|s| s.as_ref().is_some_and(|s| s.ready_state() == WebSocket::OPEN))
}

pub(crate) fn ws_send(ws: RwSignal<Option<WebSocket>>, msg: &ClientMessage) {
    if let Some(s) = ws.get() {
        if let Ok(j) = serde_json::to_string(msg) { let _ = s.send_with_str(&j); }
    }
}

pub(crate) fn ws_introspect(ws: RwSignal<Option<WebSocket>>, session_id: &str, sql: String, tag: String) {
    ws_send(ws, &ClientMessage::IntrospectQuery { session_id: session_id.to_string(), sql, tag });
}

pub(crate) fn ws_dml(ws: RwSignal<Option<WebSocket>>, session_id: &str, sql: String, tag: String) {
    ws_send(ws, &ClientMessage::ExecuteDml { session_id: session_id.to_string(), sql, tag });
}

pub(crate) fn scroll_shell_to_bottom(pane_id: &str) {
    let id = format!("shell-output-{}", pane_id);
    if let Some(win) = web_sys::window() {
        let cb = Closure::once(move || {
            let _ = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id(&id))
                .map(|el| el.set_scroll_top(el.scroll_height()));
        });
        let _ = win.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    }
}

/// A shell pane's width in characters of its result tables' (monospace)
/// font — what a terminal's width is to psql's `\x auto`: the output's
/// width inside its padding. None when the pane can't be measured (not
/// on screen), as psql has none off a terminal.
pub(crate) fn shell_columns(pane_id: &str) -> Option<usize> {
    let window = web_sys::window()?;
    let document = window.document()?;
    let output = document.get_element_by_id(&format!("shell-output-{pane_id}"))?;
    let probe = document.create_element("span").ok()?;
    probe.set_attribute("class", "shell-table").ok()?;
    probe.set_attribute("style", "position:absolute;visibility:hidden;white-space:pre;padding:0;border:0").ok()?;
    probe.set_text_content(Some(&"0".repeat(100)));
    output.append_child(&probe).ok()?;
    let char_width = probe.get_bounding_client_rect().width() / 100.0;
    probe.remove();
    let style = window.get_computed_style(&output).ok()??;
    let padding = |side: &str| style.get_property_value(side).ok()
        .and_then(|px| px.trim_end_matches("px").parse::<f64>().ok())
        .unwrap_or(0.0);
    let width = output.client_width() as f64 - padding("padding-left") - padding("padding-right");
    (char_width > 0.0 && width > 0.0).then(|| (width / char_width) as usize)
}

/// Focus `el` without scrolling it into view. A shell's input sits at
/// the bottom of its scrollback, so a plain `focus()` yanks a reader who
/// scrolled up back to the bottom; typing still brings the input into
/// view.
pub(crate) fn focus_in_place(el: &web_sys::HtmlElement) {
    let opts = web_sys::FocusOptions::new();
    opts.set_prevent_scroll(true);
    let _ = el.focus_with_options(&opts);
}

/// Put the keyboard in a shell's input on the next frame (after the
/// pane has rendered). No-op when the pane isn't a mounted shell.
pub(crate) fn focus_shell_input(pane_id: &str) {
    let id = format!("shell-input-{}", pane_id);
    if let Some(win) = web_sys::window() {
        let cb = Closure::once(move || {
            use wasm_bindgen::JsCast;
            let _ = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id(&id))
                .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
                .map(|el| el.focus());
        });
        let _ = win.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    }
}

pub(crate) fn reset_shell_input_height(pane_id: &str) {
    use wasm_bindgen::JsCast;
    let id = format!("shell-input-{}", pane_id);
    let _ = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(&id))
        .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        .map(|ta| { let _ = ta.style().set_property("height", "auto"); });
}

pub(crate) fn with_shell_textarea<F>(pane_id: &str, f: F)
where F: FnOnce(&web_sys::HtmlTextAreaElement)
{
    use wasm_bindgen::JsCast;
    let id = format!("shell-input-{}", pane_id);
    if let Some(ta) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(&id))
        .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
    {
        f(&ta);
    }
}

/// Tick `tick_sig` every `interval_sig` (in units of `unit_ms`
/// milliseconds) while it's Some — and at once when `fire_immediately`.
pub(crate) fn setup_interval_effect(
    interval_sig: RwSignal<Option<u32>>,
    unit_ms: u32,
    tick_sig: RwSignal<u32>,
    timer_slot: StoredValue<ActivityTimerSlot>,
    fire_immediately: bool,
) {
    create_effect(move |_| {
        let interval = interval_sig.get();
        timer_slot.update_value(|slot| {
            if let Some((id, _cb)) = slot.take() {
                if let Some(w) = web_sys::window() { w.clear_interval_with_handle(id); }
            }
            if let Some(units) = interval {
                let cb = Closure::<dyn FnMut()>::new(move || {
                    tick_sig.update(|t| *t += 1);
                });
                if let Some(w) = web_sys::window() {
                    if let Ok(id) = w.set_interval_with_callback_and_timeout_and_arguments_0(
                        cb.as_ref().unchecked_ref(),
                        units.saturating_mul(unit_ms).min(i32::MAX as u32) as i32,
                    ) {
                        *slot = Some((id, cb));
                    }
                }
                if fire_immediately {
                    tick_sig.update(|t| *t += 1);
                }
            }
        });
    });
}

pub(crate) fn schedule_textarea_resize(pane_id: &str) {
    let id = format!("shell-input-{}", pane_id);
    if let Some(win) = web_sys::window() {
        let cb = Closure::once(move || {
            use wasm_bindgen::JsCast;
            let _ = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id(&id))
                .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                .map(|ta| {
                    let _ = ta.style().set_property("height", "auto");
                    let _ = ta.style().set_property("height", &format!("{}px", ta.scroll_height()));
                });
        });
        let _ = win.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    }
}
