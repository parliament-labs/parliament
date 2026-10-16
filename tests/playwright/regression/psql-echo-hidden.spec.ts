import { test, expect } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `ECHO_HIDDEN` (`PSQLexec`): a meta-command's queries, each shown
// between psql's lines before it runs — a list's word for word as psql's
// (`psql -E`); `noexec` shows them and runs nothing. Set as psql sets
// it: off to start, `\unset` off, a bad value refused in psql's words
// (skipped without psql on PATH).

function psql(args: string[]): string | null {
  try {
    return execFileSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', ...args]).toString();
  } catch {
    return null;
  }
}

test('psql: ECHO_HIDDEN shows the queries a meta-command sends', async ({ page }) => {
  const echoed = psql(['-E', '-c', '\\dt actor']);
  test.skip(echoed === null, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output').last();
  const printed = out.locator('.shell-printed');
  const tables = out.locator('.shell-table');

  await run('\\echo :ECHO_HIDDEN');
  await expect(said).toHaveText('off', { timeout: 10_000 });
  await run('\\set ECHO_HIDDEN x');
  await expect(out.locator('.shell-meta-output').last())
    .toHaveText(/^unrecognized value "x" for "ECHO_HIDDEN"\s*Available values are: on, off, noexec\.$/, { timeout: 10_000 });

  // On: the list's query, as psql prints it, then the list.
  await run('\\set ECHO_HIDDEN on');
  await run('\\dt actor');
  await expect(tables).toHaveCount(1, { timeout: 10_000 });
  const block = echoed!.slice(0, echoed!.indexOf('/************************/') + '/************************/'.length);
  expect((await printed.last().textContent())!.trimEnd()).toBe(block);
  await expect(tables.last().locator('td').first()).toHaveText('public');

  // A staged command: each batch's queries, then what it prints.
  const before = await printed.count();
  await run('\\d actor');
  await expect(out.locator('.shell-printed', { hasText: 'Table "public.actor"' })).toBeVisible({ timeout: 10_000 });
  expect(await printed.count()).toBeGreaterThan(before + 2);
  await expect(printed.nth(before)).toContainText('/******** QUERY *********/');

  // noexec: shown, not run — nothing more.
  await run('\\set ECHO_HIDDEN noexec');
  const shown = await printed.count();
  await run('\\dn');
  await expect(printed).toHaveCount(shown + 1, { timeout: 10_000 });
  await expect(printed.last()).toContainText('FROM pg_catalog.pg_namespace n');
  await run('\\echo done');
  await expect(said).toHaveText('done', { timeout: 10_000 });
  await expect(tables).toHaveCount(1);

  // \unset: off.
  await run('\\unset ECHO_HIDDEN');
  await run('\\echo :ECHO_HIDDEN');
  await expect(said).toHaveText('off', { timeout: 10_000 });
  await run('\\dn');
  await expect(tables).toHaveCount(2, { timeout: 10_000 });
  await expect(printed).toHaveCount(shown + 1);
});
