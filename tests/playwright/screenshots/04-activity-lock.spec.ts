import { test, expect, type Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { connectToSakila } from '../helpers/connect';
import { openActivityPane, setActivityRefresh } from '../helpers/activity';
import { activatePaneByIndex, splitActivePane } from '../helpers/panes';
import { openMySql } from '../helpers/db';
import { saveScreenshot } from '../helpers/paths';

// Four shots for the Flashy-README Activity Pane section:
//   activity-01-open                — Activity Pane open, default view
//   activity-02-three-shells-split  — Activity over a row of 3 shells
//   activity-03-lock-wait-visible   — Lock contention after a refresh
//   activity-04-lock-scroll         — Scrolled Activity detail
//
// Mirrors regression/two-session-lock-wait-mysql.spec.ts for the
// concurrency scenario. Ends with a ROLLBACK + admin-path row restore
// so Sakila's actor table survives the run unchanged.

// Three shells are on screen at once, so the first visible one isn't
// the one just activated: the shell holding the keyboard is.
const focusedShell = (page: Page) => page.locator('.shell:focus-within');

async function submitInFocusedShell(page: Page, sql: string): Promise<void> {
  const input = focusedShell(page).locator('.shell-input-inline');
  await expect(input).toBeVisible();
  await input.fill(sql);
  await input.press('Enter');
}
test('activity pane — lock contention screenshots', async ({ page }) => {
  await openApp(page);
  await connectToSakila(page);

  // Convert the default pane to an Activity Pane.
  await openActivityPane(page);

  // Deterministic refresh cadence for shot 03.
  await setActivityRefresh(page, '5');

  // Shot 01: Activity Pane open, no lock traffic yet.
  await saveScreenshot(page, 'activity-01-open', __filename);

  // Horizontal split → Activity on top, new Shell pane below.
  await splitActivePane(page, 'horizontal');

  // The newest tab is the bottom shell. Activate it so the next split
  // targets the bottom half.
  const afterFirstSplit = await page.locator('.pane-tab').count();
  await activatePaneByIndex(page, afterFirstSplit - 1);

  // First vertical split of the bottom pane → 2 shells side-by-side.
  await splitActivePane(page, 'vertical');
  const afterSecondSplit = await page.locator('.pane-tab').count();
  await activatePaneByIndex(page, afterSecondSplit - 1);

  // Second vertical split → 3 shells side-by-side across the bottom row.
  await splitActivePane(page, 'vertical');
  const totalTabs = await page.locator('.pane-tab').count();

  // Bottom-row shell indices. The two most recent splits produced tabs
  // at totalTabs-2 and totalTabs-1; the original bottom shell sits at
  // totalTabs-3.
  const shell1Idx = totalTabs - 3;
  const shell2Idx = totalTabs - 2;

  // Shot 02: Activity on top, three shells below.
  await saveScreenshot(page, 'activity-02-three-shells-split', __filename);

  // ── Lock scenario ───────────────────────────────────────────────
  // Session 1: open a transaction and lock the actor rows whose
  // last_name = 'WOOD' (Sakila has exactly 2 such rows).
  await activatePaneByIndex(page, shell1Idx);
  await submitInFocusedShell(page, 'BEGIN;');
  await expect(focusedShell(page).locator('.shell-entry-info').last())
    .toContainText(/Query OK/i, { timeout: 5_000 });

  await submitInFocusedShell(
    page,
    "UPDATE actor SET last_name = 'STONE' WHERE last_name = 'WOOD';",
  );
  await expect(focusedShell(page).locator('.shell-entry-info').last())
    .toContainText(/2 rows affected/i, { timeout: 5_000 });

  // Session 2: contending UPDATE — blocks on session 1's row lock.
  await activatePaneByIndex(page, shell2Idx);
  await submitInFocusedShell(
    page,
    "UPDATE actor SET last_name = 'WATER' WHERE last_name = 'STONE';",
  );
  await expect(focusedShell(page).locator('.shell-entry-pending'))
    .toBeVisible({ timeout: 5_000 });

  // One full Activity refresh cycle plus a small cushion so the
  // blocker-tree visualization lands before we shoot.
  await page.waitForTimeout(5_500);

  // Shot 03: Lock wait visible in Activity Pane.
  await saveScreenshot(page, 'activity-03-lock-wait-visible', __filename);

  // Scroll the Activity pane's list to reveal additional session
  // detail. If the list isn't scrollable (short session list), the
  // fallback is another capture of the same state — still useful.
  const activityScroll = page
    .locator('.activity-shell .explorer-list-scroll')
    .first();
  if (await activityScroll.isVisible().catch(() => false)) {
    await activityScroll.evaluate((el) => {
      const target = Math.min(el.scrollHeight - el.clientHeight, 160);
      if (target > 0) el.scrollTop = target;
    });
    // Small settle before the shot.
    await page.waitForTimeout(200);
  }

  // Shot 04: Scrolled Activity Pane detail.
  await saveScreenshot(page, 'activity-04-lock-scroll', __filename);

  // ── Cleanup ─────────────────────────────────────────────────────
  // Release session 1's lock. Session 2's UPDATE will then proceed;
  // we ROLLBACK it too so actor data is not mutated by this spec.
  await activatePaneByIndex(page, shell1Idx);
  await submitInFocusedShell(page, 'ROLLBACK;');
  await expect(focusedShell(page).locator('.shell-entry-info').last())
    .toContainText(/Query OK/i, { timeout: 5_000 });

  await activatePaneByIndex(page, shell2Idx);
  await expect(focusedShell(page).locator('.shell-entry-pending'))
    .toHaveCount(0, { timeout: 15_000 });
  await submitInFocusedShell(page, 'ROLLBACK;');
  await expect(focusedShell(page).locator('.shell-entry-info').last())
    .toContainText(/Query OK/i, { timeout: 5_000 });

  // Belt-and-suspenders: admin-path restore in case anything committed
  // above. No-op if the table is already in its seed state.
  const admin = await openMySql();
  try {
    await admin.query(
      "UPDATE actor SET last_name = 'WOOD' WHERE last_name IN ('STONE','WATER')",
    );
  } finally {
    await admin.end();
  }
});
