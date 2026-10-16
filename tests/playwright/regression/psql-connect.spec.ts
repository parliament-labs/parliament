import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\c`: a new session, the previous one kept until it's open —
// and kept for good if it can't be ("Previous connection kept") — and
// psql's words for both. A bare `\c` reconnects to the same database.

test('\\c switches database, and keeps the session it had when it fails', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const currentDatabase = async (name: string) => {
    await run('SELECT current_database();');
    await expect(out.locator('.shell-table').last().locator('td').first()).toHaveText(name, { timeout: 10_000 });
  };

  await run('\\c postgres');
  await expect(out).toContainText(/You are now connected to database "postgres" as user "[^"]+"\./, { timeout: 10_000 });
  // The command was taken: the input is empty again.
  await expect(input).toHaveValue('');
  await currentDatabase('postgres');

  // Inside a transaction: the new session has none.
  await run('BEGIN;');
  await expect(page.locator('.txn-badge.txn-active')).toBeVisible({ timeout: 8_000 });
  await run('\\c pagila');
  await expect(out).toContainText(/You are now connected to database "pagila" as user "[^"]+"\./, { timeout: 10_000 });
  await expect(page.locator('.txn-badge')).toHaveCount(0);

  await run('\\c parliament_no_such_db');
  await expect(out).toContainText(
    /connection to server at "[^"]+", port \d+ failed: FATAL: {2}database "parliament_no_such_db" does not exist/,
    { timeout: 10_000 },
  );
  await expect(out).toContainText('Previous connection kept');
  await currentDatabase('pagila');
});

// psql's `-reuse-previous=`, first: a Boolean as psql reads one — on is
// as without it — refused in psql's words otherwise, nothing connected
// and no word of the arguments after it (compared with psql's own
// output; skipped without psql on PATH).
test('\\c -reuse-previous reads its value as psql does', async ({ page }) => {
  const refused = spawnSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres'],
    { input: '\\c -reuse-previous=bogus pagila u h p extra\n', encoding: 'utf8' });
  test.skip(refused.error !== undefined, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const lastBlock = () => out.locator('.shell-block').last();

  await run('\\c -reuse-previous=bogus pagila u h p extra');
  await expect(lastBlock().locator('.shell-entry-error')).toHaveText(
    refused.stderr.replace(/^psql:<stdin>:\d+: (error: )?/gm, '').trim(), { timeout: 10_000 });
  await expect(lastBlock()).not.toContainText('extra argument');
  await expect(lastBlock()).not.toContainText('You are now connected');

  await run('\\c -reuse-previous=on pagila');
  await expect(lastBlock()).toContainText(/You are now connected to database "pagila" as user "[^"]+"\./, { timeout: 10_000 });
  // Off takes libpq's defaults for the rest: another connection, not here.
  await run('\\c -reuse-previous=off pagila');
  await expect(lastBlock().locator('.shell-entry-error')).toContainText("isn't supported yet", { timeout: 10_000 });
});
