// node build/verify-release.mjs <zip>
// Checks a release ZIP against the install layout, then runs its streamloop.exe with --version and
// its token server until /ping answers, and expects the version in the ZIP's file name from both.
// A streamloop-vX.Y.Z.exe beside the ZIP must be the same file as the one inside.
import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { FULL_LAYOUT, missingPaths } from './release-layout.js';

const freePort = () => new Promise((resolve, reject) => {
  const s = net.createServer().once('error', reject).listen(0, '127.0.0.1', () => {
    const { port } = s.address();
    s.close(() => resolve(port));
  });
});

/** What the token server names itself on /ping, or why it did not answer. Answering /ping asks YouTube nothing. */
async function tokenServerIdentity(exe, cwd) {
  const port = await freePort();
  const child = spawn(exe, ['server', '--host', '127.0.0.1', '--port', String(port)], { cwd, stdio: 'ignore', windowsHide: true });
  const exited = new Promise((resolve) => child.once('exit', resolve));
  try {
    for (const until = Date.now() + 10_000; Date.now() < until;) {
      if (child.exitCode !== null) return `an exit with code ${child.exitCode}`;
      const ping = await fetch(`http://127.0.0.1:${port}/ping`, { signal: AbortSignal.timeout(1000) })
        .then((r) => r.json())
        .catch(() => null);
      if (ping) return ping.implementation;
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    return 'no answer on /ping within 10 s';
  } finally {
    child.kill();
    await exited;
  }
}

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

/** The first problem with the ZIP's two exes, or null. */
async function checkExes(work) {
  execFileSync(TAR, ['-xf', zip, '-C', work, 'streamloop/streamloop.exe', 'streamloop/supervisor/streamloop-pot.exe'], { windowsHide: true });
  const exe = join(work, 'streamloop', 'streamloop.exe');
  const printed = execFileSync(exe, ['--version'], { encoding: 'utf-8', cwd: work, timeout: 10000, windowsHide: true }).trim();
  if (printed !== version) return `${basename(zip)}: streamloop.exe --version printed "${printed}", expected "${version}"`;
  const pot = await tokenServerIdentity(join(work, 'streamloop', 'supervisor', 'streamloop-pot.exe'), work);
  if (pot !== `streamloop-pot ${version}`) return `${basename(zip)}: the token server gave ${JSON.stringify(pot)}, expected "streamloop-pot ${version}"`;
  const loose = join(dirname(zip), `streamloop-v${version}.exe`);
  const hash = (p) => createHash('sha256').update(readFileSync(p)).digest('hex');
  if (existsSync(loose) && hash(loose) !== hash(exe)) return `${basename(loose)} differs from the streamloop.exe inside ${basename(zip)}`;
  return null;
}

const work = mkdtempSync(join(tmpdir(), 'streamloop-verify-'));
let problem;
try {
  problem = await checkExes(work);
} finally {
  rmSync(work, { recursive: true, force: true });
}
if (problem) {
  console.error(problem);
  process.exit(1);
}
console.log(`${basename(zip)}: ${entries.length} entries, layout complete, streamloop.exe --version ${version}, token server streamloop-pot ${version}`);
