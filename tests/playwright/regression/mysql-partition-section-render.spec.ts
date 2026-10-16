import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection, connectToPagila } from '../helpers/connect';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';

// The Schema tab's Partitions section renders for MySQL as for Postgres
// (issue #21 item 7 claimed MySQL showed a count and nothing more; it
// doesn't — `render_partitions_section` gates only clickability on
// `partitions_are_navigable()`). sakila.test_partitions is RANGE by year
// with 3 partitions; the PG payment table is the navigable-plane control.
//
// The section loads in its own round trip after the columns, so each
// check waits for it rather than looking once.
//
// Reference: vault/Engineering/Testing Strategy.md § Layer 3.

test('MySQL Schema tab renders Partitions section for sakila.test_partitions', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Search the table list for test_partitions.
  const searchInput = page.locator('.explorer-search-input');
  await expect(searchInput).toBeVisible({ timeout: 5_000 });
  await searchInput.fill('test_partitions');

  // Wait for the filtered list to show test_partitions.
  const tableRow = page.locator('.explorer-list-row', { hasText: 'test_partitions' }).first();
  await expect(tableRow).toBeVisible({ timeout: 10_000 });
  await tableRow.click();

  // The inspector should open with the Schema tab active by default.
  const inspector = page.locator('.explorer-inspector');
  await expect(inspector).toBeVisible({ timeout: 10_000 });

  // Click Schema tab explicitly to ensure we are on the right tab.
  const schemaTab = inspector.locator('.detail-tab-btn', { hasText: 'Schema' });
  await expect(schemaTab).toBeVisible({ timeout: 5_000 });
  await schemaTab.click();

  // Wait for columns to load — columns are always present and signal that
  // the Schema tab body has fully rendered its synchronous sections.
  const columnsHeader = inspector.locator('.schema-section-header', { hasText: 'Columns' });
  await expect(columnsHeader).toBeVisible({ timeout: 15_000 });

  // The partitions section arrives in its own round trip: wait for it, up
  // to the columns' budget.
  const partitionsHeader = inspector.locator('.schema-section-header', { hasText: 'Partitions' });
  await expect(partitionsHeader, 'Partitions section for sakila.test_partitions').toBeVisible({ timeout: 15_000 });

  // test_partitions has 3 partitions (p2022, p2023, p2024).
  const tbodyRows = inspector.locator('table.partitions-table tbody tr');
  await expect(tbodyRows).toHaveCount(3, { timeout: 5_000 });
});

test('PG Schema tab renders Partitions section for payment (control)', async ({ page }) => {
  await openApp(page);
  await connectToPagila(page);
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Search for the payment partitioned root table.
  const searchInput = page.locator('.explorer-search-input');
  await expect(searchInput).toBeVisible({ timeout: 5_000 });
  await searchInput.fill('payment');

  // Click the row whose data-name is exactly "payment" (not payment_p2022_01 etc).
  // Each list row carries data-name=<table-name> so we can match exactly.
  const tableRow = page.locator('.explorer-list-row[data-name="payment"]').first();
  await expect(tableRow).toBeVisible({ timeout: 10_000 });
  await tableRow.click();

  const inspector = page.locator('.explorer-inspector');
  await expect(inspector).toBeVisible({ timeout: 10_000 });

  // Click Schema tab.
  const schemaTab = inspector.locator('.detail-tab-btn', { hasText: 'Schema' });
  await expect(schemaTab).toBeVisible({ timeout: 5_000 });
  await schemaTab.click();

  // Wait for columns to confirm the schema body is up.
  const columnsHeader = inspector.locator('.schema-section-header', { hasText: 'Columns' });
  await expect(columnsHeader).toBeVisible({ timeout: 15_000 });

  // PG Partitions section must render and be clickable (navigable plane).
  const partitionsHeader = inspector.locator('.schema-section-header', { hasText: 'Partitions' });
  await expect(partitionsHeader).toBeVisible({ timeout: 15_000 });

  const partitionsTable = inspector.locator('table.partitions-table');
  await expect(partitionsTable).toBeVisible({ timeout: 5_000 });

  // Pagila seeds 7 monthly payment partitions.
  const tbodyRows = partitionsTable.locator('tbody tr');
  await expect(tbodyRows).not.toHaveCount(0, { timeout: 5_000 });

  // PG partitions are navigable: each name cell should be a .rel-link span,
  // not a .rel-cross or plain text.
  const firstNameCell = tbodyRows.first().locator('td.schema-td-name span');
  await expect(firstNameCell).toHaveClass(/rel-link/);
});
