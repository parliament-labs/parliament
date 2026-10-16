import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// The mysql CLI's named commands (`status`, `use db`, `help …`), as mysql
// 9.6 reads them (`find_command`): a line typed alone, and — `add_line` —
// any statement its delimiter ends, wherever it stands: `status;` and
// `select 1; status; select 2;` run `status` instead of sending it.
// The name in any case; arguments only for a command that takes them
// (`status foo;` goes to the server); `go;` sends `go`.

test('mysql: a statement that is a named command runs as the command', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  // Status blocks (help's topic is a key/value block too).
  const status = out.locator('.shell-kv-block', { hasText: 'Connection id' });
  const errors = out.locator('.shell-entry-error');
  const order = () => out.locator('.shell-kv-block, .shell-table th')
    .evaluateAll(els => els.map(e => e.classList.contains('shell-kv-block') ? 'status' : e.textContent?.trim()));

  await run('status;');
  await expect(status).toHaveCount(1, { timeout: 10_000 });
  await expect(errors).toHaveCount(0);
  await expect(input).toHaveValue('');

  // In order, in any case, wherever the statement stands.
  await run('select 1 as a; Status ; select 3 as c;');
  await expect(out.locator('.shell-table th', { hasText: /^c$/ })).toBeVisible({ timeout: 10_000 });
  expect(await order()).toEqual(['status', 'a', 'status', 'c']);

  // `use db;` changes the database, then the line reads on.
  await run('use mysql; select database() as d;');
  await expect(out.locator('.shell-table td', { hasText: /^mysql$/ })).toBeVisible({ timeout: 10_000 });
  await run('use sakila;');
  await expect(out.locator('.shell-meta-output', { hasText: /^Database changed$/ })).toHaveCount(2, { timeout: 10_000 });

  // `help topic;` is the server's help on the topic.
  await run('help select;');
  await expect(out).toContainText("Name: 'SELECT'", { timeout: 10_000 });

  // Arguments to a command without any, and `go;`, go to the server.
  await run('status foo;');
  await expect(errors.last()).toContainText("ERROR 1064 (42000)", { timeout: 10_000 });
  await expect(errors.last()).toContainText("near 'status foo'");
  await run('go;');
  await expect(errors.last()).toContainText("near 'go'", { timeout: 10_000 });
  // `go` alone runs the empty buffer.
  await run('go');
  await expect(errors.last()).toHaveText('ERROR: \nNo query specified', { timeout: 10_000 });

  // `warnings;` turns the server's warnings on, as the CLI's does.
  await run('warnings;');
  await expect(out.locator('.shell-meta-output').last()).toHaveText('Show warnings enabled.', { timeout: 10_000 });
  // One this shell doesn't run says so.
  await run('resetconnection;');
  await expect(errors.last()).toHaveText("resetconnection isn't supported here yet.", { timeout: 10_000 });

  // Under another delimiter, that one ends the statement.
  await run('delimiter //');
  await run('status//');
  await expect(status).toHaveCount(3, { timeout: 10_000 });
  await run('delimiter ;');
});
