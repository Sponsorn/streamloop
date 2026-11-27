// R3's click-through session: a real StreamLoop install, local MediaMTX and a Discord catcher, for
// driving the app (streamloop.exe ui) by hand (or by computer use) through every dashboard action.
//   node crates/ui/scripts/ui-session.mjs dev|update|update-fail [--no-app]
// dev:         release N (dist\'s build) with nothing newer listed, so it is up to date.
// update:      N with N+1 (N's patch number plus one) listed: Update Now lands it at once; left alone, the first check stages it
//              and it lands at the planned break, brought forward to encoder uptime BREAK_AT_S.
// update-fail: N with the N+1 whose supervisor mode exits at start: the landing rolls back within 180 s and
//              the banner shows the failure.
// N comes from `npm run build:release`; N+1 and the broken N+1 are install-session.mjs's test builds, built
// here by cargo when they are missing or stale. Start it from PowerShell: a job that refuses breakaway (the
// Bash tool's) puts the supervisor in it. The install goes to %TEMP%\sl3 ui Ö\ and is started from its own
// START.bat, hidden. The session holds until supervisor\results\ui-<mode>\stop exists or Ctrl+C (a `restart`
// file there starts START.bat again), then stops everything it started, removes the install and lists what
// still runs.
import { existsSync, readFileSync, rmSync, writeFileSync, appendFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import http from 'node:http';
import { PLAYLIST, SECOND_PLAYLIST } from '../../../supervisor/scripts/lib.mjs';
import {
  spawn, run, KEY, INGEST, sleep, iso, useOut, note, extract, quietConfig, startBat, nodeUp, portOpen,
  allProcesses, sweepLeftovers, quickStop, tempFolders, onInterrupt,
  VARIANTS, ASSET_PORT, buildVariants, writeReleaseList, assetHost,
} from '../../../supervisor/scripts/install-kit.mjs';

const MODES = ['dev', 'update', 'update-fail'];
const MODE = process.argv[2];
if (!MODES.includes(MODE)) {
  console.error(`usage: node crates/ui/scripts/ui-session.mjs ${MODES.join('|')} [--no-app]`);
  process.exit(2);
}
const START_APP = !process.argv.includes('--no-app');

const REPO = fileURLToPath(new URL('../../..', import.meta.url));
// A worktree has no spikes\tools: those are ignored files of the main checkout.
const common = await run('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: REPO, encoding: 'utf8' });
const MAIN = common.status === 0 ? dirname(common.stdout.trim()) : REPO;
const inRepo = (...parts) => [join(REPO, ...parts), join(MAIN, ...parts)].find((p) => existsSync(p)) ?? join(REPO, ...parts);
const TOOLS = dirname(inRepo('spikes', 'tools', 'mediamtx.exe'));

const VERSION = JSON.parse(readFileSync(join(REPO, 'package.json'), 'utf8')).version;
const DIST = join(REPO, 'dist');
const ZIP = join(DIST, `streamloop-v${VERSION}.zip`);
const SERVE = join(REPO, 'supervisor', 'results', 'ui-releases', 'serve');
const LISTED = { dev: [], update: ['n1'], 'update-fail': ['n1-supervisor-fails'] }[MODE];
const BREAK_AT_S = 300;

const OUT = join(REPO, 'supervisor', 'results', `ui-${MODE}`);
const STOP = join(OUT, 'stop');
const RESTART = join(OUT, 'restart');
const STARTUP_VBS = join(process.env.APPDATA ?? '', 'Microsoft', 'Windows', 'Start Menu', 'Programs', 'Startup', 'StreamLoop.vbs');
const S = join(tmpdir(), 'sl3 ui Ö');
const ROOT = join(S, 'streamloop');
const IN_TEMP = tempFolders(['sl3 ui ']);
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

if (!existsSync(ZIP)) { note(`missing ${ZIP}: run npm run build:release first`); process.exit(2); }
const swept = await sweepLeftovers({ dirs: [S], ascii: IN_TEMP, mediamtx: join(TOOLS, 'mediamtx.exe') });
if (swept.owner) { note('BLOCKED: the install folder belongs to a session that is still running', swept); process.exit(3); }
const ports = [7654, 4417, 4416, CATCHER_PORT, ...(LISTED.length ? [ASSET_PORT] : [])];
const taken = [];
for (const port of ports) if (await portOpen(port)) taken.push(port);
if (taken.length) { note('BLOCKED: ports taken by processes this session did not start', { taken }); process.exit(3); }
if (LISTED.length) {
  const built = await buildVariants({ repo: REPO, serve: SERVE, ids: LISTED })
    .catch((err) => { note('the test builds failed', { error: String(err?.message ?? err) }); process.exit(2); });
  note('releases listed', Object.fromEntries(Object.entries(built).map(([id, b]) => [id, { version: b.version, sha256: b.sha256 }])));
}

onInterrupt(async () => { await teardown(); });

// the release host and the Discord catcher

if (LISTED.length) {
  const host = assetHost(SERVE, { push: (row) => appendFileSync(join(OUT, 'assets.jsonl'), `${JSON.stringify(row)}\n`) });
  await new Promise((resolve) => host.listen(ASSET_PORT, '127.0.0.1', resolve));
  servers.push(host);
}
// Even with nothing listed the updater reads this file, so no check reaches GitHub.
const releasesFile = writeReleaseList(join(OUT, 'releases.json'), LISTED);

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
const example = join(ROOT, 'app', 'config.example.json');
// The update modes check at start, so the first check stages N+1 for the planned break.
quietConfig(example, { autoUpdateCheck: LISTED.length > 0 });
// A wizard left at its default ingest then streams to local MediaMTX, never the dummy key to Twitch.
const config = JSON.parse(readFileSync(example, 'utf8'));
config.supervisor = { ...config.supervisor, ingestUrl: INGEST };
writeFileSync(example, JSON.stringify(config, null, 2));
const env = {
  STREAMLOOP_TEST_HLS_URL: HLS,
  STREAMLOOP_TEST_RELEASES: releasesFile,
  ...(LISTED.length ? { STREAMLOOP_TEST_BREAK_AT_S: String(BREAK_AT_S) } : {}),
};
bat = startBat(ROOT, MODE, env);
await nodeUp(`${MODE} first start`);

if (START_APP) {
  // The app is the install's own exe in ui mode: its folder holds app\, so that is its root.
  app = spawn(join(ROOT, 'streamloop.exe'), ['ui'], { cwd: OUT, stdio: 'ignore', windowsHide: false });
  note('app started', { pid: app.pid });
}

const listed = LISTED.map((id) => `v${VARIANTS[id].version}${VARIANTS[id].feature ? ` (${VARIANTS[id].feature})` : ''}`).join(', ');
console.log([
  '',
  `ui-session ${MODE}: v${VERSION} runs from ${ROOT}`,
  `  Ingest URL:  ${INGEST}`,
  `  Stream key:  ${KEY}   (a dummy key for local MediaMTX)`,
  `  Playlists:   ${PLAYLIST}   and   ${SECOND_PLAYLIST}`,
  `  Webhook URL: ${WEBHOOK}   (posts land in ${join(OUT, 'catcher.jsonl')})`,
  ...(LISTED.length ? [
    `  Listed:      ${listed}`,
    `  Update:      Update Now lands it at once; left alone it lands at the planned break, ${BREAK_AT_S} s into the encoder's uptime`,
  ] : ['  Listed:      nothing newer']),
  `  Restart:     after Shut down (START.bat stays down on exit 0), create ${RESTART}`,
  `  Stop:        create ${STOP}, or press Ctrl+C`,
  '',
].join('\n'));

while (!existsSync(STOP)) {
  if (existsSync(RESTART)) {
    rmSync(RESTART, { force: true });
    if (bat.exitCode === null) {
      note('restart file ignored: START.bat is still running');
    } else {
      bat = startBat(ROOT, `${MODE}-restart`, env);
      await nodeUp(`${MODE} restart`);
    }
  }
  await sleep(2000);
}
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
  // Autostart turned on from the app points the user's Startup folder at this temp install.
  if (existsSync(STARTUP_VBS) && readFileSync(STARTUP_VBS, 'utf16le').includes(ROOT)) {
    rmSync(STARTUP_VBS, { force: true });
    note('removed StreamLoop.vbs from the Startup folder: it started this temp install');
  }
  // streamloop-pot.exe is the token server; node.exe only runs the TypeScript fallback.
  const names = ['streamloop.exe', 'node.exe', 'streamloop-pot.exe', 'ffmpeg.exe', 'ffprobe.exe', 'yt-dlp.exe', 'mediamtx.exe', 'cmd.exe'];
  const left = (await allProcesses())
    .filter((p) => names.includes(String(p.Name).toLowerCase()) && p.ProcessId !== process.pid)
    .filter((p) => p.Name !== 'cmd.exe' || /start\.bat/i.test(p.CommandLine ?? ''))
    .map((p) => ({ pid: p.ProcessId, name: p.Name, path: p.ExecutablePath ?? null }));
  note('still running after the teardown (other sessions may own some)', left);
  writeFileSync(join(OUT, 'leftovers.json'), JSON.stringify(left, null, 2));
}
