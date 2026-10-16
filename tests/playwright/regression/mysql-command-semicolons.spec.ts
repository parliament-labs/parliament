import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// A `;` after a mysql command, as the mysql CLI (9.6) reads it. A command
// without arguments is `\` and its letter, and the `;` after it ends what
// the buffer holds: nothing for `\s;` — the status, then `ERROR: ` / `No
// query specified` — and `select 2 as b` for `select 2 as b \s;`. A
// command with arguments reads them from its line and ends at the
// delimiter after them: `\u sakila;` asks for database `sakila;`, and
// `\u mysql ; select …;` changes database, then runs the select.

test('mysql: a ; after a command reads on, as the mysql CLI reads it', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const noQuery = out.locator('.shell-entry-error', { hasText: 'No query specified' });
  const status = out.locator('.shell-kv-block');

  // `\s;`: the status, then the empty statement, in that order.
  await run('\\s;');
  await expect(noQuery).toHaveCount(1, { timeout: 10_000 });
  await expect(noQuery).toHaveText('ERROR: \nNo query specified');
  await expect(status).toHaveCount(1);
  const order = await out.locator('.shell-kv-block, .shell-entry-error')
    .evaluateAll(els => els.map(e => e.classList.contains('shell-kv-block') ? 'status' : 'error'));
  expect(order).toEqual(['status', 'error']);
  await expect(input).toHaveValue('');

  await run('\\c;');
  await expect(noQuery).toHaveCount(2, { timeout: 10_000 });

  // The `;` ends what the buffer holds.
  await run('select 2 as b \\s;');
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toBeVisible({ timeout: 10_000 });
  await expect(status).toHaveCount(2);
  await expect(noQuery).toHaveCount(2);

  // Arguments end at the delimiter; the line reads on after it.
  await run('\\u mysql ; select database() as d;');
  await expect(out.locator('.shell-table td', { hasText: /^mysql$/ })).toBeVisible({ timeout: 10_000 });
  // A `;` in the word is the name's.
  await run('\\u sakila;');
  await expect(out.locator('.shell-entry-error').last())
    .toHaveText("ERROR 1049 (42000): Unknown database 'sakila;'", { timeout: 10_000 });
  await run('\\u sakila');
  await expect(out.locator('.shell-meta-output', { hasText: /^Database changed$/ })).toHaveCount(2, { timeout: 10_000 });
});
