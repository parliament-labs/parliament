import fs from 'node:fs';
import path from 'node:path';
import { execSync } from 'node:child_process';
import type { Page, Locator } from '@playwright/test';

// Committed PNG output for README + docs — intentionally not under
// `tests/playwright/` (that's tooling), instead under `docs/screenshots/`
// at the repo root.
const REPO_ROOT = path.resolve(__dirname, '..', '..', '..');
const SCREENSHOTS_DIR = path.join(REPO_ROOT, 'docs', 'screenshots');
const PROVENANCE_PATH = path.join(SCREENSHOTS_DIR, 'provenance.json');

export function screenshotPath(slug: string): string {
  return path.join(SCREENSHOTS_DIR, `${slug}.png`);
}

type ProvenanceEntry = {
  filename: string;
  sha: string;
  isoTimestamp: string;
  specFile: string;
};

let cachedSha: string | null = null;
function gitShortSha(): string {
  if (cachedSha !== null) return cachedSha;
  try {
    cachedSha = execSync('git rev-parse --short HEAD', {
      encoding: 'utf8',
      cwd: REPO_ROOT,
    }).trim();
  } catch {
    cachedSha = 'unknown';
  }
  return cachedSha;
}

function recordProvenance(slug: string, specFile: string): void {
  const filename = `${slug}.png`;
  const entry: ProvenanceEntry = {
    filename,
    sha: gitShortSha(),
    isoTimestamp: new Date().toISOString(),
    specFile: path.basename(specFile),
  };
  let existing: Record<string, ProvenanceEntry> = {};
  if (fs.existsSync(PROVENANCE_PATH)) {
    try {
      existing = JSON.parse(fs.readFileSync(PROVENANCE_PATH, 'utf8'));
    } catch {
      existing = {};
    }
  }
  existing[filename] = entry;
  fs.writeFileSync(PROVENANCE_PATH, JSON.stringify(existing, null, 2) + '\n');
}

// Capture a screenshot of `target` (Page for full-viewport, Locator for
// a specific element) and record its provenance in provenance.json —
// git short-SHA, ISO timestamp, source spec filename.
//
// `specFile` should be `__filename` from the calling spec so the
// provenance entry preserves which spec produced the PNG.
export async function saveScreenshot(
  target: Page | Locator,
  slug: string,
  specFile: string,
): Promise<void> {
  const fullPath = screenshotPath(slug);
  await target.screenshot({ path: fullPath });
  recordProvenance(slug, specFile);
}
