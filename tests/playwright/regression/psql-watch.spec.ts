import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\watch` prints every run, one after another: a title — the
// time the run started and the interval, after `\pset title` and a tab
// when one is set — as the result's own, over a grid or first in
// expanded output; a command's tag alone. An error
// ends the watch, as do `count` and `min_rows`, without a word; Ctrl+C
// stops it. Arguments are read, and refused, as psql reads them.

const TIME = String.raw`[A-Z][a-z]{2} [A-Z][a-z]{2} [ \d]\d \d\d:\d\d:\d\d \d{4}`;

test('\\watch prints each run as psql does', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  // A run: a grid, or expanded output, under a run's title.
  const runs = out.locator('.shell-table-frame, .shell-vertical-block').filter({ hasText: /\(every [\d.]+s\)/ });
  const bar = page.locator('.shell-watch-bar:visible');

  // Two runs, each titled, then the watch is over — silently.
  await run('select 1 as a \\watch c=2 i=0.2');
  await expect(runs).toHaveCount(2, { timeout: 10_000 });
  for (const title of await runs.locator('.shell-table-title-first').allInnerTexts()) {
    expect(title).toMatch(new RegExp(`^${TIME} \\(every 0\\.2s\\)$`));
  }
  await expect(runs.last().locator('.shell-table th')).toHaveText('a');
  await expect(bar).toHaveCount(0);
  await expect(out).not.toContainText('Watch stopped');

  // A command prints its tag each run, untitled.
  await run('create temp table pw_watch (a int) \\watch c=1');
  await expect(out).toContainText('CREATE TABLE', { timeout: 10_000 });
  await run('insert into pw_watch values (1) \\watch c=2 i=0.1');
  await expect(out.locator('.shell-entry-info', { hasText: 'INSERT 0 1' })).toHaveCount(2, { timeout: 10_000 });
  await expect(runs).toHaveCount(2);

  // min_rows: the run with too few rows prints, then the watch ends.
  await run('select a from pw_watch \\watch m=3 i=0.1');
  await expect(runs).toHaveCount(3, { timeout: 10_000 });
  await expect(bar).toHaveCount(0);

  // An error ends it.
  await run('select 1/0 \\watch c=3 i=0.1');
  await expect(out.locator('.shell-entry-error').last()).toContainText('division by zero', { timeout: 10_000 });
  await expect(bar).toHaveCount(0);
  await expect(runs).toHaveCount(3);

  // \pset title leads the title; the result has no other.
  await run('\\pset title Mine');
  await run('select 6 as f \\watch c=1');
  await expect(runs).toHaveCount(4, { timeout: 10_000 });
  await expect(runs.last().locator('.shell-table-title-first')).toHaveText(new RegExp(`^Mine\\t${TIME} \\(every 2s\\)$`));
  await expect(runs.last().locator('.shell-table-title')).toHaveCount(1);
  await run('\\pset title');

  // Expanded output.
  await run('\\x');
  await run('select 4 as d \\watch c=1');
  await expect(runs.last()).toContainText('d | 4', { timeout: 10_000 });
  await run('\\x');

  // Until stopped: Ctrl+C, as in psql.
  await run('select 9 as s \\watch 0.2');
  await expect(bar).toContainText('every 0.2s');
  await expect.poll(async () => runs.count(), { timeout: 10_000 }).toBeGreaterThanOrEqual(7);
  // With text selected, Ctrl+C is the copy it always was.
  await input.fill('abc');
  await input.evaluate((el: HTMLTextAreaElement) => el.setSelectionRange(0, 3));
  await input.press('Control+c');
  await expect(bar).toHaveCount(1);
  await input.fill('');
  await input.press('Control+c');
  await expect(bar).toHaveCount(0);
  const stoppedAt = await runs.count();
  await page.waitForTimeout(700);
  expect(await runs.count()).toBeLessThanOrEqual(stoppedAt + 1);

  // Arguments psql refuses.
  await run('select 1 \\watch c=0');
  await expect(out.locator('.shell-meta-output').last()).toHaveText('\\watch: incorrect iteration count "0"');
  await run('select 1 \\watch 1 2');
  await expect(out.locator('.shell-meta-output').last()).toHaveText('\\watch: interval value is specified more than once');
});

// What psql printed under each `[name]` marker of `script`.
function psqlSections(script: string): Record<string, string> | null {
  const run = spawnSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off'], { input: script, encoding: 'utf8' });
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

// psql's `\watch` runs its buffer itself (`exec_command_watch`), so the
// previous query stays the one sent before it: what `\p` prints and an
// empty buffer's `\g`, `\gdesc` and `\watch` run. Compared with psql's
// output for the same input (skipped without psql on PATH).
test('\\watch leaves the previous query as it was, as psql does', async ({ page }) => {
  const theirs = psqlSections(['select 1 as a;', 'select 2 as b \\watch c=1',
    '\\echo [p]', '\\p', '\\echo [g]', '\\g', '\\echo [gdesc]', '\\gdesc', '\\echo [watch]', '\\watch c=1'].join('\n') + '\n');
  test.skip(theirs === null, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const lastHeaders = () => out.locator('.shell-table').last().locator('th');

  await run('select 1 as a;');
  await expect(lastHeaders()).toHaveText(['a'], { timeout: 10_000 });
  await run('select 2 as b \\watch c=1');
  await expect(lastHeaders()).toHaveText(['b'], { timeout: 10_000 });

  // \p: the query before the watch.
  await run('\\p');
  await expect(out.locator('.shell-meta-line').last()).toHaveText('select 1 as a;');
  expect(theirs!.p.trim()).toBe('select 1 as a;');
  // \g and \watch run it; \gdesc describes it.
  await run('\\g');
  await expect(out.locator('.shell-block').last().locator('.shell-table th')).toHaveText(['a'], { timeout: 10_000 });
  expect(theirs!.g.split('\n')[0].trim()).toBe('a');
  await run('\\gdesc');
  await expect(out.locator('.shell-block').last().locator('.shell-table td')).toHaveText(['a', 'integer'], { timeout: 10_000 });
  expect(theirs!.gdesc).toContain(' a      | integer');
  await run('\\watch c=1');
  await expect(out.locator('.shell-block').last().locator('.shell-table th')).toHaveText(['a'], { timeout: 10_000 });
  expect(theirs!.watch).toMatch(/\n a \n---\n 1\n/);
});

// psql leaves the result variables alone for a `\watch` run, its
// failures too (`ExecQueryAndProcessResults`: `!is_watch`) — `\errverbose`
// still shows a failed run's error. Each echoed after a watch, compared
// with psql's.
const VARIABLES = ":ROW_COUNT :ERROR :'SQLSTATE' :'LAST_ERROR_SQLSTATE' :'LAST_ERROR_MESSAGE'";
const WATCH_CASES: [string, string[]][] = [
  ['before', ['select 1 from generate_series(1,3);']],
  ['ran', ['select 1 from generate_series(1,7) \\watch c=1']],
  ['failed', ['select 1/0 \\watch c=1']],
  ['recalled', ['select 1 from generate_series(1,2);', '\\watch c=1']],
];

test('\\watch leaves the result variables as they were, as psql does', async ({ page }) => {
  const script = WATCH_CASES.flatMap(([name, inputs]) => [...inputs, `\\echo [${name}] ${VARIABLES}`]);
  const psql = spawnSync('sh', ['-c', 'psql -X -d pagila -U postgres -P pager=off 2>&1'], { input: script.join('\n') + '\n', encoding: 'utf8' });
  test.skip(!!psql.error, 'psql not on PATH');
  const theirs = Object.fromEntries(psql.stdout.split('\n')
    .map(line => line.match(/^\[(\w+)\] (.*)$/)).filter(m => m).map(m => [m![1], m![2]]));
  expect(Object.keys(theirs)).toEqual(WATCH_CASES.map(([name]) => name));
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };

  // Each line typed straight after the last, as psql reads type-ahead:
  // what follows a watch runs once it's over.
  for (const [name, inputs] of WATCH_CASES) {
    for (const cmd of inputs) await run(cmd);
    await run(`\\echo [${name}] ${VARIABLES}`);
    const echoed = out.locator('.shell-meta-line', { hasText: `[${name}]` });
    await expect(echoed).toHaveCount(1, { timeout: 10_000 });
    expect(await echoed.textContent(), name).toBe(`[${name}] ${theirs[name]}`);
    // Read once the watch is over, as psql reads its next line: after
    // all it printed.
    const printed = out.locator('.shell-table, .shell-entry-error, .shell-meta-line');
    expect(await printed.last().textContent(), name).toBe(`[${name}] ${theirs[name]}`);
  }
  // The failed run's error is still the last one.
  await run('\\errverbose');
  await expect(out.locator('.shell-entry-error').last()).toContainText('22012: division by zero', { timeout: 10_000 });
});

// What psql printed for `lines`, typed at its prompt under a pty
// (`expect`): each line sent as the one before it starts, a `^C` as
// Ctrl+C. Null without psql or expect.
function psqlTyped(lines: string[]): string | null {
  const tcl = (s: string) => s.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\[/g, '\\[').replace(/\$/g, '\\$');
  const script = [
    'set timeout 10', 'log_user 1',
    'spawn psql -X -d pagila -U postgres -P pager=off',
    'expect "=# "',
    ...lines.flatMap(line => line === '^C'
      ? ['sleep 0.8', 'send "\\003"', 'sleep 0.3']
      : [`send "${tcl(line)}\\r"`, 'sleep 0.2']),
    'send "\\\\echo watch-done\\r"', 'expect "watch-done"', 'expect "=# "',
    'send "\\\\q\\r"', 'expect eof',
  ].join('\n');
  const run = spawnSync('expect', ['-c', script], { encoding: 'utf8' });
  if (run.error || !run.stdout.includes('=# ')) return null;
  return run.stdout.replace(/\r/g, '');
}

test('\\watch: what is typed meanwhile runs after it, or goes with Ctrl+C, as psql does', async ({ page }) => {
  // psql: a watch that ends itself, then the line typed during it; a
  // watch stopped by Ctrl+C takes the line with it.
  const ended = psqlTyped(['select 1 as a \\watch c=3 i=0.3', 'select 2 as b;']);
  const interrupted = psqlTyped(['select 3 as c \\watch 0.3', 'select 4 as d;', '^C']);
  test.skip(ended === null || interrupted === null, 'psql or expect not available');
  const runsOf = (text: string, column: string) => text.split(`\n ${column} \n`).length - 1;
  expect(runsOf(ended!, 'a')).toBe(3);
  expect(ended!.lastIndexOf('\n b \n')).toBeGreaterThan(ended!.lastIndexOf('\n a \n'));
  expect(runsOf(interrupted!, 'd')).toBe(0);

  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const bar = page.locator('.shell-watch-bar:visible');
  const headers = () => out.locator('.shell-table th').allInnerTexts();

  // Ends itself: its three runs, then the line typed during them.
  await run('select 1 as a \\watch c=3 i=0.3');
  await run('select 2 as b;');
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toHaveCount(1, { timeout: 10_000 });
  expect(await headers()).toEqual(['a', 'a', 'a', 'b']);
  await expect(bar).toHaveCount(0);

  // Ctrl+C: the watch is over, and the line typed during it gone.
  await run('select 3 as c \\watch 0.3');
  await expect(bar).toHaveCount(1, { timeout: 10_000 });
  await run('select 4 as d;');
  await expect(input).toHaveValue('');
  await expect(bar).toHaveCount(1);
  await input.press('Control+c');
  await expect(bar).toHaveCount(0);
  // Stop, its button, the same.
  await run('select 5 as e \\watch 0.3');
  await expect(bar).toHaveCount(1, { timeout: 10_000 });
  await run('select 6 as f;');
  await page.locator('.shell-watch-stop:visible').click();
  await expect(bar).toHaveCount(0);
  await run('\\echo done');
  await expect(out.locator('.shell-meta-line').last()).toHaveText('done', { timeout: 10_000 });
  expect((await headers()).filter(h => h === 'd' || h === 'f')).toEqual([]);
});

// A watch whose session is lost is over: its next run finds the
// connection gone and resets it as psql's `CheckConnection` does — the
// server's FATAL, libpq's lines, `Attempting reset: Succeeded.` — and
// what was typed meanwhile runs on the new one.
test('\\watch: a lost session resets as psql does, and what was typed runs after', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  await run('select pg_backend_pid() as pid;');
  await expect(out.locator('.shell-table td')).toHaveCount(1, { timeout: 10_000 });
  const pid = (await out.locator('.shell-table td').textContent())!.trim();

  await run('select 7 as g \\watch 2');
  await expect(page.locator('.shell-watch-bar:visible')).toHaveCount(1, { timeout: 10_000 });
  await run('select 8 as h;');
  const killed = spawnSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-c', `select pg_terminate_backend(${pid})`], { encoding: 'utf8' });
  test.skip(!!killed.error, 'psql not on PATH');
  await expect(out.locator('.shell-table th', { hasText: /^h$/ })).toHaveCount(1, { timeout: 15_000 });
  await expect(out).toContainText('server closed the connection unexpectedly');
  await expect(out).toContainText('The connection to the server was lost. Attempting reset: Succeeded.');
  await expect(page.locator('.shell-watch-bar:visible')).toHaveCount(0);
  // The reset came before the typed line.
  const lines = (await out.innerText()).split('\n');
  expect(lines.findIndex(l => l.startsWith('The connection to the server was lost')))
    .toBeLessThan(lines.findIndex(l => l === 'select 8 as h;'));
});

