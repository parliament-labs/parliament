import { test, expect } from '@playwright/test';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's list commands print under psql's title (describe.c's
// `myopt.title`): `List of tables`, `List of databases` … — over the
// expanded records too, in place of `\pset title`, and not under `\t`
// (psql's printer leaves titles out there) — each result as it printed. Nothing found with a pattern
// prints psql's message alone.

test('psql: lists print under psql\'s titles', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const titles = out.locator('.shell-table-title');

  await run('\\dt actor');
  await expect(titles.last()).toHaveText('List of tables', { timeout: 10_000 });
  await run('\\l');
  await expect(titles.last()).toHaveText('List of databases', { timeout: 10_000 });
  await run('\\d');
  await expect(titles.last()).toHaveText('List of relations', { timeout: 10_000 });
  // Titles of their own: privileges, non-default settings, partitions.
  await run('\\dp actor');
  await expect(titles.last()).toHaveText('Access privileges', { timeout: 10_000 });
  await run('\\dconfig');
  await expect(titles.last()).toHaveText('List of non-default configuration parameters', { timeout: 10_000 });
  await run('\\dPt');
  await expect(titles.last()).toHaveText('List of partitioned tables', { timeout: 10_000 });

  // Expanded: the title over the records, as psql prints them.
  await run('\\dtx actor');
  await expect(out.locator('.shell-vertical-block').last().locator('.shell-vertical-text'))
    .toHaveText(/^List of tables\n-\[ RECORD 1 \]/, { timeout: 10_000 });

  // A list's title stands in for `\pset title`; a query keeps that.
  await run('\\pset title Mine');
  await run('\\dx plpgsql');
  await expect(titles.last()).toHaveText('List of installed extensions', { timeout: 10_000 });
  await run('select 1 as one;');
  await expect(titles.last()).toHaveText('Mine', { timeout: 10_000 });
  await run('\\pset title');

  // Nothing found: psql's message, no title.
  const before = await titles.count();
  await run('\\dt nosuch');
  await expect(out.locator('.shell-meta-output').last()).toHaveText('Did not find any tables named "nosuch".', { timeout: 10_000 });
  await expect(titles).toHaveCount(before);

  // `\t`: no title over what prints now, as psql's printer leaves it
  // out — the earlier results keep theirs, as they printed.
  const shown = await titles.count();
  await run('\\t');
  await run('\\dn public');
  await expect(out.locator('.shell-table').last()).toContainText('public', { timeout: 10_000 });
  await expect(titles).toHaveCount(shown);
  await run('\\t');
});
