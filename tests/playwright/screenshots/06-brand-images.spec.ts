import { test, expect } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { ensureShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// The images that stand for Parliament outside the app, made from the
// owl (tyto/favicon.svg — Noto Emoji's, Apache-2.0, see NOTICE):
//   tyto/favicon.png                    — the favicon for browsers that
//                                         don't take an SVG one
//   docs/screenshots/social-preview.png — the card a shared link shows
//                                         (GitHub: Settings → Social preview)
// The card's shell is the real thing — psql's \d actor and a query, in
// Parliament — so it needs the local Postgres connection (Pagila).

const REPO = path.resolve(__dirname, '..', '..', '..');
const OWL = fs.readFileSync(path.join(REPO, 'tyto', 'favicon.svg'), 'utf8');
const sized = (px: number) => OWL.replace('<svg ', `<svg width="${px}" height="${px}" `);

test('favicon PNG from the owl', async ({ page }) => {
  await page.setViewportSize({ width: 64, height: 64 });
  await page.setContent(`<body style="margin:0">${sized(64)}</body>`);
  await page.screenshot({ path: path.join(REPO, 'tyto', 'favicon.png'), omitBackground: true });
});

test('social preview: the owl, what Parliament is, and its shell', async ({ browser }) => {
  // The shell at twice the pixels, so it stays crisp scaled into the card.
  const app = await browser.newContext({ viewport: { width: 1280, height: 720 }, deviceScaleFactor: 2 });
  const page = await app.newPage();
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await ensureShellPane(page);
  const input = visibleShellInput(page);
  const out = visibleShellOutput(page);
  // psql's \d as psql prints it, then a result in the grid.
  await input.fill('\\d actor');
  await input.press('Enter');
  await expect(out.locator('.shell-printed').first()).toBeVisible({ timeout: 10_000 });
  await input.fill('select title, rating, length from film order by length desc limit 5;');
  await input.press('Enter');
  await expect(out.locator('.shell-table').first()).toBeVisible({ timeout: 10_000 });
  await page.mouse.move(640, 18);
  await page.waitForTimeout(300);
  const box = (await out.boundingBox())!;
  const shell = await page.screenshot({ clip: { x: box.x, y: box.y, width: 900, height: 630 } });
  await app.close();

  const card = await browser.newContext({ viewport: { width: 1280, height: 640 } });
  const draw = await card.newPage();
  await draw.setContent(`<!doctype html><html><head><style>
    body { margin: 0; width: 1280px; height: 640px; overflow: hidden; position: relative;
           background: radial-gradient(1100px 640px at 0% 0%, #1b2330 0%, #0d1117 62%);
           font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; color: #e6edf3; }
    .about { position: absolute; left: 72px; top: 0; bottom: 0; width: 500px;
             display: flex; flex-direction: column; justify-content: center; }
    h1 { font-size: 76px; letter-spacing: -1.5px; margin: 18px 0 10px; }
    .what { font-size: 29px; line-height: 1.3; color: #c9d1d9; margin: 0 0 26px; }
    .stack { font-size: 18px; color: #8b949e; margin: 0; white-space: nowrap; }
    .stack b { color: #58a6ff; font-weight: 600; }
    .shell { position: absolute; left: 590px; top: 96px; width: 640px; height: 448px;
             border-radius: 12px; overflow: hidden; border: 1px solid #30363d;
             box-shadow: 0 24px 64px rgba(0, 0, 0, 0.55); }
    .shell img { width: 100%; height: 100%; display: block; }
  </style></head><body>
    <div class="about">
      <div>${sized(132)}</div>
      <h1>Parliament</h1>
      <p class="what">A browser SQL client that behaves exactly like psql and the mysql CLI.</p>
      <p class="stack"><b>PostgreSQL</b> · <b>MySQL</b> · Rust + WebAssembly · Apache-2.0</p>
    </div>
    <div class="shell"><img src="data:image/png;base64,${shell.toString('base64')}"></div>
  </body></html>`);
  await draw.screenshot({ path: path.join(REPO, 'docs', 'screenshots', 'social-preview.png') });
  await card.close();
});
