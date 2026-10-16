import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import {
  addShellPane,
  activatePaneByIndex,
  submitInActivePane,
  visibleShellOutput,
} from '../helpers/panes';
import { lockActorRow, openMySql } from '../helpers/db';

// Merit-badge regression: while UI pane 1 is blocked behind an
// EXTERNAL lock-holder (a direct mysql2 client outside our app),
// UI pane 2 on the same connection should still run independent
// queries to completion. This is the "a session is a session is
// a session" property — the backend read/write split must treat
// external blockers identically to internal ones.
//
// On the pre-fix serial backend, the reader was stuck inside
// pane 1's execute().await, so pane 2's SELECT 1 would never
// be dispatched. Here we assert pane 2's result arrives within
// a second while pane 1's UPDATE is still pending.
test('other UI panes stay responsive while one pane blocks on an external lock', async ({ page }) => {
  const lockHolder = await lockActorRow();
  try {
    await openApp(page);
    await clickConnection(page, 'Local MySQL');

    await addShellPane(page);
    await addShellPane(page);

    const totalTabs = await page.locator('.pane-tab').count();
    const pane1Idx = totalTabs - 2;
    const pane2Idx = totalTabs - 1;

    // Pane 1: attempt conflicting UPDATE. Will block on the external
    // lockHolder transaction.
    await activatePaneByIndex(page, pane1Idx);
    await submitInActivePane(
      page,
      "UPDATE actor SET first_name = 'MERIT_PANE1' WHERE actor_id = 1;",
    );
    await expect(visibleShellOutput(page).locator('.shell-entry-pending'))
      .toBeVisible({ timeout: 5_000 });

    // Pane 2: issue a tiny, unrelated query that does NOT touch
    // actor_id=1. If the backend reader were stuck inside pane 1's
    // await, this message would sit unread in the socket buffer. On
    // the fixed backend it runs on its own spawned task and returns
    // promptly.
    await activatePaneByIndex(page, pane2Idx);
    await submitInActivePane(page, 'SELECT 1;');
    await expect(visibleShellOutput(page).locator('.shell-table'))
      .toBeVisible({ timeout: 3_000 });

    // Sanity: pane 1 is still blocked (its pending indicator is in
    // the DOM even though it's hidden from pane 2's active view).
    // Re-activate pane 1 to confirm.
    await activatePaneByIndex(page, pane1Idx);
    await expect(visibleShellOutput(page).locator('.shell-entry-pending'))
      .toBeVisible();

    // Release the external lock. Pane 1 unblocks.
    await lockHolder.commit();
    await expect(visibleShellOutput(page).locator('.shell-entry-pending'))
      .toHaveCount(0, { timeout: 10_000 });
    await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
      .toContainText(/1 row affected/i, { timeout: 5_000 });
  } finally {
    try { await lockHolder.rollback(); } catch { /* already committed */ }
    try { await lockHolder.end(); } catch { /* ignore */ }
    // Restore via independent admin connection; pane 1 may have
    // autocommitted 'MERIT_PANE1' into the row.
    const admin = await openMySql();
    try {
      await admin.query("UPDATE actor SET first_name = 'PENELOPE' WHERE actor_id = 1");
    } finally {
      await admin.end();
    }
  }
});
