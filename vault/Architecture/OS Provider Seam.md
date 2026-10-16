# OS Provider Seam

Part of the [[PAM Integration Seams]] family, but distinct enough to deserve its own doc — this seam is about **filesystem and process access**, not credentials or policy. It's where parity features like `SOURCE file.sql`, `\i path/to/script.sql`, `\copy FROM 'data.csv'`, `LOAD DATA LOCAL INFILE`, `SELECT INTO OUTFILE`, and `\o query_output.txt` plug in.

A browser-based SQL client has a tricky relationship with the filesystem: there are *two* of them, not one, and they don't mean the same thing. That's what this seam makes explicit.

## The two filesystems

When a DBA types `SOURCE scripts/nightly-audit.sql` at a psql/mysql prompt on their laptop, there is no ambiguity — it's the laptop's filesystem, and `scripts/nightly-audit.sql` is a path there. When a DBA types the same thing in a browser-based client where the backend might be running on a jump host 500 miles away, "the filesystem" is suddenly ambiguous. Two filesystems exist:

1. **The user's laptop filesystem**, reachable only through browser APIs — `<input type="file">`, drag-and-drop, `showOpenFilePicker()` (File System Access API where supported), and `<a download>` for writes. Subject to browser sandboxing: no arbitrary path access, every read needs explicit user consent, every write is a download.

2. **The backend host filesystem**, reachable through WebSocket RPCs we control. In [[Deployment Topologies|solo-local]] mode the backend host IS the user's laptop so the two filesystems collapse into one; in solo-remote mode the backend host is a jump host; in team-shared mode it's a team server that multiple users share.

Both are useful. A user-facing `\i` probably means the laptop (user's personal scripts). A team-shared `\i /srv/team-scripts/deadlock-check.sql` probably means the backend (team-curated scripts). The same verb should work for both, dispatched by path shape or an explicit prefix.

## Two traits, not one

The cleanest shape is two seams with overlapping APIs, chosen at the call site:

```rust
/// Filesystem access on the user's local machine, via browser APIs.
/// Every operation requires explicit user consent (file picker / download).
pub trait BrowserOsProvider {
    /// Prompt the user to pick one or more files; return contents.
    async fn read_files(&self, hint: FilePickerHint) -> Result<Vec<FileBlob>, OsError>;

    /// Trigger a browser download of the given content.
    async fn write_download(&self, name: &str, content: &[u8], mime: &str) -> Result<(), OsError>;

    /// Prompt the user to pick a directory (File System Access API only,
    /// not available on every browser). Returns `Unsupported` elsewhere.
    async fn pick_directory(&self) -> Result<DirectoryHandle, OsError>;
}

/// Filesystem access on the backend host, via backend-side RPCs.
/// Subject to policy — may be denied, sandboxed to a root, or absent entirely
/// depending on deployment mode.
pub trait BackendOsProvider {
    async fn read_text_file(&self, path: &Path) -> Result<String, OsError>;
    async fn read_binary_file(&self, path: &Path) -> Result<Vec<u8>, OsError>;
    async fn write_file(&self, path: &Path, content: &[u8]) -> Result<(), OsError>;
    async fn list_files(&self, dir: &Path, filter: FileFilter) -> Result<Vec<FileRef>, OsError>;
    async fn file_exists(&self, path: &Path) -> Result<bool, OsError>;

    /// Explicit capability query — callers should check before assuming a
    /// given operation is available, since the backend may run in a reduced
    /// mode (read-only, or no filesystem at all).
    fn capabilities(&self) -> BackendCapabilities;
}

pub struct BackendCapabilities {
    pub read: bool,
    pub write: bool,
    pub list: bool,
    /// Path prefix the backend is allowed to touch, if any (None = unrestricted
    /// within its deployment sandbox). Team-shared mode probably pins this to
    /// something like `/srv/sqlclient/scripts/`.
    pub root: Option<PathBuf>,
}

pub enum OsError {
    NotFound,
    PermissionDenied,
    Unsupported(&'static str),
    UserCancelled,
    TooLarge,   // guardrail — enforce upload/download size limits
    Io(String),
}
```

`BrowserOsProvider` lives entirely in the frontend (`tyto`). It's a thin wrapper around `web_sys` — file input events, `Blob`, `URL.createObjectURL` for downloads, optionally `showOpenFilePicker()` where supported.

`BackendOsProvider` lives in the backend crate as a trait, with implementations varying by deployment mode. New WebSocket message types (`ClientMessage::OsRead`, `ClientMessage::OsWrite`, `ClientMessage::OsList`) carry the calls across the wire; results come back as `ServerMessage::OsRead{Result,Error}` etc.

## Per-mode capability matrix

| Operation | Solo-local | Solo-remote | Team-shared | Enterprise-PAM |
|---|---|---|---|---|
| Browser: read via `<input type=file>` | ✓ | ✓ | ✓ | ✓ |
| Browser: download output | ✓ | ✓ | ✓ | ✓ |
| Browser: drag-and-drop attach | ✓ | ✓ | ✓ | ✓ |
| Backend: read text file | ✓ (local FS) | ✓ (backend host) | ✓ policy-gated | ✓ PAM-gated |
| Backend: write file | ✓ (local FS) | ✓ (backend host) | ✓ policy-gated | ✓ PAM-gated |
| Backend: list directory | ✓ | ✓ | ✓ scoped to root | ✓ scoped + audited |
| Spawn OS process | ✗ permanently | ✗ permanently | ✗ permanently | ✗ permanently |
| Arbitrary shell escape `\!` / `SYSTEM` | ✗ | ✗ | ✗ | ✗ |
| `$EDITOR` external editor (`\e` / `\ef` / `\ev`) | ✓ exception — see below | ✗ | ✗ | ✗ |
| Raw TCP to arbitrary hosts | ✗ | ✗ | ✗ | ✗ |

Note the two hard nos: **process spawning and arbitrary shell escape are never supported**. Even in team-shared mode where the backend has OS access, exposing that to end users as an interactive shell is a massive attack surface with unbounded blast radius. The backend is a database client, not an SSH client. Users who need a shell on the backend host should use SSH.

## Parity items this seam unblocks

Every parity inventory item below is Tier 3 *until* one of the provider implementations lands; then it moves to Tier 1 or 2 depending on the complexity of the individual implementation. See [[Dialect Coverage Framework]] for the tier scheme.

**Frontend-only (`BrowserOsProvider`):**

- **MySQL:** `SOURCE file.sql` where `file.sql` is picked from the user's machine. `LOAD DATA LOCAL INFILE 'user-path'` where the file is streamed through the frontend. `SELECT ... INTO OUTFILE` becomes a browser download. `\T filename` (tee output to file) becomes a buffered download.
- **Postgres:** `\i filename` equivalent to SOURCE. `\copy` where the file is local to the user. `\o filename` tee output.
- **Cross-dialect:** "Save this query result as CSV / JSON / markdown" → download blob.

All of these are T2 work: finite file-picker plumbing, multi-statement splitting for SOURCE/\i, streaming for LOAD DATA.

**Backend-required (`BackendOsProvider`):**

- **MySQL:** `LOAD DATA INFILE '/path/on/backend'` (the non-LOCAL variant — path is resolved on the server's filesystem). Normally this means the MySQL server's filesystem, but a team-shared deployment might want "on the sqlclient backend's filesystem" as a distinct operation too.
- **Postgres:** `COPY ... FROM '/path/on/backend'`. Same distinction.
- **Team-shared scripts:** `SOURCE /srv/sqlclient/team-scripts/nightly-audit.sql` — scripts live on the backend so the team can share them without each user having local copies.

These are T3 work per item — each needs security thinking (path traversal, symlink attacks, resource limits on reads).

## Phased implementation path

Today (solo-local) → future (enterprise-PAM), without painting into a corner:

### Phase 0 — Today, nothing built

Parity inventory marks all OS-touching verbs as `T2 needs BrowserOsProvider` or `T3 needs BackendOsProvider`. The shell returns `MetaAction::NotYetImplemented` with a pointer to this doc when users hit them. See [[Dialect Coverage Framework]] for the runtime grace mechanism.

### Phase 1 — BrowserOsProvider landed

`tyto/src/os/browser.rs` implements the trait against `web_sys`. Parity items that only need browser-side filesystem move from T3 to T2 and get implemented on their own branches (SOURCE is probably first — it's the most-loved verb).

No backend changes needed. No new WebSocket messages. The frontend reads a file, splits on semicolons, sends each statement as an `ExecuteQuery` the normal way.

### Phase 2 — BackendOsProvider in-process

A module inside the existing `noctua` — call it `noctua/src/os/`. Implements the trait against `tokio::fs`. New WebSocket messages cross the wire. Configurable sandbox root so the backend can only touch paths under, say, `~/.sqlclient/files/` (solo-local) or `/srv/sqlclient/` (team-shared).

Still a single process, still the same binary. We just added file-handling RPCs.

### Phase 3 — Separate process (if/when needed)

Promote `noctua/src/os/` to its own crate or binary — `sqlclient-os-broker` — that the main backend RPCs to, or that the frontend talks to directly over a separate WebSocket. Benefits:

- Security isolation — the DB backend can't read files even if compromised.
- Different privileges — the OS broker can run as an unprivileged user while the DB backend runs with whatever it needs.
- Swappable — PAM mode replaces the local OS broker with a vault-backed one that reads/writes the vault's file attachments instead of the local filesystem.

This is **speculative** — don't build it until Phase 2 is cramped. The split is cheap when the trait boundary is already in place, expensive to retrofit.

### Phase 4 — PAM-backed provider

In enterprise-PAM mode the `BackendOsProvider` implementation points at the PAM system's file store instead of the local filesystem. A vault that keeps file attachments inside records, for example: a team script lives in a shared record and the backend reads it through the vault's SDK. All PAM-gated: every read is audited, every write goes into the vault's history, access is JIT-granted.

At this phase, `root` on `BackendCapabilities` becomes a virtual scope (a vault's shared folder) rather than a filesystem path.

## Security and safety rails

Every seam operation should be hard-capped:

- **Max file size** for reads (64 MB default). Larger files error with `OsError::TooLarge`. Prevents a `SOURCE giant-dump.sql` from blowing out memory.
- **Max path depth** for listings (e.g. 4 levels). Prevents a `\dir *` from walking the entire filesystem.
- **Allowlist of directories** in team-shared / enterprise modes. The backend refuses any path outside its configured root.
- **No symlink traversal outside the root.** Follow symlinks, re-check that the resolved path is still inside the sandbox root.
- **Binary detection on reads.** If a "text file" read returns non-UTF-8 bytes, return the binary variant instead of silently corrupting — parallel to the same guardrail we put on `mysql_value_to_string`'s VARBINARY UTF-8 decode.
- **Rate limiting on file operations.** A runaway Activity-pane refresh plus per-tick script reload could hammer the filesystem. Per-user rate limits belong in [[Deployment Topologies|team-shared mode]]'s resource isolation story.
- **Never auto-follow user-supplied paths into sensitive directories.** Explicit denylist on `/etc/`, `~/.ssh/`, `~/.aws/`, `$HOME/.sqlclient/` (because it has our own plaintext credentials for now, until that's PAM-migrated).

## What this seam will **never** do

Enumerated so the parity framework can cite them as `NotSupported { reason: BrowserLimitation | ByDesign }`:

- **Spawn OS processes.** `\!` / `SYSTEM` / `\shell`. Not in `BrowserOsProvider` (browsers can't), not in `BackendOsProvider` (huge attack surface, wrong tool for the job). If you need a shell on the backend host, use SSH.
- **Invoke an external editor — except solo-local.** In solo-local mode noctua runs on the user's own machine as the user, so opening *their* editor on a temp file is the one OS process it spawns (built 2026-10-01, `noctua/src/editor/`). The guard rails that make it acceptable: no command ever crosses the wire (noctua resolves the editor from its own files and environment — connection override, Settings, the CLI's env vars, `vi`); only Parliament's own origins can open the WebSocket; the editor gets a private 0600 file in a 0700 per-edit directory; GUI editors run in their own session (`setsid`) with no stdin. It stays ✗ in every shared mode — there the backend's machine isn't the user's, and the future user-machine component is where it would live.
- **Pipe output to external pagers.** psql's `\pset pager on` → `less`. The browser's native scroll is the pager.
- **Execute arbitrary binaries.** Not even in enterprise mode. If a customer needs to run a binary, they spawn it themselves and call back into the client when it's done.
- **Modify the browser's host filesystem without user consent.** Every write IS a download — the user sees a save prompt and chooses a destination.
- **Read user files without user consent.** Every read IS a file-picker dialog.

These are the canonical items in the Browser Limitations section of [[Dialect Coverage Framework]].

## Related docs

- [[Three-Tier Overview]] — the structural split this seam lives inside.
- [[PAM Integration Seams]] — the other four seams (CredentialProvider, SessionRecorder, ConnectionRegistry, NotebookStorage, AccessPolicy). OS Provider is a sixth that the existing doc doesn't cover.
- [[Deployment Topologies]] — which deployment mode activates which provider implementation.
- [[Dialect Coverage Framework]] — the parity framework; many T3 items cite this seam as their unblocking prereq.
- [[Shell Design Philosophy]] — the user-facing shell stays the same across all modes; what varies is which OS-touching verbs succeed.
