import { test, expect, Page } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// SQL NULL shows as each CLI shows it: nothing in psql until `\pset null`
// says otherwise, `NULL` in the mysql CLI — and a text 'NULL' is text, not
// a NULL (noctua sends SQL NULL apart from it). `\pset null` with no value
// says what it is, in psql's words.

async function openShell(page: Page, connection: string) {
  await openApp(page);
  await clickConnection(page, connection);
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  return { out, run };
}

function psql(args: string[]): string | null {
  try {
    return execFileSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', ...args]).toString();
  } catch {
    return null;
  }
}

test('psql: NULL shows as nothing until \\pset null, apart from the text NULL', async ({ page }) => {
  const { out, run } = await openShell(page, 'Local Postgres');
  const said = out.locator('.shell-meta-output').last();
  const cells = () => out.locator('.shell-table').last().locator('td');
  const query = "select null::text as a, 'NULL'::text as b";

  await run(`${query};`);
  await expect(cells()).toHaveCount(2, { timeout: 10_000 });
  await expect(cells().nth(0)).toHaveText('');
  await expect(cells().nth(0)).toHaveClass(/shell-null/);
  await expect(cells().nth(1)).toHaveText('NULL');
  await expect(cells().nth(1)).not.toHaveClass(/shell-null/);

  await run('\\pset null');
  await expect(said).toHaveText('Null display is "".', { timeout: 10_000 });
  await run("\\pset null '(null)'");
  await expect(said).toHaveText('Null display is "(null)".', { timeout: 10_000 });
  await run(`${query};`);
  await expect(out.locator('.shell-table')).toHaveCount(2, { timeout: 10_000 });
  await expect(cells().nth(0)).toHaveText('(null)');
  await expect(cells().nth(1)).toHaveText('NULL');

  // As psql prints it — a list's NULLs too (but `\\du`'s, which psql
  // reads cell by cell).
  await run('\\pset format unaligned');
  for (const cmd of [query, '\\dt+ actor', '\\du+ postgres']) {
    const before = await out.locator('.shell-printed').count();
    await run(cmd.startsWith('\\') ? cmd : `${cmd};`);
    await expect(out.locator('.shell-printed')).toHaveCount(before + 1, { timeout: 10_000 });
    const ours = (await out.locator('.shell-printed').last().textContent())!.trimEnd();
    const theirs = psql(['-P', 'format=unaligned', '-P', 'null=(null)', '-c', cmd]);
    if (theirs !== null) expect(ours, cmd).toBe(theirs.trimEnd());
  }
  await expect(out.locator('.shell-printed').nth(-2)).toContainText('|(null)');
  await run('\\pset format aligned');

  // \gset: a NULL unsets, the text 'NULL' sets — `:x` stays as typed.
  await run("select null as x, 'NULL' as y \\gset");
  await run('\\echo :x :y');
  await expect(said).toHaveText(':x NULL', { timeout: 10_000 });
});

test('mysql: NULL shows as NULL, apart from the text NULL', async ({ page }) => {
  const { out, run } = await openShell(page, 'Local MySQL');
  const cells = out.locator('.shell-table').last().locator('td');
  await run("select null as a, 'NULL' as b;");
  await expect(cells).toHaveCount(2, { timeout: 10_000 });
  await expect(cells.nth(0)).toHaveText('NULL');
  await expect(cells.nth(0)).toHaveClass(/shell-null/);
  await expect(cells.nth(1)).toHaveText('NULL');
  await expect(cells.nth(1)).not.toHaveClass(/shell-null/);
});
