import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { visibleShellInput } from '../helpers/panes';

// Regression suite for PR #37 — per-connection shell buffer eviction threshold.
//
// Failure class: the `buffer_threshold_mb: Option<u32>` field on `ConnectionDef`
// controls automatic eviction of the oldest shell block after each query.
// The whole feature is invisible to `cargo check`, `clippy`, and unit tests:
//  - The form field's presence, label, placeholder, and hint text are pure DOM.
//  - The serde `#[serde(default)]` legacy-compat contract shows only at runtime.
//  - The eviction guard ("never evict the only block") requires a browser session
//    that has actually accumulated history exceeding the threshold.
//  - The "one eviction per query" contract (not a bulk drain) requires measuring
//    block counts before/after successive queries.
//
// Reference: vault/Engineering/Testing Strategy.md § Layer 3.
//
// NOTE on timing: the big-result queries (generate_series × 5000) produce
// several hundred KB over WebSocket. A 30 s timeout is applied at
// `.toBeVisible` call sites that wait for these to complete.
//
// NOTE on round-trip (test 3): after clicking "Save Connection" the sidebar
// context-menu `editing_conn_id` is reset and `pane.set(Pane::Shell)` fires,
// so `.conn-form-title` disappears. We wait for that disappearance as the
// "save completed" signal before re-opening the Edit form.
//
// NOTE on shared state / cleanup: tests 4 and 5 write a non-None threshold to
// ~/.sqlclient/connections.json via SaveConnection. They clean up at the end.
// `test.afterEach` also resets the threshold as a belt-and-suspenders guard
// so a mid-test failure doesn't bias subsequent test runs. The global
// teardown restores the original connections.json snapshot, but the reset
// inside the suite is faster and more granular.

// ── Helper: open the Edit Connection form for "Local Postgres" via right-click ──

async function openEditForm(page: Parameters<typeof openApp>[0]) {
  const connItem = page
    .locator('.conn-item', {
      has: page.locator('.conn-name', { hasText: 'Local Postgres' }),
    })
    .first();
  await expect(connItem).toBeVisible({ timeout: 10_000 });
  await connItem.click({ button: 'right' });

  const editItem = page.locator('.tab-context-menu-item', { hasText: 'Edit…' });
  await expect(editItem).toBeVisible({ timeout: 5_000 });
  await editItem.click();

  // Wait for the Edit Connection form pane to appear.
  const formTitle = page.locator('.conn-form-title', { hasText: 'Edit Connection' });
  await expect(formTitle).toBeVisible({ timeout: 5_000 });
}

// ── Helper: save the connection form and wait for it to close ─────────────────

async function saveAndClose(page: Parameters<typeof openApp>[0]) {
  await page.locator('button.btn.btn-primary', { hasText: 'Save Connection' }).click();
  // The form closes when `pane.set(Pane::Shell)` fires and `.conn-form-title`
  // disappears. Use the absence of the form-title as the "save completed" gate.
  await expect(page.locator('.conn-form-title')).not.toBeVisible({ timeout: 10_000 });
}

// ── Helper: set the buffer threshold field to a value (or clear it) ───────────

async function setBufferThreshold(
  page: Parameters<typeof openApp>[0],
  value: string,
) {
  const thresholdInput = page
    .locator('label.form-label', { hasText: 'Max buffer threshold (MB)' })
    .locator('xpath=following-sibling::input[1]');
  await expect(thresholdInput).toBeVisible({ timeout: 5_000 });
  await thresholdInput.fill(value);
}

// ── Helper: parse the buffer pill text to bytes for comparison ────────────────
// Format is "buf:N B", "buf:N.N KB", "buf:N.N MB". Returns bytes as a float.
function parsePillBytes(text: string): number {
  const m = text.match(/buf:([\d.]+)\s*(B|KB|MB)/i);
  if (!m) return 0;
  const n = parseFloat(m[1]);
  const unit = m[2].toUpperCase();
  if (unit === 'KB') return n * 1024;
  if (unit === 'MB') return n * 1024 * 1024;
  return n;
}

// ── afterEach: belt-and-suspenders reset of the buffer threshold ──────────────
// If a test that sets the threshold fails before its cleanup block, the next
// run would find a stale non-None value in the saved connection. This hook
// resets it unconditionally after every test so the suite is idempotent.
// It runs in the same page that the test used, so `openApp` is not needed
// (the app is already loaded). If the form is already open from a failed test
// we re-use it; otherwise open it fresh.
test.afterEach(async ({ page }) => {
  // Check if the form is already open (a failed test may have left it open).
  const formVisible = await page
    .locator('.conn-form-title', { hasText: 'Edit Connection' })
    .isVisible();
  if (!formVisible) {
    // The app may be on the connection page but not yet logged in — guard with
    // a short timeout so the hook doesn't hang if the page is in a bad state.
    const connItem = page
      .locator('.conn-item', {
        has: page.locator('.conn-name', { hasText: 'Local Postgres' }),
      })
      .first();
    const itemVisible = await connItem.isVisible({ timeout: 3_000 }).catch(() => false);
    if (!itemVisible) return; // app not in a usable state; skip cleanup
    await connItem.click({ button: 'right' });
    const editItem = page.locator('.tab-context-menu-item', { hasText: 'Edit…' });
    const editVisible = await editItem.isVisible({ timeout: 3_000 }).catch(() => false);
    if (!editVisible) return;
    await editItem.click();
    await page
      .locator('.conn-form-title', { hasText: 'Edit Connection' })
      .waitFor({ state: 'visible', timeout: 5_000 })
      .catch(() => null);
  }

  // Clear the threshold field if it has a value.
  const thresholdInput = page
    .locator('label.form-label', { hasText: 'Max buffer threshold (MB)' })
    .locator('xpath=following-sibling::input[1]');
  const fieldVisible = await thresholdInput.isVisible({ timeout: 2_000 }).catch(() => false);
  if (!fieldVisible) return;
  const current = await thresholdInput.inputValue();
  if (current !== '') {
    await thresholdInput.fill('');
    await page.locator('button.btn.btn-primary', { hasText: 'Save Connection' }).click();
    await page.locator('.conn-form-title').waitFor({ state: 'hidden', timeout: 10_000 }).catch(() => null);
  }
});

// ─────────────────────────────────────────────────────────────────────────────
// Test 1 — Form field label, input type, placeholder, and hint text
// ─────────────────────────────────────────────────────────────────────────────

test('buffer threshold form field has correct label, type, placeholder, and hint', async ({
  page,
}) => {
  await openApp(page);
  await openEditForm(page);

  // Label must exist with exact text.
  const label = page.locator('label.form-label', { hasText: 'Max buffer threshold (MB)' });
  await expect(label).toBeVisible({ timeout: 5_000 });

  // The associated input is a number input immediately following the label.
  const input = label.locator('xpath=following-sibling::input[1]');
  await expect(input).toBeVisible();
  await expect(input).toHaveAttribute('type', 'number');

  // Placeholder matches the spec exactly.
  await expect(input).toHaveAttribute('placeholder', '(leave blank — no cleanup)');

  // The hint div immediately follows the input wrapper and contains the
  // "Not a pre-allocated buffer size" prefix (the full text is long — pin
  // the distinctive leading substring).
  const hint = page
    .locator('.form-hint', { hasText: 'Not a pre-allocated buffer size' })
    .first();
  await expect(hint).toBeVisible({ timeout: 3_000 });
});

// ─────────────────────────────────────────────────────────────────────────────
// Test 2 — Existing connection shows empty (legacy None deserialization)
// ─────────────────────────────────────────────────────────────────────────────

test('buffer threshold input is empty for an existing connection (serde default)', async ({
  page,
}) => {
  await openApp(page);

  // Ensure the threshold is clear before asserting — the afterEach of a
  // preceding failed test should have done this, but open and clear
  // defensively at the start here too. This is belt-and-suspenders for
  // the case where a prior test run left a stale value persisted to disk.
  await openEditForm(page);
  const thresholdInput = page
    .locator('label.form-label', { hasText: 'Max buffer threshold (MB)' })
    .locator('xpath=following-sibling::input[1]');
  await expect(thresholdInput).toBeVisible({ timeout: 5_000 });
  const priorValue = await thresholdInput.inputValue();
  if (priorValue !== '') {
    // A stale value from a prior run is itself a bug signal — log it, then
    // clear so the rest of the test runs deterministically.
    console.warn(`[connection-buffer-threshold] test 2 found stale threshold "${priorValue}" — clearing before assertion`);
    await thresholdInput.fill('');
    await saveAndClose(page);
    // Re-open to assert the cleared state.
    await openEditForm(page);
  }

  // The field must now be empty — `None` deserializes to "" via
  // `#[serde(default)]` and `unwrap_or_default()` in the Edit click handler.
  await expect(thresholdInput).toHaveValue('');
});

// ─────────────────────────────────────────────────────────────────────────────
// Test 3 — Value round-trips through Save → reload Edit form
// ─────────────────────────────────────────────────────────────────────────────

test('buffer threshold value round-trips through save and re-open', async ({ page }) => {
  await openApp(page);
  await openEditForm(page);
  await setBufferThreshold(page, '5');
  await saveAndClose(page);

  // Re-open the Edit form for the same connection.
  await openEditForm(page);

  const input = page
    .locator('label.form-label', { hasText: 'Max buffer threshold (MB)' })
    .locator('xpath=following-sibling::input[1]');
  await expect(input).toBeVisible({ timeout: 5_000 });

  // The saved value must pre-populate.
  await expect(input).toHaveValue('5');

  // Cleanup: clear the threshold. The afterEach also does this, but doing it
  // explicitly here makes the in-test cleanup intent clear.
  await input.fill('');
  await saveAndClose(page);
});

// ─────────────────────────────────────────────────────────────────────────────
// Test 4 — Threshold triggers eviction on the next query
// ─────────────────────────────────────────────────────────────────────────────

test('eviction fires once threshold is exceeded, dropping the oldest block', async ({
  page,
}) => {
  await openApp(page);

  // Set the threshold to 1 MB so two ~500 KB queries will push the buffer
  // over the limit, and a third (tiny) query triggers eviction.
  await openEditForm(page);
  await setBufferThreshold(page, '1');
  await saveAndClose(page);

  // Open the connection (default pane is shell per the pw-pagila fixture).
  const connItem = page
    .locator('.conn-item', {
      has: page.locator('.conn-name', { hasText: 'Local Postgres' }),
    })
    .first();
  await connItem.click();
  const connHeader = page.locator('.conn-header-name', { hasText: 'Local Postgres' }).first();
  await expect(connHeader).toBeVisible({ timeout: 15_000 });

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Big query — ~600 KB per run (6000 rows × 100-byte values).
  // One run is under the 1 MB threshold; two runs together (~1.2 MB) exceed it.
  // The eviction handler fires after the SECOND query pushes the buffer over
  // the threshold. After eviction the oldest block is dropped, leaving only
  // the Q2 block — 1 block at ~600 KB, not 2 blocks at ~1.2 MB.
  const bigQuery = `SELECT i, repeat('x', 100) FROM generate_series(1, 6000) AS i;`;

  // ── Query 1 ──────────────────────────────────────────────────────────────
  // Wait for the result TABLE, not just the block wrapper. `.shell-table-wrap`
  // renders only once the `Table` ShellEntry has been pushed to shell_history
  // — at that point the pill reflects the full result payload.
  await input.fill(bigQuery);
  await input.press('Enter');
  await expect(page.locator('.shell-table-wrap').first()).toBeVisible({ timeout: 30_000 });
  await expect(page.locator('.shell-block')).toHaveCount(1, { timeout: 5_000 });

  const pillAfterQ1 = await page.locator('.shell-buffer-pill').textContent({ timeout: 5_000 });
  const bytesAfterQ1 = parsePillBytes(pillAfterQ1 ?? '');
  // Sanity: after 6000 × 100-byte rows the pill must be > 100 KB.
  expect(bytesAfterQ1).toBeGreaterThan(100_000);
  // Q1 alone must be UNDER the 1 MB threshold — no eviction yet.
  expect(bytesAfterQ1).toBeLessThan(1024 * 1024);

  // ── Query 2 — eviction fires after this one ───────────────────────────────
  // Accumulated buffer after Q2: ~1.2 MB > 1 MB threshold.
  // Eviction runs immediately: Q1 block (oldest) is dropped.
  // Settled state: 1 block (Q2 only), pill ≈ bytesAfterQ1 (~600 KB).
  await input.fill(bigQuery);
  await input.press('Enter');
  // The transient 2-block state (both queries present before eviction) may be
  // too brief to observe reliably. Wait for the settled 1-block state with
  // the result table visible — that combination proves eviction ran and the
  // new result is present.
  await expect(page.locator('.shell-block')).toHaveCount(1, { timeout: 30_000 });
  await expect(page.locator('.shell-table-wrap').first()).toBeVisible({ timeout: 5_000 });

  const pillAfterQ2 = await page.locator('.shell-buffer-pill').textContent({ timeout: 5_000 });
  const bytesAfterQ2 = parsePillBytes(pillAfterQ2 ?? '');

  // CENTRAL REGRESSION CHECK — two separate proofs that eviction ran:
  //
  // 1. Block count == 1 after 2 queries. Pre-fix (no eviction): count == 2.
  //    This check is already enforced by the `toHaveCount(1)` assertion above.
  //
  // 2. The pill after Q2 is NOT nearly double Q1's size. After eviction,
  //    bytesAfterQ2 ≈ bytesAfterQ1 (one big block swapped for another of the
  //    same size). Pre-fix: bytesAfterQ2 ≈ 2 × bytesAfterQ1.
  //    Use < 1.5 × bytesAfterQ1 as the threshold to allow measurement noise
  //    while still catching the 2× growth that a broken (non-evicting) build
  //    would show.
  expect(bytesAfterQ2).toBeLessThan(bytesAfterQ1 * 1.5);

  // Cleanup happens in afterEach.
});

// ─────────────────────────────────────────────────────────────────────────────
// Test 5 — Single huge block is preserved (the "never evict the only block" guard)
// ─────────────────────────────────────────────────────────────────────────────

test('guard prevents evicting the only block even when it exceeds the threshold', async ({
  page,
}) => {
  await openApp(page);

  // Set threshold to 1 MB — the first query will produce ~5 MB, well over it.
  await openEditForm(page);
  await setBufferThreshold(page, '1');
  await saveAndClose(page);

  const connItem = page
    .locator('.conn-item', {
      has: page.locator('.conn-name', { hasText: 'Local Postgres' }),
    })
    .first();
  await connItem.click();
  const connHeader = page.locator('.conn-header-name', { hasText: 'Local Postgres' }).first();
  await expect(connHeader).toBeVisible({ timeout: 15_000 });

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // ~5 MB result (5000 rows × 1000-byte cell value + overhead).
  const hugeQuery = `SELECT i, repeat('x', 1000) FROM generate_series(1, 5000) AS i;`;

  await input.fill(hugeQuery);
  await input.press('Enter');
  // Wait for the result TABLE, not just the block wrapper. `.shell-table-wrap`
  // renders only once the full Table entry has been pushed to shell_history —
  // that is the point at which the pill reflects the full 5 MB payload and
  // the eviction check has already run.
  await expect(page.locator('.shell-table-wrap').first()).toBeVisible({ timeout: 60_000 });

  // ── Guard assertion ───────────────────────────────────────────────────────
  // Exactly 1 block must exist — the "never evict the only block" guard fired.
  await expect(page.locator('.shell-block')).toHaveCount(1, { timeout: 5_000 });

  // The pill must be > 1 MB. If the guard had NOT fired and eviction had
  // proceeded, the pill would read ~0 B (the block contained all history).
  const pillAfterHuge = await page.locator('.shell-buffer-pill').textContent({ timeout: 5_000 });
  const bytesAfterHuge = parsePillBytes(pillAfterHuge ?? '');
  expect(bytesAfterHuge).toBeGreaterThan(1024 * 1024); // > 1 MB

  // ── Follow-up small query — eviction now fires ────────────────────────────
  // With 2 blocks the guard no longer applies. The tiny query adds a block
  // (transient 2), then eviction drops the 5 MB block, leaving only the
  // small one (settled 1).
  await input.fill('SELECT 1 AS guard_check;');
  await input.press('Enter');
  // Wait for the settled 1-block state (huge block evicted, small one kept).
  await expect(page.locator('.shell-block')).toHaveCount(1, { timeout: 30_000 });
  // Confirm it is the small result that remains, not the huge one.
  await expect(page.locator('.shell-table-wrap').first()).toBeVisible({ timeout: 5_000 });

  const pillAfterSmall = await page.locator('.shell-buffer-pill').textContent({ timeout: 5_000 });
  const bytesAfterSmall = parsePillBytes(pillAfterSmall ?? '');

  // Buffer must be tiny — only the `SELECT 1` result remains.
  // Under 1 KB is a tight but fair bound: the 5 MB block is gone.
  expect(bytesAfterSmall).toBeLessThan(1024); // < 1 KB

  // Cleanup happens in afterEach.
});
