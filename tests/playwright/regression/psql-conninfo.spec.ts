import { test, expect } from '@playwright/test';
import { execFileSync } from 'child_process';
import { openApp } from '../helpers/app';
import { clickConnection } from '../helpers/connect';
import { addShellPane, visibleShellInput, visibleShellOutput } from '../helpers/panes';

// psql 18's `\conninfo` (`exec_command_conninfo`): "Connection
// Information", a Parameter / Value row each for what libpq knows of the
// session. Compared with psql's own for the same user and host — all but
// the backend's PID, which is the session's. The comparison needs psql
// on PATH (and the password for the connection's user, as psql reads it
// from the environment); it's skipped without.

function psql(host: string, user: string): string | null {
  try {
    return execFileSync('psql', ['-X', '-h', host, '-U', user, '-d', 'pagila', '-P', 'pager=off', '-c', '\\conninfo']).toString();
  } catch {
    return null;
  }
}

// The lines that don't change from session to session.
const lasting = (text: string) => text.split('\n').map(l => l.trimEnd())
  .filter(l => l !== '' && !l.trimStart().startsWith('Backend PID'));

test('psql: \\conninfo prints what psql prints', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  await input.fill('\\conninfo');
  await input.press('Enter');

  const block = out.locator('.shell-printed').last();
  await expect(block).toContainText('Connection Information', { timeout: 10_000 });
  const ours = await block.innerText();
  const value = (name: string) => ours.split('\n').find(l => l.trimStart().startsWith(`${name} `))?.split('|')[1]?.trim() ?? '';
  // psql's rows, in psql's order (Host Address and the TLS rows only
  // when they apply).
  const optional = ['Host Address', 'SSL Library', 'SSL Protocol', 'SSL Key Bits', 'SSL Cipher', 'SSL Compression', 'ALPN'];
  const names = ours.split('\n').filter(l => l.includes('|')).map(l => l.split('|')[0].trim()).slice(1);
  expect(names.filter(n => !optional.includes(n))).toEqual([
    'Database', 'Client User', 'Host', 'Server Port', 'Options', 'Protocol Version', 'Password Used',
    'GSSAPI Authenticated', 'Backend PID', 'SSL Connection', 'Superuser', 'Hot Standby',
  ]);
  expect(value('Database')).toBe('pagila');
  expect(ours).toMatch(/\(1[2389] rows\)/);

  const theirs = psql(value('Host'), value('Client User'));
  test.skip(theirs === null, 'psql not on PATH, or no password for the connection\'s user');
  expect(lasting(ours)).toEqual(lasting(theirs!));
});

test('psql: \\conninfo under \\x prints what psql prints', async ({ page }) => {
  await openApp(page);
  await clickConnection(page, 'Local Postgres');
  await addShellPane(page);
  const input = visibleShellInput(page);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const out = visibleShellOutput(page);
  for (const cmd of ['\\x on', '\\conninfo']) {
    await input.fill(cmd);
    await input.press('Enter');
  }
  const block = out.locator('.shell-printed').last();
  await expect(block).toContainText('-[ RECORD 1 ]', { timeout: 10_000 });
  const ours = await block.innerText();
  expect(ours.split('\n')[0]).toBe('Connection Information');
  expect(ours).not.toMatch(/\(1[2389] rows\)/);

  const row = (name: string) => {
    const lines = ours.split('\n');
    const at = lines.findIndex(l => l === `Parameter | ${name}`);
    return lines[at + 1]?.split('|')[1]?.trim() ?? '';
  };
  let theirs: string | null = null;
  try {
    theirs = execFileSync('psql', ['-X', '-h', row('Host'), '-U', row('Client User'), '-d', 'pagila', '-P', 'pager=off',
      '-c', '\\x on', '-c', '\\conninfo']).toString().replace(/^Expanded display is on\.\n/, '');
  } catch { /* no psql, or no password */ }
  test.skip(theirs === null, 'psql not on PATH, or no password for the connection\'s user');
  // All but the backend PID's value.
  const lasting = (text: string) => text.split('\n').map(l => l.trimEnd()).filter(l => l !== '');
  const pid = (lines: string[]) => lines.map((l, i) => lines[i - 1] === 'Parameter | Backend PID' ? 'Value     | <pid>' : l);
  expect(pid(lasting(ours))).toEqual(pid(lasting(theirs!)));
});
