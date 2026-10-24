import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { lockActorRow } from '../helpers/db';
import { ensureShellPane, visibleShellInput } from '../helpers/panes';

// Regression for the hidden deadlock fixed in commit 9501d4f. An
// external MySQL connection row-locks actor_id=1 inside a transaction.
// The UI then issues a conflicting UPDATE on the same row. The pending
// indicator must become visible (Defect 1 from the plan — shell
// in-flight feedback) and the query must eventually complete after the
// external lock is released (Defect 2 — backend concurrency so the
// reader isn't stuck).
//
// Requires MySQL running with Sakila seeded. Set PW_MYSQL_PASSWORD if
// the root password isn't empty.
test('UI UPDATE blocked by external lock shows pending, then completes on release', async ({ page }) => {
  const lockHolder = await lockActorRow();
  try {
    await openApp(page);
    await clickConnection(page, 'Local MySQL');

    // A shell to issue SQL in, whatever the connection opens with.
    await ensureShellPane(page);
    const shellInput = visibleShellInput(page);
    await expect(shellInput).toBeVisible();

    await shellInput.fill("UPDATE actor SET first_name = 'PENDING' WHERE actor_id = 1;");
    await shellInput.press('Enter');

    // Phase 1: the in-flight indicator must appear within ~1s.
    const pending = page.locator('.shell-entry-pending').last();
    await expect(pending).toBeVisible({ timeout: 5_000 });

    // Phase 2: while the UPDATE is blocked, release the external lock.
    // The reader loop must still be free to process this — if the
    // pre-fix serial handler were in place, the backend would be stuck
    // inside execute().await and the COMMIT wouldn't even need to come
    // from the UI to expose the deadlock. This commit from outside the
    // WebSocket proves the server-side lock release works; the UI
    // visibility of the resulting QueryResult proves the reader
    // processed the response.
    await lockHolder.commit();

    // The UPDATE should complete shortly after the lock releases.
    await expect(pending).toBeHidden({ timeout: 15_000 });

    // The Info result block (DML, no columns) should now follow the
    // Query entry — "Query OK, 1 row affected".
    await expect(page.locator('.shell-entry-info').last()).toContainText(
      /Query OK, 1 row affected/i,
      { timeout: 5_000 },
    );
  } finally {
    // Release the lock if we crashed before the in-test commit, then
    // restore actor_id=1's first_name so the test is idempotent across
    // runs, and close the admin connection.
    try { await lockHolder.rollback(); } catch { /* already committed */ }
    try { await lockHolder.query("UPDATE actor SET first_name = 'PENELOPE' WHERE actor_id = 1"); } catch { /* ignore */ }
    try { await lockHolder.end(); } catch { /* ignore */ }
  }
});
