import { test, expect, Page, Locator } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// A result keeps what it printed with — psql's `\pset null`, `\t`,
// `\pset footer`, `\pset title` — as psql's output does: a later `\pset`
// changes what prints next, not what has. Shown and copied ("Copy
// output") alike.

test.use({ permissions: ['clipboard-read', 'clipboard-write'] });

async function copyOutput(page: Page, block: Locator): Promise<string> {
  await block.click({ button: 'right' });
  await page.locator('.tab-context-menu-item', { hasText: 'Copy output' }).click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText()), { timeout: 5_000 }).not.toBe('');
  const text = await page.evaluate(() => navigator.clipboard.readText());
  // Clear it, so the next copy is read fresh.
  await page.evaluate(() => navigator.clipboard.writeText(''));
  return text;
}

test('psql: a result shows and copies as it printed', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const tables = out.locator('.shell-table');
  // The block holding `sql`'s result.
  const block = (sql: string) => out.locator('.shell-block', { has: page.locator('.shell-sql', { hasText: sql }) });

  await run("\\pset null '(null)'");
  await run("\\pset title 'Mine'");
  await run('select 1 as a, null::int as b;');
  await expect(tables).toHaveCount(1, { timeout: 10_000 });
  const first = block('select 1 as a');
  await expect(first.locator('.shell-table-title')).toHaveText('Mine');
  await expect(first.locator('td').nth(1)).toHaveText('(null)');
  expect(await copyOutput(page, first)).toBe('Mine\na\tb\n1\t(null)\n(1 row)');

  // Later settings print the next result; the first stays as it printed.
  await run("\\pset null 'X'");
  await run('\\pset title');
  await run('\\pset footer off');
  await run('select 2 as c, null::int as d;');
  await expect(tables).toHaveCount(2, { timeout: 10_000 });
  const second = block('select 2 as c');
  await expect(second.locator('.shell-table-title')).toHaveCount(0);
  expect(await copyOutput(page, second)).toBe('c\td\n2\tX');
  await expect(first.locator('.shell-table-title')).toHaveText('Mine');
  await expect(first.locator('td').nth(1)).toHaveText('(null)');
  expect(await copyOutput(page, first)).toBe('Mine\na\tb\n1\t(null)\n(1 row)');

  // `\t`: rows alone — and the earlier results keep their headers.
  await run('\\t');
  await run('select 3 as e;');
  await expect(tables).toHaveCount(3, { timeout: 10_000 });
  expect(await copyOutput(page, block('select 3 as e'))).toBe('3');
  await expect(first.locator('th')).toHaveCount(2);
  await run('\\t');

  // Expanded: psql's records under the title, copied as shown.
  await run("\\pset title 'Wide'");
  await run('select 4 as f, null::int as g \\gx');
  const vertical = block('select 4 as f').locator('.shell-vertical-text');
  await expect(vertical).toHaveText('Wide\n-[ RECORD 1 ]\nf | 4\ng | X', { timeout: 10_000 });
  expect(await copyOutput(page, block('select 4 as f'))).toBe('Wide\n-[ RECORD 1 ]\nf | 4\ng | X');
});
