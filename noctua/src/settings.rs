//! App-wide preferences: `~/.sqlclient/settings.json`, next to
//! connections.json. Loaded once at startup into `AppState`; replaced
//! wholesale by `SaveSettings`.
//!
//! It's saved as connections.json is (`userfile`): atomically, 0600, an
//! unreadable file moved aside first, a newer Parliament's file never
//! written over. A file that can't be read loads as the defaults, the
//! problem reported (`load_error`). Without `$HOME` the path is relative
//! to noctua's working directory, same as connections.json.

use std::path::{Path, PathBuf};

use shared::settings::{AppSettings, SETTINGS_VERSION};

use crate::userfile;

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
                userfile::shown(path), settings.version,
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
            userfile::shown(path),
        )),
    }
}

/// Write `settings` to `path` (`userfile::save`): refused over a newer
/// schema version's file; an unreadable one moved aside first.
pub(crate) fn save(path: &Path, settings: &AppSettings) -> Result<(), String> {
    let mut to_write = settings.clone();
    to_write.version = SETTINGS_VERSION;
    let json = serde_json::to_string_pretty(&to_write)
        .map_err(|e| format!("Couldn't encode settings: {e}"))?;
    userfile::save(path, json.as_bytes(), "settings", |bytes| match serde_json::from_slice::<AppSettings>(bytes) {
        Ok(on_disk) if on_disk.version > SETTINGS_VERSION => userfile::OnDisk::Newer(on_disk.version),
        Ok(_) => userfile::OnDisk::Readable,
        Err(_) => userfile::OnDisk::Unreadable,
    }).map(|_| ())
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
