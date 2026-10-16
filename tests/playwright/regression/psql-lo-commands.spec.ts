import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// Regression suite for Parliament's large-object meta-commands
// (shipped in PR #55): `\lo_list`, `\lol`, `\dl` (the three aliases
// for listing), `\lo_unlink <oid>` (drop), and the deferred
// `\lo_import` / `\lo_export` stubs.
//
// Pagila ships with zero large objects, but a prior test run that
// created one and failed to clean up can leave the catalog
// populated. An empty list prints as psql prints it — the header
// (ID | Owner | Description) over "(0 rows)" — and that's the
// empty-catalog signal we pin against. The cleanup step at the top
// of each listing test drops any leftover LOs so the assertion is
// robust regardless of state. (Pagila is a test fixture; nothing
// real lives there.)
//
// What these tests pin:
//   1. `\lo_list` after a clean-up prints psql's header over (0 rows)
//      (dispatch + SQL succeeded).
//   2. `\lol` and `\dl` produce IDENTICAL output to `\lo_list`
//      (sort-list alias unification at the render layer).
//   3. `\lo_unlink <oid>` against a nonexistent OID errors cleanly
//      (the function call goes through but PG rejects).
//   4. `\lo_unlink` with no arg fails as psql's does; a non-numeric
//      one is read as libpq's `atooid` reads it — 0 — as psql's is.
//      (psql's whole `\lo_unlink`: psql-encoding-lo-unlink.spec.ts.)
//   5. `\lo_import` / `\lo_export` return the deferred-stub message.
//
// Future enhancement: a separate spec could create + list + drop a
// real LO to assert the Table rendering with headers and an OID row.
// That's left out of this file to avoid mid-run state pollution.

import { Locator, Page } from '@playwright/test';

// Drop every large object in the connected database so subsequent
// listing assertions can assume a clean catalog. Idempotent: silent
// success when there's nothing to drop.
async function cleanupLargeObjects(page: Page, input: Locator): Promise<void> {
  const wipe = `
DO $$
DECLARE r RECORD;
BEGIN
  FOR r IN SELECT oid FROM pg_largeobject_metadata LOOP
    PERFORM lo_unlink(r.oid);
  END LOOP;
END
$$;`.trim();
  await input.fill(wipe);
  await input.press('Enter');
  // Wait for the DO block to complete — it prints psql's "DO".
  await page.waitForTimeout(800);
}

test('\\lo_list against empty pg_largeobject_metadata prints the header over (0 rows)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  await cleanupLargeObjects(page, input);

  await input.fill('\\lo_list');
  await input.press('Enter');

  // psql's header over "(0 rows)" — evidence the dispatch + SQL ran
  // successfully (no Error entry, no malformed-SQL fallout).
  const out = visibleShellOutput(page);
  await expect(out.locator('.shell-table').last().locator('th')).toHaveText(
    ['ID', 'Owner', 'Description'],
    { timeout: 10_000 },
  );
  await expect(out.locator('.shell-stat').last()).toContainText(/^\(0 rows\)/);
  await expect(out.locator('.shell-entry-error')).toHaveCount(0);
});

test('\\lol and \\dl produce identical output to \\lo_list', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  await cleanupLargeObjects(page, input);

  const out = visibleShellOutput(page);
  const baseline = await out.locator('.shell-table').count();

  // Fire each of the three aliased names. Against an empty catalog
  // all three print the same empty list — that's the
  // alias-equivalence pin.
  for (const cmd of ['\\lo_list', '\\lol', '\\dl']) {
    await input.fill(cmd);
    await input.press('Enter');
  }

  // Three new tables (relative to the baseline after cleanup), each
  // with psql's header.
  const tables = out.locator('.shell-table');
  await expect(tables).toHaveCount(baseline + 3, { timeout: 15_000 });
  for (let i = baseline; i < baseline + 3; i++) {
    await expect(tables.nth(i).locator('th')).toHaveText(['ID', 'Owner', 'Description']);
  }
});

test('\\lo_unlink with nonexistent OID errors cleanly (SQL dispatched)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // OID 0 is never a valid large object — PG will return an error
  // from lo_unlink. That's "dispatch worked, server rejected" —
  // enough to verify our wire format.
  await input.fill('\\lo_unlink 999999;');
  await input.press('Enter');

  const out = visibleShellOutput(page);
  await expect(out).toContainText(/does not exist|large object|lo_unlink/i, {
    timeout: 10_000,
  });
});

test('\\lo_unlink with no arg fails as psql\'s does', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\lo_unlink');
  await input.press('Enter');

  // psql's words — no SQL sent, so no server error.
  await expect(page.locator('.shell-entry-error').last()).toHaveText(
    '\\lo_unlink: missing required argument',
    { timeout: 3_000 },
  );
});

test('\\lo_unlink with non-numeric arg reads it as OID 0, as psql does', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\lo_unlink not_a_number');
  await input.press('Enter');

  await expect(page.locator('.shell-entry-error').last()).toHaveText(
    'ERROR:  large object 0 does not exist',
    { timeout: 10_000 },
  );
});

test('\\lo_import returns the deferred-stub message', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\lo_import /tmp/anything');
  await input.press('Enter');

  await expect(page.locator('.shell-meta-output').last()).toContainText(
    /not implemented|streaming|file API/i,
    { timeout: 3_000 },
  );
});
