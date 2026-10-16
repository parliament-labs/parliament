import { test, expect, Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { openMySql } from '../helpers/db';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// A MySQL shell whose session is lost reconnects on its next statement
// and runs it there, printing what the mysql CLI prints (`--reconnect`
// is on by default) — no "Session ended" banner. Two ways to lose it:
// the session killed while a statement of its own ran (that statement's
// server error shows; the next one reconnects), or killed while idle
// (the next statement finds the connection dead and runs again in the
// new one). Postgres does what psql does instead: psql-reset.spec.ts.

const LOST = [
  'ERROR 2013 (HY000): Lost connection to MySQL server during query',
  'No connection. Trying to reconnect...',
];

async function openMySqlShell(page: Page) {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  return input;
}

async function expectReconnected(page: Page, column: string) {
  const out = visibleShellOutput(page);
  for (const line of LOST) await expect(out).toContainText(line, { timeout: 10_000 });
  await expect(out).toContainText(/Connection id: {4}\d+/);
  await expect(out).toContainText('Current database: sakila');
  await expect(out.locator('.shell-table th', { hasText: new RegExp(`^${column}$`) })).toBeVisible({ timeout: 10_000 });
  await expect(page.locator('.reconnect-banner')).toHaveCount(0);
}

test('a MySQL shell that killed its own session reconnects on the next statement', async ({ page }) => {
  const input = await openMySqlShell(page);
  await input.fill('KILL CONNECTION_ID();');
  await input.press('Enter');
  await expect(visibleShellOutput(page)).toContainText('ERROR 1317 (70100)', { timeout: 10_000 });
  await expect(page.locator('.reconnect-banner')).toHaveCount(0);
  await expect(input).toBeVisible();

  await input.fill('SELECT 1 AS a;');
  await input.press('Enter');
  await expectReconnected(page, 'a');
});

test('help after the session was lost reconnects, then asks the server', async ({ page }) => {
  const input = await openMySqlShell(page);
  const out = visibleShellOutput(page);
  await input.fill('KILL CONNECTION_ID();');
  await input.press('Enter');
  await expect(out).toContainText('ERROR 1317 (70100)', { timeout: 10_000 });

  // As the CLI's com_server_help: reconnect, then the server's help.
  await input.fill('help select');
  await input.press('Enter');
  for (const line of LOST) await expect(out).toContainText(line, { timeout: 10_000 });
  await expect(out).toContainText(/Connection id: {4}\d+/);
  await expect(out.locator('.shell-printed').last()).toContainText("Name: 'SELECT'", { timeout: 10_000 });
  await expect(page.locator('.reconnect-banner')).toHaveCount(0);
});

test('a MySQL statement that finds its idle session killed runs again in a new one', async ({ page }) => {
  const input = await openMySqlShell(page);
  await input.fill('SELECT CONNECTION_ID() AS id;');
  await input.press('Enter');
  const id = visibleShellOutput(page).locator('.shell-table').last().locator('td').first();
  await expect(id).toHaveText(/^\d+$/, { timeout: 10_000 });

  const admin = await openMySql();
  try {
    await admin.query(`KILL ${Number(await id.innerText())}`);
  } finally {
    await admin.end();
  }

  await input.fill('SELECT 2 AS b;');
  await input.press('Enter');
  await expectReconnected(page, 'b');
});
