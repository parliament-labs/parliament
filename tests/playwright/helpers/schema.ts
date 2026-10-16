import { Page, expect } from '@playwright/test';

// Ensure a Schema Explorer pane is visible in the active workspace tab.
// If one is already present (e.g. because the connection's default_pane
// is "schema"), this is a no-op. Otherwise opens one via the pane-menu.
export async function openSchemaExplorer(page: Page): Promise<void> {
  if (await page.locator('.explorer-shell').first().isVisible().catch(() => false)) {
    return;
  }
  await page.locator('.pane-menu-trigger').first().click();
  const menu = page.locator('.pane-menu');
  await expect(menu).toBeVisible();
  await menu.locator('.pane-menu-item', { hasText: 'Schema Explorer' }).click();
  // Re-query after the click — the pre-click locator would only see
  // the stale DOM state.
  await expect(page.locator('.explorer-shell').first()).toBeVisible();
}

// Wait for the explorer's object list to populate (tables load by default).
export async function waitForObjectList(page: Page): Promise<void> {
  await expect(page.locator('.explorer-list-row').first()).toBeVisible({
    timeout: 15_000,
  });
}
