import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\s` (psql 18's `printHistory`): the history — here what Up-arrow
// walks — each entry as typed, oldest first, the `\s` itself last, then a
// blank line. Checked against psql 18.4 under a pty with the same input.
// The saved history is read once, as psql reads its history file at
// startup: a `\c` doesn't list this session's commands twice.

test('psql: \\s prints the history as typed', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output');
  const printed = async () => (await said.last().innerText()).replace(/\n+$/, '').split('\n');
  const typed = ['select 1 as a;', '\\x', 'select 2', 'as b;'];

  await run('select 1 as a;');
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toBeVisible({ timeout: 10_000 });
  await run('\\x');
  await expect(said.last()).toHaveText('Expanded display is on.', { timeout: 10_000 });
  await run('select 2\nas b;');
  await expect(out.locator('.shell-vertical-block')).toHaveCount(1, { timeout: 10_000 });

  await run('\\s');
  await expect.poll(printed, { timeout: 10_000 }).toEqual([...typed, '\\s']);
  await run('\\s');
  await expect.poll(printed, { timeout: 10_000 }).toEqual([...typed, '\\s', '\\s']);

  // A new session (`\c`) doesn't read the saved history again.
  await run('\\c');
  await expect(out).toContainText('You are now connected to database', { timeout: 15_000 });
  await run('\\s');
  await expect.poll(printed, { timeout: 10_000 }).toEqual([...typed, '\\s', '\\s', '\\c', '\\s']);

  // After SQL it leaves the buffer, as psql's does.
  await run('select 3 as c \\s');
  await expect(input).toHaveValue('select 3 as c\n', { timeout: 10_000 });
  await expect.poll(printed, { timeout: 10_000 }).toEqual([...typed, '\\s', '\\s', '\\c', '\\s', '\\s']);

  // To a file it would need the host.
  await run('\\s history.txt');
  await expect(out.locator('.shell-entry-error').last()).toContainText("\\s: writing the history to a file isn't supported yet.", { timeout: 10_000 });
});
