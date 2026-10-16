import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import {
  addShellPane,
  activatePaneByIndex,
  submitInActivePane,
  visibleShellOutput,
} from '../helpers/panes';
import { openPg } from '../helpers/db';

// Postgres equivalent of two-session-lock-wait-mysql.spec.ts. See that
// file for the full rationale. The PG variant is load-bearing in a
// subtle way: PG's `lock_timeout` defaults to 0 (wait forever), so
// session 2's UPDATE has no MySQL-style 50s escape hatch — the only
// way it unblocks is if session 1's COMMIT actually reaches the
// backend through the shared WebSocket. A regression of the
// concurrency fix would hang this spec indefinitely (until Playwright's
// test timeout fires).
test('two UI sessions — session 1 COMMIT unblocks session 2 UPDATE (Postgres)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');

  await addShellPane(page);
  await addShellPane(page);

  const totalTabs = await page.locator('.pane-tab').count();
  const session1Idx = totalTabs - 2;
  const session2Idx = totalTabs - 1;

  // ── Session 1: open transaction, grab lock on actor_id=1 ─────────
  await activatePaneByIndex(page, session1Idx);
  await submitInActivePane(page, 'BEGIN;');
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toHaveText(/^BEGIN\b/, { timeout: 5_000 });

  await submitInActivePane(
    page,
    "UPDATE actor SET first_name = 'PW_SESS1' WHERE actor_id = 1;",
  );
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toHaveText(/^UPDATE 1\b/, { timeout: 5_000 });

  // ── Session 2: conflicting UPDATE must block ─────────────────────
  await activatePaneByIndex(page, session2Idx);
  await submitInActivePane(page, 'BEGIN;');
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toHaveText(/^BEGIN\b/, { timeout: 5_000 });

  await submitInActivePane(
    page,
    "UPDATE actor SET first_name = 'PW_SESS2' WHERE actor_id = 1;",
  );

  await expect(visibleShellOutput(page).locator('.shell-entry-pending'))
    .toBeVisible({ timeout: 5_000 });

  // ── Session 1: COMMIT via the UI ─────────────────────────────────
  await activatePaneByIndex(page, session1Idx);
  await submitInActivePane(page, 'COMMIT;');
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toHaveText(/^COMMIT\b/, { timeout: 5_000 });

  // ── Session 2: UPDATE completes shortly after session 1 committed
  await activatePaneByIndex(page, session2Idx);
  await expect(visibleShellOutput(page).locator('.shell-entry-pending'))
    .toHaveCount(0, { timeout: 10_000 });
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toHaveText(/^UPDATE 1\b/, { timeout: 5_000 });

  // ── Cleanup ──────────────────────────────────────────────────────
  await submitInActivePane(page, 'ROLLBACK;');
  await expect(visibleShellOutput(page).locator('.shell-entry-info').last())
    .toHaveText(/^ROLLBACK\b/, { timeout: 5_000 });

  const admin = await openPg();
  try {
    await admin.query(
      "UPDATE actor SET first_name = 'PENELOPE' WHERE actor_id = 1",
    );
  } finally {
    await admin.end();
  }
});
