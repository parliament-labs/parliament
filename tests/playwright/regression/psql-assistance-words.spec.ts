import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's assistance words (psql 18's `MainLoop`): `help`, `quit`, `exit`
// at the start of a line, in any case, maybe with one `;`. With the query
// buffer empty `help` prints psql's pointers and `quit` leaves — in an
// inactive `\if` branch too. With a statement pending they're text, and
// psql says so: `Use \? for help or press control-C to clear the input
// buffer.`, `Use \q to quit.`, inside a quote `Use control-D to quit.`.
// psql has no `status`: it goes to the server.

const HELP = 'You are using psql, the command-line interface to PostgreSQL.';

test('psql: help, quit and exit as psql reads them', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output');
  const errors = out.locator('.shell-entry-error');

  await run('help');
  await expect(said.last()).toContainText(HELP, { timeout: 10_000 });
  await expect(said.last()).toContainText('\\g or terminate with semicolon to execute query');
  await run('HELP;');
  await expect(said.filter({ hasText: HELP })).toHaveCount(2, { timeout: 10_000 });
  await expect(input).toHaveValue('');

  // A statement pending: the word stays in it, and psql says so.
  await run('select 1 as a\nhelp');
  await expect(said.last()).toHaveText('Use \\? for help or press control-C to clear the input buffer.', { timeout: 10_000 });
  await run('select 2 as b\nquit');
  await expect(said.last()).toHaveText('Use \\q to quit.', { timeout: 10_000 });
  await run('select 2 as b\nquit\n;');
  await expect(errors.last()).toContainText('syntax error at or near "quit"', { timeout: 10_000 });
  await run("select 'x\nquit");
  await expect(said.last()).toHaveText('Use control-D to quit.', { timeout: 10_000 });
  await run("select 'x\nquit\n' as q;");
  await expect(out).toContainText('(1 row)', { timeout: 10_000 });

  // psql has no `status`.
  await run('status;');
  await expect(errors.last()).toContainText('syntax error at or near "status"', { timeout: 10_000 });

  // Read before `\if`'s branches: `help` prints in an inactive one.
  await run('\\if false');
  await run('help');
  await expect(said.filter({ hasText: HELP })).toHaveCount(3, { timeout: 10_000 });
  await run('\\endif');

  // `quit;` leaves: the shell's tab closes.
  const tabs = page.locator('.pane-tab');
  const before = await tabs.count();
  await run('quit;');
  await expect(tabs).toHaveCount(before - 1, { timeout: 10_000 });
});
