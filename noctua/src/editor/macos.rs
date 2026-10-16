//! macOS terminals driven by AppleScript: Terminal.app and iTerm2.
//! Each opens a window running the edit's script (see `wrapper`),
//! waits for it, then closes the window if it is still ours.
//!
//! Lessons kept from the first version (see the macOS Terminal spawn
//! notes in memory): `launch` a terminal that isn't running, so it
//! opens no default window and restores no old ones; close the window
//! from noctua rather than from inside it, so the terminal never asks
//! whether to terminate running processes.

use std::path::Path;
use std::sync::atomic::Ordering;

use super::{wrapper, Ended, Workspace};

const ITERM2: &str = "com.googlecode.iterm2";

pub(super) async fn terminal_app(argv: &[String], ws: &Workspace) -> Result<Ended, String> {
    let run_sh = wrapper::write(ws, argv)?;
    osascript(&launch_script(&run_sh)).await
        .map_err(|e| format!("Couldn't open a Terminal window: {e}"))?;
    ws.terminal_open.store(true, Ordering::Relaxed);
    let finished = wrapper::wait(ws).await?;
    ws.terminal_open.store(false, Ordering::Relaxed);
    // A window the user closed is gone, and its tty may already belong
    // to another window — only close the one we know is still ours.
    if finished.status != wrapper::WINDOW_CLOSED && !finished.tty.is_empty() {
        // Best effort: if it fails the user closes the window.
        let _ = osascript(&close_script(&finished.tty)).await;
    }
    Ok(Ended::from_script_status(finished.status))
}

pub(super) async fn iterm2(argv: &[String], ws: &Workspace) -> Result<Ended, String> {
    let run_sh = wrapper::write(ws, argv)?;
    let window = osascript(&iterm2_launch_script(&run_sh)).await
        .map_err(|e| format!("Couldn't open an iTerm2 window (is iTerm2 installed?): {e}"))?;
    ws.terminal_open.store(true, Ordering::Relaxed);
    let finished = wrapper::wait(ws).await?;
    ws.terminal_open.store(false, Ordering::Relaxed);
    // iTerm2 window ids aren't reused, but a window the user closed
    // needs nothing from us either.
    if finished.status != wrapper::WINDOW_CLOSED {
        if let Ok(id) = window.parse::<i64>() {
            let _ = osascript(&iterm2_close_script(id)).await;
        }
    }
    Ok(Ended::from_script_status(finished.status))
}

/// Open a window that runs the script. `exec` replaces the window's
/// login shell, so the window's process ends with the edit. The login
/// shell has already read the user's profile, so PATH is theirs.
fn launch_script(run_sh: &Path) -> String {
    format!(
        "tell application \"Terminal\"\n\
         \x20   if not running then\n\
         \x20       launch\n\
         \x20       delay 0.3\n\
         \x20   end if\n\
         \x20   do script \"exec /bin/bash \" & quoted form of {path}\n\
         \x20   activate\n\
         end tell",
        path = applescript_string(&run_sh.to_string_lossy()),
    )
}

/// Close the window whose tab has `tty` — but only when that tab is
/// the window's only one (AppleScript can't close a single tab, and
/// closing the window would take the user's other tabs with it), and
/// only once its process has exited, so Terminal doesn't prompt.
fn close_script(tty: &str) -> String {
    format!(
        "tell application \"Terminal\"\n\
         \x20   repeat with w in windows\n\
         \x20       try\n\
         \x20           repeat with t in tabs of w\n\
         \x20               if tty of t is {tty} then\n\
         \x20                   repeat 20 times\n\
         \x20                       if not busy of t then exit repeat\n\
         \x20                       delay 0.05\n\
         \x20                   end repeat\n\
         \x20                   if (count of tabs of w) is 1 and not busy of t then close w saving no\n\
         \x20                   return\n\
         \x20               end if\n\
         \x20           end repeat\n\
         \x20       end try\n\
         \x20   end repeat\n\
         end tell",
        tty = applescript_string(tty),
    )
}

/// Open an iTerm2 window with the default profile's login shell and
/// have it `exec` the script, as Terminal.app does; return the
/// window's id.
fn iterm2_launch_script(run_sh: &Path) -> String {
    format!(
        "tell application id \"{ITERM2}\"\n\
         \x20   if not running then\n\
         \x20       launch\n\
         \x20       delay 0.5\n\
         \x20   end if\n\
         \x20   set w to (create window with default profile)\n\
         \x20   tell current session of w to write text \"exec /bin/bash \" & quoted form of {path}\n\
         \x20   activate\n\
         \x20   return id of w\n\
         end tell",
        path = applescript_string(&run_sh.to_string_lossy()),
    )
}

/// Close iTerm2 window `id` if it's still open with just our tab —
/// iTerm2 usually closes it itself when the script ends.
fn iterm2_close_script(id: i64) -> String {
    format!(
        "tell application id \"{ITERM2}\"\n\
         \x20   delay 0.2\n\
         \x20   repeat with w in windows\n\
         \x20       if id of w is {id} then\n\
         \x20           if (count of tabs of w) is 1 then close w\n\
         \x20           exit repeat\n\
         \x20       end if\n\
         \x20   end repeat\n\
         end tell",
    )
}

/// `s` as an AppleScript string literal.
pub(super) fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Run an AppleScript; the error is osascript's own message (e.g. the
/// user declined to let noctua control Terminal).
pub(super) async fn osascript(script: &str) -> Result<String, String> {
    let out = tokio::process::Command::new("osascript").arg("-e").arg(script)
        .output().await
        .map_err(|e| format!("couldn't run osascript: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_quotes_the_script_path_twice_over() {
        let s = launch_script(Path::new("/Users/Jo \"JJ\" Doe/.parliament/edit-x/run.sh"));
        assert!(s.contains(r#"do script "exec /bin/bash " & quoted form of "/Users/Jo \"JJ\" Doe/.parliament/edit-x/run.sh""#), "{s}");
        assert!(s.contains("if not running then\n        launch"), "{s}");
    }

    #[test]
    fn iterm2_runs_the_script_in_a_new_window_and_returns_its_id() {
        let s = iterm2_launch_script(Path::new("/Users/Jo Doe/.parliament/edit-x/run.sh"));
        assert!(s.starts_with("tell application id \"com.googlecode.iterm2\"\n"), "{s}");
        assert!(s.contains("set w to (create window with default profile)"), "{s}");
        assert!(s.contains(r#"tell current session of w to write text "exec /bin/bash " & quoted form of "/Users/Jo Doe/.parliament/edit-x/run.sh""#), "{s}");
        assert!(s.contains("return id of w"), "{s}");
        let c = iterm2_close_script(4242);
        assert!(c.contains("if id of w is 4242 then") && c.contains("if (count of tabs of w) is 1 then close w"), "{c}");
    }

    #[test]
    fn close_only_the_windows_single_idle_tab() {
        let s = close_script("/dev/ttys004");
        assert!(s.contains(r#"if tty of t is "/dev/ttys004" then"#), "{s}");
        assert!(s.contains("if (count of tabs of w) is 1 and not busy of t then close w saving no"), "{s}");
    }
}
