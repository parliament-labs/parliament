import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { connectToPagila } from '../helpers/connect';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';
import { screenshotPath } from '../helpers/paths';

// Proof-of-life screenshot: Schema Explorer with DDL tab active on a Pagila
// table. No external state required — Pagila's built-in schema is enough.
test('schema explorer — DDL tab for a Pagila table', async ({ page }) => {
  await openApp(page);
  await connectToPagila(page);
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  await page.locator('.explorer-list-row').first().click();

  const ddlTab = page.locator('.detail-tab-btn', { hasText: 'DDL' });
  await expect(ddlTab).toBeVisible();
  await ddlTab.click();

  // Wait for the reconstructed CREATE TABLE text to land.
  const ddlBody = page.locator('.detail-ddl');
  await expect(ddlBody).toBeVisible();
  await expect(ddlBody).toContainText(/CREATE TABLE/i, { timeout: 10_000 });

  await page.locator('.app').screenshot({ path: screenshotPath('03-schema-explorer-ddl') });
});
