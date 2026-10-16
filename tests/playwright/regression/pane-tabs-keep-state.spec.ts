import { test, expect, Page } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, activatePaneByIndex, visibleShellInput, visibleShellOutput, submitInActivePane, splitActivePane } from '../helpers/panes';

// Switching or adding pane tabs used to rebuild every pane in the
// workspace: the closure that drew the connection header read the
// leaf's active tab, so it re-ran — and re-rendered the whole pane tree
// — on every tab click. A shell lost its scroll position when you came
// back to it, and Leptos panicked (OwnerDisposed) whenever a pane built
// during one update was dropped before its effects first ran. Opening
// the explorer from `\d <name>` hit that on every use and froze the
// page.
//
// Needs the suite's "Local Postgres" connection on Pagila (`film`,
// `actor`), like the other regression specs.
//
// What these tests pin:
//   1. Adding a tab and switching tabs raise no Leptos panics.
//   2. A shell keeps its scroll position across a tab round-trip.
//   3. The table in `\d <table>`'s output opens the Schema Explorer on
//      it, again with an explorer already open, and `\d <missing>`
//      answers in the shell.
//   4. Splitting, zooming (double-click a tab) and closing leaves keep
//      a shell as it was — same element, scroll position, typed text and
//      cursor — without panics. Panes used to sit inside the layout
//      tree's boxes, so every split or close rebuilt every pane (the
//      scrollback went back to the top), and a split wrote the zoom and
//      the layout separately: two rebuilds, 15 panics. A tab click
//      re-rendered the tab bar, so a double-click never zoomed.
//   5. The New connection form leaves the panes as they were (showing it
//      unmounted every workspace).
//   6. A shell keeps its scroll position and the keyboard across a
//      round trip to another connection's workspace (coverage: this
//      never broke).

function collectPanics(page: Page): string[] {
  const panics: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error' && m.text().includes('panicked')) panics.push(m.text().split('\n')[0]);
  });
  page.on('pageerror', (e) => panics.push(String(e)));
  return panics;
}

// A shell next to whatever the connection opens by default.
async function openShell(page: Page): Promise<void> {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  await expect(visibleShellInput(page)).toBeVisible({ timeout: 15_000 });
}

// Ten results: enough scrollback to park mid-way. Each is in once the
// shell has scrolled to it — on the frame after it renders — so a park
// that follows isn't undone by that scroll.
async function fillScrollback(page: Page): Promise<void> {
  const out = visibleShellOutput(page);
  for (let i = 0; i < 10; i++) {
    await submitInActivePane(page, 'SELECT generate_series(1, 20);');
    await expect(out.locator('.shell-table-wrap')).toHaveCount(i + 1, { timeout: 8_000 });
    await expect.poll(() => out.evaluate((el) => el.scrollHeight - el.clientHeight - el.scrollTop)).toBeLessThanOrEqual(1);
  }
}

test('switching pane tabs keeps a shell where it was, without panics', async ({ page }) => {
  const panics = collectPanics(page);
  await openShell(page);
  const shellIndex = (await page.locator('.pane-tab').count()) - 1;

  // Enough output to scroll, then park the scrollback mid-way.
  await fillScrollback(page);
  const out = visibleShellOutput(page);
  await out.evaluate((el) => { el.scrollTop = 500; });
  await expect.poll(() => out.evaluate((el) => el.scrollTop)).toBe(500);

  for (let round = 0; round < 3; round++) {
    await activatePaneByIndex(page, 0);
    await activatePaneByIndex(page, shellIndex);
  }

  await expect(visibleShellInput(page)).toBeVisible();
  expect(await visibleShellOutput(page).evaluate((el) => el.scrollTop)).toBe(500);
  expect(panics).toEqual([]);
});

// Park a shell mid-scrollback with text typed and the cursor inside it,
// and mark its element, to tell it apart from a rebuilt one.
async function parkShell(page: Page): Promise<void> {
  await fillScrollback(page);
  const input = visibleShellInput(page);
  await input.fill('select 42');
  await input.evaluate((el: HTMLTextAreaElement) => el.setSelectionRange(3, 3));
  await visibleShellOutput(page).evaluate((el) => {
    el.scrollTop = 500;
    (el as unknown as { kept: boolean }).kept = true;
  });
}

// The marked shell, wherever it is now; null if it was rebuilt.
function parkedShell(page: Page) {
  return page.evaluate(() => {
    const out = [...document.querySelectorAll<HTMLElement>('.shell-output')]
      .find((el) => (el as unknown as { kept?: boolean }).kept);
    const input = out?.querySelector<HTMLTextAreaElement>('.shell-input-inline');
    return out && input ? { scrollTop: out.scrollTop, value: input.value, caret: input.selectionStart } : null;
  });
}
const PARKED = { scrollTop: 500, value: 'select 42', caret: 3 };

test('splitting, zooming and closing leaves keep a shell as it was', async ({ page }) => {
  const panics = collectPanics(page);
  await openShell(page);
  const shellTab = page.locator('.pane-tab').nth((await page.locator('.pane-tab').count()) - 1);
  await parkShell(page);
  const leaves = page.locator('.pane-leaf-bar:visible');

  await splitActivePane(page, 'vertical');
  await expect(page.locator('.shell-input-inline:visible')).toHaveCount(2, { timeout: 15_000 });
  expect(await parkedShell(page)).toEqual(PARKED);
  await page.locator('.tab-action-icon[title="Split horizontal"]:visible').last().click();
  await expect(page.locator('.shell-input-inline:visible')).toHaveCount(3, { timeout: 15_000 });
  await expect(leaves).toHaveCount(3);
  expect(await parkedShell(page)).toEqual(PARKED);

  // Double-click its tab: its leaf alone fills the workspace; again: back.
  await shellTab.dblclick();
  await expect(leaves).toHaveCount(1);
  await expect(shellTab).toHaveClass(/pane-tab-zoomed/);
  expect(await parkedShell(page)).toEqual(PARKED);
  await shellTab.dblclick();
  await expect(leaves).toHaveCount(3);
  expect(await parkedShell(page)).toEqual(PARKED);

  // Close the two new leaves.
  for (const remaining of [2, 1]) {
    await page.locator('.tab-action-icon[title="Close pane"]:visible').last().click();
    await expect(leaves).toHaveCount(remaining);
    expect(await parkedShell(page)).toEqual(PARKED);
  }
  expect(panics).toEqual([]);
});

test('the New connection form leaves the panes as they were', async ({ page }) => {
  const panics = collectPanics(page);
  await openShell(page);
  await parkShell(page);
  await page.locator('button[title="New connection"]').click();
  await page.getByRole('button', { name: 'Cancel' }).click();
  await expect(visibleShellOutput(page)).toBeVisible();
  expect(await parkedShell(page)).toEqual(PARKED);
  expect(panics).toEqual([]);
});

test('a shell keeps its scroll position across a round trip to another workspace', async ({ page }) => {
  const panics = collectPanics(page);
  await openShell(page);
  await fillScrollback(page);
  const out = visibleShellOutput(page);
  await out.evaluate((el) => { el.scrollTop = 500; });
  await expect.poll(() => out.evaluate((el) => el.scrollTop)).toBe(500);

  await clickConnection(page, 'Local MySQL');
  await clickConnection(page, 'Local Postgres');

  // Back with the keyboard in the input, and the scrollback where it was.
  await expect(visibleShellInput(page)).toBeFocused();
  expect(await visibleShellOutput(page).evaluate((el) => el.scrollTop)).toBe(500);
  expect(panics).toEqual([]);
});

// The title of `\d <table>`'s printed output.
function describedTable(page: Page) {
  return visibleShellOutput(page).locator('.shell-printed').last().locator('.shell-printed-link');
}

test('\\d <table> opens the Schema Explorer on it, also with one already open', async ({ page }) => {
  const panics = collectPanics(page);
  await openShell(page);
  const shellIndex = (await page.locator('.pane-tab').count()) - 1;

  await submitInActivePane(page, '\\d film');
  await describedTable(page).click({ timeout: 10_000 });
  await expect(page.locator('.explorer-inspector-title:visible', { hasText: 'public · film' })).toBeVisible({ timeout: 10_000 });
  await expect(page.locator('.detail-tab-btn:visible').first()).toBeVisible();
  // Opened to show one table, the inspector starts expanded (its ⤢ state).
  const collapse = page.locator('.explorer-inspector-expand[title="Collapse"]:visible');
  await expect(collapse).toBeVisible();
  // Collapsed, the list shows the table selected.
  await collapse.click();
  await expect(page.locator('.explorer-list-row-active:visible', { hasText: 'film' })).toBeVisible();

  await activatePaneByIndex(page, shellIndex);
  await submitInActivePane(page, '\\d actor');
  await expect(describedTable(page)).toHaveText('Table "public.actor"', { timeout: 10_000 });
  await describedTable(page).click();
  await expect(page.locator('.explorer-inspector-title:visible', { hasText: 'public · actor' })).toBeVisible({ timeout: 10_000 });

  await activatePaneByIndex(page, shellIndex);
  await submitInActivePane(page, '\\d nosuch_relation');
  await expect(visibleShellOutput(page).locator('.shell-meta-line', {
    hasText: 'Did not find any relation named "nosuch_relation".',
  })).toBeVisible({ timeout: 10_000 });

  expect(panics).toEqual([]);
});
