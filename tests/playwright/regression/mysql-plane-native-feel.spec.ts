import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';
import { addShellPane, visibleShellInput } from '../helpers/panes';

// Regression suite for Parliament issue #10 phase 1 — MySQL plane
// native-feel improvements.
//
// Failure classes (pre-fix):
//   1. Schema-picker label — MySQL's SchemaExplorer showed "Schema" for
//      the database-picker dropdown. MySQL's concept is "database", not
//      "schema" (they are synonyms at the server level, but the UX term
//      that MySQL users know is "database"). The label is driven by
//      `plane.schema_picker_label()` which now returns "Database" for
//      MySQL and retains "Schema" for Postgres.
//   2. SHOW keyword highlight — MySQL's most-used meta-verb (`SHOW
//      DATABASES`, `SHOW TABLES`, `SHOW CREATE TABLE`) rendered without
//      major-keyword colour because `SHOW` was absent from the universal
//      `MAJOR_KEYWORDS_UNIVERSAL` list and the MySQL plane had no
//      override. The plane now supplies `MYSQL_MAJOR_KEYWORDS` which
//      includes SHOW, USE, EXPLAIN, and ANALYZE. Audit finding 4 of
//      issue #10.
//   3. MySQL superuser banner — clicking `root@localhost` in the Admin →
//      Roles panel and opening the Privileges sub-tab showed an empty
//      matrix and no "superuser bypasses checks" banner because
//      `role_is_superuser()` had no MySQL override (trait default
//      returned false unconditionally). The MySQL plane now reads
//      `Super_priv = 'Y'` from `mysql.user` and emits "Yes" in the
//      `Superuser` column, which `role_is_superuser()` checks. Audit
//      finding 1 of issue #10.
//
// Reference: vault/Engineering/Testing Strategy.md § Layer 3 (UI-only
// failure classes).

test('MySQL Schema Explorer dropdown label reads "Database"', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // The label sits above the schema/database picker dropdown.
  // Only one `.explorer-control-label` exists in the explorer header
  // for MySQL (Postgres has a second one for its DB picker). We want
  // an exact equality check, not a substring — pre-fix it read "Schema".
  const label = page.locator('.explorer-control-label').first();
  await expect(label).toBeVisible({ timeout: 5_000 });
  await expect(label).toHaveText('Database');
});

test('PG Schema Explorer dropdown label reads "Schema"', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Postgres has two `.explorer-control-label` elements — "Database"
  // for the PG-database picker at the top and "Schema" for the
  // schema-filter below. The schema picker label is the last one.
  // Pre-fix it would have been rendered by the same `schema_picker_label()`
  // default so this assertion protects the Postgres path from regression
  // if the default were ever inadvertently changed.
  const labels = page.locator('.explorer-control-label');
  const count = await labels.count();
  // The schema-picker label is whichever is last in the list.
  const schemaLabel = labels.nth(count - 1);
  await expect(schemaLabel).toHaveText('Schema');
});

test('MySQL SHOW keyword renders with major-keyword style (sql-kw-major)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');

  // Ensure we have a shell pane. The fixture default_pane for Local MySQL
  // is "shell", but addShellPane is safe to call regardless — if a shell
  // is already active it creates a second tab, which is harmless.
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Submit SHOW DATABASES. The shell highlights the query text as it
  // is rendered into the history entry.
  await input.fill('SHOW DATABASES;');
  await input.press('Enter');

  // Wait for the query to appear in the shell history. Both the history
  // entry and the live-input overlay use `.shell-entry-query` and both
  // live inside `.shell-output`. History entries use `.shell-sql` to
  // wrap their highlighted tokens; the live-input overlay uses
  // `.shell-input-highlight` + `.shell-input-inline` instead. Scoping
  // to `.shell-sql` therefore isolates the history side.
  const shellOutput = page.locator('.shell-output').first();
  const shellSql = shellOutput.locator('.shell-sql').last();
  await expect(shellSql).toBeVisible({ timeout: 10_000 });

  // The SHOW token must carry .sql-kw-major — same class SELECT gets.
  // Pre-fix: SHOW was absent from MySqlPlane.major_keywords() so it
  // tokenised as plain text and received no colour treatment at all.
  // This assertion checks the class directly rather than computed colour
  // because the tokeniser emits class names, not inline styles.
  const showToken = shellSql.locator('.sql-kw-major', { hasText: 'SHOW' }).first();
  await expect(showToken).toBeVisible({ timeout: 5_000 });
  await expect(showToken).toHaveClass(/sql-kw-major/);
});

test('MySQL root@localhost shows superuser banner on Privileges tab', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Navigate to Admin (the ⛭ button in the type bar).
  // `.explorer-type-btn` with text "Admin" — rendered only when the
  // plane exposes admin_areas (MySQL does: Roles, Databases, ServerConfig).
  const adminBtn = page.locator('.explorer-type-btn', {
    has: page.locator('.explorer-type-label', { hasText: 'Admin' }),
  });
  await expect(adminBtn).toBeVisible({ timeout: 5_000 });
  await adminBtn.click();

  // The Admin sub-toolbar appears. Click the "Roles" area button.
  // MySQL's admin_areas are [Roles, Databases, ServerConfig]; "Roles"
  // is the first and may already be active, but clicking explicitly is
  // safe and deterministic.
  const rolesBtn = page.locator('.admin-area-btn', {
    has: page.locator('.admin-area-label', { hasText: 'Roles' }),
  });
  await expect(rolesBtn).toBeVisible({ timeout: 5_000 });
  await rolesBtn.click();

  // Wait for the roles list to load. The Sakila fixture runs as root so
  // `mysql.user` is accessible; `root@localhost` is always present.
  // We match by the name column — `.ex-td-name` containing "root".
  const rootRow = page.locator('.explorer-list-row', {
    has: page.locator('.ex-td-name', { hasText: 'root' }),
  }).first();
  await expect(rootRow).toBeVisible({ timeout: 15_000 });
  await rootRow.click();

  // The role detail panel opens. Click the "Privileges" sub-tab.
  // The tab button contains the text "Privileges".
  const privTab = page.locator('.role-detail-tab', { hasText: 'Privileges' });
  await expect(privTab).toBeVisible({ timeout: 5_000 });
  await privTab.click();

  // The superuser banner must be visible. Pre-fix: `role_is_superuser()`
  // had no MySQL override so it always returned false, and the banner
  // at `.role-super-banner` was never rendered. The banner text begins
  // with "Superuser —" (see explorer.rs line ~1881).
  const banner = page.locator('.role-super-banner');
  await expect(banner).toBeVisible({ timeout: 10_000 });
  // Confirm the text is the superuser-bypass explanation, not some other
  // banner (e.g. a revoke-confirmation banner from the data tab).
  await expect(banner).toContainText(/superuser/i);
  await expect(banner).toContainText(/bypass/i);
});
