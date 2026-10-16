import { test, expect, Locator } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// A table's title sits as psql's aligned printer puts it
// (`print_aligned_text`): centred over the table when it's narrower — its
// first line where its widest line would centre, the rest from the left
// edge — and from the left edge when it's as wide as the table or wider.
// A `\watch` run's title is the run's table's own, a blank line after it.

const box = async (l: Locator) => (await l.boundingBox())!;
const centre = (b: { x: number; width: number }) => b.x + b.width / 2;

test('psql: a title is centred over its table as psql centres it', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const frames = out.locator('.shell-table-frame');
  const result = async (sql: string) => {
    const before = await frames.count();
    await run(sql);
    await expect(frames).toHaveCount(before + 1, { timeout: 10_000 });
    const frame = frames.last();
    return { frame, table: frame.locator('.shell-table'), title: frame.locator('.shell-table-title'),
             first: frame.locator('.shell-table-title-first > span').first() };
  };
  const wide = "select repeat('x', 60) as a, 1 as b;";

  // Narrower than the table: centred over it, not over the pane.
  await run("\\pset title 'Short'");
  let r = await result(wide);
  expect(Math.abs(centre(await box(r.first)) - centre(await box(r.table)))).toBeLessThan(4);
  expect((await box(r.table)).width).toBeLessThan((await box(out)).width - 100);

  // As wide or wider: from the left edge.
  await run("\\pset title 'A title rather longer than the table it sits over'");
  r = await result('select 1 as a;');
  const titleBox = await box(r.title);
  expect((await box(r.first)).x).toBeLessThan(titleBox.x + 16);
  expect((await box(r.table)).x).toBeLessThan(titleBox.x + 2);

  // Several lines: the first where the widest would centre, the rest
  // from the left edge (psql pads the first line alone).
  await run("\\pset title 'ab\\nmuch longer line'");
  r = await result(wide);
  const lines = r.frame.locator('.shell-table-title-width');
  const widest = await box(lines.nth(1));
  expect(Math.abs(centre(widest) - centre(await box(r.table)))).toBeLessThan(4);
  expect(Math.abs((await box(r.first)).x - widest.x)).toBeLessThan(1);
  const second = await box(r.title.locator(':scope > div').nth(1));
  expect(second.x).toBeLessThan(titleBox.x + 16);
  await expect(r.title.locator(':scope > div').nth(1)).toHaveText('much longer line');

  // An empty title is a blank line.
  await run("\\pset title ''");
  r = await result('select 1 as a;');
  expect((await box(r.title)).height).toBeGreaterThan(8);

  // A `\watch` run: its title centred over a table wider than it, then
  // a blank line.
  await run('\\pset title');
  r = await result("select repeat('y', 90) as a, 2 as b \\watch c=1");
  await expect(r.first).toHaveText(/\(every 2s\)$/);
  expect(Math.abs(centre(await box(r.first)) - centre(await box(r.table)))).toBeLessThan(4);
  await expect(r.title.locator(':scope > div')).toHaveCount(2);
});
