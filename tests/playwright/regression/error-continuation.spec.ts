import { test, expect, Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// Pasted statements keep going past one that fails, as both CLIs do:
// psql's ON_ERROR_STOP is off by default, and the mysql CLI has no such
// switch. Neither prints anything beyond the server's error. With
// `\set ON_ERROR_STOP on`, psql drops the rest of the input silently.

const BATCH = 'SELECT 1 AS a; SELECT nosuch FROM actor; SELECT 2 AS b;';

async function openShell(page: Page, connection: string) {
  await openApp(page);
  await clickConnection(page, connection);
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  return input;
}

for (const connection of ['Local Postgres', 'Local MySQL']) {
  test(`${connection}: a failed statement doesn't stop the rest`, async ({ page }) => {
    const input = await openShell(page, connection);
    await input.fill(BATCH);
    await input.press('Enter');

    const out = visibleShellOutput(page);
    await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toBeVisible({ timeout: 10_000 });
    await expect(out).toContainText(/nosuch/);
    await expect(out).not.toContainText(/Aborted|skipped/);
  });
}

test('Postgres with ON_ERROR_STOP on drops the rest silently', async ({ page }) => {
  const input = await openShell(page, 'Local Postgres');
  await input.fill('\\set ON_ERROR_STOP on');
  await input.press('Enter');
  await input.fill(BATCH);
  await input.press('Enter');

  const out = visibleShellOutput(page);
  await expect(out).toContainText(/column "nosuch" does not exist/, { timeout: 10_000 });
  // A later statement's result: had `b` still run, it would be there by now.
  await input.fill('SELECT 3 AS c;');
  await input.press('Enter');
  await expect(out.locator('.shell-table th', { hasText: /^c$/ })).toBeVisible({ timeout: 10_000 });
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toBeVisible();
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toHaveCount(0);
  await expect(out).not.toContainText(/Aborted|skipped/);
});
