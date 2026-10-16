# Open Source Strategy

## The Model

**Open source foundation** — a fully functional local SQL client. No PAM, no vault, credentials in `~/.sqlclient/connections.json`. Works standalone. This is what gets adopted by developers and the broader community.

**Enterprise PAM tier** — swap in the PAM integration seams (see [[PAM Integration Seams]]). A vault provides credentials. Every connection goes through the audit bus. JIT access policies enforced.

## Why Open Source the Foundation

- DBAs and developers will use and improve a good open source SQL client
- Community adoption creates organic evangelism
- Contributors who build DBMS planes (Oracle, MSSQL, etc.) extend its reach
- Credibility — it's a real client first, not a PAM feature
- Forces good design: if the seams are clean enough to be swappable, the PAM integration will be too

## The Plugin Boundary

Each DBMS has a **plane** that implements a `DbmsPlane` trait. This trait is the natural plugin point:

```rust
pub trait DbmsPlane: Send + Sync {
    fn dbms_name(&self) -> &str;
    fn handle_meta_cmd(&self, cmd: &str, args: &str) -> MetaCmdResult;
    fn translate_query(&self, sql: &str) -> String;
    // ... type mapping, schema inspection, etc.
}
```

Third parties can implement planes for their DBMS without touching core code.

## Licensing

Apache-2.0 (decided 2026-10-16): open, commercial-friendly, with a patent grant; the license of SQLx and most of the Rust ecosystem. `NOTICE` carries the PostgreSQL License for the psql / libpq ports and SQLx's for the vendored crates. PAM integration layers could carry their own license.

## What Goes in Each Tier

| Layer | Open Source | Enterprise PAM |
|-------|-------------|----------------|
| Browser WASM UI | ✅ | ✅ (same) |
| Rust middle tier | ✅ (local mode) | ✅ (PAM mode) |
| CredentialProvider | file-based | vault API |
| SessionRecorder | not built | PAM audit bus |
| ConnectionRegistry | local JSON | PAM shared records |
| AccessPolicy | allow all | JIT PAM policies |
| DBMS Planes | open | open |
