import { Page, expect } from '@playwright/test';

// Click a sidebar connection entry by its displayed name. The fixture
// `connections.sample.json` seeds "Local Postgres" and "Local MySQL".
//
// After click, wait for a pane tab bearing the connection's name to
// appear in the workspace — this is the universal "pane opened"
// signal, independent of the connection's default pane kind (shell /
// schema / activity).
export async function clickConnection(page: Page, name: string): Promise<void> {
  const item = page
    .locator('.conn-item', { has: page.locator('.conn-name', { hasText: name }) })
    .first();
  await expect(item).toBeVisible({ timeout: 10_000 });
  await item.click();

  // The workspace renders a `.conn-header-name` showing the connection's
  // display name once a pane for it is open. Pane tabs themselves show
  // the database name, not the connection name, so this header is the
  // stable signal.
  const connHeader = page
    .locator('.conn-header-name', { hasText: name })
    .first();
  await expect(connHeader).toBeVisible({ timeout: 15_000 });
}

export async function connectToPagila(page: Page): Promise<void> {
  await clickConnection(page, 'Local Postgres');
}

export async function connectToSakila(page: Page): Promise<void> {
  await clickConnection(page, 'Local MySQL');
}
