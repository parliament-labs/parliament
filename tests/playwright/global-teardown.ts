import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

// Restore the user's original ~/.sqlclient/connections.json (if the
// backup exists). Delete the fixture version we installed.

const HOME = os.homedir();
const CONFIG_DIR = path.join(HOME, '.sqlclient');
const TARGET = path.join(CONFIG_DIR, 'connections.json');
const BACKUP = path.join(CONFIG_DIR, 'connections.json.playwright-backup');

async function globalTeardown(): Promise<void> {
  if (process.env.PLAYWRIGHT_SKIP_FIXTURE_CONNECTIONS === '1') {
    return;
  }
  if (fs.existsSync(BACKUP)) {
    fs.renameSync(BACKUP, TARGET);
    console.log(`[playwright] restored original connections.json`);
  } else if (fs.existsSync(TARGET)) {
    fs.unlinkSync(TARGET);
    console.log(`[playwright] removed fixture connections.json (no backup existed)`);
  }
}

export default globalTeardown;
