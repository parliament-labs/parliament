import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\if` / `\elif` / `\else` / `\endif`, as psql 18 runs them: a
// block spans as many Enters as it takes (the shell keeps psql's
// conditional stack); each branching command acts where it stands in the
// input. In an inactive branch nothing runs — each statement is "query
// ignored", each other command "command ignored". `%R` reads `@` there;
// Ctrl+C leaves the innermost block (`\if: escaped`).

const IGNORED = 'query ignored; use \\endif or Ctrl-C to exit current \\if block';

async function openShell(page) {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  return input;
}

test('a pasted block runs the branch it takes; the others are ignored', async ({ page }) => {
  const input = await openShell(page);
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const tables = out.locator('.shell-table-wrap');

  await run("\\if false\nSELECT 'first' AS branch;\n\\elif true\nSELECT 'second' AS branch;\n\\else\nSELECT 'third' AS branch;\n\\endif");
  await expect(tables.filter({ hasText: 'second' })).toHaveCount(1, { timeout: 10_000 });
  await expect(tables.filter({ hasText: 'first' })).toHaveCount(0);
  await expect(tables.filter({ hasText: 'third' })).toHaveCount(0);
  await expect(out.locator('.shell-entry-error', { hasText: IGNORED })).toHaveCount(2);
  await expect(input).toHaveValue('');

  // A variable drives the expression.
  await run('\\set runit on');
  await run("\\if :runit\nSELECT 'enabled' AS state;\n\\else\nSELECT 'disabled' AS state;\n\\endif");
  await expect(tables.filter({ hasText: 'enabled' })).toHaveCount(1, { timeout: 10_000 });
  await expect(tables.filter({ hasText: 'disabled' })).toHaveCount(0);
});

test('a block spans Enters, as in psql', async ({ page }) => {
  const input = await openShell(page);
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const errors = out.locator('.shell-entry-error');

  await run('\\set PROMPT1 %R>');
  await run('\\if true');
  await expect(input).toHaveValue('');
  await run('select 1 as a;');
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toBeVisible({ timeout: 10_000 });
  await run('\\else');
  // `%R` reads `@` in the inactive branch.
  await expect(out.locator('.shell-prompt').last()).toHaveText('@>');
  await run('select 2 as b;');
  await expect(errors.last()).toHaveText(IGNORED, { timeout: 10_000 });
  await run('\\echo hidden');
  await expect(errors.last()).toHaveText('\\echo command ignored; use \\endif or Ctrl-C to exit current \\if block');
  await run('\\endif');
  await expect(out.locator('.shell-prompt').last()).toHaveText('=>');
  await run('select 3 as c;');
  await expect(out.locator('.shell-table th', { hasText: /^c$/ })).toBeVisible({ timeout: 10_000 });
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toHaveCount(0);
  await expect(out).not.toContainText(/^hidden$/m);
});

test('the query buffer keeps what active branches add', async ({ page }) => {
  const input = await openShell(page);
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };

  // psql: `select 1 as a` / `\if false` / `, 2 as b` / `\else` / `, 3 as c`
  // / `\endif` / `;` runs `select 1 as a , 3 as c`.
  await run('select 1 as a\n\\if false\n, 2 as b\n\\else\n, 3 as c\n\\endif\n;');
  await expect(out.locator('.shell-table th', { hasText: /^c$/ })).toBeVisible({ timeout: 10_000 });
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toBeVisible();
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toHaveCount(0);
});

test("misplaced commands and bad expressions say what psql says; Ctrl+C escapes", async ({ page }) => {
  const input = await openShell(page);
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const errors = out.locator('.shell-entry-error');

  await run('\\endif');
  await expect(errors.last()).toHaveText('\\endif: no matching \\if', { timeout: 10_000 });
  await run('\\else');
  await expect(errors.last()).toHaveText('\\else: no matching \\if');

  // A bad expression is said, and the branch is false.
  await run('\\if maybe');
  await expect(errors.last()).toHaveText('unrecognized value "maybe" for "\\if expression": Boolean expected');
  await run('select 1 as a;');
  await expect(errors.last()).toHaveText(IGNORED, { timeout: 10_000 });
  await run('\\else');
  await run('\\elif true');
  await expect(errors.last()).toHaveText('\\elif: cannot occur after \\else');

  // Ctrl+C at the prompt leaves the innermost block.
  await input.press('Control+c');
  await expect(errors.last()).toHaveText('\\if: escaped');
  await run('select 4 as d;');
  await expect(out.locator('.shell-table th', { hasText: /^d$/ })).toBeVisible({ timeout: 10_000 });
});
