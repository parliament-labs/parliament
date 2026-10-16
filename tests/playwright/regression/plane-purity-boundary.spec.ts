import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// Regression suite for plane purity (PR #62). Each plane handles only
// its own native dialect's verbs — no cross-CLI shortcuts.
//
// Each plane's lexer (`DbmsPlane::lexicon()`) makes regression at the
// trait surface hard: only mysql's reads `\G` as a terminator. But the
// historical pattern was for psql-feature
// PRs to drift over time and add MySQL refuse-arms with PG-vocab
// pointers (#54 originally landed one for `\password`; #55 for `\lo_*`).
// These tests pin the boundary so future PRs that re-introduce that
// pattern fail loudly here.
//
// What these tests pin:
//   1. On MySQL: typing `\password` produces no PG-vocab output
//      (no "SCRAM-SHA-256", no "ALTER USER … IDENTIFIED BY" pointer).
//   2. On MySQL: typing `\lo_list` produces no PG-vocab output (no
//      mention of pg_largeobject_metadata, no BLOB / LONGBLOB pointer).
//   3. On MySQL: typing `SELECT 1\G` still fires vertical output —
//      mysql-native behavior is preserved.
//   4. On Postgres: typing `\?` does NOT show `\G` in the help table
//      (PR #62 removed the stale entry).
//   5. On Postgres: typing `SELECT 1\G` does NOT trigger vertical
//      output — `\G` is mysql-native and shouldn't be recognized
//      by the PG shell-layer terminator strip.
//   6. On Postgres: typing `SELECT 1\gx` (psql-native) DOES trigger
//      vertical output — sanity that the PG vertical path still works.

// ── MySQL: psql commands fall through with no PG-vocab pointer ──────

test('on MySQL, \\password does not produce PG-vocabulary output', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\password');
  await input.press('Enter');

  // Wait a beat for any meta dispatch to fire.
  await page.waitForTimeout(800);

  // The shell output must not contain PG-vocab strings that would
  // surface if a refuse-arm regressed.
  const out = visibleShellOutput(page);
  await expect(out).not.toContainText(/SCRAM/i);
  await expect(out).not.toContainText(/IDENTIFIED BY/i);
  await expect(out).not.toContainText(/psql-specific/i);
});

test('on MySQL, \\lo_list does not produce PG-vocabulary output', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\lo_list');
  await input.press('Enter');

  await page.waitForTimeout(800);

  const out = visibleShellOutput(page);
  // No PG-large-object vocabulary, no refuse-arm pointer at native
  // MySQL BLOB / LONGBLOB columns.
  await expect(out).not.toContainText(/pg_largeobject/i);
  await expect(out).not.toContainText(/PG-specific/i);
  await expect(out).not.toContainText(/BLOB \/ LONGBLOB/i);
});

// ── MySQL: native \G still works ────────────────────────────────────

test('on MySQL, SELECT 1\\G fires vertical output (mysql-native preserved)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('SELECT 1 AS sentinel\\G');
  await input.press('Enter');

  // Vertical output renders inside `.shell-vertical-block`. The
  // sentinel value must appear somewhere in the vertical view.
  const out = visibleShellOutput(page);
  const vert = out.locator('.shell-vertical-block').last();
  await expect(vert).toBeVisible({ timeout: 10_000 });
  await expect(vert).toContainText('sentinel');
});

// ── Postgres: \G is not advertised + does not fire vertical ─────────

test('on Postgres, \\? help table does NOT advertise \\G', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  await input.fill('\\?');
  await input.press('Enter');

  // The meta-output area renders the help table. It MUST contain `\g`
  // and `\gx` (psql-native), and MUST NOT contain a standalone `\G`
  // entry. We assert on a fairly tight pattern to avoid matching
  // legitimate prose mentions.
  const out = visibleShellOutput(page);
  const helpBlock = out.locator('.shell-meta-output').last();
  await expect(helpBlock).toBeVisible({ timeout: 8_000 });
  await expect(helpBlock).toContainText('\\g');
  await expect(helpBlock).toContainText('\\gx');
  // The previous entry was the literal line `\G   Execute query,
  // display result vertically.` — assert that the "vertically"
  // description does not appear next to a `\G` token.
  await expect(helpBlock).not.toContainText(/\\G\s+Execute query, display result vertically/);
});

test('on Postgres, SELECT 1\\G does NOT fire vertical output (\\G is mysql-only)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // Sanity baseline: count any existing `.shell-vertical-block`s
  // before our submission so we can assert "no NEW vertical block
  // appeared."
  const out = visibleShellOutput(page);
  const vertBefore = await out.locator('.shell-vertical-block').count();

  await input.fill('SELECT 1\\G');
  await input.press('Enter');

  // psql doesn't have `\G`: `invalid command \G`, and the buffer before
  // it waits for a proper terminator (`;` or `\gx`). No vertical block.
  await expect(out.locator('.shell-entry-error').last()).toHaveText('invalid command \\G\nTry \\? for help.', { timeout: 10_000 });
  await expect(input).toHaveValue('SELECT 1\n');

  const vertAfter = await out.locator('.shell-vertical-block').count();
  expect(vertAfter).toBe(vertBefore);
});

test('on Postgres, SELECT 1\\gx fires vertical output (psql-native still works)', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);

  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });

  // `\gx` is a meta-dispatched tail-line: PG plane returns
  // MetaAction::Go { expanded: true }. The shell submits and
  // forces vertical for that one run.
  await input.fill('SELECT 1 AS sentinel\n\\gx');
  await input.press('Enter');

  const out = visibleShellOutput(page);
  const vert = out.locator('.shell-vertical-block').last();
  await expect(vert).toBeVisible({ timeout: 10_000 });
  await expect(vert).toContainText('sentinel');
});
