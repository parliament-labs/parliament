//! The files noctua keeps for the user in `~/.sqlclient` —
//! connections.json (which holds passwords) and settings.json — and the
//! rules both follow, since each is the user's data:
//!
//! - Written atomically and 0600: a crash mid-write can't leave half a
//!   file, and no other account can read it. A `~/.sqlclient` noctua
//!   creates is 0700; an existing one keeps the permissions the user
//!   gave it. A symlink is replaced by a regular file (the rename
//!   doesn't write through links).
//! - A file this build can't read is never written over: the save moves
//!   it aside to `<name>.broken-<unix-secs>` first.
//! - A file a newer Parliament wrote is never written over at all — this
//!   build would drop what it doesn't understand.

use std::io::Write;
use std::path::{Path, PathBuf};

/// `path` as the user knows it: `~/.sqlclient/connections.json`, not the
/// home directory spelled out.
pub(crate) fn shown(path: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if home.components().count() > 1 && path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap_or(path).display())
        }
        _ => path.display().to_string(),
    }
}

/// What a save finds at the path, as the file's own reader judges it.
pub(crate) enum OnDisk {
    /// A file this build reads: the save replaces it.
    Readable,
    /// Written by a newer Parliament, at this schema version: left alone.
    Newer(u32),
    /// Not a file this build can read: moved aside, then saved over.
    Unreadable,
}

/// Save `bytes` to `path` under the rules above, `judge` reading what's
/// there now (`what` names the file's schema in the refusal: `settings`).
/// Returns where an unreadable file was moved, if one was.
pub(crate) fn save(path: &Path, bytes: &[u8], what: &str, judge: impl Fn(&[u8]) -> OnDisk) -> Result<Option<PathBuf>, String> {
    let mut moved = None;
    // Bytes, not a String: a file that isn't UTF-8 is just another
    // unreadable file to move aside, not a reason to refuse saving.
    match std::fs::read(path) {
        Ok(existing) => match judge(&existing) {
            OnDisk::Readable => {}
            OnDisk::Newer(version) => return Err(format!(
                "{} was written by a newer Parliament ({what} v{version}); not overwriting it.",
                shown(path),
            )),
            OnDisk::Unreadable => {
                let backup = broken_backup_path(path);
                std::fs::rename(path, &backup).map_err(|e| format!(
                    "Couldn't move the unreadable {} aside: {e}", shown(path),
                ))?;
                moved = Some(backup);
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Couldn't read {}: {e}", shown(path))),
    }
    write_atomic_0600(path, bytes).map_err(|e| match &moved {
        Some(backup) => format!("Couldn't write {} ({e}); the unreadable one is kept as {}.", shown(path), shown(backup)),
        None => format!("Couldn't write {}: {e}", shown(path)),
    })?;
    Ok(moved)
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
fn write_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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

    #[test]
    fn a_path_under_home_is_shown_from_tilde() {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        assert_eq!(shown(&home.join(".sqlclient").join("connections.json")), "~/.sqlclient/connections.json");
        assert_eq!(shown(Path::new("/etc/parliament/connections.json")), "/etc/parliament/connections.json");
    }
}
