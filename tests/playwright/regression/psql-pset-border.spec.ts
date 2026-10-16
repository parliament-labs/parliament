import { test, expect } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\pset border` and `linestyle` (with the unicode style's border,
// column and header lines) draw what psql prints as text — expanded
// output, `\d`, wrapped and the other formats — compared with psql's own
// output for the same command (skipped without psql on PATH). Each says
// what psql says when set, and refuses what psql refuses. A result keeps
// the lines it printed with.

const QUERY = "select actor_id, first_name, E'a\\nb' as two_lines from actor where actor_id <= 2 order by actor_id";

function psql(args: string[]): string | null {
  try {
    return execFileSync('psql', ['-X', '-q', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', ...args]).toString();
  } catch {
    return null;
  }
}

test('psql: \\pset border and linestyle draw what psql draws', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output').last();
  const vertical = out.locator('.shell-vertical-block');
  const verticalText = () => vertical.last().locator('.shell-vertical-text');
  const printed = out.locator('.shell-printed');

  await run('\\pset border 2');
  await expect(said).toHaveText('Border style is 2.', { timeout: 10_000 });
  await run('\\pset linestyle u');
  await expect(said).toHaveText('Line style is unicode.', { timeout: 10_000 });
  await run('\\pset unicode_header_linestyle double');
  await expect(said).toHaveText('Unicode header line style is "double".', { timeout: 10_000 });
  await run('\\pset linestyle x');
  await expect(out.locator('.shell-entry-error').last())
    .toHaveText('\\pset: allowed line styles are ascii, old-ascii, unicode', { timeout: 10_000 });
  const style = ['-P', 'border=2', '-P', 'linestyle=unicode', '-P', 'unicode_header_linestyle=double'];

  // Expanded output.
  await run('\\x on');
  await run(`${QUERY};`);
  await expect(verticalText()).toContainText('┌─[ RECORD 1 ]', { timeout: 10_000 });
  const expanded = psql([...style, '-P', 'expanded=on', '-c', QUERY]);
  test.skip(expanded === null, 'psql not on PATH');
  expect((await verticalText().textContent())!.trimEnd()).toBe(expanded!.trimEnd());
  await run('\\x off');
  await expect(said).toHaveText('Expanded display is off.', { timeout: 10_000 });

  // What psql prints itself: `\d`.
  const before = await printed.count();
  await run('\\d actor');
  await expect(printed).toHaveCount(before + 1, { timeout: 10_000 });
  expect((await printed.last().textContent())!.trimEnd()).toBe(psql([...style, '-c', '\\d actor'])!.trimEnd());

  // Wrapped, to `\pset columns`, under old-ascii's marks.
  await run('\\pset linestyle old-ascii');
  await expect(said).toHaveText('Line style is old-ascii.', { timeout: 10_000 });
  await run('\\pset format wrapped');
  await run('\\pset columns 30');
  await expect(said).toHaveText('Target width is 30.', { timeout: 10_000 });
  const wide = "select 1 as id, 'the quick brown fox jumps over the lazy dog' as words, E'x\\ny' as m";
  await run(`${wide};`);
  await expect(printed).toHaveCount(before + 2, { timeout: 10_000 });
  expect((await printed.last().textContent())!.trimEnd())
    .toBe(psql(['-P', 'border=2', '-P', 'linestyle=old-ascii', '-P', 'format=wrapped', '-P', 'columns=30', '-c', wide])!.trimEnd());

  // html writes the border as it is.
  await run('\\pset format html');
  await run('\\pset border 0');
  await expect(said).toHaveText('Border style is 0.', { timeout: 10_000 });
  await run('select 1 as n;');
  await expect(printed).toHaveCount(before + 3, { timeout: 10_000 });
  await expect(printed.last()).toContainText('<table border="0">');

  // The expanded result printed earlier keeps its frame.
  await expect(verticalText()).toContainText('┌─[ RECORD 1 ]');
});
