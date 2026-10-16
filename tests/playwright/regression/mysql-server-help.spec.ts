import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// The mysql CLI's `help topic` / `\h topic` (`com_server_help`): the
// server's `HELP 'topic'`, printed as the CLI prints each answer — one
// topic (name, description, examples), several items or a category's
// contents (a list under `categories:` / `topics:`), nothing ("Nothing
// found"), or the server's error. Each compared with the CLI's own
// output for the same command, typed at its prompt under a pty
// (`expect`; skipped without the CLI or its login, MYSQL_PWD).

const COMMANDS = ['help create index', 'help create', 'help contents', "help 'Data Types'", 'help nosuch', '\\h select;'];

// What the mysql CLI printed for each of `commands`, typed in turn.
function mysqlCli(commands: string[]): string[] | null {
  const tcl = (s: string) => s.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\[/g, '\\[').replace(/\$/g, '\\$');
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
  const parts = run.stdout.replace(/\r/g, '').split('mysql> ').slice(1, commands.length + 1);
  return parts.map((part, i) => {
    const lines = part.split('\n');
    lines.shift();
    if (lines[0] === commands[i]) lines.shift();
    return lines.join('\n').trimEnd();
  });
}

test('mysql: help is the server\'s, printed as the CLI prints it', async ({ page }) => {
  const theirs = mysqlCli(COMMANDS);
  test.skip(theirs === null, 'mysql CLI not available');
  expect(theirs![1]).toContain('Many help items for your request exist.');
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const printed = out.locator('.shell-printed');

  for (const [i, command] of COMMANDS.entries()) {
    await input.fill(command);
    await input.press('Enter');
    await expect(printed).toHaveCount(i + 1, { timeout: 10_000 });
    expect((await printed.last().textContent())!.trimEnd(), command).toBe(theirs![i]);
  }

  // A quote in the topic breaks the server's statement: its error.
  await input.fill("help o'x");
  await input.press('Enter');
  const error = out.locator('.shell-entry-error').last();
  await expect(error).toContainText('ERROR 1064 (42000)', { timeout: 10_000 });
  await expect(error).toContainText("near 'x''");
  // No topic: the CLI's own command list.
  await input.fill('help');
  await input.press('Enter');
  await expect(out.locator('.shell-meta-output').last()).toContainText('List of all MySQL commands', { timeout: 10_000 });
});
