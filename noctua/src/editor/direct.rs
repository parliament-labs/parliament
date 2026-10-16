//! Run a GUI editor directly and wait for it. The editor must be told
//! to wait (`code -w`, `mvim -f`); one that returns at once is caught
//! by the caller.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::Ended;

/// Keep this much of the editor's stderr for an error message.
const STDERR_TAIL: usize = 400;

/// Run `argv` and wait for it to exit. Its stderr goes to `stderr_log`
/// — a file, not a pipe, so a process the editor leaves running in
/// the background can't hold the wait open. Returns how it ended and
/// how long it took.
pub(super) async fn run(argv: &[String], stderr_log: &Path) -> Result<(Ended, Duration), String> {
    let log = std::fs::File::create(stderr_log)
        .map_err(|e| format!("Couldn't create the editor's log: {e}"))?;
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    // A session of its own, without a controlling terminal: an editor
    // that wants a terminal fails instead of taking over noctua's.
    #[cfg(unix)]
    // SAFETY: setsid() is async-signal-safe and touches no memory the
    // parent shares; it's all the child runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    #[cfg(not(unix))]
    cmd.kill_on_drop(true);
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start `{}`: {e}", argv[0]))?;
    let mut stop = StopOnDrop(child.id());
    let status = child.wait().await
        .map_err(|e| format!("Lost track of `{}`: {e}", argv[0]))?;
    stop.0 = None;
    let ended = match status.code() {
        Some(code) => Ended::Exited { code, stderr: tail(&std::fs::read(stderr_log).unwrap_or_default()) },
        None => Ended::Killed,
    };
    Ok((ended, started.elapsed()))
}

/// If the task is aborted while the editor runs (the socket closed, so
/// nobody is waiting for the result), ask the editor to quit. SIGTERM
/// rather than SIGKILL, so it can keep a swap file or offer to save.
/// The editor leads its own process group (setsid), so this reaches
/// anything it started too.
struct StopOnDrop(Option<u32>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.and_then(|p| libc::pid_t::try_from(p).ok()) {
            // SAFETY: plain syscall; the pid is our unreaped child, so
            // it can't have been reused.
            unsafe { libc::kill(-pid, libc::SIGTERM); }
        }
    }
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    match text.char_indices().rev().nth(STDERR_TAIL) {
        Some((i, _)) => format!("…{}", &text[i..]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn log() -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("stderr.log");
        (d, p)
    }

    #[tokio::test]
    async fn reports_status_and_stderr() {
        let (_d, log) = log();
        let (ended, _) = run(&argv(&["sh", "-c", "echo oops >&2; exit 2"]), &log).await.unwrap();
        assert_eq!(ended, Ended::Exited { code: 2, stderr: "oops".into() });
        let (ended, _) = run(&argv(&["sh", "-c", "kill -KILL $$"]), &log).await.unwrap();
        assert_eq!(ended, Ended::Killed);
        assert!(run(&argv(&["/no/such/editor"]), &log).await.unwrap_err().contains("Couldn't start"));
    }

    #[tokio::test]
    async fn has_no_controlling_terminal() {
        let (_d, log) = log();
        let (ended, _) = run(&argv(&["sh", "-c", ": < /dev/tty"]), &log).await.unwrap();
        assert!(matches!(ended, Ended::Exited { code, .. } if code != 0), "{ended:?}");
    }

    #[tokio::test]
    async fn a_background_process_left_behind_doesnt_hold_the_wait() {
        let (_d, log) = log();
        let (ended, took) = run(&argv(&["sh", "-c", "sleep 3 & exit 0"]), &log).await.unwrap();
        assert_eq!(ended, Ended::Exited { code: 0, stderr: String::new() });
        assert!(took < Duration::from_secs(2), "{took:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn aborting_asks_the_editor_to_quit() {
        let (d, log) = log();
        let pidfile = d.path().join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 30", pidfile.display());
        let task = tokio::spawn(async move { run(&argv(&["sh", "-c", &script]), &log).await });
        let pid = loop {
            if let Some(p) = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
                break p;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        task.abort();
        let _ = task.await;
        // Gone, or a zombie waiting to be reaped.
        let running = || std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map(|o| {
                let stat = String::from_utf8_lossy(&o.stdout).trim().to_string();
                !stat.is_empty() && !stat.starts_with('Z')
            })
            .unwrap_or(false);
        for _ in 0..100 {
            if !running() { return; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("editor {pid} still running after abort");
    }

    #[test]
    fn stderr_is_trimmed_to_its_tail() {
        let long = "x".repeat(1000) + "end";
        let t = tail(long.as_bytes());
        assert!(t.starts_with('…') && t.ends_with("end") && t.chars().count() == STDERR_TAIL + 2);
    }
}
