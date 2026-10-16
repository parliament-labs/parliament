import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { connectToPagila } from '../helpers/connect';

// Regression test for the v0.3.5 CSS layout bug documented in
// `vault/Engineering/Testing Strategy.md`. A `.pane-tab-bar` with
// `overflow: hidden` silently clipped the absolutely-positioned
// `.pane-menu` dropdown, making it appear invisible. cargo check,
// clippy, unit tests, and code review all missed it — only a browser
// can see that an element has zero rendered dimensions.
//
// Assertion: after clicking the pane-menu trigger, the dropdown must
// have non-zero width and height. This is the exact failure class.
test('pane-menu dropdown renders with non-zero box', async ({ page }) => {
  await openApp(page);
  await connectToPagila(page);

  await page.locator('.pane-menu-trigger').first().click();

  const menu = page.locator('.pane-menu');
  await expect(menu).toBeVisible();

  const box = await menu.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.width).toBeGreaterThan(0);
  expect(box!.height).toBeGreaterThan(0);

  // Items inside the dropdown must also render — catches a different
  // failure mode where the container has size but children are clipped.
  const items = menu.locator('.pane-menu-item');
  await expect(items.first()).toBeVisible();
  const itemCount = await items.count();
  expect(itemCount).toBeGreaterThan(0);
});
