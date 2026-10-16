use shared::DbType;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct HistoryEntry { pub(crate) sql: String, pub(crate) ts: f64 }

pub(crate) const HISTORY_CAP: usize = 2000;

pub(crate) fn make_history_key(db_type: DbType, host: &str, port: u16, database: &str) -> String {
    let t = match db_type { DbType::MySql => "mysql", DbType::Postgres => "postgres" };
    format!("history:{}:{}:{}:{}", t, host, port, database)
}

/// Whether `text` may carry a secret, and so must never be written to
/// the persisted history (localStorage outlives the session and anyone
/// with the browser can read it). Errs towards not keeping: anything
/// with a `\set` or `\setenv` (the value can be anything), and anything
/// mentioning a password or a key — `PASSWORD '…'`, MySQL's `IDENTIFIED
/// BY`, `password=` in a connection string, secrets, credentials, and
/// the encryption / hashing functions that take a key or a plaintext
/// password (`crypt()`, `pgp_sym_encrypt()`, `AES_ENCRYPT()`, `hmac()`)
/// — and any URI with a user part (`postgresql://bob:pw@host/db`, as
/// `\c` or `dblink_connect()` take). A false positive only costs a
/// history entry.
pub(crate) fn may_hold_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("\\set")
        || lower.find("://").is_some_and(|at| lower[at..].contains('@'))
        || ["password", "passwd", "pwd", "identified", "secret", "credential", "crypt", "hmac"]
            .iter().any(|word| lower.contains(word))
}

/// The persisted history for `key`. Entries that may hold a secret are
/// dropped — and removed from storage, so history saved before this
/// rule existed gets cleaned the first time it's read.
pub(crate) fn load_history(key: &str) -> Vec<HistoryEntry> {
    let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten()) else { return vec![] };
    let Some(json) = storage.get_item(key).ok().flatten() else { return vec![] };
    let entries: Vec<HistoryEntry> = serde_json::from_str(&json).unwrap_or_default();
    let kept: Vec<HistoryEntry> = entries.iter().filter(|e| !may_hold_secret(&e.sql)).cloned().collect();
    if kept.len() != entries.len() {
        if let Ok(json) = serde_json::to_string(&kept) {
            let _ = storage.set_item(key, &json);
        }
    }
    kept
}

/// Add `sql` — as the user typed it — to the persisted history, unless
/// it may hold a secret (see [`may_hold_secret`]).
pub(crate) fn persist_command(key: &str, sql: &str) {
    if key.is_empty() || sql.trim().is_empty() || may_hold_secret(sql) { return; }
    let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten()) else { return };
    let mut entries = load_history(key);
    entries.retain(|e| e.sql != sql);
    entries.push(HistoryEntry { sql: sql.to_string(), ts: js_sys::Date::now() });
    if entries.len() > HISTORY_CAP {
        entries.drain(..entries.len() - HISTORY_CAP);
    }
    if let Ok(json) = serde_json::to_string(&entries) {
        let _ = storage.set_item(key, &json);
    }
}

// ── Sidebar folder collapse state ───────────────────────────────────────
// One global key — the folder tree is shared across the whole browser.
// Empty string is a valid path (it's the synthetic "All Connections" row).

const COLLAPSED_FOLDERS_KEY: &str = "sidebar:collapsed_folders";

pub(crate) fn load_collapsed_folders() -> std::collections::HashSet<String> {
    let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten()) else { return Default::default() };
    let Some(json) = storage.get_item(COLLAPSED_FOLDERS_KEY).ok().flatten() else { return Default::default() };
    serde_json::from_str(&json).unwrap_or_default()
}

pub(crate) fn persist_collapsed_folders(set: &std::collections::HashSet<String>) {
    let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten()) else { return };
    if let Ok(json) = serde_json::to_string(set) {
        let _ = storage.set_item(COLLAPSED_FOLDERS_KEY, &json);
    }
}

// ── Recents (auto-populated last-opened list) ────────────────────────────
// localStorage-only; the backend never sees this list. Capped at
// RECENTS_CAP so the virtual folder doesn't grow unbounded.

const RECENTS_KEY: &str = "sidebar:recents";
pub(crate) const RECENTS_CAP: usize = 10;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct RecentEntry {
    pub(crate) conn_id:   String,
    pub(crate) opened_at: f64,
}

pub(crate) fn load_recents() -> Vec<RecentEntry> {
    let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten()) else { return vec![] };
    let Some(json) = storage.get_item(RECENTS_KEY).ok().flatten() else { return vec![] };
    serde_json::from_str(&json).unwrap_or_default()
}

pub(crate) fn persist_recents(recents: &[RecentEntry]) {
    let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten()) else { return };
    if let Ok(json) = serde_json::to_string(recents) {
        let _ = storage.set_item(RECENTS_KEY, &json);
    }
}

/// Push a connection-open event onto Recents. De-dupes by `conn_id` so a
/// connection only appears once (most-recent timestamp wins), and trims
/// to [`RECENTS_CAP`]. Most-recent first.
pub(crate) fn touch_recent(recents: &mut Vec<RecentEntry>, conn_id: &str) {
    recents.retain(|r| r.conn_id != conn_id);
    recents.insert(0, RecentEntry {
        conn_id:   conn_id.to_string(),
        opened_at: js_sys::Date::now(),
    });
    if recents.len() > RECENTS_CAP {
        recents.truncate(RECENTS_CAP);
    }
}

#[cfg(test)]
mod tests {
    use super::may_hold_secret;

    #[test]
    fn secrets_never_reach_the_persisted_history() {
        for text in [
            "ALTER USER bob PASSWORD 'hunter2';",
            "CREATE ROLE app LOGIN password 'x';",
            "CREATE USER 'bob'@'%' IDENTIFIED BY 'hunter2';",
            "ALTER USER bob IDENTIFIED WITH caching_sha2_password BY 'x';",
            "SET PASSWORD FOR bob = 'x';",
            "SELECT dblink_connect('host=db user=u password=p');",
            "\\set pw hunter2",
            "\\set api_key 'abc123'",
            "\\set",
            "SELECT 1;\n\\set pw hunter2",
            "\\setenv API_TOKEN abc123",
            "CREATE USER MAPPING FOR bob SERVER s OPTIONS (user 'bob', passwd 'x');",
            "SELECT * FROM vault WHERE client_secret = 'x';",
            "SELECT credentials FROM t;",
            "INSERT INTO users (pw) VALUES (crypt('hunter2', gen_salt('bf')));",
            "SELECT pgp_sym_encrypt(data, 'k3y') FROM t;",
            "SELECT AES_ENCRYPT('data', 'k3y');",
            "SELECT hmac('msg', 'k3y', 'sha256');",
            "\\c postgresql://bob:hunter2@db.example.com/app",
            "SELECT dblink_connect('postgres://u:p@h/db');",
            "CREATE SUBSCRIPTION s CONNECTION 'postgresql://u:p@h/x' PUBLICATION p;",
        ] {
            assert!(may_hold_secret(text), "{text}");
        }
        for text in ["SELECT 1;", "\\d film", "\\x", "\\unset pw", "\\c pagila", "SELECT 'https://example.com/a';", "UPDATE t SET x = 1;"] {
            assert!(!may_hold_secret(text), "{text}");
        }
    }
}
