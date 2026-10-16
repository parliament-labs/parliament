import { test, expect, Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// A result is a table whatever its shape, as psql and the mysql CLI print
// it: one wider than twelve columns stays a table (going vertical is
// `\x` / `\gx` / `\G`, or psql's `\x auto` when it's wider than the
// pane), and a single column whose values hold line breaks is a table
// whose cells keep them, under its header and row count.

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

for (const [connection, footer] of [['Local Postgres', '(2 rows)'], ['Local MySQL', '2 rows in set']]) {
  test(`${connection}: wide and multi-line results are tables`, async ({ page }) => {
    const { out, run } = await openShell(page, connection);
    const tables = out.locator('.shell-table');

    // Fourteen columns.
    const wide = Array.from({ length: 14 }, (_, i) => `${i} as c${i}`).join(', ');
    await run(`select ${wide};`);
    await expect(tables).toHaveCount(1, { timeout: 10_000 });
    await expect(tables.last().locator('th')).toHaveCount(14);
    await expect(out.locator('.shell-vertical-block')).toHaveCount(0);

    // One column, values on several lines.
    const lines = connection === 'Local Postgres' ? "E'a\\nb'" : "'a\\nb'";
    await run(`select ${lines} as two_lines union all select 'c';`);
    await expect(tables).toHaveCount(2, { timeout: 10_000 });
    await expect(tables.last().locator('th')).toHaveText('two_lines');
    await expect(tables.last().locator('td').first()).toHaveText('a\nb');
    await expect(out.locator('.shell-stat').last()).toContainText(footer);
  });
}
