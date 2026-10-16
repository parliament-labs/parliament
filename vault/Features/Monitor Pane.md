# Monitor Pane

> **Note:** This feature is called the "Activity pane" in the codebase (`PaneKind::Activity`, `activity.rs`). The terms "Monitor Pane" and "Activity pane" refer to the same feature.

## Purpose

A live view of what's happening on the database server. The DBA's "what is this database doing right now?" surface. This differentiates a PAM-integrated client from a simple SQL editor — it's a real operations tool.

## Core Views

### Process List

`SHOW FULL PROCESSLIST` on a timer (configurable: 1s / 5s / manual). Highlight:
- Long-running queries (> N seconds, configurable)
- Queries in `Locked` or `Waiting for lock` state — red highlight
- Own connection highlighted
- Click a row → show full query text in a detail drawer
- Kill button for long-running queries (sends `KILL QUERY <id>`)

```
┌─────┬────────┬──────┬────────┬──────┬────────────────────────────────────────┐
│ Id  │ User   │ DB   │ Time   │ State│ Info                                   │
├─────┼────────┼──────┼────────┼──────┼────────────────────────────────────────┤
│  42 │ root   │ prod │  0.001 │      │ SHOW FULL PROCESSLIST                  │
│  31 │ appusr │ prod │ 47.320 │      │ SELECT * FROM orders WHERE ...  ← long │
│  28 │ appusr │ prod │  0.003 │Locked│ UPDATE users SET ...            ← 🔴   │
└─────┴────────┴──────┴────────┴──────┴────────────────────────────────────────┘
```

### InnoDB Status Summary

Parse `SHOW ENGINE INNODB STATUS` and extract the key sections:
- Deadlock information (last deadlock, if any)
- Transaction list (active transactions with age)
- Lock wait summary
- Buffer pool stats (hit ratio, dirty pages)

Raw output of INNODB STATUS is a wall of text — parsing and surfacing the interesting parts is high value.

### Key Metrics Dashboard

Small at-a-glance cards, refreshing on a timer:

| Metric | Source |
|--------|--------|
| Threads connected | `@@threads_connected` |
| Threads running | `@@threads_running` |
| QPS (5s avg) | Delta of `Queries` status var |
| Slow queries | Delta of `Slow_queries` status var |
| InnoDB buffer pool hit% | Calculate from status vars |
| Replication lag | `SHOW REPLICA STATUS` → `Seconds_Behind_Source` |
| Connection% | `Threads_connected / max_connections` |

### Replication Status

For replicas: `SHOW REPLICA STATUS` (MySQL 8+, or `SHOW SLAVE STATUS` for older). Show replication lag, IO/SQL thread state, last error.

For source: `SHOW BINARY LOG STATUS`, list of connected replicas.

## Aurora-Specific Views

For AWS Aurora, the middle tier could pull CloudWatch metrics:
- Reader/writer endpoint distinction
- Aurora-specific metrics: `AuroraReplicaLag`, `CPUUtilization`, `DatabaseConnections`
- This requires AWS SDK access in the middle tier (IAM role or passed credentials)

Out of scope for initial implementation — note for when PAM integration is fleshed out.

## Implementation Approach

The monitor pane drives its own refresh loop. Two options:

**Option A: Frontend polling** — frontend sends `ExecuteQuery { sql: "SHOW FULL PROCESSLIST" }` on a timer, parses the result, updates the pane. Reuses the existing query path.
- Pro: no new backend code
- Con: uses the session's only connection (or needs a separate monitoring connection)

**Option B: Dedicated monitoring WebSocket message** — new `ClientMessage::StartMonitor { session_id, interval_ms }`, backend sends periodic `ServerMessage::MonitorUpdate { processes, metrics }`.
- Pro: backend can multiplex and optimize (single `SHOW PROCESSLIST` serves multiple tabs)
- Con: more backend code, new message types

Recommendation: Option A first (faster to build, uses existing machinery). Migrate to Option B when the polling becomes noticeable overhead or when the monitoring view is heavily used.

## Killing Queries

`KILL QUERY <id>` needs to be in-band (sent as a regular `ExecuteQuery`). Note: in MySQL, `KILL QUERY` requires the `PROCESS` or `SUPER` privilege. PAM integration could gate this via `AccessPolicy`.

In the PAM tier, killing another user's query should be a logged, potentially policy-gated action.
