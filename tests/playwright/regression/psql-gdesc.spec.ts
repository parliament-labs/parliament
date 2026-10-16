import { test, expect, Page } from '@playwright/test';
import { execFileSync, spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\gdesc` (`DescribeQuery`): the statement before it is parsed
// and described, never run, and its columns' names and types — named by
// the server (`format_type`, with each column's type modifier:
// `character varying(30)`) — print as any result does (`\x`, the row
// count, `\timing`). A statement without columns says so in psql's
// words. Compared with psql's own output for the same statement
// (skipped without psql on PATH).

function psql(input: string, args: string[] = []): string | null {
  try {
    return execFileSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', ...args], { input }).toString();
  } catch {
    return null;
  }
}

async function openShell(page: Page) {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  return { out, run };
}

const TYPES = "select film_id, title, 'x'::varchar(30) as v, 1.5::numeric(5,2) as n, now() as t, " +
              "array[1]::int[] as arr, null as u, rating from film";

test('\\gdesc: names and types as psql describes them', async ({ page }) => {
  test.skip(psql('select 1;') === null, 'psql not on PATH');
  const { out, run } = await openShell(page);
  const tables = out.locator('.shell-table');
  const cells = async () => tables.last().locator('td').allInnerTexts();
  // psql's rows, unaligned: `name|type` a line.
  const theirs = (sql: string) => psql(`${sql} \\gdesc\n`, ['-At'])!.trimEnd().split('\n').flatMap(l => l.split('|'));

  await run(`${TYPES} \\gdesc`);
  await expect(tables).toHaveCount(1, { timeout: 10_000 });
  await expect(tables.last().locator('th')).toHaveText(['Column', 'Type']);
  expect(await cells()).toEqual(theirs(TYPES));
  await expect(out.locator('.shell-stat').last()).toContainText('(8 rows)');

  // Expanded, as psql prints it under `\x`.
  await run('\\x on');
  await run(`${TYPES} \\gdesc`);
  const vertical = out.locator('.shell-vertical-block').last().locator('.shell-vertical-text');
  await expect(vertical).toBeVisible({ timeout: 10_000 });
  expect((await vertical.textContent())!.trimEnd()).toBe(psql(`${TYPES} \\gdesc\n`, ['-P', 'expanded=on'])!.trimEnd());
  await run('\\x off');

  // Timed as psql times it: the describe and the names, together.
  await run('\\timing on');
  await run(`${TYPES} \\gdesc`);
  await expect(tables).toHaveCount(2, { timeout: 10_000 });
  await expect(out.locator('.shell-block').last()).toContainText(/Time: \d+\.\d{3} ms/);
});

test('\\gdesc: described, never run', async ({ page }) => {
  test.skip(psql('select 1;') === null, 'psql not on PATH');
  const { out, run } = await openShell(page);
  await run('create temp table gd (a int);');
  await expect(out.locator('.shell-entry-info', { hasText: 'CREATE TABLE' })).toBeVisible({ timeout: 10_000 });
  // RETURNING describes its columns; the row isn't inserted.
  await run('insert into gd values (1) returning a \\gdesc');
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText(['a', 'integer'], { timeout: 10_000 });
  // No columns: psql's word for it.
  await run('insert into gd values (2) \\gdesc');
  await expect(out.locator('.shell-meta-output').last())
    .toHaveText('The command has no result, or the result has no columns.', { timeout: 10_000 });
  await run('select count(*) as n from gd;');
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText(['0'], { timeout: 10_000 });
  // A prepared statement isn't left behind, as psql's unnamed one isn't.
  await run('select count(*) as p from pg_prepared_statements;');
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText(['0'], { timeout: 10_000 });
});

test('\\gdesc: an empty buffer describes the previous query', async ({ page }) => {
  const { out, run } = await openShell(page);
  await run('select actor_id, first_name from actor limit 1;');
  await expect(out.locator('.shell-table')).toHaveCount(1, { timeout: 10_000 });
  await run('\\gdesc');
  await expect(out.locator('.shell-table')).toHaveCount(2, { timeout: 10_000 });
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText(['actor_id', 'integer', 'first_name', 'text']);
});

test('\\gdesc: a statement the server refuses prints its error', async ({ page }) => {
  test.skip(psql('select 1;') === null, 'psql not on PATH');
  const { out, run } = await openShell(page);
  await run('select * frmo film \\gdesc');
  const error = out.locator('.shell-entry-error').last();
  await expect(error).toBeVisible({ timeout: 10_000 });
  // psql's error, without the `psql:<stdin>:1: ` its script input adds.
  const said = spawnSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres'], { input: 'select * frmo film \\gdesc\n', encoding: 'utf8' })
    .stderr.replace(/^psql:<stdin>:\d+: /gm, '');
  const lines = (text: string) => text.split('\n').map(l => l.trimEnd()).join('\n').trim();
  expect(lines((await error.innerText()))).toBe(lines(said));
  await expect(out.locator('.shell-table')).toHaveCount(0);
  // The session goes on.
  await run('select 1 as ok;');
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText(['1'], { timeout: 10_000 });
});
