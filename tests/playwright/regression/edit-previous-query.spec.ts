import { test, expect, type Page } from '@playwright/test';
import { spawnSync } from 'child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// `\e` on an empty buffer edits the previous query as the CLI keeps it:
// psql's `previous_buf` — the statement as it was sent, its `;` and all
// (`select 1 as a;`), or up to its `\g` line (`select 7 as g `), what
// psql's `\p` prints — and the mysql CLI's `old_buffer`, the statement
// alone. Edited, it comes back to the input and waits for Enter; with
// its `;`, Enter runs it. Unchanged, psql's is dropped.
//
// The editor is the test's: the page's `EditExternally` is answered
// here instead of going to noctua, which would open one on this
// machine; everything else goes through.

// Route the page's socket to noctua, answering each edit with
// `edit(content)`. What each edit was asked to open lands in the list.
async function editWith(page: Page, edit: (content: string) => string): Promise<string[]> {
  const opened: string[] = [];
  await page.routeWebSocket(/\/ws$/, ws => {
    const server = ws.connectToServer();
    ws.onMessage(message => {
      const sent = JSON.parse(String(message));
      if (sent.type !== 'EditExternally') {
        server.send(message);
        return;
      }
      opened.push(sent.data.content);
      ws.send(JSON.stringify({
        type: 'EditExternallyResult',
        data: { session_id: sent.data.session_id, tag: sent.data.tag, content: edit(sent.data.content) },
      }));
    });
  });
  return opened;
}

// What psql's `\p` printed under each `[name]` marker of `script`, its
// own trailing line break left off.
function psqlPrinted(script: string): Record<string, string> | null {
  const run = spawnSync('psql', ['-X', '-d', 'pagila', '-U', 'postgres', '-P', 'pager=off'], { input: script, encoding: 'utf8' });
  if (run.error) return null;
  const sections: Record<string, string> = {};
  let name = '';
  for (const line of run.stdout.split('\n')) {
    const marker = line.match(/^\[(\w+)\]$/);
    if (marker) { name = marker[1]; sections[name] = ''; continue; }
    if (name) sections[name] += `${line}\n`;
  }
  for (const key of Object.keys(sections)) sections[key] = sections[key].replace(/\n+$/, '');
  return sections;
}

test('psql: \\e on an empty buffer edits the previous query as psql keeps it', async ({ page }) => {
  const theirs = psqlPrinted([
    'select 1 as a;', '\\echo [semi]', '\\p',
    '\\echo [_]', 'select 2 as b;', '\\g', '\\echo [recall]', '\\p',
    '\\echo [_]', 'select 7 as g \\g', '\\echo [g]', '\\p',
  ].join('\n') + '\n');
  test.skip(theirs === null, 'psql not on PATH');
  const opened = await editWith(page, content => content.replace('1 as a', '2 as b'));
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const lastHeaders = () => out.locator('.shell-table').last().locator('th');

  // The statement with its `;`, as psql sent it; edited, it comes back
  // ready to run.
  await run('select 1 as a;');
  await expect(lastHeaders()).toHaveText(['a'], { timeout: 10_000 });
  await run('\\e');
  await expect.poll(() => opened.length, { timeout: 10_000 }).toBe(1);
  expect(opened[0]).toBe(theirs!.semi);
  expect(opened[0]).toBe('select 1 as a;');
  await expect(input).toHaveValue('select 2 as b;', { timeout: 10_000 });
  await input.press('Enter');
  await expect(lastHeaders()).toHaveText(['b'], { timeout: 10_000 });

  // A `\g` that runs it again sends the same buffer, `;` and all: it
  // stays the previous query. Back unchanged, it's dropped, as psql's
  // `discard_on_quit` drops it.
  await run('\\g');
  await expect(out.locator('.shell-table th', { hasText: /^b$/ })).toHaveCount(2, { timeout: 10_000 });
  await run('\\e');
  await expect.poll(() => opened.length, { timeout: 10_000 }).toBe(2);
  expect(opened[1]).toBe(theirs!.recall);
  expect(opened[1]).toBe('select 2 as b;');
  await expect(input).toBeEditable({ timeout: 10_000 });
  await expect(input).toHaveValue('');

  // After a `\g` with a buffer: the buffer it sent, the space before it
  // kept.
  await run('select 7 as g \\g');
  await expect(lastHeaders()).toHaveText(['g'], { timeout: 10_000 });
  await run('\\e');
  await expect.poll(() => opened.length, { timeout: 10_000 }).toBe(3);
  expect(opened[2]).toBe(theirs!.g);
  expect(opened[2]).toBe('select 7 as g ');
  // The edit over (the input read-only while it lasts), nothing came back.
  await expect(input).toBeEditable({ timeout: 10_000 });
  await expect(input).toHaveValue('');
});

test('mysql: \\e on an empty buffer edits the previous statement alone, as its old_buffer', async ({ page }) => {
  const opened = await editWith(page, content => content.replace('1 as a', '2 as b'));
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  await input.fill('select 1 as a;');
  await input.press('Enter');
  await expect(out.locator('.shell-table').last().locator('th')).toHaveText(['a'], { timeout: 10_000 });
  await input.fill('\\e');
  await input.press('Enter');
  await expect.poll(() => opened.length, { timeout: 10_000 }).toBe(1);
  expect(opened[0]).toBe('select 1 as a');
  // Back in the input, waiting for its terminator.
  await expect(input).toHaveValue('select 2 as b', { timeout: 10_000 });
  // A comment typed before it is a buffer, which it edits (`com_edit`).
  await input.fill('/* c */ \\e');
  await input.press('Enter');
  await expect.poll(() => opened.length, { timeout: 10_000 }).toBe(2);
  expect(opened[1].trim()).toBe('/* c */');
});

// What the mysql CLI's `\e` opened for each, typed in turn at its
// prompt under a pty (`expect`) — its editor a script that writes down
// each file it's handed. Null without the CLI or expect.
function mysqlEdits(lines: string[]): string[] | null {
  const dir = mkdtempSync(join(tmpdir(), 'pw-mysql-edit-'));
  const log = join(dir, 'opened');
  const editor = join(dir, 'editor.sh');
  writeFileSync(editor, `#!/bin/sh\ncat "$1" >> '${log}'\nprintf '\\0' >> '${log}'\n`, { mode: 0o755 });
  const tcl = (s: string) => s.replace(/\\/g, '\\\\').replace(/"/g, '\\"');
  const script = [
    'set timeout 10', 'log_user 0', `set env(EDITOR) "${editor}"`,
    'spawn mysql -h 127.0.0.1 -u root sakila', 'expect "mysql> "',
    // Its prompt at the end of what it printed (help text holds
    // `mysql> ` examples).
    ...lines.flatMap(line => [`send "${tcl(line)}\\r"`, 'expect -re {(mysql|    -)> $}']),
    'send "quit\\r"', 'expect eof',
  ].join('\n');
  try {
    const run = spawnSync('expect', ['-c', script], { encoding: 'utf8' });
    if (run.error || run.status !== 0) return null;
    // No log: the editor never ran (no CLI, or no login).
    return readFileSync(log, 'utf8').split('\0').slice(0, -1);
  } catch {
    return null;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

// The mysql CLI's `com_go` copies the statement to `old_buffer` before
// it says "No query specified", so after `;` alone — or `\g` alone —
// `\e` opens nothing — and so after `go` / `ego` alone, which run the
// empty buffer; its server help copies the buffer too: nothing
// after one typed alone, the statement after one typed as one. A help
// without a topic leaves it. Compared with the CLI's.
test('mysql: \\e after an empty statement or a help opens what the CLI does', async ({ page }) => {
  const LINES = [
    'select 1 as a;', '\\e', '\\c', ';', '\\e', '\\c', 'select 2 as b\\G', '\\g', '\\e', '\\c',
    'select 3 as c;', 'help select', '\\e', '\\c', 'help select;', '\\e', '\\c',
    'select 4 as d;', 'help', '\\e', '\\c', '\\h select', '\\e', '\\c',
    'select 5 as e;', 'go', '\\e', '\\c', 'select 6 as f;', 'EGO', '\\e', '\\c', 'select 7 as g;', 'go;', '\\e', '\\c',
  ];
  const theirs = mysqlEdits(LINES);
  test.skip(theirs === null, 'mysql CLI or expect not available');
  expect(theirs).toEqual(['select 1 as a', '', '', '', 'help select', 'select 4 as d', '', '', '', 'go']);
  const opened = await editWith(page, content => content);
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const blocks = out.locator('.shell-block');
  // `\c` is the CLI's way to clear what its `\e` gave back; here the
  // next line replaces it in the input.
  for (const line of LINES.filter(line => line !== '\\c')) {
    const edits = opened.length;
    const before = await blocks.count();
    await input.fill(line);
    await input.press('Enter');
    if (line === '\\e') {
      await expect.poll(() => opened.length, { timeout: 10_000 }).toBe(edits + 1);
      await expect(input).toBeEditable({ timeout: 10_000 });
    } else {
      await expect(blocks).toHaveCount(before + 1, { timeout: 10_000 });
      await expect(blocks.last().locator('.shell-table, .shell-vertical-block, .shell-entry-error, .shell-printed, .shell-meta-output').first()).toBeVisible({ timeout: 10_000 });
    }
  }
  expect(opened).toEqual(theirs);
});
