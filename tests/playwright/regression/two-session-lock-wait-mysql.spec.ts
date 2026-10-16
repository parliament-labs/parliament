import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import {
  addShellPane,
  activatePaneByIndex,
  submitInActivePane,
  visibleShellOutput,
} from '../helpers/panes';
import { openMySql } from '../helpers/db';

// End-to-end regression for the backend concurrency fix (commit 9501d4f).
//
// Before the fix, handle_socket processed WebSocket messages serially.
// While session 2's UPDATE was blocked inside process_message.await
// waiting for session 1's lock, the socket reader could not pick up
// any further message from the browser — including session 1's COMMIT.
// That was a hard deadlock: the only action that could unblock
// session 2 could not reach the server.
//
// This spec reproduces the full scenario inside one browser context:
// two shell panes on the same Local MySQL connection (each with its
// own physical MySqlConnection), session 1 holds a row lock inside
// a transaction, session 2 attempts a conflicting UPDATE, and we
// COMMIT from session 1 *through the UI* — which requires the reader
// task to still be free. If the backend regresses to serial, this
// spec times out long before MySQL's innodb_lock_wait_timeout (~50s)
// can mask it.
//
// Requires Sakila seeded. Override PW_MYSQL_PASSWORD if needed.
test('two UI sessions — session 1 COMMIT unblocks session 2 UPDATE', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');

  // Establish two shell panes on top of whatever the connection's
  // default_pane opened (could be Shell or Schema Explorer).
  await addShellPane(page);
  await addShellPane(page);

  const totalTabs = await page.locator('.pane-tab').count();
  const session1Idx = totalTabs - 2;
  const session2Idx = totalTabs - 1;

  // ── Session 1: open transaction, grab lock on actor_id=1 ─────────
  await activatePaneByIndex(page, session1Idx);
  await submitInActivePane(page, 'BEGIN;');
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toContainText(/Query OK/i, { timeout: 5_000 });

  await submitInActivePane(
    page,
    "UPDATE actor SET first_name = 'PW_SESS1' WHERE actor_id = 1;",
  );
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toContainText(/1 row affected/i, { timeout: 5_000 });

  // ── Session 2: conflicting UPDATE must block ─────────────────────
  await activatePaneByIndex(page, session2Idx);
  await submitInActivePane(page, 'BEGIN;');
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toContainText(/Query OK/i, { timeout: 5_000 });

  await submitInActivePane(
    page,
    "UPDATE actor SET first_name = 'PW_SESS2' WHERE actor_id = 1;",
  );

  // Pending indicator appears in session 2. This proves the frontend
  // in-flight feedback is wired correctly.
  await expect(visibleShellOutput(page).locator('.shell-entry-pending'))
    .toBeVisible({ timeout: 5_000 });

  // ── Session 1: COMMIT via the UI ─────────────────────────────────
  // This is the critical step. On the pre-fix serial backend the
  // reader would be stuck inside session 2's execute.await and this
  // COMMIT would never be processed; pane 1's prompt would advance
  // in the UI but the backend wouldn't see the message. The spec
  // would then hang and time out waiting for session 2 to unblock.
  await activatePaneByIndex(page, session1Idx);
  await submitInActivePane(page, 'COMMIT;');
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toContainText(/Query OK/i, { timeout: 5_000 });

  // ── Session 2: UPDATE completes shortly after session 1 committed
  await activatePaneByIndex(page, session2Idx);
  await expect(visibleShellOutput(page).locator('.shell-entry-pending'))
    .toHaveCount(0, { timeout: 10_000 });
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toContainText(/1 row affected/i, { timeout: 5_000 });

  // ── Cleanup: release session 2's transaction + restore row ──────
  await submitInActivePane(page, 'ROLLBACK;');
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toContainText(/Query OK/i, { timeout: 5_000 });

  // Restore actor_id=1 from whatever state the test left it in. Use a
  // direct mysql2 connection so cleanup is independent of UI state.
  const admin = await openMySql();
  try {
    await admin.query(
      "UPDATE actor SET first_name = 'PENELOPE' WHERE actor_id = 1",
    );
  } finally {
    await admin.end();
  }
});
