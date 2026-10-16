//! Open a terminal window for an editor that needs one, in the
//! terminal Settings › Editor names: Terminal.app (the default) or
//! iTerm2 on macOS, or a Custom command for anything else — Ghostty,
//! kitty, WezTerm, Alacritty, or a Linux terminal.

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::Ordering;

use shared::settings::{EditorSettings, TerminalApp};

use super::{wrapper, Ended, Workspace};

pub(super) async fn run(settings: &EditorSettings, argv: &[String], ws: &Workspace) -> Result<Ended, String> {
    match settings.terminal {
        #[cfg(target_os = "macos")]
        TerminalApp::TerminalApp => super::macos::terminal_app(argv, ws).await,
        #[cfg(target_os = "macos")]
        TerminalApp::ITerm2 => super::macos::iterm2(argv, ws).await,
        #[cfg(not(target_os = "macos"))]
        other @ (TerminalApp::TerminalApp | TerminalApp::ITerm2) => Err(format!(
            "`{}` needs a terminal window, and {} is macOS-only — set Terminal \
             to Custom in Settings › Editor (e.g. `xterm -e {{cmd}}`).",
            argv[0], other.label(),
        )),
        TerminalApp::Custom => custom(&settings.terminal_template, argv, ws).await,
    }
}

/// Run the user's terminal command with `{cmd}` replaced by the edit's
/// script. The command is split like a shell would, but run without
/// one. Some launchers return at once (`open -na Ghostty`), others
/// last as long as their window (`kitty`); either way the script's
/// `done` file says when the editor finished. A launcher that fails
/// before the script ran is reported.
async fn custom(template: &str, argv: &[String], ws: &Workspace) -> Result<Ended, String> {
    let run_sh = wrapper::write(ws, argv)?;
    let launcher = custom_argv(template, &run_sh)?;
    let log = ws.path(wrapper::TERMINAL_LOG);
    let log_file = std::fs::File::create(&log)
        .map_err(|e| format!("Couldn't create the terminal's log: {e}"))?;
    let mut child = tokio::process::Command::new(&launcher[0])
        .args(&launcher[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .spawn()
        .map_err(|e| format!("Couldn't start the terminal command `{}`: {e}", launcher[0]))?;
    ws.terminal_open.store(true, Ordering::Relaxed);
    let finished = tokio::select! {
        finished = wrapper::wait(ws) => finished?,
        status = child.wait() => {
            let failed = status.as_ref().map(|s| !s.success()).unwrap_or(true);
            if failed && !ws.path(wrapper::DONE).exists() {
                ws.terminal_open.store(false, Ordering::Relaxed);
                let said = std::fs::read_to_string(&log).unwrap_or_default();
                let code = status.ok().and_then(|s| s.code()).map(|c| c.to_string()).unwrap_or_else(|| "a signal".into());
                return Err(format!(
                    "The terminal command `{}` failed (exit {code}){}",
                    launcher[0],
                    match said.trim() { "" => String::new(), s => format!(": {s}") },
                ));
            }
            wrapper::wait(ws).await?
        }
    };
    ws.terminal_open.store(false, Ordering::Relaxed);
    Ok(Ended::from_script_status(finished.status))
}

/// The terminal command with `{cmd}` replaced by the script's path —
/// inside any word, so `--command={cmd}` works. Without `{cmd}` the
/// path goes last.
fn custom_argv(template: &str, script: &Path) -> Result<Vec<String>, String> {
    let words = shlex::split(template).ok_or_else(|| format!(
        "Couldn't parse the Custom terminal command (unbalanced quotes?): {template}",
    ))?;
    if words.is_empty() {
        return Err(
            "Settings › Editor › Terminal is Custom, but its command is empty \
             (e.g. `open -na Ghostty --args -e {cmd}`)."
                .into(),
        );
    }
    let script = script.to_string_lossy();
    let mut out: Vec<String> = words.iter().map(|w| w.replace("{cmd}", &script)).collect();
    if !words.iter().any(|w| w.contains("{cmd}")) {
        out.push(script.into_owned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_command_takes_the_script_in_place_of_cmd() {
        let script = Path::new("/Users/Jo Doe/.parliament/edit-x/run.sh");
        assert_eq!(
            custom_argv("open -na Ghostty --args -e {cmd}", script).unwrap(),
            ["open", "-na", "Ghostty", "--args", "-e", "/Users/Jo Doe/.parliament/edit-x/run.sh"],
        );
        assert_eq!(
            custom_argv("wezterm start --cwd '/tmp/a b' -- {cmd}", script).unwrap(),
            ["wezterm", "start", "--cwd", "/tmp/a b", "--", "/Users/Jo Doe/.parliament/edit-x/run.sh"],
        );
        assert_eq!(custom_argv("term --command={cmd}", script).unwrap()[1], "--command=/Users/Jo Doe/.parliament/edit-x/run.sh");
        assert_eq!(custom_argv("kitty", script).unwrap(), ["kitty", "/Users/Jo Doe/.parliament/edit-x/run.sh"]);
        assert!(custom_argv("  ", script).unwrap_err().contains("command is empty"));
        assert!(custom_argv("term 'x", script).unwrap_err().contains("unbalanced"));
    }

    fn settings(template: &str) -> EditorSettings {
        EditorSettings { terminal: TerminalApp::Custom, terminal_template: template.into(), ..Default::default() }
    }

    #[tokio::test]
    async fn custom_terminal_runs_the_script_and_reports_how_the_editor_ended() {
        let parent = tempfile::tempdir().unwrap();
        // bash stands in for a terminal: it runs the script, no window.
        let ws = Workspace::create_in(parent.path(), "select 1").unwrap();
        let file = ws.file().to_string_lossy().into_owned();
        let editor = ["sh".to_string(), "-c".into(), "echo 'select 2' > \"$0\"".into(), file];
        let ended = run(&settings("/bin/bash {cmd}"), &editor, &ws).await.unwrap();
        assert_eq!(ended, Ended::Exited { code: 0, stderr: String::new() });
        assert_eq!(ws.read().unwrap(), "select 2\n");
        assert!(!ws.terminal_open.load(Ordering::Relaxed));

        let ws = Workspace::create_in(parent.path(), "").unwrap();
        let ended = run(&settings("/bin/bash {cmd}"), &["sh".into(), "-c".into(), "exit 4".into()], &ws).await.unwrap();
        assert_eq!(ended, Ended::Exited { code: 4, stderr: String::new() });
    }

    #[tokio::test]
    async fn a_launcher_that_returns_at_once_still_waits_for_the_editor() {
        // `open -na Ghostty …` exits as soon as Ghostty has the request.
        let parent = tempfile::tempdir().unwrap();
        let ws = Workspace::create_in(parent.path(), "").unwrap();
        let template = r#"sh -c '(sleep 0.3; /bin/bash "$0") > /dev/null 2>&1 &' {cmd}"#;
        let ended = run(&settings(template), &["sh".into(), "-c".into(), "exit 5".into()], &ws).await.unwrap();
        assert_eq!(ended, Ended::Exited { code: 5, stderr: String::new() });
    }

    #[tokio::test]
    async fn a_custom_terminal_that_fails_to_start_the_script_is_reported() {
        let parent = tempfile::tempdir().unwrap();
        let ws = Workspace::create_in(parent.path(), "").unwrap();
        let err = run(&settings("sh -c 'echo no such app >&2; exit 1' {cmd}"), &["true".into()], &ws).await.unwrap_err();
        assert!(err.contains("failed (exit 1): no such app"), "{err}");
        assert!(!ws.terminal_open.load(Ordering::Relaxed));
        let ws = Workspace::create_in(parent.path(), "").unwrap();
        let err = run(&settings("/no/such/terminal {cmd}"), &["true".into()], &ws).await.unwrap_err();
        assert!(err.contains("Couldn't start the terminal command"), "{err}");
    }
}
