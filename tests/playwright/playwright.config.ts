import { defineConfig, devices } from '@playwright/test';

// User runs `cargo run -p noctua` and `trunk serve` themselves. Helpers in
// `helpers/app.ts` probe both URLs and fail fast if either is down.
// Opt-in autostart for CI: PLAYWRIGHT_AUTOSTART=1 (not wired yet).

export default defineConfig({
  testDir: '.',
  testMatch: ['screenshots/**/*.spec.ts', 'regression/**/*.spec.ts'],
  outputDir: 'test-results',
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 2 : 0,
  reporter: [
    ['list'],
    ['html', { open: 'never', outputFolder: 'playwright-report' }],
  ],
  globalSetup: './global-setup.ts',
  globalTeardown: './global-teardown.ts',
  use: {
    baseURL: 'http://localhost:8080',
    headless: true,
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 2,
    trace: 'retain-on-failure',
    video: 'off',
    screenshot: 'off',
    actionTimeout: 10_000,
    navigationTimeout: 30_000,
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
});
