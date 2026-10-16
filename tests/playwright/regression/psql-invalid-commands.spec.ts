import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// A backslash command psql doesn't have fails as psql fails it
// (`HandleSlashCmds`): `invalid command \foo` / `Try \? for help.`, the
// command and the rest of its line dropped, the query buffer typed
// before it kept — `select 1 as a \G` leaves `select 1 as a` waiting
// for its `;`. Recorded as typed, as psql's history keeps it. psql's
// own commands this shell can't run yet say so instead.

test('psql: an invalid command fails as psql fails it, the buffer kept', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const invalid = out.locator('.shell-entry-error', { hasText: 'invalid command' });

  // Alone: its block, psql's error under it, nothing left.
  await run('\\foo');
  await expect(invalid).toHaveCount(1, { timeout: 10_000 });
  await expect(invalid.first()).toHaveText('invalid command \\foo\nTry \\? for help.');
  await expect(out.locator('.shell-block').last()).toContainText('\\foo');
  await expect(input).toHaveValue('');

  // Up-arrow brings it back, as psql's history does.
  await input.press('ArrowUp');
  await expect(input).toHaveValue('\\foo');
  await input.fill('');

  // After SQL: the buffer stays and waits for its terminator.
  await run('select 1 as a \\G');
  await expect(invalid).toHaveCount(2, { timeout: 10_000 });
  await expect(invalid.last()).toHaveText('invalid command \\G\nTry \\? for help.');
  await expect(input).toHaveValue('select 1 as a\n');
  await run('select 1 as a\n;');
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toBeVisible({ timeout: 10_000 });

  // The rest of its line goes with it: the `\x` after it doesn't run.
  await run('\\foo bar \\x');
  await expect(invalid).toHaveCount(3, { timeout: 10_000 });
  await run('select 2 as b;');
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toBeVisible({ timeout: 10_000 });

  // A paste that starts with one: the lines after its own aren't its —
  // they run after it, as psql reads on.
  await run('\\foo\nselect 9 as z;');
  await expect(invalid).toHaveCount(4, { timeout: 10_000 });
  await expect(out.locator('.shell-table th', { hasText: /^z$/ })).toBeVisible({ timeout: 10_000 });
  await expect(input).toHaveValue('');

  // A `;` stuck to a name is part of it, as psql reads one: `\\x;` is
  // no command; `\\dt;` is `\\dt`, its letters read as psql reads them.
  await run('\\x;');
  await expect(invalid.last()).toHaveText('invalid command \\x;\nTry \\? for help.', { timeout: 10_000 });
  await run('\\dt;');
  await expect(out.locator('.shell-printed, .shell-table-wrap').last()).toContainText('actor', { timeout: 10_000 });

  // psql's own commands this shell doesn't run yet say so instead.
  await run('\\copy actor to stdout');
  await expect(out.locator('.shell-entry-error').last()).toHaveText("\\copy isn't supported here yet.", { timeout: 10_000 });
  await expect(input).toHaveValue('');
  await expect(invalid).toHaveCount(5);
});
