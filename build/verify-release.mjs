// node build/verify-release.mjs <zip>
// Checks a release ZIP against the install layout; an -update.zip against the update layout.
import { execFileSync } from 'node:child_process';
import { basename } from 'node:path';
import { FULL_LAYOUT, UPDATE_LAYOUT, missingPaths } from './release-layout.js';

const zip = process.argv[2];
if (!zip) {
  console.error('usage: node build/verify-release.mjs <zip>');
  process.exit(2);
}
const update = basename(zip).endsWith('-update.zip');
const entries = execFileSync('C:\\Windows\\System32\\tar.exe', ['-tf', zip], { encoding: 'utf-8', maxBuffer: 1 << 28, windowsHide: true })
  .split(/\r?\n/)
  .filter(Boolean);
const missing = missingPaths(entries, update ? UPDATE_LAYOUT : FULL_LAYOUT);
if (missing.length) {
  console.error(`${basename(zip)} is missing:\n${missing.map((p) => `  ${p}`).join('\n')}`);
  process.exit(1);
}
console.log(`${basename(zip)}: ${entries.length} entries, ${update ? 'update' : 'full'} layout complete`);
