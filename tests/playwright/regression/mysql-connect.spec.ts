import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// The mysql CLI's `connect [db [host]]` (`\r`, `com_connect`): the
// session closed and a new one opened — on the database named, or the
// one it's on — saying `Connection id:` and `Current database:`, after
// `Reading table information…` when a database was named; the buffer
// cleared. A database the server refuses: its error, and no connection —
// the next statement's reconnect tries it again, and says it can't. Each
// command's output compared with the CLI's own for it, typed at its
// prompt under a pty (`expect`; skipped without the CLI or its login,
// MYSQL_PWD).

const COMMANDS = [
  '\\r',
  'connect mysql',
  "\\r 'sakila'",
  'select 1 as a \\r',
  ';',
  '\\r sakila;',
  'select 2 as b;',
  '\\r sakila',
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
  // Between prompts: the echoed command (once or twice, as the terminal
  // echoes it), then what the CLI printed.
  return run.stdout.replace(/\r/g, '').split('mysql> ').slice(1, commands.length + 1).map((part, i) => {
    const lines = part.split('\n');
    lines.shift();
    if (lines[0] === commands[i]) lines.shift();
    return lines.join('\n');
  });
}

// The connection id is the server's to number; the bell the CLI rings
// after an error (`\a`, without --no-beep) is the terminal's.
const normal = (text: string) => text.replace(/Connection id: {4}\d+/g, 'Connection id:    N').replace(/\x07/g, '').trimEnd();

test('mysql: connect opens a new session as the CLI does', async ({ page }) => {
  const theirs = mysqlCli(COMMANDS);
  test.skip(theirs === null, 'mysql CLI not available');
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const blocks = out.locator('.shell-block');
  // What a block printed: its words and errors, line by line.
  const said = (block: ReturnType<typeof blocks.nth>) => block.locator('.shell-meta-line, .shell-entry-error')
    .evaluateAll(nodes => nodes.flatMap(n => n.classList.contains('shell-meta-line')
      ? [n.textContent ?? ''] : (n as HTMLElement).innerText.split('\n')).join('\n'));

  for (const [i, command] of COMMANDS.entries()) {
    const before = await blocks.count();
    await input.fill(command);
    await input.press('Enter');
    await expect(blocks).toHaveCount(before + 1, { timeout: 10_000 });
    const expected = normal(theirs![i]);
    await expect.poll(async () => normal(await said(blocks.last())), { message: command, timeout: 10_000 }).toBe(expected);
  }

  // On the database it connected to: a new session, the one before gone.
  await input.fill('select database() as db;');
  await input.press('Enter');
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText('sakila', { timeout: 10_000 });

  // Another host needs its own credentials: not here yet.
  await input.fill('\\r sakila elsewhere.example');
  await input.press('Enter');
  await expect(out.locator('.shell-entry-error').last()).toHaveText("Connecting to another host isn't supported here yet.", { timeout: 10_000 });
});
