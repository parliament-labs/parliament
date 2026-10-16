# Vision & Positioning

## The One-Line Pitch

A browser-based SQL client that feels like the native CLI — and can be the front end for a PAM system's database access tier, so every query is authenticated, authorized, and audited without the DBA ever touching a credential.

## What Problem Does This Solve

PAM products gate access to databases, but existing clients (DataGrip, DBeaver, TablePlus) require direct DB credentials — they bypass PAM entirely. The missing piece is a usable SQL client that works *through* PAM rather than around it. This client routes every connection through its own middle tier, which is where PAM plugs in.

## Three-Tier Architecture

```
Browser (WASM)  ─── WebSocket ───  Middle Tier (Rust)  ─── TCP ───  MySQL / Postgres
     UI only                         holds credentials                actual database
   no secrets                        audit log here
```

The browser WASM layer never sees credentials. That's the whole point.

## Primary Targets

- **MySQL Aurora + RDS** — common in AWS shops; RDS IAM auth = zero-standing-privilege story
- **Postgres** — psql users are vocal and will notice if it doesn't feel right
- Others later (SQL Server, SQLite for dev) via the [[DBMS Planes]] plugin model

## The "Feels Like the Real CLI" Requirement

This is a primary differentiator. DBAs use the real `mysql` and `psql` CLIs every day. They have muscle memory. A client that doesn't support `\G`, `\dt`, `\l`, `\d tablename`, `\c dbname` will feel like a toy.

The shell must make a MySQL DBA feel at home the moment they connect. Likewise for Postgres.

See [[Shell Design Philosophy]] and [[Shell Meta-Commands]].

## Audience

- DBAs and developers with existing CLI muscle memory — they should feel at home
- Broad audience beyond DBAs — GUI panes make it approachable for devs who've never typed `\G`
- Teams on a PAM system who want PAM-gated DB access without friction

## Where This Fits

The client works standalone. The [[PAM Integration Seams]] are structured so a PAM system's middle tier can take over credentials, audit, connection sharing and access policy without changing the client.
