import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\x` expands every result, each statement of a multi-statement
// input included — not only an input's single statement.

test('\\x expands every statement of a batch', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);

  await input.fill('\\x');
  await input.press('Enter');
  await expect(out).toContainText('Expanded display is on.', { timeout: 5_000 });

  await input.fill('select 1 as a; select 2 as b;');
  await input.press('Enter');
  await expect(out.locator('.shell-vertical-block')).toHaveCount(2, { timeout: 10_000 });
  await expect(out.locator('.shell-table')).toHaveCount(0);
});
