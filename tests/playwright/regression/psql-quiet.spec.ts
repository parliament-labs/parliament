import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's `QUIET` (`pset.quiet`): command tags (`PrintQueryStatus` — the
// time still printed), what `\pset`, `\x`, `\t`, `\a` … and `\timing`
// say of a setting, `\r`'s and `\c`'s words, `lo_unlink N`, and
// describe.c's "Did not find …" left unsaid — `\dt` and `\drds` print
// their empty table instead. Set as psql sets it: off to start, a
// Boolean, `\unset` off. Compared with psql's own output where it
// prints anything (skipped without psql on PATH); everything is
// temporary or rolled back.

function psql(input: string): { out: string; err: string } | null {
  const run = spawnSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off'], { input, encoding: 'utf8' });
  if (run.error) return null;
  return { out: run.stdout, err: run.stderr.replace(/^psql:<stdin>:\d+: (error: )?/gm, '') };
}

test('psql: QUIET leaves unsaid what psql leaves unsaid', async ({ page }) => {
  const refused = psql('\\set QUIET x\n');
  test.skip(refused === null, 'psql not on PATH');
  const html = psql('\\pset format html\nbegin;\nrollback;\n')!;
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const said = out.locator('.shell-meta-output');
  const info = out.locator('.shell-entry-info');
  const blockOf = (cmd: string) => out.locator('.shell-block').filter({ has: page.locator('.shell-entry-query', { hasText: cmd }) }).last();
  const settled = async () => { await run('\\echo settled'); await expect(said.last()).toHaveText('settled', { timeout: 10_000 }); };

  // psql's starting value, and its hook.
  await run('\\echo :QUIET');
  await expect(said.last()).toHaveText('off', { timeout: 10_000 });
  await run('\\set QUIET x');
  await expect(out.locator('.shell-meta-output, .shell-entry-error').last()).toHaveText(refused!.err.trim(), { timeout: 10_000 });

  await run('\\set QUIET on');
  // Command tags: none — but the time, with \timing.
  await run('begin;');
  await run('create temp table q (a int);');
  await run('insert into q values (1);');
  await run('\\timing on');
  await run('update q set a = 2;');
  await expect(info).toHaveCount(1, { timeout: 10_000 });
  await expect(info.last()).toHaveText(/^Time: \d+\.\d{3} ms$/);
  await run('\\timing off');
  await settled();
  const quietSaid = await said.count();

  // What psql says of a setting: nothing.
  for (const cmd of ['\\x', '\\x', '\\a', '\\a', '\\H', '\\H', '\\t', '\\t', "\\C 'T'", '\\C', "\\f ';'", '\\f |',
                     '\\T border', '\\T', '\\pset null', '\\pset footer', '\\pset footer', '\\pset format', '\\pset linestyle',
                     '\\r']) {
    await run(cmd);
  }
  // Nothing found: nothing said — or, for \dt and \drds, the empty table.
  await run('\\d nosuch');
  await run('\\dx+ nosuch');
  await run('\\dt nosuch');
  await expect(out.locator('.shell-table-title').last()).toHaveText('List of tables', { timeout: 10_000 });
  await expect(out.locator('.shell-table').last().locator('th')).toHaveText(['Schema', 'Name', 'Type', 'Owner']);
  await expect(out.locator('.shell-stat').last()).toContainText('(0 rows)');
  await run('\\drds nosuch');
  await expect(out.locator('.shell-table-title').last()).toHaveText('List of settings', { timeout: 10_000 });
  // lo_unlink N: nothing.
  await run('select lo_create(424244) is not null as made;');
  await run('\\lo_unlink 424244');
  await settled();
  // Only the \echo said anything; no errors.
  await expect(said).toHaveCount(quietSaid + 1);
  await expect(out.locator('.shell-entry-error')).toHaveCount(0);
  await expect(blockOf('\\lo_unlink 424244').locator('.shell-meta-output, .shell-entry-error')).toHaveCount(0);
  // Bare \pset lists every setting all the same.
  await run('\\pset');
  await expect(said.last()).toContainText('xheader_width', { timeout: 10_000 });
  await run('rollback;');
  await settled();
  // \c: connected, nothing said.
  const before = await said.count();
  await run('\\c pagila');
  await settled();
  await expect(said).toHaveCount(before + 1);

  // \unset: off, and psql's words again — tags in an html paragraph.
  await run('\\unset QUIET');
  await run('\\echo :QUIET');
  await expect(said.last()).toHaveText('off', { timeout: 10_000 });
  await run('\\pset format html');
  await run('begin;');
  await run('rollback;');
  await expect(info.last()).toHaveText('<p>ROLLBACK</p>', { timeout: 10_000 });
  const ours = [await said.last().innerText(), ...(await info.allInnerTexts()).slice(-2)].join('\n');
  expect(ours).toBe(html.out.trimEnd());
  await run('\\pset format aligned');
  await expect(said.last()).toHaveText('Output format is aligned.', { timeout: 10_000 });
});
