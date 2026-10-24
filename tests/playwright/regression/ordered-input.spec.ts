import { test, expect, type Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// Each CLI reads its input in order: the statements ended before a
// command run first, then the command, then what follows it — each
// waiting for what's before it to finish (`shell::process_input`,
// `resume_input`). Input entered meanwhile reads on after it, as
// type-ahead does.

async function openShell(page: Page, connection: string) {
  await openApp(page);
  await clickConnection(page, connection);
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  return input;
}

test('psql: statements, then the command, then what follows it', async ({ page }) => {
  const input = await openShell(page, 'Local Postgres');
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const order = (selector: string) => out.locator(selector).allInnerTexts();
  const said = out.locator('.shell-meta-output');

  // `select 1` runs before `\x` turns expanded output on; `select 2`
  // is the buffer `\x` leaves.
  await run('select 1 as a; select 2 as b \\x');
  await expect(said.last()).toHaveText('Expanded display is on.', { timeout: 10_000 });
  await expect(input).toHaveValue('select 2 as b\n');
  expect((await order('.shell-table th, .shell-meta-output')).slice(-2)).toEqual(['a', 'Expanded display is on.']);

  // A paste that starts with a command: it, then the lines below.
  await run('\\x\nselect 3 as c;');
  await expect(out.locator('.shell-table th', { hasText: /^c$/ })).toBeVisible({ timeout: 10_000 });
  await expect(input).toHaveValue('');
  expect((await order('.shell-table th, .shell-meta-output')).slice(-2)).toEqual(['Expanded display is off.', 'c']);

  // A command that waits for its reply: what follows waits for it.
  await run('\\d actor_info\nselect 4 as d;');
  await expect(out.locator('.shell-table th', { hasText: /^d$/ })).toBeVisible({ timeout: 10_000 });
  const printed = (await order('.shell-printed, .shell-table th')).slice(-2);
  expect(printed[0]).toContain('View "public.actor_info"');
  expect(printed[1]).toBe('d');

  // `\set`, then a statement that uses it.
  await run('\\set v 7\nselect :v as e;');
  await expect(out.locator('.shell-table td', { hasText: /^7$/ })).toBeVisible({ timeout: 10_000 });

  // Type-ahead: entered while `\echo done` waits for the sleep, it
  // runs after that.
  await run('select pg_sleep(1) as z; \\echo done');
  await run('select 8 as y;');
  await expect(out.locator('.shell-table th', { hasText: /^y$/ })).toBeVisible({ timeout: 10_000 });
  expect((await order('.shell-table th, .shell-meta-output')).slice(-3)).toEqual(['z', 'done', 'y']);

  // Typed but not entered while it waits: it stays in the input.
  await run('select pg_sleep(1) as w; \\echo later');
  await input.fill('select 9');
  await expect(said.last()).toHaveText('later', { timeout: 10_000 });
  await expect(input).toHaveValue('select 9');
});

test('mysql: a pasted script reads in order — DELIMITER, use, statements', async ({ page }) => {
  const input = await openShell(page, 'Local MySQL');
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const order = (selector: string) => out.locator(selector).allInnerTexts();

  // DELIMITER lines are commands where they stand: the statement between
  // them ends at `//`, the last at `;` again.
  await run('DELIMITER //\nSELECT 1 AS p //\nDELIMITER ;\nSELECT 2 AS q;');
  await expect(out.locator('.shell-table th', { hasText: /^q$/ })).toBeVisible({ timeout: 10_000 });
  expect((await order('.shell-table th')).slice(-2)).toEqual(['p', 'q']);
  await run('select 3 as r;');
  await expect(out.locator('.shell-table th', { hasText: /^r$/ })).toBeVisible({ timeout: 10_000 });

  // `use` (named) and `\u`: the statements after them run on the new
  // database.
  await run('use mysql\nselect database() as d1;');
  await expect(out.locator('.shell-table td', { hasText: /^mysql$/ })).toBeVisible({ timeout: 10_000 });
  await run('\\u sakila\nselect database() as d2;');
  await expect(out.locator('.shell-table td', { hasText: /^sakila$/ })).toBeVisible({ timeout: 10_000 });

  // \s in mid-statement: its status, then the statement, finished by the
  // rest of the line.
  await run('select 6 as s \\s from dual;');
  await expect(out.locator('.shell-table th', { hasText: /^s$/ })).toBeVisible({ timeout: 10_000 });
  const kv = (await order('.shell-kv-block, .shell-table th')).slice(-2);
  expect(kv[1]).toBe('s');
  await expect(input).toHaveValue('');
});
