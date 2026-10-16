import { test, expect, Page } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// Vertical output prints as each CLI prints it (`DbmsPlane::print_vertical`):
// psql's expanded records (`\x`, `\gx` — `-[ RECORD 1 ]-` over
// `header | value` lines, the title over them, `\t` leaving the labels
// out, NULLs as `\pset null`), the mysql CLI's `\G` rows (`*** 1. row
// ***`, names right-justified). The footer and time follow as before.
// Compared with the CLI's own output for the same query (skipped without
// it on PATH).

function cli(command: string, args: string[]): string | null {
  try {
    return execFileSync(command, args).toString();
  } catch {
    return null;
  }
}

async function openShell(page: Page, connection: string) {
  await openApp(page);
  await clickConnection(page, connection);
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const text = () => out.locator('.shell-vertical-block').last().locator('.shell-vertical-text');
  return { out, run, text };
}

const PG_QUERY = "select actor_id, first_name, null::text as nothing, E'a\\nb' as two_lines from actor where actor_id <= 2 order by actor_id";
const MY_QUERY = "select actor_id, first_name, null as nothing, 'a\\nb' as two_lines, 'é' as `é` from actor where actor_id <= 2 order by actor_id";

test('psql: expanded output is psql\'s', async ({ page }) => {
  const { out, run, text } = await openShell(page, 'Local Postgres');
  const psql = (...args: string[]) => cli('psql', ['-X', '-q', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', ...args]);
  const blocks = out.locator('.shell-vertical-block');

  await run('\\x on');
  await run(`${PG_QUERY};`);
  await expect(text()).toContainText('-[ RECORD 1 ]', { timeout: 10_000 });
  const expanded = psql('-P', 'expanded=on', '-c', PG_QUERY);
  test.skip(expanded === null, 'psql not on PATH');
  expect((await text().textContent())!.trimEnd()).toBe(expanded!.trimEnd());

  // The title over the records, NULLs as `\pset null` says.
  await run("\\pset title 'Two actors'");
  await run("\\pset null '(null)'");
  await run(`${PG_QUERY};`);
  await expect(blocks).toHaveCount(2, { timeout: 10_000 });
  expect((await text().textContent())!.trimEnd())
    .toBe(psql('-P', 'expanded=on', '-P', 'null=(null)', '-P', 'title=Two actors', '-c', PG_QUERY)!.trimEnd());

  // `\t`: no title, no labels, a rule between records — and no footer.
  await run('\\t');
  await run(`${PG_QUERY};`);
  await expect(blocks).toHaveCount(3, { timeout: 10_000 });
  expect((await text().textContent())!.trimEnd())
    .toBe(psql('-P', 'expanded=on', '-P', 'null=(null)', '-P', 'tuples_only=on', '-c', PG_QUERY)!.trimEnd());
  await run('\\t');

  // No rows: psql's count alone.
  await run('select 1 as x where false;');
  await expect(blocks).toHaveCount(4, { timeout: 10_000 });
  await expect(blocks.last().locator('.shell-vertical-text')).toHaveCount(0);
  await expect(blocks.last().locator('.shell-stat')).toContainText('(0 rows)');
});

test('mysql: \\G output is the mysql CLI\'s', async ({ page }) => {
  const { out, run, text } = await openShell(page, 'Local MySQL');
  const blocks = out.locator('.shell-vertical-block');

  // A value ending in a line break leaves the CLI's blank line after it:
  // as tall as one with a second line.
  const height = async (query: string) => {
    const before = await blocks.count();
    await run(query);
    await expect(blocks).toHaveCount(before + 1, { timeout: 10_000 });
    return text().evaluate(el => el.getBoundingClientRect().height);
  };
  expect(await height("select 'a\\n' as x\\G")).toBe(await height("select 'a\\nb' as x\\G"));

  await run(`${MY_QUERY}\\G`);
  await expect(blocks).toHaveCount(3, { timeout: 10_000 });
  await expect(text()).toContainText('*************************** 1. row ***************************');
  const theirs = cli('mysql', ['-h', '127.0.0.1', '-u', 'root', 'sakila', '-E', '-e', MY_QUERY]);
  test.skip(theirs === null, 'mysql not on PATH');
  expect((await text().textContent())!.trimEnd()).toBe(theirs!.trimEnd());
});
