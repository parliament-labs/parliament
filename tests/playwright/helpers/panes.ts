import { Page, Locator, expect } from '@playwright/test';

// Click the "New shell" button in the currently active pane's tab bar.
// Each click creates a fresh backend session (new MySqlConnection /
// PgConnection) for that tab — confirmed via add_tab in main.rs.
export async function addShellPane(page: Page): Promise<void> {
  // The button lives once per visible pane's tab bar; the active
  // pane's bar is the one whose button is clickable.
  await page.locator('.tab-action-icon[title="New shell"]:visible').first().click();
  // After click, one more .pane-tab exists and is active. Wait briefly
  // for the new tab to appear so subsequent tab-index math is stable.
  await page.waitForTimeout(100);
}

// Click the i-th `.pane-tab` in the active leaf's tab bar to activate
// that pane. Tab labels collide when multiple panes share one
// connection (e.g. all say "pagila"), so indexing beats text filtering.
export async function activatePaneByIndex(page: Page, i: number): Promise<void> {
  const tab = page.locator('.pane-tab').nth(i);
  await tab.click();
  await expect(tab).toHaveClass(/pane-tab-active/);
}

// Returns a locator for the shell input of the currently active pane.
// Inactive panes stay mounted but `visibility:hidden`, which Playwright
// counts as not visible — so `:visible` disambiguates.
export function visibleShellInput(page: Page): Locator {
  return page.locator('.shell-input-inline:visible').first();
}

// Same for the visible pane's output container — useful for scoped
// assertions (e.g. "this specific pane's pending indicator").
export function visibleShellOutput(page: Page): Locator {
  return page.locator('.shell-output:visible').first();
}

// Type SQL into the currently active pane and submit. Caller is
// responsible for assertions on the resulting state — this helper
// does NOT wait for completion because some scenarios (lock-wait)
// deliberately want the query to remain in flight.
export async function submitInActivePane(page: Page, sql: string): Promise<void> {
  const input = visibleShellInput(page);
  await expect(input).toBeVisible();
  await input.fill(sql);
  await input.press('Enter');
}

// Click the split-horizontal or split-vertical icon in the currently
// active pane's tab bar.
//   'horizontal' → Parliament's "Split horizontal" button → stacks
//      panes top/bottom with a horizontal divider.
//   'vertical'   → Parliament's "Split vertical" button   → side-by-side
//      with a vertical divider.
// The new pane becomes a fresh leaf in the layout tree.
export async function splitActivePane(
  page: Page,
  direction: 'horizontal' | 'vertical',
): Promise<void> {
  const title = direction === 'horizontal' ? 'Split horizontal' : 'Split vertical';
  await page.locator(`.tab-action-icon[title="${title}"]:visible`).first().click();
  // Give the layout a moment to settle before downstream tab-index math.
  await page.waitForTimeout(150);
}
