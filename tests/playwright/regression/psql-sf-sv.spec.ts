import { test, expect } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\sf[+]` / `\sv[+]` (`exec_command_sf_sv`): the definition as
// psql prints it — no header, no row count — `+` numbering its lines (a
// function's from its body); refusals in psql's words. Compared with
// psql's own output (skipped without psql on PATH). Byte-for-byte parity,
// a view with reloptions and a check option included, is pinned natively:
// `cargo test -p tyto -- --ignored sf_and_sv_match_psql`.

function psql(cmd: string): string | null {
  try {
    return execFileSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', '-c', cmd]).toString();
  } catch {
    return null;
  }
}

test('psql: \\sf and \\sv print definitions as psql does', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const printed = out.locator('.shell-printed');
  const errors = out.locator('.shell-entry-error');

  for (const cmd of ['\\sf last_updated', '\\sf+ last_updated', '\\sv actor_info', '\\sv+ actor_info']) {
    const before = await printed.count();
    await run(cmd);
    await expect(printed).toHaveCount(before + 1, { timeout: 10_000 });
    const ours = (await printed.last().textContent())!;
    expect(ours).toContain('CREATE OR REPLACE');
    const theirs = psql(cmd);
    if (theirs !== null) expect(ours.trimEnd(), cmd).toBe(theirs.trimEnd());
  }
  // No result table: no header, no row count.
  await expect(out.locator('.shell-table')).toHaveCount(0);

  // Refusals.
  await run('\\sf nosuch');
  await expect(errors.last()).toHaveText('ERROR:  function "nosuch" does not exist', { timeout: 10_000 });
  await run('\\sv film');
  await expect(errors.last()).toHaveText('"public.film" is not a view', { timeout: 10_000 });
  await run('\\sf');
  await expect(errors.last()).toHaveText('function name is required', { timeout: 10_000 });
});
