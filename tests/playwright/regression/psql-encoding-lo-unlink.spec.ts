import { test, expect, Page } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `\encoding` (`exec_command_encoding`): the client encoding —
// UTF8, the encoding noctua's sessions speak — without a query; a name
// is read as the server reads one, UTF8's said nothing, an unknown one
// refused in psql's words. psql's `\lo_unlink` (`do_lo_unlink`): `lo_unlink
// N` once removed, in a transaction of its own outside one (BEGIN, then
// COMMIT, or ROLLBACK when the call fails — what ECHO_HIDDEN shows, the
// call itself psql's fast path), inside the session's otherwise, refused
// in a failed one. Each compared with psql's own output for the same
// input (skipped without psql on PATH); the large object lives in a
// transaction that's rolled back.

// psql's output for `input` — stdout, and stderr without the
// `psql:<stdin>:N: ` (and `error: `) its script input adds — quiet, but
// for what `\set QUIET` hides too (`lo_unlink N`).
function psql(input: string, quiet = true): { out: string; err: string } | null {
  const args = ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off', ...(quiet ? ['-q'] : [])];
  const run = spawnSync('psql', args, { input, encoding: 'utf8' });
  if (run.error) return null;
  return { out: run.stdout, err: run.stderr.replace(/^psql:<stdin>:\d+: (error: )?/gm, '') };
}

async function openShell(page: Page) {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  return { out, run };
}

const lines = (text: string) => text.split('\n').map(l => l.trimEnd()).join('\n').trim();

test('psql: \\encoding is the client encoding, as psql says it', async ({ page }) => {
  const shown = psql('\\encoding\n\\echo :ENCODING\n\\encoding utf-8\n\\encoding\n');
  test.skip(shown === null, 'psql not on PATH');
  const bogus = psql('\\encoding bogus\n')!;
  const { out, run } = await openShell(page);
  const said = out.locator('.shell-meta-output');
  const lastBlock = () => out.locator('.shell-block').last();

  // Shown as psql shows it, `:ENCODING` too — and no query: ECHO_HIDDEN
  // has nothing to show.
  await run('\\set ECHO_HIDDEN on');
  await run('\\encoding');
  await expect(said.last()).toHaveText('UTF8', { timeout: 10_000 });
  await expect(lastBlock().locator('.shell-printed')).toHaveCount(0);
  await run('\\set ECHO_HIDDEN off');
  await run('\\echo :ENCODING');
  await expect(said.last()).toHaveText('UTF8', { timeout: 10_000 });
  // UTF8 by another name: nothing said, as psql's.
  await run('\\encoding utf-8');
  await expect(out.locator('.shell-block', { hasText: '\\encoding utf-8' })).toBeVisible({ timeout: 10_000 });
  await expect(out.locator('.shell-block', { hasText: '\\encoding utf-8' }).locator('.shell-meta-output, .shell-entry-error')).toHaveCount(0);
  await run('\\encoding');
  await expect(said).toHaveCount(3, { timeout: 10_000 });
  expect((await said.allInnerTexts()).join('\n')).toBe(shown!.out.trimEnd());

  // An unknown name: psql's words.
  await run('\\encoding bogus');
  await expect(out.locator('.shell-entry-error').last()).toHaveText(bogus.err.trim(), { timeout: 10_000 });
  // One this shell's sessions can't switch to: said so.
  await run('\\encoding latin1');
  await expect(out.locator('.shell-entry-error').last()).toContainText('other than UTF8', { timeout: 10_000 });
});

test('psql: \\lo_unlink removes a large object as psql does', async ({ page }) => {
  // psql first: its large object would wait on the shell's, uncommitted.
  const missing = psql('\\lo_unlink\n');
  test.skip(missing === null, 'psql not on PATH');
  const nosuch = psql('\\lo_unlink 999999\n\\lo_unlink abc\n')!;
  const removed = psql('begin;\nselect lo_create(424242) is not null as made \\gset\n\\lo_unlink 424242\n\\lo_unlink 424242\n\\lo_unlink 1\nrollback;\n', false)!;
  removed.out = removed.out.split('\n').filter(l => l !== 'BEGIN' && l !== 'ROLLBACK').join('\n');
  const echoed = psql('\\set ECHO_HIDDEN on\n\\lo_unlink 999999\n')!;
  const blocks = echoed.out.split('/******** QUERY *********/\n').slice(1)
    .map(block => '/******** QUERY *********/\n' + block.slice(0, block.indexOf('/************************/') + '/************************/'.length));
  expect(blocks.map(b => b.split('\n')[1])).toEqual(['BEGIN', 'ROLLBACK']);

  const { out, run } = await openShell(page);
  const error = out.locator('.shell-entry-error');
  const said = out.locator('.shell-meta-output');
  const lastBlock = () => out.locator('.shell-block').last();
  const active = page.locator('.txn-badge.txn-active');
  const failed = page.locator('.txn-badge.txn-failed');

  await run('\\lo_unlink');
  await expect(error.last()).toHaveText(missing!.err.trim(), { timeout: 10_000 });

  // Outside a transaction: its own, rolled back when the call fails.
  await run('\\lo_unlink 999999');
  await run('\\lo_unlink abc');
  await expect(error).toHaveCount(3, { timeout: 10_000 });
  expect(lines((await error.allInnerTexts()).slice(1).join('\n'))).toBe(lines(nosuch.err));
  await expect(active).toHaveCount(0);
  await expect(failed).toHaveCount(0);
  // The call's failure isn't kept for \errverbose: psql's fast path
  // makes no result.
  await run('\\errverbose');
  await expect(said.last()).toHaveText('There is no previous error.', { timeout: 10_000 });

  // ECHO_HIDDEN: psql's BEGIN and ROLLBACK, not the call.
  await run('\\set ECHO_HIDDEN on');
  await run('\\lo_unlink 999999');
  await expect(lastBlock().locator('.shell-entry-error')).toHaveCount(1, { timeout: 10_000 });
  const echo = lastBlock().locator('.shell-printed');
  await expect(echo).toHaveCount(2);
  expect([(await echo.nth(0).textContent())!.trimEnd(), (await echo.nth(1).textContent())!.trimEnd()]).toEqual(blocks);
  // noexec: BEGIN shown, nothing run.
  await run('\\set ECHO_HIDDEN noexec');
  await run('\\lo_unlink 999999');
  await expect(lastBlock().locator('.shell-printed')).toHaveCount(1, { timeout: 10_000 });
  await expect(lastBlock().locator('.shell-printed')).toContainText('BEGIN');
  await expect(lastBlock().locator('.shell-entry-error')).toHaveCount(0);
  await run('\\set ECHO_HIDDEN off');
  await expect(active).toHaveCount(0);

  // Inside a transaction: removed there; again, the server's error —
  // which fails it, as psql's call does; then psql's refusal.
  await run('begin;');
  await run('select lo_create(424242) is not null as made;');
  await expect(out.locator('.shell-table').last().locator('td')).toHaveText(['t'], { timeout: 10_000 });
  await run('\\lo_unlink 424242');
  await expect(said.last()).toHaveText('lo_unlink 424242', { timeout: 10_000 });
  await expect(active).toBeVisible();
  const before = await error.count();
  await run('\\lo_unlink 424242');
  await run('\\lo_unlink 1');
  await expect(error).toHaveCount(before + 2, { timeout: 10_000 });
  await expect(failed).toBeVisible();
  const ours = ['lo_unlink 424242', ...(await error.allInnerTexts()).slice(before)].join('\n');
  expect(lines(ours)).toBe(lines(removed.out + removed.err));
  await run('rollback;');
  await expect(failed).toHaveCount(0, { timeout: 10_000 });

  // AUTOCOMMIT off: its own transaction stays open — failed here, as
  // psql leaves it.
  await run('\\set AUTOCOMMIT off');
  await run('\\lo_unlink 999999');
  await expect(failed).toBeVisible({ timeout: 10_000 });
  await run('rollback;');
  await expect(failed).toHaveCount(0, { timeout: 10_000 });
  await run('\\set AUTOCOMMIT on');

  // Under `\pset format html`, psql's paragraph.
  await run('\\pset format html');
  await run('begin;');
  await run('select lo_create(424242) is not null as made;');
  await run('\\lo_unlink 424242');
  await expect(said.last()).toHaveText('<p>lo_unlink 424242</p>', { timeout: 10_000 });
  await run('rollback;');
  await expect(active).toHaveCount(0, { timeout: 10_000 });
});
