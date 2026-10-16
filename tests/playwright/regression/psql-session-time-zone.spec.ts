import { test, expect, Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// The time zone a Postgres session asks for: this browser's choice (kept
// in its localStorage, `session:timezone`), above PGTZ in noctua's
// environment, above the server's setting — the closer to the user, the
// stronger. Unset, a session gets what psql gets. The browser here runs
// in Tokyo, which neither noctua's machine nor the server is in.

test.use({ timezoneId: 'Asia/Tokyo' });

async function newShellSays(page: Page, sql: string) {
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  await input.fill(sql);
  await input.press('Enter');
  return visibleShellOutput(page);
}

// The zone, and its offset that day — whatever DateStyle noctua's
// PGDATESTYLE or the server sets.
const ZONE = "select current_setting('TimeZone') as tz, extract(timezone_hour from '2026-10-02 12:34:56+02'::timestamptz)::int as hours;";

test('a session asks for the zone this browser chose, else PGTZ, else the server', async ({ page }) => {
  await openApp(page);
  const choose = (value: string | null) => page.evaluate(v => {
    if (v === null) localStorage.removeItem('session:timezone'); else localStorage.setItem('session:timezone', v);
  }, value);
  await clickConnection(page, 'Local Postgres');

  // Default: not the browser's zone — PGTZ or the server's, as psql.
  await choose(null);
  let out = await newShellSays(page, ZONE);
  await expect(out.locator('.shell-table td').first()).not.toHaveText('', { timeout: 10_000 });
  await expect(out.locator('.shell-table td').first()).not.toHaveText('Asia/Tokyo');

  // This browser's.
  await choose('browser');
  out = await newShellSays(page, ZONE);
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText(['Asia/Tokyo', '9'], { timeout: 10_000 });

  // A zone by name.
  await choose('UTC');
  out = await newShellSays(page, ZONE);
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText(['UTC', '0'], { timeout: 10_000 });

  // One the server doesn't know: the session is refused in its words.
  await choose('Nowhere/Land');
  await addShellPane(page);
  await expect(page.locator('.shell-output:visible, .shell:visible').first())
    .toContainText('invalid value for parameter "TimeZone": "Nowhere/Land"', { timeout: 10_000 });

  // The Settings dialog shows this browser's choice and zone.
  await choose('browser');
  await page.locator('button[title="Settings"]').click();
  await expect(page.locator('#settings-timezone')).toHaveValue('browser');
  await expect(page.locator('#settings-timezone option[value="browser"]')).toHaveText("This browser's (Asia/Tokyo)");
  await page.keyboard.press('Escape');
  await choose(null);
});
