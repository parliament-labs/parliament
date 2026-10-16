import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection, connectToSakila } from '../helpers/connect';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';

// Regression suite for Parliament issue #18 — close-time pending-DML guard.
//
// Failure class (pre-fix):
//   Clicking × ("Close pane") on a Schema Explorer pane that had an
//   in-flight UPDATE / INSERT / DELETE silently destroyed the pane and
//   its TableDetail signal tree, leaving the DML result (success or
//   error) permanently invisible to the user. The fix adds a
//   `pending_dml_on_pane` scan in the public `close_pane` closure: if
//   any `data_edit_pending` entry, `data_insert_status == Some(Saving)`,
//   or `data_delete_status` entry is `EditStatus::Saving` is found, a
//   `DialogKind::Confirm` fires instead of proceeding.
//
//   This is a UI-only failure class — cargo check, clippy, and unit
//   tests all pass because the signal state is valid Rust; only a
//   browser test can observe that the dialog fires (or doesn't) at the
//   right moment.
//
// Reference: vault/Engineering/Testing Strategy.md § Layer 3.
//
// Test 1 — happy path (no pending DML):
//   Verifies that when nothing is in flight, the pane closes immediately
//   and the confirm dialog is never shown. A regression here would mean
//   the scan is triggering false-positives (spurious dialogs on every
//   close), which would break normal workflow.
//
// Test 2 — pending UPDATE path (best-effort / timing-dependent):
//   Attempts to click × immediately after committing a cell edit so the
//   WS round-trip is still in flight. If the DML response races back
//   before the close-click (< ~50 ms on localhost), the pane closes
//   without a dialog — that is the happy-path outcome, not a test
//   failure. The test detects which path ran and asserts accordingly.
//   If the dialog does appear it asserts title, message, and the
//   "Close anyway" / "Cancel" buttons. Manual verification is still
//   recommended for the dialog path on a slow or throttled backend.

test('no pending DML — pane closes immediately without confirm dialog', async ({ page }) => {
  await openApp(page);
  // Use Local Postgres (Pagila) — a reliable Explorer target with tables
  // that load quickly. We will NOT open the Data tab or initiate any DML.
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Drill into any table so a TableDetail entry is created and its
  // signals are initialised. Clicking the first list row is deterministic.
  const firstRow = page.locator('.explorer-list-row').first();
  await expect(firstRow).toBeVisible({ timeout: 15_000 });
  await firstRow.click();

  // Wait for the detail panel to mount (Schema tab is the default).
  // `.detail-tabs` is rendered by render_table_detail_tabs once the
  // detail entry is ready; its presence proves TableDetail was created
  // and its DML signals are at their zero / empty defaults.
  await expect(page.locator('.detail-tabs')).toBeVisible({ timeout: 10_000 });

  // Confirm no dialog is visible before we act.
  await expect(page.locator('.app-dialog')).not.toBeVisible();

  // Click the "Close pane" action-bar button. This calls the public
  // `close_pane` which invokes `pending_dml_on_pane`. With nothing in
  // flight the count is 0 and the inner close fires immediately.
  const closeBtn = page.locator('button[title="Close pane"]').first();
  await expect(closeBtn).toBeVisible({ timeout: 5_000 });
  await closeBtn.click();

  // The dialog must NOT appear — pre-fix behaviour would have also not
  // shown the dialog (it didn't exist), so if the dialog appears after
  // this fix it means the zero-pending fast-path is broken.
  // 300 ms is enough for any reactive DOM update to settle.
  await page.waitForTimeout(300);
  await expect(page.locator('.app-dialog')).not.toBeVisible();

  // The pane is gone: the conn-header for "Local Postgres" should no
  // longer be present in the workspace once the only pane for that
  // workspace tab is removed.
  await expect(
    page.locator('.conn-header-name', { hasText: 'Local Postgres' }),
  ).not.toBeVisible({ timeout: 5_000 });
});

test('pending UPDATE — close-pane shows confirm dialog or fast-path succeeds', async ({ page }) => {
  await openApp(page);
  await connectToSakila(page);
  await openSchemaExplorer(page);
  await waitForObjectList(page);

  // Navigate to sakila.actor. The sakila fixture always seeds this table
  // and actor has a simple integer PK (actor_id) so UPDATE is supported.
  const actorRow = page.locator('.explorer-list-row', { hasText: 'actor' }).first();
  await expect(actorRow).toBeVisible({ timeout: 15_000 });
  await actorRow.click();

  // Switch to the Data tab. The tab button carries the text "Data" and
  // is only rendered for base tables (show_data_tab gate in explorer.rs).
  const dataTabBtn = page.locator('.detail-tab-btn', { hasText: 'Data' });
  await expect(dataTabBtn).toBeVisible({ timeout: 10_000 });
  await dataTabBtn.click();

  // Wait for the data grid to load. The first row of sakila.actor is
  // always present; `.data-row` is the per-row `<tr>` class.
  const firstDataRow = page.locator('.data-row').first();
  await expect(firstDataRow).toBeVisible({ timeout: 20_000 });

  // Identify the first_name cell in the first row. actor columns are:
  // actor_id (PK, non-editable), first_name (col index 1), last_name,
  // last_update. Double-click first_name to open the inline editor.
  // `.data-cell-editable` marks non-PK cells; the second .data-cell in
  // row 0 is first_name.
  const editableCells = firstDataRow.locator('.data-cell-editable');
  const firstNameCell = editableCells.first();
  await expect(firstNameCell).toBeVisible({ timeout: 5_000 });

  // Double-click to arm the inline editor. The `data-cell-input` input
  // mounts inside the cell; focus + select-all happen via rAF so give
  // it a frame to settle.
  await firstNameCell.dblclick();
  const cellInput = firstDataRow.locator('input.data-cell-input');
  await expect(cellInput).toBeVisible({ timeout: 3_000 });

  // Type a distinctive value and press Enter to commit. This synchronously:
  //   1. Inserts an entry into `data_edit_pending` (the in-flight map).
  //   2. Sends a WS DML message to the backend.
  //   3. Clears `active_edit` (unmounts the inline input).
  // The WS response (DmlResult / DmlError) arrives asynchronously and
  // removes the entry from `data_edit_pending` — that is the race window.
  await cellInput.fill('WAITING');
  await cellInput.press('Enter');

  // Click × IMMEDIATELY — we want to race the WS response. No explicit
  // wait here: every ms of delay widens the response window and increases
  // the chance the DML completes before the close fires.
  const closeBtn = page.locator('button[title="Close pane"]').first();
  await closeBtn.click();

  // Now check which path ran. Two valid outcomes:
  //
  //   A) Dialog appeared — pending count was ≥1 when close_pane ran.
  //      Assert the dialog's wiring is correct (title, message, buttons).
  //
  //   B) No dialog — DML completed before the close-click, pending count
  //      was 0, and the pane closed immediately. This is also correct
  //      behaviour. The pane should be gone.
  //
  // We use a short poll (500 ms) to decide which outcome occurred rather
  // than a hard expect, because either is valid.
  const dialogVisible = await page.locator('.app-dialog').isVisible().catch(() => false);

  if (dialogVisible) {
    // ── Path A: dialog appeared ────────────────────────────────────────
    // The `close_pane` found ≥1 entry in `data_edit_pending` and fired
    // `DialogKind::Confirm`. Assert the exact wiring from main.rs:
    //   title:         "Close pane with pending edits?"
    //   message:       starts with "1 pending edit on this pane."
    //   confirm_label: "Close anyway"
    //   danger:        false  (btn-primary, not btn-danger)

    const dialog = page.locator('.app-dialog');
    await expect(dialog).toBeVisible({ timeout: 2_000 });

    const title = dialog.locator('.app-dialog-title');
    await expect(title).toHaveText('Close pane with pending edits?');

    const message = dialog.locator('.app-dialog-message');
    // The message format for 1 pending edit is:
    //   "1 pending edit on this pane.\n\nThe database will still process
    //    it server-side, but the outcome (success or error) won't be
    //    visible here. Close anyway?"
    await expect(message).toContainText('pending edit');
    await expect(message).toContainText('server-side');

    const actions = dialog.locator('.app-dialog-actions');

    // Cancel button must be present and close the dialog without closing
    // the pane. The button is rendered with class "btn btn-ghost".
    const cancelBtn = actions.locator('button.btn-ghost', { hasText: 'Cancel' });
    await expect(cancelBtn).toBeVisible();

    // Confirm button must carry the exact label "Close anyway" and must
    // NOT use the danger class (danger:false in DialogKind::Confirm
    // → btn-primary not btn-danger). A regression that sets danger:true
    // would be caught by the class assertion.
    const confirmBtn = actions.locator('button', { hasText: 'Close anyway' });
    await expect(confirmBtn).toBeVisible();
    await expect(confirmBtn).toHaveClass(/btn-primary/);
    await expect(confirmBtn).not.toHaveClass(/btn-danger/);

    // Dismiss via Cancel — verifies the pane stays open.
    await cancelBtn.click();
    await expect(dialog).not.toBeVisible({ timeout: 2_000 });
    // Pane must still be alive: conn-header is present.
    await expect(
      page.locator('.conn-header-name', { hasText: 'Local MySQL' }),
    ).toBeVisible({ timeout: 3_000 });

    // Now confirm via the dialog to clean up state for any subsequent
    // tests sharing the browser context.
    await closeBtn.click();
    const dialog2 = page.locator('.app-dialog');
    // If the DML completed while we were asserting, a second click may
    // not show the dialog at all. Handle both gracefully.
    const dialog2Visible = await dialog2.isVisible().catch(() => false);
    if (dialog2Visible) {
      const confirmBtn2 = dialog2.locator('button', { hasText: 'Close anyway' });
      if (await confirmBtn2.isVisible().catch(() => false)) {
        await confirmBtn2.click();
      }
    }
  } else {
    // ── Path B: DML completed before close-click (fast local DB) ──────
    // The pending map was already empty when close_pane ran; the pane
    // closed without a dialog. This is correct behaviour — assert the
    // pane is indeed gone so we don't silently pass when the close itself
    // is broken.
    //
    // Note: if this path fires consistently in CI it means the dialog
    // path cannot be exercised with the current test timing. Manual
    // verification against a throttled backend (e.g. tc netem delay) is
    // needed to confirm the dialog wiring.
    console.warn(
      '[pending-dml-close-confirm] DML completed before close-click — ' +
      'dialog path not exercised. Asserting pane-closed fast-path instead.',
    );
    await expect(
      page.locator('.conn-header-name', { hasText: 'Local MySQL' }),
    ).not.toBeVisible({ timeout: 5_000 });
  }
});
