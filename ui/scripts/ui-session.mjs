// R3's click-through session: a real StreamLoop install, local MediaMTX and a Discord catcher, for
// driving streamloop-ui.exe by hand (or by computer use) through every dashboard action.
//   node ui/scripts/ui-session.mjs dev|full|full-fail|update [--no-app]
// dev:       release N (patch + 1) from the local release list; only N is listed, so it is up to date.
// full:      release N, with N+1 listed: Update Now is a full update (supervisor and token server).
// full-fail: release N, with the broken N+1 listed: the full update rolls back.
// update:    v3.0.4's full ZIP from dist\, updating from GitHub's real release list (app only).
// The releases for dev and the full runs come from `node supervisor/scripts/update-session.mjs build`.
// The install goes to %TEMP%\sl3 ui Ö\ and is started from its own START.bat, hidden. The session
// holds until supervisor\results\ui-<mode>\stop exists or Ctrl+C, then stops everything it started,
// removes the install and lists what is still running.
import { createReadStream, existsSync, readFileSync, statSync, writeFileSync, appendFileSync, copyFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import http from 'node:http';
import { PLAYLIST, SECOND_PLAYLIST } from '../../supervisor/scripts/lib.mjs';
import {
  spawn, run, KEY, INGEST, sleep, iso, useOut, note, extract, quietConfig, startBat, nodeUp, portOpen,
  allProcesses, sweepLeftovers, quickStop, tempFolders, onInterrupt, sha256File,
} from '../../supervisor/scripts/install-kit.mjs';

const MODES = ['dev', 'full', 'full-fail', 'update'];
const MODE = process.argv[2];
if (!MODES.includes(MODE)) {
  console.error(`usage: node ui/scripts/ui-session.mjs ${MODES.join('|')} [--no-app]`);
  process.exit(2);
}
const START_APP = !process.argv.includes('--no-app');

const REPO = fileURLToPath(new URL('../..', import.meta.url));
// A worktree has no spikes\tools or older dist\ ZIPs: those are ignored files of the main checkout.
const common = await run('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: REPO, encoding: 'utf8' });
const MAIN = common.status === 0 ? dirname(common.stdout.trim()) : REPO;
const inRepo = (...parts) => [join(REPO, ...parts), join(MAIN, ...parts)].find((p) => existsSync(p)) ?? join(REPO, ...parts);
const TOOLS = dirname(inRepo('spikes', 'tools', 'mediamtx.exe'));
const UI_EXE = join(REPO, 'ui', 'target', 'release', 'streamloop-ui.exe');

const VERSION = JSON.parse(readFileSync(join(REPO, 'package.json'), 'utf8')).version;
const [MAJOR, MINOR, PATCH] = VERSION.split('-')[0].split('.').map(Number);
const N = `${MAJOR}.${MINOR}.${PATCH + 1}`;
const N1 = `${MAJOR}.${MINOR}.${PATCH + 2}`;
const SERVE = join(REPO, 'supervisor', 'results', 'update-releases', 'serve');
const RELEASES = { n: N, n1: N1, 'n1-broken': N1 };
const releaseFiles = (v) => [`streamloop-v${v}.zip`, `streamloop-v${v}.sha256`, `streamloop-v${v}-update.zip`, `streamloop-v${v}-update.sha256`, 'node-version.txt', 'components.json'];
const LISTED = { dev: ['n'], full: ['n', 'n1'], 'full-fail': ['n', 'n1-broken'], update: [] }[MODE];

const OLD_TAG = 'v3.0.4';
const OLD_ZIP = inRepo('dist', `streamloop-${OLD_TAG}.zip`);
const ZIP = MODE === 'update' ? OLD_ZIP : join(SERVE, 'n', `streamloop-v${N}.zip`);

const OUT = join(REPO, 'supervisor', 'results', `ui-${MODE}`);
const STOP = join(OUT, 'stop');
const S = join(tmpdir(), 'sl3 ui Ö');
const ROOT = join(S, 'streamloop');
const IN_TEMP = tempFolders(['sl3 ui ']);
const ASSET_PORT = 8791;
const CATCHER_PORT = 8790;
const WEBHOOK = `http://127.0.0.1:${CATCHER_PORT}/webhook/ui-${MODE}`;
// The Twitch read-back reads the local HLS and the channel counts as live, so nothing reaches Twitch.
const HLS = `http://127.0.0.1:8888/live/${KEY}/index.m3u8`;

useOut(OUT);
const servers = [];
let mediamtx = null;
let bat = null;
let app = null;

// preflight

if (!existsSync(ZIP)) {
  note(MODE === 'update' ? `missing ${ZIP}` : `missing ${ZIP}: run node supervisor/scripts/update-session.mjs build first`);
  process.exit(2);
}
if (MODE === 'update') {
  const want = readFileSync(`${OLD_ZIP.slice(0, -4)}.sha256`, 'utf8').trim().split(/\s+/)[0].toLowerCase();
  if (sha256File(OLD_ZIP) !== want) { note(`${OLD_ZIP} does not match its .sha256`); process.exit(2); }
}
for (const id of LISTED) {
  for (const f of releaseFiles(RELEASES[id])) {
    if (!existsSync(join(SERVE, id, f))) { note(`missing ${join(SERVE, id, f)}: run update-session.mjs build`); process.exit(2); }
  }
}
if (START_APP && !existsSync(UI_EXE)) { note(`missing ${UI_EXE}: cargo build --release in ui\\`); process.exit(2); }
const swept = await sweepLeftovers({ dirs: [S], ascii: IN_TEMP, mediamtx: join(TOOLS, 'mediamtx.exe') });
if (swept.owner) { note('BLOCKED: the install folder belongs to a session that is still running', swept); process.exit(3); }
const ports = [7654, 4417, 4416, CATCHER_PORT, ...(LISTED.length ? [ASSET_PORT] : [])];
const taken = [];
for (const port of ports) if (await portOpen(port)) taken.push(port);
if (taken.length) { note('BLOCKED: ports taken by processes this session did not start', { taken }); process.exit(3); }

onInterrupt(async () => { await teardown(); });

// the release host and the Discord catcher

if (LISTED.length) {
  const host = http.createServer((req, res) => {
    const parts = decodeURIComponent(new URL(req.url, 'http://127.0.0.1').pathname).split('/').filter(Boolean);
    const file = parts.length === 2 && LISTED.includes(parts[0]) && !parts[1].includes('..') ? join(SERVE, parts[0], parts[1]) : null;
    if (!file || !existsSync(file)) { res.writeHead(404).end(); return; }
    res.writeHead(200, { 'content-length': statSync(file).size, 'content-type': 'application/octet-stream' });
    createReadStream(file).pipe(res);
    appendFileSync(join(OUT, 'assets.jsonl'), `${JSON.stringify({ at: iso(), path: req.url })}\n`);
  });
  await new Promise((resolve) => host.listen(ASSET_PORT, '127.0.0.1', resolve));
  servers.push(host);
}
const releasesFile = join(OUT, 'releases.json');
writeFileSync(releasesFile, JSON.stringify(LISTED.map((id) => ({
  tag_name: `v${RELEASES[id]}`, draft: false, prerelease: true,
  assets: releaseFiles(RELEASES[id]).map((f) => ({ name: f, browser_download_url: `http://127.0.0.1:${ASSET_PORT}/${id}/${f}` })),
})), null, 2));

const catcher = http.createServer((req, res) => {
  let body = '';
  req.on('data', (c) => { body += c; });
  req.on('end', () => {
    let parsed = {};
    try { parsed = JSON.parse(body || '{}'); } catch { /* not json */ }
    const embed = parsed.embeds?.[0] ?? {};
    const row = { at: iso(), path: req.url, username: parsed.username ?? null, content: parsed.content ?? null, title: embed.title ?? null, description: embed.description ?? null, footer: embed.footer?.text ?? null };
    appendFileSync(join(OUT, 'catcher.jsonl'), `${JSON.stringify(row)}\n`);
    res.writeHead(204).end();
  });
});
await new Promise((resolve) => catcher.listen(CATCHER_PORT, '127.0.0.1', resolve));
servers.push(catcher);

if (await portOpen(1935)) {
  note('MediaMTX already up on 1935: using it');
} else {
  mediamtx = spawn(join(TOOLS, 'mediamtx.exe'), [], { cwd: TOOLS, stdio: 'ignore' });
  await sleep(3000);
  if (!(await portOpen(1935))) { note('MediaMTX did not open 1935'); await teardown(); process.exit(2); }
  note('MediaMTX started', { pid: mediamtx.pid });
}

// the install

note('extracting', { zip: ZIP, to: S, ms: await extract(ZIP, S) });
quietConfig(join(ROOT, 'app', 'config.example.json'));
const env = { STREAMLOOP_TEST_HLS_URL: HLS, ...(LISTED.length ? { STREAMLOOP_TEST_RELEASES: releasesFile } : {}) };
bat = startBat(ROOT, MODE, env);
await nodeUp(`${MODE} first start`);

if (START_APP) {
  // The app runs from a copy in the results folder, so a rebuild in ui\target is not blocked.
  const exe = join(OUT, 'streamloop-ui.exe');
  copyFileSync(UI_EXE, exe);
  app = spawn(exe, [], { cwd: OUT, stdio: 'ignore', windowsHide: false });
  note('app started', { pid: app.pid });
}

console.log([
  '',
  `ui-session ${MODE}: the install runs from ${ROOT}`,
  `  Ingest URL:  ${INGEST}`,
  `  Stream key:  ${KEY}   (a dummy key for local MediaMTX)`,
  `  Playlists:   ${PLAYLIST}   and   ${SECOND_PLAYLIST}`,
  `  Webhook URL: ${WEBHOOK}   (posts land in ${join(OUT, 'catcher.jsonl')})`,
  `  Stop:        create ${STOP}, or press Ctrl+C`,
  '',
].join('\n'));

while (!existsSync(STOP)) await sleep(2000);
note('stop file found');
await teardown();
process.exit(0);

// teardown

async function teardown() {
  if (teardown.done) return;
  teardown.done = true;
  if (app && app.exitCode === null) await run('taskkill', ['/F', '/T', '/PID', String(app.pid)]);
  note('quick stop', await quickStop([bat], [S]));
  await sleep(3000);
  note('sweep', await sweepLeftovers({ dirs: [S], ascii: IN_TEMP, mediamtx: join(TOOLS, 'mediamtx.exe') }));
  if (mediamtx && mediamtx.exitCode === null) await run('taskkill', ['/F', '/T', '/PID', String(mediamtx.pid)]);
  for (const s of servers) s.close();
  const names = ['node.exe', 'streamloop-supervisor.exe', 'ffmpeg.exe', 'ffprobe.exe', 'yt-dlp.exe', 'mediamtx.exe', 'streamloop-ui.exe', 'cmd.exe'];
  const left = (await allProcesses())
    .filter((p) => names.includes(String(p.Name).toLowerCase()) && p.ProcessId !== process.pid)
    .filter((p) => p.Name !== 'cmd.exe' || /start\.bat/i.test(p.CommandLine ?? ''))
    .map((p) => ({ pid: p.ProcessId, name: p.Name, path: p.ExecutablePath ?? null }));
  note('still running after the teardown (other sessions may own some)', left);
  writeFileSync(join(OUT, 'leftovers.json'), JSON.stringify(left, null, 2));
}
