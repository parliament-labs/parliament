import { test, expect, Page } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// Each CLI lines its table's columns up by type — numbers right, the rest
// left (psql's `column_type_alignment`, by the column's OID; the mysql
// CLI's `IS_NUM`) — and psql centres every header where the mysql CLI
// starts them at the left. The grid does the same: each column is checked
// against how the CLI itself printed the same query (skipped without the
// CLI on PATH).

// Every value narrower than its header, so each cell shows its padding.
const PG_QUERY = "select null::int as null_column, 1 as int_column, 'x' as text_column, 1.5 as numeric_column, "
  + "2::money as money_column, '1'::xid as xid_column, 2010::year as year_column, true as bool_column, "
  + "date '2026-01-02' as the_date_column, array[1] as array_column";
const MY_QUERY = "select null as null_column, 1 as int_column, 'x' as text_column, 1.5 as decimal_column, "
  + "cast('2026-01-02' as date) as the_date_column, cast(1 as unsigned) as unsigned_column, "
  + "release_year as year_column, rental_rate as rate_column, true as bool_column, "
  + "cast(1 as double) as double_column, rating as enum_column from film where film_id = 1";

function cli(command: string, args: string[]): string | null {
  try {
    return execFileSync(command, args).toString();
  } catch {
    return null;
  }
}

/** How a CLI lined up each column of a table row: right when the value
 *  is padded before it (`| ` / ` ` then the cell). */
function cliAligns(row: string): string[] {
  const cells = row.split('|');
  // mysql's rows start and end with a border.
  const inner = row.startsWith('|') ? cells.slice(1, -1) : cells;
  return inner.map(cell => (cell.slice(1).startsWith(' ') ? 'right' : 'left'));
}

async function gridAligns(page: Page, query: string) {
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  await input.fill(`${query};`);
  await input.press('Enter');
  const table = out.locator('.shell-table').last();
  await expect(table.locator('td').first()).toBeVisible({ timeout: 10_000 });
  // A cell the browser starts at the left reads `start`.
  return table.evaluate(t => {
    const align = (el: Element) => getComputedStyle(el).textAlign.replace('start', 'left');
    return {
      cells: [...t.querySelectorAll('tbody tr:first-child td')].map(align),
      headers: [...t.querySelectorAll('th')].map(align),
    };
  });
}

test('Postgres: the grid lines columns up as psql does', async ({ page }) => {
  const psql = cli('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', '-P', 'null=NULL', '-c', PG_QUERY]);
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const { cells, headers } = await gridAligns(page, PG_QUERY);
  expect(cells).toEqual(['right', 'right', 'left', 'right', 'right', 'right', 'right', 'left', 'left', 'left']);
  expect(new Set(headers)).toEqual(new Set(['center']));
  test.skip(psql === null, 'psql not on PATH');
  // psql's first data row, under its header and rule.
  expect(cells).toEqual(cliAligns(psql!.split('\n')[2]));
});

test('MySQL: the grid lines columns up as the mysql CLI does', async ({ page }) => {
  const mysql = cli('mysql', ['-h', '127.0.0.1', '-u', 'root', 'sakila', '--table', '-e', MY_QUERY]);
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const { cells, headers } = await gridAligns(page, MY_QUERY);
  expect(cells).toEqual(['right', 'right', 'left', 'right', 'left', 'right', 'right', 'right', 'right', 'right', 'left']);
  expect(new Set(headers)).toEqual(new Set(['left']));
  test.skip(mysql === null, 'mysql not on PATH');
  // The CLI's first data row, under its border, header and border.
  expect(cells).toEqual(cliAligns(mysql!.split('\n')[3]));
});
