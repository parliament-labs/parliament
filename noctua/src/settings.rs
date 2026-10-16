//! App-wide preferences: `~/.sqlclient/settings.json`, next to
//! connections.json. Loaded once at startup into `AppState`; replaced
//! wholesale by `SaveSettings`.
//!
//! The file is user data, so two rules: writes are atomic (a crash
//! mid-write can't leave half a file), and a file that can't be read is
//! never overwritten — defaults are used in memory, the problem is
//! reported, and the next save moves the unreadable file aside first.
//!
//! The file is written 0600. A `~/.sqlclient` directory noctua creates
//! is 0700; an existing one keeps the permissions the user gave it. A
//! symlinked settings.json is replaced by a regular file (the rename
//! doesn't write through links). Without `$HOME` the path is relative
//! to noctua's working directory, same as connections.json.

use std::io::Write;
use std::path::{Path, PathBuf};

use shared::settings::{AppSettings, SETTINGS_VERSION};

pub(crate) fn settings_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".sqlclient").join("settings.json")
}

/// Settings as loaded, plus why the file couldn't be used if it
/// couldn't.
#[derive(Debug, Clone, Default)]
pub(crate) struct LoadedSettings {
    pub(crate) settings:   AppSettings,
    pub(crate) load_error: Option<String>,
}

/// Read the settings file. A missing file is normal (defaults, no
/// error); an unreadable one yields defaults plus `load_error`.
pub(crate) fn load(path: &Path) -> LoadedSettings {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LoadedSettings::default(),
        Err(e) => return unreadable(path, &e.to_string()),
    };
    match serde_json::from_slice::<AppSettings>(&bytes) {
        Ok(settings) if settings.version > SETTINGS_VERSION => LoadedSettings {
            load_error: Some(format!(
                "{} was written by a newer Parliament (settings v{}); this build \
                 shows what it understands but won't save over it.",
                path.display(), settings.version,
            )),
            settings,
        },
        Ok(settings) => LoadedSettings { settings, load_error: None },
        Err(e) => unreadable(path, &e.to_string()),
    }
}

fn unreadable(path: &Path, why: &str) -> LoadedSettings {
    LoadedSettings {
        settings:   AppSettings::default(),
        load_error: Some(format!(
            "{} couldn't be read ({why}); using defaults. Saving will keep the \
             old file as a .broken backup.",
            path.display(),
        )),
    }
}

/// Write `settings` to `path`. Refuses to overwrite a file from a newer
/// schema version; moves an unreadable file aside to
/// `<name>.broken-<unix-secs>` before writing.
pub(crate) fn save(path: &Path, settings: &AppSettings) -> Result<(), String> {
    // Bytes, not a String: a file that isn't UTF-8 is just another
    // unreadable file to move aside, not a reason to refuse saving.
    match std::fs::read(path) {
        Ok(existing) => match serde_json::from_slice::<AppSettings>(&existing) {
            Ok(on_disk) if on_disk.version > SETTINGS_VERSION => {
                return Err(format!(
                    "{} was written by a newer Parliament (settings v{}); not \
                     overwriting it.",
                    path.display(), on_disk.version,
                ));
            }
            Ok(_) => {}
            Err(_) => {
                let backup = broken_backup_path(path);
                std::fs::rename(path, &backup).map_err(|e| format!(
                    "Couldn't move the unreadable {} aside: {e}", path.display(),
                ))?;
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Couldn't read {}: {e}", path.display())),
    }
    let mut to_write = settings.clone();
    to_write.version = SETTINGS_VERSION;
    let json = serde_json::to_string_pretty(&to_write)
        .map_err(|e| format!("Couldn't encode settings: {e}"))?;
    write_atomic_0600(path, json.as_bytes())
        .map_err(|e| format!("Couldn't write {}: {e}", path.display()))
}

fn broken_backup_path(path: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{name}.broken-{secs}"))
}

/// Replace `path` with `bytes` atomically: write a temp file in the
/// same directory (created 0600), flush it to disk, then rename over
/// the target. A missing directory is created 0700; an existing one is
/// left as the user set it.
pub(crate) fn write_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::other("path has no parent directory"))?;
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::settings::EditorPreset;

    fn tmp_settings_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("settings.json");
        (dir, path)
    }

    fn vim() -> AppSettings {
        let mut s = AppSettings::default();
        s.editor.preset = EditorPreset::Vim;
        s
    }

    #[test]
    fn missing_file_is_defaults_without_error() {
        let (_d, path) = tmp_settings_path();
        let loaded = load(&path);
        assert_eq!(loaded.settings, AppSettings::default());
        assert!(loaded.load_error.is_none());
    }

    #[test]
    fn saves_atomically_with_private_permissions_and_reloads() {
        let (_d, path) = tmp_settings_path();
        save(&path, &vim()).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.settings.editor.preset, EditorPreset::Vim);
        assert!(loaded.load_error.is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let dir_mode = std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
        }
    }

    #[test]
    fn unreadable_file_is_reported_and_kept_as_backup_on_save() {
        let (_d, path) = tmp_settings_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.settings, AppSettings::default());
        assert!(loaded.load_error.unwrap().contains("couldn't be read"));
        // Loading never touches the file.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");

        save(&path, &vim()).unwrap();
        let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.broken-"))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read_to_string(backups[0].path()).unwrap(), "{ not json");
        assert_eq!(load(&path).settings.editor.preset, EditorPreset::Vim);
    }

    #[test]
    fn non_utf8_file_is_backed_up_on_save_too() {
        let (_d, path) = tmp_settings_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x7b]).unwrap();
        assert!(load(&path).load_error.is_some());
        save(&path, &vim()).unwrap();
        assert_eq!(load(&path).settings.editor.preset, EditorPreset::Vim);
        let backups = std::fs::read_dir(path.parent().unwrap()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.broken-"))
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn newer_version_loads_but_is_not_overwritten() {
        let (_d, path) = tmp_settings_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let newer = format!(r#"{{"version":{},"editor":{{"preset":"nano"}}}}"#, SETTINGS_VERSION + 1);
        std::fs::write(&path, &newer).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.settings.editor.preset, EditorPreset::Nano);
        assert!(loaded.load_error.unwrap().contains("newer Parliament"));
        assert!(save(&path, &vim()).unwrap_err().contains("not overwriting"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
    }

    #[test]
    fn save_stamps_the_current_version() {
        let (_d, path) = tmp_settings_path();
        let mut s = vim();
        s.version = 0;
        save(&path, &s).unwrap();
        assert_eq!(load(&path).settings.version, SETTINGS_VERSION);
    }
}
