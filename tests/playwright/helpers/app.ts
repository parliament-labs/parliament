import { Page, expect } from '@playwright/test';

const FRONTEND_URL = 'http://localhost:8080';
const BACKEND_URL = 'http://localhost:3000';

// Probe trunk + backend once per process so a missing server fails fast
// with a clear message instead of a 30s Playwright navigation timeout.
let probed = false;
async function probeServers(): Promise<void> {
  if (probed) return;
  const checks: Array<[string, string]> = [
    [FRONTEND_URL, 'trunk serve (frontend)'],
    [`${BACKEND_URL}/ws`, 'cargo run -p noctua (backend WebSocket)'],
  ];
  for (const [url, label] of checks) {
    try {
      await fetch(url, { method: 'GET' });
    } catch (err) {
      throw new Error(
        `Cannot reach ${label} at ${url}. ` +
          `Start it in another terminal before running Playwright.`,
      );
    }
  }
  probed = true;
}

// Load the app and wait until the top-level shell is mounted.
// Leptos mount_to_body means `<div class="app">` appears once WASM boots.
export async function openApp(page: Page): Promise<void> {
  await probeServers();
  await page.goto('/');
  await expect(page.locator('.app')).toBeVisible({ timeout: 20_000 });
  await expect(page.locator('.sidebar')).toBeVisible();
}
