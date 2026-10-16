import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// The mysql CLI reads `\` and one letter as its command, whatever follows
// it: `\usakila` is `use sakila`, `\d//` sets the delimiter `//`, `\sf x`
// is `\s` (status) and then `f x` as SQL, `\cxx` clears and leaves `xx`.
// (`\endif` is its `\e` — the editor — so it isn't run here.)

test('mysql: a command is its letter; what follows is its argument or SQL', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };

  // Arguments right after the letter.
  await run('\\umysql');
  await expect(out.locator('.shell-meta-output', { hasText: /^Database changed$/ })).toHaveCount(1, { timeout: 10_000 });
  await run('select database() as d;');
  await expect(out.locator('.shell-table td', { hasText: /^mysql$/ })).toBeVisible({ timeout: 10_000 });
  await run('\\usakila');
  await expect(out.locator('.shell-meta-output', { hasText: /^Database changed$/ })).toHaveCount(2, { timeout: 10_000 });

  await run('\\d//');
  await run('select 1 as a //');
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toBeVisible({ timeout: 10_000 });
  await run('delimiter ;');

  // `\sf x` is `\s`: its status, then `f x` waiting as SQL.
  const kv = out.locator('.shell-kv-block');
  const before = await kv.count();
  await run('\\sf x');
  await expect(kv).toHaveCount(before + 1, { timeout: 10_000 });
  await expect(input).toHaveValue('f x');

  // `\cxx` clears, then `xx` reads on.
  await run('\\cxx');
  await expect(input).toHaveValue('xx');

  // `\h`'s topic follows a space: `\hselect` is plain `\h`.
  await run('\\hselect');
  await expect(out.locator('.shell-meta-output').last()).toContainText('List of all MySQL commands', { timeout: 10_000 });
});
