import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

// Swap ~/.sqlclient/connections.json for the Playwright fixture so
// sidebar entries for "Local Postgres" and "Local MySQL" are guaranteed
// present. The original file is moved aside and restored by
// global-teardown.ts.
//
// Caveat: the backend reads this file on startup. If it's already
// running, restart it once after first Playwright run to pick up the
// fixture entries.

const HOME = os.homedir();
const CONFIG_DIR = path.join(HOME, '.sqlclient');
const TARGET = path.join(CONFIG_DIR, 'connections.json');
const BACKUP = path.join(CONFIG_DIR, 'connections.json.playwright-backup');
const FIXTURE = path.resolve(__dirname, 'fixtures', 'connections.sample.json');

async function globalSetup(): Promise<void> {
  if (process.env.PLAYWRIGHT_SKIP_FIXTURE_CONNECTIONS === '1') {
    console.log(`[playwright] PLAYWRIGHT_SKIP_FIXTURE_CONNECTIONS=1 — using existing connections.json untouched`);
    return;
  }

  fs.mkdirSync(CONFIG_DIR, { recursive: true });

  // If a backup already exists, a previous run crashed before teardown.
  // Refuse to proceed — otherwise we would overwrite the real current
  // connections.json and the stale backup would be restored later,
  // silently losing the user's current state.
  if (fs.existsSync(BACKUP)) {
    throw new Error(
      `[playwright] stale backup detected at ${BACKUP}. ` +
        `A previous run did not tear down cleanly. ` +
        `Inspect it, then either restore it manually (mv ${BACKUP} ${TARGET}) ` +
        `or delete it before re-running.`,
    );
  }

  if (fs.existsSync(TARGET)) {
    fs.renameSync(TARGET, BACKUP);
    console.log(`[playwright] backed up existing connections.json → ${BACKUP}`);
  }

  fs.copyFileSync(FIXTURE, TARGET);
  console.log(`[playwright] installed fixture connections.json`);
  console.log(`[playwright] restart \`cargo run -p noctua\` if it was running before this setup`);
}

export default globalSetup;
