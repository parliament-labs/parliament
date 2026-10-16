import { test, expect, Page } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `AUTOCOMMIT` and `ON_ERROR_ROLLBACK` (`SendQuery`): with
// AUTOCOMMIT off a statement outside a transaction opens one (`BEGIN`
// first — not before a COMMIT, a VACUUM …); with ON_ERROR_ROLLBACK on (or
// `interactive`, as this shell is) each statement in a transaction runs
// inside a savepoint, rolled back to when it fails, so the transaction
// goes on. Each script's outcome is psql's for the same script (skipped
// without psql on PATH). Everything is temporary or rolled back.

function psql(script: string): string | null {
  try {
    return execFileSync('psql', ['-X', '-q', '-d', 'pagila', '-U', 'postgres', '-At'], { input: script, stdio: ['pipe', 'pipe', 'pipe'] }).toString();
  } catch {
    return null;
  }
}

async function openShell(page: Page) {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output').last();
  const lastCells = () => out.locator('.shell-table').last().locator('td');
  return { page, out, run, said, lastCells };
}

test('psql: AUTOCOMMIT off opens a transaction for a statement', async ({ page }) => {
  test.skip(psql('select 1;') === null, 'psql not on PATH');
  const { out, run, said, lastCells } = await openShell(page);
  const active = page.locator('.txn-badge.txn-active');
  const failed = page.locator('.txn-badge.txn-failed');

  // psql's starting value, and its hooks: \unset is off.
  await run('\\echo :AUTOCOMMIT :ON_ERROR_ROLLBACK');
  await expect(said).toHaveText('on off', { timeout: 10_000 });
  await run('\\set AUTOCOMMIT x');
  await expect(out.locator('.shell-meta-output, .shell-entry-error').last())
    .toHaveText('unrecognized value "x" for "AUTOCOMMIT": Boolean expected', { timeout: 10_000 });

  await run('\\set AUTOCOMMIT off');
  await run('create temp table ac (a int);');
  await expect(active).toBeVisible({ timeout: 10_000 });
  await run('insert into ac values (1);');
  await expect(out.locator('.shell-entry-info', { hasText: 'INSERT 0 1' })).toBeVisible({ timeout: 10_000 });
  // Rolled back: the table too.
  await run('rollback;');
  await expect(active).toHaveCount(0, { timeout: 10_000 });
  await run('select count(*) from ac;');
  await expect(out.locator('.shell-entry-error').last()).toContainText('relation "ac" does not exist', { timeout: 10_000 });
  // That statement opened a transaction too, which its error failed.
  await expect(failed).toBeVisible();
  await run('rollback;');
  await expect(failed).toHaveCount(0, { timeout: 10_000 });
  // No BEGIN before a command that ends one: psql's warning.
  await run('commit;');
  await expect(out).toContainText('there is no transaction in progress', { timeout: 10_000 });
  await expect(active).toHaveCount(0);

  // psql: the same script, the same end.
  const theirs = psql('\\set AUTOCOMMIT off\ncreate temp table ac (a int);\ninsert into ac values (1);\nrollback;\nselect count(*) from ac;\n');
  expect(theirs).toBe('');

  // \unset is off; on again: no transaction.
  await run('\\unset AUTOCOMMIT');
  await run('\\echo :AUTOCOMMIT');
  await expect(said).toHaveText('off', { timeout: 10_000 });
  await run('\\set AUTOCOMMIT on');
  await run('create temp table ac2 (a int);');
  await expect(out.locator('.shell-entry-info', { hasText: 'CREATE TABLE' }).last()).toBeVisible({ timeout: 10_000 });
  await run('select 1 as one;');
  await expect(lastCells()).toHaveText(['1'], { timeout: 10_000 });
  await expect(active).toHaveCount(0);
});

test('psql: ON_ERROR_ROLLBACK rolls a failed statement back to its savepoint', async ({ page }) => {
  test.skip(psql('select 1;') === null, 'psql not on PATH');
  const { run, lastCells, out } = await openShell(page);
  const active = page.locator('.txn-badge.txn-active');
  const failed = page.locator('.txn-badge.txn-failed');
  const script = (setting: string) => [
    `\\set ON_ERROR_ROLLBACK ${setting}`,
    'begin;',
    'create temp table r (a int);',
    'insert into r values (1);',
    'select 1/0;',
    'insert into r values (2);',
    'commit;',
    'select count(*) as n from r;',
  ];

  // As this shell is interactive, `interactive` is on.
  for (const cmd of script('interactive').slice(0, 5)) await run(cmd);
  await expect(out.locator('.shell-entry-error').last()).toContainText('division by zero', { timeout: 10_000 });
  // The error didn't fail the transaction.
  await expect(active).toBeVisible();
  await expect(failed).toHaveCount(0);
  for (const cmd of script('interactive').slice(5)) await run(cmd);
  await expect(lastCells()).toHaveText(['2'], { timeout: 10_000 });
  await expect(active).toHaveCount(0);
  // psql, run from a script — not interactive — with it on: the same count.
  expect(psql(script('on').join('\n') + '\n')).toBe('2\n');

  // AUTOCOMMIT off and ON_ERROR_ROLLBACK on together: BEGIN, then the
  // savepoint, before each statement outside a transaction.
  await run('\\set AUTOCOMMIT off');
  await run('create temp table b (a int);');
  await run('select 1/0;');
  await expect(out.locator('.shell-entry-error').last()).toContainText('division by zero', { timeout: 10_000 });
  await run('insert into b values (1);');
  await run('commit;');
  await run('select count(*) as n from b;');
  await expect(lastCells()).toHaveText(['1'], { timeout: 10_000 });
  await run('rollback;');
  expect(psql('\\set AUTOCOMMIT off\n\\set ON_ERROR_ROLLBACK on\ncreate temp table b (a int);\nselect 1/0;\ninsert into b values (1);\ncommit;\nselect count(*) from b;\n'))
    .toBe('1\n');
});
