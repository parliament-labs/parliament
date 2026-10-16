import { Page, expect } from '@playwright/test';

// Ensure an Activity Pane is visible in the active workspace tab.
// If one is already present (e.g. because the connection's default_pane
// is "activity"), this is a no-op. Otherwise opens one via the pane-menu.
export async function openActivityPane(page: Page): Promise<void> {
  if (await page.locator('.activity-shell').first().isVisible().catch(() => false)) {
    return;
  }
  await page.locator('.pane-menu-trigger').first().click();
  const menu = page.locator('.pane-menu');
  await expect(menu).toBeVisible();
  await menu.locator('.pane-menu-item', { hasText: 'Activity' }).click();
  await expect(page.locator('.activity-shell').first()).toBeVisible();
}

// Set the Activity pane's auto-refresh interval. Explicit values over
// relying on defaults keep screenshot timing deterministic.
export async function setActivityRefresh(
  page: Page,
  secondsOrManual: '1' | '2' | '5' | '10' | '30' | '60' | 'manual',
): Promise<void> {
  const activity = page.locator('.activity-shell').first();
  await expect(activity).toBeVisible();
  const select = activity.locator('.explorer-select').first();
  await select.selectOption(secondsOrManual);
}
