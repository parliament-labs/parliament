import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { connectToPagila } from '../helpers/connect';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';
import { saveScreenshot } from '../helpers/paths';

// Three shots for the Flashy-README "Schema Explorer" section:
//   schema-01-table-list       — table list view
//   schema-02-table-detail     — full-screen table detail (Schema tab)
//   schema-03-view-structure   — a Pagila view with its definition
//
// Uses Local Postgres / Pagila — tables, views, and pg_get_viewdef
// output are all available without external seed state.
test('schema explorer — table list, table detail, view structure', async ({ page }) => {
  await openApp(page);
  await connectToPagila(page);
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // schema-01: Table list view. Full app shot so sidebar + explorer
  // list both show context.
  await saveScreenshot(page, 'schema-01-table-list', __filename);

  // schema-02: Table detail. Click the first table and wait for the
  // column + index sections to render, then capture.
  await page.locator('.explorer-list-row').first().click();
  await expect(
    page.locator('.schema-section-header', { hasText: 'Indexes' }).first(),
  ).toBeVisible({ timeout: 10_000 });
  await saveScreenshot(page, 'schema-02-table-detail', __filename);

  // schema-03: View structure. Switch the object-type selector to
  // Views, pick the first view, flip to the DDL tab if present so
  // the view definition is visible.
  await page
    .locator('.explorer-type-btn', {
      has: page.locator('.explorer-type-label', { hasText: 'Views' }),
    })
    .click();
  await waitForObjectList(page);
  await page.locator('.explorer-list-row').first().click();

  const ddlTab = page.locator('.detail-tab-btn', { hasText: 'DDL' });
  if (await ddlTab.isVisible().catch(() => false)) {
    await ddlTab.click();
    await expect(page.locator('.detail-ddl')).toContainText(/SELECT|CREATE/i, {
      timeout: 10_000,
    });
  }
  await saveScreenshot(page, 'schema-03-view-structure', __filename);
});
