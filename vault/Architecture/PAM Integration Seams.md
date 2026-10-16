# PAM Integration Seams

## The Design Pattern

Every place where the application touches secrets, sessions, audit, or access policy is wrapped in a seam — a trait interface with two implementations: a local stub for the open source version, and a PAM-backed implementation for enterprise deployments.

The goal: the entire codebase compiles and works perfectly in both modes. Swapping in the PAM tier is a compile-time configuration, not a code change.

## The Five Seams

### 1. CredentialProvider

> "Where do connection credentials come from?"

| Mode | Implementation |
|------|---------------|
| Open source | `~/.sqlclient/connections.json` (plaintext — prototype only) |
| PAM | The vault's API — credentials never leave the vault; injected at connection time |

The connection definition stored locally includes the password. In PAM mode, the `ConnectionDef` would carry a vault record reference instead, and `CredentialProvider` would call the vault API to resolve it at connect time.

**Future consideration**: RDS IAM auth. Instead of a static password, generate a short-lived IAM auth token. `CredentialProvider` would call the AWS SDK for this. Zero standing privilege story.

### 2. SessionRecorder

> "Who recorded what, when, and from where?"

| Mode | Implementation |
|------|---------------|
| Open source | Not built — queries are not persisted |
| PAM | The PAM audit bus — every query, every result, every session event |

The middle tier has the right interception point: between the WebSocket handler and the `MySqlConnection`. Every `ExecuteQuery` call passes through the session recorder.

This is also where session recording (full keystroke/output replay) would be implemented. PAM systems already record SSH/RDP sessions — the SQL client extends that to database sessions.

### 3. ConnectionRegistry

> "What connections are defined and who can see them?"

| Mode | Implementation |
|------|---------------|
| Open source | `~/.sqlclient/connections.json` |
| PAM | Shared records — connections visible to teams, not just individuals |

In PAM mode, a DBA team sees the same set of connections. Connections are managed in the PAM system's vault, not on each user's laptop.

### 4. NotebookStorage

> "Where are saved queries and notebooks stored?"

| Mode | Implementation |
|------|---------------|
| Open source | Not built — see [[Notebook & Scripts]] for design |
| PAM | Vault records + sharing — notebooks shareable across the team |

A team DBA writes a useful maintenance query notebook. In PAM mode it's a shared record. Team members can view and run it.

### 5. AccessPolicy

> "Is this user allowed to run this query against this database right now?"

| Mode | Implementation |
|------|---------------|
| Open source | Allow all |
| PAM | JIT policies — time-limited access grants, break-glass workflows |

The most powerful seam. In PAM mode, a developer might request "read access to production db for 2 hours." The PAM system approves it (or routes for approval), creates a JIT grant, and the connection goes through. When time expires, the session terminates.

## Implementation Approach

Currently all five seams are stubs — the open source implementations. The PAM implementations will live in a separate crate (`crates/pam` or similar) that is conditionally compiled or feature-flagged.

```toml
# Cargo.toml
[features]
default = []
pam = ["dep:pam-sdk"]
```

```rust
#[cfg(feature = "pam")]
use pam::VaultCredentialProvider as CredentialProvider;
#[cfg(not(feature = "pam"))]
use local::FileCredentialProvider as CredentialProvider;
```

## Locked Out Risk

One known concern: if the PAM system is down, users can't connect to databases. This is a PAM system tradeoff, not specific to this client. Mitigations that exist in the PAM world:
- Break-glass access with elevated audit logging
- AWS Secrets Manager as a fallback credential store (rotating secrets, not vault-dependent)
- Local caching of time-limited credentials

This is a PAM architectural decision, not something this client needs to solve now.
