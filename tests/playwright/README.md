# Playwright — Parliament

Browser-level e2e tests and README screenshot capture for the Tyto (Parliament frontend) WASM app.

Two jobs:

1. **Screenshots** (`screenshots/*.spec.ts`) — render the running app in headless Chromium and write curated PNGs to `docs/screenshots/` for use in the project README.
2. **Regressions** (`regression/*.spec.ts`) — assert on UI-only failures that `cargo test`, `clippy`, and code review cannot catch. The canonical case is a CSS layout interaction (see `vault/Engineering/Testing Strategy.md` — Layer 3).

## Prerequisites

One-time:

```bash
# Project-level
rustup target add wasm32-unknown-unknown
cargo install trunk

# Playwright-level (from this directory)
npm install
npx playwright install chromium
```

A running Postgres and MySQL are required (Pagila + Sakila). Seed them with:

```bash
# From the repo root
tests/fixtures/setup.sh all
```

## Running

Three terminals:

```bash
# Terminal 1 — backend
cargo run -p noctua

# Terminal 2 — frontend
trunk serve

# Terminal 3 — tests
cd tests/playwright
npx playwright test                        # everything
npx playwright test screenshots/           # screenshots only
npx playwright test regression/            # regressions only
npx playwright test --headed               # visible browser for debugging
npx playwright show-report                 # open HTML report
```

## How it works

- **Global setup** swaps `~/.sqlclient/connections.json` for `fixtures/connections.sample.json` (backing up the existing file), so sidebar entries for "Local Postgres" and "Local MySQL" are guaranteed present. Global teardown restores the original.
  - If the backend is already running when setup runs, **restart it once** so it re-reads the swapped connections file.
- Tests use `helpers/app.ts` to load the app and wait for a WS-ready DOM signal, then `helpers/connect.ts` to click the relevant sidebar entry.
- Screenshot specs capture specific UI states and write PNGs to `../../docs/screenshots/`. That directory is committed.
- The README's demo GIF: `screenshots/05-readme-demo.spec.ts` captures its frames (and how long each shows) to `screenshots/_demo/`, ignored; `python3 scripts/readme-demo-gif.py` (needs Pillow) builds `docs/screenshots/readme-demo.gif` from them. It's 1x, so the README embeds it without a width, at the column's full width. Run it on connections that open a shell and show nothing private.

## Conventions

- One spec file per screenshot, named `NN-kebab-case.spec.ts` matching the ranking in the Playwright scaffold plan.
- Screenshot output path is `docs/screenshots/NN-kebab-case.png`.
- Use `page.locator('.app').screenshot(...)` for most shots (crops browser chrome). Use full `page.screenshot(...)` only when the shot legitimately needs the full viewport (e.g. multi-pane split geometry).
- README embeds use `<img width="720" src="docs/screenshots/...">` — explicit sizing avoids GitHub's over-zoom on 2x images.

## Troubleshooting

- **"Cannot connect to localhost:8080"** — `trunk serve` isn't running. First build takes 60–120s.
- **"Cannot connect to localhost:3000"** — `cargo run -p noctua` isn't running.
- **Sidebar entries missing** — backend was running before global-setup swapped connections.json. Restart it and re-run.
- **Tests flake on WebSocket** — raise the `openApp` ready-signal timeout in `helpers/app.ts`.
