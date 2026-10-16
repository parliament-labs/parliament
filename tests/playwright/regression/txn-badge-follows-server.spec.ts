import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput } from '../helpers/panes';

// The TXN badge (and psql's `%x`) follow the transaction status the
// server reports after every command (noctua's TransactionStatus), not
// a guess from the SQL typed. Cases the guess got wrong:
//   - Postgres: ROLLBACK TO SAVEPOINT leaves a failed transaction block
//     usable again (TXN FAILED → TXN); a meta-command whose catalog
//     query fails inside a transaction fails it, as in psql.
//   - MySQL: with autocommit off a write opens a transaction no BEGIN
//     announced; DDL commits implicitly — even DDL that then fails.
// Everything runs in a transaction that's rolled back, or on a
// temporary table.

test('the TXN badge and %x follow the server\'s transaction status', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const active = page.locator('.txn-badge.txn-active');
  const failed = page.locator('.txn-badge.txn-failed');
  const prompt = page.locator('.shell-prompt').last();

  await run('\\set PROMPT1 %x>');
  await run('BEGIN;');
  await expect(active).toBeVisible({ timeout: 8_000 });
  await expect(prompt).toHaveText('*>');

  await run('SAVEPOINT a;');
  await run('SELECT 1/0;');
  await expect(failed).toBeVisible({ timeout: 8_000 });
  await expect(prompt).toHaveText('!>');

  await run('ROLLBACK TO SAVEPOINT a;');
  await expect(active).toBeVisible({ timeout: 8_000 });
  await expect(failed).toHaveCount(0);
  await expect(prompt).toHaveText('*>');

  // `\d (film` is an invalid regular expression: psql's lookup fails.
  await run('\\d (film');
  await expect(failed).toBeVisible({ timeout: 8_000 });

  await run('ROLLBACK;');
  await expect(page.locator('.txn-badge')).toHaveCount(0, { timeout: 8_000 });
  await expect(prompt).toHaveText('>');
});

test('on MySQL the TXN badge follows the server too', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const active = page.locator('.txn-badge.txn-active');

  await run('CREATE TEMPORARY TABLE t (a int);');
  await run('BEGIN;');
  await expect(active).toBeVisible({ timeout: 8_000 });
  // `actor` exists: the CREATE fails, after committing implicitly.
  await run('CREATE TABLE actor (x int);');
  await expect(page.locator('.txn-badge')).toHaveCount(0, { timeout: 8_000 });

  await run('SET autocommit = 0;');
  await run('INSERT INTO t VALUES (1);');
  await expect(active).toBeVisible({ timeout: 8_000 });
  await run('ROLLBACK;');
  await expect(page.locator('.txn-badge')).toHaveCount(0, { timeout: 8_000 });
  await run('SET autocommit = 1;');
});
