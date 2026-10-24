# Contributing to Parliament

Thanks for wanting to help. Bug reports, parity gaps and pull requests are all welcome.

## The bar: behave like the native CLI

Parliament's shell aims to behave exactly as `psql` (18) and the `mysql` client (9) do: the same output — byte for byte where the CLI prints text — the same errors, the same handling of odd input. So a change to the shell starts from what the real CLI does. Try it there, then pin what you find with a test that compares against the CLI's own output. A difference from the CLI is a bug unless it's deliberate and documented; the few deliberate ones are described in [CLAUDE.md](CLAUDE.md) (for example, `\e` puts the edited text back in the input and waits for Enter rather than running it).

## Questions, bugs and ideas

- **Questions and ideas:** [Discussions](https://github.com/parliament-labs/parliament/discussions).
- **Bugs:** an [issue](https://github.com/parliament-labs/parliament/issues), with what you typed, what Parliament printed, and what `psql` or `mysql` prints for the same input.
- **Security problems:** privately, as [SECURITY.md](SECURITY.md) describes — never in a public issue.

## Setting up

You'll need:

- Rust (stable, through [rustup](https://rustup.rs)), the wasm target and [Trunk](https://trunkrs.dev): `rustup target add wasm32-unknown-unknown`, then `cargo install trunk`.
- PostgreSQL 13+ and MySQL 8.0+ running locally, with the Pagila and Sakila sample databases: `tests/fixtures/setup.sh all` loads both ([tests/fixtures/README.md](tests/fixtures/README.md)).
- `psql` and `mysql` on your `PATH`, for the tests that compare with them.
- Node, for the browser tests: see [tests/playwright/README.md](tests/playwright/README.md).
- Your test databases' passwords in `~/.parliament/secrets.env`, made from [tests/fixtures/secrets.env.template](tests/fixtures/secrets.env.template). Never put a password in a file in the repository.

Then, in two terminals:

```bash
cargo run -p noctua   # the backend, on localhost:3000
trunk serve           # the frontend, on localhost:8080 — open this
```

Restart noctua after changing backend code; Trunk reloads the frontend by itself.

## Before you open a pull request

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`, and for the frontend `cargo clippy -p tyto --target wasm32-unknown-unknown -- -D warnings`. CI runs these on Linux and macOS for every pull request.
- If you changed the shell or a database's behaviour, also the live parity tests, `cargo test --workspace -- --ignored` (they need the sample databases and the CLIs, with `~/.parliament/secrets.env` sourced), and the browser regression specs ([tests/playwright/README.md](tests/playwright/README.md)).
- A test for what you changed: a unit test for logic, and for what the shell prints, a spec that compares it with the CLI's own output.

Keep a pull request to one change, and describe in its message what changed and why.

## Finding your way around

- [ARCHITECTURE.md](ARCHITECTURE.md) gives the overall design.
- [CLAUDE.md](CLAUDE.md) has the detail. It's written as the working guide for Claude Code, which much of Parliament was built with, and it's the most complete description of how the shell behaves and why.
- `tyto/src/plane/` holds everything specific to a database: its SQL, its meta-commands, how its CLI prints. noctua stays database-agnostic and runs the SQL it's given.
- `vendor/` holds patched sqlx crates. Change them only as their `PARLIAMENT.md` describes.
- `vault/` holds design notes.

## License

Parliament is licensed under Apache-2.0. By contributing, you agree that your contribution is licensed under it too (section 5 of the license).
