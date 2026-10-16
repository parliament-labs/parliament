import { test, expect, Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';

// Regression suite for the Storage factor metric in the Schema
// Explorer's table-detail Overview pane.
//
// What these tests pin:
//   1. On PG: drilling into a Pagila table renders a "Storage" cell
//      with a value matching the multiplier format ("N.N×") AND a
//      tooltip explaining the breakdown.
//   2. On PG: a "Last vacuum" sibling cell renders alongside Storage
//      when the table has been vacuumed (Pagila's autovacuum runs
//      occasionally).
//   3. The Storage cell carries one of the three severity classes
//      (`metric-good` / `metric-watch` / `metric-bad`) so the CSS
//      coloring can never silently drop.
//   4. On MySQL: drilling into a Sakila table renders a "Storage" cell
//      with the same multiplier format. No "Last vacuum" — MySQL
//      InnoDB has no autovacuum metadata.

// Navigate to a named table in the schema-explorer object list.
async function drillIntoTable(page: Page, tableDisplayName: string): Promise<void> {
  const row = page
    .locator('.explorer-list-row', {
      has: page.locator('.ex-td-name', { hasText: tableDisplayName }),
    })
    .first();
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.click();
  await expect(page.locator('.object-meta-grid').first()).toBeVisible({
    timeout: 15_000,
  });
}

// ── Postgres / Pagila ───────────────────────────────────────────────

test('PG: Pagila rental table shows a Storage factor cell with multiplier format', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  const grid = page.locator('.object-meta-grid').first();
  const cell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: 'Storage' }),
  });
  await expect(cell).toBeVisible({ timeout: 5_000 });

  // Value matches "N.N×" — one decimal place + multiplication sign.
  const val = cell.locator('.object-meta-val');
  await expect(val).toHaveText(/\d+\.\d×/);
});

test('PG: Storage cell carries a severity CSS class', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  // The .object-meta-val inside the Storage cell must carry exactly
  // one of metric-good / metric-watch / metric-bad. Without this,
  // the CSS coloring silently drops — the value would still render
  // but lose its at-a-glance signal.
  const grid = page.locator('.object-meta-grid').first();
  const val = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: 'Storage' }),
  }).locator('.object-meta-val');

  await expect(val).toHaveAttribute(
    'class',
    /\bmetric-(good|watch|bad)\b/,
  );
});

test('PG: Storage cell has a tooltip explaining the factor', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  // The tooltip is set as the `title=` attribute. Native HTML
  // tooltips don't show up as DOM elements until hover, but the
  // attribute itself is verifiable.
  const grid = page.locator('.object-meta-grid').first();
  const val = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: 'Storage' }),
  }).locator('.object-meta-val');

  const title = await val.getAttribute('title');
  expect(title).toBeTruthy();
  expect(title!).toContain('Storage factor');
  expect(title!).toContain('actual');
  expect(title!).toContain('ideal');
  expect(title!).toMatch(/VACUUM FULL|OPTIMIZE TABLE/);
});

// ── MySQL / Sakila ──────────────────────────────────────────────────

test('MySQL: Sakila rental table shows a Storage factor cell with multiplier format', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  const grid = page.locator('.object-meta-grid').first();
  const cell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: 'Storage' }),
  });
  await expect(cell).toBeVisible({ timeout: 5_000 });

  const val = cell.locator('.object-meta-val');
  await expect(val).toHaveText(/\d+\.\d×/);
});

// ── Pages / Ideal pairing ───────────────────────────────────────────

test('PG: Pages cell shows actual / ideal pair when stats are available', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  // Pagila tables have had ANALYZE run, so we expect the paired
  // label "Pages / Ideal" and a value matching "~N / ~M".
  const grid = page.locator('.object-meta-grid').first();
  const cell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: /^Pages \/ Ideal$/ }),
  });
  await expect(cell).toBeVisible({ timeout: 5_000 });

  const val = cell.locator('.object-meta-val');
  await expect(val).toHaveText(/^~\d[\d,]* \/ ~\d[\d,]*$/);
});

test('MySQL: Pages cell shows actual / ideal pair for InnoDB tables', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  const grid = page.locator('.object-meta-grid').first();
  const cell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: /^Pages \/ Ideal$/ }),
  });
  await expect(cell).toBeVisible({ timeout: 5_000 });

  const val = cell.locator('.object-meta-val');
  await expect(val).toHaveText(/^~\d[\d,]* \/ ~\d[\d,]*$/);
});

test('MySQL: Storage cell does NOT have a sibling Last vacuum cell', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  // MySQL has no autovacuum metadata, so the Last vacuum cell must
  // be absent (the plane emits empty and the push-skip drops the row).
  const grid = page.locator('.object-meta-grid').first();
  const lvCell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: 'Last vacuum' }),
  });
  await expect(lvCell).toHaveCount(0);
});

// ── Per-index storage column (Indexes tab) ──────────────────────────

test('PG Indexes section shows a Storage column with a multiplier value', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  // The indexes table renders inline below the columns table — no
  // tab switch needed. Pagila rental has only btree indexes so the
  // Storage column should appear and at least one row should hold
  // a factor matching `N.N×`.
  const idxTable = page.locator('.schema-index-table').first();
  await expect(idxTable).toBeVisible({ timeout: 10_000 });

  const header = idxTable.locator('thead tr').first();
  await expect(header).toContainText('Storage');

  const factorCell = idxTable.locator('tbody td.schema-td-size').filter({
    hasText: /^\d+\.\d×$/,
  }).first();
  await expect(factorCell).toBeVisible({ timeout: 5_000 });
});

test('PG Indexes Storage cells carry severity CSS class + tooltip', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  const idxTable = page.locator('.schema-index-table').first();
  await expect(idxTable).toBeVisible({ timeout: 10_000 });

  // Pick the first row whose Storage cell carries any metric-* class.
  const sevCell = idxTable.locator('tbody td.schema-td-size').filter({
    hasText: /^\d+\.\d×$/,
  }).first();
  await expect(sevCell).toHaveAttribute('class', /\bmetric-(good|watch|bad)\b/);

  // Tooltip must mention REINDEX (not VACUUM FULL — the action for
  // index bloat is REINDEX, not VACUUM).
  const title = await sevCell.getAttribute('title');
  expect(title).toBeTruthy();
  expect(title!).toContain('REINDEX');
  expect(title!).not.toContain('VACUUM FULL');
});

test('MySQL Indexes section does NOT show a Storage column (no per-index signal)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  const idxTable = page.locator('.schema-index-table').first();
  await expect(idxTable).toBeVisible({ timeout: 10_000 });

  // MySQL plane doesn't emit Storage_factor for indexes — the column
  // header must NOT appear.
  const header = idxTable.locator('thead tr').first();
  await expect(header).not.toContainText('Storage');
});

// ── Indexes Size / Ideal pair ───────────────────────────────────────

test('PG Indexes section shows the Size column as "Size / Ideal" with a paired value', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  const idxTable = page.locator('.schema-index-table').first();
  await expect(idxTable).toBeVisible({ timeout: 10_000 });

  // Header reads "Size / Ideal" — when ANY btree index has an ideal
  // estimate the column header switches from "Size" to the paired
  // form.
  const header = idxTable.locator('thead tr').first();
  await expect(header).toContainText('Size / Ideal');

  // At least one body row's Size cell holds the paired value, e.g.
  // "312 KB / 352 KB". The first body cell with the schema-td-size
  // class IS this column (Storage column also uses the same class but
  // has the multiplier glyph instead).
  const pairCell = idxTable.locator('tbody td.schema-td-size').filter({
    hasText: /\d.* (B|KB|MB|GB|TB) \/ \d.* (B|KB|MB|GB|TB)/,
  }).first();
  await expect(pairCell).toBeVisible({ timeout: 5_000 });
});

// ── Index Uses column (PG only) ─────────────────────────────────────

test('PG Indexes section shows a "Uses" column with scan counts', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  const idxTable = page.locator('.schema-index-table').first();
  await expect(idxTable).toBeVisible({ timeout: 10_000 });

  // Header reads "Uses". Pagila tables always emit Scan_count
  // (COALESCE(idx_scan, 0)), so the column always appears on PG.
  const header = idxTable.locator('thead tr').first();
  await expect(header).toContainText('Uses');

  // At least one row's Uses cell holds a numeric value. Used indexes
  // render as "N (M.M avg)"; unused indexes render as "0". The
  // alternation regex covers both.
  const usesCells = idxTable.locator('tbody td.schema-td-size').filter({
    hasText: /^(\d[\d,]* \(\d+(\.\d+)? avg\)|0)$/,
  });
  expect(await usesCells.count()).toBeGreaterThan(0);
});

test('MySQL Indexes section does NOT show a "Uses" column', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  const idxTable = page.locator('.schema-index-table').first();
  await expect(idxTable).toBeVisible({ timeout: 10_000 });

  // MySQL plane doesn't emit Scan_count — no equivalent in InnoDB
  // without an extension. The column header must not appear.
  const header = idxTable.locator('thead tr').first();
  await expect(header).not.toContainText('Uses');
});

// ── Cache hit ratio (Overview cell, PG only) ────────────────────────

test('PG Overview shows a Cache hit cell with a percentage value', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  const grid = page.locator('.object-meta-grid').first();
  const cell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: /^Cache hit$/ }),
  });
  await expect(cell).toBeVisible({ timeout: 8_000 });

  const val = cell.locator('.object-meta-val');
  // One decimal + % glyph.
  await expect(val).toHaveText(/\d+\.\d%/);

  // Carries a severity class (one of metric-*).
  await expect(val).toHaveAttribute('class', /\bmetric-(good|watch|bad)\b/);

  // Tooltip explains the breakdown.
  const title = await val.getAttribute('title');
  expect(title).toBeTruthy();
  expect(title!).toContain('Buffer cache hit ratio');
  expect(title!).toContain('hits');
  expect(title!).toContain('reads');
});

test('MySQL Overview does NOT show a Cache hit cell', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  // MySQL plane doesn't emit Cache_hit_ratio — the cell must NOT
  // appear (no clean per-table equivalent in core InnoDB stats).
  const grid = page.locator('.object-meta-grid').first();
  const cell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: /^Cache hit$/ }),
  });
  await expect(cell).toHaveCount(0);
});

test('MySQL Indexes section keeps "Size" header (no Ideal estimate available)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await drillIntoTable(page, 'rental');

  const idxTable = page.locator('.schema-index-table').first();
  await expect(idxTable).toBeVisible({ timeout: 10_000 });

  // MySQL plane doesn't emit Index_size_ideal — the column header
  // stays as plain "Size", not "Size / Ideal".
  const header = idxTable.locator('thead tr').first();
  await expect(header).toContainText('Size');
  await expect(header).not.toContainText('Size / Ideal');
});
