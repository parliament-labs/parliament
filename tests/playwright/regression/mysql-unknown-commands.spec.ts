import { test, expect } from '@playwright/test';
import { spawnSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql's \if blocks aren't the mysql CLI's. It reads `\` and one
// character as a command and has none for `i`: `\if 1` gets "Unknown
// command '\i'." as the line is read, and the text stays in the
// statement, which the server gets (and refuses). psql's blocks work as
// before (psql-conditional.spec.ts).

test("mysql: \\if is the CLI's unknown \\i, left in the statement", async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const unknown = out.locator('.shell-entry-error', { hasText: 'Unknown command' });
  const syntax = out.locator('.shell-entry-error', { hasText: 'ERROR 1064 (42000)' });

  // Said at once; the statement waits for more.
  await run('\\if 1');
  await expect(unknown).toHaveCount(1, { timeout: 10_000 });
  await expect(unknown.first()).toHaveText("ERROR: \nUnknown command '\\i'.");
  await expect(input).toHaveValue('\\if 1\n');

  // The next line ends it, and the server gets `\if 1` with it. Nothing
  // more is said about the line read before.
  await run('\\if 1\nselect 1 as a;');
  await expect(syntax).toHaveCount(1, { timeout: 10_000 });
  await expect(syntax).toContainText("near '\\if 1");
  await expect(unknown).toHaveCount(1);
  await expect(out.locator('.shell-table th', { hasText: /^a$/ })).toHaveCount(0);

  // Each unknown command on the line, said under the line's block and
  // before the server's answer.
  await run('select 4 as b \\i \\j;');
  await expect(syntax).toHaveCount(2, { timeout: 10_000 });
  await expect(unknown).toHaveCount(3);
  const text = await out.innerText();
  const block = text.lastIndexOf('select 4 as b');
  const said = text.indexOf("Unknown command '\\j'.", block);
  expect(block).toBeGreaterThan(-1);
  expect(said).toBeGreaterThan(block);
  expect(text.indexOf('ERROR 1064', block)).toBeGreaterThan(said);

  // Its own commands still run past one: `\c` clears the statement.
  await run('\\if 1 \\c');
  await expect(unknown).toHaveCount(4, { timeout: 10_000 });
  await expect(input).toHaveValue('');

  // Quoted, a backslash is the string's.
  await run('select "a\\ib" as s;');
  await expect(out.locator('.shell-table th', { hasText: /^s$/ })).toBeVisible({ timeout: 10_000 });
  await expect(unknown).toHaveCount(4);

  // Outside quotes a backslash takes the character after it: `\'`
  // opens no string, so the `;` ends the statement.
  await run("select 5 \\'c;");
  await expect(syntax).toHaveCount(3, { timeout: 10_000 });
  await expect(unknown.last()).toHaveText("ERROR: \nUnknown command '\\''.");
  await expect(input).toHaveValue('');
});

// What the mysql CLI prints for `input` typed at its prompt — under a
// pty (`expect`), as a paste arrives: each line read in turn, its notes
// said as it's read, each statement run at its delimiter. Null without
// the CLI (or its login: MYSQL_PWD).
function mysqlCli(input: string): string | null {
  const script = [
    'set timeout 10', 'log_user 1',
    'spawn mysql -h 127.0.0.1 -u root sakila',
    'expect "mysql> "',
    `send "${input.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\r')}\\r"`,
    'sleep 2', 'send "quit\\r"', 'expect eof',
  ].join('\n');
  const run = spawnSync('expect', ['-c', script], { encoding: 'utf8' });
  if (run.error || !run.stdout.includes('mysql> ')) return null;
  return run.stdout.replace(/\r/g, '');
}

// Each line of a paste said in turn, as the CLI reads it: a line's
// notes after what the statements before it printed, before what its
// own statement prints.
test('mysql: each line of a paste is noted as the CLI reads it', async ({ page }) => {
  const paste = "select 1 \\i;\nselect 2 as b;\nselect 3 \\j as c;";
  const theirs = mysqlCli(paste);
  test.skip(theirs === null, 'mysql CLI not available');
  await openApp(page);
  await clickConnection(page, 'Local MySQL');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  const run = async (cmd: string) => { await input.fill(cmd); await input.press('Enter'); };
  const unknown = out.locator('.shell-entry-error', { hasText: 'Unknown command' });
  const syntax = out.locator('.shell-entry-error', { hasText: 'ERROR 1064 (42000)' });

  // The order things are said in: what follows each marker, first seen.
  const markers = ["Unknown command '\\i'.", "near '\\i'", '| b |', "Unknown command '\\j'.", "near '\\j as c'"];
  const order = (text: string) => markers.map(m => text.indexOf(m));
  const cli = order(theirs!.slice(theirs!.indexOf('mysql> select 1')).replace(/\| 2 \|/, ''));
  expect(cli.every((at, i) => at >= 0 && (i === 0 || at > cli[i - 1])), `${cli}`).toBe(true);

  await run(paste);
  await expect(syntax).toHaveCount(2, { timeout: 10_000 });
  await expect(unknown).toHaveCount(2);
  const ours = await out.locator('.shell-entry-error, .shell-table').evaluateAll(nodes => nodes.map(n =>
    n.classList.contains('shell-table') ? `| ${[...n.querySelectorAll('th')].map(th => th.textContent).join(' | ')} |` : n.textContent!));
  const said = ours.join('\n');
  const at = order(said);
  expect(at.every((pos, i) => pos >= 0 && (i === 0 || pos > at[i - 1])), `${at}\n${said}`).toBe(true);

  // A statement over lines: each line's notes, then its answer.
  await run('select 6 \\i\n as d \\j\n;');
  await expect(syntax).toHaveCount(3, { timeout: 10_000 });
  await expect(unknown).toHaveCount(4);
  const last = (await out.locator('.shell-entry-error').allInnerTexts()).slice(-3);
  expect(last[0]).toContain("'\\i'");
  expect(last[1]).toContain("'\\j'");
  expect(last[2]).toContain('ERROR 1064');

  // Lines entered one at a time: each said once, as entered.
  await run('select 7 \\i');
  await expect(unknown).toHaveCount(5, { timeout: 10_000 });
  await run('select 7 \\i\n\\j as e');
  await expect(unknown).toHaveCount(6, { timeout: 10_000 });
  await expect(unknown.last()).toContainText("'\\j'");
  await run('select 7 \\i\n\\j as e\n;');
  await expect(syntax).toHaveCount(4, { timeout: 10_000 });
  await expect(unknown).toHaveCount(6);

  // Cleared and typed again: read afresh, said again.
  await run('select 8 \\i');
  await expect(unknown).toHaveCount(7, { timeout: 10_000 });
  await input.fill('');
  await run('select 8 \\i;');
  await expect(syntax).toHaveCount(5, { timeout: 10_000 });
  await expect(unknown).toHaveCount(8);

  // A command after the line: the line's note first, as the CLI reads
  // it, then the command's output.
  await run('select 9 \\i\n\\s');
  await expect(unknown).toHaveCount(9, { timeout: 10_000 });
  await expect(out).toContainText('Current database:', { timeout: 10_000 });
  const text = await out.innerText();
  expect(text.lastIndexOf("Unknown command '\\i'.")).toBeLessThan(text.lastIndexOf('Current database:'));
});
