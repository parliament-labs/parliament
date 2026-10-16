//! App-wide user preferences, stored by noctua in
//! `~/.sqlclient/settings.json` and sent to tyto when the WebSocket
//! opens. Today: the external editor (`\e` / `\ef` / `\ev`) and a
//! reserved shell line-editing mode.
//!
//! Every field has a default and every enum reads an unknown value as
//! its default, so an older file — or one written by a newer build —
//! always loads.

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Schema version this build writes. noctua refuses to overwrite a file
/// with a higher version (it would drop settings it doesn't know).
pub const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub version: u32,
    pub editor:  EditorSettings,
    pub shell:   ShellSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            version: SETTINGS_VERSION,
            editor:  EditorSettings::default(),
            shell:   ShellSettings::default(),
        }
    }
}

/// The external editor `\e` / `\ef` / `\ev` open. A connection's own
/// `editor_command` still overrides this.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub preset: EditorPreset,
    /// `Custom` only: the editor command, e.g. `hx` or `gvim -f`.
    pub command: String,
    /// `Custom` only: how to open the file at a line, with `{file}` and
    /// `{line}` tokens (e.g. `+{line} {file}`). Empty: no positioning.
    pub line_args: String,
    /// `Custom` only: whether the editor needs a terminal window.
    pub launch: LaunchMode,
    /// Where terminal editors (vi, nano, …) open.
    pub terminal: TerminalApp,
    /// `TerminalApp::Custom` only: the command that opens a terminal
    /// running `{cmd}`, e.g. `open -na Ghostty --args -e {cmd}`.
    pub terminal_template: String,
}

/// Shell preferences. Reserved: the vi line-editing mode is built with
/// the shell rework; nothing reads this yet.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellSettings {
    pub editing_mode: EditingMode,
}

/// A string-valued enum: serialized as its kebab-case name. Reading is
/// lenient: an unknown name — or a value that isn't a string at all
/// (`null`, a number, an object from some future format) — becomes the
/// default variant instead of failing the whole settings file.
macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $s:literal),+ $(,)? } default $default:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vmeta])* $variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            pub fn parse(s: &str) -> Self {
                match s { $($s => $name::$variant,)+ _ => $name::$default }
            }
        }

        impl Default for $name {
            fn default() -> Self { $name::$default }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct Lenient;
                impl<'de> Visitor<'de> for Lenient {
                    type Value = $name;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str(concat!("a ", stringify!($name), " name"))
                    }
                    fn visit_str<E>(self, v: &str) -> Result<$name, E> { Ok($name::parse(v)) }
                    fn visit_bool<E>(self, _: bool) -> Result<$name, E> { Ok($name::$default) }
                    fn visit_i64<E>(self, _: i64) -> Result<$name, E> { Ok($name::$default) }
                    fn visit_u64<E>(self, _: u64) -> Result<$name, E> { Ok($name::$default) }
                    fn visit_f64<E>(self, _: f64) -> Result<$name, E> { Ok($name::$default) }
                    fn visit_unit<E>(self) -> Result<$name, E> { Ok($name::$default) }
                    fn visit_none<E>(self) -> Result<$name, E> { Ok($name::$default) }
                    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<$name, D::Error> {
                        d.deserialize_any(Lenient)
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<$name, A::Error> {
                        while a.next_element::<IgnoredAny>()?.is_some() {}
                        Ok($name::$default)
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<$name, A::Error> {
                        while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                        Ok($name::$default)
                    }
                }
                d.deserialize_any(Lenient)
            }
        }
    };
}

string_enum! {
    /// Editor choices offered in Settings. `Environment` defers to the
    /// connected database's CLI convention (`$PSQL_EDITOR` / `$EDITOR` /
    /// `$VISUAL` for Postgres, `$EDITOR` / `$VISUAL` for MySQL, then vi).
    EditorPreset {
        Environment => "environment",
        Vi          => "vi",
        Vim         => "vim",
        Nvim        => "nvim",
        Nano        => "nano",
        Emacs       => "emacs",
        Micro       => "micro",
        VsCode      => "vscode",
        Sublime     => "sublime",
        Zed         => "zed",
        MacVim      => "macvim",
        BBEdit      => "bbedit",
        Custom      => "custom",
    } default Environment
}

string_enum! {
    /// How a `Custom` editor is launched. `Auto`: known GUI editors run
    /// directly; anything else gets a terminal window (safe for any
    /// editor — the only cost is an extra window).
    LaunchMode {
        Auto     => "auto",
        Terminal => "terminal",
        Direct   => "direct",
    } default Auto
}

string_enum! {
    /// The terminal app terminal editors open in.
    TerminalApp {
        TerminalApp => "terminal-app",
        ITerm2      => "iterm2",
        Custom      => "custom",
    } default TerminalApp
}

string_enum! {
    /// Shell line-editing keys, like readline's `editing-mode`. Reserved.
    EditingMode {
        Emacs => "emacs",
        Vi    => "vi",
    } default Emacs
}

/// What a preset launches. `argv` is empty for `Environment` and
/// `Custom`, which take their command from elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetSpec {
    pub label:       &'static str,
    pub argv:        &'static [&'static str],
    /// Needs a terminal window (vi, nano, …) rather than running directly.
    pub in_terminal: bool,
    /// Opening at a line, with `{file}` / `{line}` tokens.
    pub line_args:   &'static str,
}

impl EditorPreset {
    pub fn spec(self) -> PresetSpec {
        let (label, argv, in_terminal, line_args): (_, &'static [&'static str], _, _) = match self {
            // argv / in_terminal / line_args unused: the editor named by
            // the environment variable is classified on its own.
            EditorPreset::Environment => ("Environment (the database CLI's $EDITOR convention)", &[], true, ""),
            EditorPreset::Vi          => ("vi",                 &["vi"],             true,  "+{line} {file}"),
            EditorPreset::Vim         => ("Vim",                &["vim"],            true,  "+{line} {file}"),
            EditorPreset::Nvim        => ("Neovim",             &["nvim"],           true,  "+{line} {file}"),
            EditorPreset::Nano        => ("nano",               &["nano"],           true,  "+{line} {file}"),
            EditorPreset::Emacs       => ("Emacs (terminal)",   &["emacs", "-nw"],   true,  "+{line} {file}"),
            EditorPreset::Micro       => ("micro",              &["micro"],          true,  "+{line} {file}"),
            EditorPreset::VsCode      => ("Visual Studio Code", &["code", "-w"],     false, "-g {file}:{line}"),
            EditorPreset::Sublime     => ("Sublime Text",       &["subl", "-w"],     false, "{file}:{line}"),
            EditorPreset::Zed         => ("Zed",                &["zed", "-w"],      false, "{file}:{line}"),
            EditorPreset::MacVim      => ("MacVim",             &["mvim", "-f"],     false, "+{line} {file}"),
            EditorPreset::BBEdit      => ("BBEdit",             &["bbedit", "-w"],   false, "+{line} {file}"),
            EditorPreset::Custom      => ("Custom…",            &[],                 true,  ""),
        };
        PresetSpec { label, argv, in_terminal, line_args }
    }
}

impl TerminalApp {
    pub fn label(self) -> &'static str {
        match self {
            TerminalApp::TerminalApp => "Terminal",
            TerminalApp::ITerm2      => "iTerm2",
            TerminalApp::Custom      => "Custom…",
        }
    }
}

impl LaunchMode {
    pub fn label(self) -> &'static str {
        match self {
            LaunchMode::Auto     => "Auto (GUI editors directly, others in a terminal)",
            LaunchMode::Terminal => "In a terminal window",
            LaunchMode::Direct   => "Directly (GUI editor with a wait flag)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_object_reads_as_defaults() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, AppSettings::default());
        assert_eq!(s.version, SETTINGS_VERSION);
        assert_eq!(s.editor.preset, EditorPreset::Environment);
        assert_eq!(s.editor.terminal, TerminalApp::TerminalApp);
        assert_eq!(s.shell.editing_mode, EditingMode::Emacs);
    }

    #[test]
    fn round_trips_with_readable_names() {
        let mut s = AppSettings::default();
        s.editor.preset = EditorPreset::VsCode;
        s.editor.terminal = TerminalApp::ITerm2;
        s.shell.editing_mode = EditingMode::Vi;
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"preset\":\"vscode\""), "{json}");
        assert!(json.contains("\"terminal\":\"iterm2\""), "{json}");
        assert_eq!(serde_json::from_str::<AppSettings>(&json).unwrap(), s);
    }

    #[test]
    fn unknown_values_read_as_defaults() {
        let s: AppSettings = serde_json::from_str(
            r#"{"editor":{"preset":"kakoune","terminal":"warp","launch":"x"},"shell":{"editing_mode":"ed"}}"#,
        ).unwrap();
        assert_eq!(s.editor.preset, EditorPreset::Environment);
        assert_eq!(s.editor.terminal, TerminalApp::TerminalApp);
        assert_eq!(s.editor.launch, LaunchMode::Auto);
        assert_eq!(s.shell.editing_mode, EditingMode::Emacs);
    }

    #[test]
    fn non_string_values_read_as_defaults() {
        let s: AppSettings = serde_json::from_str(
            r#"{"editor":{"preset":null,"terminal":7,"launch":{"mode":"x"}},"shell":{"editing_mode":["vi"]}}"#,
        ).unwrap();
        assert_eq!(s.editor.preset, EditorPreset::Environment);
        assert_eq!(s.editor.terminal, TerminalApp::TerminalApp);
        assert_eq!(s.editor.launch, LaunchMode::Auto);
        assert_eq!(s.shell.editing_mode, EditingMode::Emacs);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let s: AppSettings = serde_json::from_str(
            r#"{"version":1,"future_section":{"x":1},"editor":{"preset":"vim","later":true}}"#,
        ).unwrap();
        assert_eq!(s.editor.preset, EditorPreset::Vim);
    }

    #[test]
    fn every_preset_has_a_spec() {
        for p in EditorPreset::ALL {
            let spec = p.spec();
            assert!(!spec.label.is_empty());
            let launches_itself = !matches!(p, EditorPreset::Environment | EditorPreset::Custom);
            assert_eq!(!spec.argv.is_empty(), launches_itself, "{p:?}");
        }
    }
}
