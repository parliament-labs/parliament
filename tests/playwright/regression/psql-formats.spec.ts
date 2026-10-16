import { test, expect, Page } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\pset format`s (fe_utils/print.c, ported in
// tyto/src/plane/postgres/print.rs): a result in each prints what psql
// prints — numbers right where the markup aligns, the title inside it,
// `\x` vertical — compared with psql's own output for the same query,
// with psql's defaults (NULL shows as nothing). `\a` and `\H` toggle
// as psql's do, and `\pset format` reads abbreviations and refuses in
// psql's words. Aligned stays the shell's grid. The comparisons need
// psql on PATH (skipped without).

const QUERY = `select 1 as n, 'a|b "c"' as t, null::text as z, E'line1\\nline2' as m, ' lead & <x>' as h, 1.5::numeric as d `
  + `union all select 22, '', 'x', '', '  ', 2`;

function psql(args: string[]): string | null {
  try {
    return execFileSync('psql', ['-X', '-q', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', ...args]).toString();
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
  const said = () => out.locator('.shell-meta-output').last();
  const printed = () => out.locator('.shell-printed').last();
  const text = async () => (await printed().evaluate(el => el.textContent ?? '')).trimEnd();
  return { out, run, said, printed, text };
}

test('psql: each \\pset format prints what psql prints', async ({ page }) => {
  test.skip(psql(['-c', 'select 1']) === null, 'psql not on PATH');
  const { out, run, said, printed, text } = await openShell(page);

  for (const format of ['unaligned', 'csv', 'html', 'asciidoc', 'latex', 'latex-longtable', 'troff-ms']) {
    for (const x of ['off', 'on']) {
      await run(`\\pset format ${format}`);
      await expect(said()).toHaveText(`Output format is ${format}.`, { timeout: 10_000 });
      await run(`\\x ${x}`);
      await expect(said()).toHaveText(`Expanded display is ${x}.`, { timeout: 10_000 });
      const blocks = await out.locator('.shell-printed').count();
      await run(`${QUERY};`);
      await expect(out.locator('.shell-printed')).toHaveCount(blocks + 1, { timeout: 10_000 });
      const theirs = psql(['-P', `format=${format}`, '-P', `expanded=${x}`, '-c', QUERY])!;
      expect(await text(), `${format}, \\x ${x}`).toBe(theirs.trimEnd());
    }
  }
  await run('\\x off');

  // The title goes in the text; a list's too.
  await run('\\pset format unaligned');
  await run("\\pset title 'My Title'");
  await run('select 1 as a;');
  await expect(printed()).toHaveText('My Title\na\n1\n(1 row)', { timeout: 10_000 });
  await run('\\pset title');
  await run('\\dt actor');
  await expect(printed()).toHaveText(/^List of tables\nSchema\|Name\|Type\|Owner\npublic\|actor\|table\|\w+\n\(1 row\)$/, { timeout: 10_000 });
  // A `\watch` run's title, once, in the text — its own line ending
  // and a blank line after it, as psql's.
  await run('select 7 as w \\watch c=1');
  await expect(printed()).toHaveText(/^\w{3} \w{3} [ \d]\d \d\d:\d\d:\d\d \d{4} \(every 2s\)\n\nw\n7\n\(1 row\)$/, { timeout: 10_000 });
  await expect(out.locator('.shell-table-title')).toHaveCount(0);

  // `\d` too, aligned or not (expanded only for a sequence, as psql).
  await run('\\pset format csv');
  await run('\\d actor');
  await expect(printed()).toContainText('Column,Type,Collation,Nullable,Default', { timeout: 10_000 });
  expect(await text()).toBe(psql(['-P', 'format=csv', '-c', '\\d actor'])!.trimEnd());

  // Aligned is the grid again.
  await run('\\pset format aligned');
  await run('select 2 as b;');
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toBeVisible({ timeout: 10_000 });
});

test('psql: wrapped fits the pane, as psql fits its terminal', async ({ page }) => {
  const { out, run, said, printed, text } = await openShell(page);
  await run('\\pset format wrapped');
  await expect(said()).toHaveText('Output format is wrapped.', { timeout: 10_000 });
  await run(`select 1 as id, repeat('the quick brown fox ', 40) as words, 12345 as num;`);
  await expect(printed()).toBeVisible({ timeout: 10_000 });
  const lines = (await text()).split('\n');
  // The pane's width in characters, as the shell measures it.
  const columns = await out.evaluate((el) => {
    const probe = document.createElement('span');
    probe.className = 'shell-table';
    probe.style.cssText = 'position:absolute;visibility:hidden;white-space:pre;padding:0;border:0';
    probe.textContent = '0'.repeat(100);
    el.appendChild(probe);
    const charWidth = probe.getBoundingClientRect().width / 100;
    probe.remove();
    const style = getComputedStyle(el);
    const width = el.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
    return Math.floor(width / charWidth);
  });
  // Wrapped, each break marked `.` on both sides.
  expect(lines.some(l => l.includes('.|'))).toBe(true);
  expect(lines.some(l => l.includes('|.'))).toBe(true);
  expect(Math.max(...lines.map(l => l.length))).toBeLessThanOrEqual(columns);
  const theirs = psql(['-P', 'format=wrapped', '-P', `columns=${columns}`, '-c',
    `select 1 as id, repeat('the quick brown fox ', 40) as words, 12345 as num`]);
  test.skip(theirs === null, 'psql not on PATH');
  expect(lines.join('\n')).toBe(theirs!.trimEnd());
});

test('psql: \\a, \\H and \\pset format read as psql reads them', async ({ page }) => {
  const { out, run, said } = await openShell(page);
  const error = () => out.locator('.shell-entry-error').last();
  await run('\\a');
  await expect(said()).toHaveText('Output format is unaligned.', { timeout: 10_000 });
  await run('\\a');
  await expect(said()).toHaveText('Output format is aligned.', { timeout: 10_000 });
  await run('\\H');
  await expect(said()).toHaveText('Output format is html.', { timeout: 10_000 });
  await run('\\H');
  await expect(said()).toHaveText('Output format is aligned.', { timeout: 10_000 });
  await run('\\pset format latex-');
  await expect(said()).toHaveText('Output format is latex-longtable.', { timeout: 10_000 });
  await run('\\pset format');
  await expect(said()).toHaveText('Output format is latex-longtable.', { timeout: 10_000 });
  await run('\\pset format a');
  await expect(error()).toHaveText('\\pset: ambiguous abbreviation "a" matches both "aligned" and "asciidoc"', { timeout: 10_000 });
  await run('\\pset format nope');
  await expect(error()).toHaveText(
    '\\pset: allowed formats are aligned, asciidoc, csv, html, latex, latex-longtable, troff-ms, unaligned, wrapped', { timeout: 10_000 });
  await run('\\pset format');
  await expect(said()).toHaveText('Output format is latex-longtable.', { timeout: 10_000 });
});
