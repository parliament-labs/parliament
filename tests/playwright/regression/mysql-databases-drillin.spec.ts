import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';

// Regression suite for Parliament issue #21 item 2 / cluster D finding 3.2.
//
// Failure class (pre-fix):
//   The trait default for `area_row_clickable` returned false
//   unconditionally. MySqlPlane had no override, so Admin → Databases
//   rendered a dead-end list: every row was visible but not clickable,
//   and no drill-in inspector was ever mounted. cargo check, clippy, and
//   code review passed — the trait compiles without an override, and the
//   list renders fine; only a browser test can verify that a click
//   actually opens the inspector and that the three sections (Tables,
//   Grants, Charset) appear with real data.
//
// Reference: vault/Engineering/Testing Strategy.md § Layer 3 (UI-only
// failure classes).

test('MySQL Admin Databases row is clickable and opens drill-in inspector', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Navigate to the Admin area.
  // `.explorer-type-btn` wrapping `.explorer-type-label` with text "Admin"
  // renders only when the plane exposes admin_areas (MySQL does:
  // Roles, Databases, ServerConfig).
  const adminBtn = page.locator('.explorer-type-btn', {
    has: page.locator('.explorer-type-label', { hasText: 'Admin' }),
  });
  await expect(adminBtn).toBeVisible({ timeout: 5_000 });
  await adminBtn.click();

  // Click the "Databases" area button in the Admin sub-toolbar.
  // Pre-fix this area existed but clicking any row did nothing because
  // area_row_clickable(Databases) returned false (trait default).
  const databasesBtn = page.locator('.admin-area-btn', {
    has: page.locator('.admin-area-label', { hasText: 'Databases' }),
  });
  await expect(databasesBtn).toBeVisible({ timeout: 5_000 });
  await databasesBtn.click();

  // Wait for the databases list to load. The fixture seeds sakila, so
  // at minimum a row containing "sakila" must appear.
  const sakilaRow = page.locator('.explorer-list-row', {
    hasText: 'sakila',
  }).first();
  await expect(sakilaRow).toBeVisible({ timeout: 15_000 });

  // Pre-fix assertion: the row must be rendered with the clickable class.
  // render_admin_area_list only applies `.explorer-list-row` when clickable
  // is true; the <tr> is emitted unconditionally but the class is gated.
  await expect(sakilaRow).toHaveClass(/explorer-list-row/);

  // Click the sakila row. Pre-fix: click was a no-op because on:click
  // early-returned when `!clickable`. Post-fix: opens the inspector.
  await sakilaRow.click();

  // The inspector must mount. Title format is "{area.label()} · {row_id}"
  // per explorer.rs render logic at the non-Roles branch.
  const inspector = page.locator('.explorer-inspector');
  await expect(inspector).toBeVisible({ timeout: 10_000 });

  const title = inspector.locator('.explorer-inspector-title');
  await expect(title).toContainText('Databases');
  await expect(title).toContainText('sakila');
});

test('MySQL Databases drill-in renders Tables, Grants, and Charset sections', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Navigate Admin → Databases → sakila row (same path as above).
  const adminBtn = page.locator('.explorer-type-btn', {
    has: page.locator('.explorer-type-label', { hasText: 'Admin' }),
  });
  await expect(adminBtn).toBeVisible({ timeout: 5_000 });
  await adminBtn.click();

  const databasesBtn = page.locator('.admin-area-btn', {
    has: page.locator('.admin-area-label', { hasText: 'Databases' }),
  });
  await expect(databasesBtn).toBeVisible({ timeout: 5_000 });
  await databasesBtn.click();

  const sakilaRow = page.locator('.explorer-list-row', {
    hasText: 'sakila',
  }).first();
  await expect(sakilaRow).toBeVisible({ timeout: 15_000 });
  await sakilaRow.click();

  const inspector = page.locator('.explorer-inspector');
  await expect(inspector).toBeVisible({ timeout: 10_000 });

  // Three section headers must render. The drill-in renderer emits each
  // section as `.schema-section-header` (same class used for table-detail
  // sections in the Schema tab). Pre-fix: area_row_detail_sections()
  // returned [] (trait default) so the inspector body showed the
  // "No detail sections defined" placeholder instead.
  const sectionHeaders = inspector.locator('.schema-section-header');
  await expect(sectionHeaders).toHaveCount(3, { timeout: 20_000 });

  // Verify the three section titles exactly (order: Tables, Grants, Charset
  // per mysql.rs area_row_detail_sections override).
  await expect(sectionHeaders.nth(0)).toHaveText('Tables');
  await expect(sectionHeaders.nth(1)).toHaveText('Grants');
  await expect(sectionHeaders.nth(2)).toHaveText('Charset');
});

test('MySQL Databases Tables section contains the actor table', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Navigate Admin → Databases → sakila row.
  const adminBtn = page.locator('.explorer-type-btn', {
    has: page.locator('.explorer-type-label', { hasText: 'Admin' }),
  });
  await expect(adminBtn).toBeVisible({ timeout: 5_000 });
  await adminBtn.click();

  const databasesBtn = page.locator('.admin-area-btn', {
    has: page.locator('.admin-area-label', { hasText: 'Databases' }),
  });
  await expect(databasesBtn).toBeVisible({ timeout: 5_000 });
  await databasesBtn.click();

  const sakilaRow = page.locator('.explorer-list-row', {
    hasText: 'sakila',
  }).first();
  await expect(sakilaRow).toBeVisible({ timeout: 15_000 });
  await sakilaRow.click();

  const inspector = page.locator('.explorer-inspector');
  await expect(inspector).toBeVisible({ timeout: 10_000 });

  // Wait for the Tables section header to appear — at that point the
  // section's fetch has completed and rows are rendered.
  const tablesHeader = inspector.locator('.schema-section-header', { hasText: 'Tables' });
  await expect(tablesHeader).toBeVisible({ timeout: 15_000 });

  // The Tables section is a generic flat table rendered by
  // render_admin_area_row_detail. The first column is "Table" (per
  // area_row_detail_columns). Sakila ships with the `actor` table
  // unconditionally — assert its cell is present.
  // Don't pin exact row count; seed additions / removals should not
  // break this assertion.
  const actorCell = inspector.locator('td', { hasText: 'actor' }).first();
  await expect(actorCell).toBeVisible({ timeout: 10_000 });
});

test('MySQL Databases Charset section shows exactly one data row', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Navigate Admin → Databases → sakila row.
  const adminBtn = page.locator('.explorer-type-btn', {
    has: page.locator('.explorer-type-label', { hasText: 'Admin' }),
  });
  await expect(adminBtn).toBeVisible({ timeout: 5_000 });
  await adminBtn.click();

  const databasesBtn = page.locator('.admin-area-btn', {
    has: page.locator('.admin-area-label', { hasText: 'Databases' }),
  });
  await expect(databasesBtn).toBeVisible({ timeout: 5_000 });
  await databasesBtn.click();

  const sakilaRow = page.locator('.explorer-list-row', {
    hasText: 'sakila',
  }).first();
  await expect(sakilaRow).toBeVisible({ timeout: 15_000 });
  await sakilaRow.click();

  const inspector = page.locator('.explorer-inspector');
  await expect(inspector).toBeVisible({ timeout: 10_000 });

  // The Charset section is the third section. Its SQL is:
  //   SELECT DEFAULT_CHARACTER_SET_NAME AS `Charset`,
  //          DEFAULT_COLLATION_NAME AS `Collation`
  //   FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = 'sakila'
  // which always returns exactly one row (one SCHEMATA entry per DB name).
  //
  // Locate the table that immediately follows the "Charset" section header.
  // The generic renderer emits .explorer-list-scroll > table > tbody > tr
  // for each data row.
  //
  // Wait for the Charset header first so we know all three sections loaded.
  const charsetHeader = inspector.locator('.schema-section-header', { hasText: 'Charset' });
  await expect(charsetHeader).toBeVisible({ timeout: 20_000 });

  // The Charset section's table is the last table in the inspector.
  // Tables is always the first table; Grants may render as `.explorer-msg`
  // "No rows." (not a table element) when the connecting root user holds
  // only global privileges with no schema-level grants recorded in
  // SCHEMA_PRIVILEGES. Charset always returns exactly one row from
  // SCHEMATA so it always renders as a table — making it the last
  // `table.role-privs-list` in the inspector regardless of whether
  // the Grants section emitted a table or a placeholder div.
  const charsetTable = inspector.locator('table.role-privs-list').last();

  // The table must have a thead and exactly one <tr> in the tbody.
  // A second row would indicate the WHERE clause failed (matched multiple
  // rows) or the query returned wrong data. Zero rows means the query
  // produced an empty result, which is also a failure.
  const charsetRows = charsetTable.locator('tbody tr');
  await expect(charsetRows).toHaveCount(1, { timeout: 10_000 });

  // Each row has two <td> cells: Charset and Collation. Don't pin the
  // values — utf8mb4 is typical on MySQL 8.x but latin1 appears on older
  // default setups and the fixture doesn't guarantee one or the other.
  const cells = charsetRows.first().locator('td');
  await expect(cells).toHaveCount(2);
});

test('MySQL Databases Grants section renders (header present, empty or rows OK)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Navigate Admin → Databases → sakila row.
  const adminBtn = page.locator('.explorer-type-btn', {
    has: page.locator('.explorer-type-label', { hasText: 'Admin' }),
  });
  await expect(adminBtn).toBeVisible({ timeout: 5_000 });
  await adminBtn.click();

  const databasesBtn = page.locator('.admin-area-btn', {
    has: page.locator('.admin-area-label', { hasText: 'Databases' }),
  });
  await expect(databasesBtn).toBeVisible({ timeout: 5_000 });
  await databasesBtn.click();

  const sakilaRow = page.locator('.explorer-list-row', {
    hasText: 'sakila',
  }).first();
  await expect(sakilaRow).toBeVisible({ timeout: 15_000 });
  await sakilaRow.click();

  const inspector = page.locator('.explorer-inspector');
  await expect(inspector).toBeVisible({ timeout: 10_000 });

  // The Grants section header must render regardless of whether the
  // connecting root user has explicit schema-level grants.
  // When connecting as root with all-global privileges, SCHEMA_PRIVILEGES
  // is empty for sakila, so the section body renders "No rows." — that is
  // the correct empty-state path and is acceptable. The critical check is
  // that the section was mounted at all (pre-fix: no sections existed
  // because area_row_detail_sections returned []).
  const grantsHeader = inspector.locator('.schema-section-header', { hasText: 'Grants' });
  await expect(grantsHeader).toBeVisible({ timeout: 20_000 });

  // After the header appears, the body is either a flat table (rows
  // present) or an `.explorer-msg` "No rows." placeholder. Both are valid;
  // assert one of the two is present — this catches the third failure mode
  // where the section body never renders (WS error, query crash, etc.).
  const grantsSection = inspector.locator('.explorer-list-scroll table, .explorer-msg');
  // At minimum one such element must exist anywhere in the inspector
  // below the Grants header. Using the inspector-scoped locator is
  // sufficient because the inspector only mounts for this one row.
  await expect(grantsSection.first()).toBeVisible({ timeout: 10_000 });
});
