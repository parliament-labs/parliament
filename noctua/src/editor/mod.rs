//! External editor round-trip for `\e` / `\ef` / `\ev`: write the
//! buffer to a private temp file, open the user's editor on it, wait
//! for the editor to exit, read the file back.
//!
//! `resolve` picks the editor and how to launch it. Known GUI editors
//! run directly with their wait flag (`direct`); terminal editors —
//! and, on macOS, any editor we don't recognise — get a new window in
//! the terminal Settings names (`terminal`, `macos`), running a small
//! script that reports back when the editor exits (`wrapper`). Either
//! way the app that was in front comes back afterwards (`focus`).
//!
//! Like psql, a non-zero exit discards the edit: the input is left as
//! it was. That includes `:cq` in vi and closing the editor's window.

mod direct;
mod focus;
#[cfg(target_os = "macos")]
mod macos;
mod resolve;
mod terminal;
mod wrapper;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use shared::settings::EditorSettings;

pub(crate) use resolve::Request;
use resolve::{Editor, Launch};

/// How an edit ended, when nothing went wrong.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The editor exited cleanly; here is the file.
    Saved(String),
    /// The editor exited with an error or its window was closed; the
    /// edit is thrown away, and this says why.
    Discarded(String),
}

/// Round-trip `content` through the editor `req` resolves to (see
/// `resolve` for the order), opened at `line` when given.
pub(crate) async fn edit(req: &Request<'_>, content: &str, line: Option<u32>) -> Result<Outcome, String> {
    let mut editor = resolve::resolve(req, |k| std::env::var(k).ok(), cfg!(target_os = "macos"))?;
    locate_program(&mut editor, std::env::var("PATH").ok().as_deref(), is_executable)?;
    let ws = Workspace::create(content)?;
    let argv = editor.command(&ws.file().to_string_lossy(), line);

    let front = focus::frontmost().await;
    let ended = launch(&editor, req.settings, &argv, &ws).await;
    if let Some(app) = front {
        focus::restore(&app).await;
    }
    let (ended, elapsed) = ended?;
    conclude(&argv[0], ended, elapsed, || ws.read(), content)
}

/// What Settings › Save & Test opens: three lines, the cursor meant
/// to land on the second.
const TEST_SAMPLE: &str = "-- Parliament editor test.\n\
                           -- The cursor should be on this line.\n\
                           SELECT 1;\n";

/// Open the editor `req` resolves to on [`TEST_SAMPLE`] and describe
/// how it went: which editor, from which setting, and what came back.
pub(crate) async fn test(req: &Request<'_>) -> Result<String, String> {
    let editor = resolve::resolve(req, |k| std::env::var(k).ok(), cfg!(target_os = "macos"))?;
    let which = format!("`{}` (from {})", editor.argv.join(" "), editor.source.describe());
    match edit(req, TEST_SAMPLE, Some(2)).await {
        Ok(Outcome::Saved(text)) if text == TEST_SAMPLE => Ok(format!("{which} opened and returned without changes.")),
        Ok(Outcome::Saved(_)) => Ok(format!("{which} opened and returned your changes.")),
        Ok(Outcome::Discarded(why)) => Err(format!("{which} opened, but: {why}")),
        Err(why) => Err(format!("{which}: {why}")),
    }
}

async fn launch(
    editor:   &Editor,
    settings: &EditorSettings,
    argv:     &[String],
    ws:       &Workspace,
) -> Result<(Ended, Option<Duration>), String> {
    match editor.launch {
        Launch::Direct => direct::run(argv, &ws.path("stderr.log")).await.map(|(ended, took)| (ended, Some(took))),
        Launch::Terminal => terminal::run(settings, argv, ws).await.map(|ended| (ended, None)),
    }
}

/// How the editor ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    Exited { code: i32, stderr: String },
    /// Killed by a signal (direct launches).
    Killed,
    /// Its terminal window was closed mid-edit.
    WindowClosed,
    /// The terminal's shell couldn't find the program.
    NotFound,
}

impl Ended {
    fn from_script_status(status: i32) -> Self {
        match status {
            wrapper::WINDOW_CLOSED => Ended::WindowClosed,
            wrapper::NOT_FOUND     => Ended::NotFound,
            code                   => Ended::Exited { code, stderr: String::new() },
        }
    }
}

/// An editor that exits within this long without changing anything
/// almost certainly didn't wait for the user.
const RETURNED_AT_ONCE: Duration = Duration::from_secs(2);

/// Turn how the editor ended into the result the shell sees.
fn conclude(
    program: &str,
    ended:   Ended,
    elapsed: Option<Duration>,
    read:    impl FnOnce() -> Result<String, String>,
    before:  &str,
) -> Result<Outcome, String> {
    let name = Path::new(program).file_name().map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string());
    match ended {
        Ended::Exited { code: 0, .. } => {
            let text = read()?;
            if text == before && elapsed.is_some_and(|t| t < RETURNED_AT_ONCE) {
                return Err(format!(
                    "`{name}` returned right away instead of waiting for you to \
                     finish — add its wait flag (`code -w`, `subl -w`, `zed -w`, \
                     `bbedit -w`, `mvim -f`, `open -W`).",
                ));
            }
            Ok(Outcome::Saved(text))
        }
        Ended::Exited { code, stderr } => {
            let mut msg = format!("`{name}` exited with status {code}; edit discarded.");
            if !stderr.is_empty() {
                msg.push_str(&format!(" It said: {stderr}"));
            }
            if cfg!(target_os = "macos") && name == "vi" {
                msg.push_str(
                    " (macOS vi exits with status 1 after any error during the \
                     session, even when you saved — Vim in Settings › Editor \
                     doesn't.)",
                );
            }
            Ok(Outcome::Discarded(msg))
        }
        Ended::Killed => Ok(Outcome::Discarded(format!("`{name}` was killed; edit discarded."))),
        Ended::WindowClosed => Ok(Outcome::Discarded(
            "The editor's window was closed before the editor exited; edit discarded.".into(),
        )),
        Ended::NotFound => Err(format!(
            "`{name}` exited with status 127 — most likely the terminal's shell couldn't find it.",
        )),
    }
}

/// Point argv[0] at the binary on noctua's PATH, so a launch through a
/// terminal's login shell runs the same program. A terminal editor
/// that isn't on noctua's PATH is left by name — the login shell may
/// still find it, and says so in the window if not.
fn locate_program(
    editor:        &mut Editor,
    path_var:      Option<&str>,
    is_executable: impl Fn(&Path) -> bool,
) -> Result<(), String> {
    let Some(program) = editor.argv.first() else {
        return Err(format!("The editor command from {} is empty.", editor.source.describe()));
    };
    match resolve::locate(program, path_var, is_executable) {
        Some(found) => editor.argv[0] = found.to_string_lossy().into_owned(),
        None if editor.launch == Launch::Terminal => {}
        None => return Err(format!(
            "Editor `{program}` (from {}) isn't on noctua's PATH.", editor.source.describe(),
        )),
    }
    Ok(())
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// One edit's private directory under `~/.parliament`: the file being
/// edited and, for terminal launches, the script and its `done`
/// marker. Removed with everything in it when dropped — unless a
/// terminal editor is still open on it (the socket closed and the task
/// was aborted): then the files stay so the editor can still save, and
/// the script removes them when the editor exits (see `wrapper`).
struct Workspace {
    dir: tempfile::TempDir,
    /// A terminal editor is running on the file.
    terminal_open: AtomicBool,
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let still_open = self.terminal_open.load(Ordering::Relaxed) && !self.path(wrapper::DONE).exists();
        if still_open && std::fs::write(self.path(wrapper::ABANDONED), b"").is_ok() {
            self.dir.disable_cleanup(true);
        }
    }
}

impl Workspace {
    const FILE: &'static str = "parliament-edit.sql";

    fn create(content: &str) -> Result<Self, String> {
        let home = std::env::var("HOME").map_err(|_| "HOME isn't set".to_string())?;
        Self::create_in(&PathBuf::from(home).join(".parliament"), content)
    }

    fn create_in(parent: &Path, content: &str) -> Result<Self, String> {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Couldn't create {}: {e}", parent.display()))?;
        }
        let mut builder = tempfile::Builder::new();
        builder.prefix("edit-");
        #[cfg(unix)]
        builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700));
        let dir = builder.tempdir_in(parent)
            .map_err(|e| format!("Couldn't create a temp directory in {}: {e}", parent.display()))?;
        let ws = Workspace { dir, terminal_open: AtomicBool::new(false) };
        create_private(&ws.file(), content.as_bytes(), 0o600)
            .map_err(|e| format!("Couldn't write the file to edit: {e}"))?;
        Ok(ws)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn file(&self) -> PathBuf {
        self.path(Self::FILE)
    }

    fn read(&self) -> Result<String, String> {
        std::fs::read_to_string(self.file())
            .map_err(|e| format!("Couldn't read the edited file back: {e}"))
    }
}

/// Create `path` (it must not exist) with `mode`, holding `bytes`.
fn create_private(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, mode);
    #[cfg(not(unix))]
    let _ = mode;
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::settings::{EditorPreset, EditorSettings};

    fn preset(p: EditorPreset) -> Editor {
        let settings = EditorSettings { preset: p, ..Default::default() };
        let req = Request { connection_override: None, settings: &settings, env_chain: &[], line_flag_var: None };
        resolve::resolve(&req, |_| None, true).unwrap()
    }

    #[test]
    fn locate_program_makes_argv0_absolute() {
        let mut e = preset(EditorPreset::VsCode);
        locate_program(&mut e, Some("/usr/bin:/usr/local/bin"), |p| p == Path::new("/usr/local/bin/code")).unwrap();
        assert_eq!(e.argv, ["/usr/local/bin/code", "-w"]);
    }

    #[test]
    fn a_missing_gui_editor_fails_early_but_a_terminal_one_is_left_to_the_shell() {
        let mut e = preset(EditorPreset::VsCode);
        let err = locate_program(&mut e, Some("/usr/bin"), |_| false).unwrap_err();
        assert!(err.contains("`code`") && err.contains("Visual Studio Code"), "{err}");
        let mut e = preset(EditorPreset::Nvim);
        locate_program(&mut e, Some("/usr/bin"), |_| false).unwrap();
        assert_eq!(e.argv, ["nvim"]);
    }

    fn saved(text: &str) -> impl FnOnce() -> Result<String, String> + '_ {
        move || Ok(text.to_string())
    }

    #[test]
    fn a_clean_exit_returns_the_file() {
        let r = conclude("/usr/bin/vim", Ended::Exited { code: 0, stderr: String::new() }, None, saved("b"), "a");
        assert_eq!(r, Ok(Outcome::Saved("b".into())));
        // Unchanged is fine too: the user looked and quit.
        let r = conclude("/usr/bin/vim", Ended::Exited { code: 0, stderr: String::new() }, None, saved("a"), "a");
        assert_eq!(r, Ok(Outcome::Saved("a".into())));
    }

    #[test]
    fn a_gui_editor_that_returns_at_once_unchanged_lacks_its_wait_flag() {
        let quick = Some(Duration::from_millis(300));
        let r = conclude("/usr/local/bin/code", Ended::Exited { code: 0, stderr: String::new() }, quick, saved("a"), "a");
        assert!(r.unwrap_err().contains("`code` returned right away"));
        // Changed, or slow: the user really edited.
        let r = conclude("code", Ended::Exited { code: 0, stderr: String::new() }, quick, saved("b"), "a");
        assert_eq!(r, Ok(Outcome::Saved("b".into())));
        let r = conclude("code", Ended::Exited { code: 0, stderr: String::new() }, Some(Duration::from_secs(9)), saved("a"), "a");
        assert_eq!(r, Ok(Outcome::Saved("a".into())));
    }

    #[test]
    fn non_zero_exits_and_closed_windows_discard_the_edit() {
        let never_read = || -> Result<String, String> { panic!("a discarded edit isn't read") };
        let Ok(Outcome::Discarded(m)) = conclude("/usr/bin/vim", Ended::Exited { code: 1, stderr: "E37".into() }, None, never_read, "")
        else { panic!() };
        assert!(m.contains("`vim` exited with status 1; edit discarded. It said: E37"), "{m}");
        assert!(!m.contains("macOS vi"));
        let Ok(Outcome::Discarded(m)) = conclude("/usr/bin/vi", Ended::Exited { code: 1, stderr: String::new() }, None, never_read, "")
        else { panic!() };
        assert_eq!(m.contains("macOS vi"), cfg!(target_os = "macos"), "{m}");
        assert!(matches!(conclude("vi", Ended::WindowClosed, None, never_read, ""), Ok(Outcome::Discarded(m)) if m.contains("window was closed")));
        assert!(matches!(conclude("vi", Ended::Killed, None, never_read, ""), Ok(Outcome::Discarded(_))));
        assert!(conclude("nvim", Ended::NotFound, None, never_read, "").unwrap_err().contains("`nvim` exited with status 127"));
    }

    #[test]
    fn workspace_is_private_and_removed_on_drop() {
        let parent = tempfile::tempdir().unwrap();
        let ws = Workspace::create_in(&parent.path().join("p"), "select 1").unwrap();
        assert_eq!(ws.read().unwrap(), "select 1");
        assert_eq!(ws.file().file_name().unwrap(), "parliament-edit.sql");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&ws.file()), 0o600);
            assert_eq!(mode(ws.dir.path()), 0o700);
        }
        let dir = ws.dir.path().to_path_buf();
        drop(ws);
        assert!(!dir.exists());
    }

    #[test]
    fn workspace_with_a_terminal_editor_still_open_is_handed_to_the_script() {
        let parent = tempfile::tempdir().unwrap();
        let ws = Workspace::create_in(parent.path(), "select 1").unwrap();
        let dir = ws.dir.path().to_path_buf();
        ws.terminal_open.store(true, Ordering::Relaxed);
        drop(ws);
        assert!(dir.join(wrapper::ABANDONED).exists());
        assert_eq!(std::fs::read_to_string(dir.join(Workspace::FILE)).unwrap(), "select 1");
        // Once the editor has finished, nothing is left waiting: clean up as usual.
        let ws = Workspace::create_in(parent.path(), "").unwrap();
        let dir = ws.dir.path().to_path_buf();
        ws.terminal_open.store(true, Ordering::Relaxed);
        std::fs::write(ws.path(wrapper::DONE), "0\n").unwrap();
        drop(ws);
        assert!(!dir.exists());
    }
}
