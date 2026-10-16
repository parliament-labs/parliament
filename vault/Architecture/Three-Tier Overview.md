# Three-Tier Overview

## The Tiers

```
┌─────────────────────┐
│   Browser (WASM)    │  Leptos 0.6 CSR → compiled to WASM
│   UI only           │  No DB credentials. No desktop app.
│   no secrets        │  WebSocket only.
└────────┬────────────┘
         │ WebSocket (JSON messages)
         │ ws://host:3000/ws
┌────────┴────────────┐
│   Rust Middle Tier  │  axum 0.7 + sqlx 0.8
│   PAM layer         │  Holds persistent DB connections
│   audit seam        │  Credential injection point
│   session pinning   │  One MySqlConnection per session
└────────┬────────────┘
         │ TCP (MySQL wire protocol)
         │ or TLS (RDS / Aurora)
┌────────┴────────────┐
│   MySQL / Postgres  │  Aurora, RDS, local, etc.
└─────────────────────┘
```

## Why No Desktop App

The browser is the right host for this client. Reasons:

1. **PAM works better with browser auth** — SSO, SAML, MFA all live in the browser already
2. **Zero install on client machines** — critical for enterprise; no IT ticket to deploy
3. **WASM is fast enough** — the WASM payload is a one-time load; queries go through WebSocket
4. **Central deployment** — the Rust middle tier can run on a bastion or PAM server; all users share it
5. **No credential leakage to disk** — the browser never persists secrets; they stay in the middle tier

Tauri and Electron are non-starters. This is a browser-first product.

## Session Pinning

Each WebSocket connection maps to exactly **one** `MySqlConnection` — never pooled. This is essential for:

- `TEMPORARY TABLE` — scoped to the connection
- `TRANSACTION` state — BEGIN / COMMIT / ROLLBACK
- `SET @variable` — session variables
- `USE dbname` — affects the connection
- Anything that depends on MySQL session state

```rust
struct Session {
    conn: Arc<Mutex<MySqlConnection>>,
    // future: audit_handle, access_policy_handle
}
```

## WebSocket Message Protocol

See `shared/src/lib.rs` for the canonical types. Key messages:

**Client → Server**
- `ListConnections` — fetch saved connections on open
- `SaveConnection { def }` — persist a new connection definition
- `Connect { connection_id }` — establish a DB session
- `Disconnect { session_id }` — close DB session
- `ExecuteQuery { session_id, sql }` — run SQL

**Server → Client**
- `ConnectionList { connections }` — response to ListConnections
- `SessionOpened { session_id, server_version, .. }` — connection established
- `SessionError { message }` — connection failed
- `SessionClosed` — session terminated
- `QueryResult { columns, rows, exec_time_ms, rows_affected }` — SELECT/SHOW result
- `QueryError { message }` — query failed

## Dev Setup

```bash
cargo run -p noctua          # port 3000
trunk serve                   # port 8080 → open in browser
```

Backend restarts manually; trunk hot-reloads WASM automatically.
