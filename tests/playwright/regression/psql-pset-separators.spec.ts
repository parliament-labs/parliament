import { test, expect } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\pset fieldsep` / `recordsep` (unaligned), `csv_fieldsep`,
// `tableattr` (html, latex-longtable), `columns` (the width `wrapped` and
// `\x auto` fit) — and `\f`, `\T` — print what psql prints, compared with
// psql's own output for the same query (skipped without psql on PATH), and
// say what psql says when set.

const QUERY = "select 1 as n, 'a;b' as t, null::text as z union all select 2, 'c', 'd'";

function psql(args: string[]): string | null {
  try {
    return execFileSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', ...args]).toString();
  } catch {
    return null;
  }
}

test('psql: \\pset separators, tableattr and columns print as psql prints them', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output').last();
  const printed = out.locator('.shell-printed');
  // Runs `sql`, returns what printed (its text, as the CLI wrote it).
  const result = async (sql: string) => {
    const before = await printed.count();
    await run(`${sql};`);
    await expect(printed).toHaveCount(before + 1, { timeout: 10_000 });
    return (await printed.last().textContent())!.trimEnd();
  };
  const compare = async (sql: string, args: string[]) => {
    const ours = await result(sql);
    const theirs = psql([...args, '-c', sql]);
    if (theirs !== null) expect(ours, args.join(' ')).toBe(theirs.trimEnd());
  };

  await run('\\pset format unaligned');
  await run("\\pset fieldsep ';'");
  await expect(said).toHaveText('Field separator is ";".', { timeout: 10_000 });
  await run("\\pset recordsep '#'");
  await expect(said).toHaveText('Record separator is "#".', { timeout: 10_000 });
  await compare(QUERY, ['-P', 'format=unaligned', '-P', 'fieldsep=;', '-P', 'recordsep=#']);
  await run('\\x on');
  await compare(QUERY, ['-P', 'format=unaligned', '-P', 'fieldsep=;', '-P', 'recordsep=#', '-P', 'expanded=on']);
  await run('\\x off');
  await expect(said).toHaveText('Expanded display is off.', { timeout: 10_000 });
  // No value says what it is; `\f` is `\pset fieldsep`, keeping a `;`
  // as psql's does.
  const messages = out.locator('.shell-meta-output');
  const before = await messages.count();
  await run('\\pset recordsep');
  await expect(messages).toHaveCount(before + 1, { timeout: 10_000 });
  await expect(said).toHaveText('Record separator is "#".');
  await run('\\f ;');
  await expect(said).toHaveText('Field separator is ";".', { timeout: 10_000 });
  await run("\\f '\\t'");
  await expect.poll(() => said.textContent(), { timeout: 10_000 }).toBe('Field separator is "\t".');

  await run('\\pset format csv');
  await run("\\pset csv_fieldsep ';'");
  await expect(said).toHaveText('Field separator for CSV is ";".', { timeout: 10_000 });
  await compare(QUERY, ['-P', 'format=csv', '-P', 'csv_fieldsep=;']);
  await run('\\pset csv_fieldsep ab');
  await expect(out.locator('.shell-entry-error').last()).toHaveText('\\pset: csv_fieldsep must be a single one-byte character', { timeout: 10_000 });

  await run('\\pset format html');
  await run("\\T 'class=\"x\"'");
  await expect(said).toHaveText('Table attributes are "class="x"".', { timeout: 10_000 });
  await compare(QUERY, ['-P', 'format=html', '-P', 'tableattr=class="x"']);
  await run('\\T');
  await expect(said).toHaveText('Table attributes unset.', { timeout: 10_000 });

  // `columns`: the width to fit, the pane's no longer.
  await run('\\pset format wrapped');
  await run('\\pset columns 30');
  await expect(said).toHaveText('Target width is 30.', { timeout: 10_000 });
  const wide = "select 1 as id, 'the quick brown fox jumps over the lazy dog' as words, 12345 as num";
  await compare(wide, ['-P', 'format=wrapped', '-P', 'columns=30']);
  await run('\\pset format aligned');
  await run('\\x auto');
  await run(`${wide};`);
  await expect(out.locator('.shell-vertical-block').last()).toContainText('words | the quick', { timeout: 10_000 });
  await run('\\pset columns 0');
  await expect(said).toHaveText('Target width is unset.', { timeout: 10_000 });
});
