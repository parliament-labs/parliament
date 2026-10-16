import { test, expect, Page } from '@playwright/test';
import path from 'node:path';
import { clickConnection } from '../helpers/connect';

const SHOT_DIR = path.join(__dirname, '_active_connections');

// Returns a locator for the Active Connections section's collapsible
// folder header — used to anchor "rows directly below the header"
// queries.
function activeFolderHeader(page: Page) {
  return page
    .locator('.folder-node-root', { hasText: 'Active Connections' })
    .first();
}

// Count the connection rows currently sitting under Active Connections
// in the DOM. The sidebar renders one flat list — rows are children of
// the same parent as the folder header — so we count `.conn-item`
// siblings between this header and the next `.folder-node-root`.
async function activeRowCount(page: Page): Promise<number> {
  return await page.evaluate(() => {
    const headers = Array.from(document.querySelectorAll('.folder-node-root'));
    const ac = headers.find(h => h.textContent?.includes('Active Connections'));
    if (!ac) return 0;
    let count = 0;
    let el = ac.nextElementSibling;
    while (el && !el.classList.contains('folder-node-root')) {
      if (el.classList.contains('conn-item')) count += 1;
      el = el.nextElementSibling;
    }
    return count;
  });
}

// Click the i-th Active Connections row (0-indexed).
async function clickActiveRow(page: Page, i: number): Promise<void> {
  await page.evaluate((idx) => {
    const headers = Array.from(document.querySelectorAll('.folder-node-root'));
    const ac = headers.find(h => h.textContent?.includes('Active Connections'));
    if (!ac) throw new Error('Active Connections header not found');
    let seen = 0;
    let el = ac.nextElementSibling;
    while (el && !el.classList.contains('folder-node-root')) {
      if (el.classList.contains('conn-item')) {
        if (seen === idx) { (el as HTMLElement).click(); return; }
        seen += 1;
      }
      el = el.nextElementSibling;
    }
    throw new Error(`Active row index ${idx} not found`);
  }, i);
}

// Click an All-Connections row (force_new=true) by name. Lets us spawn
// a 2nd workspace tab on a connection that already has one. Targets
// the rows directly under the "All Connections" folder header so we
// don't get a wrong match elsewhere in the sidebar.
async function clickAllConnectionsRow(page: Page, name: string): Promise<void> {
  await page.evaluate((connName) => {
    const headers = Array.from(document.querySelectorAll('.folder-node-root'));
    const all = headers.find(h => h.textContent?.includes('All Connections'));
    if (!all) throw new Error('All Connections header not found');
    let el = all.nextElementSibling;
    while (el && !el.classList.contains('folder-node-root')) {
      if (el.classList.contains('conn-item')) {
        const nameEl = el.querySelector('.conn-name');
        if (nameEl?.textContent?.trim() === connName) {
          (el as HTMLElement).click();
          return;
        }
      }
      el = el.nextElementSibling;
    }
    throw new Error(`Connection "${connName}" not found under All Connections`);
  }, name);
}

// The visible session-id pill shows "#NNN" — the DB-assigned
// connection id, unique per session. Each workspace tab has its own
// session against the same DB so two tabs on Local MySQL have
// distinct #NNN. Works for any pane kind (Shell / Schema Explorer)
// because both render the pill via `session_id_pill`. Inactive
// workspace tabs are display:none so their pills don't show.
async function visibleSessionId(page: Page): Promise<string | null> {
  return await page.evaluate(() => {
    const pills = Array.from(document.querySelectorAll('.session-id-pill'));
    const visible = pills.find(el => (el as HTMLElement).offsetParent !== null);
    return visible?.textContent ?? null;
  });
}

test('Active Connections section: per-workspace-tab rows + click navigation', async ({ page }) => {
  await page.goto('/');
  await page.waitForSelector('.sidebar', { timeout: 15_000 });

  // (1) Empty state — section header is always shown even with no
  // sessions yet. (No assertion on row count; just presence.)
  await expect(activeFolderHeader(page)).toBeVisible();
  await page.screenshot({ path: path.join(SHOT_DIR, '01_empty_state.png') });

  // (2) Open Local MySQL → spawns workspace tab + Shell pane. Active
  // Connections now has exactly one row.
  await clickConnection(page, 'Local MySQL');
  await page.waitForTimeout(400);
  expect(await activeRowCount(page)).toBe(1);
  await page.screenshot({ path: path.join(SHOT_DIR, '02_one_active.png') });

  // Capture the visible shell-input id as a proxy for the active
  // workspace tab — we'll verify we can come back to it later.
  const idA = await visibleSessionId(page);
  expect(idA).not.toBeNull();

  // (3) Click Local MySQL in All Connections (force_new=true) to spawn
  // a SECOND workspace tab on the same connection. Active Connections
  // now has two rows.
  await clickAllConnectionsRow(page, 'Local MySQL');
  await page.waitForTimeout(500);
  expect(await activeRowCount(page)).toBe(2);
  await page.screenshot({ path: path.join(SHOT_DIR, '03_two_active.png') });

  const idB = await visibleSessionId(page);
  expect(idB).not.toBeNull();
  expect(idB).not.toBe(idA);

  // (4) Click the first Active Connections row → workspace switches
  // back to the first tab (visible shell input id == idA). Click the
  // second → switches to the second tab (== idB). Each row navigates
  // to its specific workspace tab independently.
  await clickActiveRow(page, 0);
  await page.waitForTimeout(300);
  expect(await visibleSessionId(page)).toBe(idA);
  await page.screenshot({ path: path.join(SHOT_DIR, '04_clicked_first.png') });

  await clickActiveRow(page, 1);
  await page.waitForTimeout(300);
  expect(await visibleSessionId(page)).toBe(idB);
  await page.screenshot({ path: path.join(SHOT_DIR, '05_clicked_second.png') });

  // (5) Visual capture of folder-header tint — the mint-green
  // `--sidebar-folder-bg` should be visible. No assertion on color
  // beyond what the screenshot shows.
  await page.screenshot({ path: path.join(SHOT_DIR, '06_final_state.png') });
});
