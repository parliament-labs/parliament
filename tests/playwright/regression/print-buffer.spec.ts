import { test, expect, type Page } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// `\p` prints the query buffer as each CLI's does, and leaves it where
// it is. psql's (`exec_command_print`): the buffer as its lexer holds it
// — leading whitespace and `--` comments dropped, an empty line too, the
// space before the command kept, `:var`s expanded — or, empty, the
// previous query as it was sent; with neither `Query buffer is empty.`,
// nothing under QUIET. The mysql CLI's (`com_print`): every line since
// the last statement ended, between two rules, never the previous query.
// Each compared with the CLI's own output for the same input — psql
// reading a script, mysql typed at its prompt under a pty (`expect`) —
// skipped without them.

// What a shell printed for its last command: each line of its words and
// warnings, in order.
async function lastSaid(page: Page): Promise<string[]> {
  const block = visibleShellOutput(page).locator('.shell-block').last();
  return block.locator('.shell-meta-line, .shell-entry-error').evaluateAll(nodes => nodes.flatMap(n =>
    n.classList.contains('shell-meta-line') ? [n.textContent ?? ''] : (n as HTMLElement).innerText.split('\n')));
}

// psql's output for each `[name]`-marked section of `script`, its
// warnings in place.
function psqlSections(script: string): Record<string, string[]> | null {
  const run = spawnSync('sh', ['-c', 'psql -X -d pagila -U postgres -P pager=off 2>&1'], { input: script, encoding: 'utf8' });
  if (run.error || !run.stdout.includes('[empty]')) return null;
  const sections: Record<string, string[]> = {};
  let name = '';
  for (const line of run.stdout.replace(/^psql:<stdin>:\d+: /gm, '').split('\n')) {
    const marker = line.match(/^\[(\w+)\]$/);
    if (marker) { name = marker[1]; sections[name] = []; continue; }
    // `\r` between the cases, as the shell's input starts over.
    if (name && line !== 'Query buffer reset (cleared).') sections[name].push(line);
  }
  for (const lines of Object.values(sections)) {
    while (lines.length && lines[lines.length - 1] === '') lines.pop();
  }
  return sections;
}

// Each case: what runs before it, and the input with the `\p`.
const PSQL_CASES: [string, string[], string][] = [
  ['quiet', ['\\set QUIET on'], '\\p'],
  ['empty', ['\\set QUIET off'], '\\p'],
  ['same', [], 'select 1 as a \\p'],
  ['multi', [], 'select 2\n  as b\n\\p'],
  ['blank', [], 'select 3\n\n  as c\n\\p'],
  ['comment', [], '-- lead\n/* block */ select 4 -- tail\n\\p'],
  ['indent', [], 'select 5\n   \\p'],
  ['var', ['\\set v 42'], "select :v as v, ':v' \\p"],
  ['quote', [], "select 'x\n\ny' as q \\p"],
  ['prev', ['select 6 as f;'], '\\p'],
  ['prevg', ['select 7 as g \\g'], '\\p'],
  ['prevnl', ['select 8 as h\n\\g'], '\\p'],
  ['extra', [], '\\p x y'],
  ['gx', ['select 11 as k \\gx'], '\\p'],
];

test('psql: \\p prints the query buffer, or the previous query, as psql does', async ({ page }) => {
  // What runs before a case prints under a marker of its own (`[_]`).
  const theirs = psqlSections(PSQL_CASES
    .map(([name, before, input]) => ['\\echo [_]', ...before, `\\echo [${name}]`, input, '\\r'].join('\n')).join('\n') + '\n');
  test.skip(theirs === null, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const settled = async () => {
    await run('\\echo settled');
    await expect(out.locator('.shell-meta-output').last()).toHaveText('settled', { timeout: 10_000 });
  };

  for (const [name, before, command] of PSQL_CASES) {
    for (const cmd of before) {
      await run(cmd);
      await settled();
    }
    // The command in a block of its own (as typed, the buffer left
    // out), what it printed under it.
    const blocks = await out.locator('.shell-block').count();
    await run(command);
    await expect(out.locator('.shell-block')).toHaveCount(blocks + 1, { timeout: 10_000 });
    await expect(out.locator('.shell-sql').last()).toHaveText(command.split('\n').pop()!.replace(/^.* (?=\\p)/, ''));
    expect(await lastSaid(page), name).toEqual(theirs![name]);
  }

  // The buffer stays in the input, as psql keeps it.
  await run('select 1 as a \\p');
  await expect(input).toHaveValue('select 1 as a\n');
  // Then runs as typed: \p changed nothing.
  await input.press('End');
  await input.pressSequentially(';');
  await input.press('Enter');
  await expect(out.locator('.shell-table').last().locator('th')).toHaveText('a', { timeout: 10_000 });

  // Statements before it run first; the buffer is what follows them.
  await run('select 9 as i; select 10 as j \\p');
  await expect(out.locator('.shell-meta-line').last()).toHaveText('select 10 as j ', { timeout: 10_000 });
  await expect(out.locator('.shell-table').last().locator('th')).toHaveText('i');
});

// What the mysql CLI printed for `lines`, typed in turn at its prompt:
// each `print`'s buffer, the lines between its two rules.
function mysqlFrames(lines: string[]): string[] | null {
  const tcl = (s: string) => s.replace(/\\/g, '\\\\').replace(/"/g, '\\"');
  const script = [
    'set timeout 10', 'log_user 1',
    'spawn mysql -h 127.0.0.1 -u root sakila',
    'expect "mysql> "',
    ...lines.flatMap(l => [`send "${tcl(l)}\\r"`, 'expect "> "']),
    'send "\\\\c\\r"', 'expect "mysql> "',
    'send "quit\\r"', 'expect eof',
  ].join('\n');
  const run = spawnSync('expect', ['-c', script], { encoding: 'utf8' });
  if (run.error || !run.stdout.includes('mysql> ')) return null;
  return frames(run.stdout.replace(/\r/g, '').split('\n'));
}

const RULE = '--------------';

// The buffers `print` printed in `lines`: what lies between each pair of
// rules.
function frames(lines: string[]): string[] {
  const found: string[] = [];
  for (let at = lines.indexOf(RULE); at >= 0; ) {
    const end = lines.indexOf(RULE, at + 1);
    found.push(lines.slice(at + 1, end).join('\n'));
    at = lines.indexOf(RULE, end + 1);
  }
  return found;
}

// Each case: the lines typed at the CLI's prompt (`\c` clearing what
// a case leaves), and the inputs typed in the shell.
const MYSQL_CASES: [string[], string[]][] = [
  [['\\p'], ['\\p']],
  [['select 1 as a \\p', '\\c'], ['select 1 as a \\p']],
  [['select 3', '', '  as c \\p', '\\c'], ['select 3\n\n  as c \\p']],
  [['select 4 as d; select 5 as e \\p', '\\c'], ['select 4 as d; select 5 as e \\p']],
  [['select 6 as f;', '\\p'], ['select 6 as f;', '\\p']],
  [['print'], ['print']],
  [['print;'], ['print;']],
  [['select 7 as g \\p from dual;'], ['select 7 as g \\p from dual;']],
  [['/* c */ select 8 -- x', '\\p', '\\c'], ['/* c */ select 8 -- x\n\\p']],
  [['select 9 as h\\p;'], ['select 9 as h\\p;']],
  [['select 10; print;'], ['select 10; print;']],
];

test('mysql: \\p and print show the buffer between rules, as the CLI does', async ({ page }) => {
  const theirs = mysqlFrames(MYSQL_CASES.flatMap(([cli]) => cli));
  test.skip(theirs === null, 'mysql CLI not available');
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  // The buffers printed so far, each between its rules.
  const ours = async () => frames(await out.locator('.shell-meta-line').allTextContents());

  let printed = 0;
  for (const [, inputs] of MYSQL_CASES) {
    for (const typed of inputs) {
      await input.fill(typed);
      await input.press('Enter');
    }
    printed += 1;
    await expect.poll(async () => (await ours()).length, { message: inputs.join(' / '), timeout: 10_000 }).toBe(printed);
  }
  expect(await ours()).toEqual(theirs);
  // A blank line under each frame, as the CLI's `tee_puts("…\n")`.
  const last = out.locator('.shell-meta-output').last().locator('.shell-meta-line');
  await expect(last).toHaveText([RULE, 'print', RULE, '']);
  // The empty buffer's line is a line on screen, as tall as the rules'.
  const rule = await last.first().boundingBox();
  const empty = await out.locator('.shell-meta-line:empty').first().boundingBox();
  expect(empty!.height).toBeCloseTo(rule!.height, 0);
  // What follows `\p` on its line still runs: `select 7 as g  from dual`.
  await expect(out.locator('.shell-table th', { hasText: /^g$/ })).toHaveCount(1);
});
