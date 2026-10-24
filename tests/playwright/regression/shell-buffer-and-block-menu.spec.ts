import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { ensureShellPane, visibleShellInput } from '../helpers/panes';

// Regression suite covering PRs #31, #34, #35 — three shell improvements
// that are invisible to cargo check / clippy / unit tests.
//
// PR #31 — Shell buffer pill and "Clear shell buffer" context menu item.
//   Failure class: no indicator existed for accumulated result memory; no
//   menu action existed to free it without reloading the page.
//   Secondary failure class: the right-click handler was attached only to
//   the textarea, so right-clicking the result history produced the
//   browser native menu instead of Parliament's shell menu.
//
// PR #34 — Block-scoped right-click context menu (Copy command / Copy
//   output / Copy whole block).
//   Failure class: no clipboard access for a single block's SQL or result;
//   right-clicking a query block showed only the textarea-level menu,
//   leaking layout context (the block vs. the gutter) from the DOM.
//
// PR #35 — Collapse / Expand output per block.
//   Failure class: long tabular results pushed subsequent queries off
//   screen. The CSS trick (max-height + overflow on a wrapper) is
//   undetectable by any static analysis; only a browser test can verify
//   the class and computed styles actually apply.
//
// Reference: vault/Engineering/Testing Strategy.md § Layer 3.

// Clipboard read/write permission required for Copy-* tests.
test.use({ permissions: ['clipboard-read', 'clipboard-write'] });

// ── Shared setup helper ───────────────────────────────────────────────────────
// Opens the app, connects to Local Postgres, has a shell on it (whatever
// the connection opens with), and waits for the shell input. Returns the
// visible shell input locator.
async function setupShell(page: Parameters<typeof openApp>[0]) {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await ensureShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  return input;
}

// ── PR #31: Buffer pill ───────────────────────────────────────────────────────

test('buf:0 B visible on fresh shell', async ({ page }) => {
  await setupShell(page);
  // The pill is always rendered (unlike the session-id pill which hides
  // when the session is stale). On a fresh connection with no history it
  // must read exactly "buf:0 B" (case-insensitive).
  const pill = page.locator('.shell-buffer-pill');
  await expect(pill).toBeVisible({ timeout: 10_000 });
  await expect(pill).toHaveText(/^buf:0 b$/i);
});

test('pill grows after a query', async ({ page }) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1;');
  await input.press('Enter');
  // Wait for a result block to confirm the query completed.
  await expect(page.locator('.shell-block').first()).toBeVisible({ timeout: 10_000 });
  // The pill must no longer read "buf:0 B" — any non-zero size value is
  // acceptable since the query's SQL string accounts for at least a few bytes.
  const pill = page.locator('.shell-buffer-pill');
  await expect(pill).not.toHaveText(/^buf:0 b$/i, { timeout: 5_000 });
});

test('right-click shell output area opens context menu', async ({ page }) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1;');
  await input.press('Enter');
  await expect(page.locator('.shell-block').first()).toBeVisible({ timeout: 10_000 });

  // Pre-fix: the contextmenu handler was attached only to the textarea.
  // Right-clicking the history area (the output div, not the textarea)
  // fell through to the browser's native context menu — Parliament's
  // `.tab-context-menu` never mounted.
  const outputArea = page.locator('.shell-output').first();
  await outputArea.click({ button: 'right', position: { x: 10, y: 10 } });
  await expect(page.locator('.tab-context-menu')).toBeVisible({ timeout: 5_000 });
});

test('"Clear shell buffer" item visible and clears the buffer', async ({ page }) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1;');
  await input.press('Enter');
  await expect(page.locator('.shell-block').first()).toBeVisible({ timeout: 10_000 });

  // Open the shell context menu from the output area.
  const outputArea = page.locator('.shell-output').first();
  await outputArea.click({ button: 'right', position: { x: 10, y: 10 } });
  await expect(page.locator('.tab-context-menu')).toBeVisible({ timeout: 5_000 });

  // The item text is "Clear shell buffer (N B)" when the buffer is non-empty.
  const clearItem = page.locator('.tab-context-menu-item', {
    hasText: /^clear shell buffer/i,
  });
  await expect(clearItem).toBeVisible({ timeout: 3_000 });
  await clearItem.click();

  // After clearing, no blocks remain and the pill returns to "buf:0 B".
  await expect(page.locator('.shell-block')).toHaveCount(0, { timeout: 5_000 });
  await expect(page.locator('.shell-buffer-pill')).toHaveText(/^buf:0 b$/i, {
    timeout: 3_000,
  });
});

test('TXN badge, session-id pill, and buffer pill share one row', async ({ page }) => {
  await setupShell(page);

  // The strip exists regardless of TXN state.
  const strip = page.locator('.shell-status-strip').first();
  await expect(strip).toBeVisible({ timeout: 10_000 });

  // Both pills must be direct children of the strip (or descendants
  // within it — locator scoping is sufficient here). The real check is
  // that their bounding-box tops are within a few pixels of each other,
  // proving they are horizontally co-linear rather than stacked.
  const sessionPill = strip.locator('.session-id-pill');
  const bufPill = strip.locator('.shell-buffer-pill');

  // session-id pill only renders after WS SessionOpened; wait for it.
  await expect(sessionPill).toBeVisible({ timeout: 15_000 });
  await expect(bufPill).toBeVisible();

  const sesBox = await sessionPill.boundingBox();
  const bufBox = await bufPill.boundingBox();
  expect(sesBox).not.toBeNull();
  expect(bufBox).not.toBeNull();

  // Allow up to 4px vertical drift (sub-pixel rendering, flex alignment).
  const verticalDrift = Math.abs(sesBox!.y - bufBox!.y);
  expect(verticalDrift).toBeLessThanOrEqual(4);
});

// ── PR #34: Block context menu ────────────────────────────────────────────────

test('shell-block :hover rule declares a non-transparent border-color', async ({ page }) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1;');
  await input.press('Enter');
  await expect(page.locator('.shell-block').first()).toBeVisible({ timeout: 10_000 });

  // getComputedStyle cannot observe :hover pseudo-class state from JS —
  // it always returns the non-hovered computed values. The correct approach
  // for a `:hover`-only CSS rule is to walk the document's CSSStyleSheets
  // and find the rule directly.
  //
  // Pre-fix: no `.shell-block:hover` rule existed; the border stayed
  // `1px solid transparent` on mouse-over. Post-fix the rule sets
  // `border-color: rgba(88, 166, 255, 0.45)` (the --accent colour).
  const hoverBorderColor: string = await page.evaluate(() => {
    for (const sheet of Array.from(document.styleSheets)) {
      let rules: CSSRuleList;
      try { rules = sheet.cssRules; } catch { continue; }
      for (const rule of Array.from(rules)) {
        if (
          rule instanceof CSSStyleRule &&
          rule.selectorText === '.shell-block:hover'
        ) {
          return rule.style.borderColor || rule.style.getPropertyValue('border-color');
        }
      }
    }
    return '';
  });

  // Must find the rule and it must not be empty / transparent.
  expect(hoverBorderColor).toBeTruthy();
  expect(hoverBorderColor).not.toBe('transparent');
  // Must reference the accent channel values (88, 166, 255).
  expect(hoverBorderColor).toMatch(/88,\s*166,\s*255/);
});

test('right-click block shows Copy command, Copy output, Copy whole block in order', async ({
  page,
}) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1 AS x;');
  await input.press('Enter');
  const block = page.locator('.shell-block').first();
  await expect(block).toBeVisible({ timeout: 10_000 });

  await block.click({ button: 'right' });
  const menu = page.locator('.tab-context-menu');
  await expect(menu).toBeVisible({ timeout: 5_000 });

  // All three copy items must be present.
  const items = menu.locator('.tab-context-menu-item');
  const texts = await items.allTextContents();

  // "Copy command" must precede "Copy output" which must precede "Copy whole block".
  const idxCmd   = texts.findIndex(t => t === 'Copy command');
  const idxOut   = texts.findIndex(t => t === 'Copy output');
  const idxWhole = texts.findIndex(t => t === 'Copy whole block');

  expect(idxCmd).toBeGreaterThanOrEqual(0);
  expect(idxOut).toBeGreaterThan(idxCmd);
  expect(idxWhole).toBeGreaterThan(idxOut);
});

test('"Copy command" copies only the SQL to the clipboard', async ({ page }) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1 AS x;');
  await input.press('Enter');
  const block = page.locator('.shell-block').first();
  await expect(block).toBeVisible({ timeout: 10_000 });

  await block.click({ button: 'right' });
  await expect(page.locator('.tab-context-menu')).toBeVisible({ timeout: 5_000 });

  await page.locator('.tab-context-menu-item', { hasText: 'Copy command' }).click();

  // clipboard.writeText is async; wait briefly before reading.
  await page.waitForTimeout(200);
  const text: string = await page.evaluate(() => navigator.clipboard.readText());

  expect(text).toMatch(/select\s+1\s+as\s+x/i);
  // Must not contain the result value or column name from the output row.
  // The output line would be "x\t1" (TSV); check neither appears in isolation.
  expect(text).not.toMatch(/^\s*1\s*$/m);
});

test('"Copy output" gives TSV with header and value', async ({ page }) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1 AS x;');
  await input.press('Enter');
  const block = page.locator('.shell-block').first();
  await expect(block).toBeVisible({ timeout: 10_000 });

  await block.click({ button: 'right' });
  await expect(page.locator('.tab-context-menu')).toBeVisible({ timeout: 5_000 });

  await page.locator('.tab-context-menu-item', { hasText: 'Copy output' }).click();
  await page.waitForTimeout(200);
  const text: string = await page.evaluate(() => navigator.clipboard.readText());

  // TSV output: header row ("x") + data row ("1").
  expect(text).toContain('x');
  expect(text).toContain('1');
  // Must not contain the SQL keyword "select".
  expect(text).not.toMatch(/select/i);
});

test('"Copy whole block" contains both SQL and output, starts with prompt', async ({ page }) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1 AS x;');
  await input.press('Enter');
  const block = page.locator('.shell-block').first();
  await expect(block).toBeVisible({ timeout: 10_000 });

  await block.click({ button: 'right' });
  await expect(page.locator('.tab-context-menu')).toBeVisible({ timeout: 5_000 });

  await page.locator('.tab-context-menu-item', { hasText: 'Copy whole block' }).click();
  await page.waitForTimeout(200);
  const text: string = await page.evaluate(() => navigator.clipboard.readText());

  // Whole block starts with "N SQL>" prompt prefix.
  expect(text).toMatch(/^\d+\s+SQL>/);
  // Must contain the SQL.
  expect(text).toMatch(/select\s+1\s+as\s+x/i);
  // Must contain the result value.
  expect(text).toContain('1');
});

test('right-click on textarea shows shell-level items only, no block items', async ({ page }) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1 AS x;');
  await input.press('Enter');
  await expect(page.locator('.shell-block').first()).toBeVisible({ timeout: 10_000 });

  // Right-click the textarea itself (not a block).
  await input.click({ button: 'right' });
  const menu = page.locator('.tab-context-menu');
  await expect(menu).toBeVisible({ timeout: 5_000 });

  const texts = await menu.locator('.tab-context-menu-item').allTextContents();

  // Block-scoped items must be absent — when block_num is None the menu
  // skips the entire "Copy command / Copy output / Copy whole block" group.
  expect(texts).not.toContain('Copy command');
  expect(texts).not.toContain('Copy output');
  expect(texts).not.toContain('Copy whole block');

  // Shell-level items must be present.
  const hasClear    = texts.some(t => /^clear shell buffer/i.test(t));
  const hasFormat   = texts.some(t => /^format with/i.test(t));
  expect(hasClear).toBe(true);
  expect(hasFormat).toBe(true);
});

// ── PR #35: Collapse / Expand ─────────────────────────────────────────────────

test('"Collapse output" menu item present for tabular block', async ({ page }) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1 AS x;');
  await input.press('Enter');
  const block = page.locator('.shell-block').first();
  await expect(block).toBeVisible({ timeout: 10_000 });

  await block.click({ button: 'right' });
  await expect(page.locator('.tab-context-menu')).toBeVisible({ timeout: 5_000 });

  const collapseItem = page.locator('.tab-context-menu-item', {
    hasText: /^collapse output$/i,
  });
  await expect(collapseItem).toBeVisible({ timeout: 3_000 });
});

test('clicking "Collapse output" applies .shell-collapsed-region-clipped with 400px max-height', async ({
  page,
}) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1 AS x;');
  await input.press('Enter');
  const block = page.locator('.shell-block').first();
  await expect(block).toBeVisible({ timeout: 10_000 });

  await block.click({ button: 'right' });
  await expect(page.locator('.tab-context-menu')).toBeVisible({ timeout: 5_000 });

  await page.locator('.tab-context-menu-item', {
    hasText: /^collapse output$/i,
  }).click();

  // After collapsing, the Table wrapper gains .shell-collapsed-region-clipped.
  const clipped = block.locator('.shell-collapsed-region-clipped');
  await expect(clipped).toBeVisible({ timeout: 3_000 });

  // The CSS rule sets max-height: 400px and overflow: auto on that class.
  // Read computed style to confirm the class actually applied the values —
  // a missing CSS rule would leave max-height as "none".
  const maxHeight: string = await clipped.evaluate(el =>
    window.getComputedStyle(el).maxHeight,
  );
  const overflow: string = await clipped.evaluate(el =>
    window.getComputedStyle(el).overflow,
  );

  expect(maxHeight).toBe('400px');
  // CSS `overflow: auto` computes as "auto" (shorthand resolves uniformly).
  expect(overflow).toBe('auto');
});

test('"Expand output" replaces "Collapse output" after collapsing', async ({ page }) => {
  const input = await setupShell(page);
  await input.fill('SELECT 1 AS x;');
  await input.press('Enter');
  const block = page.locator('.shell-block').first();
  await expect(block).toBeVisible({ timeout: 10_000 });

  // Collapse.
  await block.click({ button: 'right' });
  await expect(page.locator('.tab-context-menu')).toBeVisible({ timeout: 5_000 });
  await page.locator('.tab-context-menu-item', {
    hasText: /^collapse output$/i,
  }).click();

  // Verify the clipped state took effect before checking the menu again.
  await expect(block.locator('.shell-collapsed-region-clipped')).toBeVisible({ timeout: 3_000 });

  // Re-open the menu on the same block.
  await block.click({ button: 'right' });
  await expect(page.locator('.tab-context-menu')).toBeVisible({ timeout: 5_000 });

  const expandItem = page.locator('.tab-context-menu-item', {
    hasText: /^expand output$/i,
  });
  await expect(expandItem).toBeVisible({ timeout: 3_000 });

  const collapseItem = page.locator('.tab-context-menu-item', {
    hasText: /^collapse output$/i,
  });
  await expect(collapseItem).not.toBeVisible();
});

test('no collapse/expand item when right-clicking outside any block', async ({ page }) => {
  const input = await setupShell(page);

  // \timing calls meta_respond() directly — which pushes a MetaOutput entry
  // WITHOUT a preceding ShellEntry::Query. That means no `.shell-block`
  // wrapper is created; the MetaOutput is rendered as an orphan directly
  // inside `.shell-output`. Right-clicking the output area outside any block
  // (block_num == None) must produce a menu with only the shell-level items
  // and no block-scoped Copy / Collapse / Expand items.
  await input.fill('\\timing');
  await input.press('Enter');

  // Wait for the MetaOutput acknowledgement ("Timing is on." / "off.").
  await expect(page.locator('.shell-meta-output').last()).toBeVisible({ timeout: 10_000 });

  // Right-click the output area at a position that is NOT inside any .shell-block.
  // Because \timing never emits a Query entry, the only content in the output
  // is the orphan MetaOutput — no .shell-block div exists at this point.
  // Clicking near the top of the output area (y=5) safely avoids any block.
  const outputArea = page.locator('.shell-output').first();
  await outputArea.click({ button: 'right', position: { x: 10, y: 5 } });
  await expect(page.locator('.tab-context-menu')).toBeVisible({ timeout: 5_000 });

  const texts = await page
    .locator('.tab-context-menu-item')
    .allTextContents();

  // Neither "Collapse output" nor "Expand output" should appear.
  // block_num is None → the entire block-scoped section is skipped in the
  // menu render, regardless of what entries exist in the history.
  const hasCollapse = texts.some(t => /collapse output/i.test(t));
  const hasExpand   = texts.some(t => /expand output/i.test(t));
  const hasCopyCmd  = texts.some(t => t === 'Copy command');
  expect(hasCollapse).toBe(false);
  expect(hasExpand).toBe(false);
  expect(hasCopyCmd).toBe(false);

  // Toggle \timing back off so the pane is in a clean state.
  await page.locator('.tab-context-menu-backdrop').click();
  await input.fill('\\timing');
  await input.press('Enter');
});
