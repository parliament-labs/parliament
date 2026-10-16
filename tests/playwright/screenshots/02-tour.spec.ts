import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { connectToPagila } from '../helpers/connect';
import { saveScreenshot } from '../helpers/paths';

// Four shots for the Flashy-README "A 30-second tour" section:
//   tour-03-welcome-owl        — welcome pane with 🦉 branding
//   tour-01-sidebar-with-bell  — sidebar showing fixture connections
//   tour-02-add-connection-form — New Connection form
//   tour-04-tabs-splits-control — tab bar with add / split / pane-menu
//
// Runs off the fixture connections installed by global-setup.ts
// (Local Postgres + Local MySQL). No external state required.
test('tour — sidebar, add-connection, welcome, tab controls', async ({ page }) => {
  await openApp(page);

  // tour-03: Welcome pane captured first, before any connection is
  // selected — workspace still shows the welcome state with the owl.
  const welcome = page.locator('.welcome').first();
  await expect(welcome).toBeVisible();
  await saveScreenshot(welcome, 'tour-03-welcome-owl', __filename);

  // tour-01: Sidebar with fixture connections listed. Shooting just
  // the sidebar keeps the framing focused on the connection list.
  const sidebar = page.locator('.sidebar').first();
  await saveScreenshot(sidebar, 'tour-01-sidebar-with-bell', __filename);

  // tour-02: New Connection form. Trigger via the "+" in the app header.
  await page.locator('.app-header-btn[title="New connection"]').first().click();
  await expect(page.locator('.conn-form-pane')).toBeVisible();
  await saveScreenshot(page, 'tour-02-add-connection-form', __filename);

  // Close the form so we can reach the tab bar for shot 4.
  await page.locator('.btn.btn-ghost', { hasText: 'Cancel' }).first().click();
  await expect(page.locator('.conn-form-pane')).not.toBeVisible();

  // tour-04: Tab bar with split / add-tab / pane-menu action icons.
  // Need a real pane to be open — connect to Pagila.
  await connectToPagila(page);
  const tabBar = page.locator('.pane-tab-bar').first();
  await expect(tabBar).toBeVisible();
  // The tab-bar's render closure subscribes to both `leaf.tabs` and
  // `leaf.active` (the fix in 6df1366 for the New-shell button bug).
  // Right after connect, `leaf.active` ticks once more as the new
  // session opens, which re-renders the bar and detaches the old DOM
  // node. The 100ms settle gives that final tick room before we
  // snapshot the (now stable) node.
  await page.waitForTimeout(100);
  await saveScreenshot(tabBar, 'tour-04-tabs-splits-control', __filename);
});
