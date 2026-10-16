import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's variables, expanded where psql expands them: in a backslash
// command's arguments — a value stays one argument, spaces and all — and
// in a statement, except inside quotes, comments and `::` casts. `\set`
// says nothing. The mysql CLI has no `:var`s: a MySQL statement goes as
// typed.

test('psql: variables in command arguments and statements', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = () => out.locator('.shell-meta-output').last();

  await run("\\set x 'hello  world'");
  await expect(input).toHaveValue('');
  await expect(out.locator('.shell-meta-output')).toHaveCount(0);

  await run('\\echo :x');
  await expect(said()).toHaveText('hello  world', { timeout: 5_000 });
  await run("\\echo :'x' ':x' pre:x :nosuch :{?x}");
  await expect(said()).toHaveText("'hello  world' :x prehello  world :nosuch TRUE");
  await run('\\set z a b c');
  await run('\\echo :z');
  await expect(said()).toHaveText('abc');

  // A pattern from a variable.
  await run('\\set t film');
  await run('\\dt :t');
  await expect(out.locator('.shell-table').last()).toContainText('film', { timeout: 10_000 });

  // A statement: not in a string or a cast.
  await run('\\set int 5');
  await run("select 1::int as b, 'a:x' as c, :'x' as d;");
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText(['1', 'a:x', 'hello  world'], { timeout: 10_000 });

  // psql's error for a name it won't take.
  await run('\\set a-b v');
  await expect(said()).toHaveText('invalid variable name: "a-b"');
});

test('mysql: a statement goes as typed — no :var expansion', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  await input.fill("select 'a:USER' as s;");
  await input.press('Enter');
  await expect(visibleShellOutput(page).locator('.shell-table').last().locator('td')).toHaveText('a:USER', { timeout: 10_000 });
});
