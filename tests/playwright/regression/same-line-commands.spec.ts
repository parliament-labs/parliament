import { test, expect, type Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// A backslash command typed after SQL on the same line works on that SQL,
// as the native CLI reads a line: the first backslash outside quotes and
// comments starts it. psql: the `\g` family, `\watch`, `\e`, `\r` (and the
// statements ended before it run first). mysql: `\g` / `\G` end a
// statement wherever they're typed, `\c` clears the buffer without a word.

async function openShell(page: Page, connection: string) {
  await openApp(page);
  await clickConnection(page, connection);
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  return input;
}

test('psql: \\gx, \\gset and \\r after SQL on the same line', async ({ page }) => {
  const input = await openShell(page, 'Local Postgres');
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const vertical = out.locator('.shell-vertical-block');

  await run('select 1 as a \\gx');
  await expect(vertical.last()).toContainText('a | 1', { timeout: 10_000 });
  await expect(input).toHaveValue('');

  // The statement ended before the command runs first, as psql sends it.
  await run('select 1 as b; select 2 as c \\gx');
  await expect(vertical).toHaveCount(2, { timeout: 10_000 });
  await expect(vertical.last()).toContainText('c | 2');
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toBeVisible();

  await run('select 3 as x, 4 as y \\gset p_');
  await run('\\echo :p_x :p_y');
  await expect(out.locator('.shell-meta-output').last()).toHaveText('3 4', { timeout: 10_000 });

  // Quoted, the backslash is the string's.
  await run("select '\\gx' as s;");
  await expect(out.locator('.shell-table').last().locator('td').first()).toHaveText('\\gx', { timeout: 10_000 });

  // Commented, it's the comment's: Enter waits for the statement's end.
  await run('select 5 as f -- \\gx');
  await expect(input).toHaveValue('select 5 as f -- \\gx\n');
  await input.press('End');
  await input.type(';');
  await input.press('Enter');
  await expect(out.locator('.shell-table th', { hasText: /^f$/ })).toBeVisible({ timeout: 10_000 });

  // \watch repeats the buffer before it.
  await run('select 7 as w \\watch c=1');
  const watched = out.locator('.shell-table-frame', { hasText: /\(every 2s\)/ });
  await expect(watched.last()).toBeVisible({ timeout: 10_000 });
  await expect(watched.last().locator('.shell-table th', { hasText: /^w$/ })).toBeVisible();

  await run('select 6 as g \\r');
  await expect(out.locator('.shell-meta-output').last()).toHaveText('Query buffer reset (cleared).');
  await expect(input).toHaveValue('');
  await expect(out.locator('.shell-table th', { hasText: /^g$/ })).toHaveCount(0);
});

test('psql: any other command after SQL runs and leaves the buffer where it was', async ({ page }) => {
  const input = await openShell(page, 'Local Postgres');
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output');

  // \x runs; the buffer waits for its terminator. The block is the
  // command's alone.
  await run('select 1 as a \\x');
  await expect(said.last()).toHaveText('Expanded display is on.', { timeout: 10_000 });
  await expect(input).toHaveValue('select 1 as a\n');
  await expect(out.locator('.shell-block').last()).not.toContainText('select 1 as a');
  await run('select 1 as a\n;');
  await expect(out.locator('.shell-vertical-block').last()).toContainText('a', { timeout: 10_000 });

  // On a line of its own below the buffer, the same.
  await run('select 2 as b\n\\x');
  await expect(said.last()).toHaveText('Expanded display is off.', { timeout: 10_000 });
  await expect(input).toHaveValue('select 2 as b\n');
  await run('select 2 as b\n;');
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toBeVisible({ timeout: 10_000 });

  // \set, then \echo, the buffer waiting through both.
  await run('select 3 as c \\set v 5');
  await expect(input).toHaveValue('select 3 as c\n');
  await run('select 3 as c\n\\echo :v');
  await expect(said.last()).toHaveText('5', { timeout: 10_000 });
  await expect(input).toHaveValue('select 3 as c\n');

  // \d prints its description.
  await run('select 4 as d \\d actor_info');
  await expect(out.locator('.shell-printed').last()).toContainText('View "public.actor_info"', { timeout: 10_000 });
  await expect(input).toHaveValue('select 4 as d\n');

  // \prompt takes the input for its answer, then gives the buffer back.
  await run('select 5 as e \\prompt v2');
  await expect(input).toHaveValue('');
  await run('hello');
  await expect(input).toHaveValue('select 5 as e\n');
  await run('\\echo :v2');
  await expect(said.last()).toHaveText('hello', { timeout: 10_000 });

  // `--` comments alone aren't a buffer psql keeps.
  await run('-- why\n\\x');
  await expect(said.last()).toHaveText('Expanded display is on.', { timeout: 10_000 });
  await expect(input).toHaveValue('');
});

test('mysql: \\g and \\G end a statement wherever they are typed; \\c clears silently', async ({ page }) => {
  const input = await openShell(page, 'Local MySQL');
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };

  await run('select 1 as a \\g');
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toBeVisible({ timeout: 10_000 });

  await run('select 2 as b\\G select 3 as c;');
  await expect(out.locator('.shell-vertical-block').last()).toContainText('b:', { timeout: 10_000 });
  await expect(out.locator('.shell-table th', { hasText: /^c$/ })).toBeVisible({ timeout: 10_000 });

  await run('select 3 as d\n\\g');
  await expect(out.locator('.shell-table th', { hasText: /^d$/ })).toBeVisible({ timeout: 10_000 });

  const entries = out.locator('.shell-entry-query, .shell-meta-output, .shell-table');
  const before = await entries.count();
  await run('select 4 as e \\c');
  await expect(input).toHaveValue('');
  await expect(entries).toHaveCount(before);
  await expect(out).not.toContainText('Query buffer reset');
});

test("mysql: its other commands after SQL run there; one without arguments ends at its letter", async ({ page }) => {
  const input = await openShell(page, 'Local MySQL');
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };

  // \s runs; the buffer waits for its terminator, the block the
  // command's alone.
  await run('select 1 as a \\s');
  await expect(out.locator('.shell-kv-block').last()).toBeVisible({ timeout: 10_000 });
  await expect(input).toHaveValue('select 1 as a\n');
  await expect(out.locator('.shell-block').last()).not.toContainText('select 1 as a');
  await run('select 1 as a\n;');
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toBeVisible({ timeout: 10_000 });

  // \u takes the rest of its line.
  await run('select 2 as b \\u sakila');
  await expect(out.locator('.shell-meta-output', { hasText: /^Database changed$/ })).toBeVisible({ timeout: 10_000 });
  await expect(input).toHaveValue('select 2 as b\n');

  // After one without arguments the line reads on as SQL, joining the
  // buffer — and runs at its terminator, as the CLI runs it. (`\W`
  // turns the server's warnings on.)
  await run('select 3 as c \\W from dual;');
  await expect(out.locator('.shell-meta-output').filter({ hasText: /^Show warnings enabled\.$/ })).toBeVisible({ timeout: 10_000 });
  await expect(out.locator('.shell-table th', { hasText: /^c$/ })).toBeVisible({ timeout: 10_000 });
  await expect(input).toHaveValue('');

  // The line break typed before the command stays between them: the
  // buffer's line comment ends there. (After \s's reply.)
  await run('select 7 as g -- c\n\\s from dual;');
  await expect(out.locator('.shell-table th', { hasText: /^g$/ })).toBeVisible({ timeout: 10_000 });

  // \c clears the buffer; what follows it runs.
  await run('select 4 \\c select 5 as e;');
  await expect(out.locator('.shell-table th', { hasText: /^e$/ })).toBeVisible({ timeout: 10_000 });

  // A `;` alone after a command is let go.
  await run('\\s;');
  await expect(input).toHaveValue('');

  // A paste that starts with DELIMITER reads on with the new one.
  await run('delimiter //\nselect 6 as f //');
  await expect(out.locator('.shell-table th', { hasText: /^f$/ })).toBeVisible({ timeout: 10_000 });
  await run('delimiter ;');
});
