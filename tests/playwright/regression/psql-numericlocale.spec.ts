import { test, expect } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\pset numericlocale`: numbers — the cells of number-typed
// columns — in the client's locale, as psql's printQuery writes them, in
// every format and in a sequence's `\d`; `\crosstabview`'s grid as the
// values are. The client's locale is noctua's environment's, as psql's
// is its own: compared with psql run in this test's environment, which
// noctua shares (skipped without psql on PATH).

const QUERY = "select 1234567.891::numeric as n, -1234567::int as i, 1e10::float8 as f, 1234567::money as m, null::int as z, '1234'::text as t";

function psql(args: string[], input?: string): string | null {
  try {
    return execFileSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', ...args], { input }).toString();
  } catch {
    return null;
  }
}

test('psql: \\pset numericlocale writes numbers as psql does', async ({ page }) => {
  const plain = psql(['-At', '-P', 'numericlocale=on', '-c', QUERY]);
  test.skip(plain === null, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output').last();
  const tables = out.locator('.shell-table');
  const printed = out.locator('.shell-printed');
  const cells = (table: number) => tables.nth(table).locator('td').allInnerTexts();

  // Set silently, as psql sets it.
  await run('\\pset numericlocale on');
  await run('\\echo set');
  await expect(said).toHaveText('set', { timeout: 10_000 });
  await expect(out.locator('.shell-meta-output', { hasText: 'Locale-adjusted' })).toHaveCount(0);

  // The grid: number columns in the locale, the rest as they are.
  await run(`${QUERY};`);
  await expect(tables).toHaveCount(1, { timeout: 10_000 });
  expect(await cells(0)).toEqual(plain!.replace(/\n$/, '').split('|'));

  // Expanded, and csv.
  await run('\\x on');
  await run(`${QUERY};`);
  const vertical = out.locator('.shell-vertical-block').last().locator('.shell-vertical-text');
  await expect(vertical).toBeVisible({ timeout: 10_000 });
  expect((await vertical.textContent())!.trimEnd())
    .toBe(psql(['-P', 'numericlocale=on', '-P', 'expanded=on', '-c', QUERY])!.trimEnd());
  await run('\\x off');
  await run('\\pset format csv');
  await run(`${QUERY};`);
  await expect(printed).toHaveCount(1, { timeout: 10_000 });
  expect((await printed.last().textContent())!.trimEnd())
    .toBe(psql(['-P', 'numericlocale=on', '-P', 'format=csv', '-c', QUERY])!.trimEnd());
  await run('\\pset format aligned');

  // A sequence's `\d`, which psql prints as a result.
  await run('\\d actor_actor_id_seq');
  await expect(printed).toHaveCount(2, { timeout: 10_000 });
  expect((await printed.last().textContent())!.trimEnd())
    .toBe(psql(['-P', 'numericlocale=on', '-c', '\\d actor_actor_id_seq'])!.trimEnd());

  // `\crosstabview` prints its values as they are.
  const crosstab = "select 'r' as v, 'c' as h, 1234567 as d \\crosstabview";
  await run(crosstab);
  await expect(tables).toHaveCount(2, { timeout: 10_000 });
  const theirs = psql(['-At', '-P', 'numericlocale=on'], `${crosstab}\n`)!;
  expect(await cells(1)).toEqual(theirs.replace(/\n$/, '').split('|'));

  // Off again: what printed keeps its numbers.
  await run('\\pset numericlocale');
  await expect(said).toHaveText('Locale-adjusted numeric output is off.', { timeout: 10_000 });
  expect(await cells(0)).toEqual(plain!.replace(/\n$/, '').split('|'));
});
