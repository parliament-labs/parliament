import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane } from '../helpers/panes';

// Regression suite for Parliament issue #9 — AST-based safe-mode
// classification replacing three brittle prefix-match inspections.
//
// Failure class (pre-fix): safe-mode used `trim().to_uppercase().starts_with("DROP")`
// on the raw input string. This was bypassed by:
//   1. A leading block comment  — `/* migration */ DROP TABLE x` starts with
//      `/`, not `DROP`, so no confirm dialog fired.
//   2. A multi-statement batch — `SELECT 1; DROP TABLE x;` sent the batch
//      through the multi-statement path which never consulted safe-mode at all.
//   3. `DELETE FROM x` without WHERE — `DELETE` is also the prefix of the
//      safe form `DELETE FROM x WHERE …`, so the prefix match had to choose:
//      gate all DELETEs (annoying) or gate none (wrong). It chose none.
//   4. TXN badge / comment prefix — `/* tx */ BEGIN;` started with `/`, so
//      the prefix-match TXN detector never lit the badge.
//
// The fix (sql_inspect.rs) parses with sqlparser-rs and pattern-matches the
// AST. All four bypass shapes are covered below.
//
// Reference: vault/Engineering/Testing Strategy.md § Layer 3 (UI-only failure
// classes), Audit Findings 2026-06.md findings 1.1, 1.2, 1.3, 1.6.

test('safe-mode: comment-prefixed DROP triggers confirm dialog', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  // Open a shell pane explicitly — the connection's default_pane may be
  // "schema" in a live noctua session vs the fixture. addShellPane covers
  // both cases; when a shell is already the active pane it simply adds a
  // second shell tab, which is harmless.
  await addShellPane(page);

  // `.shell-input-inline` is only rendered once the backend assigns a session
  // ID over WebSocket. Give the WS handshake time to complete.
  const input = page.locator('.shell-input-inline').last();
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Enable safe-mode for this session.
  await input.fill('\\safe-mode on');
  await input.press('Enter');
  // Wait for the MetaOutput acknowledgement before proceeding.
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Safe mode ON',
    { timeout: 5_000 },
  );

  // Pre-fix: prefix-match saw `/` and let this through without a dialog.
  // Trailing semicolon required — the shell only submits on delimiter + Enter.
  await input.fill('/* migration */ DROP TABLE pw_test;');
  await input.press('Enter');

  // The confirm block must appear with the DROP header text.
  const confirmBlock = page.locator('.shell-confirm-block');
  await expect(confirmBlock).toBeVisible({ timeout: 5_000 });
  await expect(confirmBlock.locator('.shell-confirm-warn')).toContainText('DROP');

  // Cancel — do not actually execute the DROP.
  await confirmBlock.locator('.shell-confirm-cancel').click();
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Cancelled.',
    { timeout: 3_000 },
  );
});

test('safe-mode: DELETE without WHERE triggers confirm dialog', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = page.locator('.shell-input-inline').last();
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\safe-mode on');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Safe mode ON',
    { timeout: 5_000 },
  );

  // Pre-fix: `DELETE` matches both safe and unsafe forms; prefix-match
  // couldn't distinguish — it gated nothing. AST sees no WHERE selection.
  // Trailing semicolon required — the shell only submits on delimiter + Enter.
  await input.fill('DELETE FROM film;');
  await input.press('Enter');

  const confirmBlock = page.locator('.shell-confirm-block');
  await expect(confirmBlock).toBeVisible({ timeout: 5_000 });
  await expect(confirmBlock.locator('.shell-confirm-warn')).toContainText(
    'DELETE without WHERE',
  );

  await confirmBlock.locator('.shell-confirm-cancel').click();
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Cancelled.',
    { timeout: 3_000 },
  );
});

test('safe-mode: DELETE with WHERE is NOT gated', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = page.locator('.shell-input-inline').last();
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\safe-mode on');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Safe mode ON',
    { timeout: 5_000 },
  );

  // Negative id — row cannot exist; nothing deleted, no actual data damage.
  await input.fill('DELETE FROM film WHERE film_id = -1;');
  await input.press('Enter');

  // The confirm dialog must NOT appear for a WHERE-scoped DELETE.
  // Short timeout — if the dialog were going to appear it would be
  // immediate (it's synchronous client-side logic).
  await expect(page.locator('.shell-confirm-block')).not.toBeVisible({
    timeout: 500,
  });

  // The query should run and print psql's command tag.
  await expect(page.locator('.shell-entry-info').last()).toHaveText(
    /^DELETE 0\b/,
    { timeout: 8_000 },
  );
});

test('safe-mode: multi-statement batch with DROP triggers batch confirm', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = page.locator('.shell-input-inline').last();
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\safe-mode on');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Safe mode ON',
    { timeout: 5_000 },
  );

  // Pre-fix: multi-statement path bypassed safe-mode entirely.
  // The DROP shipped without a confirm dialog.
  await input.fill('SELECT 1; DROP TABLE pw_test_nonexistent;');
  await input.press('Enter');

  const confirmBlock = page.locator('.shell-confirm-block');
  await expect(confirmBlock).toBeVisible({ timeout: 5_000 });

  // Header must describe the batch-level classification: N destructive
  // statements in an M-statement batch.
  await expect(confirmBlock.locator('.shell-confirm-warn')).toContainText(
    /destructive.*batch/i,
  );

  // The batch-confirm button reads "Run batch (N statements)", not "Execute".
  // Its presence proves we are in Batch confirm mode, not Single.
  await expect(
    confirmBlock.locator('.shell-confirm-execute'),
  ).toContainText(/Run batch/i);

  await confirmBlock.locator('.shell-confirm-cancel').click();
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Cancelled.',
    { timeout: 3_000 },
  );
});

test('safe-mode: a statement run by \\g is gated like one ended by ;', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = page.locator('.shell-input-inline').last();
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\safe-mode on');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText('Safe mode ON', { timeout: 5_000 });

  // Pre-fix: the \g family sent the buffer straight to the server.
  await input.fill('DROP TABLE pw_test_nonexistent\n\\g');
  await input.press('Enter');

  const confirmBlock = page.locator('.shell-confirm-block');
  await expect(confirmBlock).toBeVisible({ timeout: 5_000 });
  await expect(confirmBlock.locator('.shell-confirm-warn')).toContainText('DROP');
  await expect(confirmBlock.locator('.shell-confirm-sql')).toContainText('DROP TABLE pw_test_nonexistent\n\\g');
  await confirmBlock.locator('.shell-confirm-cancel').click();
  await expect(page.locator('.shell-meta-output').last()).toContainText('Cancelled.', { timeout: 3_000 });
});

test('safe-mode: a statement \\watch would repeat is gated before the watch starts', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = page.locator('.shell-input-inline').last();
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\safe-mode on');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText('Safe mode ON', { timeout: 5_000 });

  await input.fill('delete from pw_test_nonexistent \\watch 1');
  await input.press('Enter');

  const confirmBlock = page.locator('.shell-confirm-block');
  await expect(confirmBlock).toBeVisible({ timeout: 5_000 });
  await expect(confirmBlock.locator('.shell-confirm-warn')).toContainText('DELETE without WHERE');
  await confirmBlock.locator('.shell-confirm-cancel').click();
  await expect(page.locator('.shell-meta-output').last()).toContainText('Cancelled.', { timeout: 3_000 });
  await expect(page.locator('.shell-watch-bar')).toHaveCount(0);
  await expect(page.locator('.shell-table-title', { hasText: '(every' })).toHaveCount(0);
});

test('TXN badge lights up through a leading block comment', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = page.locator('.shell-input-inline').last();
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Pre-fix: prefix-match trimmed whitespace but not block comments; the
  // string started with `/`, not `BEGIN`, so the TXN badge stayed dark.
  await input.fill('/* tx */ BEGIN;');
  await input.press('Enter');

  // The TXN badge must appear in the pane toolbar.
  await expect(page.locator('.txn-badge.txn-active')).toBeVisible({
    timeout: 8_000,
  });

  // Clean up: close the transaction so the next test run starts clean.
  await input.fill('ROLLBACK;');
  await input.press('Enter');
  await expect(page.locator('.txn-badge.txn-active')).not.toBeVisible({
    timeout: 5_000,
  });
});
