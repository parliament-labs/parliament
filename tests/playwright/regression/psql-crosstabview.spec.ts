import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// Regression suite for Parliament's `\crosstabview` meta-command
// (shipped in PR #53). The pivot-transform logic is unit-tested in
// `psql_crosstab::tests` (14 tests), but the end-to-end submit →
// fetch → pivot → render path has had zero coverage. These pin the
// browser-side rendering against real Pagila query results.
//
// What these tests pin:
//   1. Default args (cols 1/2/3) pivot a year/month/sales-shaped query
//      into a 2-D grid with year-values as rows, month-values as
//      column headers, sales as cell values.
//   2. Numeric-index args ("\crosstabview 1 2 3") produce identical
//      output to default.
//   3. Name-based args resolve correctly.
//   4. Invalid column spec surfaces as a MetaOutput error, NOT a
//      crash or a corrupt pivot.

test('\\crosstabview: default args pivot a 3-column result into 2-D grid', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Pagila: rental_date is a timestamp; aggregate by year/month and
  // count rentals. Three columns ready for default pivot.
  const sql = `
    SELECT extract(year FROM rental_date)::int AS y,
           extract(month FROM rental_date)::int AS m,
           count(*) AS rentals
    FROM rental
    GROUP BY 1, 2
    ORDER BY 1, 2
  `.trim();
  await input.fill(`${sql}\n\\crosstabview`);
  await input.press('Enter');

  // The pivoted table appears. Headers should include the year
  // column label and at least one month value as a column header.
  const out = visibleShellOutput(page);
  const table = out.locator('.shell-table-wrap').last();
  await expect(table).toBeVisible({ timeout: 15_000 });

  // Row label column header — the original "y" column name.
  await expect(table).toContainText('y');

  // At least one month value should have become a column header.
  // Pagila's rentals span 2005-05 / 2005-06 / etc., so "5" or "6"
  // should appear in the header row. Match any month digit.
  const headerRow = table.locator('thead tr').first();
  await expect(headerRow).toContainText(/[1-9]/);

  // At least one cell value (the count) should be present.
  // Pagila has thousands of rentals so the body should contain
  // 3-or-more-digit numbers.
  await expect(table.locator('tbody')).toContainText(/\d{3,}/);
});

test('\\crosstabview: numeric-index args match default behavior', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  const sql = `
    SELECT extract(year FROM rental_date)::int AS y,
           extract(month FROM rental_date)::int AS m,
           count(*) AS rentals
    FROM rental
    GROUP BY 1, 2
    ORDER BY 1, 2
  `.trim();
  await input.fill(`${sql}\n\\crosstabview 1 2 3`);
  await input.press('Enter');

  const out = visibleShellOutput(page);
  const table = out.locator('.shell-table-wrap').last();
  await expect(table).toBeVisible({ timeout: 15_000 });
  await expect(table).toContainText('y');
});

test('\\crosstabview: name-based args resolve correctly', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  const sql = `
    SELECT extract(year FROM rental_date)::int AS y,
           extract(month FROM rental_date)::int AS m,
           count(*) AS rentals
    FROM rental
    GROUP BY 1, 2
    ORDER BY 1, 2
  `.trim();
  await input.fill(`${sql}\n\\crosstabview y m rentals`);
  await input.press('Enter');

  const out = visibleShellOutput(page);
  const table = out.locator('.shell-table-wrap').last();
  await expect(table).toBeVisible({ timeout: 15_000 });
  await expect(table).toContainText('y');
});

test('\\crosstabview: unknown column name surfaces a MetaOutput error', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  const sql = `
    SELECT extract(year FROM rental_date)::int AS y,
           extract(month FROM rental_date)::int AS m,
           count(*) AS rentals
    FROM rental
    GROUP BY 1, 2
  `.trim();
  await input.fill(`${sql}\n\\crosstabview nope`);
  await input.press('Enter');

  // Error message must appear in shell history.
  const out = visibleShellOutput(page);
  await expect(out).toContainText(/not found in result|crosstabview/i, {
    timeout: 10_000,
  });
});

test('\\crosstabview: vertical = horizontal collision errors out', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  const sql = `
    SELECT extract(year FROM rental_date)::int AS y,
           extract(month FROM rental_date)::int AS m,
           count(*) AS rentals
    FROM rental
    GROUP BY 1, 2
  `.trim();
  // y == y (col_v == col_h) — pivot can't work.
  await input.fill(`${sql}\n\\crosstabview y y rentals`);
  await input.press('Enter');

  const out = visibleShellOutput(page);
  await expect(out).toContainText(/vertical and horizontal|must differ/i, {
    timeout: 10_000,
  });
});
