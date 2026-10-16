import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// `\pset` reads and answers as psql's `do_pset` does: option names exactly
// as psql spells them, a Boolean set silently and toggled aloud, errors
// naming the option as typed, the pager settings kept and said, a title
// that's unset apart from an empty one, and bare `\pset` listing every
// setting. Each command's reply is compared with what psql says after
// the same commands (skipped without psql on PATH).

const CASES = [
  '\\pset FORMAT',
  '\\pset format=csv',
  '\\pset tuples_only x',
  '\\pset t o',
  '\\pset tuples_only on',
  '\\pset t',
  '\\t x',
  '\\pset footer off',
  '\\pset footer',
  '\\pset footer x',
  '\\pset x x',
  '\\x x',
  '\\pset vertical off',
  '\\pset numericlocale x',
  '\\pset numericlocale off',
  '\\pset pager',
  '\\pset pager ALWAYS',
  '\\pset pager x',
  '\\pset pager_min_lines 1',
  '\\pset pager_min_lines 0x10',
  '\\pset pager_min_lines 5x',
  '\\pset xheader_width',
  '\\pset xheader_width x',
  '\\pset title',
  "\\pset title ''",
  '\\C x',
  '\\C',
  '\\pset',
];

// What psql says to `cmd` after `before` — its output and its errors,
// read apart so their order can't mix, the errors `before` caused left
// out. Null without psql.
function psqlSays(before: string[], cmd: string): string | null {
  const psql = (commands: string[]) => {
    const args = ['-X', '-d', 'pagila', '-U', 'postgres'];
    for (const c of commands) args.push('-c', c);
    return spawnSync('psql', args, { encoding: 'utf8' });
  };
  const setup = psql(before);
  const run = psql([...before, '\\echo ---said---', cmd]);
  if (run.error || run.stdout === null) return null;
  const said = run.stdout.split('---said---\n')[1] ?? '';
  return lines(said + run.stderr.slice(setup.stderr.length));
}

function lines(text: string): string {
  return text.split('\n').map(l => l.trimEnd()).join('\n').trim();
}

test('psql: \\pset reads and answers as psql does', async ({ page }) => {
  test.skip(psqlSays([], '\\echo ok') === null, 'psql not on PATH');
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const replies = out.locator('.shell-meta-output, .shell-entry-error');
  const texts = () => replies.evaluateAll(els => els.map(e => (e as HTMLElement).innerText));

  for (const [i, cmd] of CASES.entries()) {
    const before = await replies.count();
    await run(cmd);
    // `\echo` after it says when it's done — it may say nothing.
    await run(`\\echo ---${i}---`);
    await expect(replies.last()).toHaveText(`---${i}---`, { timeout: 10_000 });
    const ours = lines((await texts()).slice(before, -1).join('\n'));
    expect(ours, cmd).toBe(psqlSays(CASES.slice(0, i), cmd));
  }
});
