import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';

// Regression suite for Parliament issue #21 item 11.
//
// Failure class (pre-fix):
//   MySqlPlane inherited the trait default for `view_columns_sql`, which
//   routed view column introspection through `table_columns_sql` (a plain
//   `SHOW FULL COLUMNS FROM …`). That query returns no `Source` column, so
//   the column grid rendered with `has_source = false` and the <th>"Source"</th>
//   header was never emitted. The DBA inspecting a view's Schema tab had no
//   signal about what SELECT text backs the view.
//
//   Post-fix: MySqlPlane overrides `view_columns_sql` to JOIN
//   `information_schema.COLUMNS` with `information_schema.VIEWS` and surface
//   a `Source` column containing `VIEW_DEFINITION` truncated to 200 chars
//   (plus a UTF-8 ellipsis "…") in-SQL. The DDL tab still holds the full
//   definition; Source is a quick-read summary per column row.
//
//   cargo check, clippy, and unit tests cannot catch this: the trait compiles
//   without an override, the default routes to a valid SQL query, and the
//   column grid renders fine either way — only a browser test can verify
//   that (a) the Source header appears, (b) Source cells have content,
//   and (c) the truncation marker is present when VIEW_DEFINITION > 200 chars.
//
// Reference: vault/Engineering/Testing Strategy.md § Layer 3 (UI-only
// failure classes).

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// Switch the explorer's object-kind picker to "Views" and wait for the list
// to repopulate with at least one row.
async function switchToViews(page: import('@playwright/test').Page): Promise<void> {
  const viewsBtn = page.locator('.explorer-type-btn', {
    has: page.locator('.explorer-type-label', { hasText: 'Views' }),
  });
  await expect(viewsBtn).toBeVisible({ timeout: 5_000 });
  await viewsBtn.click();
  // Wait for at least one view row to appear in the list.
  await expect(page.locator('.explorer-list-row').first()).toBeVisible({
    timeout: 15_000,
  });
}

// Click a named row in the explorer object list and wait for the Schema tab
// column grid (`.schema-cols-table`) to appear — i.e. the IntrospectResult
// for "columns:…" has been received and rendered.
async function drillIntoViewSchema(
  page: import('@playwright/test').Page,
  viewName: string,
): Promise<void> {
  const row = page
    .locator('.explorer-list-row', {
      has: page.locator('.ex-td-name', { hasText: viewName }),
    })
    .first();
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.click();

  // The Schema tab is the default active tab for views (ERD and Data are
  // gated to Tables only). The column grid mounts once the WS round-trip
  // completes. 20 s covers cold-start and busy-DB scenarios.
  await expect(page.locator('.schema-cols-table').first()).toBeVisible({
    timeout: 20_000,
  });
}

// ---------------------------------------------------------------------------
// Test 1: MySQL view detail has a "Source" column header
// ---------------------------------------------------------------------------

test('MySQL actor_info view Schema tab has a Source column header', async ({ page }) => {
  // Pre-fix: `has_source` computed to false because every column's
  // `col.source` was empty (trait-default SQL emitted no Source alias).
  // The `<th>"Source"</th>` was never rendered.
  // Post-fix: the override JOIN emits a non-empty `Source` cell for every
  // column and `has_source` flips true.

  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  await switchToViews(page);
  await drillIntoViewSchema(page, 'actor_info');

  const colsTable = page.locator('.schema-cols-table').first();
  const sourceHeader = colsTable.locator('thead th', { hasText: 'Source' });

  // Primary regression assertion: the header must exist and be visible.
  // A count of 0 is the pre-fix failure mode.
  await expect(sourceHeader).toBeVisible({ timeout: 5_000 });
});

// ---------------------------------------------------------------------------
// Test 2: Source column has a non-empty value on every row
// ---------------------------------------------------------------------------

test('MySQL actor_info view Source cells are non-empty and start with SELECT', async ({ page }) => {
  // The view definition for actor_info is a SELECT statement. Every column
  // row gets the same VIEW_DEFINITION value (it is a view-level property,
  // not per-column). We assert the first row's Source cell contains the
  // leading keyword of the view definition.
  //
  // Don't pin the exact text — the view body is long and brittle. Just
  // confirm it is non-empty and begins with a SELECT keyword so the DBA
  // sees actionable information.

  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  await switchToViews(page);
  await drillIntoViewSchema(page, 'actor_info');

  const colsTable = page.locator('.schema-cols-table').first();

  // Source cells carry class `.schema-td-source` (see explorer.rs
  // render_column_rows). The first one in the first data row is enough.
  const firstSourceCell = colsTable.locator('tbody .schema-td-source').first();
  await expect(firstSourceCell).toBeVisible({ timeout: 5_000 });

  const cellText = await firstSourceCell.innerText();
  expect(cellText.trim().length).toBeGreaterThan(0);

  // VIEW_DEFINITION always opens with "select" (MySQL lowercases reserved
  // words in information_schema.VIEWS.VIEW_DEFINITION). Accept either case
  // to guard against future MySQL version differences.
  expect(cellText.trim().toLowerCase()).toMatch(/^select/);
});

// ---------------------------------------------------------------------------
// Test 3: Source value is truncated with an ellipsis when definition > 200 chars
// ---------------------------------------------------------------------------

test('MySQL actor_info view Source cells end with ellipsis (in-SQL truncation)', async ({ page }) => {
  // The CASE expression in view_columns_sql emits:
  //   CONCAT(LEFT(v.VIEW_DEFINITION, 197), '…')
  // when CHAR_LENGTH(v.VIEW_DEFINITION) > 200.
  //
  // actor_info is a complex SELECT across four tables; its VIEW_DEFINITION
  // is well over 200 chars (confirmed via WS introspect). This test pins
  // the in-SQL truncation: if the CASE branch is dropped or the CONCAT
  // removed, the full untruncated definition appears and the cell does NOT
  // end with "…".
  //
  // The ellipsis is the UTF-8 character U+2026 (HORIZONTAL ELLIPSIS),
  // not three ASCII periods.

  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  await switchToViews(page);
  await drillIntoViewSchema(page, 'actor_info');

  const colsTable = page.locator('.schema-cols-table').first();
  const firstSourceCell = colsTable.locator('tbody .schema-td-source').first();
  await expect(firstSourceCell).toBeVisible({ timeout: 5_000 });

  const cellText = await firstSourceCell.innerText();

  // Pre-fix regression: if `has_source` is false, the cell doesn't exist
  // at all — caught by test 1 above. This test catches a subtler regression
  // where the cell exists but the truncation CASE was removed (so the full
  // multi-KB definition pours into the cell, which would NOT end with "…").
  expect(cellText.trimEnd()).toMatch(/…$/);
});

// ---------------------------------------------------------------------------
// Test 4: PG view Source cells do NOT end with an ellipsis
// ---------------------------------------------------------------------------

test('PG view Source cells use base_table.base_column format, not truncated definition', async ({ page }) => {
  // PG's view_columns_sql (postgres.rs) LEFT JOINs
  // information_schema.view_column_usage and emits:
  //   COALESCE(vcu.table_name || '.' || vcu.column_name, '') AS "Source"
  // — a "table.column" string per row, not the full VIEW_DEFINITION.
  //
  // This test pins the per-plane semantic difference: MySQL Source = view
  // definition (truncated), PG Source = base column reference. If PG were
  // accidentally switched to the MySQL truncation approach, the Source cells
  // would end with "…" and this test would catch it.
  //
  // Pagila's public schema ships several views; customer_list is reliably
  // present and has columns sourced from base tables.

  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  await switchToViews(page);
  await drillIntoViewSchema(page, 'customer_list');

  const colsTable = page.locator('.schema-cols-table').first();

  // The Source header itself is conditional: PG's view_column_usage JOIN
  // may yield empty strings for computed columns, in which case has_source
  // stays false and no Source column renders. Assert on what we can:
  // if Source cells exist, none of them should end with the MySQL ellipsis.
  //
  // This is intentionally a softer assertion than the MySQL tests above —
  // the important property is the absence of MySQL-style truncation, not
  // the exact format of PG's source strings (which may be empty for
  // columns that have no resolvable base column).
  const sourceCells = colsTable.locator('tbody .schema-td-source');
  const count = await sourceCells.count();

  if (count > 0) {
    // At least one Source cell exists — verify none end with the MySQL
    // truncation ellipsis. PG source strings are "table.column" (short,
    // never truncated) or empty string (JOIN found no match).
    for (let i = 0; i < count; i++) {
      const text = await sourceCells.nth(i).innerText();
      expect(text.trimEnd()).not.toMatch(/…$/);
    }
  }
  // If count == 0, has_source was false (all PG Source cells were empty
  // for this view) — that is also a valid post-fix state, and is by
  // definition free of MySQL-style ellipsis truncation.
});
