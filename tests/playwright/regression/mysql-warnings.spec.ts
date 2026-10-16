import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// The mysql CLI's `warnings` (`\W`) / `nowarning` (`\w`): with it on,
// each statement that has warnings — or fails — is followed by the
// server's `SHOW WARNINGS`, a line each (`Warning (Code 1365): Division
// by 0`), but for a failure's own error alone (`print_warnings`); then
// the next statement. Each command's lines compared with the CLI's own
// for it, typed at its prompt under a pty (`expect`; skipped without the
// CLI or its login, MYSQL_PWD).

const COMMANDS = [
  'warnings',
  'select 1/0 as q;',
  "select cast('a' as signed) as x, cast('b' as signed) as y;",
  'select * from nosuch;',
  'select 1 as ok;',
  '\\w',
  'select 1/0 as q;',
];

// What the mysql CLI printed for each of `commands`, typed in turn.
function mysqlCli(commands: string[]): string[] | null {
  const tcl = (s: string) => s.replace(/\\/g, '\\\\').replace(/"/g, '\\"');
  const script = [
    'set timeout 10', 'log_user 1',
    'spawn mysql -h 127.0.0.1 -u root sakila',
    'expect "mysql> "',
    ...commands.flatMap(c => [`send "${tcl(c)}\\r"`, 'expect "mysql> "']),
    'send "quit\\r"', 'expect eof',
  ].join('\n');
  const run = spawnSync('expect', ['-c', script], { encoding: 'utf8' });
  if (run.error || !run.stdout.includes('mysql> ')) return null;
  return run.stdout.replace(/\r/g, '').split('mysql> ').slice(1, commands.length + 1);
}

// The lines a command printed that aren't its result: what the
// `warnings` switch says, and the server's warnings.
const said = (text: string) => text.split('\n')
  .filter(l => /^Show warnings|^(Warning|Note|Error) \(Code \d+\)/.test(l)).join('\n');

test('mysql: warnings shows the server\'s warnings as the CLI does', async ({ page }) => {
  const theirs = mysqlCli(COMMANDS);
  test.skip(theirs === null, 'mysql CLI not available');
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const blocks = out.locator('.shell-block');

  for (const [i, command] of COMMANDS.entries()) {
    const before = await blocks.count();
    await input.fill(command);
    await input.press('Enter');
    await expect(blocks).toHaveCount(before + 1, { timeout: 10_000 });
    const block = blocks.last();
    // Settled: its result, error or words in.
    await expect(block.locator('.shell-table, .shell-entry-error, .shell-meta-output').first()).toBeVisible({ timeout: 10_000 });
    const expected = said(theirs![i]);
    await expect.poll(async () => said((await block.locator('.shell-meta-output').allInnerTexts()).join('\n')),
      { message: command, timeout: 10_000 }).toBe(expected);
  }

  // A paste: each statement's warnings before the next statement runs.
  await input.fill('warnings');
  await input.press('Enter');
  await input.fill('select 1/0 as a; select 2 as b;');
  await input.press('Enter');
  const table = (column: string) => out.locator('.shell-table', { has: page.locator('th', { hasText: new RegExp(`^${column}$`) }) }).last();
  await expect(table('b')).toBeVisible({ timeout: 10_000 });
  const warning = out.locator('.shell-meta-output', { hasText: 'Division by 0' }).last();
  // DOM order: a's table, its warning, then b's table.
  const follows = async (first: typeof warning, second: typeof warning) =>
    first.evaluate((a, b) => !!(a.compareDocumentPosition(b!) & Node.DOCUMENT_POSITION_FOLLOWING), await second.elementHandle());
  expect(await follows(table('a'), warning)).toBe(true);
  expect(await follows(warning, table('b'))).toBe(true);
});
