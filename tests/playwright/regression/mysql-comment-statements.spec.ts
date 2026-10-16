import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// The mysql CLI (`--comments`, on by default) sends comments to the
// server: a line comment starting a line with nothing pending is a
// statement of its own, sent as the line ends; one after a statement on
// its line goes with that statement; a statement of block comments goes
// at its `;`. The server answers each comment-only one "Query OK".
// psql sends one its `;` ends too; Postgres answers with nothing (see
// empty-statements.spec.ts).

const QUERY_OK = /^Query OK, 0 rows affected \(\d+\.\d{3} sec\)$/;

test('mysql: comments go to the server where the mysql CLI sends them', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const ok = out.locator('.shell-entry-info', { hasText: QUERY_OK });

  await run('-- a note');
  await expect(ok).toHaveCount(1, { timeout: 10_000 });
  await expect(input).toHaveValue('');

  await run('/* only */;');
  await expect(ok).toHaveCount(2, { timeout: 10_000 });

  // After a statement on its line: that statement's — no Query OK of its own.
  await run('select 1 as a; # why');
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toBeVisible({ timeout: 10_000 });
  await expect(out.locator('.shell-entry-query', { hasText: 'select 1 as a; # why' })).toHaveCount(1);
  await expect(ok).toHaveCount(2);

  // A script's header comments: one Query OK each, then the result.
  await run('-- header\n-- more\nselect 2 as b;');
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toBeVisible({ timeout: 10_000 });
  await expect(ok).toHaveCount(4);

  // A comment line ahead of an unfinished statement waits with it.
  await run('-- note\nselect a,');
  await expect(input).toHaveValue('-- note\nselect a,\n');
  await expect(ok).toHaveCount(4);
  await input.fill('');

  // Indented, a comment waits for the statement it's part of.
  await run('   -- indented');
  await expect(input).toHaveValue('   -- indented\n');
  await input.press('End');
  await input.type(';');
  await input.press('Enter');
  await expect(ok).toHaveCount(5, { timeout: 10_000 });

  // A `;` on the line after a comment comes back from history with it.
  await run('select 3 as c -- why\n;');
  await expect(out.locator('.shell-table th', { hasText: /^c$/ })).toHaveCount(1, { timeout: 10_000 });
  await input.press('ArrowUp');
  await expect(input).toHaveValue('select 3 as c -- why\n;');
  await input.press('Enter');
  await expect(out.locator('.shell-table th', { hasText: /^c$/ })).toHaveCount(2, { timeout: 10_000 });
});

test('psql: a statement of comments only goes to the server, which answers with nothing', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  await input.fill('/* only */;');
  await input.press('Enter');
  await expect(input).toHaveValue('');
  // In the scrollback as typed; nothing under it.
  await expect(out.locator('.shell-entry-query', { hasText: 'only' })).toHaveCount(1, { timeout: 10_000 });
  await input.fill('\\echo settled');
  await input.press('Enter');
  await expect(out.locator('.shell-meta-line').last()).toHaveText('settled', { timeout: 10_000 });
  const blocks = out.locator('.shell-block');
  await expect(blocks.nth(await blocks.count() - 2).locator('.shell-entry-info, .shell-entry-error, .shell-meta-output, .shell-table')).toHaveCount(0);
  await expect(out).not.toContainText('Query OK');
});
