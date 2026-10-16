import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// A MySQL result's footer reads as the mysql CLI's: "N rows in set",
// the server's warnings, "Empty set", and the time in seconds — on
// statements too ("Query OK, … (0.002 sec)").

test('MySQL result footers read as the mysql CLI prints them', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const footer = () => out.locator('.shell-stat, .shell-entry-info').last();
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };

  await run('SELECT actor_id FROM actor LIMIT 2;');
  await expect(footer()).toContainText(/^2 rows in set \(\d+\.\d{3} sec\)/, { timeout: 10_000 });
  await run('SELECT 1/0 AS x, 2/0 AS y;');
  await expect(footer()).toContainText(/^1 row in set, 2 warnings \(\d+\.\d{3} sec\)/, { timeout: 10_000 });
  await run("SELECT actor_id FROM actor WHERE actor_id = 'x';");
  await expect(footer()).toHaveText(/^Empty set, 1 warning \(\d+\.\d{3} sec\)$/, { timeout: 10_000 });
  await run('SELECT 1 AS a\\G');
  await expect(footer()).toContainText(/^1 row in set \(\d+\.\d{3} sec\)/, { timeout: 10_000 });
  await run('CREATE TEMPORARY TABLE t (a int);');
  await expect(footer()).toHaveText(/^Query OK, 0 rows affected \(\d+\.\d{3} sec\)$/, { timeout: 10_000 });
});
