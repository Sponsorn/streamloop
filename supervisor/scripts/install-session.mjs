// The install and update proof on the dev machine: the full ZIP as a user gets it, a local release list
// and local MediaMTX.
//   node install-session.mjs <run>[,<run>...] | all | build
// Runs, each in a fresh install at %TEMP%\sl3 install Ö\streamloop that is removed at its end:
//   install       the full ZIP's START.bat, first run, setup check, key, config, live; the key in no
//                 install file; a second START.bat window exits 2 and changes nothing.
//   app           the app as a user starts it, the spec's actions: streamloop.exe started as Explorer does
//                 starts START.bat hidden and the wizard's calls take it live (A1); a second start brings its
//                 window forward (A2); START.bat by hand opens one window, none after a crash restart, and an app
//                 started in its 10 s wait starts no second START.bat (A11); autostart on writes the Run value,
//                 its command as logon runs it gives the tray only (A9, A10); StreamLoop.vbs moves to the value
//                 (A18); forged Host and Origin get 403 on 7654 and 4417, /admin 404 (A15, A16); Quit leaves nothing
//                 (A7); a start after it goes live (A8).
//   update        N live; the first check stages N+1 and it lands at the planned break (encoder uptime
//                 STREAMLOOP_TEST_BREAK_AT_S=300); then, with autoStartStream off, Update Now to N+2 on air;
//                 then N+3 stopped from the dashboard and N+4 waiting for Start, each landing without starting
//                 the stream and confirmed by `streamloop.exe check`. Changing routes are probed for 409
//                 throughout, and must be answered so by the new control process while it confirms. The app
//                 follows each landing, with its window open for N+1 and closed after.
//   rollback      N+1 whose supervisor mode exits at start: the new control process answers 409 while it
//                 confirms, rolls back within 180 s, and the next planned break leaves that version alone. The
//                 app follows onto N+1 and back to N.
//   rollback-bat  N+1 whose control mode exits at start: START.bat puts N back, N reports it in the timeline
//                 and as one critical, N replaces N+1's supervisor (version rule), and the next planned break
//                 leaves that version alone. The app stays on N.
//   move          v3.0.5 from GitHub (checksum checked) live with a dummy key, Stop, window closed, the full
//                 ZIP extracted over it: config, key and timeline kept, the 3.0 leftovers gone. Then again
//                 with the 3.0.5 supervisor left streaming: the new control process replaces it.
//   faults        the stage 5 gate's fault checks through POST /debug/fault (STREAMLOOP_DEBUG_FAULTS=1).
//   all           every run above in that order. build: only the test builds.
// N is dist\'s build (package.json's version); `npm run build:release` first, and again after a change to
// the crates, START.bat or the dashboard. N+1 to N+4 count up N's patch number, and the two broken
// N+1 builds are N+1 with the test-supervisor-fails or test-control-fails feature, each built by cargo
// into its own target\install-session\<id>\. The release list (STREAMLOOP_TEST_RELEASES) carries each
// release's ZIP, exe and exe checksum; the exe and its checksum are served on
// 127.0.0.1:8791, and the ZIP is never fetched. Discord posts go to a catcher on 127.0.0.1:8790.
// Start it from PowerShell: a job that refuses breakaway (the Bash tool's) puts the supervisor in it.
// Each run prints PASS, FAIL or SKIPPED and writes supervisor\results\install\<run>\results.json. SKIPPED means
// no check failed but one was skipped: a step that opens or activates a window waits while a full-screen app is
// in front, and after 30 min skips that step.
// Ctrl+C or Ctrl+Break tears the run down; a start after a killed run first sweeps what it left.
import { existsSync, mkdirSync, readFileSync, writeFileSync, rmSync, cpSync, readdirSync, statSync, copyFileSync, appendFileSync, renameSync, openSync, closeSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, basename } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import http from 'node:http';
import { dailyFiles, EVENTS, PLAYLIST, redact, exeMode, autostartSandbox } from './lib.mjs';
import {
  spawn, run, KEY, INGEST, READ_URL, SUP, sleep, iso, Stop, useOut, note, waitFor, startBat, nodeUp, api, nodeStatus, supStatus, waitLive,
  packets, reconnects, runReader, stopReader, resetReader, gapStats, noDrop, sha256File, hashTree, grepTree, extract, extractOver, quietConfig, portOpen,
  allProcesses, under, runningFrom, killAll, sweepLeftovers, onInterrupt, interrupted, quickStop, tempFolders, useApiPort,
  VARIANTS, exeName, ASSET_PORT, buildVariants, writeReleaseList, assetHost, TAR,
  installCensus, windowCensus, closeWindows, windowsOf, fullScreenApp, foregroundGate, runCommandParts, verdictOf, SKIPPED,
  startApp, explorerEnv, regValue, regKeyExists, realAutostart, rawRequest, token,
} from './install-kit.mjs';

const REPO = fileURLToPath(new URL('../..', import.meta.url));
// Autostart goes to a test key and a temp Startup folder, never the user's own entries.
const sandbox = autostartSandbox('install');
const VERSION = JSON.parse(readFileSync(join(REPO, 'package.json'), 'utf8')).version;
const [V1, V2, V3, V4] = ['n1', 'n2', 'n3', 'n4'].map((id) => VARIANTS[id].version);
const DIST = join(REPO, 'dist');
const DIST_ZIP = join(DIST, `streamloop-v${VERSION}.zip`);
const DIST_EXE = join(DIST, `streamloop-v${VERSION}.exe`);
const RESULTS = join(REPO, 'supervisor', 'results');
const OUT_ROOT = join(RESULTS, 'install');
const WORK = join(RESULTS, 'install-releases');
const SERVE = join(WORK, 'serve');
const V305_CACHE = join(RESULTS, 'v305-zip');
const TOOLS = join(REPO, 'spikes', 'tools');
const S = join(tmpdir(), 'sl3 install Ö');
const ROOT = join(S, 'streamloop');
const DATA = join(ROOT, 'data');
const CONFIG = join(ROOT, 'app', 'config.json');
const EXE = join(ROOT, 'streamloop.exe');
const EXE_OLD = join(ROOT, 'streamloop.exe.old');
const EXE_BAD = join(ROOT, 'streamloop.exe.bad');
const UPDATE_DIR = join(ROOT, '_update');
const STAGED = join(UPDATE_DIR, 'streamloop.exe');
const LANDED = join(UPDATE_DIR, 'landed');
const ROLLED_BACK = join(DATA, 'update-rolled-back');
const STATE_FILE = join(DATA, 'update-state.json');
const WAITING = join(DATA, 'waiting-for-start');
const STOPPED_MARKER = join(DATA, 'stopped-by-user');
const TIMELINE_DIR = join(ROOT, 'app', 'logs');
/** The control process's quit line (crates/control/src/app.rs QUIT_LINE). */
const QUIT_LINE = 'Quitting StreamLoop. The stream goes off air.';
const REFUSED = 'Forbidden: requests from a browser are refused';
/** How soon after the new control process answers the app must run the version it reports. */
const FOLLOW_WITHIN_MS = 10_000;
/** How long the control process retries removing a held streamloop.exe.old after the confirmation. */
const OLD_GONE_WITHIN_MS = 120_000;
const CATCHER_PORT = 8790;
const WEBHOOK = `http://127.0.0.1:${CATCHER_PORT}/webhook/install`;
const BREAK_AT_S = 300;
/** The confirmation window (landing.rs CONFIRM_WITHIN). */
const CONFIRM_MS = 180_000;
const V305_TAG = 'v3.0.5';
const V305_URL = `https://github.com/Sponsorn/streamloop/releases/download/${V305_TAG}/streamloop-${V305_TAG}`;
/** What the control process removes at every start (spec: Moving from 3.0). */
const LEFTOVERS_30 = ['app/src', 'app/node_modules', 'app/package.json', 'app/package-lock.json', 'app/tsconfig.json', 'app/node-version.txt',
  'supervisor/streamloop-supervisor.exe', '_update_tmp', '_update_old', 'START.bat.new', '_apply_launcher.bat'];

/** The 3.0 token server's folders; the control process removes them once supervisor\streamloop-pot.exe is installed. */
const TOKEN_FALLBACK = ['pot', 'node'];

const RUNS = { install: runInstall, app: runApp, update: runUpdate, rollback: runRollback, 'rollback-bat': runRollbackBat, move: runMove, faults: runFaults };

const readJson = (file) => { try { return JSON.parse(readFileSync(file, 'utf8').replace(/^\uFEFF/, '')); } catch { return null; } };
const readText = (file) => { try { return readFileSync(file, 'utf8'); } catch { return null; } };
const sha = (text) => createHash('sha256').update(text).digest('hex');
let apiBase = '';
async function getJson(path, timeoutMs = 3000) {
  try {
    const res = await fetch(apiBase + path, { signal: AbortSignal.timeout(timeoutMs) });
    return res.ok ? await res.json() : null;
  } catch { return null; }
}
const updStatus = () => getJson('/update/status');
const timeline = async () => (await getJson('/events', 5000)) ?? [];
const supQuick = async (timeoutMs = 1500) => { try { return await (await fetch(`${SUP}/status`, { signal: AbortSignal.timeout(timeoutMs) })).json(); } catch { return null; } };

// processes of the install

const IN_TEMP = tempFolders(['sl3 install ']);
const underS = () => runningFrom([S], IN_TEMP);
// START.bat runs streamloop.exe with no mode word, so the census reads the control process from its parent.
const mode = (p) => p.mode ?? exeMode(p.CommandLine);
/** The streaming core: `streamloop.exe supervisor`, or a 3.0 install's streamloop-supervisor.exe. */
const isCore = (p) => (p.Name === 'streamloop.exe' && mode(p) === 'supervisor')
  || (p.Name === 'streamloop-supervisor.exe' && !['launch', 'protect', 'check'].includes(mode(p)));
const isControl = (p) => p.Name === 'streamloop.exe' && mode(p) === 'control';
const isCheck = (p) => p.Name === 'streamloop.exe' && mode(p) === 'check';
const isNode30 = (p) => p.Name === 'node.exe' && /src[\\/]server[\\/]index\.ts/.test(p.CommandLine ?? '');
const cores = async () => (await underS()).filter(isCore);
const brief = (p) => ({ pid: p.ProcessId, name: p.Name, mode: mode(p) });

async function exeVersion(exe = EXE) {
  const r = await run(exe, ['--version'], { cwd: tmpdir(), encoding: 'utf8', timeout: 10_000 });
  return r.status === 0 ? r.stdout.trim() : `exit ${r.status}`;
}

let BUILT = {};

/** The list a run's control process reads, as releases-<name>.json in the run's work folder. */
const writeReleases = (name, ids) => writeReleaseList(join(WORK, `releases-${name}.json`), ids);

// the local release host and the Discord catcher

const assetLog = [];
const assetServer = assetHost(SERVE, assetLog);
const fetchedSince = (ms) => assetLog.filter((a) => a.ms >= ms).map((a) => `${a.status} ${a.path}`);

const alerts = [];
let runOut = join(OUT_ROOT, 'session');
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
const messagesSince = (ms, to = Infinity) => alerts.filter((a) => a.ms >= ms && a.ms <= to).flatMap((a) => a.messages);
const criticalsSince = (ms) => messagesSince(ms).filter((m) => m.startsWith('**CRITICAL:**'));

// the install

let bat = null;
let secondBat = null;
let API_PORT = 7654;

/** Extracts the full ZIP, starts its START.bat hidden and takes it live with the wizard's calls. */
async function install(r, { env = {}, label, updates = false }) {
  const i = r.install = { zip: basename(DIST_ZIP), bytes: statSync(DIST_ZIP).size, sha256: sha256File(DIST_ZIP) };
  i.extractMs = await extract(DIST_ZIP, S);
  KNOWN_EXES.set(sha256File(EXE), VERSION);
  i.configShipped = existsSync(CONFIG);
  quietConfig(join(ROOT, 'app', 'config.example.json'), { port: API_PORT, autoUpdateCheck: updates });
  const t0 = Date.now();
  bat = startBat(ROOT, label, env);
  i.controlUpMs = await nodeUp(`${label} first start`);
  i.configCreated = existsSync(CONFIG);
  const first = await nodeStatus();
  i.firstRun = first?.firstRun ?? null;
  i.streamBeforeWizard = first?.stream?.state ?? null;
  i.coresBeforeWizard = (await cores()).length;
  await wizard(i);
  const live = await waitLive(`live after the wizard (${label})`);
  if (!live.value) throw new Stop(`${label} did not go live after the wizard`);
  i.wizardToLiveMs = live.ms;
  i.startToLiveMs = Date.now() - t0;
  runReader();
  note('installed and live', i);
}

/** The wizard's calls in its order: config, the system check and its settings, then Finish (key first). */
async function wizard(i) {
  const cfg = (await api('GET', '/config')).json;
  const check = await api('POST', '/setup-check/run');
  i.setupCheck = { status: check.status, encoder: check.json?.encoder ?? null, cacheMb: check.json?.cache?.recommendedMb ?? null, failedDeps: check.json?.dependencies?.filter((d) => !d.ok).map((d) => d.name) ?? null };
  const picked = { cacheBudgetMb: check.json?.cache?.recommendedMb };
  if (check.json?.encoder?.works) picked.encoder = check.json.encoder.chosen;
  const patch = Object.fromEntries(Object.entries(picked).filter(([k, v]) => v !== undefined && cfg?.supervisor?.[k] !== v));
  i.setupSave = Object.keys(patch).length ? { patch, status: (await api('POST', '/config', { supervisor: patch })).status } : 'nothing differed';
  i.keyStatus = (await api('POST', '/stream-key', { key: KEY })).status;
  i.saveStatus = (await api('POST', '/config', { playlists: [{ id: PLAYLIST }], discord: { webhookUrl: WEBHOOK }, supervisor: { ingestUrl: INGEST } })).status;
}

function configFacts() {
  const text = readText(CONFIG) ?? '';
  const c = readJson(CONFIG);
  return { sha256: sha(text), playlist: c?.playlists?.[0]?.id ?? null, ingest: c?.supervisor?.ingestUrl ?? null, encoder: c?.supervisor?.encoder ?? null, keyBlobSha256: sha(c?.supervisor?.streamKeyProtected ?? ''), keySet: Boolean(c?.supervisor?.streamKeyProtected), port: c?.port ?? null };
}

/** The update files, the supervisor and control processes, the versions. */
async function snapshot() {
  const procs = await underS();
  const s = await supStatus();
  const u = await updStatus();
  return {
    exeVersion: await exeVersion(), supervisorVersion: s?.version ?? null, encoderAlive: s?.encoder?.alive ?? null, uptimeS: s?.encoder?.uptimeS ?? null,
    currentVersion: u?.currentVersion ?? null, updateStatus: u?.status ?? null, notice: u?.nodeNotice ?? null,
    cores: procs.filter(isCore).map(brief), controls: procs.filter(isControl).map(brief),
    files: fileFlags(), config: configFacts(), stream: (await nodeStatus())?.stream?.state ?? null,
  };
}
/** SHA-256 of the exe, the config and `_update\` (null when absent): what a second window must not change. */
function installFiles() {
  const tree = (p) => (existsSync(p) ? hashTree(p).sha256 : null);
  return { exe: sha256File(EXE), config: existsSync(CONFIG) ? sha256File(CONFIG) : null, update: tree(UPDATE_DIR) };
}
const fileFlags = () => ({ old: existsSync(EXE_OLD), bad: existsSync(EXE_BAD), update: existsSync(UPDATE_DIR), staged: existsSync(STAGED), landed: existsSync(LANDED), rolledBack: existsSync(ROLLED_BACK) });

// watching an update: every loop records a value only when it changes, with the time

/** SHA-256 of each exe a run can install, with its version: the installed N and the test builds. */
const KNOWN_EXES = new Map();
/** streamloop.exe's file identity (its file index) and the version of the exe behind it, hashed once per file. */
const exeVersions = new Map();
function exeIdentity() {
  let ino;
  try { ino = String(statSync(EXE, { bigint: true }).ino); } catch { return null; }
  if (!exeVersions.has(ino)) {
    // A file half way through a rename cannot be read; the next look hashes it.
    try { exeVersions.set(ino, KNOWN_EXES.get(sha256File(EXE)) ?? null); } catch { return { ino, version: null }; }
  }
  return { ino, version: exeVersions.get(ino) };
}

function startWatch() {
  const w = { t0: Date.now(), rows: [], uptimes: [], stop: false };
  const push = (kind, v) => {
    const last = w.rows.findLast((x) => x.kind === kind);
    if (!last || JSON.stringify(last.v) !== JSON.stringify(v)) w.rows.push({ t: Date.now(), kind, v });
  };
  const loop = async (fn) => { while (!w.stop) { await fn(); await sleep(250); } };
  w.done = Promise.all([
    loop(async () => {
      const st = readJson(STATE_FILE);
      push('state', st ? { step: st.step, result: st.result, to: st.to, reason: st.reason ?? null } : null);
      push('files', fileFlags());
      push('exe', exeIdentity());
    }),
    loop(async () => {
      const s = await supQuick();
      push('sup', s ? { version: s.version ?? null, state: s.state ?? null, alive: s.encoder?.alive ?? null } : null);
      if (s?.encoder?.uptimeS != null) w.uptimes.push({ t: Date.now(), s: s.encoder.uptimeS });
    }),
    loop(async () => {
      const u = await getJson('/update/status', 1500);
      push('control', u ? { status: u.status, current: u.currentVersion } : null);
    }),
  ]);
  return w;
}
async function endWatch(w) {
  if (!w) return null;
  w.stop = true;
  await w.done;
  return w;
}
/** The value of `kind` at time `t`: the last change at or before it. */
function valueAt(w, kind, t) {
  let v;
  for (const r of w.rows) { if (r.t > t) break; if (r.kind === kind) v = r.v; }
  return v;
}
/** The first time at or after `from` that `kind` satisfies `pred`, or null. */
function when(w, kind, from, pred) {
  if (pred(valueAt(w, kind, from) ?? null)) return from;
  return w.rows.find((r) => r.t > from && r.kind === kind && pred(r.v ?? null))?.t ?? null;
}
const uptimeBefore = (w, t) => w.uptimes.filter((u) => u.t <= t).at(-1)?.s ?? null;
const rel = (t, t0) => (t === null || t0 === null ? null : t - t0);

// the 409 probe: two changing routes, each with a body it refuses, so they answer 409 while an update runs
// and 400 otherwise, and change nothing either way

const PROBE_ROUTES = [
  { path: '/player/jump', body: '{}' },
  { path: '/config', body: '{"ytdlCookiesFromBrowser":0}' },
];
function startProbe() {
  const p = { rows: [], stop: false };
  let tok = '';
  let version = null;
  const post = (route) => fetch(`${apiBase}${route.path}`, { method: 'POST', headers: { 'x-api-token': tok, 'content-type': 'application/json' }, body: route.body, signal: AbortSignal.timeout(2000) });
  p.done = (async () => {
    for (let i = 0; !p.stop; i += 1) {
      const route = PROBE_ROUTES[i % PROBE_ROUTES.length];
      const before = readJson(STATE_FILE)?.result ?? null;
      let status = 0;
      try {
        let res = await post(route);
        // a new token means a new control process: note which version it is
        if (res.status === 403) {
          await res.text();
          tok = (await getJson('/token', 2000))?.token ?? '';
          version = (await getJson('/update/status', 2000))?.currentVersion ?? null;
          res = await post(route);
        }
        status = res.status;
        await res.text();
      } catch { status = 0; }
      const after = readJson(STATE_FILE)?.result ?? null;
      p.rows.push({ t: Date.now(), route: route.path, status, version, before, after });
      await sleep(250);
    }
  })();
  return p;
}
/** `by` names the version whose control process must have answered at least one probe while the update ran. */
async function endProbe(p, { by = null } = {}) {
  if (!p) return null;
  p.stop = true;
  await p.done;
  const answered = p.rows.filter((x) => x.status !== 0);
  const during = answered.filter((x) => x.before === 'running' && x.after === 'running');
  const outside = answered.filter((x) => x.before !== 'running' && x.after !== 'running');
  const count = (rows) => Object.fromEntries(PROBE_ROUTES.map((r) => [r.path, rows.filter((x) => x.route === r.path).length]));
  const byVersion = {};
  for (const x of during) byVersion[x.version] = (byVersion[x.version] ?? 0) + 1;
  return {
    probes: p.rows.length, during: during.length, during409: during.filter((x) => x.status === 409).length, duringByRoute: count(during), duringByVersion: byVersion,
    outside: outside.length, outside400: outside.filter((x) => x.status === 400).length,
    statuses: [...new Set(answered.map((x) => `${x.route} ${x.status}`))],
    ok: during.length > 0 && during.every((x) => x.status === 409) && outside.every((x) => x.status === 400)
      && PROBE_ROUTES.every((r) => during.some((x) => x.route === r.path)) && (by === null || (byVersion[by] ?? 0) > 0),
  };
}

/** Waits for the staged exe of `id` with the status idle and the offer up. */
async function waitStaged(id, timeoutMs = 240_000) {
  const v = VARIANTS[id].version;
  const t0 = Date.now();
  const got = await waitFor(`${v} staged`, async () => {
    const u = await updStatus();
    return existsSync(STAGED) && u?.status === 'idle' && u.updateAvailable && u.latestVersion === v ? u : null;
  }, timeoutMs, 1000);
  const s = await supStatus();
  return {
    ms: got.ms, status: got.value?.status ?? null, latestVersion: got.value?.latestVersion ?? null, notice: got.value?.nodeNotice ?? null,
    stagedSha256Matches: existsSync(STAGED) && sha256File(STAGED) === BUILT[id].sha256, uptimeS: s?.encoder?.uptimeS ?? null,
    fetched: fetchedSince(runStartMs),
  };
}
const fetchedOnlyTheExe = (fetched, id) => {
  const v = VARIANTS[id].version;
  return fetched.includes(`200 /${id}/${exeName(v)}`) && fetched.includes(`200 /${id}/${exeName(v)}.sha256`) && !fetched.some((f) => f.endsWith('.zip'));
};

/** One landing from its first step to the result the next control process records for it. */
async function followLanding(w, to, { from = w.t0, onAir = true, label }) {
  const started = await waitFor(`the landing of ${to} (${label})`, async () => when(w, 'state', from, (v) => v?.to === to && v.result === 'running' && v.step !== 'download'), (BREAK_AT_S + 240) * 1000, 250);
  if (started.value === null) throw new Stop(`${to} did not start landing (${label})`);
  const tStart = started.value;
  const ended = await waitFor(`the result of ${to} (${label})`, async () => when(w, 'state', tStart, (v) => v?.to === to && v.result !== 'running'), CONFIRM_MS + 240_000, 500);
  const tResult = ended.value;
  const result = tResult === null ? null : valueAt(w, 'state', tResult);
  await nodeUp(`after the landing of ${to}`, 60_000).catch(() => null);
  return { tStart, tResult, result, uptimeAtStart: uptimeBefore(w, tStart), onAir };
}

/** The checks every on-air landing shares, with the stream gap the reader saw. With `app` ({ ctx, windowOpen }),
 *  also that the app follows (`appFollowed`) and that streamloop.exe.old goes. */
async function onAirLanding(r, w, probe, to, label, before, app = null) {
  const L = await followLanding(w, to, { label });
  const tLive = await waitFor(`${to} on air (${label})`, async () => when(w, 'sup', L.tStart, (v) => v?.version === to && v.alive === true), 120_000, 500);
  const live = await waitLive(`live on ${to} (${label})`, 120_000);
  const tFilesGone = (await waitFor(`the update files gone (${label})`, async () => when(w, 'files', L.tStart, (v) => v && !v.old && !v.landed && !v.update), CONFIRM_MS, 500)).value;
  await sleep(20_000);
  const p = await endProbe(probe);
  const tStop = when(w, 'sup', L.tStart, (v) => v === null);
  const tOldGone = when(w, 'control', L.tStart, (v) => v === null);
  const tNewControl = tOldGone === null ? null : when(w, 'control', tOldGone, (v) => v?.current === to);
  const after = await snapshot();
  const gapWindow = gapStats(L.tStart - 5000, (tLive.value ?? Date.now()) + 15_000);
  const steady = gapStats(Date.now() - 15_000, Date.now());
  const tl = (await timeline()).map((e) => e.message);
  const said = messagesSince(L.tStart - 10_000);
  const x = {
    times: { uptimeAtLandingS: L.uptimeAtStart, supervisorStoppedMs: rel(tStop, L.tStart), oldControlGoneMs: rel(tOldGone, L.tStart), newControlMs: rel(tNewControl, L.tStart),
      newVersionLiveMs: rel(tLive.value, L.tStart), resultMs: rel(L.tResult, L.tStart), filesGoneMs: rel(tFilesGone, L.tStart) },
    result: L.result, gap: gapWindow, steadyAfter: steady, probe: p, after, liveMs: live.ms,
    timeline: tl.filter((m) => /StreamLoop|supervisor/i.test(m)).slice(-12), discord: said,
  };
  x.checks = {
    confirmed: L.result?.result === 'ok' && L.result?.to === to,
    versionsAreNew: after.exeVersion === to && after.supervisorVersion === to && after.currentVersion === to,
    configAndKeyKept: after.config.sha256 === before.config.sha256 && after.config.keyBlobSha256 === before.config.keyBlobSha256 && after.config.keySet,
    updateFilesGoneWithin180s: tFilesGone !== null && tFilesGone - L.tStart <= CONFIRM_MS && !after.files.old && !after.files.update && !after.files.landed,
    oneSupervisorOneControl: after.cores.length === 1 && after.cores[0].name === 'streamloop.exe' && after.controls.length === 1,
    liveAgainSteady: live.value !== null && noDrop(steady),
    changingRoutes409WhileLanding: p.ok,
    timelineSaysInstallingAndUpdated: tl.includes(`Installing StreamLoop ${to}. The stream restarts.`) && tl.includes(`Updated to StreamLoop ${to}.`),
    discordGotInstalling: said.includes(`Recovery action: **Installing StreamLoop ${to}. The stream restarts.**`),
    noCriticalAlert: !said.some((m) => m.startsWith('**CRITICAL:**')),
  };
  if (app) {
    x.app = await appFollowed(w, app.ctx, { from: tNewControl, version: to, label, windowOpen: app.windowOpen });
    x.old = oldGone(w, L.tStart, to);
    Object.assign(x.checks, x.app.checks, { oldGoneWithin2MinOfTheConfirmation: x.old.ok });
  }
  note(`${label}: landing of ${to}`, x);
  return x;
}

// the app: started as Explorer starts it, followed across updates, and the windows it shows

/** A step that opens or activates a window waits while a full-screen app is in front; false when it was skipped. */
async function gate(r, label) {
  const g = await foregroundGate({ label });
  (r.gates ??= []).push({ label, state: g.state, waitedMs: g.waitedMs, looks: g.looks, foreground: g.foreground });
  if (g.state === SKIPPED) note(`${label}: SKIPPED, a full-screen app stayed in front for 30 min`, g.foreground);
  return g.state !== SKIPPED;
}
/** A check that needs a window on screen: SKIPPED when its gate skipped the window. */
const onScreen = (shown, value) => (shown ? Boolean(value) : SKIPPED);

/** streamloop.exe started as Explorer starts it, with `env` added to the harness's environment. */
async function openApp(label, args = [], env = {}, exe = EXE) {
  const t = Date.now();
  const pid = await startApp(exe, args, explorerEnv(env));
  note(`app started (${label})`, { pid, args });
  if (pid === null) throw new Stop(`the app did not start (${label})`);
  return { pid, t };
}

/** The install's processes by role, and the shown windows they own; a console host counts as its client's. */
async function installSnapshot() {
  const all = await allProcesses();
  const inst = under(all, [S], IN_TEMP);
  const pids = inst.map((p) => p.ProcessId);
  const hosts = all.filter((p) => /^(conhost|openconsole)\.exe$/i.test(p.Name ?? '') && pids.includes(p.ParentProcessId)).map((p) => p.ProcessId);
  const roles = installCensus(inst);
  const census = await windowCensus();
  const windows = windowsOf(census, [...pids, ...hosts])
    .map((w) => ({ hwnd: w.hwnd, pid: w.pid, title: w.title, className: w.className, owner: inst.find((p) => p.ProcessId === w.pid)?.Name ?? 'console host' }));
  const fg = census.foreground;
  return {
    t: Date.now(), counts: roles.counts, roles, appPids: roles.apps.map((p) => p.ProcessId), windows,
    foreground: fg && { pid: fg.pid, name: fg.name, title: fg.title }, censusError: census.error ?? null,
  };
}
/** A snapshot as the results keep it. */
const snapBrief = (s) => ({ counts: s.counts, appPids: s.appPids, apps: s.roles.apps.map((p) => ({ pid: p.ProcessId, created: p.created, cmd: p.CommandLine })),
  startBats: s.roles.startBats.map((p) => p.ProcessId), controls: s.roles.controls.map((p) => p.ProcessId), supervisors: s.roles.supervisors.map((p) => p.ProcessId),
  windows: s.windows, foreground: s.foreground, censusError: s.censusError });
const appWindows = (snap, pid) => snap.windows.filter((w) => w.pid === pid);
/** One app, one START.bat, one control process, one supervisor. */
const oneOfEach = (s) => s.counts.apps === 1 && s.counts.startBats === 1 && s.counts.controls === 1 && s.counts.supervisors === 1;

/** POST /api/quit as the app sends it: its token in X-API-Token and no body (crates/ui/src/api/http.rs). */
async function quitAsTheApp() {
  const tok = (await getJson('/token', 2000))?.token ?? token;
  try {
    const res = await fetch(`${apiBase}/quit`, { method: 'POST', headers: { 'X-API-Token': tok }, signal: AbortSignal.timeout(10_000) });
    const text = await res.text();
    let json = null;
    try { json = JSON.parse(text); } catch { /* not json */ }
    return { status: res.status, json };
  } catch (err) { return { status: 0, json: null, error: String(err) }; }
}

/** Quit, then the app, which a quit from the harness does not reach: the app's own Quit waits for the port to close
 *  and exits, so the harness ends it once the port has closed. Answers when nothing of the install runs. */
async function quitAndEnd(label) {
  const t = Date.now();
  const quit = await quitAsTheApp();
  const closed = await waitFor(`the port to close (${label})`, async () => ((await portOpen(API_PORT)) ? null : true), 60_000, 1000);
  const tClosed = Date.now();
  const apps = installCensus(await underS()).apps.map((p) => p.ProcessId);
  for (const pid of apps) await run('taskkill', ['/F', '/PID', String(pid)]);
  const gone = await waitFor(`nothing of the install left (${label})`, async () => ((await underS()).some((p) => /^(streamloop|cmd)\.exe$/i.test(p.Name ?? '')) ? null : true), 60_000, 1000);
  const tGone = Date.now();
  await waitFor(`no process of the install (${label})`, async () => ((await underS()).length === 0 ? true : null), 30_000, 1000);
  const x = { quit: quit.status, body: quit.json, portClosedMs: closed.value ? tClosed - t : null, endedApps: apps, noStreamloopOrCmdMs: gone.value ? tGone - t : null };
  note(`quit (${label})`, x);
  return x;
}

/** The control process exits 75 (STREAMLOOP_TEST_HOOKS) and START.bat starts it again. */
async function restartControl(label) {
  const res = await api('POST', '/test/exit-75');
  await waitFor(`the control process to exit (${label})`, async () => (!(await getJson('/token', 1000)) ? true : null), 30_000, 250);
  await nodeUp(label, 60_000);
  return res.status;
}

/** Every app process of the install, sampled about once a second: PID, start time, command line, last seen. */
function startAppWatch() {
  const a = { t0: Date.now(), apps: new Map(), samples: [], stop: false };
  a.done = (async () => {
    while (!a.stop) {
      const apps = installCensus(await underS()).apps;
      const t = Date.now();
      for (const p of apps) {
        const key = `${p.ProcessId}@${p.created}`;
        const x = a.apps.get(key) ?? { pid: p.ProcessId, created: p.created, cmd: p.CommandLine, firstSeen: t };
        x.lastSeen = t;
        a.apps.set(key, x);
      }
      a.samples.push({ t, pids: apps.map((p) => p.ProcessId) });
      for (let i = 0; i < 4 && !a.stop; i += 1) await sleep(250);
    }
  })();
  return a;
}
async function endAppWatch(a) {
  if (!a) return null;
  a.stop = true;
  await a.done;
  return a;
}
const aliveApps = (aw) => aw.samples.at(-1)?.pids ?? [];

/** While a full-screen app is in front, the app's window goes to the tray (WM_CLOSE, as its close button), so no
 *  window that follows an update opens over it. A window check after such a close is SKIPPED. */
function startWindowGuard(aw) {
  const g = { closes: [], stop: false };
  g.done = (async () => {
    while (!g.stop) {
      const c = await windowCensus();
      const mine = windowsOf(c, aliveApps(aw));
      if (mine.length && fullScreenApp(c.foreground)) {
        await closeWindows(mine.map((x) => x.hwnd));
        g.closes.push({ t: Date.now(), at: iso(), foreground: c.foreground.name, windows: mine.length });
        note('a full-screen app is in front: the app\'s window went to the tray', g.closes.at(-1));
      }
      for (let i = 0; i < 10 && !g.stop; i += 1) await sleep(500);
    }
  })();
  return g;
}
async function endWindowGuard(g) {
  if (!g) return null;
  g.stop = true;
  await g.done;
  return g;
}

/** Posts WM_CLOSE to the app's windows, as their close button does: the window goes, the app stays in the tray. */
async function closeAppWindow(pids) {
  const c = await windowCensus();
  const mine = windowsOf(c, pids);
  const after = mine.length ? await closeWindows(mine.map((x) => x.hwnd)) : c;
  const apps = installCensus(await underS()).apps.map((p) => p.ProcessId);
  const x = { closed: mine.length, left: windowsOf(after, pids).length, pids, apps };
  x.ok = mine.length > 0 && x.left === 0 && pids.length > 0 && pids.every((p) => apps.includes(p));
  note('the app\'s window closed to the tray', x);
  return x;
}

/** The app of an update run, started before the landing: with its window when the gate let it (`shown`), else in
 *  the tray; its watch, and the window guard while it has a window. Call after startWatch. */
async function appBeforeTheLanding(r, label, shown, env) {
  const aw = startAppWatch();
  const app = await openApp(label, shown ? [] : ['--tray'], env);
  const seen = await waitFor(`the app of ${label}`, async () => (aliveApps(aw).includes(app.pid) ? true : null), 20_000, 500);
  const guard = shown ? startWindowGuard(aw) : null;
  r.appWatch = aw;
  r.guard = guard;
  r.appStart = { pid: app.pid, shown, seenMs: seen.ms };
  return { aw, shown, guard, pid: app.pid, started: seen.value === true };
}

/** The version of the exe an app started from: streamloop.exe's file at the app's start. */
function appVersion(w, a) {
  const row = valueAt(w, 'exe', a.created);
  return row?.version ?? (a.created < w.t0 ? VERSION : null);
}

/** The app after a landing or rollback: within 10 s of `from` (the control process of `version` first answering)
 *  the install's one app runs from that version's exe; a follower names it in `--follow`, or with `follows: false`
 *  the app from before stays. `windowOpen`: with its window (SKIPPED when taken away), or none and `--tray`. */
async function appFollowed(w, ctx, { from, version, label, follows = true, windowOpen }) {
  const { aw, shown, guard } = ctx;
  const a = from === null ? null : [...aw.apps.values()].sort((x, y) => x.created - y.created)
    .find((x) => appVersion(w, x) === version && x.lastSeen >= from) ?? null;
  const readyMs = a ? Math.max(0, a.created - from) : null;
  const follow = /--follow\s+(\S+)/.exec(a?.cmd ?? '')?.[1] ?? null;
  const tray = /--tray\b/.test(a?.cmd ?? '');
  // The app it replaced quits right after starting it.
  const settleAt = a ? Math.max(from + FOLLOW_WITHIN_MS, a.created + 5000) : null;
  const settled = settleAt === null ? null : (aw.samples.find((s) => s.t >= settleAt) ?? aw.samples.at(-1))?.pids ?? null;
  const snap = await installSnapshot();
  const windows = a ? appWindows(snap, a.pid).length : 0;
  const u = await updStatus();
  const exeNow = await exeVersion();
  const windowTaken = !shown || (guard?.closes.length ?? 0) > 0;
  const x = { label, from: from === null ? null : iso(from), version, pid: a?.pid ?? null, created: a ? iso(a.created) : null, readyMs, follow, tray, cmd: a?.cmd ?? null,
    appsWhenSettled: settled, appsNow: snap.appPids, windows, currentVersion: u?.currentVersion ?? null, exeVersion: exeNow };
  const check = {
    runsTheVersionWithin10s: a !== null && readyMs <= FOLLOW_WITHIN_MS,
    isTheOnlyApp: a !== null && JSON.stringify(settled) === JSON.stringify([a.pid]),
    matchesExeAndControlProcess: a !== null && version === exeNow && version === u?.currentVersion,
    ...(follows ? { followedNamingTheVersion: a !== null && a.created >= from - 1000 && follow === version } : { stayedTheSameApp: a !== null && a.created < from }),
  };
  if (windowOpen === true) check.windowOpen = windowTaken ? SKIPPED : !tray && windows > 0;
  else if (windowOpen === false) check.noWindow = windows === 0 && (!follows || tray);
  x.checks = Object.fromEntries(Object.entries(check).map(([k, v]) => [`app.${k}`, v]));
  note(`the app after ${label}`, x);
  return x;
}

/** streamloop.exe.old gone within 2 minutes of the confirmation of `to`, which retries a removal the app held up. */
function oldGone(w, tStart, to) {
  const tOk = when(w, 'state', tStart, (v) => v?.to === to && v.result === 'ok');
  const tGone = tOk === null ? null : when(w, 'files', tOk, (v) => v && !v.old);
  return { confirmedMs: rel(tOk, tStart), oldGoneAfterConfirmMs: rel(tGone, tOk), ok: tGone !== null && tGone - tOk <= OLD_GONE_WITHIN_MS };
}

/** streamloop.exe.bad renamed to a temp name and back, then opened for writing. A rename works on an exe a
 *  process runs from; a write-open does not, so the second shows that START.bat's next delete would work. */
function badProbe() {
  if (!existsSync(EXE_BAD)) return { exists: false, ok: false };
  const tmp = `${EXE_BAD}.probe`;
  const x = { exists: true, renamed: false, back: false, writable: false, error: null };
  try {
    renameSync(EXE_BAD, tmp);
    x.renamed = true;
    renameSync(tmp, EXE_BAD);
    x.back = true;
  } catch (err) { x.error = err.code ?? String(err); }
  if (x.renamed && !x.back) { try { renameSync(tmp, EXE_BAD); x.back = true; } catch { /* reported */ } }
  try { closeSync(openSync(EXE_BAD, 'r+')); x.writable = true; } catch (err) { x.error ??= err.code ?? String(err); }
  x.ok = x.renamed && x.back && x.writable;
  return x;
}
async function badNotHeld(label) {
  const r = await waitFor(`nothing holding streamloop.exe.bad (${label})`, async () => { const x = badProbe(); return x.ok ? x : null; }, 30_000, 1000);
  const x = r.value ?? badProbe();
  note(`streamloop.exe.bad (${label})`, x);
  return x;
}

/** Writes StreamLoop.vbs into the test Startup folder as 3.1 wrote it: UTF-16LE with a BOM. */
function writeVbs(bat) {
  const text = `Set WshShell = CreateObject("WScript.Shell")\r\nWshShell.Run """${bat}""", 1, False`;
  writeFileSync(join(sandbox.startupDir, 'StreamLoop.vbs'), Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(text, 'utf16le')]));
}
const runValue = () => regValue(`${sandbox.key}\\Run`, 'StreamLoop');

/** The timeline's messages on disk, oldest first, read once the control process has exited. */
function timelineOnDisk() {
  if (!existsSync(TIMELINE_DIR)) return [];
  const files = readdirSync(TIMELINE_DIR).filter((n) => /^events-\d{4}-\d{2}-\d{2}\.jsonl$/.test(n)).sort();
  return files.flatMap((n) => readFileSync(join(TIMELINE_DIR, n), 'utf8').split(/\r?\n/).filter(Boolean))
    .map((l) => { try { return JSON.parse(l).message ?? null; } catch { return null; } });
}
/** The supervisor's own report of its exit, which the events tail may still add after the quit line. */
const SUPERVISOR_SHUT_DOWN = 'Supervisor shut down';

// runs

/** The full ZIP's first run to live, the key in no install file, and a second START.bat window. */
async function runInstall(r) {
  await install(r, { label: 'install' });
  await sleep(20_000);
  const i = r.install;
  i.readerFirst20s = gapStats(Date.now() - 18_000, Date.now());
  const procs = await underS();
  const s = await supStatus();
  const u = await updStatus();
  const launched = supEvents().filter((e) => e.kind === 'launched').map((e) => ({ breakaway: e.breakaway, pid: e.pid }));
  i.after = { cores: procs.filter(isCore).map(brief), controls: procs.filter(isControl).map(brief), supervisorVersion: s?.version ?? null, currentVersion: u?.currentVersion ?? null, isDevMode: u?.isDevMode ?? null, launched, config: configFacts() };

  // A second window finds the port taken, says so and stays down; the stream is untouched.
  const filesBefore = installFiles();
  const t = Date.now();
  const coresBefore = (await cores()).map((p) => p.ProcessId);
  secondBat = startBat(ROOT, 'second');
  const ended = await waitFor('the second START.bat to end', async () => (secondBat.exitCode !== null ? true : null), 60_000, 250);
  await sleep(5000);
  const second = { endedMs: ended.ms, exitCode: secondBat.exitCode, console: (readText(join(runOut, 'console-second.log')) ?? '').trim().split(/\r?\n/).slice(-4),
    coresAfter: (await cores()).map((p) => p.ProcessId), controlsAfter: (await underS()).filter(isControl).length, gap: gapStats(t - 5000, Date.now()) };
  second.filesBefore = filesBefore;
  second.filesAfter = installFiles();
  secondBat = null;
  i.secondWindow = second;
  // Synchronous over thousands of files, so after the reader's windows: a blocked event loop reads as a gap.
  i.keyGrep = grepTree(S, KEY);
  r.figures = { extractS: i.extractMs / 1000, controlUpS: i.controlUpMs / 1000, wizardToLiveS: i.wizardToLiveMs / 1000, installToLiveS: i.startToLiveMs / 1000 };
  r.checks = {
    noConfigJsonShipped: i.configShipped === false,
    configCreatedFromExample: i.configCreated,
    firstRunStartsNothing: i.firstRun === true && i.coresBeforeWizard === 0,
    setupCheckFoundEverything: i.setupCheck.status === 200 && i.setupCheck.encoder?.works === true && i.setupCheck.failedDeps?.length === 0,
    keyAndConfigSaved: i.keyStatus === 200 && i.saveStatus === 200,
    live: i.wizardToLiveMs !== null,
    readerSteady: noDrop(i.readerFirst20s),
    oneSupervisorLaunchedWithBreakaway: i.after.cores.length === 1 && i.after.cores[0].name === 'streamloop.exe' && launched.at(-1)?.breakaway === true,
    oneControlProcess: i.after.controls.length === 1,
    versionIsN: i.after.supervisorVersion === VERSION && i.after.currentVersion === VERSION,
    rootFromTheExeFolder: i.after.isDevMode === false,
    keyStoredOnlyProtected: i.after.config.keySet && !(readText(CONFIG) ?? '').includes(KEY),
    keyInNoInstallFile: i.keyGrep.files > 0 && i.keyGrep.hits.length === 0,
    secondWindowExits2: second.exitCode === 2 && second.console.some((l) => l.includes('Another StreamLoop is already running. Close it and start this one again.'))
      && second.console.some((l) => l.includes(`Port ${API_PORT} is in use. Is another StreamLoop running? Close it and start this one again.`)),
    secondWindowChangesNothing: coresBefore.length === 1 && JSON.stringify(second.coresAfter) === JSON.stringify(coresBefore) && second.controlsAfter === 1 && noDrop(second.gap)
      && JSON.stringify(second.filesAfter) === JSON.stringify(second.filesBefore),
  };
}

/** The spec's A1, A2, A11, A9 and A10, A18, A15 and A16, A7 and A8, every app started as Explorer starts it.
 *  Every control process gets the test autostart key and Startup folder through the environment. */
async function runApp(r) {
  // exit-75 restarts the control process for A18
  const env = { STREAMLOOP_TEST_HOOKS: '1' };
  const a = r.app = {};
  const c = {};
  r.install = { zip: basename(DIST_ZIP), sha256: sha256File(DIST_ZIP), extractMs: await extract(DIST_ZIP, S) };
  quietConfig(join(ROOT, 'app', 'config.example.json'), { port: API_PORT });
  // The app reads its port from config.json, which the first start writes from the example; on another port than
  // 7654 it must be there before the app starts.
  if (API_PORT !== 7654) copyFileSync(join(ROOT, 'app', 'config.example.json'), CONFIG);
  const expected = `"${EXE}" ui --tray`;

  // A1: double-clicked with nothing running. The app starts START.bat hidden, and the wizard's calls take it live.
  const shown1 = await gate(r, 'A1');
  const t1 = Date.now();
  const app1 = await openApp('A1', shown1 ? [] : ['--tray'], env);
  const x1 = a.A1 = { pid: app1.pid, shown: shown1 };
  x1.apiUpMs = await nodeUp('A1: the control process the app started', 60_000);
  x1.firstRun = (await nodeStatus())?.firstRun ?? null;
  await wizard(x1);
  const live1 = await waitLive('A1 live after the wizard');
  x1.wizardToLiveMs = live1.ms;
  x1.startToLiveMs = Date.now() - t1;
  runReader();
  await sleep(15_000);
  const s1 = await installSnapshot();
  x1.snapshot = snapBrief(s1);
  x1.reader = gapStats(Date.now() - 12_000, Date.now());
  Object.assign(c, {
    'A1.oneOfEach': oneOfEach(s1) && s1.appPids[0] === app1.pid,
    'A1.apiAnswers': x1.apiUpMs !== null,
    'A1.wizardTakesItLive': x1.firstRun === true && x1.keyStatus === 200 && x1.saveStatus === 200 && live1.value !== null && noDrop(x1.reader),
    'A1.windowsOnlyTheApps': onScreen(shown1, appWindows(s1, app1.pid).length > 0 && s1.windows.every((w) => w.pid === app1.pid)),
  });

  // A2: a second start brings the window forward and exits.
  const shown2 = shown1 && await gate(r, 'A2');
  if (shown2) {
    const second = await openApp('A2', [], env);
    const gone = await waitFor('the second app to exit', async () => (!(await underS()).some((p) => p.ProcessId === second.pid) ? true : null), 20_000, 500);
    const front = await waitFor('the app\'s window in front', async () => ((await windowCensus()).foreground?.pid === app1.pid ? true : null), 10_000, 500);
    const s2 = await installSnapshot();
    a.A2 = { secondPid: second.pid, secondExitMs: gone.ms, inFrontMs: front.ms, snapshot: snapBrief(s2) };
    c['A2.oneAppProcess'] = gone.value === true && JSON.stringify(s2.appPids) === JSON.stringify([app1.pid]);
    c['A2.windowInFront'] = front.value === true;
  } else {
    a.A2 = { skipped: true };
    c['A2.oneAppProcess'] = SKIPPED;
    c['A2.windowInFront'] = SKIPPED;
  }

  // A11: START.bat by hand opens one window at its first start, and none after a crash restart.
  a.beforeA11 = await quitAndEnd('before A11');
  const shown11 = await gate(r, 'A11');
  bat = startBat(ROOT, 'by-hand', env, { fromApp: !shown11 });
  const x11 = a.A11 = { shown: shown11 };
  x11.apiUpMs = await nodeUp('A11 START.bat by hand');
  const live11 = await waitLive('A11 live');
  // With the window skipped, the app comes from the tray instead, so the rest of A11 still has one.
  if (!shown11) await openApp('A11 (tray)', ['--tray'], env);
  const opened = await waitFor('the app at START.bat\'s first start', async () => {
    const s = await installSnapshot();
    return s.counts.apps === 1 && (!shown11 || appWindows(s, s.appPids[0]).length > 0) ? s : null;
  }, 30_000, 1000);
  const s11 = opened.value ?? await installSnapshot();
  const app11 = s11.roles.apps[0] ?? null;
  x11.first = snapBrief(s11);
  // Closed to the tray first, so a window seen after the crash restart is one the restart opened.
  x11.closed = shown11 && app11 ? await closeAppWindow([app11.ProcessId]) : null;
  const ctl1 = s11.roles.controls[0]?.ProcessId ?? null;
  const tKill1 = Date.now();
  if (ctl1 !== null) await run('taskkill', ['/F', '/PID', String(ctl1)]);
  await waitFor('the port to close after the kill', async () => ((await portOpen(API_PORT)) ? null : true), 15_000, 250);
  await nodeUp('A11 after the crash restart', 60_000);
  x11.crashToApiMs = Date.now() - tKill1;
  await sleep(15_000);
  const s11b = await installSnapshot();
  x11.afterCrash = snapBrief(s11b);
  Object.assign(c, {
    'A11.oneWindowAtTheFirstStart': onScreen(shown11, opened.value !== null && app11 !== null && appWindows(s11, app11.ProcessId).length > 0
      && / ui --root /.test(app11.CommandLine ?? '') && live11.value !== null),
    'A11.closedWindowLeavesTheAppInTheTray': onScreen(shown11, x11.closed?.ok),
    'A11.noWindowAfterTheCrashRestart': app11 !== null && JSON.stringify(s11b.appPids) === JSON.stringify([app11.ProcessId])
      && s11b.roles.apps.every((p) => p.created < tKill1) && appWindows(s11b, app11.ProcessId).length === 0
      && s11b.counts.controls === 1 && s11b.roles.controls[0].ProcessId !== ctl1 && bat.exitCode === null,
  });

  // Review focus 3: the app started inside START.bat's 10 s wait after a crash starts no second START.bat.
  const shownRf = await gate(r, 'A11 app in START.bat\'s wait');
  if (app11) await run('taskkill', ['/F', '/PID', String(app11.ProcessId)]);
  const ctl2 = s11b.roles.controls[0]?.ProcessId ?? null;
  const tKill2 = Date.now();
  if (ctl2 !== null) await run('taskkill', ['/F', '/PID', String(ctl2)]);
  await waitFor('the port to close after the second kill', async () => ((await portOpen(API_PORT)) ? null : true), 15_000, 250);
  const inWait = await openApp('A11 in the wait', shownRf ? [] : ['--tray'], env);
  await sleep(30_000);
  const sRf = await installSnapshot();
  x11.inWait = { pid: inWait.pid, startedAfterKillMs: inWait.t - tKill2, snapshot: snapBrief(sRf) };
  // The app came up before START.bat started the control process again, so it found nothing answering.
  c['A11.appInTheWaitStartsNoSecondStartBat'] = inWait.t - tKill2 < 10_000 && oneOfEach(sRf) && sRf.appPids[0] === inWait.pid
    && sRf.roles.startBats[0].ProcessId === bat.pid && bat.exitCode === null && sRf.roles.apps[0].created < sRf.roles.controls[0].created;

  // A9 and A10: autostart on writes the Run value; its command as logon runs it gives the tray and the stream.
  const on = await api('POST', '/autostart', { enabled: true });
  const readOn = (await api('GET', '/autostart')).json;
  const valueOn = await runValue();
  a.A9 = { on: on.status, read: readOn, value: valueOn, expected };
  c['A9.valueForThisInstall'] = on.status === 200 && valueOn === expected && readOn?.enabled === true && readOn?.thisInstall === true;
  a.beforeA10 = await quitAndEnd('before A10');
  const shown10 = await gate(r, 'A10');
  const parts = runCommandParts(valueOn);
  const x10 = a.A10 = { parts };
  const t10 = Date.now();
  if (shown10 && parts) {
    const logon = await openApp('A10 as logon', parts.args, env, parts.exe);
    x10.pid = logon.pid;
    x10.apiUpMs = await nodeUp('A10 after the logon start', 60_000);
    const live10 = await waitLive('A10 live');
    x10.startToLiveMs = live10.value ? Date.now() - t10 : null;
    await sleep(10_000);
    const s10 = await installSnapshot();
    x10.snapshot = snapBrief(s10);
    c['A10.trayOnlyAndTheStreamUp'] = oneOfEach(s10) && s10.appPids[0] === logon.pid && /\bui --tray\b/.test(s10.roles.apps[0].CommandLine ?? '')
      && s10.windows.length === 0 && live10.value !== null;
  } else {
    x10.skipped = true;
    c['A10.trayOnlyAndTheStreamUp'] = parts ? SKIPPED : false;
    bat = startBat(ROOT, 'after-A10', env);
    await nodeUp('after A10 (skipped)', 60_000);
    await waitLive('live after A10 (skipped)');
  }
  const off = await api('POST', '/autostart', { enabled: false });
  const valueOff = await runValue();
  const readOff = (await api('GET', '/autostart')).json;
  a.A9.off = { status: off.status, value: valueOff, read: readOff };
  c['A9.offRemovesTheValue'] = off.status === 200 && valueOff === null && readOff?.enabled === false;

  // A18: a StreamLoop.vbs of this install becomes the Run value at the next start; another install's stays.
  const vbs = join(sandbox.startupDir, 'StreamLoop.vbs');
  writeVbs(join(ROOT, 'START.bat'));
  const x18 = a.A18 = { thisRestart: await restartControl('A18 with this install\'s StreamLoop.vbs') };
  const moved = await waitFor('the Run value from StreamLoop.vbs', async () => ((await runValue()) === expected && !existsSync(vbs) ? true : null), 20_000, 1000);
  x18.thisInstall = { value: await runValue(), vbsLeft: existsSync(vbs) };
  const other = join(tmpdir(), 'sl3 other install', 'streamloop', 'START.bat');
  writeVbs(other);
  const otherSha = sha256File(vbs);
  x18.otherRestart = await restartControl('A18 with another install\'s StreamLoop.vbs');
  await sleep(10_000);
  x18.otherInstall = { value: await runValue(), vbsLeft: existsSync(vbs), sameBytes: existsSync(vbs) && sha256File(vbs) === otherSha };
  c['A18.thisInstallsVbsBecomesTheValue'] = x18.thisRestart === 200 && moved.value === true;
  c['A18.anotherInstallsVbsStays'] = x18.otherRestart === 200 && x18.otherInstall.sameBytes && x18.otherInstall.value === expected;
  rmSync(vbs, { force: true });
  x18.offAfter = (await api('POST', '/autostart', { enabled: false })).status;
  x18.valueAfter = await runValue();

  // A15 and A16: a foreign Host, no Host or any Origin is refused on 7654 and 4417, before routing and the token.
  await waitLive('live before A15');
  const supsBefore = (await cores()).map((p) => p.ProcessId);
  const tok = (await getJson('/token', 2000))?.token ?? token;
  const evil = 'http://evil.example';
  const forged = [
    { name: '7654 foreign Host', port: API_PORT, path: '/api/token', headers: { Host: `evil.example:${API_PORT}` } },
    { name: '7654 Origin', port: API_PORT, path: '/api/token', headers: { Origin: evil } },
    { name: '7654 no Host', port: API_PORT, path: '/api/token', noHost: true },
    { name: '7654 Origin on Stop with the token', port: API_PORT, method: 'POST', path: '/api/stream/stop', headers: { Origin: evil, 'X-API-Token': tok, 'Content-Type': 'application/json' }, body: '{}' },
    { name: '4417 foreign Host', port: 4417, path: '/status', headers: { Host: 'evil.example:4417' } },
    { name: '4417 Origin', port: 4417, path: '/status', headers: { Origin: evil } },
    { name: '4417 no Host', port: 4417, path: '/status', noHost: true },
    { name: '4417 Origin on shutdown', port: 4417, method: 'POST', path: '/shutdown', headers: { Origin: evil } },
    { name: '4417 foreign Host on shutdown', port: 4417, method: 'POST', path: '/shutdown', headers: { Host: 'evil.example:4417' } },
  ];
  const tA15 = Date.now();
  const refused = [];
  for (const f of forged) {
    const res = await rawRequest(f);
    let error = null;
    try { error = JSON.parse(res.body).error ?? null; } catch { /* not json */ }
    refused.push({ name: f.name, status: res.status, error });
  }
  const plain = [
    await rawRequest({ port: API_PORT, path: '/api/token' }),
    await rawRequest({ port: API_PORT, path: '/api/token', headers: { Host: `localhost:${API_PORT}` } }),
    await rawRequest({ port: 4417, path: '/status' }),
  ].map((x) => x.status);
  const admin = await rawRequest({ port: API_PORT, path: '/admin' });
  await sleep(10_000);
  const supsAfter = (await cores()).map((p) => p.ProcessId);
  const sup = await supStatus();
  a.A15 = { refused, plain, admin: admin.status, supsBefore, supsAfter, encoderAlive: sup?.encoder?.alive ?? null, gap: gapStats(tA15 - 2000, Date.now()), stream: (await nodeStatus())?.stream?.state ?? null };
  Object.assign(c, {
    'A15.refusedWith403': refused.every((x) => x.status === 403 && x.error === REFUSED),
    'A15.loopbackHostsAnswer': plain.every((s) => s === 200),
    'A15.supervisorStillStreams': supsBefore.length === 1 && JSON.stringify(supsAfter) === JSON.stringify(supsBefore) && sup?.encoder?.alive === true
      && a.A15.stream === 'live' && noDrop(a.A15.gap),
    'A16.adminIs404': admin.status === 404,
  });

  // A7: Quit, as the app sends it: within 60 s nothing of the install runs; the timeline ends on the quit line.
  const tQuit = Date.now();
  const x7 = a.A7 = { quit: await quitAsTheApp() };
  const closed = await waitFor('the port to close after Quit', async () => ((await portOpen(API_PORT)) ? null : true), 60_000, 1000);
  x7.portClosedMs = closed.value ? Date.now() - tQuit : null;
  // The app's own Quit waits for the port to close, then exits; this quit came from the harness, so the harness
  // ends the app at that point instead.
  x7.appsEndedByTheHarness = installCensus(await underS()).apps.map((p) => p.ProcessId);
  for (const pid of x7.appsEndedByTheHarness) await run('taskkill', ['/F', '/PID', String(pid)]);
  const left = await waitFor('no streamloop.exe or cmd.exe of the install', async () => ((await underS()).some((p) => /^(streamloop|cmd)\.exe$/i.test(p.Name ?? '')) ? null : true),
    Math.max(1000, tQuit + 60_000 - Date.now()), 1000);
  x7.nothingLeftMs = left.value ? Date.now() - tQuit : null;
  x7.left = (await underS()).map((p) => `${p.Name} ${p.ProcessId}`);
  const tl7 = timelineOnDisk();
  const quitAt = tl7.lastIndexOf(QUIT_LINE);
  x7.timelineFromTheQuitLine = quitAt < 0 ? [] : tl7.slice(quitAt);
  x7.stoppedMarker = existsSync(STOPPED_MARKER);
  x7.discord = messagesSince(tQuit - 1000).filter((m) => m.includes('Quitting'));
  Object.assign(c, {
    'A7.quitAnswersOk': x7.quit.status === 200 && x7.quit.json?.ok === true,
    'A7.nothingLeftWithin60s': left.value === true && x7.nothingLeftMs <= 60_000,
    'A7.timelineEndsOnTheQuitLine': quitAt >= 0 && x7.timelineFromTheQuitLine.slice(1).every((m) => m === SUPERVISOR_SHUT_DOWN),
    'A7.discordGotTheQuitLine': x7.discord.includes(`Recovery action: **${QUIT_LINE}**`),
    'A7.noStoppedMarker': x7.stoppedMarker === false,
  });

  // A8: started again after Quit, live with autoStartStream on.
  await waitFor('no process of the install', async () => ((await underS()).length === 0 ? true : null), 30_000, 1000);
  const shown8 = await gate(r, 'A8');
  const t8 = Date.now();
  const app8 = await openApp('A8', shown8 ? [] : ['--tray'], env);
  const x8 = a.A8 = { pid: app8.pid, shown: shown8 };
  x8.apiUpMs = await nodeUp('A8 after Quit', 60_000);
  const live8 = await waitLive('A8 live with autoStartStream on');
  x8.startToLiveMs = live8.value ? Date.now() - t8 : null;
  await sleep(10_000);
  const s8 = await installSnapshot();
  x8.snapshot = snapBrief(s8);
  x8.autoStartStream = readJson(CONFIG)?.autoStartStream ?? null;
  Object.assign(c, {
    'A8.liveWithAutoStartOn': live8.value !== null && x8.autoStartStream !== false,
    'A8.oneOfEach': oneOfEach(s8) && s8.appPids[0] === app8.pid,
    'A8.windowsOnlyTheApps': onScreen(shown8, appWindows(s8, app8.pid).length > 0 && s8.windows.every((w) => w.pid === app8.pid)),
  });

  r.figures = {
    A1: { apiUpS: x1.apiUpMs / 1000, wizardToLiveS: x1.wizardToLiveMs / 1000, startToLiveS: x1.startToLiveMs / 1000, windows: s1.windows.length },
    A2: a.A2.skipped ? 'skipped' : { secondExitS: a.A2.secondExitMs / 1000, inFrontS: a.A2.inFrontMs / 1000 },
    A11: { crashToApiS: x11.crashToApiMs / 1000, appStartedAfterKillS: x11.inWait.startedAfterKillMs / 1000 },
    A10: x10.skipped ? 'skipped' : { startToLiveS: x10.startToLiveMs / 1000 },
    A7: { portClosedS: x7.portClosedMs / 1000, nothingLeftS: x7.nothingLeftMs / 1000 },
    A8: { startToLiveS: x8.startToLiveMs / 1000 },
  };
  r.checks = c;
}

/** N+1 at the planned break, N+2 by Update Now, N+3 while stopped from the dashboard, N+4 while waiting for Start. */
async function runUpdate(r) {
  const releases = (ids) => writeReleases('update', ids);
  const env = { STREAMLOOP_TEST_RELEASES: releases(['n1']), STREAMLOOP_TEST_BREAK_AT_S: String(BREAK_AT_S) };
  // Before the install, so a wait for a full-screen app cannot run past the planned break.
  const shown = await gate(r, 'update: the app');
  await install(r, { env, label: 'update', updates: true });
  const w = startWatch();
  r.watch = w;
  const ctx = await appBeforeTheLanding(r, 'update', shown, env);

  // The first check stages N+1, and it lands at 300 s of encoder uptime, with the app's window open.
  const staged = await waitStaged('n1');
  const before = await snapshot();
  const probe = startProbe();
  const b = await onAirLanding(r, w, probe, V1, 'break', before, { ctx, windowOpen: true });
  r.break = { staged, before, ...b };
  r.break.checks = {
    stagedBeforeTheBreak: staged.stagedSha256Matches && staged.uptimeS !== null && staged.uptimeS < BREAK_AT_S,
    statusIdleWhileStaged: staged.status === 'idle',
    noticeGivesTheHoursLeft: staged.notice === 'It installs by itself in about 1 hour, just before Twitch\'s 48-hour cut. Update Now installs it at once and restarts the stream.',
    onlyTheExeFetched: fetchedOnlyTheExe(staged.fetched, 'n1'),
    landedAtTheBreak: b.times.uptimeAtLandingS !== null && b.times.uptimeAtLandingS >= BREAK_AT_S && b.times.uptimeAtLandingS < BREAK_AT_S + 30,
    ...b.checks,
  };

  // Update Now to N+2, once N+1's first check (30 s after its confirmation) and the 60 s cooldown have passed.
  const tConfirmed = b.times.resultMs === null ? Date.now() : w.rows.find((x) => x.kind === 'state' && x.v?.to === V1 && x.v.result === 'ok')?.t ?? Date.now();
  const wait = tConfirmed + 30_000 + 62_000 - Date.now();
  if (wait > 0) await sleep(wait);
  releases(['n1', 'n2']);
  const checked = await waitFor(`the check to offer ${V2}`, async () => {
    const c = (await api('POST', '/update/check')).json;
    if (c?.latestVersion === V2 && c.updateAvailable) return c;
    await sleep(10_000);
    return null;
  }, 120_000, 0);
  // autoStartStream off: an on-air landing still relaunches the supervisor.
  const autoOff = await api('POST', '/config', { autoStartStream: false });
  // From here the app's window is closed: the app follows N+2 to N+4 in the tray.
  r.windowClosed = ctx.shown && !ctx.guard?.closes.length ? await closeAppWindow(aliveApps(ctx.aw)) : null;
  const before3 = await snapshot();
  const probe3 = startProbe();
  const tApply = Date.now();
  const apply = await api('POST', '/update/apply');
  const n = await onAirLanding(r, w, probe3, V2, 'update-now', before3, { ctx, windowOpen: false });
  r.updateNow = { check: checked.value, autoStartStreamOff: { status: autoOff.status, inConfig: readJson(CONFIG)?.autoStartStream ?? null }, apply: { status: apply.status, body: apply.json }, applyToLandingMs: null, before: before3, ...n };
  const landingAt = w.rows.find((x) => x.t >= tApply && x.kind === 'state' && x.v?.to === V2 && x.v.result === 'running' && x.v.step !== 'download')?.t ?? null;
  r.updateNow.applyToLandingMs = rel(landingAt, tApply);
  r.updateNow.checks = {
    offered: checked.value !== null,
    autoStartStreamOff: autoOff.status === 200 && r.updateNow.autoStartStreamOff.inConfig === false,
    applyAnswered: apply.status === 200 && apply.json?.ok === true,
    landedAtOnceNotAtTheBreak: landingAt !== null && landingAt - tApply < 30_000 && before3.uptimeS !== null && before3.uptimeS < BREAK_AT_S,
    relaunchedWithAutoStartOff: n.after.supervisorVersion === V2 && n.checks.liveAgainSteady,
    ...n.checks,
  };

  // Stopped from the dashboard, N+3 lands without a supervisor and `check` confirms it.
  const stop = await api('POST', '/stream/stop');
  const down = await waitFor('the supervisor to exit after Stop', async () => ((await cores()).length === 0 ? true : null), 60_000, 500);
  const tConfirmed2 = w.rows.find((x) => x.kind === 'state' && x.v?.to === V2 && x.v.result === 'ok')?.t ?? Date.now();
  const wait2 = tConfirmed2 + 30_000 + 62_000 - Date.now();
  if (wait2 > 0) await sleep(wait2);
  releases(['n1', 'n2', 'n3']);
  let offered = null;
  const h = await heldLanding(w, V3, 'stopped', async () => {
    const tCheck = Date.now();
    offered = await waitFor(`the check to offer ${V3}`, async () => {
      const c = (await api('POST', '/update/check')).json;
      if (c?.latestVersion === V3 && c.updateAvailable) return c;
      await sleep(10_000);
      return null;
    }, 120_000, 0);
    return tCheck;
  }, { ctx, windowOpen: false });
  r.held = { stop: stop.status, supervisorExitMs: down.ms, offered: offered?.value?.latestVersion ?? null, ...h.summary };
  r.held.checks = {
    stopped: stop.status === 200 && down.value === true,
    offered: offered?.value != null,
    landedSoonAfterTheCheck: h.L.tStart !== null && h.L.tStart - h.tFrom < 30_000,
    ...h.checks,
    stillStoppedAfter: h.after.stream === 'stopped' && h.after.reason === 'The stream was stopped from the dashboard. It stays stopped, even after a restart, until you press Start.',
  };

  // Waiting for Start (autoStartStream off and a fresh start with no supervisor): N+4 lands by Update Now.
  const wt = r.waiting = {};
  wt.shutdown = (await api('POST', '/shutdown')).status;
  await waitFor('the control process to exit', async () => (!(await getJson('/token', 1000)) ? true : null), 30_000, 500);
  await waitFor('START.bat to end', async () => (bat.exitCode !== null ? true : null), 20_000, 500);
  wt.batExitCode = bat.exitCode;
  try { await fetch(`${SUP}/shutdown`, { method: 'POST', signal: AbortSignal.timeout(5000) }); } catch { /* it went away */ }
  wt.supervisorGoneMs = (await waitFor('the supervisor to exit', async () => ((await cores()).length === 0 ? true : null), 60_000, 500)).ms;
  bat = startBat(ROOT, 'update-waiting', env);
  await nodeUp(`${V3} waiting for Start`);
  await sleep(10_000);
  const st0 = await nodeStatus();
  wt.before = { stream: st0?.stream ?? null, launcher: st0?.launcher ?? null, marker: existsSync(WAITING), cores: (await cores()).length };
  releases(['n1', 'n2', 'n3', 'n4']);
  wt.staged = await waitStaged('n4', 120_000);
  let applyW = null;
  const hw = await heldLanding(w, V4, 'waiting', async () => {
    const t = Date.now();
    applyW = await api('POST', '/update/apply');
    return t;
  }, { ctx, windowOpen: false });
  Object.assign(wt, { apply: applyW && { status: applyW.status, body: applyW.json } }, hw.summary);
  wt.checks = {
    waitingBeforeTheLanding: wt.before.stream?.state === 'stopped' && wt.before.stream?.reason === 'Ready to stream. Press Start.' && wt.before.launcher === 'ready' && wt.before.cores === 0,
    stagedN4: wt.staged.stagedSha256Matches,
    applyAnswered: applyW?.status === 200 && applyW.json?.ok === true,
    landedAtOnce: hw.L.tStart !== null && hw.L.tStart - hw.tFrom < 30_000,
    ...hw.checks,
    stillWaitingAfter: hw.after.stream === 'stopped' && hw.after.reason === 'Ready to stream. Press Start.' && hw.after.launcher === 'ready',
  };
  await endWatch(w);
  await endAppWatch(ctx.aw);
  await endWindowGuard(ctx.guard);

  r.figures = {
    breakGapS: r.break.gap.maxWallGapMs / 1000, breakReconnects: r.break.gap.readerReconnects, breakUptimeAtLandingS: r.break.times.uptimeAtLandingS,
    breakNewVersionLiveS: (r.break.times.newVersionLiveMs ?? NaN) / 1000, breakConfirmedS: (r.break.times.resultMs ?? NaN) / 1000,
    updateNowGapS: r.updateNow.gap.maxWallGapMs / 1000, updateNowReconnects: r.updateNow.gap.readerReconnects, updateNowApplyToLandingS: (r.updateNow.applyToLandingMs ?? NaN) / 1000,
    stoppedLandingToConfirmedS: (r.held.landing.resultMs ?? NaN) / 1000, waitingLandingToConfirmedS: (r.waiting.landing.resultMs ?? NaN) / 1000,
    probes409: { break: r.break.probe.duringByVersion, updateNow: r.updateNow.probe.duringByVersion, stopped: r.held.probe.duringByVersion, waiting: r.waiting.probe.duringByVersion },
    appFollowS: { break: r.break.app.readyMs / 1000, updateNow: r.updateNow.app.readyMs / 1000, stopped: r.held.app.readyMs / 1000, waiting: r.waiting.app.readyMs / 1000 },
    oldGoneAfterConfirmS: { break: r.break.old.oldGoneAfterConfirmMs / 1000, updateNow: r.updateNow.old.oldGoneAfterConfirmMs / 1000, stopped: r.held.old.oldGoneAfterConfirmMs / 1000, waiting: r.waiting.old.oldGoneAfterConfirmMs / 1000 },
  };
  r.checks = {
    'app.started': ctx.started,
    // null when the gate or the window guard had closed it already
    'app.windowClosedToTheTray': r.windowClosed === null ? SKIPPED : r.windowClosed.ok,
    ...Object.fromEntries(Object.entries(r.break.checks).map(([k, v]) => [`break.${k}`, v])),
    ...Object.fromEntries(Object.entries(r.updateNow.checks).map(([k, v]) => [`updateNow.${k}`, v])),
    ...Object.fromEntries(Object.entries(r.held.checks).map(([k, v]) => [`stopped.${k}`, v])),
    ...Object.fromEntries(Object.entries(r.waiting.checks).map(([k, v]) => [`waiting.${k}`, v])),
  };
}

/** A landing that starts nothing (stopped from the dashboard, or waiting for Start). `land` starts
 *  it and returns the time to count from. No supervisor may run, `streamloop.exe check` confirms it, and the new
 *  control process answers changing routes with 409 while it does; then Start takes it live. With `app`, as in
 *  `onAirLanding`. */
async function heldLanding(w, to, label, land, app = null) {
  const samples = [];
  let sampling = true;
  const t0 = Date.now();
  const sampler = (async () => {
    while (sampling && Date.now() - t0 < 600_000) {
      const procs = await underS();
      samples.push({ t: Date.now(), cores: procs.filter(isCore).length, checks: procs.filter(isCheck).length, controls: procs.filter(isControl).length });
    }
  })();
  const probe = startProbe();
  const tFrom = await land();
  const L = await followLanding(w, to, { from: tFrom, onAir: false, label });
  await sleep(30_000);
  sampling = false;
  await sampler;
  const p = await endProbe(probe, { by: to });
  const after = await snapshot();
  const st = await nodeStatus();
  after.reason = st?.stream?.reason ?? null;
  after.launcher = st?.launcher ?? null;
  const tl = (await timeline()).map((e) => e.message);
  const tStart = Date.now();
  const start = await api('POST', '/stream/start');
  const liveAgain = await waitLive(`live on ${to} after Start (${label})`, 180_000);
  const s = await supStatus();
  const summary = {
    landing: { fromMs: rel(L.tStart, tFrom), resultMs: rel(L.tResult, L.tStart), result: L.result },
    samples: { n: samples.length, maxCores: Math.max(...samples.map((x) => x.cores)), checkSeen: samples.some((x) => x.checks > 0) },
    probe: p, after, start: { status: start.status, liveMs: liveAgain.ms, supervisorVersion: s?.version ?? null, startToLiveMs: Date.now() - tStart },
  };
  const checks = {
    confirmedByCheck: L.result?.result === 'ok' && L.result?.to === to && summary.samples.checkSeen,
    noSupervisorWhileHeld: summary.samples.maxCores === 0 && after.cores.length === 0,
    changingRoutes409WhileTheNewControlProcessConfirms: p.ok,
    exeIsNew: after.exeVersion === to && after.currentVersion === to,
    updateFilesGone: !after.files.old && !after.files.update && !after.files.landed,
    timelineSaysUpdated: tl.includes(`Updated to StreamLoop ${to}.`),
    timelineSaysItStaysStopped: tl.includes(`Installing StreamLoop ${to}. The stream stays stopped.`) && !tl.includes(`Installing StreamLoop ${to}. The stream restarts.`),
    startTakesItLive: start.status === 200 && liveAgain.value !== null && s?.version === to,
  };
  if (app) {
    const tOld = when(w, 'control', L.tStart, (v) => v === null);
    const tNew = tOld === null ? null : when(w, 'control', tOld, (v) => v?.current === to);
    summary.app = await appFollowed(w, app.ctx, { from: tNew, version: to, label, windowOpen: app.windowOpen });
    summary.old = oldGone(w, L.tStart, to);
    Object.assign(checks, summary.app.checks, { oldGoneWithin2MinOfTheConfirmation: summary.old.ok });
  }
  note(`${label}: landing of ${to}`, { ...summary, checks });
  return { tFrom, L, after, summary, checks };
}

/** After a rollback: N stages the version again, and the planned break comes and goes without landing it. */
async function nextBreakLeavesItAlone(id, liveFrom) {
  const v = VARIANTS[id].version;
  const staged = await waitStaged(id, 180_000);
  const pidsAtStart = (await cores()).map((p) => p.ProcessId);
  const passed = await waitFor(`encoder uptime past ${BREAK_AT_S + 30} s`, async () => {
    const s = await supStatus();
    return (s?.encoder?.uptimeS ?? 0) >= BREAK_AT_S + 30 ? s.encoder.uptimeS : null;
  }, (BREAK_AT_S + 180) * 1000, 5000);
  const after = await snapshot();
  const st = readJson(STATE_FILE);
  const tl = await timeline();
  const installingAgain = tl.filter((e) => Date.parse(e.timestamp) >= liveFrom && e.message === `Installing StreamLoop ${v}. The stream restarts.`).length;
  const x = { staged, uptimeS: passed.value, pidsAtStart, after, state: st, installingAgain };
  x.checks = {
    stagedAgainUpdateNowOnly: staged.stagedSha256Matches && staged.notice === 'Update Now installs it and restarts the stream.',
    breakPassed: passed.value !== null,
    notLandedAgain: after.exeVersion === VERSION && after.supervisorVersion === VERSION && installingAgain === 0 && st?.result !== 'running' && after.updateStatus !== 'ready',
    sameSupervisorThroughTheBreak: pidsAtStart.length === 1 && JSON.stringify(after.cores.map((p) => p.pid)) === JSON.stringify(pidsAtStart),
  };
  note(`the break after the rollback of ${v}`, x);
  return x;
}

/** N+1's supervisor exits at start; the new control process rolls back within 180 s. */
async function runRollback(r) {
  const env = { STREAMLOOP_TEST_RELEASES: writeReleases('rollback', ['n1-supervisor-fails']), STREAMLOOP_TEST_BREAK_AT_S: String(BREAK_AT_S) };
  const shown = await gate(r, 'rollback: the app');
  await install(r, { env, label: 'rollback', updates: true });
  const w = startWatch();
  r.watch = w;
  const ctx = await appBeforeTheLanding(r, 'rollback', shown, env);
  const staged = await waitStaged('n1-supervisor-fails');
  const before = await snapshot();
  // through the landing and the 180 s that N+1's control process confirms
  const probe = startProbe();
  // The app follows onto N+1 within 10 s of N+1's control process answering, and is looked at while it runs.
  const ontoN1 = (async () => {
    const t = await waitFor(`${V1}'s control process`, async () => {
      const ts = when(w, 'state', w.t0, (v) => v?.to === V1 && v.result === 'running' && v.step !== 'download');
      const tg = ts === null ? null : when(w, 'control', ts, (v) => v === null);
      return tg === null ? null : when(w, 'control', tg, (v) => v?.current === V1);
    }, (BREAK_AT_S + 300) * 1000, 500);
    if (t.value === null) return null;
    await sleep(Math.max(0, t.value + FOLLOW_WITHIN_MS + 5000 - Date.now()));
    return appFollowed(w, ctx, { from: t.value, version: V1, label: `the landing of ${V1}`, windowOpen: true });
  })();
  const L = await followLanding(w, V1, { label: 'rollback' });
  const tOldGone = when(w, 'control', L.tStart, (v) => v === null);
  const tN1 = tOldGone === null ? null : when(w, 'control', tOldGone, (v) => v?.current === V1);
  // The rollback's first rename (streamloop.exe to .bad) marks when N+1's control process gave up.
  const tSwapBack = tN1 === null ? null : when(w, 'files', tN1, (v) => v?.bad === true);
  const back = await waitFor(`${VERSION} on air again`, async () => when(w, 'sup', L.tResult ?? L.tStart, (v) => v?.version === VERSION && v.alive === true), 180_000, 500);
  const live = await waitLive(`live on ${VERSION} after the rollback`, 180_000);
  await sleep(20_000);
  const p = await endProbe(probe, { by: V1 });
  const after = await snapshot();
  // ...and back onto N within 10 s of N's control process answering; nothing then holds streamloop.exe.bad.
  const tN1Gone = tN1 === null ? null : when(w, 'control', tN1, (v) => v === null);
  const tNBack = tN1Gone === null ? null : when(w, 'control', tN1Gone, (v) => v?.current === VERSION);
  const appOntoN1 = await ontoN1;
  const appBackToN = await appFollowed(w, ctx, { from: tNBack, version: VERSION, label: `the rollback to ${VERSION}`, windowOpen: true });
  const badHeld = await badNotHeld('after the rollback by the control process');
  const rolled = readJson(ROLLED_BACK);
  const bad = existsSync(EXE_BAD) ? sha256File(EXE_BAD) : null;
  const tl = (await timeline()).map((e) => e.message);
  const criticals = criticalsSince(L.tStart - 10_000);
  const text = `The update to ${V1} did not come up, so StreamLoop went back to ${VERSION}.`;
  r.rollback = {
    staged, before, landing: { uptimeAtLandingS: L.uptimeAtStart, n1ControlMs: rel(tN1, L.tStart), swapBackAfterN1ControlMs: rel(tSwapBack, tN1), rolledBackMs: rel(L.tResult, L.tStart),
      rolledBackAfterN1ControlMs: rel(L.tResult, tN1), liveOnNMs: rel(back.value, L.tStart), result: L.result },
    after, rolledBack: rolled, badSha256: bad, criticals, timeline: tl.slice(-12), probe: p,
    gap: gapStats(L.tStart - 5000, (back.value ?? Date.now()) + 15_000), steadyAfter: gapStats(Date.now() - 15_000, Date.now()),
  };
  r.rollback.app = { ontoN1: appOntoN1, backToN: appBackToN, bad: badHeld };
  note('rollback by the control process', r.rollback);
  r.nextBreak = await nextBreakLeavesItAlone('n1-supervisor-fails', back.value ?? Date.now());
  await endWatch(w);
  await endAppWatch(ctx.aw);
  await endWindowGuard(ctx.guard);
  r.figures = { rollbackS: (r.rollback.landing.liveOnNMs ?? NaN) / 1000, swapBackAfterN1ControlS: (r.rollback.landing.swapBackAfterN1ControlMs ?? NaN) / 1000, offAirS: r.rollback.gap.maxWallGapMs / 1000,
    probes409: p.duringByVersion, appOntoN1S: (appOntoN1?.readyMs ?? NaN) / 1000, appBackToNS: (appBackToN.readyMs ?? NaN) / 1000 };
  r.checks = {
    'app.started': ctx.started,
    ...Object.fromEntries(Object.entries(appOntoN1?.checks ?? { 'app.runsTheVersionWithin10s': false }).map(([k, v]) => [`ontoN1.${k}`, v])),
    ...Object.fromEntries(Object.entries(appBackToN.checks).map(([k, v]) => [`backToN.${k}`, v])),
    badNotHeldAfterTheAppFollowedBack: badHeld.ok,
    staged: staged.stagedSha256Matches,
    landedAtTheBreak: L.uptimeAtStart !== null && L.uptimeAtStart >= BREAK_AT_S && L.uptimeAtStart < BREAK_AT_S + 30,
    n1ControlRan: tN1 !== null,
    // the window, plus a 2 s poll and its 1 s client timeout
    rolledBackWithin180s: L.result?.result === 'rolled-back' && L.result?.to === V1 && tSwapBack !== null && tSwapBack - tN1 <= CONFIRM_MS + 5000,
    exeIsNAgain: after.exeVersion === VERSION && after.currentVersion === VERSION,
    streamingOnN: live.value !== null && after.supervisorVersion === VERSION && after.cores.length === 1 && noDrop(r.rollback.steadyAfter),
    rolledBackNamesN1: rolled?.to === V1 && rolled?.from === VERSION,
    badIsTheFailingBuild: bad === BUILT['n1-supervisor-fails'].sha256,
    changingRoutes409WhileN1Confirms: p.ok,
    oneCritical: criticals.length === 1 && criticals[0] === `**CRITICAL:** ${text}`,
    timelineSaysWhy: tl.includes(text),
    updateFilesGone: !after.files.old && !after.files.landed,
    ...Object.fromEntries(Object.entries(r.nextBreak.checks).map(([k, v]) => [`nextBreak.${k}`, v])),
  };
}

/** N+1's control mode exits at start; START.bat puts N back and N replaces N+1's supervisor. */
async function runRollbackBat(r) {
  const env = { STREAMLOOP_TEST_RELEASES: writeReleases('rollback-bat', ['n1-control-fails']), STREAMLOOP_TEST_BREAK_AT_S: String(BREAK_AT_S) };
  const shown = await gate(r, 'rollback-bat: the app');
  await install(r, { env, label: 'rollback-bat', updates: true });
  const w = startWatch();
  r.watch = w;
  const ctx = await appBeforeTheLanding(r, 'rollback-bat', shown, env);
  const staged = await waitStaged('n1-control-fails');
  const before = await snapshot();
  const started = await waitFor(`the landing of ${V1}`, async () => when(w, 'state', w.t0, (v) => v?.to === V1 && v.result === 'running'), (BREAK_AT_S + 240) * 1000, 250);
  if (started.value === null) throw new Stop(`${V1} did not start landing`);
  const tStart = started.value;
  // N+1's supervisor comes up from the landing; its PID is the one N must replace.
  const n1Sup = await waitFor(`the ${V1} supervisor`, async () => (when(w, 'sup', tStart, (v) => v?.version === V1) !== null ? (await cores()).map((p) => p.ProcessId) : null), 120_000, 500);
  const tN1Sup = when(w, 'sup', tStart, (v) => v?.version === V1);
  const consoleFile = join(runOut, 'console-rollback-bat.log');
  const RECOVER = '[RECOVER] The updated StreamLoop stopped before it was confirmed, so the previous version is back.';
  const recovered = await waitFor('START.bat\'s [RECOVER] line', async () => ((readText(consoleFile) ?? '').includes(RECOVER) ? true : null), 120_000, 250);
  const tRecover = Date.now();
  // N+1's control process never answers, so the first after the old one went is N's again.
  const tOldGone = when(w, 'control', tStart, (v) => v === null);
  const tNControl = await waitFor(`${VERSION}'s control process`, async () => (tOldGone === null ? null : when(w, 'control', tOldGone, (v) => v?.current === VERSION)), 90_000, 250);
  await nodeUp(`${VERSION} after START.bat's rollback`, 90_000);
  // N's supervisor alive again after N+1's, which the landing started.
  const back = await waitFor(`${VERSION} on air again`, async () => (tN1Sup === null ? null : when(w, 'sup', tN1Sup, (v) => v?.version === VERSION && v.alive === true)), 180_000, 500);
  const live = await waitLive(`live on ${VERSION} after START.bat's rollback`, 180_000);
  await sleep(20_000);
  const after = await snapshot();
  // N+1's control process never answered, so the app from before stays; N replaced N+1's supervisor, so nothing
  // holds streamloop.exe.bad.
  const appStays = await appFollowed(w, ctx, { from: tNControl.value, version: VERSION, label: 'START.bat\'s rollback', follows: false, windowOpen: true });
  const badHeld = await badNotHeld('after START.bat\'s rollback');
  const rolled = readJson(ROLLED_BACK);
  const consoleText = readText(consoleFile) ?? '';
  const tl = (await timeline()).map((e) => e.message);
  const replaceText = `Restarting the supervisor: it runs ${V1}, this is ${VERSION}.`;
  // N settles the progress START.bat's rollback left at the landing's start step, and reports it.
  const whyText = `The update to ${V1} failed at starting the stream: The updated StreamLoop stopped before it was confirmed. Still on ${VERSION}.`;
  const st = readJson(STATE_FILE);
  r.rollbackBat = {
    staged, before, n1SupervisorPids: n1Sup.value, n1SupervisorMs: rel(tN1Sup, tStart), recoverSeenMs: rel(tRecover, tStart), nControlMs: rel(tNControl.value, tStart), liveOnNMs: rel(back.value, tStart),
    replaceMs: rel(back.value, tNControl.value), uptimeAtLandingS: uptimeBefore(w, tStart),
    after, rolledBack: rolled, state: st, consoleTail: consoleText.trim().split(/\r?\n/).slice(-12), timeline: tl.slice(-12),
    badSha256: existsSync(EXE_BAD) ? sha256File(EXE_BAD) : null, gap: gapStats(tStart - 5000, (tN1Sup ?? tStart) + 15_000), replaceGap: gapStats(tNControl.value ?? tRecover, (back.value ?? Date.now()) + 15_000), steadyAfter: gapStats(Date.now() - 15_000, Date.now()),
    criticals: criticalsSince(tStart - 10_000),
  };
  r.rollbackBat.app = { stays: appStays, bad: badHeld };
  note('rollback by START.bat', r.rollbackBat);
  r.nextBreak = await nextBreakLeavesItAlone('n1-control-fails', back.value ?? Date.now());
  await endWatch(w);
  await endAppWatch(ctx.aw);
  await endWindowGuard(ctx.guard);
  r.figures = { rollbackS: (r.rollbackBat.liveOnNMs ?? NaN) / 1000, recoverLineS: (r.rollbackBat.recoverSeenMs ?? NaN) / 1000, versionRuleS: (r.rollbackBat.replaceMs ?? NaN) / 1000,
    landingGapS: r.rollbackBat.gap.maxWallGapMs / 1000, replaceGapS: r.rollbackBat.replaceGap.maxWallGapMs / 1000 };
  r.checks = {
    'app.started': ctx.started,
    ...appStays.checks,
    badNotHeldAfterStartBatsRollback: badHeld.ok,
    staged: staged.stagedSha256Matches,
    landedAtTheBreak: r.rollbackBat.uptimeAtLandingS !== null && r.rollbackBat.uptimeAtLandingS >= BREAK_AT_S && r.rollbackBat.uptimeAtLandingS < BREAK_AT_S + 30,
    n1SupervisorCameUp: n1Sup.value?.length === 1,
    n1ControlExited1: /StreamLoop exited \(code 1\)/.test(consoleText),
    recoverLinePrinted: recovered.value === true,
    exeIsNAgainBadIsN1: after.exeVersion === VERSION && r.rollbackBat.badSha256 === BUILT['n1-control-fails'].sha256,
    rolledBackNamesN1: rolled?.to === V1 && rolled?.from === VERSION,
    n1SupervisorReplacedByVersionRule: n1Sup.value?.length === 1 && after.cores.length === 1 && after.cores[0].pid !== n1Sup.value[0] && after.supervisorVersion === VERSION && tl.includes(replaceText),
    streamBack: live.value !== null && noDrop(r.rollbackBat.steadyAfter),
    progressSaysRolledBack: st?.to === V1 && st.result === 'rolled-back',
    timelineSaysWhy: tl.includes(whyText),
    oneCritical: r.rollbackBat.criticals.length === 1 && r.rollbackBat.criticals[0] === `**CRITICAL:** ${whyText}`,
    ...Object.fromEntries(Object.entries(r.nextBreak.checks).map(([k, v]) => [`nextBreak.${k}`, v])),
  };
}

// the move from 3.0.5

/** v3.0.5 from its GitHub release, read-only, checked against its .sha256 asset. */
async function v305Zip(r) {
  mkdirSync(V305_CACHE, { recursive: true });
  const zip = join(V305_CACHE, `streamloop-${V305_TAG}.zip`);
  const shaText = await (await fetch(`${V305_URL}.sha256`, { signal: AbortSignal.timeout(30_000) })).text();
  const want = shaText.trim().split(/\s+/)[0].toLowerCase();
  if (!existsSync(zip) || sha256File(zip) !== want) {
    const res = await fetch(`${V305_URL}.zip`, { signal: AbortSignal.timeout(600_000) });
    writeFileSync(zip, Buffer.from(await res.arrayBuffer()));
  }
  const got = sha256File(zip);
  r.v305 = { url: `${V305_URL}.zip`, bytes: statSync(zip).size, sha256: got, expected: want, match: got === want };
  if (!r.v305.match) throw new Stop('v3.0.5 checksum mismatch');
  return zip;
}

/** 3.0.5 extracted, started from its own START.bat and taken live with the wizard's calls. */
async function install305(zip, label) {
  const i = { extractMs: await extract(zip, S) };
  // 3.0.5's own yt-dlp falls behind YouTube and quietConfig turns its self-update off, so it runs the tested ZIP's.
  const ytdlp = await run(TAR, ['-xf', DIST_ZIP, '-C', S, 'streamloop/yt-dlp/yt-dlp.exe'], { timeout: 120_000 });
  if (ytdlp.status !== 0) throw new Stop(`tar could not take yt-dlp.exe from ${DIST_ZIP}: ${ytdlp.stderr}`);
  quietConfig(join(ROOT, 'app', 'config.example.json'), { port: API_PORT });
  bat = startBat(ROOT, label, { STREAMLOOP_TEST_HOOKS: '1' });
  i.nodeUpMs = await nodeUp(`3.0.5 (${label})`);
  i.version = (await updStatus())?.currentVersion ?? null;
  await wizard(i);
  const live = await waitLive(`3.0.5 live (${label})`);
  if (!live.value) throw new Stop(`3.0.5 did not go live (${label})`);
  i.wizardToLiveMs = live.ms;
  const procs = await underS();
  i.cores = procs.filter(isCore).map(brief);
  i.node = procs.filter(isNode30).map(brief);
  i.status = await supStatus();
  i.timeline = await timeline();
  note(`3.0.5 live (${label})`, { ...i, timeline: i.timeline.length, status: { state: i.status?.state, version: i.status?.version ?? null } });
  return i;
}

/** Ends 3.0.5's START.bat window and its Node, as closing the window does. */
async function closeWindow(label) {
  const pid = bat?.pid;
  if (bat && bat.exitCode === null) await run('taskkill', ['/F', '/T', '/PID', String(pid)]);
  const gone = await waitFor(`3.0.5's window and Node gone (${label})`, async () => (bat.exitCode !== null && !(await underS()).some(isNode30) ? true : null), 30_000, 500);
  bat = null;
  return gone.ms;
}

const leftoversPresent = () => LEFTOVERS_30.filter((p) => existsSync(join(ROOT, ...p.split('/'))));
const tokenFallbackPresent = () => TOKEN_FALLBACK.filter((p) => existsSync(join(ROOT, p)));
const sameEntries = (old, now) => old.filter((e) => now.some((n) => n.timestamp === e.timestamp && n.message === e.message));

/** Stop, close, extract over, start, press Start; then again with 3.0.5's supervisor left streaming. */
async function runMove(r) {
  const zip = await v305Zip(r);

  // As the release notes say: Stop and wait for Stopped, close the window, extract, start, Start.
  const a = r.withStop = {};
  a.before = await install305(zip, '305-stop');
  runReader();
  await sleep(15_000);
  a.stop = (await api('POST', '/stream/stop')).status;
  a.supervisorExitMs = (await waitFor('3.0.5\'s supervisor to exit after Stop', async () => ((await cores()).length === 0 ? true : null), 60_000, 500)).ms;
  a.timeline305 = await timeline();
  a.windowClosedMs = await closeWindow('305-stop');
  a.config305 = configFacts();
  a.leftoversBefore = leftoversPresent();
  a.tokenFallbackBefore = tokenFallbackPresent();
  a.extract = await extractOver(DIST_ZIP, S);
  bat = startBat(ROOT, 'moved', { STREAMLOOP_TEST_HOOKS: '1' });
  a.controlUpMs = await nodeUp('the new control process after the move');
  await sleep(10_000);
  a.whileStopped = { cores: (await cores()).length, stream: (await nodeStatus())?.stream?.state ?? null, firstRun: (await nodeStatus())?.firstRun ?? null };
  a.leftoversAfter = leftoversPresent();
  a.tokenFallbackAfter = tokenFallbackPresent();
  a.app = readdirSync(join(ROOT, 'app')).sort();
  a.timelineAfter = await timeline();
  a.config = configFacts();
  const tStart = Date.now();
  a.start = (await api('POST', '/stream/start')).status;
  const live = await waitLive('live after the move without the key set again', 240_000);
  a.startToLiveMs = live.ms === null ? null : Date.now() - tStart;
  await sleep(15_000);
  a.after = await snapshot();
  a.steady = gapStats(Date.now() - 12_000, Date.now());
  const kept = sameEntries(a.timeline305, a.timelineAfter);
  a.timelineKept = { entries305: a.timeline305.length, foundAfter: kept.length };
  a.checks = {
    v305Verified: r.v305.match,
    v305WasLive: a.before.version === '3.0.5' && a.before.cores.length === 1 && a.before.cores[0].name === 'streamloop-supervisor.exe',
    stoppedAndClosed: a.stop === 200 && a.supervisorExitMs !== null && a.windowClosedMs !== null,
    extractedOver: a.extract.status === 0,
    staysStoppedUntilStart: a.whileStopped.cores === 0 && a.whileStopped.stream === 'stopped' && a.whileStopped.firstRun === false,
    leftoversGone: a.leftoversBefore.length > 0 && a.leftoversAfter.length === 0,
    // 3.0.5's pot\ and node\ go at the first start, since the ZIP brought streamloop-pot.exe
    potAndNodeGone: a.tokenFallbackBefore.length === TOKEN_FALLBACK.length && a.tokenFallbackAfter.length === 0 && existsSync(join(ROOT, 'supervisor', 'streamloop-pot.exe')),
    configAndKeyKept: a.config.keyBlobSha256 === a.config305.keyBlobSha256 && a.config.keySet && a.config.playlist === a.config305.playlist && a.config.ingest === a.config305.ingest && a.config.port === a.config305.port,
    liveWithoutTheKeyAgain: a.start === 200 && live.value !== null && a.after.supervisorVersion === VERSION && a.after.cores.length === 1 && noDrop(a.steady),
    timelineKept: a.timeline305.length > 0 && kept.length === a.timeline305.length,
  };
  note('move after Stop', { ...a, timeline305: a.timeline305.length, timelineAfter: a.timelineAfter.length });
  await endInstall(r, 'withStop');

  // Without Stop: 3.0.5's supervisor still streams when the new control process starts.
  resetReader();
  const b = r.running = {};
  b.before = await install305(zip, '305-running');
  runReader();
  await sleep(15_000);
  b.windowClosedMs = await closeWindow('305-running');
  const oldPids = (await cores()).map((p) => p.ProcessId);
  const sOld = await supStatus();
  b.whileClosed = { cores: oldPids, state: sOld?.state ?? null, alive: sOld?.encoder?.alive ?? null, version: sOld?.version ?? null };
  b.config305 = configFacts();
  b.extract = await extractOver(DIST_ZIP, S);
  // a file the running 3.0.5 supervisor holds stays as it was; the same file in the ZIP says whether that matters
  b.heldFiles = await heldFilesCompare(b.extract.failed);
  const w = startWatch();
  r.watch = w;
  bat = startBat(ROOT, 'moved-running', { STREAMLOOP_TEST_HOOKS: '1' });
  b.controlUpMs = await nodeUp('the new control process beside 3.0.5\'s supervisor');
  const tUp = Date.now();
  const back = await waitFor(`${VERSION}'s supervisor on air`, async () => when(w, 'sup', tUp, (v) => v?.version === VERSION && v.alive === true), 180_000, 500);
  const live2 = await waitLive('live after the version rule', 180_000);
  await sleep(15_000);
  b.afterFirstStart = { ...(await snapshot()), leftovers: leftoversPresent(), tokenFallback: tokenFallbackPresent() };
  b.timeline = (await timeline()).map((e) => e.message).slice(-12);
  b.gap = gapStats(tUp - 5000, (back.value ?? Date.now()) + 15_000);
  // the next start removes the old exe, which was running at the first one
  const pidsBefore = (await cores()).map((p) => p.ProcessId);
  b.restart = (await api('POST', '/test/exit-75')).status;
  await waitFor('the control process gone after exit 75', async () => (!(await getJson('/token', 1000)) ? true : null), 30_000, 250);
  b.nextStartMs = await nodeUp('the next start after the move', 60_000);
  await sleep(10_000);
  b.afterNextStart = { leftovers: leftoversPresent(), tokenFallback: tokenFallbackPresent(), cores: (await cores()).map((p) => p.ProcessId), stream: (await nodeStatus())?.stream?.state ?? null };
  b.steady = gapStats(Date.now() - 15_000, Date.now());
  await endWatch(w);
  b.checks = {
    v305WasLive: b.before.version === '3.0.5' && b.before.cores.length === 1,
    v305SupervisorKeptStreaming: oldPids.length === 1 && b.whileClosed.state === 'playing' && b.whileClosed.alive === true,
    // tar skips what the running supervisor's tools hold; streamloop.exe, START.bat and app\ are new files.
    // yt-dlp and deno ship as the latest at each build, so a held copy may be 3.0.5's older one; ffmpeg is pinned.
    extractLeftOnlyHeldToolFiles: b.extract.status === 0 || (b.heldFiles.length > 0 && b.heldFiles.every((f) => /^streamloop\/(node|ffmpeg|yt-dlp|pot)\//.test(f.entry)
      && (f.sameAsZip || /^streamloop\/yt-dlp\/(yt-dlp|deno)\.exe$/.test(f.entry)))),
    replacedByVersionRule: b.afterFirstStart.cores.length === 1 && !oldPids.includes(b.afterFirstStart.cores[0].pid) && b.afterFirstStart.supervisorVersion === VERSION
      && b.timeline.includes(`Restarting the supervisor: it runs an unknown version, this is ${VERSION}.`),
    streamBack: live2.value !== null && back.value !== null,
    configAndKeyKept: b.afterFirstStart.config.keyBlobSha256 === b.config305.keyBlobSha256 && b.afterFirstStart.config.keySet,
    oldSupervisorExeGoneAfterNextStart: b.afterNextStart.leftovers.length === 0,
    // files the running 3.0.5 supervisor held may keep pot\ and node\ at the first start; the next removes the rest
    potAndNodeGoneAfterNextStart: b.afterNextStart.tokenFallback.length === 0,
    sameSupervisorThroughTheNextStart: pidsBefore.length === 1 && JSON.stringify(b.afterNextStart.cores) === JSON.stringify(pidsBefore) && noDrop(b.steady),
  };
  note('move with 3.0.5 still streaming', b);
  r.figures = {
    withStop: { startToLiveS: (a.startToLiveMs ?? NaN) / 1000, configAndKeyKept: a.checks.configAndKeyKept, timelineKept: `${a.timelineKept.foundAfter}/${a.timelineKept.entries305}`, leftoversGone: a.checks.leftoversGone },
    running: { offAirS: b.gap.maxWallGapMs / 1000, reconnects: b.gap.readerReconnects, replaced: b.checks.replacedByVersionRule, heldFiles: b.heldFiles.map((f) => f.entry) },
  };
  r.checks = {
    ...Object.fromEntries(Object.entries(a.checks).map(([k, v]) => [`withStop.${k}`, v])),
    ...Object.fromEntries(Object.entries(b.checks).map(([k, v]) => [`running.${k}`, v])),
  };
}

/** The entries tar could not write over a running 3.0.5, each compared with the ZIP's copy. */
async function heldFilesCompare(failedLines) {
  const entries = [...new Set(failedLines.map((l) => /streamloop\/[^:]+/.exec(l.replace(/\\/g, '/'))?.[0]).filter(Boolean))];
  if (entries.length === 0) return [];
  const dir = join(WORK, 'held-files');
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  await run('C:\\Windows\\System32\\tar.exe', ['-xf', DIST_ZIP, '-C', dir, ...entries], { timeout: 300_000 });
  const out = entries.map((e) => {
    const inZip = join(dir, ...e.split('/'));
    const inPlace = join(S, ...e.split('/'));
    return { entry: e, sameAsZip: existsSync(inZip) && existsSync(inPlace) && sha256File(inZip) === sha256File(inPlace) };
  });
  rmSync(dir, { recursive: true, force: true });
  return out;
}

// the stage 5 gate's fault checks, unchanged, against this install

function supEvents() {
  const dir = join(DATA, 'logs');
  if (!existsSync(dir)) return [];
  return dailyFiles(readdirSync(dir), EVENTS, 'jsonl')
    .flatMap((n) => readFileSync(join(dir, n), 'utf8').split(/\r?\n/).filter(Boolean))
    .map((l) => { try { const e = JSON.parse(l); return { ...e, ms: Date.parse(e.ts) }; } catch { return null; } }).filter(Boolean);
}
const eventsSince = (ms, kinds) => supEvents().filter((e) => e.ms >= ms && (!kinds || kinds.includes(e.kind)));
const QUIET = ['handover', 'spare-resolve', 'spare-used', 'youtube-probe', 'download-cancel'];
const briefEvent = (e) => Object.fromEntries(Object.entries(e).filter(([k]) => !['level', 'at', 'ms'].includes(k)));
const eventsOf = (ms) => eventsSince(ms).filter((e) => !QUIET.includes(e.kind)).map(briefEvent);
const firstEvent = (from, kind, timeoutMs, where = () => true) => waitFor(kind, async () => eventsSince(from, [kind]).find(where), timeoutMs, 500);
const supervisors = async () => (await cores()).map((p) => p.ProcessId);

const ALERT_KINDS = {
  error: /^Playback error/, skip: /^Skipping video/, frozen: /^Output frozen/, drop: /^Stream stopped unexpectedly/, restart: /^Stream restarted successfully/,
  down: /supervisor has stopped answering/, back: /Supervisor answering again/, resume: /^Playback resumed/,
};
const alertKind = (m) => Object.keys(ALERT_KINDS).find((k) => ALERT_KINDS[k].test(m)) ?? 'other';
/** Alerts caught in [from, to] against the expected count per kind; `optional` kinds may also appear, up to their count. */
function alertCheck(from, to, want, optional = {}) {
  const got = alerts.filter((a) => a.ms >= from && a.ms <= to).flatMap((a) => a.messages.map((m) => ({ at: a.at, kind: alertKind(m), m })));
  const counts = {};
  for (const g of got) counts[g.kind] = (counts[g.kind] ?? 0) + 1;
  const ok = Object.entries(want).every(([k, n]) => (counts[k] ?? 0) === n)
    && Object.entries(counts).every(([k, n]) => k in want || (k in optional && n <= optional[k]));
  return { ok, counts, messages: got };
}
function alertSummary(from) {
  const messages = alerts.filter((a) => a.ms >= from).flatMap((a) => a.messages.map((m) => ({ ms: a.ms, m })));
  const dupes = messages.filter((x, i) => messages.some((y, j) => j < i && y.m === x.m && x.ms - y.ms < 120_000));
  return { posts: alerts.filter((a) => a.ms >= from).length, messages: messages.length, duplicatesWithin2Min: dupes.map((d) => d.m) };
}

async function fault(kind) {
  const playing = (await supStatus())?.playing ?? null;
  const ms = Date.now();
  let reply;
  try {
    reply = await (await fetch(`${SUP}/debug/fault`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ kind }), signal: AbortSignal.timeout(10_000) })).json();
  } catch (err) { reply = { error: String(err) }; }
  note(`fault ${kind}`, { playing, reply });
  return { ms, playing, reply };
}

/** Mean absolute difference of two grabs, 5 s apart, of a picture patch clear of the overlay, read
 *  from the output as a viewer gets it. About 0 is a still picture. */
async function grabDiff(crop = 'crop=400:200:760:300') {
  const grab = async () => {
    const r = await run(join(ROOT, 'ffmpeg', 'ffmpeg.exe'), ['-hide_banner', '-loglevel', 'error', '-i', READ_URL, '-frames:v', '1', '-vf', `${crop},format=gray`, '-f', 'rawvideo', '-'], { timeout: 30_000, encoding: 'buffer' });
    return r.stdout?.length > 0 ? r.stdout : null;
  };
  const a = await grab();
  await sleep(5000);
  const b = await grab();
  if (!a || !b || a.length !== b.length) return null;
  let sum = 0;
  for (let i = 0; i < a.length; i += 1) sum += Math.abs(a[i] - b[i]);
  return Math.round((sum / a.length) * 1000) / 1000;
}
const MOVING = 0.5;

/** A freeze or a silence needs the read-ahead plus the watchdog's duration on one video. A fault
 *  that takes the retry ladder also needs a retry left: cold-start stalls can spend both. */
async function roomLeft(seconds, retriesLeft = false) {
  const r = await waitFor(`${seconds} s left in a video${retriesLeft ? ' with a retry left' : ''}`, async () => {
    const s = await supStatus();
    const p = s?.playing;
    const ok = p && p.source !== 'slate' && p.duration && p.duration - p.position >= seconds && (!retriesLeft || (s.recovery?.step === 'none' && s.recovery.retries < 2));
    return ok ? { ...p, retries: s.recovery?.retries } : null;
  }, 20 * 60_000, 2000);
  if (!r.value) throw new Stop(`no video with ${seconds} s left`);
  return r.value;
}
const liveNow = async () => (await nodeStatus())?.stream?.state === 'live';

/** Records the checks; a failed one stops the run before the next fault. */
function verdict(r, name, f, checks) {
  f.checks = checks;
  f.pass = Object.values(checks).every(Boolean);
  r.faults[name] = f;
  note(`${name}: ${f.pass ? 'PASS' : 'FAIL'}`, f);
  if (!f.pass) throw new Stop(`${name} failed: ${Object.keys(checks).filter((k) => !checks[k]).join(', ')}`);
}

/** The stage 5 gate, one fault at a time, each checked before the next. */
async function runFaults(r) {
  await install(r, { env: { STREAMLOOP_DEBUG_FAULTS: '1' }, label: 'faults' });
  const t0 = Date.now();
  r.faults = {};
  // Past the cold start's URL feeder before anything is injected.
  await sleep(30_000);
  r.alertsBeforeFaults = alertCheck(t0 - 300_000, Date.now(), {});

  // 1. kill-feeder: a retry in place, covered by the read-ahead.
  {
    const room = await roomLeft(60, true);
    const f = await fault('kill-feeder');
    const retry = await firstEvent(f.ms, 'retry', 30_000);
    await sleep(20_000);
    const live = await waitFor('live after kill-feeder', liveNow, 30_000, 1000);
    const x = { at: iso(f.ms), playing: f.playing, retriesBefore: room.retries, reply: f.reply, retryMs: retry.value ? retry.value.ms - f.ms : null, retry: retry.value && briefEvent(retry.value),
      events: eventsOf(f.ms), gap: gapStats(f.ms - 5000, Date.now()), alerts: alertCheck(f.ms, Date.now(), { error: 1 }) };
    verdict(r, 'kill-feeder', x, { accepted: f.reply?.accepted === true, retry: Boolean(retry.value), live: Boolean(live.value),
      gapUnder2s: x.gap.maxWallGapMs < 2000 && x.gap.readerReconnects === 0, oneErrorAlert: x.alerts.ok });
  }

  // 2. freeze-video: the frozen picture goes out after the read-ahead; the watchdog retries it.
  {
    const room = await roomLeft(120, true);
    const before = await grabDiff();
    const f = await fault('freeze-video');
    await sleep(25_000);
    const frozen = await grabDiff();
    const wd = await firstEvent(f.ms, 'watchdog-freeze', 120_000);
    // On air: the reader's first video packet at the freeze's start (reader dts = encoder media time).
    const since = wd.value?.since;
    const onAir = since === undefined ? null : packets.find((p) => p.ms >= f.ms && p.s === 0 && p.dts >= since)?.ms ?? null;
    const retry = await firstEvent(f.ms, 'retry', 30_000, (e) => e.why === 'freeze');
    await sleep(15_000);
    const after = [];
    for (let i = 0; i < 3 && !(after.at(-1) >= MOVING); i += 1) after.push(await grabDiff());
    await sleep(10_000);
    const x = { at: iso(f.ms), playing: f.playing, retriesBefore: room.retries, reply: f.reply, watchdog: wd.value && briefEvent(wd.value), faultToVerdictMs: wd.value ? wd.value.ms - f.ms : null,
      frozenOnAirToVerdictMs: onAir && wd.value ? wd.value.ms - onAir : null, retry: retry.value && briefEvent(retry.value),
      pictureDiff: { before, frozen, after }, events: eventsOf(f.ms), gap: gapStats(f.ms - 5000, Date.now()), alerts: alertCheck(f.ms, Date.now(), { frozen: 1 }) };
    verdict(r, 'freeze-video', x, { accepted: f.reply?.accepted === true, verdictRetry: wd.value?.outcome === 'retry',
      // stage 2 run B acted 25.1 s after the frozen picture went out; 35 s allows for the reader's timing
      verdictInTime: x.frozenOnAirToVerdictMs !== null && x.frozenOnAirToVerdictMs <= 35_000, retried: Boolean(retry.value),
      movingAgain: after.at(-1) >= MOVING, live: await liveNow(), oneFrozenAlert: x.alerts.ok });
  }

  // 3. mute-audio: silence alone never acts, so the verdict is content and play goes on.
  {
    await roomLeft(120);
    const f = await fault('mute-audio');
    const wd = await firstEvent(f.ms, 'watchdog-silence', 150_000);
    await sleep(30_000);
    const restarts = eventsSince(f.ms, ['feeder-start']).filter((e) => e.reason === 'watchdog');
    const x = { at: iso(f.ms), playing: f.playing, reply: f.reply, watchdog: wd.value && briefEvent(wd.value), faultToVerdictMs: wd.value ? wd.value.ms - f.ms : null,
      watchdogFeederStarts: restarts.map(briefEvent), events: eventsOf(f.ms), gap: gapStats(f.ms - 5000, Date.now()), alerts: alertCheck(f.ms, Date.now(), {}) };
    verdict(r, 'mute-audio', x, { accepted: f.reply?.accepted === true, verdictContent: wd.value?.outcome === 'content', noWatchdogRestart: restarts.length === 0,
      noRetry: !x.events.some((e) => e.kind === 'retry'), live: await liveNow(), noGap: x.gap.maxWallGapMs < 2000 && x.gap.readerReconnects === 0, noAlert: x.alerts.ok });
  }

  // 4. kill-encoder: the one fault here that drops the RTMP connection.
  {
    const gen = (await supStatus())?.encoder?.generation ?? null;
    const segAtFault = reconnects.length - 1;
    const f = await fault('kill-encoder');
    const states = [];
    const back = await waitFor('live after kill-encoder', async () => {
      const s = (await nodeStatus())?.stream?.state ?? null;
      if (states.at(-1)?.state !== s) states.push({ ms: Date.now() - f.ms, state: s });
      return s === 'live' && states.some((x) => x.state !== 'live') ? s : null;
    }, 60_000, 250);
    await sleep(20_000);
    const s = await supStatus();
    const encStart = eventsSince(f.ms, ['encoder-start'])[0];
    const outputBack = packets.find((p) => p.ms > f.ms && p.seg > segAtFault);
    // The control process says live only after the viewer has watched 10 s past the fault (the supervisor's
    // step rule), so on air is judged on the encoder and the reader, and its live time is recorded beside it.
    const x = { at: iso(f.ms), playing: f.playing, reply: f.reply, states, controlLiveMs: back.value ? states.at(-1).ms : null,
      encoderStartMs: encStart ? encStart.ms - f.ms : null, outputBackMs: outputBack ? outputBack.ms - f.ms : null,
      generation: { before: gen, after: s?.encoder?.generation ?? null }, events: eventsOf(f.ms), gap: gapStats(f.ms - 5000, Date.now()),
      alerts: alertCheck(f.ms, Date.now(), { drop: 1, restart: 1 }) };
    verdict(r, 'kill-encoder', x, { accepted: f.reply?.accepted === true, sawRecovering: states.some((y) => y.state !== 'live'),
      onAirWithin10s: x.encoderStartMs !== null && x.encoderStartMs <= 10_000 && x.outputBackMs !== null && x.outputBackMs <= 10_000,
      controlLive: back.value !== null, oneDropOneRestart: x.alerts.ok });
  }

  // 5. The supervisor process alone (no /T): its Job Object takes the children, the control process relaunches it.
  {
    const [pid] = await supervisors();
    const playing = (await supStatus())?.playing ?? null;
    const t = Date.now();
    const kill = await run('taskkill', ['/F', '/PID', String(pid)], { encoding: 'utf8' });
    const relaunched = await waitFor('a new supervisor process', async () => (await supervisors()).some((p) => p !== pid), 60_000, 250);
    const orphans = (await allProcesses()).filter((p) => p.ParentProcessId === pid).map((p) => `${p.Name} ${p.ProcessId}`);
    const live = await waitLive('live after the supervisor kill', 180_000);
    const liveMs = live.value ? Date.now() - t : null;
    const resume = await firstEvent(t, 'resume', 30_000);
    await sleep(20_000);
    const x = { at: iso(t), oldPid: pid, newPids: await supervisors(), kill: kill.stdout.trim(), playing, relaunchMs: relaunched.ms, orphansOfOldPid: orphans,
      liveMs, resume: resume.value && briefEvent(resume.value), events: eventsOf(t), gap: gapStats(t - 5000, Date.now()),
      // the resume event's own alert may ride along with "back"
      alerts: alertCheck(t, Date.now(), { down: 1, back: 1 }, { resume: 1 }) };
    const order = x.alerts.messages.map((m) => m.kind);
    verdict(r, 'supervisor-kill', x, { relaunchWithin10s: relaunched.ms !== null && relaunched.ms <= 10_000, noOrphans: orphans.length === 0, live: Boolean(live.value),
      sameVideo: resume.value?.index === playing?.index && resume.value?.id === playing?.id, downThenBack: x.alerts.ok && order.indexOf('down') < order.indexOf('back') });
  }
  r.alertsOverall = alertSummary(t0);
  r.figures = Object.fromEntries(Object.entries(r.faults).map(([k, f]) => [k, { pass: f.pass, maxGapMs: f.gap?.maxWallGapMs ?? null, reconnects: f.gap?.readerReconnects ?? null }]));
  r.checks = { ...Object.fromEntries(Object.entries(r.faults).map(([k, f]) => [k, f.pass])), noAlertRepeatedWithin2Min: r.alertsOverall.duplicatesWithin2Min.length === 0 };
}

// session

/** Ends an install: stop and shut down through the API unless an interrupt wants speed, then a kill of
 *  what still runs, the evidence copied and grepped for the key, and the folder removed. */
async function endInstall(r, part = null, fast = false) {
  stopReader();
  const c = { fast };
  if (!fast) {
    try {
      if (await getJson('/token', 2000)) {
        await nodeUp('cleanup', 10_000).catch(() => null);
        // A confirmation may still hold every changing route.
        const stop = await waitFor('Stop to be taken', async () => { const s = (await api('POST', '/stream/stop')).status; return s === 409 ? null : { s }; }, 240_000, 5000);
        c.stop = stop.value?.s ?? null;
        c.supervisorExitMs = (await waitFor('the supervisor to exit', async () => ((await cores()).length === 0 ? true : null), 60_000, 1000)).ms;
        c.shutdown = (await api('POST', '/shutdown')).status;
        c.controlExitMs = (await waitFor('the control process to exit', async () => (!(await getJson('/token', 1000)) ? true : null), 30_000, 500)).ms;
        if (bat && bat.exitCode === null) await waitFor('START.bat to end', async () => (bat.exitCode !== null ? true : null), 20_000, 500);
      }
    } catch (err) { c.error = String(err); }
  }
  const out = part ? join(runOut, part) : runOut;
  mkdirSync(out, { recursive: true });
  c.batExitCode = bat?.exitCode ?? null;
  for (const b of [secondBat, bat]) if (b && b.exitCode === null) await run('taskkill', ['/F', '/T', '/PID', String(b.pid)]);
  bat = null;
  secondBat = null;
  c.killed = await killAll(await underS());
  await sleep(2000);
  for (const [from, to] of [[join(DATA, 'logs'), 'supervisor-logs'], [join(ROOT, 'app', 'logs'), 'control-logs']]) {
    try { if (existsSync(from)) cpSync(from, join(out, to), { recursive: true }); } catch (err) { note(`could not copy ${from}`, { error: String(err) }); }
  }
  for (const f of ['update-state.json', 'update-rolled-back', 'state.json']) {
    try { if (existsSync(join(DATA, f))) copyFileSync(join(DATA, f), join(out, f)); } catch { /* evidence only */ }
  }
  const grep = { install: grepTree(S, KEY), output: grepTree(out, KEY) };
  c.keyGrep = { installFiles: grep.install.files, outputFiles: grep.output.files, hits: [...grep.install.hits.map((h) => `install: ${h}`), ...grep.output.hits.map((h) => `output: ${h}`)] };
  try { rmSync(S, { recursive: true, force: true, maxRetries: 5, retryDelay: 1000 }); } catch (err) { c.removeError = String(err); }
  c.removed = !existsSync(S);
  c.stillRunning = (await underS()).map((p) => `${p.Name} ${p.ProcessId}`);
  // a grep that read nothing would pass on a folder that was already gone
  c.ok = c.removed && c.stillRunning.length === 0 && c.keyGrep.installFiles > 0 && c.keyGrep.hits.length === 0;
  (r.cleanups ??= []).push({ part, ...c });
  note('cleanup', { part, ...c });
  return c;
}

const summary = { at: iso(), runs: {} };
let current = null;
let runStartMs = 0;
let mediamtx = null;

function judge(r) {
  if (r.endedAt) return;
  r.cleanupOk = (r.cleanups ?? []).every((c) => c.ok) && (r.cleanups ?? []).length > 0;
  const v = verdictOf(r.checks, { stopped: Boolean(r.failed), cleanupOk: r.cleanupOk });
  Object.assign(r, { verdict: v.verdict, failedChecks: v.failedChecks, skippedChecks: v.skippedChecks, pass: v.verdict === 'PASS' });
  r.endedAt = iso();
  const plain = {
    ...r,
    watch: r.watch ? { rows: r.watch.rows, uptimes: r.watch.uptimes.length } : undefined,
    appWatch: r.appWatch ? { apps: [...r.appWatch.apps.values()].map((x) => ({ ...x, created: iso(x.created) })), samples: r.appWatch.samples.length } : undefined,
    guard: r.guard ? { closes: r.guard.closes } : undefined,
  };
  writeFileSync(join(OUT_ROOT, r.run, 'results.json'), redact(JSON.stringify(plain, null, 2), KEY));
  for (const [k, x] of Object.entries(r.checks ?? {})) note(`check ${x === SKIPPED ? 'SKIP' : x ? 'ok  ' : 'FAIL'} ${k}`);
  note(`${r.run}: ${r.verdict}`, { failedChecks: r.failedChecks, skippedChecks: r.skippedChecks, failed: r.failed?.split('\n')[0], figures: r.figures ?? null, cleanup: r.cleanupOk });
  summary.runs[r.run] = { verdict: r.verdict, pass: r.pass, failedChecks: r.failedChecks, skippedChecks: r.skippedChecks, failed: r.failed?.split('\n')[0] ?? null, figures: r.figures ?? null };
}

async function runOne(name) {
  const out = join(OUT_ROOT, name);
  useOut(out);
  runOut = out;
  resetReader();
  runStartMs = Date.now();
  const r = { run: name, startedAt: iso(), version: VERSION };
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
  await endWatch(r.watch);
  await endAppWatch(r.appWatch);
  await endWindowGuard(r.guard);
  if (!r.cleanups?.some((c) => c.part === null)) await endInstall(r);
  current = null;
  judge(r);
  return r;
}

async function portHolder(port) {
  const r = await run('powershell', ['-NoProfile', '-Command',
    `$c = Get-NetTCPConnection -LocalPort ${port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1; if ($c) { Get-CimInstance Win32_Process -Filter "ProcessId=$($c.OwningProcess)" | Select-Object ProcessId,Name,ExecutablePath,CommandLine | ConvertTo-Json -Compress }`],
    { encoding: 'utf8' });
  try { return JSON.parse(r.stdout); } catch { return { raw: r.stdout.trim() }; }
}
/** The install's own ports; held by something else, a run would drive or stop another session's StreamLoop, or its
 *  supervisor would use another session's token server. */
async function portsTaken() {
  const taken = [];
  for (const port of [API_PORT, 4416, 4417, 4418]) if (await portOpen(port)) taken.push({ port, holder: await portHolder(port) });
  return taken;
}
/** A control process, or a 3.0 install's Node: on another port it would still poll 4417 and attach to this run's supervisor. */
const isBackend = (p) => p?.Name === 'streamloop.exe' || (p?.Name === 'node.exe' && /src[\\/]server[\\/]index\.ts/.test(p.CommandLine ?? ''));

/** The source files newer than dist's ZIP, which was then built without them. */
function distIsStale() {
  const built = statSync(DIST_ZIP).mtimeMs;
  const newer = [];
  const walk = (dir) => {
    if (!existsSync(dir)) return;
    for (const e of readdirSync(dir, { withFileTypes: true })) {
      const p = join(dir, e.name);
      if (e.isDirectory()) { if (e.name !== 'target' && e.name !== 'tests' && e.name !== 'fixtures') walk(p); } else if (statSync(p).mtimeMs > built) newer.push(p.slice(REPO.length));
    }
  };
  for (const d of ['crates', join('supervisor', 'src'), join('src', 'admin'), join('tokenserver', 'src'), join('tokenserver', 'vendor')]) walk(join(REPO, d));
  for (const f of ['Cargo.toml', 'Cargo.lock', join('supervisor', 'Cargo.toml'), join('scripts', 'START.bat'), 'config.example.json',
    join('tokenserver', 'Cargo.toml'), join('tokenserver', 'Cargo.lock'), join('tokenserver', '.cargo', 'config.toml')]) if (statSync(join(REPO, f)).mtimeMs > built) newer.push(f);
  return newer;
}

let sessionEnd = null;
/** Stops the reader, MediaMTX if this session started it, the release host and the catcher, then fails the
 *  session on anything left. */
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
    const tools = join(TOOLS, 'mediamtx.exe').toLowerCase();
    summary.leftovers = {
      underInstall: under(all, [S], IN_TEMP).map((p) => `${p.Name} ${p.ProcessId}`),
      mediamtx: mediamtx ? all.filter((p) => p.Name === 'mediamtx.exe' && String(p.ExecutablePath).toLowerCase() === tools).map((p) => p.ProcessId) : [],
      readers: all.filter((p) => p.Name === 'ffprobe.exe' && (p.CommandLine ?? '').includes(READ_URL)).map((p) => p.ProcessId),
    };
    summary.installRemoved = !existsSync(S);
    // The test key and Startup folder go; the user's own Run value and Startup folder must read as before.
    sandbox.cleanup();
    summary.autostart = { testKey: sandbox.key, testKeyGone: !(await regKeyExists(sandbox.key)), testStartupDirGone: !existsSync(sandbox.startupDir), before: autostartBefore, after: await realAutostart() };
    summary.autostart.realUnchanged = summary.autostart.before === summary.autostart.after;
    summary.clean = summary.installRemoved && Object.values(summary.leftovers).every((l) => l.length === 0)
      && summary.autostart.testKeyGone && summary.autostart.testStartupDirGone && summary.autostart.realUnchanged;
    summary.interrupted = interrupted();
    writeFileSync(join(OUT_ROOT, 'summary.json'), JSON.stringify(summary, null, 2));
    for (const [name, x] of Object.entries(summary.runs)) {
      console.log(`${name}: ${x.blocked ? 'BLOCKED' : x.verdict}${x.failedChecks?.length ? ` failed: ${x.failedChecks.join(', ')}` : ''}${x.skippedChecks?.length ? ` skipped: ${x.skippedChecks.join(', ')}` : ''}${x.failed ? ` stopped: ${x.failed}` : ''} ${JSON.stringify(x.figures ?? {})}`);
    }
    note('session end', { clean: summary.clean, leftovers: summary.leftovers, installRemoved: summary.installRemoved, autostart: { ...summary.autostart, before: undefined, after: undefined } });
    return summary.clean;
  })();
  return sessionEnd;
}

const arg = process.argv[2] ?? 'all';
const names = arg === 'all' ? Object.keys(RUNS) : arg === 'build' ? [] : arg.split(',');
const unknown = names.filter((n) => !(n in RUNS));
mkdirSync(OUT_ROOT, { recursive: true });
mkdirSync(WORK, { recursive: true });
useOut(join(OUT_ROOT, 'session'));
if (unknown.length) { note(`unknown run: ${unknown.join(', ')}; runs: ${Object.keys(RUNS).join(', ')}, all, build`); process.exit(2); }
for (const f of [DIST_ZIP, DIST_EXE]) if (!existsSync(f)) { note(`missing ${f}: npm run build:release first`); process.exit(2); }
const stale = distIsStale();
if (stale.length) { note('dist is older than the code it is built from: npm run build:release first', { newer: stale.slice(0, 10) }); process.exit(2); }
if ((await exeVersion(DIST_EXE)) !== VERSION) { note(`dist's exe does not print ${VERSION}`); process.exit(2); }
BUILT = await buildVariants({ repo: REPO, serve: SERVE }).catch((err) => { note('the test builds failed', { error: String(err?.message ?? err) }); process.exit(2); });
summary.builds = BUILT;
for (const b of Object.values(BUILT)) KNOWN_EXES.set(b.sha256, b.version);
if (arg === 'build') process.exit(0);
// Read only, and again at the end.
const autostartBefore = await realAutostart();

const swept = await sweepLeftovers({ dirs: [S], ascii: IN_TEMP, mediamtx: join(TOOLS, 'mediamtx.exe') });
if (swept.owner) { note('BLOCKED: the install folder belongs to a harness that is still running', swept); process.exit(3); }
if (swept.found.length || swept.removed.length) note('swept what an earlier run left', swept);
// 7654 is shared with other sessions: another API port when it is taken, unless a StreamLoop backend holds it.
if (await portOpen(7654)) {
  const holder = await portHolder(7654);
  if (isBackend(holder)) { note('BLOCKED: a StreamLoop backend holds 7654 and would attach to this run\'s supervisor', holder); process.exit(3); }
}
for (const port of [7654, 7664, 7674, 7684]) { if (!(await portOpen(port))) { API_PORT = port; break; } }
apiBase = useApiPort(API_PORT);
const taken = await portsTaken();
for (const port of [ASSET_PORT, CATCHER_PORT]) if (await portOpen(port)) taken.push({ port, holder: await portHolder(port) });
if (taken.length) { note('BLOCKED: ports taken by processes this session did not start', taken); process.exit(3); }
summary.apiPort = API_PORT;

onInterrupt(async (signal) => {
  stopReader();
  note('quick stop', await quickStop([secondBat, bat], [S]));
  mediamtx?.kill();
  const r = current;
  if (r) {
    r.failed ??= `interrupted by ${signal}`;
    await endWatch(r.watch);
    await endAppWatch(r.appWatch);
    await endWindowGuard(r.guard);
    await endInstall(r, null, true);
    judge(r);
  }
  await endSession();
});
await new Promise((resolve) => assetServer.listen(ASSET_PORT, '127.0.0.1', resolve));
await new Promise((resolve) => catcher.listen(CATCHER_PORT, '127.0.0.1', resolve));
// MediaMTX: this session's own unless one already listens, which is never stopped here.
if (!(await portOpen(1935))) {
  mediamtx = spawn(join(TOOLS, 'mediamtx.exe'), [], { cwd: TOOLS, stdio: 'ignore' });
  await sleep(3000);
  if (!(await portOpen(1935))) { note('MediaMTX did not open :1935'); await endSession(); process.exit(2); }
}
note('preflight done', { apiPort: API_PORT, mediamtx: mediamtx ? `own, pid ${mediamtx.pid}` : 'already running, not ours', builds: Object.keys(BUILT) });

for (const name of names) {
  const r = await runOne(name);
  if (interrupted() || r.blocked || /YouTube breaker open/.test(r.failed ?? '')) { note('stopping the session'); break; }
}
const clean = await endSession();
if (interrupted()) process.exit(130);
// A run with a skipped check is no pass.
process.exit(names.every((n) => summary.runs[n]?.pass) && clean ? 0 : 1);
