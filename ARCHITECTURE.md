# Architecture

Parliament is a browser-native SQL client with a three-tier design. This document
is the top-level orientation; for deep dives see [`vault/Architecture/`](vault/Architecture/)
and the strategic future direction in [`vault/Roadmap/Secure Backend Roadmap.md`](vault/Roadmap/Secure%20Backend%20Roadmap.md).

## Three tiers

```
┌──────────────────────┐   WebSocket    ┌──────────────────────┐    TCP    ┌────────────────────┐
│  Tyto  (browser)     │ ◀────────────▶ │  Noctua  (server)    │ ◀───────▶ │  MySQL / PostgreSQL │
│  Leptos 0.6 → WASM   │                │  axum + sqlx, Rust   │           │                     │
│  Port 8080 (trunk)   │                │  Port 3000, localhost│           │                     │
└──────────────────────┘                └──────────────────────┘           └────────────────────┘
```

- **Tyto** — the frontend. Pure browser WASM; holds no database credentials, talks
  only to Noctua. Leptos 0.6 CSR. Source in `tyto/`.
- **Noctua** — the backend. Rust binary. Holds database connections via sqlx,
  serves the compiled WASM to the browser, and is the sole execution intermediary
  between Tyto and any database. Source in `noctua/`.
- **Databases** — MySQL and PostgreSQL. Noctua dials out via TCP (sqlx native
  protocol); no JDBC, no proxy.
- **Shared wire types** — `ClientMessage` / `ServerMessage` / `ConnectionDef` in
  `shared/`, compiled into both Tyto and Noctua.

## Why this shape

1. **Browser isolation.** Tyto has no ability to speak the native MySQL/Postgres
   wire protocols from a browser context. Noctua is the only thing that can.
   This also means the browser never sees raw credentials — a prerequisite for
   the governed-access positioning described in the Secure Backend Roadmap.
2. **Single security boundary.** All database interaction flows through one
   Rust process we control. Authentication, authorization, audit, masking, and
   export policy all land at Noctua when they're built.
3. **Native-feeling UX.** WASM gives us a reactive shell experience that rivals
   desktop clients, without desktop-packaging overhead.

## Key design decisions

### DBMS planes

Per-dialect logic lives in `tyto/src/plane/` as traits — `MysqlPlane`, `PostgresPlane`,
etc. Each plane owns:

- meta-command translation (`\dt`, `\df`, `\sf`, ...)
- schema-explorer SQL (table list, column list, DDL reconstruction)
- keyword and function lists for syntax highlighting
- connection-form defaults

Noctua is **dialect-agnostic** — it receives final SQL text and runs it.
All dialect knowledge is front-loaded in the plane. Adding a new database
means implementing a new plane, not changing the backend.

See [`vault/Architecture/DBMS Planes.md`](vault/Architecture/DBMS%20Planes.md).

### Session pinning

Each pane in the UI owns a dedicated database connection (`MySqlConnection` or
`PgConnection`), never pooled. Required for:

- temp tables (session-scoped on the server)
- open transactions
- session variables (`SET search_path`, `SET timezone`, `innodb_lock_wait_timeout`, ...)

Connections live as `Arc<Mutex<LiveConnection>>` in Noctua's state map, keyed by
a session UUID the frontend uses to route all future messages for that pane.

### Typed message envelope

All browser↔backend traffic is JSON-serialized `ClientMessage` / `ServerMessage`
variants defined in `shared/`. Each variant carries the minimum payload for one
operation (connect, execute, introspect, disconnect, etc.) and all responses
carry a `session_id` (or `client_tab_id`) so the frontend can route them to the
correct UI pane.

### Concurrency model

Noctua splits its WebSocket handler into a reader and a writer connected by an
mpsc channel. Messages that do a database round-trip (`ExecuteQuery`,
`IntrospectQuery`, `Connect`, `TestConnection`, `ListDatabases`) are spawned as
independent tasks so a long-running query on one session never blocks the reader
from processing messages for other sessions. State-mutating messages (save,
delete, list connections, disconnect) stay inline on the reader so
"issue-then-read-back" ordering is preserved.

Per-session serialization still holds: the session's `Arc<Mutex<LiveConnection>>`
ensures a single session's queries execute one at a time.

This was added in response to a real lock-wait scenario — see
[`vault/Roadmap/Secure Backend Roadmap.md`](vault/Roadmap/Secure%20Backend%20Roadmap.md)
for how this groundwork feeds the "per-operation enforcement" goal in
the secure-backend vision.

## Repository layout

```
parliament/
├── tyto/           Frontend crate (WASM)
├── noctua/         Backend crate (native binary)
├── shared/         Shared message types, compiles for both native and wasm32
├── tests/
│   ├── fixtures/   Pagila + Sakila seed scripts
│   └── playwright/ End-to-end browser tests and README screenshots
├── docs/
│   └── screenshots/ Committed PNGs referenced from README
├── vault/          Obsidian-flavoured design vault (see vault/Home.md)
│   ├── Architecture/
│   ├── Engineering/
│   ├── Features/
│   ├── Parliament/  Branding + naming rationale + legacy prototypes
│   └── Roadmap/     Secure Backend Roadmap (north star)
├── Trunk.toml      Frontend build config
├── Cargo.toml      Workspace root (members: tyto, noctua, shared)
├── CLAUDE.md       Working guidance for Claude Code sessions
└── README.md       Public-facing project README
```

## Pointers into the vault

- **Naming rationale** — [`vault/Parliament/NAMING.md`](vault/Parliament/NAMING.md)
- **Three-tier motivation** — [`vault/Architecture/Three-Tier Overview.md`](vault/Architecture/Three-Tier%20Overview.md)
- **DBMS planes in detail** — [`vault/Architecture/DBMS Planes.md`](vault/Architecture/DBMS%20Planes.md)
- **Shell design philosophy** — [`vault/Architecture/Shell Design Philosophy.md`](vault/Architecture/Shell%20Design%20Philosophy.md)
- **Testing strategy (4 layers)** — [`vault/Engineering/Testing Strategy.md`](vault/Engineering/Testing%20Strategy.md)
- **PAM / secure-backend vision** — [`vault/Roadmap/Secure Backend Roadmap.md`](vault/Roadmap/Secure%20Backend%20Roadmap.md)
