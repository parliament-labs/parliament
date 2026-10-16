//! The script a terminal window runs for one edit, and how noctua
//! learns that it finished.
//!
//! A terminal app gives no handle on what it launched, so the window
//! runs `run.sh`, which runs the editor and then writes `done`:
//! the editor's exit status and the window's tty (so noctua can close
//! that window and no other). The write is atomic — `done.tmp`, then
//! `mv` — so noctua never reads half a file. Closing the window sends
//! the script SIGHUP; the trap still writes `done`, with status 129,
//! so the wait ends at once instead of running into the timeout.
//!
//! If noctua stops waiting first (the socket closed), it leaves the
//! workspace in place with an `abandoned` file, so the open editor can
//! still save; when the editor exits, the script deletes the files it
//! knows by name and the then-empty directory.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Workspace;

pub(super) const SCRIPT: &str = "run.sh";
pub(super) const DONE: &str = "done";
pub(super) const ABANDONED: &str = "abandoned";
/// A Custom terminal command's stderr (see `terminal`).
pub(super) const TERMINAL_LOG: &str = "terminal.log";

/// Status the script reports when its window was closed mid-edit.
pub(super) const WINDOW_CLOSED: i32 = 129;
/// Status bash reports when it can't find the program.
pub(super) const NOT_FOUND: i32 = 127;

const POLL: Duration = Duration::from_millis(250);
/// Backstop only: closing the window ends the wait through the trap.
const TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

/// Write the script for `argv` into the workspace (0700) and return its path.
pub(super) fn write(ws: &Workspace, argv: &[String]) -> Result<PathBuf, String> {
    let path = ws.path(SCRIPT);
    let text = script(argv, ws.dir.path())?;
    super::create_private(&path, text.as_bytes(), 0o700)
        .map_err(|e| format!("Couldn't write the editor script: {e}"))?;
    Ok(path)
}

fn script(argv: &[String], dir: &Path) -> Result<String, String> {
    let q = |s: &str| shlex::try_quote(s).map(|c| c.into_owned()).map_err(|e| e.to_string());
    let path = |name: &str| q(&dir.join(name).to_string_lossy());
    let command = argv.iter().map(|a| q(a)).collect::<Result<Vec<_>, _>>()?.join(" ");
    let (done, tmp) = (path(DONE)?, path(&format!("{DONE}.tmp"))?);
    let abandoned = path(ABANDONED)?;
    // Everything an abandoned edit leaves behind: our files plus the
    // backup some editors keep (`name~`).
    let leftovers = [Workspace::FILE, &format!("{}~", Workspace::FILE), SCRIPT, DONE, ABANDONED, TERMINAL_LOG]
        .iter().map(|n| path(n)).collect::<Result<Vec<_>, _>>()?.join(" ");
    let dir = q(&dir.to_string_lossy())?;
    Ok(format!(
        "#!/bin/bash\n\
         # Parliament: one external edit. Runs the editor, then records its\n\
         # exit status and this window's tty for noctua.\n\
         status=1\n\
         tty_name=$(tty 2>/dev/null) || tty_name=\n\
         finish() {{\n\
         \x20   printf '%s\\n%s\\n' \"$status\" \"$tty_name\" > {tmp} && mv -f {tmp} {done}\n\
         \x20   if [ -e {abandoned} ]; then\n\
         \x20       rm -f -- {leftovers}\n\
         \x20       rmdir -- {dir} 2>/dev/null\n\
         \x20   fi\n\
         }}\n\
         trap finish EXIT\n\
         trap 'status={WINDOW_CLOSED}; exit' HUP\n\
         trap 'status=130; exit' INT TERM\n\
         {command}\n\
         status=$?\n"
    ))
}

/// What `done` says: the editor's exit status and the window's tty.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Finished {
    pub(super) status: i32,
    pub(super) tty:    String,
}

fn parse(text: &str) -> Option<Finished> {
    let mut lines = text.lines();
    let status = lines.next()?.trim().parse().ok()?;
    let tty = lines.next().unwrap_or("").trim().to_string();
    Some(Finished { status, tty })
}

/// Wait for the script to write `done`.
pub(super) async fn wait(ws: &Workspace) -> Result<Finished, String> {
    let done = ws.path(DONE);
    let poll = async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(&done).await {
                return parse(&text);
            }
            tokio::time::sleep(POLL).await;
        }
    };
    match tokio::time::timeout(TIMEOUT, poll).await {
        Ok(Some(finished)) => Ok(finished),
        Ok(None) => Err("The editor's terminal reported an unreadable status.".into()),
        Err(_) => Err("Gave up waiting for the editor after 4 hours.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_script(dir: &Path, argv: &[&str]) -> Option<Finished> {
        let _ = std::fs::remove_file(dir.join(DONE));
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let text = script(&argv, dir).unwrap();
        std::process::Command::new("/bin/bash").arg("-c").arg(&text).status().unwrap();
        std::fs::read_to_string(dir.join(DONE)).ok().and_then(|t| parse(&t))
    }

    #[test]
    fn quotes_every_word_including_paths_with_spaces_and_quotes() {
        let argv = ["/Applications/My Editor/ed".to_string(), "+3".into(), "/Users/o'neil/edit/parliament-edit.sql".into()];
        let s = script(&argv, Path::new("/Users/o'neil/edit")).unwrap();
        assert!(s.contains("'/Applications/My Editor/ed' +3 \"/Users/o'neil/edit/parliament-edit.sql\"\n"), "{s}");
        assert!(s.contains("mv -f \"/Users/o'neil/edit/done.tmp\" \"/Users/o'neil/edit/done\""), "{s}");
        assert!(s.contains("rmdir -- \"/Users/o'neil/edit\""), "{s}");
        assert!(s.starts_with("#!/bin/bash\n"));
        assert!(s.contains("trap 'status=129; exit' HUP"));
    }

    #[test]
    fn script_runs_and_reports_status_and_tty() {
        let dir = tempfile::tempdir().unwrap();
        let status = |argv: &[&str]| run_script(dir.path(), argv).unwrap().status;
        assert_eq!(status(&["true"]), 0);
        assert_eq!(status(&["sh", "-c", "exit 3"]), 3);
        assert_eq!(status(&["/no/such/editor"]), NOT_FOUND);
        // Closing the window hangs up the script while the editor runs.
        assert_eq!(status(&["sh", "-c", "kill -HUP $PPID"]), WINDOW_CLOSED);
        assert!(!dir.path().join("done.tmp").exists());
        // Not abandoned: the files stay for noctua to read and remove.
        assert!(dir.path().join(DONE).exists());
    }

    #[test]
    fn an_abandoned_workspace_is_removed_when_the_editor_exits() {
        let parent = tempfile::tempdir().unwrap();
        let ws = parent.path().join("edit-x");
        std::fs::create_dir(&ws).unwrap();
        for name in [Workspace::FILE, "parliament-edit.sql~", SCRIPT, ABANDONED, TERMINAL_LOG] {
            std::fs::write(ws.join(name), "").unwrap();
        }
        run_script(&ws, &["true"]);
        assert!(!ws.exists());
        // Something we don't know about is left alone, and so is the directory.
        std::fs::create_dir(&ws).unwrap();
        std::fs::write(ws.join(ABANDONED), "").unwrap();
        std::fs::write(ws.join("notes.txt"), "keep").unwrap();
        run_script(&ws, &["true"]);
        assert_eq!(std::fs::read_to_string(ws.join("notes.txt")).unwrap(), "keep");
    }

    #[test]
    fn parse_marker() {
        assert_eq!(parse("0\n/dev/ttys004\n"), Some(Finished { status: 0, tty: "/dev/ttys004".into() }));
        assert_eq!(parse("129\n\n"), Some(Finished { status: 129, tty: String::new() }));
        assert_eq!(parse("1"), Some(Finished { status: 1, tty: String::new() }));
        assert_eq!(parse(""), None);
        assert_eq!(parse("x\n/dev/ttys1"), None);
    }
}
