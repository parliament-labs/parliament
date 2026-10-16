import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// The mysql CLI's `use` (com_use): "Database changed" — not the server's
// "Query OK" — typed alone, as `\u`, or as a statement of a batch; and
// the shell is then on that database (`database()`, the tab's label,
// where a reconnect goes). Without a name it says
// "ERROR: / USE must be followed by a database name".

test('use switches the database as the mysql CLI does', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const changed = out.locator('.shell-meta-output', { hasText: /^Database changed$/ });

  await run('use mysql');
  await expect(changed).toHaveCount(1, { timeout: 10_000 });
  await run('SELECT database() AS db;');
  await expect(out.locator('.shell-table').last().locator('td').first()).toHaveText('mysql', { timeout: 10_000 });
  await expect(out).not.toContainText('Query OK');

  await run('use');
  await expect(out).toContainText('USE must be followed by a database name');

  // A batch: `use` ends at its `;`, the rest runs after it.
  await run('use sakila; SELECT 1 AS a;');
  await expect(changed).toHaveCount(2, { timeout: 10_000 });
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toBeVisible({ timeout: 10_000 });
  await run('SELECT database() AS db;');
  await expect(out.locator('.shell-table').last().locator('td').first()).toHaveText('sakila', { timeout: 10_000 });
});

// What the mysql CLI printed for each of `commands`, typed in turn at
// its prompt under a pty (`expect`): its words and errors, the bell it
// rings after an error left out. Null without the CLI or its login.
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
  return run.stdout.replace(/\r/g, '').replace(/\x07/g, '').split('mysql> ').slice(1, commands.length + 1).map((part, i) => {
    const lines = part.split('\n');
    lines.shift();
    if (lines[0] === commands[i]) lines.shift();
    return lines.join('\n').trimEnd();
  });
}

// Onto another database, the CLI reads its names for completion first
// and says so (`com_use`'s `build_completion_hash`): "Reading table
// information …" over "Database changed"; not when it's the database
// it's on, nor when the server refuses it. Compared with the CLI's.
test('use says it reads the new database for completion, as the CLI does', async ({ page }) => {
  const COMMANDS = ['use mysql', 'use mysql', 'use nosuchdb', '\\u sakila', 'use sakila;', 'use mysql; select 1 as a;', 'use SAKILA', 'use mysql'];
  const theirs = mysqlCli(COMMANDS);
  test.skip(theirs === null, 'mysql CLI not available');
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const blocks = out.locator('.shell-block');
  // What the blocks from `from` on said: words and errors, line by line.
  const said = (from: number) => blocks.evaluateAll((nodes, from) => nodes.slice(from).flatMap(block =>
    [...block.querySelectorAll('.shell-meta-line, .shell-entry-error')]
      .flatMap(n => n.classList.contains('shell-meta-line') ? [n.textContent ?? ''] : (n as HTMLElement).innerText.split('\n')))
    .join('\n').trimEnd(), from);

  for (const [i, command] of COMMANDS.entries()) {
    const from = await blocks.count();
    await input.fill(command);
    await input.press('Enter');
    // The CLI's words, without the rows a statement after it printed.
    const expected = theirs![i].split('\n').filter(line => !/^[+|]|^\d+ rows? in set/.test(line)).join('\n').trimEnd();
    await expect.poll(() => said(from), { message: command, timeout: 10_000 }).toBe(expected);
  }
});
