import { test, expect, Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { openPg } from '../helpers/db';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// A Postgres shell whose session is lost does what psql does: the command
// that found it lost prints the server's FATAL and libpq's "server closed
// the connection unexpectedly" lines, the connection is reset at once
// ("The connection to the server was lost. Attempting reset: Succeeded."),
// and the rest of the input runs on the new one — the command itself
// isn't run again. No "Session ended" banner. (A reset that fails —
// "Failed." twice, then "You are currently not connected to a
// database." — needs the server unreachable; it was checked by hand
// through a TCP proxy.)

const LOST = [
  'FATAL:  terminating connection due to administrator command',
  'server closed the connection unexpectedly',
  'This probably means the server terminated abnormally',
  'before or while processing the request.',
  'The connection to the server was lost. Attempting reset: Succeeded.',
];

async function openPgShell(page: Page) {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  return input;
}

const column = (page: Page, name: string) =>
  visibleShellOutput(page).locator('.shell-table th', { hasText: new RegExp(`^${name}$`) });

test('a session that terminates itself is reset, and the rest of the input runs', async ({ page }) => {
  const input = await openPgShell(page);
  await input.fill('BEGIN;');
  await input.press('Enter');
  await expect(page.locator('.txn-badge.txn-active')).toBeVisible({ timeout: 8_000 });

  await input.fill('SELECT pg_terminate_backend(pg_backend_pid()); SELECT 5 AS e;');
  await input.press('Enter');
  const out = visibleShellOutput(page);
  for (const line of LOST) await expect(out).toContainText(line, { timeout: 10_000 });
  await expect(column(page, 'e')).toBeVisible({ timeout: 10_000 });
  // The transaction went with the session.
  await expect(page.locator('.txn-badge')).toHaveCount(0);
  await expect(page.locator('.reconnect-banner')).toHaveCount(0);
});

test('a statement that finds its idle session killed is not run again', async ({ page }) => {
  const input = await openPgShell(page);
  await input.fill('SELECT pg_backend_pid() AS pid;');
  await input.press('Enter');
  const pid = visibleShellOutput(page).locator('.shell-table').last().locator('td').first();
  await expect(pid).toHaveText(/^\d+$/, { timeout: 10_000 });

  const admin = await openPg();
  try {
    await admin.query('SELECT pg_terminate_backend($1)', [Number(await pid.innerText())]);
  } finally {
    await admin.end();
  }

  await input.fill('SELECT 2 AS b;');
  await input.press('Enter');
  const out = visibleShellOutput(page);
  for (const line of LOST) await expect(out).toContainText(line, { timeout: 10_000 });
  // psql doesn't run it again: the next statement does run.
  await input.fill('SELECT 3 AS c;');
  await input.press('Enter');
  await expect(column(page, 'c')).toBeVisible({ timeout: 10_000 });
  await expect(column(page, 'b')).toHaveCount(0);
  await expect(page.locator('.reconnect-banner')).toHaveCount(0);
});
