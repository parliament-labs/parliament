# Agent Playbook

How we use Claude Code agents on this project. Short-form reference; the full
rationale lives in `foamy-waddling-alpaca.md` (the plan file that produced
this doc).

## The rules of thumb, in one screen

| Pattern                              | When to reach for it                                                                                                 | Axis     |
|--------------------------------------|----------------------------------------------------------------------------------------------------------------------|----------|
| **Parallel Explore**                 | Feature touches ≥3 files or spans multiple crates. Kick off 2–3 `Explore` agents in one message, one per area.      | ⚡ + ✓    |
| **Worktree-parallel implementation** | Genuinely independent sub-tasks (e.g. a new DBMS plane). Each agent gets its own worktree via `isolation: "worktree"`. | ⚡        |
| **Pre-commit diff review**           | Any commit >100 lines or touching more than one file. Run `/review-diff` before committing.                         | ✓        |
| **Integration testing**              | Any change to plane SQL or the introspect parser. Run `/integration-test-sql` against local MySQL + Postgres.        | ✓        |
| **Architecture pressure-test**       | Non-trivial architectural decisions. Spawn a `Plan` agent with the same brief + no prior context for a cold read.    | ✓        |
| **Scheduled hygiene**                | Weekly vault drift audit, dep freshness, clippy sweep. Not wired up yet; add when a feature justifies it.           | ✓ (slow) |
| **Project custom agents**            | Repetitive role-specific work where briefing overhead > 30 seconds. See `.claude/agents/`.                           | ⚡ + ✓    |

(⚡ = efficiency, ✓ = quality)

## When to skip the playbook entirely

- **Single-file CSS tweaks.** Just do it.
- **Typo / rename refactors in one file.** Just do it.
- **Commits under ~30 lines.** A review pass has higher overhead than the expected value.
- **Sequential work dressed up as parallel.** If phase 2 reads phase 1's output, they're not parallel.
- **Decisions I'm already confident about.** A second-opinion agent there is performative, not useful.

## Custom agents (project-scoped)

Defined in `.claude/agents/`:

- **`code-reviewer`** — diff reviewer with our conventions (no emojis in files, Edit over Write, short-comments-only rule, commit-message style). Checklist-driven; catches the bug classes I systematically miss.
- **`sql-dialect-expert`** — knows the `DbmsPlane` trait, the introspect parser's column-alias contract ("every query AS-aliases its output; never branch in the handler"), the MySQL VARBINARY-as-text quirk, `BIGINT UNSIGNED`, and the PG `pg_get_*def` family.

More can be added when a role proves recurring. Don't define an agent for a one-time use case.

## Project slash commands

Defined in `.claude/commands/`:

- **`/review-diff`** — runs `git diff HEAD` (or `git diff main..HEAD`), spawns the `code-reviewer` agent, returns a bulleted findings list. Use before any non-trivial commit.

## Integration testing (when it lands)

Planned layout:

```
test/
├── fixtures/
│   ├── mysql_seed.sql       # drops + recreates sqlclient_test, fills edge-case tables
│   └── postgres_seed.sql
└── golden/
    ├── mysql/<plane-method>.txt
    └── postgres/<plane-method>.txt
```

Skill: `/integration-test-sql` — reads `SQLCLIENT_TEST_MYSQL_DSN` and
`SQLCLIENT_TEST_PG_DSN`, applies the seed SQL, runs every plane method,
diffs against the golden files, prints drift.

Safety rails: the test DB is always `sqlclient_test` and the skill refuses
to touch anything else. Credentials live in env vars outside the repo.

## Verification / success metrics

How we'll know the playbook is earning its keep:

1. **Speed:** Adding a new DBMS plane (e.g. SQLite) finishes in fewer wall-clock phases than Postgres did, thanks to worktree-parallel sub-tasks.
2. **Correctness:** The reviewer agent catches at least one real bug in its first five uses.
3. **Integration coverage:** Once it exists, `/integration-test-sql` surfaces at least one plane-SQL failure I missed in type-checking.
4. **Drift detection:** The first time we audit the vault against code, we find at least one doc that's out of date.

If any of these fail, the pattern is revisited or dropped — don't keep doing things that aren't earning rent.

## Guardrails

- Don't spam agents. Every agent call costs user-visible context switching.
- Don't let custom agent briefs rot. Treat them like any other code artifact.
- Don't pile on scheduled jobs. Noise defeats the purpose. Start with one.
- Don't reach for agent review when you're confident and the change is small. The threshold matters more than the habit.

## Related docs

- `foamy-waddling-alpaca.md` (plan file) — full rationale and retrospective.
- `memory/feedback_agent_patterns.md` — the when-to-use rules as a loadable feedback memory.
- `CLAUDE.md` — top-level project guide; references this playbook from its Development Workflow section.
