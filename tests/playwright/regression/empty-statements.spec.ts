import { test, expect, type Page } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// A terminator that ends nothing, as each CLI answers it. The mysql CLI
// says `ERROR: ` / `No query specified` for every empty statement — `;`
// alone, `\G` alone, the second `;` of `select 1;;` — in order with the
// rest of the line. psql sends it: the server answers with nothing.

async function openShell(page: Page, connection: string) {
  await openApp(page);
  await clickConnection(page, connection);
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  return input;
}

test('mysql: No query specified, for each empty statement in its place', async ({ page }) => {
  const input = await openShell(page, 'Local MySQL');
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const errors = out.locator('.shell-entry-error', { hasText: 'No query specified' });

  await run(';');
  await expect(errors).toHaveCount(1, { timeout: 10_000 });
  await expect(errors.first()).toHaveText('ERROR: \nNo query specified');
  await expect(input).toHaveValue('');

  await run('\\G');
  await expect(errors).toHaveCount(2, { timeout: 10_000 });

  // In order: the result, the error, the next result.
  await run('select 1 as a; ; select 2 as b;');
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toBeVisible({ timeout: 10_000 });
  await expect(errors).toHaveCount(3);
  const order = await out.locator('.shell-table th, .shell-entry-error').allInnerTexts();
  expect(order.slice(-3)).toEqual(['a', 'ERROR: \nNo query specified', 'b']);

  await run('select 3 as c\\G;');
  await expect(errors).toHaveCount(4, { timeout: 10_000 });
  await expect(out.locator('.shell-vertical-block').last()).toContainText('c:');

  // A DELIMITER string alone ends nothing too; `delimiter ;` goes back
  // (silently, both ways, as mysql sets it).
  await run('delimiter //');
  await run('//');
  await expect(errors).toHaveCount(5, { timeout: 10_000 });
  await run('delimiter ;');
  await run('select 4 as d;');
  await expect(out.locator('.shell-table th', { hasText: /^d$/ })).toBeVisible({ timeout: 10_000 });
  await expect(out).not.toContainText('Delimiter set to');
});

// What psql printed under each `[name]` marker of `script` — its errors
// and warnings in place — each aligned table as `<table>`, times as
// `N ms`.
function psqlSections(script: string): Record<string, string[]> | null {
  const run = spawnSync('sh', ['-c', 'psql -X -d pagila -U postgres -P pager=off 2>&1'], { input: script, encoding: 'utf8' });
  if (run.error || !run.stdout.includes('[')) return null;
  const sections: Record<string, string[]> = {};
  let name = '';
  const lines = run.stdout.replace(/^psql:<stdin>:\d+: /gm, '').split('\n');
  for (let i = 0; i < lines.length; i++) {
    const marker = lines[i].match(/^\[(\w+)\]$/);
    if (marker) { name = marker[1]; sections[name] = []; continue; }
    if (!name) continue;
    if (/^-+(\+-+)*$/.test(lines[i + 1] ?? '')) {
      // A table: its header, rule, rows and count.
      sections[name].push('<table>');
      while (i < lines.length && !/^\(\d+ rows?\)$/.test(lines[i])) i++;
      continue;
    }
    if (lines[i] !== '') sections[name].push(lines[i].replace(/\d+\.\d{3} ms/, 'N ms'));
  }
  return sections;
}

// Each case: what runs before it, and the inputs whose output is
// compared — typed in one shell, in turn, as psql reads the script
// (what runs before a case prints under a marker of its own, `[_]`).
const PSQL_CASES: [string, string[], string[]][] = [
  ['noquery', ['\\timing on'], ['\\g', '\\gdesc', '\\watch 1']],
  ['semi', [], [';']],
  ['double', [], ['select 1 as a;;']],
  ['block', [], ['/* c */;']],
  ['line', [], ['-- c\n;']],
  ['blockg', [], ['/* c */ \\g']],
  ['lineg', ['select 9 as n;'], ['-- c\n\\g']],
  ['prev', ['\\timing off', 'select 7 as z;'], [';', '\\p', '\\g']],
  ['block_prev', [], ['/* k */;', '\\p']],
  ['rowcount', ['select 1 from generate_series(1,3);'], ['\\echo :ROW_COUNT', ';', '\\echo :ROW_COUNT']],
  // `\\watch` after a `;` watches it: each run's time alone.
  ['watch', ['\\timing on'], [';', '\\watch c=2 i=0.2', '\\timing off']],
  ['autocommit', ['\\set AUTOCOMMIT off'], [';', 'rollback;']],
];

// psql sends what its buffer holds — even nothing: a `;` alone, the
// second of `;;`, a statement of comments only, a `/* */` comment before
// `\g`, and the `\g` family with no query at all. The server answers an
// empty query with nothing: a `Time:` under `\timing`, `:ROW_COUNT` 0, a
// `BEGIN` before it under AUTOCOMMIT off — and it's the previous query
// (`\p` prints `;`). Compared with psql's output for the same input
// (skipped without psql on PATH).
test('psql: an empty statement goes to the server, as psql sends it', async ({ page }) => {
  const theirs = psqlSections(PSQL_CASES
    .map(([name, before, inputs]) => ['\\echo [_]', ...before, `\\echo [${name}]`, ...inputs].join('\n')).join('\n') + '\n');
  test.skip(theirs === null, 'psql not on PATH');
  const input = await openShell(page, 'Local Postgres');
  const out = visibleShellOutput(page);
  const blocks = out.locator('.shell-block');
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  // Once what was typed has run: a marker of its own each time, echoed
  // after it.
  let settles = 0;
  const settled = async () => {
    const marker = `--settled ${++settles}--`;
    await run(`\\echo ${marker}`);
    await expect(out.locator('.shell-meta-line').last()).toHaveText(marker, { timeout: 10_000 });
  };
  // What the blocks from `from` on printed, before the settling `\echo`.
  const printed = (from: number) => blocks.evaluateAll((nodes, from) => nodes.slice(from, -1).flatMap(block =>
    [...block.querySelectorAll('.shell-table, .shell-timing, .shell-entry-info, .shell-meta-line, .shell-entry-error, .shell-notice')]
      .flatMap(n => n.classList.contains('shell-table') ? ['<table>'] : (n as HTMLElement).innerText.split('\n'))
      .map(line => line.replace(/\d+\.\d{3} ms/, 'N ms'))), from);

  for (const [name, before, inputs] of PSQL_CASES) {
    for (const cmd of before) await run(cmd);
    await settled();
    const from = await blocks.count();
    for (const cmd of inputs) await run(cmd);
    await settled();
    expect(await printed(from), name).toEqual(theirs![name]);
  }
  // \gdesc with no columns: psql times only the query naming the types.
  expect(theirs!.noquery).toContain('The command has no result, or the result has no columns.');
  await run('\\set AUTOCOMMIT on');
  // Sent, a `;` is kept in the history, as psql's readline keeps it.
  await run(';');
  await input.press('ArrowUp');
  await expect(input).toHaveValue(';');
});
