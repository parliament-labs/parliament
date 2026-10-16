//! Which editor `\e` runs, and how. Pure: the environment and the
//! filesystem are passed in, so every rule here is unit-tested.
//!
//! Order, first match wins:
//!   1. The connection's editor override (connections.json).
//!   2. Settings › Editor, unless it's "Environment".
//!   3. The plane's environment variables, in order — psql's
//!      `PSQL_EDITOR` → `EDITOR` → `VISUAL`, the mysql CLI's
//!      `EDITOR` → `VISUAL`.
//!   4. `vi` on macOS, where it's always installed.
//!
//! Every command comes from noctua's own files or environment; the
//! browser only says which variables its CLI reads.

use std::path::{Path, PathBuf};

use shared::settings::{EditorPreset, EditorSettings, LaunchMode};
use shared::{EditorVar, LineFlagVar};

/// What the browser asked for, plus the user's stored choices.
pub(crate) struct Request<'a> {
    pub(crate) connection_override: Option<&'a str>,
    pub(crate) settings:            &'a EditorSettings,
    pub(crate) env_chain:           &'a [EditorVar],
    pub(crate) line_flag_var:       Option<LineFlagVar>,
}

/// Where the editor came from, so a failure names the setting to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Connection,
    Settings(EditorPreset),
    Env(EditorVar),
    Fallback,
}

impl Source {
    pub(crate) fn describe(self) -> String {
        match self {
            Source::Connection  => "the connection's editor override".into(),
            Source::Settings(p) => format!("Settings › Editor ({})", p.spec().label),
            Source::Env(v)      => format!("${}", v.name()),
            Source::Fallback    => "the default editor (vi)".into(),
        }
    }
}

/// Terminal editors need a terminal window of their own; GUI editors
/// are spawned directly and must be told to wait (`code -w`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Launch {
    Terminal,
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Editor {
    /// Program and its own flags; the file isn't included.
    pub(crate) argv:   Vec<String>,
    /// Arguments that open the file at a line, as tokens holding
    /// `{line}` / `{file}`. Empty: the editor can't, or we don't know how.
    line_args:         Vec<String>,
    pub(crate) launch: Launch,
    pub(crate) source: Source,
}

impl Editor {
    /// The full command to edit `file`, at `line` when given and the
    /// editor supports it.
    pub(crate) fn command(&self, file: &str, line: Option<u32>) -> Vec<String> {
        let mut out = self.argv.clone();
        match line {
            Some(n) if !self.line_args.is_empty() => {
                let n = n.to_string();
                out.extend(self.line_args.iter().map(|t| t.replace("{line}", &n).replace("{file}", file)));
                if !self.line_args.iter().any(|t| t.contains("{file}")) {
                    out.push(file.to_string());
                }
            }
            _ => out.push(file.to_string()),
        }
        out
    }
}

pub(crate) fn resolve(
    req:   &Request,
    env:   impl Fn(&str) -> Option<String>,
    macos: bool,
) -> Result<Editor, String> {
    if let Some(cmd) = req.connection_override.map(str::trim).filter(|s| !s.is_empty()) {
        return from_command(cmd, Source::Connection, LaunchMode::Auto, None, macos);
    }

    let preset = req.settings.preset;
    match preset {
        EditorPreset::Environment => {}
        EditorPreset::Custom => {
            let cmd = req.settings.command.trim();
            if cmd.is_empty() {
                return Err("Settings › Editor is set to Custom, but the command is empty.".into());
            }
            let line_args = split(&req.settings.line_args, Source::Settings(preset), "line template")?;
            return from_command(cmd, Source::Settings(preset), req.settings.launch, Some(line_args), macos);
        }
        _ => {
            let spec = preset.spec();
            return Ok(Editor {
                argv:      spec.argv.iter().map(|s| s.to_string()).collect(),
                line_args: split(spec.line_args, Source::Settings(preset), "line template")?,
                launch:    if spec.in_terminal { Launch::Terminal } else { Launch::Direct },
                source:    Source::Settings(preset),
            });
        }
    }

    for &var in req.env_chain {
        let Some(cmd) = env(var.name()).filter(|s| !s.trim().is_empty()) else { continue };
        // psql passes the line as PSQL_EDITOR_LINENUMBER_ARG + N (one
        // shell word, so a value like "--line " becomes two). Unset,
        // psql uses "+"; we use the editor's own form when we know it.
        let line_args = match req.line_flag_var.and_then(|v| env(v.name())).filter(|s| !s.is_empty()) {
            Some(flag) => {
                let mut t = split(&format!("{flag}{{line}}"), Source::Env(var), line_flag_name(req))?;
                t.push("{file}".into());
                Some(t)
            }
            None if req.line_flag_var.is_some() => {
                let base = shlex::split(&cmd).and_then(|v| v.into_iter().next()).map(|p| basename_of(&p));
                Some(base.and_then(|b| known_line_args(&b)).unwrap_or_else(|| vec!["+{line}".into(), "{file}".into()]))
            }
            None => None,
        };
        return from_command(&cmd, Source::Env(var), LaunchMode::Auto, line_args, macos);
    }

    if macos {
        return Ok(Editor {
            argv:      vec!["vi".into()],
            line_args: vec!["+{line}".into(), "{file}".into()],
            launch:    Launch::Terminal,
            source:    Source::Fallback,
        });
    }
    let vars: Vec<String> = req.env_chain.iter().map(|v| format!("${}", v.name())).collect();
    Err(format!(
        "No editor configured. Choose one in Settings › Editor, or set {} in \
         noctua's environment.",
        if vars.is_empty() { "$EDITOR".into() } else { vars.join(" or ") },
    ))
}

fn line_flag_name(req: &Request) -> &'static str {
    req.line_flag_var.map(|v| v.name()).unwrap_or("line flag")
}

/// Build an editor from a command string. `line_args` None → infer
/// from the program name.
fn from_command(
    cmd:       &str,
    source:    Source,
    launch:    LaunchMode,
    line_args: Option<Vec<String>>,
    macos:     bool,
) -> Result<Editor, String> {
    let argv = split(cmd, source, "command")?;
    let Some(program) = argv.first() else {
        return Err(format!("The editor command from {} is empty.", source.describe()));
    };
    let base = basename_of(program);
    Ok(Editor {
        line_args: line_args.unwrap_or_else(|| known_line_args(&base).unwrap_or_default()),
        launch: match launch {
            LaunchMode::Terminal => Launch::Terminal,
            LaunchMode::Direct   => Launch::Direct,
            LaunchMode::Auto     => classify(&base, macos),
        },
        argv,
        source,
    })
}

fn split(s: &str, source: Source, what: &str) -> Result<Vec<String>, String> {
    shlex::split(s).ok_or_else(|| format!(
        "Couldn't parse the editor {what} from {} (unbalanced quotes?): {s}",
        source.describe(),
    ))
}

/// Lower-cased program name without directory or extension:
/// `/opt/homebrew/bin/nvim` → `nvim`, `vim.basic` → `vim`.
fn basename_of(program: &str) -> String {
    Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(program)
        .to_ascii_lowercase()
}

/// Known GUI editors run directly, known terminal editors get a
/// terminal window. An editor we don't know gets a terminal on macOS,
/// which works for anything — a GUI editor started from a terminal
/// still opens its own window, while a terminal editor run directly
/// would take over noctua's own terminal. Elsewhere a terminal window
/// needs a Custom terminal command in Settings, so unknown editors run
/// directly, as they always did.
fn classify(base: &str, macos: bool) -> Launch {
    match base {
        "code" | "code-insiders" | "cursor" | "codium" | "windsurf"
        | "subl" | "sublime_text" | "zed" | "mvim" | "gvim" | "bbedit"
        | "mate" | "open" | "nova" | "atom" | "pulsar" | "lite-xl"
        | "kate" | "gedit" | "gnome-text-editor" | "xed" | "pluma"
        | "mousepad" | "geany" | "kwrite" | "xdg-open"
        | "notepad" | "notepad++"
        | "idea" | "pycharm" | "webstorm" | "clion" | "goland"
        | "rubymine" | "datagrip" | "fleet" => Launch::Direct,
        "vi" | "vim" | "nvim" | "view" | "nano" | "pico" | "emacs"
        | "emacsclient" | "micro" | "joe" | "jed" | "mg" | "ne"
        | "kak" | "hx" | "helix" => Launch::Terminal,
        _ if macos => Launch::Terminal,
        _ => Launch::Direct,
    }
}

/// How a known editor opens a file at a line.
fn known_line_args(base: &str) -> Option<Vec<String>> {
    let t: &[&str] = match base {
        "vi" | "vim" | "nvim" | "view" | "gvim" | "mvim" | "nano" | "pico"
        | "emacs" | "emacsclient" | "micro" | "joe" | "jed" | "mg" | "ne"
        | "kak" | "bbedit" => &["+{line}", "{file}"],
        "code" | "code-insiders" | "cursor" | "codium" | "windsurf" => &["-g", "{file}:{line}"],
        "subl" | "sublime_text" | "zed" | "hx" | "helix" => &["{file}:{line}"],
        "mate" => &["-l", "{line}", "{file}"],
        _ => return None,
    };
    Some(t.iter().map(|s| s.to_string()).collect())
}

/// Find `program` the way a shell would: as given if it contains a
/// `/`, otherwise in the first `PATH` directory holding an executable
/// of that name.
pub(crate) fn locate(
    program:       &str,
    path_var:      Option<&str>,
    is_executable: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if program.contains('/') {
        let p = PathBuf::from(program);
        return is_executable(&p).then_some(p);
    }
    std::env::split_paths(path_var?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(program))
        .find(|p| is_executable(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::settings::EditorSettings;

    const PSQL: &[EditorVar] = &[EditorVar::PsqlEditor, EditorVar::Editor, EditorVar::Visual];
    const MYSQL: &[EditorVar] = &[EditorVar::Editor, EditorVar::Visual];

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    fn req<'a>(settings: &'a EditorSettings, chain: &'a [EditorVar], psql: bool) -> Request<'a> {
        Request {
            connection_override: None,
            settings,
            env_chain: chain,
            line_flag_var: psql.then_some(LineFlagVar::PsqlEditorLinenumberArg),
        }
    }

    fn cmd(e: &Editor, line: Option<u32>) -> String {
        e.command("/t/f.sql", line).join(" ")
    }

    #[test]
    fn psql_order_is_psql_editor_then_editor_then_visual() {
        let s = EditorSettings::default();
        let all = [("PSQL_EDITOR", "nano"), ("EDITOR", "vim"), ("VISUAL", "code -w")];
        let e = resolve(&req(&s, PSQL, true), env_of(&all), true).unwrap();
        assert_eq!((cmd(&e, None), e.source), ("nano /t/f.sql".into(), Source::Env(EditorVar::PsqlEditor)));
        let e = resolve(&req(&s, PSQL, true), env_of(&all[1..]), true).unwrap();
        assert_eq!(e.source, Source::Env(EditorVar::Editor));
        let e = resolve(&req(&s, PSQL, true), env_of(&all[2..]), true).unwrap();
        assert_eq!((e.source, e.launch), (Source::Env(EditorVar::Visual), Launch::Direct));
    }

    #[test]
    fn mysql_ignores_psql_editor() {
        let s = EditorSettings::default();
        let e = resolve(&req(&s, MYSQL, false), env_of(&[("PSQL_EDITOR", "nano"), ("VISUAL", "vim")]), true).unwrap();
        assert_eq!(e.source, Source::Env(EditorVar::Visual));
    }

    #[test]
    fn blank_variables_are_skipped_and_vi_is_the_macos_fallback() {
        let s = EditorSettings::default();
        let e = resolve(&req(&s, PSQL, true), env_of(&[("PSQL_EDITOR", "  ")]), true).unwrap();
        assert_eq!((cmd(&e, Some(4)), e.launch, e.source), ("vi +4 /t/f.sql".into(), Launch::Terminal, Source::Fallback));
        let err = resolve(&req(&s, PSQL, true), env_of(&[]), false).unwrap_err();
        assert!(err.contains("$PSQL_EDITOR or $EDITOR or $VISUAL"), "{err}");
    }

    #[test]
    fn connection_override_beats_settings_and_environment() {
        let s = EditorSettings { preset: EditorPreset::Nano, ..Default::default() };
        let r = Request { connection_override: Some("mvim -f"), ..req(&s, PSQL, true) };
        let e = resolve(&r, env_of(&[("EDITOR", "vim")]), true).unwrap();
        assert_eq!((cmd(&e, Some(3)), e.launch, e.source), ("mvim -f +3 /t/f.sql".into(), Launch::Direct, Source::Connection));
        // A blank override is no override.
        let r = Request { connection_override: Some("  "), ..req(&s, PSQL, true) };
        assert_eq!(resolve(&r, env_of(&[]), true).unwrap().source, Source::Settings(EditorPreset::Nano));
    }

    #[test]
    fn settings_preset_beats_environment() {
        let s = EditorSettings { preset: EditorPreset::VsCode, ..Default::default() };
        let e = resolve(&req(&s, PSQL, true), env_of(&[("EDITOR", "vim")]), true).unwrap();
        assert_eq!((cmd(&e, Some(7)), e.launch), ("code -w -g /t/f.sql:7".into(), Launch::Direct));
        assert_eq!(cmd(&e, None), "code -w /t/f.sql");
    }

    #[test]
    fn custom_preset_uses_its_template_and_launch_mode() {
        let mut s = EditorSettings { preset: EditorPreset::Custom, ..Default::default() };
        assert!(resolve(&req(&s, PSQL, true), env_of(&[]), true).unwrap_err().contains("command is empty"));
        s.command = "'/Applications/My Editor/bin/ed' --wait".into();
        s.line_args = "--goto {line}".into();
        let e = resolve(&req(&s, PSQL, true), env_of(&[]), true).unwrap();
        // No {file} in the template → the file goes last.
        assert_eq!(e.command("/t/f.sql", Some(2)), ["/Applications/My Editor/bin/ed", "--wait", "--goto", "2", "/t/f.sql"]);
        assert_eq!(e.launch, Launch::Terminal, "unknown programs get a terminal on Auto");
        s.launch = LaunchMode::Direct;
        assert_eq!(resolve(&req(&s, PSQL, true), env_of(&[]), true).unwrap().launch, Launch::Direct);
        s.command = "ed 'unbalanced".into();
        assert!(resolve(&req(&s, PSQL, true), env_of(&[]), true).unwrap_err().contains("unbalanced"));
    }

    #[test]
    fn psql_line_flag_variable() {
        let s = EditorSettings::default();
        // Set: used verbatim, split like a shell word.
        let e = resolve(&req(&s, PSQL, true), env_of(&[("EDITOR", "myed"), ("PSQL_EDITOR_LINENUMBER_ARG", "--line ")]), true).unwrap();
        assert_eq!(cmd(&e, Some(5)), "myed --line 5 /t/f.sql");
        // Unset: the editor's own form when known, else psql's "+".
        let e = resolve(&req(&s, PSQL, true), env_of(&[("EDITOR", "code -w")]), true).unwrap();
        assert_eq!(cmd(&e, Some(5)), "code -w -g /t/f.sql:5");
        let e = resolve(&req(&s, PSQL, true), env_of(&[("EDITOR", "myed")]), true).unwrap();
        assert_eq!(cmd(&e, Some(5)), "myed +5 /t/f.sql");
        // MySQL has no line flag: unknown editors open without a line.
        let e = resolve(&req(&s, MYSQL, false), env_of(&[("EDITOR", "myed")]), true).unwrap();
        assert_eq!(cmd(&e, Some(5)), "myed /t/f.sql");
        let e = resolve(&req(&s, MYSQL, false), env_of(&[("EDITOR", "/usr/bin/vim")]), true).unwrap();
        assert_eq!(cmd(&e, Some(5)), "/usr/bin/vim +5 /t/f.sql");
    }

    #[test]
    fn gui_editors_from_the_environment_run_directly() {
        let s = EditorSettings::default();
        for (ed, launch) in [("code -w", Launch::Direct), ("/opt/homebrew/bin/subl -w", Launch::Direct),
                             ("nvim", Launch::Terminal), ("hx", Launch::Terminal), ("myed", Launch::Terminal)] {
            let e = resolve(&req(&s, PSQL, true), env_of(&[("EDITOR", ed)]), true).unwrap();
            assert_eq!(e.launch, launch, "{ed}");
        }
    }

    #[test]
    fn unknown_editors_run_directly_where_noctua_cant_open_a_terminal() {
        let s = EditorSettings::default();
        for (ed, launch) in [("myed", Launch::Direct), ("notepad++", Launch::Direct), ("vim", Launch::Terminal)] {
            let e = resolve(&req(&s, MYSQL, false), env_of(&[("VISUAL", ed)]), false).unwrap();
            assert_eq!(e.launch, launch, "{ed}");
        }
    }

    #[test]
    fn presets_agree_with_the_known_editor_table() {
        for &p in EditorPreset::ALL {
            let spec = p.spec();
            let Some(program) = spec.argv.first() else { continue };
            let expected: Vec<String> = shlex::split(spec.line_args).unwrap();
            assert_eq!(known_line_args(program), Some(expected), "{program}");
            let launch = if spec.in_terminal { Launch::Terminal } else { Launch::Direct };
            assert_eq!(classify(program, true), launch, "{program}");
            assert_eq!(classify(program, false), launch, "{program}");
        }
    }

    #[test]
    fn locate_searches_path_like_a_shell() {
        let exists = |p: &Path| p == Path::new("/opt/homebrew/bin/nvim") || p == Path::new("/usr/bin/vi");
        assert_eq!(locate("nvim", Some("/usr/bin:/opt/homebrew/bin"), exists), Some("/opt/homebrew/bin/nvim".into()));
        assert_eq!(locate("vi", Some("/usr/bin:/opt/homebrew/bin"), exists), Some("/usr/bin/vi".into()));
        assert_eq!(locate("nope", Some("/usr/bin"), exists), None);
        assert_eq!(locate("/usr/bin/vi", None, exists), Some("/usr/bin/vi".into()));
        assert_eq!(locate("nvim", None, exists), None);
    }
}
