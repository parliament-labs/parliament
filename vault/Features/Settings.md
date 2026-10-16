# Settings

App-wide preferences, kept by noctua in `~/.sqlclient/settings.json` next to `connections.json`, edited in the **Settings** dialog (⚙ in the app header). Built 2026-10-01 for the external-editor rework; the first section is **Editor**.

## What's stored

`shared/src/settings.rs` — one schema for noctua and tyto:

```rust
AppSettings {
    version: u32,                    // SETTINGS_VERSION = 1
    editor:  EditorSettings {
        preset:            EditorPreset,  // Environment (default) | Vi | Vim | Nvim | Nano | Emacs | Micro
                                          // | VsCode | Sublime | Zed | MacVim | BBEdit | Custom
        command:           String,        // Custom only, e.g. "code -w"
        line_args:         String,        // Custom only, e.g. "+{line} {file}"
        launch:            LaunchMode,    // Custom only: Auto | Terminal | Direct
        terminal:          TerminalApp,   // TerminalApp (default) | ITerm2 | Custom
        terminal_template: String,        // Custom terminal, e.g. "open -na Ghostty --args -e {cmd}"
    },
    shell:   ShellSettings { editing_mode: EditingMode },  // Emacs | Vi — reserved for the shell rework
}
```

Every field is `#[serde(default)]`; enum fields read leniently (an unknown name, or a non-string such as `null`, becomes the default) so one odd value never loses the whole file. The preset table (`EditorPreset::spec()`: label, argv, terminal-or-not, line template) lives in `shared` so the dialog and noctua describe the same thing.

## File handling (`noctua/src/settings.rs`)

- Loaded once at startup into `AppState`; `GetSettings` answers from memory.
- Writes are atomic (temp file in the same directory → `sync_all` → rename), mode 0600; a `~/.sqlclient` noctua creates is 0700.
- **An unreadable file is never overwritten.** Defaults are used in memory and `load_error` is shown in the dialog; the next save moves the old file to `settings.json.broken-<unix-secs>` first. Non-UTF-8 counts as unreadable.
- A file written by a **newer** Parliament (higher `version`) loads but is never saved over.
- `SaveSettings` holds the settings lock across the write, so two tabs can't leave memory and disk disagreeing.

## Wire messages

| Client → server | Server → client |
|---|---|
| `GetSettings` (sent on every WebSocket open) | `Settings { settings, load_error }` |
| `SaveSettings { settings }` | `SettingsSaved { settings }` / `SettingsError { message }` |
| `TestEditor { env_chain, line_flag_var }` | `EditorTestResult { ok, message }` |

`TestEditor` carries no command: noctua opens the **saved** choice on a three-line sample at line 2 and reports which editor ran, from which setting, and what came back. With the Environment preset there's no connection to take an env chain from, so the dialog sends the PostgreSQL plane's and says so.

## The dialog (`tyto/src/settings.rs`)

Modal (`.app-dialog-wide`); Esc / backdrop / Cancel close it, and the app's global key handler stands aside while it's open. It edits a draft; **Save** sends it whole and closes on `SettingsSaved`; **Save & Test** saves, then tests. Replies for a dialog closed meanwhile are dropped; saving while disconnected says so.

- **Open with** — the preset, with one line saying what it runs (`Runs \`vim +N FILE\` in a terminal window.`; Environment lists each plane's variables). Custom adds Command, Open at line, Launch.
- **Terminal** — where editors that need one open. Custom adds the command template.

A connection's own **Editor override** (connection form) still wins over Settings. How the editor is resolved and launched: CLAUDE.md's `\e` bullet and `noctua/src/editor/`.

## Not built (yet)

The broader settings ideas from the original sketch — editor font / tab width, theme, per-DBMS and per-connection layers, the [[SQL Beautifier]] configuration — are still future work; they'd be new sections of the same file and dialog. Vault-backed storage for an enterprise build is likewise unbuilt.
