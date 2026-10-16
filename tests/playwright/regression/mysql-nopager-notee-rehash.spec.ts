import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// The mysql CLI's `nopager` (`\n`), `notee` (`\t`) and `rehash` (`\#`):
// a pane pages nothing and has no outfile, so the first two say what
// the CLI says — `PAGER set to stdout`, `Outfile disabled.` — and
// `rehash` fetches completion's names afresh, saying nothing. Each
// compared with what the CLI prints for it at its prompt, under a pty
// (`expect`; skipped without the CLI or its login, MYSQL_PWD).

const COMMANDS = ['nopager', '\\n', 'notee', '\\t', 'rehash', '\\#', 'rehash;'];

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
  const parts = run.stdout.replace(/\r/g, '').split('mysql> ').slice(1, commands.length + 1);
  return parts.map((part, i) => {
    const lines = part.split('\n');
    lines.shift();
    if (lines[0] === commands[i]) lines.shift();
    return lines.join('\n').trim();
  });
}

test('mysql: nopager, notee and rehash say what the CLI says', async ({ page }) => {
  const theirs = mysqlCli(COMMANDS);
  test.skip(theirs === null, 'mysql CLI not available');
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  // A command's entry in the scrollback, by its text exactly, and its block.
  const exactly = (cmd: string) => new RegExp(`^${cmd.replace(/[\\#;]/g, m => `\\${m}`)}$`);
  const entry = (cmd: string) => out.locator('.shell-sql').filter({ hasText: exactly(cmd) });
  // (A `has` locator is read inside each block: rooted at the page.)
  const block = (cmd: string) => out.locator('.shell-block')
    .filter({ has: page.locator('.shell-sql').filter({ hasText: exactly(cmd) }) }).last();

  for (const [i, command] of COMMANDS.entries()) {
    await input.fill(command);
    await input.press('Enter');
    await expect(entry(command)).toHaveCount(1, { timeout: 10_000 });
    await expect(input).toHaveValue('');
    const said = async () => (await block(command).locator('.shell-meta-output, .shell-entry-error').allInnerTexts()).join('\n').trim();
    await expect.poll(said, { message: command, timeout: 10_000 }).toBe(theirs![i]);
  }

  // Typed after SQL, as the CLI reads it: said, and the statement runs.
  await input.fill('select 1 as a \\n;');
  await input.press('Enter');
  await expect(out.locator('.shell-table').last().locator('th')).toHaveText('a', { timeout: 10_000 });
  await expect(out.locator('.shell-meta-output', { hasText: 'PAGER set to stdout' })).toHaveCount(3);
});
