import { test, expect, type Locator, type Page } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { ensureShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';
import { openSchemaExplorer, waitForObjectList } from '../helpers/schema';

// The README's demo GIF, as frames: a screenshot after each typing step
// and on each result, with how long each shows (`frames.json`). Then
// `scripts/readme-demo-gif.py` makes docs/screenshots/readme-demo.gif.
// Frames go to screenshots/_demo/ (ignored). Needs only the two local
// connections (Local Postgres on Pagila, Local MySQL on Sakila).

const FRAMES = path.join(__dirname, '_demo');
const TYPING_MS = 90;   // per two characters typed
const SETTLE_MS = 150;  // the shell scrolls to the result on the next frame
const SCROLL_STEP = 40; // px per frame scrolling the table's detail
const SCROLL_MS = 70;

test.use({ viewport: { width: 1280, height: 720 } });

test('README demo: psql and mysql as they print, then a table in the Schema Explorer', async ({ page }) => {
  test.setTimeout(120_000);
  fs.rmSync(FRAMES, { recursive: true, force: true });
  fs.mkdirSync(FRAMES, { recursive: true });
  const frames: { file: string; ms: number }[] = [];
  const frame = async (ms: number) => {
    const file = `${String(frames.length).padStart(4, '0')}.png`;
    await page.screenshot({ path: path.join(FRAMES, file) });
    frames.push({ file, ms });
  };
  const type = async (text: string) => {
    for (let i = 0; i < text.length; i += 2) {
      await page.keyboard.type(text.slice(i, i + 2));
      await frame(TYPING_MS);
    }
  };
  // Type `text`, press Enter, and hold on what it printed.
  const run = async (text: string, printed: (block: Locator) => Locator, hold: number) => {
    const blocks = visibleShellOutput(page).locator('.shell-block');
    const before = await blocks.count();
    await type(text);
    await page.keyboard.press('Enter');
    await expect(blocks).toHaveCount(before + 1, { timeout: 10_000 });
    await expect(printed(blocks.last())).toBeVisible({ timeout: 10_000 });
    await page.waitForTimeout(SETTLE_MS);
    await frame(hold);
  };
  const shellOn = async (p: Page, connection: string) => {
    await clickConnection(p, connection);
    await ensureShellPane(p);
    await visibleShellInput(p).click();
  };

  await openApp(page);
  await shellOn(page, 'Local Postgres');
  await frame(1200);

  // psql's \d, printed as psql prints it (actor: it fits on screen).
  await run('\\d actor', b => b.locator('.shell-printed').first(), 2800);
  await run('select title, rating, length from film order by length desc limit 5;',
    b => b.locator('.shell-table').first(), 2200);
  // Expanded display, as \x gives it.
  await run('\\x', b => b.locator('.shell-meta-output').first(), 700);
  await run('select film_id, title, rating, rental_rate from film where film_id = 1;',
    b => b.locator('.shell-vertical-block').first(), 2400);

  // The mysql CLI's \G, on a MySQL connection.
  await shellOn(page, 'Local MySQL');
  await frame(700);
  await run('select first_name, last_name from actor limit 2\\G',
    b => b.locator('.shell-vertical-block').first(), 2400);

  // The Schema Explorer: choose a table, its detail over the whole pane,
  // all of it, then its ERD.
  await clickConnection(page, 'Local Postgres');
  await openSchemaExplorer(page);
  await waitForObjectList(page);
  await frame(1200);
  const film = page.locator('.explorer-list-row', { has: page.locator('.ex-td-name', { hasText: /^film$/ }) }).first();
  await film.hover();
  await frame(500);
  await film.click();
  await expect(page.locator('.schema-section-header:visible', { hasText: /relationships/i })).toBeVisible({ timeout: 10_000 });
  await page.waitForTimeout(SETTLE_MS * 3);
  await frame(1800);
  await page.locator('.explorer-inspector-expand[title="Expand"]:visible').click();
  // The pointer out of the way: rows scrolling under it would light up.
  await page.mouse.move(640, 18);
  await page.waitForTimeout(SETTLE_MS);
  await frame(1400);
  const detail = page.locator('.schema-scroll:visible').first();
  const bottom = await detail.evaluate(el => el.scrollHeight - el.clientHeight);
  for (let top = SCROLL_STEP; top < bottom; top += SCROLL_STEP) {
    await detail.evaluate((el, y) => { el.scrollTop = y; }, top);
    await frame(SCROLL_MS);
  }
  await detail.evaluate(el => { el.scrollTop = el.scrollHeight; });
  await frame(2400);

  await page.locator('.detail-tab-btn:visible', { hasText: /^ERD$/ }).click();
  // One generation — its parents, film, its children — two steps
  // smaller, so the whole diagram fits.
  await page.locator('.erd-gen-select[title^="How many generations"]:visible').selectOption('1');
  const zoomOut = page.locator('.erd-zoom-btn[title="Zoom out"]:visible');
  await zoomOut.click();
  await zoomOut.click();
  await expect(page.locator('.erd-card:visible').nth(2)).toBeVisible({ timeout: 10_000 });
  await page.waitForTimeout(400);
  await frame(3200);

  fs.writeFileSync(path.join(FRAMES, 'frames.json'), JSON.stringify(frames, null, 2));
});
