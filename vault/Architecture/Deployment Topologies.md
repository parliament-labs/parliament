# Deployment Topologies

The [[Three-Tier Overview]] establishes browser / middle-tier / DB as the structural decomposition. This doc is about something more specific: **the middle tier is locationally flexible**, and that single observation unlocks a capability ladder of deployment modes from "one user on a laptop" to "shared PAM-integrated DBA team".

Framing this explicitly now matters because several decisions elsewhere in the codebase — session ownership, credential storage, audit seams, product packaging — only make sense if you know which rung of the ladder you're building for.

## The four modes

| Mode | Browser runs | Middle tier runs | Who uses it | Product shape |
|---|---|---|---|---|
| **Solo-local** | Laptop | Same laptop | 1 individual | Today's default — standalone binary, single-user assumption baked in. |
| **Solo-remote** | Laptop | Jump host / bastion near the DBs | 1 individual | Same binary, deployed remotely. Browser reaches it over VPN / zero-trust. |
| **Team-shared** | N laptops | One shared server | Small team (DBA pod, SRE on-call rotation) | Different binary — same core crate, compiled with team-server features. Multi-user. |
| **Enterprise-PAM** | N laptops | Shared server(s) | Whole org | Team-shared + one of the [[PAM Integration Seams]] implementations compiled in. |

Each rung adds capabilities; none removes them. Solo-local is what's deployed today. The rest are the roadmap.

## Capability matrix

| Capability | Solo-local | Solo-remote | Team-shared | Enterprise-PAM |
|---|:---:|:---:|:---:|:---:|
| Transport-agnostic middle tier | ✓ | ✓ | ✓ | ✓ |
| User authentication | — | — | required | required |
| Per-user session ownership | — | — | required | required |
| Multi-user metadata store (connections, scripts, history) | — | — | required | required |
| TLS on the middle-tier listener | optional | required | required | required |
| Shared connection catalog (team publishes) | — | — | ✓ | ✓ |
| Shared script library | — | — | ✓ | ✓ |
| Shared result snapshots (handoff to a teammate) | — | — | ✓ | ✓ |
| Live session visibility across the team | — | — | ✓ | ✓ |
| `CredentialProvider` integration | local stub | local stub | local stub | PAM |
| `SessionRecorder` / audit trail | local stub | local stub | local / PAM | PAM (append-only) |
| `AccessPolicy` with approval workflows | allow-all | allow-all | allow-all | PAM |
| Just-in-time credentials (connection carries vault ref, not password) | — | — | — | ✓ |
| Session recording (full query + result log, compliance-grade) | — | — | optional | ✓ |

The key insight: **team-shared is the first rung where the [[PAM Integration Seams]] actually pay rent**. Solo-local has nothing to PAM-ify — one user, one box, one credential set. Team-shared makes shared credentials meaningful. Enterprise-PAM makes policy meaningful. You don't skip rungs; each is a useful stopping point.

## Today's posture (Solo-local, strict)

The backend as shipped:

- Binds to `127.0.0.1:3000` loopback only (via kernel-level `TcpListener::bind` — see `noctua/src/main.rs`).
- CORS allowlist restricts HTTP endpoints to `http://localhost:{3000,8080}` and `http://127.0.0.1:{3000,8080}`.
- WebSocket `/ws` upgrade rejects any request whose `Origin` header isn't one of Parliament's own pages (`http://localhost` / `http://127.0.0.1` on port 3000 or 8080 — the same list as CORS). Other pages on localhost are refused too. This check is load-bearing: a page that could open the socket could save a connection or Settings with an editor command of its choosing and then ask noctua to open the editor.
- Zero authentication — the backend trusts every local caller. Safe *because* of the three defenses above; unsafe the moment any one is relaxed.
- Sessions are a flat global `HashMap<session_id, LiveConnection>` — any caller on the same machine with a session_id can drive it. TODOs already marked on `type Sessions = ...` and `fn load_connections`.
- Connections persist to `~/.sqlclient/connections.json` as a single global file keyed off `$HOME`.

None of this is wrong for solo-local. It's *only* wrong if you relax the loopback-only stance without closing the other gaps.

## What solo-remote actually needs (minimal delta)

Almost nothing code-wise, but the operational story changes:

1. **Relax the bind back to a routable interface.** Revert the `127.0.0.1` bind to `0.0.0.0` (or better, a configurable bind address) when deploying on a jump host. This is a config flag, not a code change.
2. **TLS-terminate in front of the backend.** Usually a reverse proxy (nginx, Caddy, Cloudflare Tunnel, Tailscale Funnel). The backend stays plain HTTP; TLS is someone else's problem. No code change.
3. **Authentication at the proxy.** The proxy handles SSO / OIDC / client certs; the backend trusts "whoever got through the proxy". Still single-user-per-instance.
4. **Browser connects over the real network.** `wss://` to the TLS endpoint. UI side is already transport-agnostic.

Solo-remote is viable *today* with only operational work (reverse proxy config). It's still single-user-per-backend; N users means N backends.

## What team-shared actually needs (code delta)

This is where real engineering starts. In priority order:

### 1. User authentication inside the backend

The proxy-based "auth at the edge" story stops working when multiple users share one backend, because the backend now has to know *which user* is making each request — CORS headers and bearer tokens need to be parsed server-side.

**Sketch:** OIDC-based login, returning a signed short-lived token. Each WebSocket `Connect` message carries the token; the backend validates it and stamps `user_id` into the session record. Bearer tokens on the HTTP routes (`/ws` upgrade, connection CRUD) same shape.

Dev-mode fallback: pre-shared token in an env var for local multi-user testing without standing up an IdP.

### 2. Per-user session ownership

The current `Sessions = Arc<Mutex<HashMap<String, Arc<Mutex<LiveConnection>>>>>` is keyed by a UUID with no owner. Needs `(user_id, session_id)` keying and every request-handling path needs `assert_owned_by(user_id, session_id)` gating.

Session ids should probably be opaque random strings per-user, not a global namespace — prevents one user from guessing another user's id.

### 3. Multi-user metadata store

`connections.json` as a single global file keyed off `$HOME` stops being the right shape. Options, in order of complexity:

- **SQLite next to the backend** — simple, file-based, handles concurrency, transactional. My strong lean for the first iteration.
- **Postgres alongside the DB the client manages** — overkill until you need replication.
- **External K/V (Redis, etcd)** — wrong tool; we need durability, not speed.

Schema for the metadata store:
- `users` — user_id, display_name, email, auth_provider_sub, created_at.
- `connections` — id, owner_user_id, scope (`personal` / `team:<team_id>` / `global`), definition (encrypted ConnectionDef blob or external reference).
- `scripts` — id, owner, scope, name, content, last_modified. (`vault/Features/Notebook & Scripts.md` hints at this; here it becomes real.)
- `history` — user_id, connection_id, session_id, query, exec_time_ms, timestamp. Append-only.
- `audit` — user_id, action, target, timestamp, payload_summary. Append-only. Feeds the [[PAM Integration Seams]] `SessionRecorder`.
- `snapshots` — id, owner, query, columns, rows (JSONB), created_at. For handoff.

### 4. Resource isolation

One user's runaway query shouldn't eat the backend's CPU or memory budget. Per-user quotas:

- Max concurrent sessions (e.g. 10).
- Max concurrent running queries (e.g. 5).
- Row-count or byte-count cap on result sets (streamed down with a "truncated" flag).
- Rate limit on introspect queries (the Activity pane can hammer the backend at 1s refresh — needs a cap).

### 5. TLS on the listener

wss:// with a real cert becomes mandatory when the backend is reachable from more than one process. Either terminate at the backend (rustls) or at a reverse proxy and let the backend stay plain. Either way, not localhost anymore.

## Enterprise-PAM delta

On top of team-shared, the enterprise rung activates the [[PAM Integration Seams]] in earnest:

- **`CredentialProvider` → CyberArk / HashiCorp Vault / AWS Secrets Manager / wherever.** Connection definitions store a vault record reference, not a password. At connect-time the backend asks the PAM system for the credential, uses it exactly once, and never writes it to disk. Cache locally within the connect call so we don't hammer the PAM API.
- **`SessionRecorder` → PAM audit bus.** Every query, every result summary, every session event is logged append-only with user identity, IP, justification, approval context. Volume and PII handling become a real design problem; *probably* we log queries unconditionally and result data only when policy demands.
- **`AccessPolicy` → PAM policy engine.** Before running a query, ask PAM: is this user allowed to run this query against this database right now? Policy may say yes / no / requires-approval. The approval flow is asynchronous — the shell needs a "waiting for approval" state and a cancel path. This implies a new `MetaAction` variant or `LoadState` state.
- **`ConnectionRegistry` → PAM shared records.** Teams see a curated connection list managed in the PAM system, not in our metadata store. The `scope` column on `connections` becomes a reference to a PAM concept instead of our internal team id.
- **Just-in-time credentials.** The connect path goes from `Connect { connection_id }` to `Connect { vault_record_ref }` with the credential resolved server-side. This is a protocol-level change in `shared`.

## Product variants — build shapes

Given the four modes and the existing [[PAM Integration Seams]] vision of seam-per-implementation, there are three plausible ways to ship:

1. **Cargo features on one crate** — `default = []`, `--features pam-cyberark`, `--features pam-hashicorp`. Single workspace, conditional compilation. Simple. Every build carries the kitchen sink even when only one feature is on.
2. **Dynamic plugin loading** — compile PAM integrations as `.so` / `.dylib`, load at startup. Maximum flexibility. Every dynamic loader has a footgun surface (ABI stability, symbol conflicts, security). Probably not worth it for our size.
3. **Separate binaries backed by a common core crate** — `sqlclient-local`, `sqlclient-team`, `sqlclient-enterprise` are distinct workspace members. Each depends on `sqlclient-core` plus exactly one PAM crate. Two products, one codebase.

**My lean is option 3**, with option 1 as a stepping stone. Benefits of separate binaries:

- Clear SKU boundaries matching the deployment modes.
- Local build stays small — zero PAM code in the OSS tree.
- Security audit scope per binary — the enterprise build carries PAM code, the local build doesn't.
- Different license terms per binary are allowed without contortions.
- Matches how you'd actually sell the thing (community OSS + enterprise licensed).

Tradeoff: every binary is its own `Cargo.lock`, its own build, its own CI job. Acceptable.

The `sqlclient-core` crate absorbs everything that's dialect-agnostic and mode-agnostic: message types (already `shared`), plane trait, UI renderer, introspect parser, cream theme, pane tree. The per-mode crates add: authentication, metadata store, `CredentialProvider` / `SessionRecorder` / `AccessPolicy` / `ConnectionRegistry` / `NotebookStorage` / `OsProvider` implementations ([[OS Provider Seam]]).

## Open questions

These are **not blockers for today's solo-local work** — parked here so future sessions can find them.

- **Audit log storage volume.** Compliance modes may require logging result data, which is GB/day in a busy team. Local JSONL append is fine for solo; shared mode needs thought. Off-box ship-to-SIEM? Rotated local files? PAM systems have their own audit pipelines; the PAM implementation should write through to them directly rather than duplicating.
- **Schema cache invalidation across users.** If user A creates a table, user B's Schema Explorer cache is stale. Needs an invalidate-on-DDL signal the backend emits to all sessions. Manageable but not trivial.
- **Session handoff.** Sarah starts a shell on mobile, wants to continue on her laptop. Architecturally possible once sessions are backend-owned and user-keyed. Not urgent.
- **Backend restart / session persistence.** A backend restart today drops every session — bad for solo-local, worse for shared mode. Options: session state snapshots on graceful shutdown, or just accept the drop and make the reconnect flow fast.
- **Per-user preferences vs team defaults.** Theme, refresh intervals, workspace layouts. Usually per-user; sometimes team defaults. New pane type? Metadata store columns? Think about this when shared mode is real.
- **Where does the backend run in team-shared mode and who pays for it?** Customer-hosted is simplest — they run it on their own infra, we ship a binary. SaaS-hosted is convenient but gives us a whole new operational surface and a new threat model (our ops people can read customer query metadata). For the first team-shared build, ship the customer-hosted shape only.

## Related docs

- [[Three-Tier Overview]] — the structural browser / middle / DB split this doc presumes.
- [[PAM Integration Seams]] — the five seams whose implementations vary by mode. This doc is what decides *which* mode activates which seam.
- [[OS Provider Seam]] — the file/process seam; team-shared mode is where the `BrowserOsProvider` vs `BackendOsProvider` distinction starts to matter.
- [[Dialect Coverage Framework]] — the parity framework; several Tier 3 items are unblocked only in specific deployment modes.
- [[Shell Design Philosophy]] — the shell experience stays consistent across all four modes. This doc doesn't change the shell; it changes what's *underneath* the shell.
