import { test, expect, Page } from '@playwright/test';
import path from 'node:path';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput } from '../helpers/panes';

// Ad-hoc verification for the tab-completion feature shipped in commit
// 69beed8. Captures screenshots through the canonical flows; not a
// regression test — single-shot evidence for a reviewer.

const SHOT_DIR = path.resolve(__dirname, '..', 'screenshots', '_tab_completion');

async function shellInput(page: Page) {
  return visibleShellInput(page);
}

async function snapshot(page: Page, name: string) {
  await page.screenshot({ path: path.join(SHOT_DIR, `${name}.png`), fullPage: false });
}

test.beforeAll(async () => {
  const fs = await import('node:fs');
  fs.mkdirSync(SHOT_DIR, { recursive: true });
});

test('PG tab completion — meta-commands, keywords, tables', async ({ page }) => {
  await page.goto('/');
  await page.waitForSelector('.sidebar', { timeout: 15_000 });

  // Connect to "Local Postgres" (pagila) from the fixture.
  await clickConnection(page, 'Local Postgres');
  // Local Postgres' default pane is Schema Explorer; explicitly add a
  // Shell pane so the input textarea is reachable.
  await addShellPane(page);
  const input = await shellInput(page);
  await input.waitFor({ state: 'visible', timeout: 20_000 });
  await input.click();

  // ── (1) \d<Tab> → PG meta-commands ─────────────────────────────────
  await input.fill('\\d');
  await input.press('Tab');
  await page.waitForTimeout(150);
  let popupVisible = await page.locator('.completion-popup').isVisible();
  await snapshot(page, '01_pg_meta_command_d');
  expect(popupVisible).toBeTruthy();
  let items = await page.locator('.completion-popup-item').allTextContents();
  // Expect a generous set of `\d*` commands
  expect(items.length).toBeGreaterThan(20);
  expect(items).toContain('\\dt');
  expect(items).toContain('\\dnS');
  // Esc dismisses
  await input.press('Escape');
  await page.waitForTimeout(50);
  expect(await page.locator('.completion-popup').isVisible()).toBeFalsy();

  // ── Reset input ───────────────────────────────────────────────────
  await input.fill('');

  // ── (2) SEL<Tab> → keywords ────────────────────────────────────────
  await input.fill('SEL');
  await input.press('Tab');
  await page.waitForTimeout(150);
  popupVisible = await page.locator('.completion-popup').isVisible();
  // SEL may have only SELECT — single-candidate path inserts directly.
  // Either way we should NOT have left the literal "SEL" untouched.
  const value = await input.inputValue();
  await snapshot(page, '02_pg_keyword_sel');
  expect(value).toContain('SELECT');

  await input.fill('');

  // ── (3) FROM <Tab> → tables in public ──────────────────────────────
  // Eager-fetch on SessionOpened means a single Tab now lands the popup
  // already populated — no two-tap fetch dance needed.
  await input.fill('SELECT * FROM ');
  await input.press('Tab');
  await page.waitForTimeout(200);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  items = await page.locator('.completion-popup-item').allTextContents();
  await snapshot(page, '03_pg_from_tab');
  // Pagila's public schema should include at least: actor, address,
  // category, city, country, customer, film, etc.
  expect(items).toContain('actor');
  expect(items).toContain('film');
  expect(items.length).toBeGreaterThan(10);

  // ── (4) Type "ti" to narrow to "title"… well, Pagila has `film` not
  // `title`. Narrow to `fi` to get `film` family instead.
  await input.type('fi', { delay: 30 });
  await page.waitForTimeout(150);
  items = await page.locator('.completion-popup-item').allTextContents();
  await snapshot(page, '04_pg_narrowed_fi');
  expect(items.length).toBeGreaterThan(0);
  // Every visible item must start with "fi" or "film" given the prefix
  items.forEach(t => expect(t.startsWith('fi') || t.startsWith('film')).toBeTruthy());

  // ── (5) ArrowDown cycles selection without moving the textarea cursor.
  // Read the current cursor position; press ArrowDown; cursor position
  // should be unchanged.
  const cursorBefore = await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    return ta?.selectionStart ?? -1;
  });
  await input.press('ArrowDown');
  await page.waitForTimeout(50);
  const cursorAfter = await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    return ta?.selectionStart ?? -1;
  });
  await snapshot(page, '05_pg_arrowdown_cycled');
  expect(cursorAfter).toBe(cursorBefore);
  // The selected item should have advanced (class .completion-popup-item-selected on the 2nd row)
  const selectedIdx = await page.evaluate(() => {
    const items = Array.from(document.querySelectorAll('.completion-popup-item'));
    return items.findIndex(el => el.classList.contains('completion-popup-item-selected'));
  });
  expect(selectedIdx).toBe(1);

  // ── (6) Esc dismisses
  await input.press('Escape');
  await page.waitForTimeout(50);
  expect(await page.locator('.completion-popup').isVisible()).toBeFalsy();
  await snapshot(page, '06_pg_esc_dismissed');
});

test('PG column completion (Phase 1)', async ({ page }) => {
  await page.goto('/');
  await page.waitForSelector('.sidebar', { timeout: 15_000 });
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = await shellInput(page);
  await input.waitFor({ state: 'visible', timeout: 20_000 });
  await input.click();

  // Give eager fetch for tables a beat to land so the dot-prefetch can
  // see "film" as a known table.
  await page.waitForTimeout(400);

  // ── SELECT <Tab> with empty prefix → tables only ──────────────────
  await input.fill('SELECT ');
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  let items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '09_pg_select_empty_tables.png') });
  expect(items).toContain('actor');
  expect(items).toContain('film');
  // No SELECT or DISTINCT — keywords only land with a non-empty prefix
  expect(items).not.toContain('SELECT');
  expect(items).not.toContain('DISTINCT');
  await input.press('Escape');

  // ── SELECT DIS<Tab> → DISTINCT (keyword survives non-empty prefix) ─
  await input.fill('');
  await input.fill('SELECT DIS');
  await input.press('Tab');
  await page.waitForTimeout(150);
  // Single-candidate path inserts directly; check the input value.
  const valueAfterDis = await input.inputValue();
  await page.screenshot({ path: path.join(SHOT_DIR, '10_pg_select_dis_keyword.png') });
  expect(valueAfterDis).toContain('DISTINCT');

  // ── SELECT film.<Tab> → film's columns ────────────────────────────
  await input.fill('');
  // Type char-by-char so on:input fires after each — the eager column
  // prefetch hook reads the just-typed `.`.
  await input.type('SELECT film.', { delay: 30 });
  // Give the column fetch a beat to land.
  await page.waitForTimeout(400);
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '11_pg_film_columns.png') });
  // Pagila's film table includes: film_id, title, description, release_year,
  // language_id, original_language_id, rental_duration, rental_rate,
  // length, replacement_cost, rating, last_update, special_features,
  // fulltext.
  expect(items).toContain('film_id');
  expect(items).toContain('title');
  expect(items).toContain('description');
  expect(items).not.toContain('actor');         // not a column
  expect(items).not.toContain('SELECT');        // not a keyword context
  await input.press('Escape');

  // ── SELECT film.ti<Tab> → narrows to title ────────────────────────
  await input.fill('');
  await input.type('SELECT film.ti', { delay: 30 });
  await page.waitForTimeout(400);
  await input.press('Tab');
  await page.waitForTimeout(150);
  // Single candidate path may inline-insert; check input value
  const valueAfterTi = await input.inputValue();
  await page.screenshot({ path: path.join(SHOT_DIR, '12_pg_film_ti_narrowed.png') });
  expect(valueAfterTi).toContain('film.title');

  // ── SELECT f.<Tab> with no table 'f' → nothing ────────────────────
  await input.fill('');
  await input.type('SELECT f.', { delay: 30 });
  await page.waitForTimeout(400);
  await input.press('Tab');
  await page.waitForTimeout(150);
  // No popup (no table named 'f' in public)
  expect(await page.locator('.completion-popup').isVisible()).toBeFalsy();
  await page.screenshot({ path: path.join(SHOT_DIR, '13_pg_f_dot_nothing.png') });
});

test('PG alias resolution (Phase 2)', async ({ page }) => {
  await page.goto('/');
  await page.waitForSelector('.sidebar', { timeout: 15_000 });
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = await shellInput(page);
  await input.waitFor({ state: 'visible', timeout: 20_000 });
  await input.click();
  await page.waitForTimeout(400); // eager table fetch

  // ── (1) `SELECT a.<Tab> FROM actor a` → actor's columns ───────────
  // We type char-by-char so on:input fires for the dot prefetch. The
  // FROM clause is typed first so the alias map is built when we type
  // the dot after `a`.
  await input.fill('SELECT FROM actor a');
  // Now move cursor to right after `SELECT ` (position 7) and type `a.`
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    ta.setSelectionRange(7, 7);
  });
  await input.type('a.', { delay: 30 });
  await page.waitForTimeout(400); // give column fetch a beat
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  let items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '14_pg_alias_a_columns.png') });
  // Pagila's actor table: actor_id, first_name, last_name, last_update
  expect(items).toContain('actor_id');
  expect(items).toContain('first_name');
  expect(items).toContain('last_name');
  expect(items).toContain('last_update');
  await input.press('Escape');

  // ── (2) `SELECT a.<Tab> FROM actor AS a` → with explicit AS ────────
  await input.fill('SELECT FROM actor AS a');
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    ta.setSelectionRange(7, 7);
  });
  await input.type('a.', { delay: 30 });
  await page.waitForTimeout(400);
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '15_pg_alias_as_columns.png') });
  expect(items).toContain('actor_id');
  await input.press('Escape');

  // ── (3) JOIN alias — `SELECT b.<Tab> FROM actor a JOIN film_actor b ON …` ──
  await input.fill('SELECT FROM actor a JOIN film_actor b ON a.actor_id = b.actor_id');
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    ta.setSelectionRange(7, 7);
  });
  await input.type('b.', { delay: 30 });
  await page.waitForTimeout(400);
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '16_pg_join_alias_b_columns.png') });
  // film_actor: actor_id, film_id, last_update
  expect(items).toContain('actor_id');
  expect(items).toContain('film_id');
  expect(items).toContain('last_update');
  await input.press('Escape');

  // ── (4) Subquery in FROM → alias is NOT resolved (Phase 3) ─────────
  await input.fill('SELECT FROM (SELECT 1) s');
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    ta.setSelectionRange(7, 7);
  });
  await input.type('s.', { delay: 30 });
  await page.waitForTimeout(400);
  await input.press('Tab');
  await page.waitForTimeout(150);
  // No popup — Phase 2 deliberately doesn't resolve inline-view aliases
  expect(await page.locator('.completion-popup').isVisible()).toBeFalsy();
  await page.screenshot({ path: path.join(SHOT_DIR, '17_pg_subquery_alias_nothing.png') });
});

test('PG CTE + inline-view projection (Phase 3)', async ({ page }) => {
  await page.goto('/');
  await page.waitForSelector('.sidebar', { timeout: 15_000 });
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = await shellInput(page);
  await input.waitFor({ state: 'visible', timeout: 20_000 });
  await input.click();
  await page.waitForTimeout(400);

  // ── (1) CTE: WITH q AS (SELECT … FROM actor) SELECT q.<Tab> ───────
  // No column fetch needed — CTE projection is computed statically.
  await input.fill('WITH q AS (SELECT actor_id, first_name FROM actor) SELECT q. FROM q');
  // Cursor right after `q.`. Find it: `WITH q AS (SELECT actor_id, first_name FROM actor) SELECT q.` length
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    const idx = ta.value.indexOf('q.') + 2;  // position right after q.
    // But there are TWO `q.` matches if FROM q comes first; use the
    // second one (after SELECT). Search the SELECT portion.
    const selectIdx = ta.value.indexOf('SELECT q.');
    ta.setSelectionRange(selectIdx + 'SELECT q.'.length, selectIdx + 'SELECT q.'.length);
    void idx;
  });
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  let items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '18_pg_cte_projection.png') });
  expect(items).toEqual(['actor_id', 'first_name']); // exactly these two, no others
  await input.press('Escape');

  // ── (2) Inline view: FROM (SELECT … FROM actor) s — SELECT s.<Tab> ─
  await input.fill('SELECT s. FROM (SELECT actor_id, first_name FROM actor) s');
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    const idx = ta.value.indexOf('SELECT s.') + 'SELECT s.'.length;
    ta.setSelectionRange(idx, idx);
  });
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '19_pg_inline_view_projection.png') });
  expect(items).toEqual(['actor_id', 'first_name']);
  await input.press('Escape');

  // ── (3) Inline view with AS-aliased function calls ───────────────
  await input.fill('SELECT s. FROM (SELECT COUNT(*) AS n, MAX(actor_id) AS hi FROM actor) s');
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    const idx = ta.value.indexOf('SELECT s.') + 'SELECT s.'.length;
    ta.setSelectionRange(idx, idx);
  });
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '20_pg_inline_view_as_aliases.png') });
  // AS aliases win — function-call projections without AS would be skipped
  expect(items).toContain('n');
  expect(items).toContain('hi');
  await input.press('Escape');

  // ── (4) Inline view with no nameable projection → no popup ────────
  // SELECT 1 has no column name to synthesize; star expansion deferred.
  await input.fill('SELECT s. FROM (SELECT 1) s');
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    const idx = ta.value.indexOf('SELECT s.') + 'SELECT s.'.length;
    ta.setSelectionRange(idx, idx);
  });
  await input.press('Tab');
  await page.waitForTimeout(150);
  // No projection extracted → no alias map entry → no popup
  expect(await page.locator('.completion-popup').isVisible()).toBeFalsy();
  await page.screenshot({ path: path.join(SHOT_DIR, '21_pg_inline_view_no_projection.png') });

  // ── (5) CTE shadows real table of the same name ───────────────────
  // Match real SQL semantics: `WITH actor AS (SELECT 1 AS x)` defines
  // a CTE named `actor` that shadows the schema's actor table for
  // this query. `actor.<Tab>` should surface the CTE's projection.
  await input.fill('WITH actor AS (SELECT 1 AS x) SELECT actor. FROM actor');
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    const idx = ta.value.indexOf('SELECT actor.') + 'SELECT actor.'.length;
    ta.setSelectionRange(idx, idx);
  });
  await input.press('Tab');
  await page.waitForTimeout(150);
  // Single-candidate path: the CTE's projection has exactly one column
  // (`x`), so Tab auto-inserts it without showing a popup. Check the
  // input value instead. The presence of `x` (and absence of `actor_id`)
  // confirms the CTE shadowed the real actor table.
  const valueAfterCte = await input.inputValue();
  await page.screenshot({ path: path.join(SHOT_DIR, '22_pg_cte_shadows_table.png') });
  expect(valueAfterCte).toContain('actor.x');
  expect(valueAfterCte).not.toContain('actor.actor_id');
});

test('PG star expansion in CTEs + inline views (Phase 4)', async ({ page }) => {
  await page.goto('/');
  await page.waitForSelector('.sidebar', { timeout: 15_000 });
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = await shellInput(page);
  await input.waitFor({ state: 'visible', timeout: 20_000 });
  await input.click();
  await page.waitForTimeout(400);

  // ── (1) CTE with bare star: WITH q AS (SELECT * FROM actor) ──────
  // Type whole query so on:input fires for each char — star prefetch
  // grabs actor's columns; by the time we Tab, expansion succeeds.
  await input.fill('');
  await input.type('WITH q AS (SELECT * FROM actor) SELECT q. FROM q', { delay: 20 });
  await page.waitForTimeout(500); // give the star prefetch a beat
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    const i = ta.value.indexOf('SELECT q.') + 'SELECT q.'.length;
    ta.setSelectionRange(i, i);
  });
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  let items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '23_pg_cte_star_expansion.png') });
  // actor's columns: actor_id, first_name, last_name, last_update
  expect(items).toContain('actor_id');
  expect(items).toContain('first_name');
  expect(items).toContain('last_name');
  expect(items).toContain('last_update');
  await input.press('Escape');

  // ── (2) Inline view with bare star: FROM (SELECT * FROM actor) s ─
  await input.fill('');
  await input.type('SELECT s. FROM (SELECT * FROM actor) s', { delay: 20 });
  await page.waitForTimeout(500);
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    const i = ta.value.indexOf('SELECT s.') + 'SELECT s.'.length;
    ta.setSelectionRange(i, i);
  });
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '24_pg_inline_star_expansion.png') });
  expect(items).toContain('actor_id');
  expect(items).toContain('first_name');
  await input.press('Escape');

  // ── (3) Table-qualified star: SELECT u.* FROM actor u ─────────────
  await input.fill('');
  await input.type('SELECT s. FROM (SELECT u.* FROM actor u) s', { delay: 20 });
  await page.waitForTimeout(500);
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    const i = ta.value.indexOf('SELECT s.') + 'SELECT s.'.length;
    ta.setSelectionRange(i, i);
  });
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '25_pg_table_star.png') });
  expect(items).toContain('actor_id');
  expect(items).toContain('first_name');
  await input.press('Escape');

  // ── (4) Mixed projection: AS-aliased + table-star ────────────────
  await input.fill('');
  await input.type('SELECT s. FROM (SELECT 1 AS extra, u.* FROM actor u) s', { delay: 20 });
  await page.waitForTimeout(500);
  await page.evaluate(() => {
    const ta = document.querySelector('textarea.shell-input-inline') as HTMLTextAreaElement;
    ta.focus();
    const i = ta.value.indexOf('SELECT s.') + 'SELECT s.'.length;
    ta.setSelectionRange(i, i);
  });
  await input.press('Tab');
  await page.waitForTimeout(150);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '26_pg_mixed_star.png') });
  // Both the AS-aliased item AND the star-expanded columns
  expect(items).toContain('extra');
  expect(items).toContain('actor_id');
  expect(items).toContain('first_name');
});

test('MySQL tab completion — tables in sakila', async ({ page }) => {
  await page.goto('/');
  await page.waitForSelector('.sidebar', { timeout: 15_000 });

  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = await shellInput(page);
  await input.waitFor({ state: 'visible', timeout: 20_000 });
  await input.click();

  // FROM <Tab> once — eager fetch on session-open means the cache is
  // already populated by the time the user types.
  await input.fill('SELECT * FROM ');
  await input.press('Tab');
  await page.waitForTimeout(200);
  expect(await page.locator('.completion-popup').isVisible()).toBeTruthy();
  const items = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '07_mysql_from_tab.png') });
  // Sakila's tables include actor, address, category, city, country,
  // customer, film, film_actor, language, etc.
  expect(items).toContain('actor');
  expect(items).toContain('film');

  // Narrow with "fil"
  await input.type('fil', { delay: 30 });
  await page.waitForTimeout(150);
  const narrowed = await page.locator('.completion-popup-item').allTextContents();
  await page.screenshot({ path: path.join(SHOT_DIR, '08_mysql_narrowed_fil.png') });
  narrowed.forEach(t => expect(t.startsWith('fil')).toBeTruthy());
  expect(narrowed).toContain('film');
});
