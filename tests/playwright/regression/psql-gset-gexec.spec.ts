import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// Regression suite for Parliament's `\gset` and `\gexec` meta-commands
// (shipped in PR #48). Both rely on the `pending_post_action`
// machinery that runs AFTER QueryResult lands — easy to leave in a
// stale state if the result-routing code regresses.
//
// What these tests pin:
//   1. `\gset prefix` on a single-row result binds each column into
//      a `\set` variable named `<prefix><column>`.
//   2. The variable is usable in subsequent `:varname` substitutions.
//   3. `\gset` on a multi-row result errors and does NOT set any
//      variables.
//   4. `\gexec` takes each non-NULL cell of a result and executes it
//      as further SQL.

test('\\gset prefix binds single-row columns into psql variables', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Bind a single-row result's columns into `gst_*` variables.
  await input.fill(
    "SELECT 'Alice' AS name, 42 AS age\n\\gset gst_",
  );
  await input.press('Enter');

  // psql's \gset says nothing when it binds; the values are there.
  await input.fill('\\echo :gst_name :gst_age');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toHaveText('Alice 42', { timeout: 8_000 });

  // Use the bound variable in a follow-up query. PG SELECT shows
  // the substituted value back.
  await input.fill("SELECT :'gst_name' AS hello;");
  await input.press('Enter');

  const out = visibleShellOutput(page);
  const table = out.locator('.shell-table-wrap').last();
  await expect(table).toBeVisible({ timeout: 10_000 });
  await expect(table).toContainText('Alice');
});

test('\\gset rejects multi-row result without setting variables', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Multi-row result — \gset must refuse. The PostAction::Gset arm
  // pushes a MetaOutput and returns early; no Table entry is
  // rendered for the producing query either (the post-action
  // handler short-circuits the normal render).
  await input.fill(
    'SELECT actor_id, first_name FROM actor LIMIT 3\n\\gset multi_',
  );
  await input.press('Enter');

  // psql's error, and no variable set.
  const out = visibleShellOutput(page);
  await expect(out.locator('.shell-entry-error').last()).toHaveText('more than one row returned for \\gset', {
    timeout: 8_000,
  });
  await input.fill('\\echo :multi_actor_id');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toHaveText(':multi_actor_id', { timeout: 8_000 });
});

test('\\gexec runs each non-NULL result cell as further SQL', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Two cells, each a valid SELECT. \gexec should fire BOTH as
  // their own queries. Trailing semicolons help split_statements
  // handle them as standalone statements.
  await input.fill(
    `SELECT 'SELECT ''alpha'' AS first;' AS q
UNION ALL
SELECT 'SELECT ''beta'' AS second;'
\\gexec`,
  );
  await input.press('Enter');

  // Both generated queries should produce result tables. Wait for
  // both result strings to appear in shell history.
  const out = visibleShellOutput(page);
  await expect(out).toContainText('alpha', { timeout: 15_000 });
  await expect(out).toContainText('beta', { timeout: 15_000 });
});

test('\\gexec on empty result does nothing (no errors, no extra tables)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  const out = visibleShellOutput(page);
  const before = await out.locator('.shell-table-wrap').count();

  await input.fill('SELECT NULL WHERE false\n\\gexec');
  await input.press('Enter');

  // Allow a beat for any post-action firing.
  await page.waitForTimeout(800);

  // The exec'd query itself produced an empty result Table (or an
  // Info entry) — that's the producer, not a \gexec'd statement.
  // No CRASH, no error spam.
  const errors = out.locator('.shell-entry-error');
  await expect(errors).toHaveCount(0);

  // Total table count should be at most +1 (the producer's empty
  // result). \gexec didn't dispatch additional queries.
  const after = await out.locator('.shell-table-wrap').count();
  expect(after - before).toBeLessThanOrEqual(1);
});
