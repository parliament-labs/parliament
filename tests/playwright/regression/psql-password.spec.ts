import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput } from '../helpers/panes';

// Regression suite for Parliament's `\password` meta-command (shipped
// in PR #54). The feature's whole point is that the plaintext password
// never crosses the WebSocket — and that the resulting SCRAM hash
// never lands in a place arrow-up / Ctrl+R can retrieve it. Both
// claims are browser-side UX behavior, only verifiable end-to-end.
//
// What these tests pin:
//   1. The textarea-overlay masks typed chars to `•` while password
//      mode is active.
//   2. The two-stage prompt (new + retype) advances correctly.
//   3. Mismatched passwords abort with a notice; no SQL fires.
//   4. Esc cancels at either stage with a notice.
//   5. On match, history shows the REDACTED meta-output line
//      (`PASSWORD '<redacted>'`), not the real hash.
//   6. Arrow-up does NOT surface the redacted line into the input —
//      it was pushed as a MetaOutput, not a Query entry.
//
// The tests deliberately target a non-existent user. The browser-side
// behavior is what matters; the resulting ALTER USER fails harmlessly
// on the server side with "role does not exist", which is itself
// useful as a "the wire format went out" assertion.

test('\\password: typed chars render as bullets, not plaintext', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\password parliament_test_nonexistent_user');
  await input.press('Enter');

  // The prompt hint must appear before we type the password.
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'enter new password',
    { timeout: 5_000 },
  );

  // Type a password. The textarea text is `color: transparent`, so the
  // ONLY visible char layer is the syntax-highlight overlay, which
  // should render `•` per char while password mode is active.
  await input.fill('hunter2plain');
  const overlay = page.locator('.shell-input-highlight:visible').first();
  // Expect at least one `•` rendered; the text content should be 12
  // bullets and contain NO ASCII letters from the typed password.
  await expect(overlay).toContainText('•', { timeout: 2_000 });
  await expect(overlay).not.toContainText('hunter');
  await expect(overlay).not.toContainText('plain');

  // Cancel — don't actually try to submit against a nonexistent user.
  await input.press('Escape');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'cancelled',
    { timeout: 3_000 },
  );
});

test('\\password: mismatch on retype aborts with notice and no SQL fires', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\password parliament_test_nonexistent_user');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'enter new password',
    { timeout: 5_000 },
  );

  // Stage 1: type the first password.
  await input.fill('first_password');
  await input.press('Enter');

  // Stage 2 hint must appear.
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Retype new password',
    { timeout: 3_000 },
  );

  // Stage 2: type a different password.
  await input.fill('different_password');
  await input.press('Enter');

  // Mismatch notice; no SQL submitted.
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    "don't match",
    { timeout: 3_000 },
  );

  // No "submitted ALTER USER" line should exist in history — that
  // would mean we sent SQL despite the mismatch.
  await expect(
    page.locator('.shell-meta-output', { hasText: 'submitted ALTER USER' }),
  ).toHaveCount(0);
});

test('\\password: successful flow shows redacted history line, not the hash', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\password parliament_test_nonexistent_user');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'enter new password',
    { timeout: 5_000 },
  );

  const pwd = 'matching_password_v1';
  await input.fill(pwd);
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Retype new password',
    { timeout: 3_000 },
  );

  await input.fill(pwd);
  await input.press('Enter');

  // The "submitted ALTER USER" meta line MUST contain `<redacted>` and
  // MUST NOT contain a literal `SCRAM-SHA-256` hash prefix anywhere.
  const submittedLine = page.locator(
    '.shell-meta-output',
    { hasText: 'submitted ALTER USER' },
  ).first();
  await expect(submittedLine).toBeVisible({ timeout: 8_000 });
  await expect(submittedLine).toContainText('<redacted>');
  await expect(submittedLine).not.toContainText('SCRAM-SHA-256');

  // The whole shell area must contain no occurrence of the SCRAM
  // hash string anywhere — neither the original prompts, the
  // redacted line, nor the error response.
  const shell = page.locator('.shell-output:visible').first();
  await expect(shell).not.toContainText('SCRAM-SHA-256');

  // The actual ALTER USER fired against PG, against a nonexistent
  // role → PG's error confirms "wire format went out" without
  // requiring a real test user. Either Error entry or the message
  // visible somewhere; we just check for "does not exist".
  await expect(shell).toContainText(/does not exist|role.*does not/i, {
    timeout: 8_000,
  });
});

test('\\password: arrow-up cannot retrieve the redacted ALTER USER line', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Submit a known query first so arrow-up has SOMETHING to surface.
  await input.fill('SELECT 1 AS sentinel;');
  await input.press('Enter');
  await expect(page.locator('.shell-table-wrap').first()).toBeVisible({
    timeout: 10_000,
  });

  // Run \password through both stages to push the submitted-line into
  // history.
  await input.fill('\\password parliament_test_nonexistent_user');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'enter new password',
    { timeout: 5_000 },
  );
  await input.fill('history_safety_pwd');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Retype new password',
    { timeout: 3_000 },
  );
  await input.fill('history_safety_pwd');
  await input.press('Enter');

  // Wait for the submission line to land before arrowing up.
  await expect(
    page.locator('.shell-meta-output', { hasText: 'submitted ALTER USER' }).first(),
  ).toBeVisible({ timeout: 8_000 });

  // Arrow-up walks command history. The first hit should be the
  // SELECT we ran, NOT the ALTER USER (which lives in MetaOutput,
  // outside `query_strings()`).
  await input.click();
  await input.press('ArrowUp');

  // Whatever surfaced into the input must be the SELECT — NOT
  // anything that contains ALTER USER or <redacted> or SCRAM.
  await expect(input).toHaveValue(/SELECT 1 AS sentinel/i, { timeout: 2_000 });
  await expect(input).not.toHaveValue(/ALTER USER/i);
  await expect(input).not.toHaveValue(/<redacted>/i);
  await expect(input).not.toHaveValue(/SCRAM-SHA-256/i);
});

test('\\password: Esc at stage 1 cancels without firing SQL', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\password parliament_test_nonexistent_user');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'enter new password',
    { timeout: 5_000 },
  );

  await input.fill('some_password');
  await input.press('Escape');

  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'cancelled',
    { timeout: 3_000 },
  );

  await expect(
    page.locator('.shell-meta-output', { hasText: 'submitted ALTER USER' }),
  ).toHaveCount(0);
});

test('\\password: Esc at stage 2 (retype) cancels without firing SQL', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\password parliament_test_nonexistent_user');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'enter new password',
    { timeout: 5_000 },
  );

  await input.fill('first_stage_pwd');
  await input.press('Enter');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'Retype new password',
    { timeout: 3_000 },
  );

  // Escape during stage 2 must also cancel cleanly.
  await input.press('Escape');
  await expect(page.locator('.shell-meta-output').last()).toContainText(
    'cancelled',
    { timeout: 3_000 },
  );

  await expect(
    page.locator('.shell-meta-output', { hasText: 'submitted ALTER USER' }),
  ).toHaveCount(0);
});
