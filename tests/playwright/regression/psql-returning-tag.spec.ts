import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `PrintQueryResult`: after the rows of an INSERT / UPDATE /
// DELETE / MERGE … RETURNING, its tag (`PrintQueryStatus` — a paragraph
// under html, nothing under QUIET), then the time; after `\gset` and
// `\crosstabview` too, and each `\watch` run. Not after a SELECT, a CTE's
// included. A statement without columns given to `\gset`,
// `\crosstabview` or `\gexec` prints its tag alone. Each tag compared
// with psql's for the same input — expanded, unaligned and html output
// whole (skipped without psql on PATH); the table is temporary.

const SETUP = 'create temp table r (a int, b text);';

// psql's output for each `[name]`-marked section of `script`.
function psqlSections(script: string): Record<string, string> | null {
  const run = spawnSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off'],
    { input: `${SETUP}\n${script}`, encoding: 'utf8' });
  if (run.error) return null;
  const sections: Record<string, string> = {};
  let name = '';
  for (const line of run.stdout.split('\n')) {
    const marker = line.match(/^\[(\w+)\]$/);
    if (marker) { name = marker[1]; sections[name] = ''; continue; }
    if (name) sections[name] += `${line}\n`;
  }
  return sections;
}

const CASES: [string, string][] = [
  ['plain', "insert into r values (1, 'x'), (2, 'y') returning a;"],
  ['expanded', "\\x on\ninsert into r values (3, 'z') returning a, b;\n\\x off"],
  ['unaligned', "\\pset format unaligned\nupdate r set b = 'w' where a = 3 returning a;\n\\pset format aligned"],
  ['html', "\\pset format html\ninsert into r values (4, 'v') returning a;\n\\pset format aligned"],
  ['gset', "insert into r values (5, 'u') returning a \\gset\n\\echo :a"],
  ['merge', "merge into r using (select 1 as k) s on r.a = s.k when matched then update set b = 'm' returning r.a;"],
  ['crosstab', "insert into r values (10, 'q') returning a, b, 1 as c \\crosstabview"],
  ['nocolumns', "insert into r values (13, 'k') \\gset\ninsert into r values (14, 'j') \\crosstabview\ninsert into r values (15, 'i') \\gexec"],
];

test('psql: a RETURNING prints its tag under its rows, as psql does', async ({ page }) => {
  const theirs = psqlSections(CASES.map(([name, input]) => `\\echo [${name}]\n${input}`).join('\n') + '\n');
  test.skip(theirs === null, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const lastBlock = () => out.locator('.shell-block').last();
  const info = () => lastBlock().locator('.shell-entry-info');
  // The lines psql printed for a case — without the settings' own words.
  const lines = (name: string) => theirs![name].split('\n')
    .filter(l => !/^(Expanded display|Output format) is/.test(l)).join('\n').trim();
  // The tag psql printed last for a case.
  const tag = (name: string) => lines(name).split('\n').slice(-1)[0];

  await run(SETUP);
  await expect(out.locator('.shell-entry-info', { hasText: 'CREATE TABLE' })).toBeVisible({ timeout: 10_000 });

  // The grid, its footer, then the tag.
  await run(CASES[0][1]);
  await expect(info()).toHaveText(tag('plain'), { timeout: 10_000 });
  await expect(lastBlock().locator('.shell-table td')).toHaveText(['1', '2']);
  await expect(lastBlock().locator('.shell-stat')).toContainText('(2 rows)');

  // Expanded: the records, then the tag.
  await run('\\x on');
  await run("insert into r values (3, 'z') returning a, b;");
  await expect(info()).toHaveText('INSERT 0 1', { timeout: 10_000 });
  const records = (await lastBlock().locator('.shell-vertical-text').textContent())!.trimEnd();
  expect(`${records}\n\nINSERT 0 1`).toBe(lines('expanded'));
  await run('\\x off');

  // Unaligned and html: psql's text, then its tag.
  for (const [name, format, sql] of [
    ['unaligned', 'unaligned', "update r set b = 'w' where a = 3 returning a;"],
    ['html', 'html', "insert into r values (4, 'v') returning a;"],
  ]) {
    await run(`\\pset format ${format}`);
    await run(sql);
    await expect(info()).toHaveCount(1, { timeout: 10_000 });
    const printed = (await lastBlock().locator('.shell-printed').textContent())!.trimEnd();
    expect(`${printed}\n${await info().innerText()}`, name).toBe(lines(name));
    await run('\\pset format aligned');
  }

  // \gset: the row stored, the tag said.
  await run("insert into r values (5, 'u') returning a \\gset");
  await expect(info()).toHaveText('INSERT 0 1', { timeout: 10_000 });
  await run('\\echo :a');
  await expect(out.locator('.shell-meta-output').last()).toHaveText('5', { timeout: 10_000 });
  expect(lines('gset')).toBe('INSERT 0 1\n5');

  // MERGE … RETURNING; and a CTE's insert, which is a SELECT: no tag.
  await run(CASES[5][1]);
  await expect(info()).toHaveText(tag('merge'), { timeout: 10_000 });
  expect(tag('merge')).toBe('MERGE 1');
  await run("with i as (insert into r values (6, 't') returning a) select a from i;");
  await expect(lastBlock().locator('.shell-table td')).toHaveText(['6'], { timeout: 10_000 });
  await expect(info()).toHaveCount(0);

  // \crosstabview: the grid, then the tag.
  await run(CASES[6][1]);
  await expect(info()).toHaveText(tag('crosstab'), { timeout: 10_000 });
  await expect(lastBlock().locator('.shell-table th')).toHaveText(['a', 'q']);

  // No columns to work on: the statement's tag alone, as psql prints it.
  for (const sql of CASES[7][1].split('\n')) {
    await run(sql);
    await expect(info()).toHaveText('INSERT 0 1', { timeout: 10_000 });
    await expect(lastBlock().locator('.shell-meta-output, .shell-entry-error')).toHaveCount(0);
  }
  expect(lines('nocolumns')).toBe('INSERT 0 1\nINSERT 0 1\nINSERT 0 1');

  // \timing: the time after the tag, not under the rows.
  await run('\\timing on');
  await run('delete from r where a = 2 returning *;');
  await expect(info()).toHaveText(/^DELETE 1\nTime: \d+\.\d{3} ms$/, { timeout: 10_000 });
  await expect(lastBlock().locator('.shell-timing')).toHaveCount(0);
  // \gset of a SELECT: its time alone, as psql's.
  await run('select 1 as g \\gset');
  await expect(info()).toHaveText(/^Time: \d+\.\d{3} ms$/, { timeout: 10_000 });
  await run('\\timing off');

  // QUIET: no tag; a SELECT: none either.
  await run('\\set QUIET on');
  await run("insert into r values (12, 'o') returning a;");
  await expect(lastBlock().locator('.shell-table td')).toHaveText(['12'], { timeout: 10_000 });
  await expect(info()).toHaveCount(0);
  await run('\\set QUIET off');
  await run('select 1 as one;');
  await expect(lastBlock().locator('.shell-table td')).toHaveText(['1'], { timeout: 10_000 });
  await expect(info()).toHaveCount(0);

  // \watch: each run's rows, then its tag.
  await run("insert into r values (20, 'n') returning a \\watch c=2 i=0");
  await expect(info()).toHaveCount(2, { timeout: 10_000 });
  await expect(info()).toHaveText(['INSERT 0 1', 'INSERT 0 1']);
});
