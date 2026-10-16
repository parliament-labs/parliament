import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';

// Regression suite for Parliament issue #21 item 3 — audit cluster D
// finding 3.3: `TableOverview` parser shape for PG-only fields.
//
// Failure class (pre-fix):
//   The `IntrospectResult` handler in `tyto/src/main.rs` used a `pick`
//   helper that always resolved to `String`, collapsing two semantically
//   distinct "no value" cases into the same empty string:
//     - "the plane didn't emit this column" (MySQL — `Owner` is not a
//       MySQL concept)
//     - "the plane emitted the column but the row resolved blank" (PG edge
//       case where `pg_get_userbyid` finds no role for the relowner OID)
//   Both cases suppressed the row in the overview grid because the renderer
//   drops empty strings — so the *display* was technically correct, but the
//   *type* carried no information about why the row was absent. A future UX
//   layer that wanted to surface "(not applicable on MySQL)" vs "(Owner
//   unresolved)" couldn't distinguish the two without re-plumbing.
//
// Post-fix: `pick_opt` returns `Option<String>`. `None` means "plane didn't
//   emit the column" (MySQL). `Some("")` means "emitted but blank" (PG
//   edge case). `Some(name)` means "render this value." The renderer's
//   v1 behavior is unchanged — both `None` and `Some("")` suppress the
//   row — but the type boundary is now correct for future layers.
//
// What these tests pin:
//   1. PG `public.actor` shows an Owner row in `.object-meta-grid`
//      (post-fix: `pick_opt("Owner")` → `Some("postgres")` or similar).
//   2. MySQL `sakila.actor` does NOT show an Owner row
//      (post-fix: `pick_opt("Owner")` → `None`; row suppressed).
//   3. Both explorers render the Engine cell (smoke: parse path intact).
//
// Reference: vault/Engineering/Testing Strategy.md § Layer 3 (UI-only
// failure classes).

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// Navigate to a named table in the schema-explorer object list.
// Waits for the overview (`object-meta-grid`) to appear in the inspector,
// which means the IntrospectResult round-trip has completed.
async function drillIntoTable(
  page: import('@playwright/test').Page,
  tableDisplayName: string,
): Promise<void> {
  const row = page
    .locator('.explorer-list-row', {
      has: page.locator('.ex-td-name', { hasText: tableDisplayName }),
    })
    .first();
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.click();

  // The object-meta-grid renders once the IntrospectResult WS message
  // comes back with the overview data. 15 s covers cold-start round trips.
  await expect(page.locator('.object-meta-grid').first()).toBeVisible({
    timeout: 15_000,
  });
}

// ---------------------------------------------------------------------------
// PG: actor table shows an Owner cell
// ---------------------------------------------------------------------------

test('PG actor overview renders an Owner cell', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  await drillIntoTable(page, 'actor');

  // The overview is inside `.object-meta-grid`. Each metadata entry is a
  // `.object-meta-cell` containing a `.object-meta-key` span (label) and a
  // `.object-meta-val` span (value). We locate the cell whose key reads
  // "Owner" and assert the whole cell exists and is visible.
  //
  // Pre-fix would have passed too for PG — but this assertion exists as the
  // paired guard for the MySQL assertion below: if we ever revert the
  // `Option` plumbing to a bare `String` approach, a different suppression
  // bug could appear on either side.
  const grid = page.locator('.object-meta-grid').first();
  const ownerCell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: 'Owner' }),
  });
  await expect(ownerCell).toBeVisible({ timeout: 5_000 });

  // The Owner value must be a non-empty string — we don't pin the exact
  // role name because the dev DB may be owned by "postgres", "admin", or
  // whatever the fixture DBA used. The absence of a blank val would be
  // unusual (it means pg_get_userbyid returned nothing), but not a
  // regression — the important check is that the cell is present at all.
  const ownerVal = ownerCell.locator('.object-meta-val');
  const valText = await ownerVal.innerText();
  expect(valText.trim().length).toBeGreaterThan(0);
});

// ---------------------------------------------------------------------------
// MySQL: actor table does NOT show an Owner cell
// ---------------------------------------------------------------------------

test('MySQL actor overview has no Owner cell', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  await drillIntoTable(page, 'actor');

  // `pick_opt("Owner")` returns None for MySQL because the MySQL plane's
  // table_overview_sql doesn't emit an "Owner" column. The renderer's
  // `if let Some(owner) = ov.owner.as_deref().filter(|s| !s.is_empty())`
  // guard skips the push, so no `.object-meta-cell` with key "Owner"
  // should appear.
  //
  // This is the primary regression check. Pre-fix relied on empty-string
  // suppression: if that suppression were accidentally removed (e.g. by a
  // refactor that changed the push guard), an "Owner" cell with a blank
  // value would appear and this assertion would catch it.
  const grid = page.locator('.object-meta-grid').first();
  const ownerCell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: 'Owner' }),
  });
  await expect(ownerCell).toHaveCount(0);
});

// ---------------------------------------------------------------------------
// PG: Engine cell present (parse-path smoke)
// ---------------------------------------------------------------------------

test('PG actor overview renders the Engine cell', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  await drillIntoTable(page, 'actor');

  // PG uses "heap" as the storage engine name. If the IntrospectResult
  // parse path were broken by the Option<String> type change, the entire
  // overview would fail to render. The Engine cell is the simplest
  // non-optional field to assert — it's always present on both DBs.
  const grid = page.locator('.object-meta-grid').first();
  const engineCell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: 'Engine' }),
  });
  await expect(engineCell).toBeVisible({ timeout: 5_000 });

  const engineVal = engineCell.locator('.object-meta-val');
  await expect(engineVal).toContainText('heap');
});

// ---------------------------------------------------------------------------
// MySQL: Engine cell present (parse-path smoke)
// ---------------------------------------------------------------------------

test('MySQL actor overview renders the Engine cell', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  await drillIntoTable(page, 'actor');

  // sakila.actor is InnoDB — confirmed by the Sakila seed schema.
  const grid = page.locator('.object-meta-grid').first();
  const engineCell = grid.locator('.object-meta-cell', {
    has: page.locator('.object-meta-key', { hasText: 'Engine' }),
  });
  await expect(engineCell).toBeVisible({ timeout: 5_000 });

  const engineVal = engineCell.locator('.object-meta-val');
  await expect(engineVal).toContainText('InnoDB');
});
