import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\gexec` (`ExecQueryTuples`): it says nothing of its own — each
// non-NULL cell runs, row by row, as if typed, its output printed as any
// statement's (a failure doesn't stop the rest unless ON_ERROR_STOP) —
// and its own statement's status follows them (`PrintQueryResult`): a
// RETURNING's tag, then the time. Each case compared with psql's own
// output for the same input (skipped without psql on PATH); the tables
// are temporary.

const SETUP = 'create temp table g (a int);';

// psql's output for each `[name]`-marked section of `script`, its
// errors and warnings in place.
function psqlSections(script: string): Record<string, string> | null {
  const run = spawnSync('sh', ['-c', 'psql -X -d pagila -U postgres -P pager=off 2>&1'],
    { input: `${SETUP}\n${script}`, encoding: 'utf8' });
  if (run.error || !run.stdout.includes('CREATE TABLE')) return null;
  const sections: Record<string, string> = {};
  let name = '';
  for (const line of run.stdout.replace(/^psql:<stdin>:\d+: /gm, '').split('\n')) {
    const marker = line.match(/^\[(\w+)\]$/);
    if (marker) { name = marker[1]; sections[name] = ''; continue; }
    if (name) sections[name] += `${line}\n`;
  }
  return sections;
}

test('psql: \\gexec runs each cell and says nothing of its own, as psql does', async ({ page }) => {
  const theirs = psqlSections([
    '\\echo [two]', "select 'select 1 as x', 'select 2 as y' \\gexec",
    '\\echo [none]', "select 'select 1' where false \\gexec",
    '\\echo [returning]', "insert into g values (1) returning 'select 5 as v' \\gexec",
  ].join('\n') + '\n');
  test.skip(theirs === null, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const lastBlock = () => out.locator('.shell-block').last();
  // What a block printed, in order: each table's headers and cells, each
  // command's status, each error and note.
  const printed = async () => lastBlock().locator('.shell-table, .shell-entry-info, .shell-entry-error, .shell-meta-output')
    .evaluateAll(nodes => nodes.map(n => n.classList.contains('shell-table')
      ? [...n.querySelectorAll('th, td')].map(c => c.textContent!.trim()).join(' ')
      : n.textContent!.trim()));
  const settled = async () => {
    await run('\\echo settled');
    await expect(out.locator('.shell-meta-output').last()).toHaveText('settled', { timeout: 10_000 });
  };

  await run(SETUP);
  await expect(out.locator('.shell-entry-info', { hasText: 'CREATE TABLE' })).toBeVisible({ timeout: 10_000 });

  // Each cell, in order; no word of its own.
  await run("select 'select 1 as x', 'select 2 as y' \\gexec");
  await expect(lastBlock().locator('.shell-table')).toHaveCount(2, { timeout: 10_000 });
  expect(await printed()).toEqual(['x 1', 'y 2']);
  expect(theirs!.two.trim()).toBe(' x \n---\n 1\n(1 row)\n\n y \n---\n 2\n(1 row)'.trim());

  // Nothing to run: nothing at all.
  await run("select 'select 1' where false \\gexec");
  await settled();
  const blocks = out.locator('.shell-block');
  expect(await blocks.nth(await blocks.count() - 2).locator('.shell-table, .shell-entry-info, .shell-meta-output').count()).toBe(0);
  expect(theirs!.none.trim()).toBe('');

  // NULLs skipped; a failure doesn't stop the rest.
  await run("select null, 'select 1/0', 'select 4 as w' \\gexec");
  await expect(lastBlock().locator('.shell-table')).toHaveCount(1, { timeout: 10_000 });
  expect(await printed()).toEqual(['ERROR:  division by zero', 'w 4']);

  // Generated DDL prints its tags.
  await run("select format('create temp table %I (a int)', 'g' || n) from generate_series(1, 2) n \\gexec");
  await expect(lastBlock().locator('.shell-entry-info')).toHaveCount(2, { timeout: 10_000 });
  expect(await printed()).toEqual(['CREATE TABLE', 'CREATE TABLE']);

  // A RETURNING: the generated statement's output, then the tag.
  await run("insert into g values (1) returning 'select 5 as v' \\gexec");
  await expect(lastBlock().locator('.shell-entry-info')).toHaveCount(1, { timeout: 10_000 });
  expect(await printed()).toEqual(['v 5', 'INSERT 0 1']);
  expect(theirs!.returning.trim().split('\n').slice(-1)[0]).toBe('INSERT 0 1');

  // \timing: each generated statement timed, then the \gexec's own time.
  await run('\\timing on');
  await run("select 'select 6 as u' \\gexec");
  await expect(lastBlock().locator('.shell-timing, .shell-entry-info')).toHaveCount(2, { timeout: 10_000 });
  await expect(lastBlock().locator('.shell-entry-info').last()).toHaveText(/^Time: \d+\.\d{3} ms$/);
  await run('\\timing off');

  // What follows it in the input runs after its statements.
  await run("select 'select 7 as t' \\gexec\nselect 8 as s;");
  await expect(out.locator('.shell-table').last().locator('th')).toHaveText('s', { timeout: 10_000 });
  expect(await blocks.nth(await blocks.count() - 2).locator('.shell-table th').allInnerTexts()).toEqual(['t']);

  // A statement that loses the session: psql's reset, the rest of what
  // it generated on the new one, then the \gexec's own time.
  await run('\\timing on');
  await run("select $$select pg_terminate_backend(pg_backend_pid())$$, $$select 11 as k$$ \\gexec");
  await expect(lastBlock()).toContainText('Attempting reset: Succeeded.', { timeout: 15_000 });
  await expect(lastBlock().locator('.shell-table th')).toHaveText('k', { timeout: 15_000 });
  await expect(lastBlock().locator('.shell-entry-info').last()).toHaveText(/^Time: \d+\.\d{3} ms$/, { timeout: 10_000 });
  const order = await lastBlock().locator('.shell-table, .shell-entry-info').evaluateAll(nodes => nodes.map(n => n.className));
  expect(order[order.length - 1]).toContain('shell-entry-info');
  await run('\\timing off');

  // ON_ERROR_STOP: the failure, and nothing after it.
  await run('\\set ON_ERROR_STOP on');
  const tables = await out.locator('.shell-table').count();
  await run("select 'select 1/0', 'select 9 as r' \\gexec\nselect 10 as q;");
  await expect(lastBlock().locator('.shell-entry-error')).toHaveText('ERROR:  division by zero', { timeout: 10_000 });
  await settled();
  await expect(out.locator('.shell-table')).toHaveCount(tables);
});

// A blank cell runs too, as psql's `ExecQueryTuples` sends every
// non-NULL one: the server answers an empty query with nothing, so it
// prints nothing — but its time under `\timing`, and under `AUTOCOMMIT
// off` the `BEGIN` psql sends before it opens a transaction.
test('psql: \\gexec sends a blank cell as psql does', async ({ page }) => {
  const theirs = psqlSections([
    '\\echo [plain]', "select '', ' ', '-- c', 'select 1 as a' \\gexec",
    '\\echo [timing]', '\\timing on', "select '', 'select 2 as b' \\gexec", '\\timing off',
    '\\set AUTOCOMMIT off',
    '\\echo [blank_begins]', "select 'commit', '' \\gexec", 'rollback;',
    '\\echo [commit_only]', "select 'commit' \\gexec", 'rollback;',
  ].join('\n') + '\n');
  test.skip(theirs === null, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const lastBlock = () => out.locator('.shell-block').last();
  // What a block printed, in order: tables, statuses, times, notices.
  const printed = async () => lastBlock().locator('.shell-table, .shell-entry-info, .shell-timing, .shell-entry-error, .shell-meta-output, .shell-notice')
    .evaluateAll(nodes => nodes.map(n => n.classList.contains('shell-table')
      ? [...n.querySelectorAll('th, td')].map(c => c.textContent!.trim()).join(' ')
      : n.textContent!.trim().replace(/\d+\.\d{3} ms/, 'N ms')));
  const settled = async () => {
    await run('\\echo settled');
    await expect(out.locator('.shell-meta-output').last()).toHaveText('settled', { timeout: 10_000 });
  };

  // Blank cells print nothing; the query among them prints its table.
  await run("select '', ' ', '-- c', 'select 1 as a' \\gexec");
  await expect(lastBlock().locator('.shell-table')).toHaveCount(1, { timeout: 10_000 });
  await settled();
  const blocks = out.locator('.shell-block');
  expect(await blocks.nth(await blocks.count() - 2).locator('.shell-table, .shell-entry-info, .shell-entry-error, .shell-meta-output').count()).toBe(1);
  expect(theirs!.plain.trim()).toBe('a \n---\n 1\n(1 row)');

  // \timing: the blank cell's time, the query's, then the \gexec's own.
  await run('\\timing on');
  await run("select '', 'select 2 as b' \\gexec");
  await expect(lastBlock().locator('.shell-entry-info, .shell-timing')).toHaveCount(3, { timeout: 10_000 });
  expect(await printed()).toEqual(['Time: N ms', 'b 2', 'Time: N ms', 'Time: N ms']);
  expect(theirs!.timing.replace(/\d+\.\d{3} ms/g, 'N ms').trim())
    .toBe('Timing is on.\nTime: N ms\n b \n---\n 2\n(1 row)\n\nTime: N ms\nTime: N ms\nTiming is off.');
  await run('\\timing off');

  // AUTOCOMMIT off: psql's BEGIN before the blank one opens a
  // transaction, which the ROLLBACK ends without a warning; without a
  // blank one there's none to end.
  await run('\\set AUTOCOMMIT off');
  await run("select 'commit', '' \\gexec");
  await expect(lastBlock().locator('.shell-entry-info')).toHaveText('COMMIT', { timeout: 10_000 });
  await run('rollback;');
  await expect(lastBlock().locator('.shell-entry-info')).toHaveText('ROLLBACK', { timeout: 10_000 });
  expect(await printed()).toEqual(['ROLLBACK']);
  expect(theirs!.blank_begins.trim()).toBe('COMMIT\nROLLBACK');
  await run("select 'commit' \\gexec");
  await expect(lastBlock().locator('.shell-entry-info')).toHaveText('COMMIT', { timeout: 10_000 });
  await run('rollback;');
  await expect(lastBlock().locator('.shell-entry-info')).toHaveText('ROLLBACK', { timeout: 10_000 });
  expect(await printed()).toEqual(['WARNING:  there is no transaction in progress', 'ROLLBACK']);
  expect(theirs!.commit_only.trim()).toBe('COMMIT\nWARNING:  there is no transaction in progress\nROLLBACK');
  await run('\\set AUTOCOMMIT on');
});

// psql sets the result variables from the `\gexec`'s own statement once
// the statements it generated have run (`ExecQueryAndProcessResults`:
// `SetResultVariables` after `PrintQueryResult`): its row count — and,
// should one of them have failed, failure, with no SQLSTATE or message
// (its own result has none). Each echoed after a `\gexec`, compared with
// psql's.
const VARIABLE_CASES: [string, string][] = [
  ['generated', "select 'select 1 from generate_series(1,3)' \\gexec"],
  ['failed', "select 'select 1/0' \\gexec"],
  ['mixed', "select 'select 1 from generate_series(1,4)', 'select 1/0' from generate_series(1,2) \\gexec"],
  ['none', "select 'select 1/0' where false \\gexec"],
  ['ddl', "select 'create temp table gv (a int)' \\gexec"],
  ['returning', "insert into g values (1), (2) returning 'select 1 from generate_series(1,5)' \\gexec"],
];
const ECHO = (name: string) => `\\echo [${name}] :ROW_COUNT :ERROR :'SQLSTATE' :'LAST_ERROR_SQLSTATE' :'LAST_ERROR_MESSAGE'`;

test('psql: \\gexec sets the result variables from its own statement, as psql does', async ({ page }) => {
  const run = spawnSync('sh', ['-c', 'psql -X -d pagila -U postgres -P pager=off 2>&1'],
    { input: [SETUP, ...VARIABLE_CASES.flatMap(([name, sql]) => [sql, ECHO(name)])].join('\n') + '\n', encoding: 'utf8' });
  test.skip(!!run.error, 'psql not on PATH');
  const theirs = Object.fromEntries(run.stdout.split('\n')
    .map(line => line.match(/^\[(\w+)\] (.*)$/)).filter(m => m).map(m => [m![1], m![2]]));
  expect(Object.keys(theirs)).toEqual(VARIABLE_CASES.map(([name]) => name));
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const send = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };

  await send(SETUP);
  await expect(out.locator('.shell-entry-info', { hasText: 'CREATE TABLE' })).toBeVisible({ timeout: 10_000 });
  for (const [name, sql] of VARIABLE_CASES) {
    await send(sql);
    await send(ECHO(name));
    const echoed = out.locator('.shell-meta-line', { hasText: `[${name}]` });
    await expect(echoed).toHaveCount(1, { timeout: 10_000 });
    expect(await echoed.textContent(), name).toBe(`[${name}] ${theirs[name]}`);
  }
});
