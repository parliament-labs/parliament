import { test, expect, Page, Locator } from '@playwright/test';
import { execFileSync, spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\dn <pattern>` (`listSchemas`, PG 15+): after the list, a
// second query for the publications of the schema the pattern names —
// the pattern put in as typed — whose names print under the rows as its
// `Publications:` footer, in place of the row count: in the grid,
// expanded, unaligned, under `\pset footer off` too, never under `\t`;
// none found, the count as ever. A list is never timed. Each compared
// with psql's own output for the same commands (skipped without psql on
// PATH); the publications live in a transaction that's rolled back.

test.use({ permissions: ['clipboard-read', 'clipboard-write'] });

const PUBLICATIONS = [
  'create publication zz_dn_pub_b for tables in schema public;',
  'create publication zz_dn_pub_a for tables in schema public;',
];

// psql's output for `commands`, run with the publications in place.
function psql(commands: string[], args: string[] = []): string | null {
  const script = ['begin;', ...PUBLICATIONS, ...commands, 'rollback;'].join('\n') + '\n';
  try {
    return execFileSync('psql', ['-X', '-q', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', ...args],
      { input: script, stdio: ['pipe', 'pipe', 'pipe'] }).toString();
  } catch {
    return null;
  }
}

async function copyOutput(page: Page, block: Locator): Promise<string> {
  await block.click({ button: 'right' });
  await page.locator('.tab-context-menu-item', { hasText: 'Copy output' }).click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText()), { timeout: 5_000 }).not.toBe('');
  const text = await page.evaluate(() => navigator.clipboard.readText());
  await page.evaluate(() => navigator.clipboard.writeText(''));
  return text;
}

test('psql: \\dn <pattern> prints the schema\'s publications as psql does', async ({ page }) => {
  // psql first: its publications would wait on the shell's, uncommitted.
  const aligned = psql(['\\dn public']);
  test.skip(aligned === null, 'psql not on PATH');
  const footers = aligned!.slice(aligned!.indexOf('Publications:')).trimEnd();
  const unaligned = psql(['\\dn public'], ['-P', 'format=unaligned'])!.trimEnd();
  const expanded = psql(['\\dn public'], ['-P', 'expanded=on'])!.trimEnd();
  const echoed = psql(['\\set ECHO_HIDDEN on', '\\dn public'])!;
  const queries = echoed.split('/******** QUERY *********/\n').slice(1)
    .map(block => '/******** QUERY *********/\n' + block.slice(0, block.indexOf('/************************/') + '/************************/'.length));
  expect(footers).toBe('Publications:\n    "zz_dn_pub_a"\n    "zz_dn_pub_b"');
  expect(queries).toHaveLength(2);

  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const tables = out.locator('.shell-table');
  const lastBlock = () => out.locator('.shell-block').last();

  // A schema's publication takes a superuser (the fixture's postgres).
  await run('select rolsuper as su from pg_roles where rolname = current_user;');
  await expect(tables).toHaveCount(1, { timeout: 10_000 });
  test.skip(await tables.last().locator('td').first().innerText() !== 't', 'the connection\'s user is no superuser');

  await run('begin;');
  for (const sql of PUBLICATIONS) await run(sql);
  await expect(out.locator('.shell-entry-info', { hasText: 'CREATE PUBLICATION' })).toHaveCount(2, { timeout: 10_000 });

  // The grid: the rows, then psql's footers in place of "(1 row)".
  await run('\\dn public');
  await expect(tables).toHaveCount(2, { timeout: 10_000 });
  await expect(tables.last().locator('td')).toHaveText(['public', 'pg_database_owner']);
  await expect(lastBlock().locator('.shell-table-footers')).toHaveText(footers);
  await expect(lastBlock().locator('.shell-stat')).not.toContainText('row');
  expect(await copyOutput(page, lastBlock())).toBe(`List of schemas\nName\tOwner\npublic\tpg_database_owner\n${footers}`);

  // A pattern names no schema of that name: the count, as psql's.
  await run('\\dn pub*');
  await expect(tables).toHaveCount(3, { timeout: 10_000 });
  await expect(lastBlock().locator('.shell-table-footers')).toHaveCount(0);
  await expect(lastBlock().locator('.shell-stat')).toContainText('(1 row)');

  // `\pset footer off` leaves the count out, not the footers; nor is a
  // list timed.
  await run('\\pset footer off');
  await run('\\timing on');
  await run('\\dn public');
  await expect(tables).toHaveCount(4, { timeout: 10_000 });
  await expect(lastBlock().locator('.shell-table-footers')).toHaveText(footers);
  await expect(lastBlock().locator('.shell-timing')).toHaveCount(0);
  await run('\\timing off');
  await run('\\pset footer on');

  // `\t`: the rows alone.
  await run('\\t on');
  await run('\\dn public');
  await expect(tables).toHaveCount(5, { timeout: 10_000 });
  await expect(lastBlock().locator('.shell-table-footers')).toHaveCount(0);
  await run('\\t off');

  // Expanded and unaligned, as psql prints them.
  await run('\\x on');
  await run('\\dn public');
  const vertical = out.locator('.shell-vertical-text');
  await expect(vertical).toHaveCount(1, { timeout: 10_000 });
  expect((await vertical.last().textContent())!.trimEnd()).toBe(expanded);
  await run('\\x off');
  await run('\\pset format unaligned');
  await run('\\dn public');
  const printed = out.locator('.shell-printed');
  await expect(printed).toHaveCount(1, { timeout: 10_000 });
  expect((await printed.last().textContent())!.trimEnd()).toBe(unaligned);
  await run('\\pset format aligned');

  // ECHO_HIDDEN: both queries, as psql shows them, then the list.
  await run('\\set ECHO_HIDDEN on');
  await run('\\dn public');
  await expect(tables).toHaveCount(6, { timeout: 10_000 });
  const echo = lastBlock().locator('.shell-printed');
  await expect(echo).toHaveCount(2);
  expect([(await echo.nth(0).textContent())!.trimEnd(), (await echo.nth(1).textContent())!.trimEnd()]).toEqual(queries);
  await run('\\set ECHO_HIDDEN off');

  await run('rollback;');
  await expect(out.locator('.shell-entry-info', { hasText: 'ROLLBACK' })).toBeVisible({ timeout: 10_000 });
});

test('psql: a quote in \\dn\'s pattern breaks psql\'s second query, and the list with it', async ({ page }) => {
  const said = spawnSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres'], { input: "\\dn 'o''x'\n", encoding: 'utf8' });
  test.skip(said.error !== undefined, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  await input.fill("\\dn 'o''x'");
  await input.press('Enter');
  const error = out.locator('.shell-entry-error').last();
  await expect(error).toBeVisible({ timeout: 10_000 });
  // psql's error, without the `psql:<stdin>:1: ` its script input adds.
  const lines = (text: string) => text.split('\n').map(l => l.trimEnd()).join('\n').trim();
  expect(lines(await error.innerText())).toBe(lines(said.stderr.replace(/^psql:<stdin>:\d+: /gm, '')));
  expect(said.stdout).toBe('');
  await expect(out.locator('.shell-table')).toHaveCount(0);
  // The shell goes on.
  await input.fill('select 1 as ok;');
  await input.press('Enter');
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText(['1'], { timeout: 10_000 });
});
