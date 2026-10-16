# Parliament Secure Backend Requirements for PAM-Dominant Positioning

## Purpose

This document defines what the **secure backend** for Parliament must support in order to become a credible, differentiated, and commercially valuable platform for:

- PAM vendors
- security-conscious enterprises
- regulated customers
- internal security teams that want controlled, auditable database access

This is **not** a generic backend checklist. It is specifically focused on the capabilities required to dominate the **secure database access / PAM-integrated SQL client** space.

---

## Executive Summary

Parliament should not be positioned as merely:

- a web SQL client
- a browser shell
- a schema browser

It should be positioned as:

> **A governed, auditable, browser-native database access platform with native-dialect UX and a hardened control plane.**

The core strategic idea is:

- the browser frontend provides a world-class SQL workspace
- the backend becomes the **enforcement, mediation, policy, and audit layer**
- for PAM environments, the backend is where trust, control, attribution, and governance live

The frontend may assist with safety and ergonomics, but the backend must be the **authoritative security boundary**.

---

## Product Positioning

### Standard Positioning

For ordinary internal teams:

> Modern browser-native SQL client with native database shells, graphical tooling, multi-tab workspaces, and centralized connectivity.

### Secure / PAM Positioning

For security vendors and regulated environments:

> Secure, policy-enforced database access workspace that routes all activity through a controlled backend for authentication, authorization, auditing, approval workflows, masking, export control, and forensic visibility.

---

## Core Architectural Principle

The secure backend must treat the WebSocket connection as:

> **a transport channel only, not a trust boundary**

Trust must be established and enforced at the backend, per operation.

### Guiding Rule

Every operation must conceptually flow through:

1. **Receive**
2. **Authenticate**
3. **Authorize**
4. **Classify**
5. **Enforce policy**
6. **Execute or block**
7. **Record**
8. **Return controlled results**

---

## Platform Structure Recommendation

Do **not** build two unrelated backends.

Instead, build:

### Shared Core Runtime

Responsible for:

- WebSocket/session lifecycle
- request routing
- database connection/session handling
- dialect-aware execution
- workspace mechanics
- schema/data services
- file/storage services
- structured logging primitives
- extension/plugin points

### Secure Capability Layer

Responsible for:

- policy decisions
- approval workflow hooks
- query classification
- audit event emission
- masking/redaction decisions
- export/download controls
- PAM/SSO/auth integration
- forensic session recording
- compliance controls

### Product Profiles

- **Standard backend**
- **Secure backend**
- **OEM / white-label secure backend**

These should share one core platform while linking different policy, auth, audit, and control implementations.

---

## Non-Negotiable Backend Responsibilities

The secure backend must be authoritative for the following:

### 1. Authentication

Backend must establish and verify identity for every session.

Must support:

- local auth for dev/test
- enterprise SSO
- PAM-mediated auth
- short-lived session credentials
- step-up authentication for sensitive actions
- revalidation for long-lived sessions

The frontend must never be treated as the source of truth for identity.

---

### 2. Authorization

Authorization must be enforced server-side, not only at connect time.

It must support decisions based on:

- user identity
- group/role
- environment
- database profile
- action type
- object classification
- approval state
- session context
- tenant context

Examples:

- user may open a read-only profile but not a production write profile
- user may run `SELECT` but not `DELETE`
- user may browse schema but not export data
- user may connect to staging without approval but must obtain approval for production

---

### 3. Policy Enforcement

Backend must support a policy engine that can answer questions such as:

- is this operation allowed?
- should this action be denied?
- does this require approval?
- should this result be masked?
- is export permitted?
- should this session be terminated?

The secure backend should initially support hard-coded and configuration-driven policies before evolving to a richer policy model.

Policies should be expressible in terms of:

- environment
- database/profile
- operation type
- object sensitivity
- identity/role
- time window
- justification presence
- prior approval

---

### 4. Query / Action Classification

Before execution, backend should classify incoming actions.

Minimum classifications:

- `READ`
- `WRITE`
- `DDL`
- `DESTRUCTIVE`
- `EXPORT`
- `ADMIN`
- `SESSION_CONTROL`
- `FILE_OPERATION`

Additional classifications should include:

- touches production
- touches sensitive object(s)
- touches PII/regulated data
- large-row-read candidate
- full-table-scan risk
- destructive-without-filter
- high-blast-radius action

This classification layer is one of the most commercially important differentiators.

---

### 5. Auditability and Forensic Visibility

The backend must produce a strong audit trail that answers:

- who performed the action
- when
- from where
- under which authenticated identity
- under which delegated role
- against which database/profile/environment
- what was attempted
- what policy decision was applied
- what result occurred

At minimum, audit events should exist for:

- login/session start
- session end
- profile open/close
- database connection open/close
- query attempt
- query execution decision
- approval request
- approval grant/deny
- export request
- file read/write/download
- masking/redaction applied
- policy violation
- forced disconnect
- admin action

The audit system must support both:

- operational visibility
- later forensic reconstruction

---

## High-Value Differentiators Required for PAM Dominance

The following features are what move the backend from "secure enough" to "market-moving."

### 1. Per-Operation Enforcement

The secure backend must never rely solely on connection-level trust.

Every sensitive action must be independently authorized.

This is essential for:

- long-lived WebSocket sessions
- multi-tab workspaces
- profile switching
- delegated roles
- just-in-time access
- approval-based access

---

### 2. Approval Workflow Hooks

Backend must support approval gating for sensitive actions such as:

- connecting to production
- destructive DML
- DDL
- exporting large datasets
- access to regulated schemas
- privileged monitoring/termination actions

At minimum, the backend must support:

- action enters pending state
- approval request is emitted
- approval or denial is recorded
- execution resumes or is blocked

The approval system may initially be adapter-driven or simulated, but the hook points must be designed early.

---

### 3. Explain-Before-Execute / Risk Preview

Before running high-risk actions, backend should be able to supply structured warnings such as:

- query type
- target environment
- estimated rows touched
- destructive operation warning
- no-filter destructive warning
- large result risk
- export risk
- sensitive object involvement

Examples:

- "This action will DELETE rows in US Production."
- "This query may return millions of rows."
- "This command targets a sensitive customer table."
- "This export exceeds approved size thresholds."

This is valuable for both security teams and human users.

---

### 4. Data Masking / Redaction

Backend must be able to enforce masking/redaction decisions independent of the database engine.

Must support:

- role-based masking
- column-based masking
- partial redaction
- display-only masking
- reveal-with-logging
- export masking rules

Examples:

- last four digits only
- redact secrets entirely
- blur/obscure until approved reveal action
- mask by default in production

Masking logic should live outside the frontend and outside the database when required by customer policy.

---

### 5. Export and Data Egress Control

This area is essential.

Secure backend must support:

- export allow/deny
- export approval requirement
- file size/row count thresholds
- destination restrictions
- no-local-write mode
- controlled cloud storage destinations
- audit trail for all exports/downloads
- potentially watermarking or tagging

The system should support secure modes such as:

- no clipboard
- no download
- cloud-only managed export
- encrypted export destination only
- approved storage target only

Even if browser limitations affect some controls, the backend must be the decision-maker and gatekeeper.

---

### 6. Session Recording / Replay

Backend should support forensic reconstruction of database sessions.

At minimum, record:

- user/session identity
- profile/environment
- opened workspaces/tabs/panes
- executed commands
- decision outcomes
- result metadata
- timestamps
- errors
- approval interactions

Later evolution may support richer replay, but even structured timeline reconstruction is highly valuable.

---

### 7. Environment / Profile Sensitivity Model

Profiles must be first-class security objects.

Each profile should have metadata such as:

- environment class (`dev`, `qa`, `staging`, `prod`)
- sensitivity level
- approval requirements
- allowed roles
- export policy
- masking policy
- file/storage policy
- session timeout policy

This is important because policy must often be profile-based before it is object-based.

---

## WebSocket / Session Security Requirements

Parliament already uses WebSockets between WASM frontend and backend. For secure deployments, the backend must harden this layer aggressively.

### Transport

- WSS only
- TLS everywhere
- no plaintext mode outside local development
- strong certificate handling

### Handshake Controls

- explicit origin allowlist
- reject untrusted browser origins
- authenticate at connect time
- reject malformed upgrade requests
- subprotocol validation if used

### Session Security

- server-authoritative session state
- short-lived session tokens or equivalent
- session revalidation
- idle timeout
- absolute lifetime limits where appropriate
- forced close on logout/session revocation
- heartbeat / ping timeout handling

### Message Validation

Every message must use a typed envelope and be schema-validated.

Must enforce:

- allowed message types
- required fields
- size limits
- tab/workspace ownership
- profile ownership
- rate limits
- denial of unknown message shapes

### Per-Message Authorization

Examples of message types requiring authorization checks:

- open profile
- execute SQL
- export result
- write file
- read file
- reveal masked value
- open monitoring pane
- terminate backend session
- administer connection

---

## Database Session Mediation Requirements

The secure backend must act as the sole execution intermediary between frontend and database.

This means:

- browser never directly talks to DB
- browser never receives raw DB credentials
- backend owns DB connection lifecycle
- backend can inject control logic before execution
- backend can log and classify all traffic
- backend can shape or redact results before returning them

### Additional Requirements

- connection pooling strategy where appropriate
- per-session dedicated connections when needed for safety/consistency
- safe handling of transaction boundaries
- clear session-to-DB mapping for auditability
- support for query cancellation / termination
- support for backend-enforced read-only sessions

---

## File / Storage Control Requirements

Because Parliament shell functionality includes reading/writing files, the backend must control file semantics for secure environments.

Must support multiple storage modes:

### Mode A: Local / Developer Friendly

- regular file operations
- simpler storage semantics

### Mode B: Secure Managed Storage

- no uncontrolled workstation writes
- no direct local downloads
- managed backend storage only
- approved cloud storage destinations only
- audit of all file operations
- size and content restrictions
- retention / expiration rules

Sensitive environments may require that file operations be entirely remapped away from the user’s local workstation.

---

## Integration Surface for PAM / Security Vendors

To support multiple PAM vendors without ad hoc chaos, Parliament secure backend must expose a clear integration surface.

### Required Integration Domains

#### Authentication Adapter

- validate identity
- obtain user/session claims
- support federated auth or PAM-mediated identity

#### Authorization / Policy Adapter

- ask whether action is allowed
- receive deny/allow/approval decision
- optionally retrieve policy metadata/reason

#### Approval Adapter

- submit approval request
- poll/receive approval decision
- associate approved action with approver identity

#### Audit Adapter

- emit structured security events
- support durable external audit sinks

#### Session Constraint Adapter

- retrieve session constraints such as:
  - allowed profiles
  - export restrictions
  - masking rules
  - session duration limits
  - read-only enforcement

### Design Goal

The vendor-specific work must be pushed into adapters, not scattered through the core runtime.

---

## High-Signal Security Events the Backend Must Surface

The secure backend should intentionally produce a small number of high-value events, not endless noise.

Examples:

- destructive production command attempted
- production connection opened under privileged role
- bulk export started/completed
- masked data reveal performed
- approval-required action requested
- approval-required action denied
- access to regulated object detected
- policy violation blocked
- unusual result volume threshold exceeded
- no-local-write policy bypass attempt
- repeated auth failures
- session token invalidation / forced logout

These events should be structured, attributable, and easy to route to the proper owners.

---

## Ownership and Attribution Model

The backend should make it impossible to ask "who did this?" without an answer.

Every sensitive action must be attributable to:

- a real user identity
- an authenticated session
- a selected profile/environment
- a role or delegated privilege set
- optionally an approver identity
- optionally a ticket/justification identifier

This is a major product advantage over loosely governed SQL access tools.

---

## Compliance and Trust Requirements

Even before formal compliance features, the backend should be designed to support:

- immutable-seeming audit pipelines
- retention controls
- configurable redaction in logs
- separation of duties
- admin visibility boundaries
- reason codes / business justification capture
- step-up auth for sensitive actions

The system should be designed so that a future compliance story is easy to build, not bolted on.

---

## Backend Safety Modes

The secure backend should support deployable safety modes.

Examples:

### Developer Mode

- permissive
- lightweight audit
- simple storage
- no approval hooks required

### Managed Enterprise Mode

- centralized auth
- audit enabled
- policy enforcement enabled
- export controls configurable

### Regulated / High-Security Mode

- strict policy enforcement
- no local writes
- approval workflows
- masking by default
- step-up auth
- forensic session capture
- aggressive session and export controls

These modes should be built from policy/capability composition, not from separate codebases.

---

## Roadmap Priorities for Maximum Commercial Leverage

If prioritizing for PAM/OEM value, backend work should roughly proceed in this order:

### Phase 1: Secure Execution Foundation

- hardened WebSocket/session model
- server-authoritative identity/session state
- per-message authorization hooks
- profile/environment metadata model
- structured audit event model

### Phase 2: Policy and Risk Controls

- operation classification
- policy engine
- risk preview / explain-before-execute
- destructive-operation blocking/approval hooks
- read-only and environment-based restrictions

### Phase 3: Egress and Visibility Controls

- export controls
- managed file/storage policies
- masking/redaction
- high-signal event surfacing
- better admin/audit views

### Phase 4: PAM/OEM Integration Layer

- clean adapter interfaces
- at least one reference integration
- approval workflow integration
- external audit sink integration
- white-label packaging model

### Phase 5: Forensic Depth and Compliance Maturity

- session replay/timeline reconstruction
- stronger retention controls
- reason/justification capture
- admin separation of duties
- advanced compliance features

---

## Explicit Non-Goals for the Secure Backend

To stay focused, the secure backend should **not** try to:

- replace native database integrity/security features
- become a full SIEM
- become a general-purpose file sharing platform
- rely on frontend-only protections for real security
- fork into multiple permanently divergent backend codebases

The secure backend is a **governed execution and mediation layer**, not a replacement for the database or the entire security stack.

---

## Strategic Value Statement

If executed correctly, the secure backend gives Parliament the ability to become:

- the best UX layer for PAM-based database access
- the best browser-native SQL workspace for regulated customers
- a white-label acceleration platform for security vendors
- a differentiated control plane for governed SQL interaction

This is the important shift:

> Not “a nicer SQL client.”

But:

> **A controlled, attributable, policy-enforced database execution platform with a first-class user experience.**

That is where the money is.

---

## Final Design Rule

For every backend feature, ask:

> **Does this strengthen the backend as the trusted policy and execution boundary?**

If yes, it belongs in the secure roadmap.

If not, it may be useful, but it is not part of the PAM-dominance story.
