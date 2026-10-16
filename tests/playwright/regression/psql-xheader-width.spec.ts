import { test, expect } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\pset xheader_width`: how far an expanded record's line runs —
// the full width, to the column line, to the page's width (`\pset
// columns`, or the pane's, as psql's terminal's), or so many characters
// — compared with psql's own output (skipped without psql on PATH).

const QUERY = "select 1 as id, 'the quick brown fox jumps over the lazy dog' as words";

function psql(args: string[]): string | null {
  try {
    return execFileSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', '-P', 'expanded=on', ...args]).toString();
  } catch {
    return null;
  }
}

test('psql: \\pset xheader_width draws record lines as psql does', async ({ page }) => {
  test.skip(psql(['-c', 'select 1']) === null, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output').last();
  const blocks = out.locator('.shell-vertical-block');
  const text = (n: number) => blocks.nth(n).locator('.shell-vertical-text');
  const expanded = async (sql: string) => {
    const before = await blocks.count();
    await run(`${sql};`);
    await expect(blocks).toHaveCount(before + 1, { timeout: 10_000 });
    return (await text(before).textContent())!.trimEnd();
  };

  await run('\\x on');
  await run('\\pset border 2');
  for (const width of ['column', '20', '-3']) {
    await run(`\\pset xheader_width ${width}`);
    await expect(said).toHaveText(width === 'column' ? 'Expanded header width is "column".' : `Expanded header width is ${width}.`, { timeout: 10_000 });
    expect(await expanded(QUERY), width).toBe(psql(['-P', 'border=2', '-P', `xheader_width=${width}`, '-c', QUERY])!.trimEnd());
  }

  // `page`: `\pset columns`, as psql's …
  await run('\\pset columns 30');
  await run('\\pset xheader_width page');
  expect(await expanded(QUERY)).toBe(psql(['-P', 'border=2', '-P', 'columns=30', '-P', 'xheader_width=page', '-c', QUERY])!.trimEnd());
  // … or the pane's width, as psql's terminal's: a record line no wider
  // than the pane, where the full one runs past it.
  await run('\\pset columns 0');
  const wide = `select repeat('x', 400) as words`;
  const paged = (await expanded(wide)).split('\n')[0];
  await run('\\pset xheader_width full');
  const full = (await expanded(wide)).split('\n')[0];
  expect(paged.length).toBeLessThan(full.length);
  expect(paged.length).toBeGreaterThan(20);
  expect(paged.endsWith('+')).toBe(true);

  // What printed keeps its lines.
  expect((await text(0).textContent())!.split('\n')[0]).toBe('+-[ RECORD 1 ]');
});
