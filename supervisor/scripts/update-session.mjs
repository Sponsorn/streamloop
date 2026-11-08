// The full-update proof on the dev machine: release ZIPs as a user gets them, a local release list
// and a local MediaMTX.
//   node update-session.mjs <run>[,<run>...] | all | build   (build: only the release ZIPs)
// Runs (RUNS below): full-on, full-off, app-only, locked, broken, bootstrap.
// Release N (3.0.6) and the N+1 variants (3.0.7) are repacked from dist\ into
// supervisor\results\update-releases\ and served on 127.0.0.1:8791; Node reads the list through
// STREAMLOOP_TEST_RELEASES. Discord posts go to a catcher on 127.0.0.1:8790.
// Each run installs into %TEMP%\sl3 update Ö\, goes live through the wizard's API calls, updates
// through the dashboard's, prints PASS or FAIL, writes supervisor\results\update\<run>\results.json,
// then removes the install and checks that nothing still runs from it.
// Finished downloads are kept in supervisor\results\update-cache-seed\ and put into the next
// install's data\cache\, so the runs fetch each video from YouTube once.
// UPDATE_SESSION_HOLD=<run> keeps that run's install up at its end, for a look at the dashboard.
// Ctrl+C or Ctrl+Break tears the run and the session down; a start after the harness was killed
// outright first sweeps what that harness left.
import { existsSync, mkdirSync, readFileSync, writeFileSync, rmSync, cpSync, readdirSync, statSync, copyFileSync, createReadStream, appendFileSync } from 'node:fs';
import { createHash, randomBytes } from 'node:crypto';
import { join, basename } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import http from 'node:http';
import { dailyFiles, EVENTS, PLAYLIST, redact } from './lib.mjs';
import {
  spawn, run, TAR, KEY, INGEST, READ_URL, NODE_API, sleep, iso, Stop, useOut, note, waitFor, startBat, nodeUp, api, supStatus, waitLive,
  packets, reconnects, runReader, stopReader, resetReader, gapStats, sha256File, grepTree, extract, quietConfig, tsxEval, portOpen,
  allProcesses, under, runningFrom, killAll, sweepLeftovers, onInterrupt, interrupted,
} from './install-kit.mjs';
import { hashParts } from '../../src/server/components.js';
import { updateOfferText, updateBannerView } from '../../src/admin/status-view.js';
import { failureText } from '../../src/admin/update-text.js';

const REPO = fileURLToPath(new URL('../..', import.meta.url));
const VERSION = JSON.parse(readFileSync(join(REPO, 'package.json'), 'utf8')).version;
const DIST_FULL = join(REPO, 'dist', `streamloop-v${VERSION}.zip`);
const DIST_SLIM = join(REPO, 'dist', `streamloop-v${VERSION}-update.zip`);
const N = '3.0.6';
const N1 = '3.0.7';
const BOOT_TAG = 'v3.0.5';
const BOOT_URL = `https://github.com/Sponsorn/streamloop/releases/download/${BOOT_TAG}/streamloop-${BOOT_TAG}`;
const RESULTS = join(REPO, 'supervisor', 'results');
const WORK = join(RESULTS, 'update-releases');
const SERVE = join(WORK, 'serve');
const OUT_ROOT = join(RESULTS, 'update');
const SEED = join(RESULTS, 'update-cache-seed');
const SEED_LIMIT = 4 * 1024 ** 3;
const TOOLS = join(REPO, 'spikes', 'tools');
const S = join(tmpdir(), 'sl3 update Ö');
const ROOT = join(S, 'streamloop');
const DATA = join(ROOT, 'data');
const STATE_FILE = join(DATA, 'update-state.json');
const MARKER = join(DATA, 'update-proof-marker.txt');
const ASSET_PORT = 8791;
const CATCHER_PORT = 8790;
const WEBHOOK = `http://127.0.0.1:${CATCHER_PORT}/webhook/update`;
const READY = 'Ready to stream. Press Start.';
const UPDATING = `Updating to ${N1}, the stream is off air until it is done.`;
const BACK = `Back on air on ${N1}.`;
// Overlay data at the end of a PE file: the exe still runs, its hash changes.
const APPEND = Buffer.from('streamloop update proof: this build is N+1\n'.repeat(64));

/** What each release changes against dist\'s build. */
const VARIANTS = {
  n: { version: N },
  n1: { version: N1, supervisor: 'append' },
  'n1-locked': { version: N1, supervisor: 'append', ffmpeg: 'append' },
  'n1-broken': { version: N1, supervisor: 'ffmpeg' },
  'n1-app': { version: N1 },
};
const zipName = (v, slim) => `streamloop-v${v}${slim ? '-update' : ''}.zip`;
const sumName = (v, slim) => `streamloop-v${v}${slim ? '-update' : ''}.sha256`;
const releaseFiles = (v) => [zipName(v, false), sumName(v, false), zipName(v, true), sumName(v, true), 'node-version.txt', 'components.json'];
const zipOf = (id, slim = false) => join(SERVE, id, zipName(VARIANTS[id].version, slim));
const componentsOf = (id) => JSON.parse(readFileSync(join(SERVE, id, 'components.json'), 'utf8'));

const readJson = (file) => { try { return JSON.parse(readFileSync(file, 'utf8')); } catch { return null; } };
const readText = (file) => { try { return readFileSync(file, 'utf8'); } catch { return null; } };
const sha = (text) => createHash('sha256').update(text).digest('hex');
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
async function getJson(path, timeoutMs = 3000) {
  try {
    const res = await fetch(NODE_API + path, { signal: AbortSignal.timeout(timeoutMs) });
    return res.ok ? await res.json() : null;
  } catch { return null; }
}
const nodeAnswers = async () => { try { return (await fetch(`${NODE_API}/token`, { signal: AbortSignal.timeout(1500) })).ok; } catch { return false; } };

// release N and the N+1 variants

function setVersion(file, version) {
  const pkg = JSON.parse(readFileSync(file, 'utf8'));
  pkg.version = version;
  writeFileSync(file, `${JSON.stringify(pkg, null, 2)}\n`);
}
/** Exit code and first output line; stdin gets `input`. Run from WORK, so a stray output file lands there. */
function exeRuns(exe, args, input = '') {
  return new Promise((resolve) => {
    const child = spawn(exe, args, { cwd: WORK, stdio: ['pipe', 'pipe', 'pipe'] });
    let out = '';
    child.stdout.on('data', (c) => { out += c; });
    child.stderr.on('data', (c) => { out += c; });
    child.on('error', (err) => resolve({ code: null, line: String(err) }));
    child.on('close', (code) => resolve({ code, line: out.trim().split(/\r?\n/)[0]?.slice(0, 120) ?? '' }));
    child.stdin.end(input);
  });
}

async function buildReleases() {
  const want = { recipe: 2, full: sha256File(DIST_FULL), slim: sha256File(DIST_SLIM), variants: VARIANTS, append: sha(APPEND) };
  const manifestFile = join(WORK, 'manifest.json');
  const have = readJson(manifestFile);
  const complete = have && same(have.want, want)
    && Object.entries(VARIANTS).every(([id, v]) => releaseFiles(v.version).every((f) => existsSync(join(SERVE, id, f))));
  if (complete) {
    note('releases already built from this dist', { built: have.at });
    return have;
  }
  rmSync(WORK, { recursive: true, force: true });
  mkdirSync(SERVE, { recursive: true });
  const full = join(WORK, 'tree-full');
  const slim = join(WORK, 'tree-slim');
  await extract(DIST_FULL, full);
  await extract(DIST_SLIM, slim);
  const top = join(full, 'streamloop');
  const supExe = join(top, 'supervisor', 'streamloop-supervisor.exe');
  const ffExe = join(top, 'ffmpeg', 'ffmpeg.exe');
  const sup0 = readFileSync(supExe);
  const ff0 = readFileSync(ffExe);
  const nodeVersion = readFileSync(join(top, 'app', 'node-version.txt'), 'utf8').trim();
  const built = {};
  for (const [id, v] of Object.entries(VARIANTS)) {
    const t0 = Date.now();
    for (const tree of [full, slim]) setVersion(join(tree, 'streamloop', 'app', 'package.json'), v.version);
    writeFileSync(supExe, v.supervisor === 'append' ? Buffer.concat([sup0, APPEND]) : v.supervisor === 'ffmpeg' ? ff0 : sup0);
    writeFileSync(ffExe, v.ffmpeg === 'append' ? Buffer.concat([ff0, APPEND]) : ff0);
    const runs = { supervisorProtect: await exeRuns(supExe, ['protect'], 'not-a-key'), ffmpegVersion: await exeRuns(ffExe, ['-hide_banner', '-version']) };
    // Only the broken release is meant to carry a supervisor that does not run.
    const supervisorRuns = runs.supervisorProtect.code === 0 && runs.supervisorProtect.line.length > 0;
    if (supervisorRuns !== (v.supervisor !== 'ffmpeg')) throw new Stop(`the repacked ${id} supervisor ${supervisorRuns ? 'runs' : 'does not run'}: protect exited ${runs.supervisorProtect.code}`);
    if (runs.ffmpegVersion.code !== 0) throw new Stop(`the repacked ${id} ffmpeg does not run: -version exited ${runs.ffmpegVersion.code}`);
    const dir = join(SERVE, id);
    mkdirSync(dir, { recursive: true });
    const components = { version: v.version, node: nodeVersion, parts: await hashParts(top) };
    writeFileSync(join(dir, 'components.json'), `${JSON.stringify(components, null, 2)}\n`);
    writeFileSync(join(dir, 'node-version.txt'), `${nodeVersion}\n`);
    const zips = {};
    for (const isSlim of [false, true]) {
      const zip = join(dir, zipName(v.version, isSlim));
      const z = await run(TAR, ['-acf', zip, '-C', isSlim ? slim : full, 'streamloop'], { timeout: 900_000 });
      if (z.status !== 0) throw new Stop(`tar -acf failed for ${zip}: ${z.stderr}`);
      const check = await run(process.execPath, [join(REPO, 'build', 'verify-release.mjs'), zip], { encoding: 'utf8' });
      if (check.status !== 0) throw new Stop(`verify-release refused ${zip}: ${check.stderr}`);
      const hash = sha256File(zip);
      writeFileSync(join(dir, sumName(v.version, isSlim)), `${hash}  ${zipName(v.version, isSlim)}\n`);
      zips[basename(zip)] = { bytes: statSync(zip).size, sha256: hash, verify: check.stdout.trim() };
    }
    built[id] = { ...v, components, runs, zips, ms: Date.now() - t0 };
    note(`built ${id}`, built[id]);
  }
  rmSync(full, { recursive: true, force: true });
  rmSync(slim, { recursive: true, force: true });
  const manifest = { want, built, at: iso() };
  writeFileSync(manifestFile, JSON.stringify(manifest, null, 2));
  return manifest;
}

/** The list a run's Node reads: GitHub's shape, local URLs. */
function writeReleases(name, ids) {
  const list = ids.map((id) => {
    const v = VARIANTS[id].version;
    return {
      tag_name: `v${v}`, draft: false, prerelease: true,
      assets: releaseFiles(v).map((f) => ({ name: f, browser_download_url: `http://127.0.0.1:${ASSET_PORT}/${id}/${f}` })),
    };
  });
  const file = join(WORK, `releases-${name}.json`);
  writeFileSync(file, JSON.stringify(list, null, 2));
  return file;
}

// the local release host and the Discord catcher

const assetLog = [];
const assetServer = http.createServer((req, res) => {
  const parts = decodeURIComponent(new URL(req.url, 'http://127.0.0.1').pathname).split('/').filter(Boolean);
  const file = parts.length === 2 && parts[0] in VARIANTS && !parts[1].includes('..') ? join(SERVE, parts[0], parts[1]) : null;
  const row = { ms: Date.now(), at: iso(), method: req.method, path: req.url };
  if (!file || !existsSync(file)) {
    res.writeHead(404).end();
    assetLog.push({ ...row, status: 404 });
    return;
  }
  const bytes = statSync(file).size;
  res.writeHead(200, { 'content-length': bytes, 'content-type': 'application/octet-stream' });
  createReadStream(file).pipe(res);
  assetLog.push({ ...row, status: 200, bytes });
});

const alerts = [];
const catcher = http.createServer((req, res) => {
  let body = '';
  req.on('data', (c) => { body += c; });
  req.on('end', () => {
    let embed = {};
    try { embed = JSON.parse(body || '{}').embeds?.[0] ?? {}; } catch { /* not json */ }
    // a batch prefixes each message with its level's emoji
    const messages = String(embed.description ?? '').split('\n\n').map((m) => m.replace(/^(ℹ️|⚠️|\u{1F6A8}) /u, '')).filter(Boolean);
    const row = { ms: Date.now(), at: iso(), title: embed.title, color: embed.color, footer: embed.footer?.text, messages };
    alerts.push(row);
    appendFileSync(join(runOut, 'catcher.jsonl'), `${JSON.stringify(row)}\n`);
    res.writeHead(204).end();
  });
});
const postsSince = (ms) => alerts.filter((a) => a.ms >= ms);
const posted = (ms, text) => postsSince(ms).some((a) => a.messages.includes(text));

// the install

const underInstall = () => runningFrom([S], /\\sl3 update /i);
const supervisors = async () => (await underInstall()).filter((p) => p.Name === 'streamloop-supervisor.exe').map((p) => p.ProcessId);

function seedCache() {
  if (!existsSync(SEED)) return 0;
  const dir = join(DATA, 'cache');
  mkdirSync(dir, { recursive: true });
  let n = 0;
  for (const f of readdirSync(SEED)) { copyFileSync(join(SEED, f), join(dir, f)); n += 1; }
  return n;
}
/** Only `<id>.mp4`: yt-dlp merges into another name and renames when the download is complete. */
function growSeed() {
  const dir = join(DATA, 'cache');
  if (!existsSync(dir)) return;
  mkdirSync(SEED, { recursive: true });
  let total = readdirSync(SEED).reduce((s, f) => s + statSync(join(SEED, f)).size, 0);
  for (const f of readdirSync(dir).filter((x) => /^[A-Za-z0-9_-]{11}\.mp4$/.test(x))) {
    const size = statSync(join(dir, f)).size;
    if (existsSync(join(SEED, f)) || total + size > SEED_LIMIT) continue;
    copyFileSync(join(dir, f), join(SEED, f));
    total += size;
  }
}

let bat = null;
let holder = null;

/** Extracts a full ZIP, starts its START.bat hidden and takes it live with the wizard's calls. */
async function goLive(r, zip, env, label) {
  r.install = { zip: basename(zip), bytes: statSync(zip).size, sha256: sha256File(zip) };
  r.install.extractMs = await extract(zip, S);
  quietConfig(join(ROOT, 'app', 'config.example.json'));
  r.install.seededVideos = seedCache();
  bat = startBat(ROOT, label, env);
  r.install.nodeUpMs = await nodeUp(`${label} first start`);
  const cfg = (await api('GET', '/config')).json;
  const check = await api('POST', '/setup-check/run');
  r.install.setupCheck = { status: check.status, encoder: check.json?.encoder, cacheMb: check.json?.cache?.recommendedMb, failedDeps: check.json?.dependencies?.filter((d) => !d.ok).map((d) => d.name) };
  const picked = { cacheBudgetMb: check.json.cache.recommendedMb };
  if (check.json.encoder.works) picked.encoder = check.json.encoder.chosen;
  const patch = Object.fromEntries(Object.entries(picked).filter(([k, v]) => cfg.supervisor[k] !== v));
  if (Object.keys(patch).length) await api('POST', '/config', { supervisor: patch });
  r.install.keyStatus = (await api('POST', '/stream-key', { key: KEY })).status;
  r.install.saveStatus = (await api('POST', '/config', { playlists: [{ id: PLAYLIST }], discord: { webhookUrl: WEBHOOK }, supervisor: { ingestUrl: INGEST } })).status;
  const live = await waitLive(`live after the wizard (${label})`);
  if (!live.value) throw new Stop(`${label} did not go live after the wizard`);
  r.install.wizardToLiveMs = live.ms;
  runReader();
  // Past the cold start's URL feeder before anything is changed.
  await sleep(30_000);
  r.install.readerBeforeUpdate = gapStats(Date.now() - 30_000, Date.now());
  r.install.playing = (await supStatus())?.playing ?? null;
}

const nodeLog = () => {
  const dir = join(ROOT, 'app', 'logs');
  if (!existsSync(dir)) return '';
  return readdirSync(dir).filter((f) => f.endsWith('.log')).map((f) => readText(join(dir, f)) ?? '').join('\n');
};

async function snapshot() {
  const cfgText = readFileSync(join(ROOT, 'app', 'config.json'), 'utf8');
  const cfg = JSON.parse(cfgText);
  const cacheDir = join(DATA, 'cache');
  return {
    version: (await getJson('/update/status'))?.currentVersion ?? null,
    parts: await hashParts(ROOT),
    ytdlpExe: sha256File(join(ROOT, 'yt-dlp', 'yt-dlp.exe')),
    startBat: sha256File(join(ROOT, 'START.bat')),
    configSha256: sha(cfgText),
    keyProtectedSha256: sha(cfg.supervisor?.streamKeyProtected ?? ''),
    autoStartStream: cfg.autoStartStream ?? null,
    marker: readText(MARKER),
    cache: existsSync(cacheDir) ? readdirSync(cacheDir).filter((f) => /^[A-Za-z0-9_-]{11}\.mp4$/.test(f)).sort() : [],
    data: readdirSync(DATA).sort(),
    root: readdirSync(ROOT).sort(),
    supervisorPids: await supervisors(),
    encoderGeneration: (await supStatus())?.encoder?.generation ?? null,
  };
}
const changedParts = (a, b) => Object.keys(a).filter((p) => a[p] !== b[p]);
const LEFTOVER = /^_update_|^START\.bat\.new$|^_apply_launcher\.bat$/;
const MARKERS = ['update-checking', 'update-swapping', 'waiting-for-start'];
const leftovers = (snap) => [...snap.root.filter((f) => LEFTOVER.test(f)), ...snap.data.filter((f) => MARKERS.includes(f))];

/** The monitor of the update in progress; runOne ends it whatever way the run ended. */
let monitor = null;
/** Polls the update's progress file, the supervisor and Node once a second while an update runs. */
function startMonitor(t0) {
  const m = { t0, states: [], playing: [], supervisor: [], node: [], savedAtStop: null, stop: false };
  monitor = m;
  m.done = (async () => {
    let lastState = '';
    let lastSup = null;
    let lastNode = null;
    while (!m.stop) {
      const st = readJson(STATE_FILE);
      const key = st ? `${st.step}/${st.result}` : 'none';
      if (key !== lastState) {
        lastState = key;
        m.states.push({ t: Date.now() - t0, step: st?.step ?? null, result: st?.result ?? null, reason: st?.reason ?? null, stepStartedAt: st?.stepStartedAt ?? null });
        note('update state', m.states.at(-1));
      }
      const s = await supStatus();
      if ((s !== null) !== lastSup) {
        lastSup = s !== null;
        m.supervisor.push({ t: Date.now() - t0, ms: Date.now(), answering: lastSup });
        if (!lastSup && !m.savedAtStop) m.savedAtStop = readJson(join(DATA, 'state.json'));
        note(lastSup ? 'supervisor answers' : 'supervisor not answering');
      }
      if (s?.playing) m.playing.push({ ms: Date.now(), id: s.playing.id, index: s.playing.index, position: s.playing.position, source: s.playing.source });
      const up = await nodeAnswers();
      if (up !== lastNode) {
        lastNode = up;
        m.node.push({ t: Date.now() - t0, answering: up });
        note(up ? 'Node answers' : 'Node not answering');
      }
      await sleep(1000);
    }
  })();
  return m;
}
async function endMonitor(m) {
  m.stop = true;
  await m.done;
  if (monitor === m) monitor = null;
  // Step lengths from the file's own start times.
  const steps = m.states.filter((s) => s.stepStartedAt).map((s) => ({ step: s.step, result: s.result, startedT: Date.parse(s.stepStartedAt) - m.t0 }));
  return { states: m.states, steps, supervisor: m.supervisor.map(({ t, answering }) => ({ t, answering })), node: m.node, savedAtStop: m.savedAtStop };
}

function supEvents() {
  const dir = join(DATA, 'logs');
  if (!existsSync(dir)) return [];
  return dailyFiles(readdirSync(dir), EVENTS, 'jsonl')
    .flatMap((n) => readFileSync(join(dir, n), 'utf8').split(/\r?\n/).filter(Boolean))
    .map((l) => { try { const e = JSON.parse(l); return { ...e, ms: Date.parse(e.ts) }; } catch { return null; } }).filter(Boolean);
}

/** The video and position on air when the supervisor went down, against the new one's resume event. */
function resumeCheck(m) {
  const gone = m.supervisor.find((x) => !x.answering);
  if (!gone) return { ok: false, why: 'the supervisor never stopped answering' };
  const last = m.playing.filter((p) => p.ms < gone.ms && p.source !== 'slate').at(-1) ?? null;
  const ev = supEvents().find((e) => e.ms >= gone.ms && e.kind === 'resume') ?? null;
  const posDiffS = last && ev ? Math.round(Math.abs(ev.pos - last.position) * 10) / 10 : null;
  return {
    lastPlaying: last && { id: last.id, index: last.index, position: last.position, beforeGoneMs: gone.ms - last.ms },
    savedAtStop: m.savedAtStop,
    resume: ev && { at: ev.ts, index: ev.index, id: ev.id, pos: ev.pos },
    posDiffS,
    ok: Boolean(last && ev && ev.id === last.id && posDiffS <= 15),
  };
}

/** The longest stretch with no packet at the reader, from shortly before `t0`. */
function offAir(t0) {
  const win = packets.filter((p) => p.ms >= t0 - 2000);
  let best = null;
  for (let i = 1; i < win.length; i += 1) {
    const gap = win[i].ms - win[i - 1].ms;
    if (!best || gap > best.offAirMs) best = { offAirMs: gap, lastPacketT: win[i - 1].ms - t0, firstPacketBackT: win[i].ms - t0 };
  }
  const tail = win.length ? Date.now() - win.at(-1).ms : null;
  return { ...best, packets: win.length, readerReconnects: reconnects.filter((t) => t > t0).length, sinceLastPacketMs: tail };
}

async function checkUpdate() {
  const c = await api('POST', '/update/check');
  const o = c.json ?? {};
  return { status: c.status, kind: o.kind, parts: o.parts, latestVersion: o.latestVersion, currentVersion: o.currentVersion, sameVersion: o.sameVersion, updateAvailable: o.updateAvailable, error: o.error, nodeNotice: o.nodeNotice, text: c.json ? updateOfferText(o) : null };
}

/** Checks for the update, records the install, presses Update and starts watching. */
async function apply(r) {
  if (interrupted()) throw new Stop(`interrupted by ${interrupted()}`);
  r.offer = await checkUpdate();
  note('offer', r.offer);
  writeFileSync(MARKER, randomBytes(16).toString('hex'));
  r.before = await snapshot();
  const t0 = Date.now();
  const mon = startMonitor(t0);
  r.applyStatus = (await api('POST', '/update/apply')).status;
  return { t0, mon };
}

/** The dashboard's banner and stream line from the API, through the dashboard's own functions. */
async function dashboard() {
  const u = await getJson('/update/status');
  const s = await getJson('/status');
  return { banner: u ? updateBannerView(u, Date.now()) : null, stream: s?.stream ?? null, progress: u?.progress ?? null, swapLog: u?.swapLog ?? null, currentVersion: u?.currentVersion ?? null, updateAvailable: u?.updateAvailable ?? null };
}

function evidence(r, t0, runStart) {
  r.swapLog = (readText(join(DATA, 'update-swap.log')) ?? '').split(/\r?\n/).filter(Boolean);
  r.state = readJson(STATE_FILE);
  r.assets = assetLog.filter((a) => a.ms >= runStart).map((a) => ({ t: a.ms - t0, path: a.path, status: a.status, bytes: a.bytes }));
  r.posts = postsSince(runStart).map((a) => ({ t: a.ms - t0, title: a.title, footer: a.footer, messages: a.messages }));
}
const fetched = (r, file) => r.assets.some((a) => a.status === 200 && a.path.endsWith(`/${file}`));

// the runs

/** Install N, go live, take N+1 with a new supervisor. */
async function fullRun(r, autoStart) {
  const runStart = Date.now();
  const env = { STREAMLOOP_TEST_RELEASES: writeReleases(r.run, ['n', 'n1']) };
  await goLive(r, zipOf('n'), env, 'n');
  r.testReleasesWarned = nodeLog().includes('STREAMLOOP_TEST_RELEASES is set');
  if (!autoStart) r.settingSave = (await api('POST', '/config', { autoStartStream: false })).status;
  const { t0, mon } = await apply(r);
  const want = autoStart ? 'done' : 'start';
  const out = await waitFor(`the update to reach ${want}`, async () => {
    const st = readJson(STATE_FILE);
    if (st && ['failed', 'rolled-back'].includes(st.result)) return { bad: st };
    if (st?.result !== 'ok' || st.step !== want) return null;
    const s = await getJson('/status');
    if (!autoStart && s?.stream?.reason !== READY) return null;
    return { st };
  }, 600_000, 2000);
  r.outcomeT = out.value ? Date.now() - t0 : null;
  if (!out.value || out.value.bad) {
    r.monitor = await endMonitor(mon);
    evidence(r, t0, runStart);
    throw new Stop(`the update did not reach ${want}: ${JSON.stringify(out.value?.bad ?? null)}`);
  }
  await nodeUp('after the update');
  r.dashboardAtResult = await dashboard();
  if (!autoStart) {
    const samples = [];
    for (let i = 0; i < 6; i += 1) {
      await sleep(5000);
      samples.push({ supervisors: (await supervisors()).length, stream: (await getJson('/status'))?.stream ?? null });
    }
    r.held = samples;
    const tStart = Date.now();
    r.start = (await api('POST', '/stream/start')).status;
    const live = await waitLive('live after Start');
    r.startToLiveMs = live.ms;
    r.offAirUntilStartT = tStart - t0;
  } else {
    r.liveAfterUpdate = (await waitLive('live after the update', 120_000)).ms;
  }
  await sleep(20_000);
  r.readerAfter = gapStats(Date.now() - 20_000, Date.now());
  r.monitor = await endMonitor(mon);
  r.after = await snapshot();
  r.offAir = offAir(t0);
  r.resume = resumeCheck(mon);
  evidence(r, t0, runStart);
  r.dashboardAfter = await dashboard();
  const n1 = componentsOf('n1').parts;
  r.checks = {
    testReleasesWarned: r.testReleasesWarned,
    offeredFullSupervisor: r.offer.kind === 'full' && same(r.offer.parts, ['supervisor']) && r.offer.latestVersion === N1 && r.offer.sameVersion === false
      && r.offer.text === 'Full update: the stream goes off air while these parts are replaced: Supervisor.',
    applied: r.applyStatus === 200,
    downloadedFullZip: fetched(r, zipName(N1, false)) && !fetched(r, zipName(N1, true)),
    result: r.state?.result === 'ok' && r.state?.step === want,
    nowN1: r.after.version === N1,
    partsAreN1: same(r.after.parts, n1),
    onlySupervisorChanged: same(changedParts(r.before.parts, r.after.parts), ['supervisor']),
    ytdlpExeKept: r.before.ytdlpExe === r.after.ytdlpExe,
    configUnchanged: r.before.configSha256 === r.after.configSha256,
    keyUnchanged: r.before.keyProtectedSha256 === r.after.keyProtectedSha256,
    dataKept: r.after.marker === r.before.marker && r.before.cache.every((f) => r.after.cache.includes(f)) && r.after.data.includes('state.json'),
    noLeftovers: leftovers(r.after).length === 0,
    resumedAtSavedPosition: r.resume.ok,
    readerSteadyAfter: r.readerAfter.maxWallGapMs < 2000 && r.readerAfter.readerReconnects === 0 && r.readerAfter.packets > 0,
    updatingPosted: posted(runStart, UPDATING),
    bannerSaysUpdated: r.dashboardAtResult.banner?.text === `Updated to ${N1}. Ready to stream.`,
  };
  if (autoStart) {
    r.checks.backOnAirByItself = r.liveAfterUpdate !== null && r.state?.step === 'done';
    r.checks.backOnAirPosted = posted(runStart, BACK);
  } else {
    r.checks.settingSaved = r.settingSave === 200 && r.before.autoStartStream === false;
    r.checks.heldReady = r.held.every((x) => x.supervisors === 0 && x.stream?.state === 'stopped' && x.stream?.reason === READY);
    r.checks.startWentLive = r.start === 200 && r.startToLiveMs !== null;
  }
}

/** N+1 with N's parts: the update ZIP only, the same supervisor streams on. */
async function appOnly(r) {
  const runStart = Date.now();
  const env = { STREAMLOOP_TEST_RELEASES: writeReleases(r.run, ['n', 'n1-app']) };
  await goLive(r, zipOf('n'), env, 'n');
  const { t0, mon } = await apply(r);
  const back = await waitFor('Node on 3.0.7', async () => ((await getJson('/update/status'))?.currentVersion === N1 ? true : null), 300_000, 1000);
  if (!back.value) {
    r.monitor = await endMonitor(mon);
    evidence(r, t0, runStart);
    throw new Stop('Node did not come back on 3.0.7');
  }
  r.nodeBackT = Date.now() - t0;
  await nodeUp('after the app update');
  r.attachMs = (await waitFor('the launcher to attach', async () => (await getJson('/status'))?.launcher === 'running', 60_000, 500)).ms;
  await sleep(30_000);
  r.monitor = await endMonitor(mon);
  r.after = await snapshot();
  r.gap = gapStats(t0 - 5000, Date.now());
  evidence(r, t0, runStart);
  r.dashboardAfter = await dashboard();
  r.consoleSaysApplied = (readText(join(OUT_ROOT, r.run, 'console-n.log')) ?? '').includes('Update applied successfully.');
  r.checks = {
    offeredAppOnly: r.offer.kind === 'app' && same(r.offer.parts, []) && r.offer.latestVersion === N1 && r.offer.updateAvailable === true,
    applied: r.applyStatus === 200,
    downloadedUpdateZip: fetched(r, zipName(N1, true)) && !fetched(r, zipName(N1, false)),
    nowN1: r.after.version === N1,
    consoleSaysApplied: r.consoleSaysApplied,
    sameSupervisorPid: r.before.supervisorPids.length === 1 && same(r.before.supervisorPids, r.after.supervisorPids),
    sameEncoderGeneration: r.before.encoderGeneration !== null && r.before.encoderGeneration === r.after.encoderGeneration,
    noDrop: r.gap.maxWallGapMs < 2000 && r.gap.readerReconnects === 0,
    partsUnchanged: same(r.before.parts, r.after.parts),
    configUnchanged: r.before.configSha256 === r.after.configSha256,
    noFullUpdateState: r.state === null && !r.monitor.states.some((s) => s.step),
    nothingPosted: !posted(runStart, UPDATING),
    noLeftovers: leftovers(r.after).length === 0,
  };
}

/** A rollback run: install N, go live, take a failing N+1, end on N with the reason shown. */
async function rollbackRun(r, id, setup) {
  const runStart = Date.now();
  const env = { STREAMLOOP_TEST_RELEASES: writeReleases(r.run, ['n', id]) };
  await goLive(r, zipOf('n'), env, 'n');
  await setup?.();
  const { t0, mon } = await apply(r);
  const out = await waitFor('the rollback to 3.0.6', async () => {
    const st = readJson(STATE_FILE);
    if (st?.result === 'ok') return { bad: st };
    if (st?.result !== 'rolled-back') return null;
    return (await getJson('/update/status'))?.currentVersion === N ? { st } : null;
  }, 600_000, 2000);
  if (!out.value || out.value.bad) {
    r.monitor = await endMonitor(mon);
    evidence(r, t0, runStart);
    throw new Stop(`no rollback to ${N}: ${JSON.stringify(out.value?.bad ?? null)}`);
  }
  r.rolledBackT = Date.now() - t0;
  await nodeUp('after the rollback');
  r.dashboardAtResult = await dashboard();
  r.liveAgainMs = (await waitLive('live again on 3.0.6', 180_000)).ms;
  r.liveAgainT = r.liveAgainMs === null ? null : Date.now() - t0;
  await sleep(20_000);
  r.readerAfter = gapStats(Date.now() - 20_000, Date.now());
  r.monitor = await endMonitor(mon);
  r.after = await snapshot();
  r.offAir = offAir(t0);
  evidence(r, t0, runStart);
  r.failureText = r.state ? failureText(r.state) : null;
  r.dashboardAfter = await dashboard();
  return {
    applied: r.applyStatus === 200,
    downloadedFullZip: fetched(r, zipName(N1, false)),
    rolledBack: r.state?.result === 'rolled-back',
    backOnN: r.after.version === N,
    partsAreN: same(r.after.parts, r.before.parts) && same(r.after.parts, componentsOf('n').parts),
    configUnchanged: r.before.configSha256 === r.after.configSha256,
    keyUnchanged: r.before.keyProtectedSha256 === r.after.keyProtectedSha256,
    dataKept: r.after.marker === r.before.marker && r.before.cache.every((f) => r.after.cache.includes(f)),
    noLeftovers: leftovers(r.after).length === 0,
    reasonShown: r.dashboardAtResult.banner?.visible === true && r.dashboardAtResult.banner?.text === r.failureText,
    failurePosted: r.failureText !== null && posted(runStart, r.failureText),
    liveAgain: r.liveAgainMs !== null && r.readerAfter.packets > 0 && r.readerAfter.maxWallGapMs < 2000,
  };
}

/** ffmpeg\ held as the working directory of a program from outside the install. */
async function locked(r) {
  const checks = await rollbackRun(r, 'n1-locked', async () => {
    holder = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1 << 30)'], { cwd: join(ROOT, 'ffmpeg'), stdio: 'ignore' });
    await sleep(1000);
    r.holder = { pid: holder.pid, exe: process.execPath, cwd: 'ffmpeg\\', runningAtApply: holder.exitCode === null };
  });
  r.holder.runningAtRollback = holder?.exitCode === null;
  r.checks = {
    holderRanThrough: r.holder.runningAtApply && r.holder.runningAtRollback,
    offeredFull: r.offer.kind === 'full' && same(r.offer.parts, ['supervisor', 'ffmpeg']) && r.offer.latestVersion === N1,
    ...checks,
    failedAtSwap: r.state?.step === 'swap' && r.state?.reason === 'ffmpeg: the folder stayed in use after 3 tries',
    swapLogSaysWhy: r.swapLog.includes('ffmpeg failed the folder stayed in use after 3 tries') && r.swapLog.includes('supervisor restored the previous version'),
  };
}

/** The supervisor exe is ffmpeg, so the new version's check fails. */
async function broken(r) {
  const checks = await rollbackRun(r, 'n1-broken');
  r.checks = {
    offeredFull: r.offer.kind === 'full' && same(r.offer.parts, ['supervisor']) && r.offer.latestVersion === N1,
    ...checks,
    swapped: r.swapLog.includes('supervisor ok swapped') && r.swapLog.includes('app ok swapped'),
    checkRan: r.monitor.states.some((s) => s.step === 'check' && s.result === 'running'),
    failedAtCheck: r.state?.step === 'check' && String(r.state?.reason).startsWith('The new supervisor failed its check:'),
    restored: r.swapLog.includes('supervisor restored the previous version') && r.swapLog.includes('app restored the previous version'),
  };
}

/** v3.0.5 from its GitHub release, read-only, checked against its .sha256 asset. */
async function bootZip(r) {
  const dir = join(RESULTS, 'v305-zip');
  mkdirSync(dir, { recursive: true });
  const zip = join(dir, `streamloop-${BOOT_TAG}.zip`);
  const shaText = await (await fetch(`${BOOT_URL}.sha256`, { signal: AbortSignal.timeout(30_000) })).text();
  const want = shaText.trim().split(/\s+/)[0].toLowerCase();
  if (!existsSync(zip) || sha256File(zip) !== want) {
    const res = await fetch(`${BOOT_URL}.zip`, { signal: AbortSignal.timeout(600_000) });
    writeFileSync(zip, Buffer.from(await res.arrayBuffer()));
  }
  const got = sha256File(zip);
  r.v305 = { url: `${BOOT_URL}.zip`, bytes: statSync(zip).size, sha256: got, expected: want, match: got === want };
  if (!r.v305.match) throw new Stop('v3.0.5 checksum mismatch');
  return zip;
}

/** 3.0.5 takes N+1's update ZIP as its own updater stages it, then N+1 brings its parts up to date. */
async function bootstrap(r) {
  const runStart = Date.now();
  const zip = await bootZip(r);
  const env = { STREAMLOOP_TEST_HOOKS: '1', STREAMLOOP_TEST_RELEASES: writeReleases(r.run, ['n', 'n1']) };
  await goLive(r, zip, env, '305');
  const a = { versionBefore: (await getJson('/update/status'))?.currentVersion ?? null, partsBefore: await hashParts(ROOT) };
  // Staged as 3.0.5's downloadAndApply does: extract into _update_tmp\extracted, then its stageUpdate.
  const tmp = join(ROOT, '_update_tmp');
  rmSync(tmp, { recursive: true, force: true });
  await extract(zipOf('n1', true), join(tmp, 'extracted'));
  a.stage = await tsxEval(ROOT, "import(process.env.MOD).then((m) => { m.stageUpdate(process.env.X, process.env.T); console.log('staged'); process.exit(0); })",
    { X: join(tmp, 'extracted'), T: tmp, MOD: pathToFileURL(join(ROOT, 'app', 'src', 'server', 'updater.ts')).href });
  a.staged = { app: existsSync(join(tmp, 'app', 'src', 'server', 'index.ts')), startBat: existsSync(join(tmp, 'START.bat')), parts: ['node', 'supervisor', 'ffmpeg', 'pot', 'yt-dlp'].filter((f) => existsSync(join(tmp, f))) };
  const pidsBefore = await supervisors();
  const tHook = Date.now();
  a.hook = (await api('POST', '/test/exit-75')).status;
  await waitFor('3.0.5 Node gone', async () => !(await nodeAnswers()), 30_000, 250);
  const back = await waitFor('Node on 3.0.7', async () => ((await getJson('/update/status'))?.currentVersion === N1 ? true : null), 180_000, 1000);
  if (!back.value) throw new Stop('3.0.5 did not come back as 3.0.7 after its app-only update');
  a.nodeBackMs = Date.now() - tHook;
  await nodeUp('3.0.7 after the app-only update');
  a.attachMs = (await waitFor('the launcher to attach', async () => (await getJson('/status'))?.launcher === 'running', 60_000, 500)).ms;
  await sleep(15_000);
  a.supervisorPids = { before: pidsBefore, after: await supervisors() };
  a.gap = gapStats(tHook - 5000, Date.now());
  const rootFiles = readdirSync(ROOT);
  a.launcher = {
    startBatIsN1: sha256File(join(ROOT, 'START.bat')) === sha256File(join(REPO, 'scripts', 'START.bat')),
    startBatNewGone: !rootFiles.includes('START.bat.new'), trampolineGone: !rootFiles.includes('_apply_launcher.bat'),
    consoleSaysTrampoline: (readText(join(OUT_ROOT, r.run, 'console-305.log')) ?? '').includes('Applying new launcher and restarting...'),
  };
  r.appStep = a;
  note('3.0.5 app-only update to 3.0.7', a);

  const { t0, mon } = await apply(r);
  const out = await waitFor('the parts update to finish', async () => {
    const st = readJson(STATE_FILE);
    if (st && ['failed', 'rolled-back'].includes(st.result)) return { bad: st };
    return st?.result === 'ok' && st.step === 'done' ? { st } : null;
  }, 600_000, 2000);
  if (!out.value || out.value.bad) {
    r.monitor = await endMonitor(mon);
    evidence(r, t0, runStart);
    throw new Stop(`the parts update did not finish: ${JSON.stringify(out.value?.bad ?? null)}`);
  }
  await nodeUp('after the parts update');
  r.dashboardAtResult = await dashboard();
  r.liveAfterUpdate = (await waitLive('live after the parts update', 120_000)).ms;
  await sleep(20_000);
  r.readerAfter = gapStats(Date.now() - 20_000, Date.now());
  r.monitor = await endMonitor(mon);
  r.after = await snapshot();
  r.offAir = offAir(t0);
  r.resume = resumeCheck(mon);
  evidence(r, t0, runStart);
  r.dashboardAfter = await dashboard();
  const n1 = componentsOf('n1').parts;
  r.checks = {
    v305Verified: r.v305.match,
    startedOn305: a.versionBefore === '3.0.5',
    stagedAppOnly: a.stage.stdout.includes('staged') && a.staged.app && a.staged.startBat && a.staged.parts.length === 0,
    appOnlyTo307: a.hook === 200 && back.value === true,
    launcherReplacedThroughTrampoline: a.launcher.startBatIsN1 && a.launcher.startBatNewGone && a.launcher.trampolineGone,
    sameSupervisorThroughAppStep: a.supervisorPids.before.length === 1 && same(a.supervisorPids.before, a.supervisorPids.after),
    noDropThroughAppStep: a.gap.maxWallGapMs < 2000 && a.gap.readerReconnects === 0,
    offeredUpdateParts: r.offer.kind === 'full' && r.offer.sameVersion === true && r.offer.latestVersion === N1 && r.offer.text === `Update parts to ${N1}`
      && same(r.offer.parts, changedParts(a.partsBefore, n1)),
    applied: r.applyStatus === 200,
    downloadedFullZip: fetched(r, zipName(N1, false)),
    result: r.state?.result === 'ok' && r.state?.step === 'done',
    partsAreN1: same(r.after.parts, n1),
    ytdlpExeKept: r.before.ytdlpExe === r.after.ytdlpExe,
    keyUnchanged: r.before.keyProtectedSha256 === r.after.keyProtectedSha256,
    configUnchanged: r.before.configSha256 === r.after.configSha256,
    dataKept: r.after.marker === r.before.marker && r.before.cache.every((f) => r.after.cache.includes(f)),
    noLeftovers: leftovers(r.after).length === 0,
    resumedAtSavedPosition: r.resume.ok,
    backOnAirByItself: r.liveAfterUpdate !== null,
    readerSteadyAfter: r.readerAfter.maxWallGapMs < 2000 && r.readerAfter.readerReconnects === 0 && r.readerAfter.packets > 0,
    updatingPosted: posted(runStart, UPDATING),
    backOnAirPosted: posted(runStart, BACK),
  };
}

const RUNS = {
  'full-on': (r) => fullRun(r, true),
  'full-off': (r) => fullRun(r, false),
  'app-only': appOnly,
  locked,
  broken,
  bootstrap,
};

// session

async function portHolder(port) {
  const r = await run('powershell', ['-NoProfile', '-Command',
    `$c = Get-NetTCPConnection -LocalPort ${port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1; if ($c) { Get-CimInstance Win32_Process -Filter "ProcessId=$($c.OwningProcess)" | Select-Object ProcessId,Name,CommandLine | ConvertTo-Json -Compress }`],
    { encoding: 'utf8' });
  try { return JSON.parse(r.stdout); } catch { return { raw: r.stdout.trim() }; }
}
/** Ports of the install itself; held by something else, the run cannot be judged. */
async function portsTaken() {
  const taken = [];
  for (const port of [7654, 4417, 4416, 4418]) if (await portOpen(port)) taken.push({ port, holder: await portHolder(port) });
  return taken;
}

/** The held install stays up until a `continue` file appears in the run's results folder. */
const HOLD = (process.env.UPDATE_SESSION_HOLD ?? '').split(',').filter(Boolean);
async function holdForLook(out) {
  note(`holding for a look at http://127.0.0.1:7654/admin; create ${join(out, 'continue')} to go on`);
  await waitFor('the continue file', async () => existsSync(join(out, 'continue')), 30 * 60_000, 2000);
}

/** Ends a run's install once, whoever asks first: an API stop unless an interrupt wants speed, then a kill
 *  of what still runs, the evidence copied and grepped for the key, and the folder removed. */
function endRun(r, fast = false) {
  r.ending ??= finishRun(r, fast);
  return r.ending;
}
async function finishRun(r, fast) {
  stopReader();
  if (monitor) {
    const ended = await endMonitor(monitor);
    r.monitor ??= ended;
  }
  const c = { fast };
  if (!fast) {
    try {
      if (await nodeAnswers()) {
        await nodeUp('cleanup', 10_000).catch(() => null);
        c.stop = (await api('POST', '/stream/stop')).status;
        c.supervisorExitMs = (await waitFor('the supervisor to exit', async () => (await supervisors()).length === 0, 60_000, 1000)).ms;
        c.shutdown = (await api('POST', '/shutdown')).status;
        c.nodeExitMs = (await waitFor('Node to exit', async () => !(await nodeAnswers()), 30_000, 500)).ms;
        // A clean exit 0 ends Node's processes and the hidden START.bat by themselves.
        c.nodeProcessesGoneMs = (await waitFor('Node processes to end', async () => !(await underInstall()).some((p) => p.Name === 'node.exe'), 20_000, 1000)).ms;
        if (bat && bat.exitCode === null) await waitFor('START.bat to end', async () => bat.exitCode !== null, 15_000, 500);
      }
    } catch (err) { c.error = String(err); }
  }
  const out = join(OUT_ROOT, r.run);
  c.batExitCode = bat?.exitCode ?? null;
  c.consoleSaysStoppedCleanly = readdirSync(out).filter((f) => f.startsWith('console-'))
    .some((f) => (readText(join(out, f)) ?? '').includes('Server stopped cleanly.'));
  if (holder) {
    holder.kill();
    holder = null;
  }
  if (bat && bat.exitCode === null) await run('taskkill', ['/F', '/T', '/PID', String(bat.pid)]);
  bat = null;
  // A trampoline's new START.bat window and anything else still running from the install.
  c.killed = await killAll(await underInstall());
  await sleep(2000);
  for (const [from, to] of [[join(DATA, 'logs'), 'supervisor-logs'], [join(ROOT, 'app', 'logs'), 'node-logs']]) {
    try { if (existsSync(from)) cpSync(from, join(out, to), { recursive: true }); } catch (err) { note(`could not copy ${from}`, { error: String(err) }); }
  }
  for (const f of ['update-state.json', 'update-swap.log', 'state.json']) {
    try { if (existsSync(join(DATA, f))) copyFileSync(join(DATA, f), join(out, f)); } catch { /* evidence only */ }
  }
  // The install holds the update's own writers (update-state.json, update-swap.log, the check's reason);
  // the output folder holds the requests, the console logs and the Discord posts.
  const grep = { install: grepTree(S, KEY), output: grepTree(out, KEY) };
  c.keyGrep = { installFiles: grep.install.files, outputFiles: grep.output.files, hits: [...grep.install.hits.map((h) => `install: ${h}`), ...grep.output.hits.map((h) => `output: ${h}`)] };
  if (!fast) {
    try { growSeed(); } catch (err) { note('could not keep the cache', { error: String(err) }); }
  }
  try { rmSync(S, { recursive: true, force: true, maxRetries: 5, retryDelay: 1000 }); } catch (err) { c.removeError = String(err); }
  c.removed = !existsSync(S);
  c.stillRunning = (await underInstall()).map((p) => `${p.Name} ${p.ProcessId}`);
  c.ok = c.removed && c.stillRunning.length === 0 && c.keyGrep.hits.length === 0;
  r.cleanup = c;
  note('cleanup', c);
}

function judge(r) {
  r.failedChecks = Object.entries(r.checks ?? {}).filter(([, v]) => !v).map(([k]) => k);
  r.pass = !r.failed && r.checks !== undefined && r.failedChecks.length === 0 && r.cleanup?.ok === true;
  r.endedAt = iso();
  const { ending, ...plain } = r;
  writeFileSync(join(OUT_ROOT, r.run, 'results.json'), redact(JSON.stringify(plain, null, 2), KEY));
  note(`${r.run}: ${r.pass ? 'PASS' : 'FAIL'}`, { failedChecks: r.failedChecks, failed: r.failed?.split('\n')[0], offAirMs: r.offAir?.offAirMs ?? null, cleanup: r.cleanup?.ok ?? null });
  summary.runs[r.run] = { pass: r.pass, blocked: r.blocked ?? null, failedChecks: r.failedChecks, failed: r.failed?.split('\n')[0] ?? null, offAirMs: r.offAir?.offAirMs ?? null };
}

/** The run in progress, for the interrupt teardown. */
let current = null;
/** Where the catcher writes the Discord posts: the run in progress, else the session's folder. */
let runOut = join(OUT_ROOT, 'session');

async function runOne(name) {
  const out = join(OUT_ROOT, name);
  useOut(out);
  runOut = out;
  resetReader();
  const r = { run: name, startedAt: iso() };
  const taken = await portsTaken();
  if (taken.length) {
    r.blocked = taken;
    note(`${name}: BLOCKED, a port is taken by a process this session did not start`, taken);
    writeFileSync(join(out, 'results.json'), JSON.stringify(r, null, 2));
    summary.runs[name] = { pass: false, blocked: taken };
    return r;
  }
  current = r;
  try {
    await RUNS[name](r);
  } catch (err) {
    r.failed = String(err?.stack ?? err);
    note('run stopped', { error: r.failed });
  }
  if (HOLD.includes(name) && !interrupted()) await holdForLook(out);
  // A run that threw after Apply still has its monitor going.
  if (monitor) {
    const ended = await endMonitor(monitor);
    r.monitor ??= ended;
  }
  await endRun(r);
  current = null;
  judge(r);
  return r;
}

let mediamtx = null;
let sessionEnd = null;
/** Stops the reader, MediaMTX, the release host and the catcher, then fails the session on anything left. */
function endSession() {
  sessionEnd ??= (async () => {
    useOut(join(OUT_ROOT, 'session-end'));
    runOut = join(OUT_ROOT, 'session-end');
    stopReader();
    mediamtx?.kill();
    assetServer.close();
    catcher.close();
    await sleep(3000);
    const all = await allProcesses();
    const watch = ['streamloop-supervisor.exe', 'ffmpeg.exe', 'ffprobe.exe', 'yt-dlp.exe', 'deno.exe', 'mediamtx.exe', 'tail.exe'];
    summary.tasklist = Object.fromEntries(watch.map((n) => [n, all.filter((p) => String(p.Name).toLowerCase() === n).length]));
    const tools = join(TOOLS, 'mediamtx.exe').toLowerCase();
    summary.leftovers = {
      underInstall: under(all, [S], /\\sl3 update /i).map((p) => `${p.Name} ${p.ProcessId}`),
      mediamtx: all.filter((p) => p.Name === 'mediamtx.exe' && String(p.ExecutablePath).toLowerCase() === tools).map((p) => p.ProcessId),
      readers: all.filter((p) => p.Name === 'ffprobe.exe' && (p.CommandLine ?? '').includes(READ_URL)).map((p) => p.ProcessId),
    };
    summary.installRemoved = !existsSync(S);
    summary.clean = summary.installRemoved && Object.values(summary.leftovers).every((l) => l.length === 0);
    summary.interrupted = interrupted();
    writeFileSync(join(OUT_ROOT, 'summary.json'), JSON.stringify(summary, null, 2));
    for (const [name, x] of Object.entries(summary.runs)) console.log(`${name}: ${x.blocked ? 'BLOCKED' : x.pass ? 'PASS' : 'FAIL'}${x.offAirMs ? ` (off air ${(x.offAirMs / 1000).toFixed(1)} s)` : ''}${x.failedChecks?.length ? ` failed: ${x.failedChecks.join(', ')}` : ''}${x.failed ? ` stopped: ${x.failed}` : ''}`);
    note('session end', { clean: summary.clean, leftovers: summary.leftovers, installRemoved: summary.installRemoved, tasklist: summary.tasklist });
    return summary.clean;
  })();
  return sessionEnd;
}

const arg = process.argv[2] ?? 'all';
const names = arg === 'all' ? Object.keys(RUNS) : arg.split(',');
const unknown = arg === 'build' ? [] : names.filter((n) => !(n in RUNS));
mkdirSync(OUT_ROOT, { recursive: true });
useOut(join(OUT_ROOT, 'session'));
if (unknown.length) { note(`unknown run: ${unknown.join(', ')}; runs: ${Object.keys(RUNS).join(', ')}, all, build`); process.exit(2); }
for (const f of [DIST_FULL, DIST_SLIM]) if (!existsSync(f)) { note(`missing ${f}: npm run build:release and build:release:slim first`); process.exit(2); }
if (arg === 'build') {
  await buildReleases();
  process.exit(0);
}
const swept = await sweepLeftovers({ dirs: [S], ascii: /\\sl3 update /i, mediamtx: join(TOOLS, 'mediamtx.exe') });
if (swept.owner) { note('BLOCKED: the install folder belongs to a harness that is still running', swept); process.exit(3); }
if (swept.found.length || swept.removed.length) note('swept what an earlier run left', swept);
const taken = await portsTaken();
for (const port of [ASSET_PORT, CATCHER_PORT, 1935, 8888, 8554, 8889]) if (await portOpen(port)) taken.push({ port, holder: await portHolder(port) });
if (taken.length) { note('BLOCKED: ports taken by processes this session did not start', taken); process.exit(3); }
note('preflight: 7654, 4417, 4416, 4418, MediaMTX, release host and catcher ports free');

const summary = { at: iso(), dist: { full: basename(DIST_FULL), slim: basename(DIST_SLIM) }, swept, runs: {} };
onInterrupt(async (signal) => {
  const r = current;
  if (r) {
    r.failed ??= `interrupted by ${signal}`;
    await endRun(r, true);
    judge(r);
  }
  await endSession();
});
const manifest = await buildReleases();
summary.releases = Object.fromEntries(Object.entries(manifest.built).map(([id, b]) => [id, { version: b.version, components: b.components.parts, runs: b.runs }]));
await new Promise((resolve) => assetServer.listen(ASSET_PORT, '127.0.0.1', resolve));
await new Promise((resolve) => catcher.listen(CATCHER_PORT, '127.0.0.1', resolve));
mediamtx = spawn(join(TOOLS, 'mediamtx.exe'), [], { cwd: TOOLS, stdio: 'ignore' });
await sleep(3000);
if (!(await portOpen(1935))) { note('MediaMTX did not open :1935'); await endSession(); process.exit(2); }
note('release host, catcher and MediaMTX up', { mediamtx: mediamtx.pid });

for (const name of names) {
  const r = await runOne(name);
  if (interrupted() || r.blocked || /YouTube breaker open/.test(r.failed ?? '')) { note('stopping the session'); break; }
}
const clean = await endSession();
process.exit(names.every((n) => summary.runs[n]?.pass) && clean && !interrupted() ? 0 : 1);
