import { test, expect, Page } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, activatePaneByIndex, visibleShellInput, visibleShellOutput, submitInActivePane } from '../helpers/panes';

// `\d <pattern>` prints what psql prints (psql's describe.c, ported in
// tyto/src/plane/postgres/describe.rs, its catalog queries run in
// batches over IntrospectBatch), and the relation it describes opens
// the Schema Explorer — from the printed title, or the block's
// right-click menu.
//
// Byte-for-byte parity across relation kinds and footers is pinned
// natively (`cargo test -p tyto -- --ignored describe_matches_psql`).
// This spec pins the shell side: the batches come back, the block
// reads as psql's, and the links open the explorer.
//
// Needs the suite's "Local Postgres" connection on Pagila. The text
// comparison also needs psql on PATH (skipped without it).

function psql(cmd: string): string | null {
  try {
    return execFileSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', '-c', cmd]).toString();
  } catch {
    return null;
  }
}

async function openShell(page: Page): Promise<number> {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  await expect(visibleShellInput(page)).toBeVisible({ timeout: 15_000 });
  return (await page.locator('.pane-tab').count()) - 1;
}

const printed = (page: Page) => visibleShellOutput(page).locator('.shell-printed').last();

test('\\d <table> prints what psql prints', async ({ page }) => {
  const theirs = psql('\\d film');
  test.skip(theirs === null, 'psql not on PATH');
  await openShell(page);
  await submitInActivePane(page, '\\d film');
  await expect(printed(page)).toBeVisible({ timeout: 10_000 });
  const ours = await printed(page).evaluate((el) => el.textContent ?? '');
  expect(ours.trimEnd()).toBe(theirs!.trimEnd());
});

test('the relation in \\d output opens the Schema Explorer', async ({ page }) => {
  const panics: string[] = [];
  page.on('console', (m) => { if (m.type() === 'error' && m.text().includes('panicked')) panics.push(m.text()); });
  const shellIndex = await openShell(page);

  // The title of a table links to it.
  await submitInActivePane(page, '\\d film');
  const title = printed(page).locator('.shell-printed-link');
  await expect(title).toHaveText('Table "public.film"', { timeout: 10_000 });

  // So does the block's right-click menu, above the Copy items.
  await printed(page).click({ button: 'right' });
  const items = page.locator('.tab-context-menu .tab-context-menu-item');
  await expect(items.first()).toHaveText('Open public.film in Schema Explorer');
  await expect(items.nth(1)).toHaveText('Copy command');
  await page.locator('.tab-context-menu-backdrop').click({ force: true });

  await title.click();
  await expect(page.locator('.explorer-inspector-title:visible', { hasText: 'public · film' })).toBeVisible({ timeout: 10_000 });

  // An index links the table in its footer; a materialized view (which
  // the explorer doesn't show) has no link.
  await activatePaneByIndex(page, shellIndex);
  await submitInActivePane(page, '\\d film_pkey');
  await expect(printed(page)).toContainText('Index "public.film_pkey"', { timeout: 10_000 });
  await expect(printed(page).locator('.shell-printed-link')).toHaveText('public.film');
  await submitInActivePane(page, '\\d rental_by_category');
  await expect(printed(page)).toContainText('Materialized view "public.rental_by_category"', { timeout: 10_000 });
  await expect(printed(page).locator('.shell-printed-link')).toHaveCount(0);

  // psql's answer for nothing found.
  await submitInActivePane(page, '\\d nosuch');
  await expect(visibleShellOutput(page).locator('.shell-meta-line', {
    hasText: 'Did not find any relation named "nosuch".',
  })).toBeVisible({ timeout: 10_000 });

  expect(panics).toEqual([]);
});
