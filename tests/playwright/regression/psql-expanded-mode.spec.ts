import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\x [on|off|auto]` / `\pset expanded`: a value sets the mode, no
// value toggles (auto goes off), anything else is psql's error. Under
// `auto` a result shows vertically only when its aligned table is wider
// than the pane — the pane playing psql's terminal — and has more than one
// column, which would only be wider expanded (psql's print_aligned_text).

test('psql: \\x on, off, auto and toggle, as psql sets them', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output');
  const vertical = out.locator('.shell-vertical-block');
  const tables = out.locator('.shell-table');

  await run('\\x on');
  await expect(said.last()).toHaveText('Expanded display is on.', { timeout: 10_000 });
  await run('select 1 as a;');
  await expect(vertical).toHaveCount(1, { timeout: 10_000 });

  // `on` again stays on (it doesn't toggle).
  await run('\\x on');
  await expect(said.last()).toHaveText('Expanded display is on.');
  await run('\\pset expanded off');
  await expect(said.last()).toHaveText('Expanded display is off.', { timeout: 10_000 });
  await run('select 2 as b;');
  await expect(tables.filter({ hasText: 'b' })).toHaveCount(1, { timeout: 10_000 });

  // auto: narrow stays a table, wider than the pane goes vertical.
  await run('\\x auto');
  await expect(said.last()).toHaveText('Expanded display is used automatically.', { timeout: 10_000 });
  await run('select 3 as c;');
  await expect(tables.filter({ hasText: 'c' })).toHaveCount(1, { timeout: 10_000 });
  await run("select 1 as n, repeat('x', 400) as wide;");
  await expect(vertical).toHaveCount(2, { timeout: 10_000 });
  await expect(vertical.last()).toContainText('wide');
  // A single column stays a table, however wide.
  await run("select repeat('z', 400) as single;");
  await expect(tables.filter({ hasText: 'single' })).toHaveCount(1, { timeout: 10_000 });
  await expect(vertical).toHaveCount(2);
  // The format doesn't change the mode, as in psql.
  await run('\\pset format aligned');
  await run("select 2 as n, repeat('y', 400) as wider;");
  await expect(vertical).toHaveCount(3, { timeout: 10_000 });

  // No value toggles; from auto, off.
  await run('\\x');
  await expect(said.last()).toHaveText('Expanded display is off.', { timeout: 10_000 });

  await run('\\x maybe');
  await expect(out.locator('.shell-entry-error').last()).toHaveText('unrecognized value "maybe" for "expanded"\nAvailable values are: on, off, auto.', { timeout: 10_000 });
});
