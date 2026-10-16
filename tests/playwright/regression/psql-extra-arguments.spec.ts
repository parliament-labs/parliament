import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's word on the arguments a command didn't read (`HandleSlashCmds`):
// `\x: extra argument "extra" ignored`, one each, after the command's own
// output — none after a command that failed; for the `\g` family, before
// the query it sends. Checked against psql 18.4 under a pty.

test('psql: extra arguments are said where psql says them', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const errors = out.locator('.shell-entry-error');
  // What the shell printed, in order: messages, errors, a result's header.
  const printed = () => out.locator('.shell-meta-output, .shell-entry-error, .shell-table th, .shell-printed')
    .evaluateAll(els => els.map(e => (e as HTMLElement).innerText.trim()));

  await run('\\x on extra');
  await expect(errors.last()).toHaveText('\\x: extra argument "extra" ignored', { timeout: 10_000 });
  expect((await printed()).slice(-2)).toEqual(['Expanded display is on.', '\\x: extra argument "extra" ignored']);
  await run('\\x off a b');
  await expect(errors.last()).toHaveText('\\x: extra argument "b" ignored', { timeout: 10_000 });
  expect((await printed()).slice(-3)).toEqual(['Expanded display is off.', '\\x: extra argument "a" ignored', '\\x: extra argument "b" ignored']);

  // A command that failed says nothing more.
  await run('\\x maybe extra');
  await expect(errors.last()).toContainText('unrecognized value "maybe" for "expanded"', { timeout: 10_000 });
  await expect(errors.filter({ hasText: 'extra argument "extra"' })).toHaveCount(1);

  // A list: after its rows. A `\d` that found nothing failed: nothing.
  await run('\\dt actor extra');
  await expect(errors.last()).toHaveText('\\dt: extra argument "extra" ignored', { timeout: 10_000 });
  const listed = await printed();
  expect(listed.lastIndexOf('\\dt: extra argument "extra" ignored')).toBeGreaterThan(listed.lastIndexOf('Schema'));
  await run('\\d nosuch extra');
  await expect(out).toContainText('Did not find any relation named "nosuch".', { timeout: 10_000 });
  await run('\\echo done');
  await expect(out.locator('.shell-meta-output').last()).toHaveText('done', { timeout: 10_000 });
  await expect(errors.filter({ hasText: '\\d: extra argument' })).toHaveCount(0);

  // The `\g` family: before the result.
  await run('select 2 as b \\gdesc x');
  await expect(out.locator('.shell-table th', { hasText: /^Column$/ })).toBeVisible({ timeout: 10_000 });
  const lines = await printed();
  expect(lines.indexOf('\\gdesc: extra argument "x" ignored')).toBeLessThan(lines.lastIndexOf('Column'));

  // A cancelled `\prompt` failed: nothing more. Answered, it says it.
  await run("\\prompt 'Name?' v x");
  await expect(out).toContainText('Name?', { timeout: 10_000 });
  await input.press('Escape');
  await expect(out).toContainText('\\prompt v cancelled.', { timeout: 10_000 });
  await run('\\echo after');
  await expect(out.locator('.shell-meta-output').last()).toHaveText('after', { timeout: 10_000 });
  await expect(errors.filter({ hasText: '\\prompt: extra argument' })).toHaveCount(0);
  await run("\\prompt 'Name?' v x");
  await expect(out.locator('.shell-meta-output').last()).toContainText('Name?', { timeout: 10_000 });
  await run('answer');
  await expect(errors.last()).toHaveText('\\prompt: extra argument "x" ignored', { timeout: 10_000 });

  // A `\pset` error: nothing more.
  await run('\\pset bogus a b');
  await expect(errors.last()).toHaveText('\\pset: unknown option: bogus', { timeout: 10_000 });
  await expect(errors.filter({ hasText: '\\pset: extra argument' })).toHaveCount(0);

  // In a false branch nothing's said; `\endif` says it in the branch it
  // leads to.
  await run('\\if false');
  await run('\\x on extra2');
  await run('\\endif y');
  await expect(errors.last()).toHaveText('\\endif: extra argument "y" ignored', { timeout: 10_000 });
  await expect(errors.filter({ hasText: 'extra2' })).toHaveCount(0);
});
