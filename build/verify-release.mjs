// node build/verify-release.mjs <zip>
// Checks a release ZIP against the install layout, then runs its streamloop.exe with --version
// and expects the version in the ZIP's file name. A streamloop-vX.Y.Z.exe beside the ZIP must be
// the same file as the one inside.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { FULL_LAYOUT, missingPaths } from './release-layout.js';

const TAR = 'C:\\Windows\\System32\\tar.exe';
const zip = process.argv[2];
const name = zip && basename(zip).match(/^streamloop-v(.+)\.zip$/);
if (!name) {
  console.error('usage: node build/verify-release.mjs dist/streamloop-vX.Y.Z.zip');
  process.exit(2);
}
const version = name[1];

const entries = execFileSync(TAR, ['-tf', zip], { encoding: 'utf-8', maxBuffer: 1 << 28, windowsHide: true })
  .split(/\r?\n/)
  .filter(Boolean);
const missing = missingPaths(entries, FULL_LAYOUT);
if (missing.length) {
  console.error(`${basename(zip)} is missing:\n${missing.map((p) => `  ${p}`).join('\n')}`);
  process.exit(1);
}

const work = mkdtempSync(join(tmpdir(), 'streamloop-verify-'));
try {
  execFileSync(TAR, ['-xf', zip, '-C', work, 'streamloop/streamloop.exe'], { windowsHide: true });
  const exe = join(work, 'streamloop', 'streamloop.exe');
  const printed = execFileSync(exe, ['--version'], { encoding: 'utf-8', cwd: work, timeout: 10000, windowsHide: true }).trim();
  if (printed !== version) {
    console.error(`${basename(zip)}: streamloop.exe --version printed "${printed}", expected "${version}"`);
    process.exit(1);
  }
  const loose = join(dirname(zip), `streamloop-v${version}.exe`);
  if (existsSync(loose)) {
    const hash = (p) => createHash('sha256').update(readFileSync(p)).digest('hex');
    if (hash(loose) !== hash(exe)) {
      console.error(`${basename(loose)} differs from the streamloop.exe inside ${basename(zip)}`);
      process.exit(1);
    }
  }
} finally {
  rmSync(work, { recursive: true, force: true });
}
console.log(`${basename(zip)}: ${entries.length} entries, layout complete, streamloop.exe --version ${version}`);
