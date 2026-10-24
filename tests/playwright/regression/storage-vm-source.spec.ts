import { test, expect, Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';

// The Storage tab's Visibility map reads pg_visibility_map_summary() when
// the pg_visibility extension is installed, and pg_class's snapshot when
// it isn't. A role that isn't allowed to run the function (a
// non-superuser without EXECUTE on it) used to get the server's
// "permission denied" in place of the section: now the explorer reads
// pg_class instead and says why — every table, however many reads were in
// flight when the first refusal came back. Which path a run takes depends
// on the connection's role; the annotation says which one ran.

const listRow = (page: Page, table: string) =>
  page.locator('.explorer-list-row', { has: page.locator('.ex-td-name', { hasText: new RegExp(`^${table}$`) }) }).first();

async function openExplorer(page: Page) {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
}

// `table`'s Visibility map, read from its Storage tab: its source badge,
// and its error if the section shows one in place of the pages.
async function visibilityMap(page: Page, table: string) {
  await listRow(page, table).click();
  await expect(page.locator('.explorer-inspector-title:visible', { hasText: `· ${table}` })).toBeVisible({ timeout: 10_000 });
  await page.locator('.detail-tab-btn:visible', { hasText: /^Storage$/ }).click();
  const header = page.locator('.schema-section-header', { hasText: /^Visibility map/ });
  await expect(header).toBeVisible({ timeout: 15_000 });
  return {
    badge: header.locator('.storage-vm-source'),
    error: header.locator('xpath=following-sibling::*[1][contains(@class, "detail-error")]'),
  };
}

test('PG: the Visibility map shows a source and its pages, never a permission error', async ({ page }) => {
  await openExplorer(page);
  const { badge, error } = await visibilityMap(page, 'rental');
  await expect(badge).toHaveText(/^via (pg_visibility|pg_class)$/, { timeout: 15_000 });
  await expect(error).toHaveCount(0);

  const refused = (await badge.getAttribute('title'))!.includes("isn't allowed to run");
  test.info().annotations.push({ type: 'path', description: `${await badge.textContent()}${refused ? ', pg_visibility refused to the role' : ''}` });
  if (refused) {
    await expect(badge).toHaveText('via pg_class');
    await expect(page.locator('.av-prose', { hasText: 'pg_visibility' })).toContainText("isn't allowed to run pg_visibility_map_summary()");
  }
  // All-visible pages of the total, whichever source.
  await expect(page.locator('table.av-table tr', { has: page.locator('td.av-key', { hasText: /^All-visible$/ }) }))
    .toContainText(/ of /);
});

test('PG: tables opened back to back each show their Visibility map', async ({ page }) => {
  await openExplorer(page);
  // Both tables' reads go out before the first reply can say anything:
  // a refusal must send each to pg_class, not just the first.
  await listRow(page, 'film').click();
  await listRow(page, 'actor').click();
  for (const table of ['actor', 'film']) {
    const { badge, error } = await visibilityMap(page, table);
    await expect(badge, table).toHaveText(/^via (pg_visibility|pg_class)$/, { timeout: 15_000 });
    await expect(error, table).toHaveCount(0);
    // The race only runs when the role is refused; say whether it did.
    const refused = (await badge.getAttribute('title'))!.includes("isn't allowed to run");
    test.info().annotations.push({ type: 'path', description: `${table}: ${await badge.textContent()}${refused ? ', pg_visibility refused to the role' : ''}` });
  }
});
