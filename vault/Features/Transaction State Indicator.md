# Transaction State Indicator

## The Problem

A DBA types `BEGIN` and runs several statements. Then gets distracted, or the connection sits idle. Without a clear indicator that a transaction is open, it's easy to:
- Forget to `COMMIT` and leave locks held
- Think the connection is clean when it isn't
- Miss that a table is locked because someone else's session is waiting on yours

The real `mysql` CLI has no transaction indicator either — it's a known pain point.

## Proposed UX

A persistent banner in the shell toolbar area, visible whenever a transaction is open:

```
┌─────────────────────────────────────────────────────────────────────┐
│  production  MySQL 8.0.32           ⚠ TRANSACTION OPEN    Commit | Rollback │
└─────────────────────────────────────────────────────────────────────┘
```

Or more subtle — a colored indicator in the toolbar:

```
┌──────────────────────────────────────────────────────────┐
│  production  MySQL 8.0.32  ● TXN      [Disconnect]       │
└──────────────────────────────────────────────────────────┘
```

Where `● TXN` is yellow/amber, changes to green on commit, and disappears on rollback.

## Implementation Sketch

**Detecting transaction state:**

Option A (client-side): Track `BEGIN` / `START TRANSACTION` and `COMMIT` / `ROLLBACK` in the frontend by parsing every SQL string sent.
- Simple, no backend changes
- Fragile: misses implicit commits (DDL statements), errors that roll back automatically
- Also misses `autocommit=0` behavior

Option B (server-side): After each query, backend checks `@@in_transaction` (MySQL 5.7+) and includes it in `QueryResult`.
- Accurate — MySQL knows the actual state
- Requires adding `in_transaction: bool` to `ServerMessage::QueryResult`
- Backend adds `SELECT @@in_transaction` after every non-SELECT statement, or checks the connection state

Option B is correct. The `ServerMessage::QueryResult` and `ServerMessage::QueryError` messages should include transaction state.

**Backend change:**
```rust
// After executing any query:
let in_txn = sqlx::query_scalar::<_, i64>("SELECT @@in_transaction")
    .fetch_one(&mut *conn)
    .await
    .unwrap_or(0);

// Include in response:
ServerMessage::QueryResult { 
    columns, rows, exec_time_ms, rows_affected,
    in_transaction: in_txn != 0,  // new field
}
```

**Frontend change:**
- Add `in_transaction: RwSignal<bool>` to App state
- Update on every `QueryResult` and `QueryError`
- Render the banner/indicator in the shell toolbar when `in_transaction` is true
- Quick-action buttons: `COMMIT` and `ROLLBACK` in the toolbar when open

## Styling

The indicator should be visually distinct from normal state — amber/yellow (`--yellow: #d29922`) to suggest caution. Red if the transaction has been open for a configurable time (future enhancement).

## Open Questions

- Should a stale open transaction (idle > N minutes) trigger a warning? This could prevent accidental lock accumulation.
- Should we show the transaction duration (stopwatch since `BEGIN`)?
- Should the `COMMIT` / `ROLLBACK` quick-action buttons be in the toolbar, or should typing `COMMIT;` be the only way? (Toolbar buttons might cause accidental commits — consider requiring explicit typing.)
