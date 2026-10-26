// Stage 4's install proof on the dev machine: the release ZIPs as a user gets them, not the repo.
//   node install-session.mjs
// 1. The full ZIP is extracted into %TEMP%\sl3 install Ö\ and its START.bat runs hidden. The
//    wizard's API calls (system check included) take it to live against local MediaMTX.
// 2. The slim ZIP's app\ is staged into _update_tmp\ by the installed updater's stageUpdate, and the
//    env-gated POST /api/test/exit-75 hook makes START.bat swap it in while a yt-dlp download runs.
//    Then the installed updater's checkForUpdate runs on GitHub's real release list plus one made-up
//    3.x prerelease (fetch is stubbed, nothing is written to GitHub).
// 3. Stop from 3.0's dashboard, kill 3.0's Node tree, start the v2.3.8 full ZIP from
//    %TEMP%\sl2 install\, read its stream guard, stop it, start 3.0 again: stopped until Start.
// 4. Key grep over both installs, leftover processes, both installs removed.
//   node install-session.mjs faults
// The stage 5 gate instead: step 1 into %TEMP%\sl3 gate Ö\ with STREAMLOOP_DEBUG_FAULTS=1 and the
// webhook on a local catcher, then one fault at a time through POST /debug/fault, each checked
// before the next (see faults()). Then the key grep, leftovers and removal as above.
// node-session.mjs runs its session on import, so the few helpers needed are repeated here.
// Nothing opens a window: every child is spawned hidden.
import * as cp from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync, readFileSync, existsSync, appendFileSync, readdirSync, statSync, openSync, closeSync, cpSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import net from 'node:net';
import http from 'node:http';
import { TOKEN_SERVER_PING, FFPROBE, PLAYLIST, redact } from './lib.mjs';

const spawn = (cmd, args, opts = {}) => cp.spawn(cmd, args, { windowsHide: true, ...opts });
const run = (cmd, args, opts = {}) => new Promise((resolve) => {
  cp.execFile(cmd, args, { windowsHide: true, maxBuffer: 64 * 1024 * 1024, ...opts }, (err, stdout, stderr) => resolve({ status: err ? (err.code ?? 1) : 0, stdout, stderr }));
});
const REPO = fileURLToPath(new URL('../..', import.meta.url));
const VERSION = JSON.parse(readFileSync(join(REPO, 'package.json'), 'utf8')).version;
const FULL_ZIP = join(REPO, 'dist', `streamloop-v${VERSION}.zip`);
const SLIM_ZIP = join(REPO, 'dist', `streamloop-v${VERSION}-update.zip`);
const V2_TAG = 'v2.3.8';
const V2_URL = `https://github.com/Sponsorn/streamloop/releases/download/${V2_TAG}/streamloop-${V2_TAG}.zip`;
const RELEASES_API = 'https://api.github.com/repos/Sponsorn/streamloop/releases?per_page=50';
const TAR = 'C:\\Windows\\System32\\tar.exe';
const TOOLS = join(REPO, 'spikes', 'tools');
const KEY = 'live_TESTKEY';
const INGEST = 'rtmp://127.0.0.1:1935/live';
const READ_URL = `${INGEST}/${KEY}`;
const NODE_API = 'http://127.0.0.1:7654/api';
const SUP = 'http://127.0.0.1:4417';
const GATE = process.argv[2] === 'faults';
const S3 = join(tmpdir(), GATE ? 'sl3 gate Ö' : 'sl3 install Ö');
const S2 = join(tmpdir(), 'sl2 install');
const ROOT3 = join(S3, 'streamloop');
const OUT = join(REPO, 'supervisor', 'results', GATE ? 'gate' : 'install');
const V2_CACHE = join(REPO, 'supervisor', 'results', 'v2-zip');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const iso = (ms = Date.now()) => new Date(ms).toISOString();

rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });
const note = (msg, data) => {
  const line = redact(`${iso()} ${msg}${data === undefined ? '' : ` ${JSON.stringify(data)}`}`, KEY);
  console.log(line);
  appendFileSync(join(OUT, 'session.log'), `${line}\n`);
};
const results = {};
class Stop extends Error {}

async function ps() {
  const r = await run('powershell', ['-NoProfile', '-Command',
    "Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('node.exe','cmd.exe','streamloop-supervisor.exe','ffmpeg.exe','ffprobe.exe','yt-dlp.exe','mediamtx.exe') } | Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json"],
    { encoding: 'utf8' });
  const v = JSON.parse(r.stdout || '[]');
  return Array.isArray(v) ? v : [v];
}
// PowerShell's output codepage can mangle the Ö, so match the ASCII part of the install folders.
const inInstall = (p) => /sl[23] install|sl3 gate/.test(p.CommandLine ?? '');
const isTestNode = (p) => p.Name === 'node.exe' && inInstall(p) && /src[\\/]server[\\/]index\.ts/.test(p.CommandLine ?? '');
const supervisors = async () => (await ps()).filter((p) => p.Name === 'streamloop-supervisor.exe').map((p) => p.ProcessId);

async function waitFor(what, fn, timeoutMs, everyMs = 1000) {
  const t0 = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return { ms: Date.now() - t0, value: v };
    if (Date.now() - t0 > timeoutMs) { note(`timed out waiting for ${what}`, { timeoutMs }); return { ms: null, value: null }; }
    await sleep(everyMs);
  }
}

// One START.bat in a hidden cmd. Its console output goes to a file; stdin is NUL, so its
// `pause` lines return at once.
function startBat(root, label, extraEnv = {}) {
  const log = openSync(join(OUT, `console-${label}.log`), 'a');
  const pathNoFfmpeg = (process.env.PATH ?? '').split(';').filter((d) => !/chocolatey\\bin|ffmpeg/i.test(d)).join(';');
  const child = spawn('cmd.exe', ['/d', '/s', '/c', `""${join(root, 'START.bat')}""`], {
    windowsVerbatimArguments: true, stdio: ['ignore', log, log], cwd: root,
    env: { ...process.env, PATH: pathNoFfmpeg, ...extraEnv },
  });
  child.on('exit', (code) => { closeSync(log); note(`START.bat (${label}) exited`, { code }); });
  return child;
}
const ENV3 = GATE ? { STREAMLOOP_DEBUG_FAULTS: '1' } : { STREAMLOOP_TEST_HOOKS: '1' };

let token = '';
async function nodeUp(label, timeoutMs = 120_000) {
  const t0 = Date.now();
  const r = await waitFor(`Node (${label})`, async () => {
    try {
      const res = await fetch(`${NODE_API}/token`, { signal: AbortSignal.timeout(2000) });
      return res.ok ? (await res.json()).token : null;
    } catch { return null; }
  }, timeoutMs, 500);
  if (!r.value) throw new Stop(`Node did not answer (${label})`);
  token = r.value;
  note(`Node up (${label})`, { ms: Date.now() - t0 });
  return Date.now() - t0;
}
async function api(method, path, body) {
  const headers = method === 'POST' ? { 'x-api-token': token, 'content-type': 'application/json' } : {};
  let status = 0;
  let text = '';
  try {
    const res = await fetch(NODE_API + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(120_000) });
    status = res.status;
    text = await res.text();
  } catch (err) { text = String(err); }
  const recorded = path === '/stream-key' ? '[stream key, not recorded]' : body;
  // Not redacted, so the key grep catches Node echoing the key.
  appendFileSync(join(OUT, 'requests.jsonl'), `${JSON.stringify({ at: iso(), method, path, request: recorded ?? null, status, response: text.slice(0, 4000) })}\n`);
  let json = null;
  try { json = JSON.parse(text); } catch { /* not json */ }
  return { status, json };
}
const nodeStatus = async () => (await api('GET', '/status')).json;
const supStatus = async () => { try { return await (await fetch(`${SUP}/status`, { signal: AbortSignal.timeout(3000) })).json(); } catch { return null; } };

/** Waits for live; a YouTube breaker that opens stops the run (no retry loop against YouTube). */
async function waitLive(what, timeoutMs = 240_000) {
  return waitFor(what, async () => {
    const s = await nodeStatus();
    if (s?.supervisor?.youtube?.breaker === 'open') throw new Stop(`YouTube breaker open (${what}): ${JSON.stringify(s.supervisor.youtube)}`);
    return s?.stream?.state === 'live' ? s : null;
  }, timeoutMs, 2000);
}

// reader: one ffprobe on the RTMP output, reconnecting
const packets = [];
const reconnects = [];
let readerStop = false;
let probe = null;
function runReader() {
  if (readerStop) return;
  const seg = reconnects.length;
  reconnects.push(Date.now());
  probe = spawn(FFPROBE, ['-v', 'error', '-show_entries', 'packet=stream_index,dts_time', '-of', 'csv=p=0', READ_URL], { stdio: ['ignore', 'pipe', 'ignore'] });
  let tail = '';
  probe.stdout.on('data', (chunk) => {
    const lines = (tail + chunk).split(/\r?\n/);
    tail = lines.pop();
    const now = Date.now();
    for (const line of lines) {
      const [si, dts] = line.split(',');
      if (dts && dts !== 'N/A') packets.push({ ms: now, s: Number(si), dts: Number(dts), seg });
    }
  });
  probe.on('close', () => { if (!readerStop) sleep(1000).then(runReader); });
}
function gapStats(fromMs, toMs) {
  const win = packets.filter((p) => p.ms >= fromMs && p.ms <= toMs);
  let maxWallGapMs = 0;
  for (let i = 1; i < win.length; i += 1) maxWallGapMs = Math.max(maxWallGapMs, win[i].ms - win[i - 1].ms);
  return { packets: win.length, readerReconnects: reconnects.filter((t) => t > fromMs && t <= toMs).length, maxWallGapMs };
}

function hashTree(dir) {
  const h = createHash('sha256');
  let files = 0;
  const walk = (d) => {
    for (const e of readdirSync(d, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const p = join(d, e.name);
      if (e.isDirectory()) walk(p);
      else { files += 1; h.update(relative(dir, p)); h.update(readFileSync(p)); }
    }
  };
  walk(dir);
  return { files, sha256: h.digest('hex') };
}
function grepTree(dir, needle) {
  const hits = [];
  let files = 0;
  const walk = (d) => {
    for (const e of readdirSync(d, { withFileTypes: true })) {
      const p = join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.isFile()) { files += 1; if (readFileSync(p).includes(needle)) hits.push(relative(dir, p)); }
    }
  };
  if (existsSync(dir)) walk(dir);
  return { files, hits };
}
const sha256File = (f) => createHash('sha256').update(readFileSync(f)).digest('hex');

async function extract(zip, dest) {
  rmSync(dest, { recursive: true, force: true });
  mkdirSync(dest, { recursive: true });
  const t0 = Date.now();
  const r = await run(TAR, ['-xf', zip, '-C', dest], { timeout: 600_000 });
  if (r.status !== 0) throw new Stop(`tar failed on ${zip}: ${r.stderr}`);
  return Date.now() - t0;
}
/** No browser, no yt-dlp self-update, no update check: the test must not open a window or touch yt-dlp\. */
function quietConfig(file) {
  const c = JSON.parse(readFileSync(file, 'utf8'));
  Object.assign(c, { autoOpenAdmin: false, autoUpdateYtdlp: false, autoUpdateCheck: false });
  writeFileSync(file, JSON.stringify(c, null, 2));
}
/** Runs a snippet under an install's own Node and tsx, from that app's folder. */
async function tsxEval(root, code, env = {}) {
  const r = await run(join(root, 'node', 'node.exe'), [join(root, 'app', 'node_modules', 'tsx', 'dist', 'cli.mjs'), '-e', code],
    { cwd: join(root, 'app'), encoding: 'utf8', timeout: 60_000, env: { ...process.env, ...env } });
  return { status: r.status, stdout: r.stdout.trim(), stderr: r.stderr.trim().slice(0, 2000) };
}

/** 2.x's own stream guard, under 2.x's own Node: true holds 2.x's OBS stream off. */
const guard2 = (root2) => tsxEval(root2, "import(process.env.MOD).then(async (m) => { console.log(JSON.stringify({ streamloop3IsStreaming: await m.streamloop3IsStreaming() })); process.exit(0); })",
  { MOD: pathToFileURL(join(root2, 'app', 'src', 'server', 'stream-guard.ts')).href });

// preflight
const portOpen = (port) => new Promise((resolve) => {
  const s = net.connect(port, '127.0.0.1');
  s.on('connect', () => { s.destroy(); resolve(true); });
  s.on('error', () => resolve(false));
});
async function tokenServerOk() { try { return (await fetch(TOKEN_SERVER_PING, { signal: AbortSignal.timeout(5000) })).ok; } catch { return false; } }
for (const f of [FULL_ZIP, SLIM_ZIP]) if (!existsSync(f)) { note(`missing ${f}`); process.exit(2); }
if (!(await tokenServerOk())) { note('PO-token server not answering: stop'); process.exit(2); }
if (await portOpen(7654) || await portOpen(4417)) { note('7654 or 4417 already taken: stop'); process.exit(2); }
note('preflight: token server answers /ping, 7654 and 4417 free');
let mediamtx = null;
if (!(await portOpen(1935))) {
  mediamtx = spawn(join(TOOLS, 'mediamtx.exe'), [], { cwd: TOOLS, stdio: 'ignore' });
  await sleep(3000);
  if (!(await portOpen(1935))) { note('MediaMTX did not open :1935'); process.exit(2); }
  note('MediaMTX started', { pid: mediamtx.pid });
}

let bat3 = null;
let bat2 = null;

async function step1() {
  const r = { zip: { file: FULL_ZIP, bytes: statSync(FULL_ZIP).size, sha256: sha256File(FULL_ZIP) } };
  r.extractMs = await extract(FULL_ZIP, S3);
  r.root = ROOT3;
  r.configBeforeStart = existsSync(join(ROOT3, 'app', 'config.json'));
  quietConfig(join(ROOT3, 'app', 'config.example.json'));
  results.ytdlpAtInstall = hashTree(join(ROOT3, 'yt-dlp'));
  note('extracted the full ZIP', { ms: r.extractMs, configJsonShipped: r.configBeforeStart, ytdlp: results.ytdlpAtInstall });

  const t0 = Date.now();
  bat3 = startBat(ROOT3, '3-first', ENV3);
  r.nodeUpMs = await nodeUp('3.0 first start');
  r.configCreatedFromExample = existsSync(join(ROOT3, 'app', 'config.json'));
  const first = await nodeStatus();
  r.firstRun = first?.firstRun;
  r.streamBeforeWizard = first?.stream;
  r.supervisorsBeforeWizard = (await supervisors()).length;

  // The wizard's calls in its order: config, the system check and its settings, then Finish (key first).
  const cfg = (await api('GET', '/config')).json;
  const check = await api('POST', '/setup-check/run');
  r.setupCheck = { status: check.status, report: check.json };
  note('setup check', { encoder: check.json?.encoder, cache: check.json?.cache, failedDeps: check.json?.dependencies?.filter((d) => !d.ok) });
  const picked = { cacheBudgetMb: check.json.cache.recommendedMb };
  if (check.json.encoder.works) picked.encoder = check.json.encoder.chosen;
  const patch = Object.fromEntries(Object.entries(picked).filter(([k, v]) => cfg.supervisor[k] !== v));
  r.setupSave = Object.keys(patch).length ? { patch, status: (await api('POST', '/config', { supervisor: patch })).status } : 'nothing differed';
  const key = await api('POST', '/stream-key', { key: KEY });
  const save = await api('POST', '/config', { playlists: [{ id: PLAYLIST }], discord: { webhookUrl: GATE ? WEBHOOK : '' }, supervisor: { ingestUrl: INGEST } });
  r.keyStatus = key.status;
  r.saveStatus = save.status;
  const live = await waitLive('live after the wizard');
  r.wizardToLiveMs = live.ms;
  r.startToLiveMs = Date.now() - t0;
  if (!live.value) throw new Stop('3.0 did not go live after the wizard');
  const s = await supStatus();
  r.supervisor = { pid: (await supervisors())[0], encoder: s?.encoder, playing: s?.playing, tokenServer: s?.tokenServer ?? s?.token_server ?? null };
  const saved = JSON.parse(readFileSync(join(ROOT3, 'app', 'config.json'), 'utf8'));
  r.savedConfig = { playlist: saved.playlists[0].id, ingest: saved.supervisor.ingestUrl, encoder: saved.supervisor.encoder, preferredEncoder: saved.supervisor.preferredEncoder ?? null, cacheBudgetMb: saved.supervisor.cacheBudgetMb, keyProtected: Boolean(saved.supervisor.streamKeyProtected) };
  runReader();
  await sleep(20_000);
  r.readerFirst20s = gapStats(Date.now() - 20_000, Date.now());
  note('step 1', r);
  results.step1 = r;
}

async function step2() {
  const r = {};
  // A download in flight: the supervisor's downloader, not a -j resolve.
  const dl = await waitFor('a yt-dlp download', async () => (await ps()).filter((p) => p.Name === 'yt-dlp.exe' && inInstall(p) && !/\s-j(\s|$)/.test(p.CommandLine ?? '')), 600_000, 2000);
  r.downloadBefore = dl.value?.map((p) => p.ProcessId) ?? null;
  r.ytdlpBefore = hashTree(join(ROOT3, 'yt-dlp'));

  // Stage as the updater does: extract into _update_tmp\extracted, then the installed stageUpdate.
  const tmp = join(ROOT3, '_update_tmp');
  rmSync(tmp, { recursive: true, force: true });
  r.slimZip = { file: SLIM_ZIP, bytes: statSync(SLIM_ZIP).size, sha256: sha256File(SLIM_ZIP) };
  r.extractMs = await extract(SLIM_ZIP, join(tmp, 'extracted'));
  const stage = await tsxEval(ROOT3, "import(process.env.MOD).then((m) => { m.stageUpdate(process.env.X, process.env.T); console.log('staged'); process.exit(0); })",
    { X: join(tmp, 'extracted'), T: tmp, MOD: pathToFileURL(join(ROOT3, 'app', 'src', 'server', 'updater.ts')).href });
  r.stage = stage;
  r.staged = { app: existsSync(join(tmp, 'app', 'src', 'server', 'index.ts')), startBat: existsSync(join(tmp, 'START.bat')), ytdlp: existsSync(join(tmp, 'yt-dlp')) };
  // The old app gets a marker the swap must take away; the new one would not have it.
  writeFileSync(join(ROOT3, 'app', 'old-app-marker'), '');
  note('staged the slim ZIP', r);

  const supBefore = await supervisors();
  const genBefore = (await supStatus())?.encoder?.generation ?? null;
  const ytBefore = (await ps()).filter((p) => p.Name === 'yt-dlp.exe' && inInstall(p)).map((p) => p.ProcessId);
  // The swap must happen during a download; the supervisor's own status says whether one runs.
  const inFlight = await waitFor('a download in the supervisor status', async () => (await supStatus())?.downloading ?? null, 120_000, 250);
  if (!inFlight.value) throw new Stop('no yt-dlp download in flight for the exit-75 hook');
  r.downloadAtHook = inFlight.value;
  const t = Date.now();
  r.hook = (await api('POST', '/test/exit-75')).status;
  await waitFor('Node gone after exit 75', async () => { try { await fetch(`${NODE_API}/token`, { signal: AbortSignal.timeout(1000) }); return false; } catch { return true; } }, 30_000, 250);
  await nodeUp('3.0 after the update swap');
  r.nodeBackMs = Date.now() - t;
  const attached = await waitFor('the launcher to attach', async () => (await nodeStatus())?.launcher === 'running', 60_000, 500);
  r.attachMs = attached.ms;
  await sleep(30_000);
  const ytAfter = (await ps()).filter((p) => p.Name === 'yt-dlp.exe' && inInstall(p)).map((p) => p.ProcessId);
  r.supervisorBefore = supBefore;
  r.supervisorAfter = await supervisors();
  r.encoderGeneration = { before: genBefore, after: (await supStatus())?.encoder?.generation ?? null };
  r.samePid = supBefore.length === 1 && r.supervisorAfter.length === 1 && supBefore[0] === r.supervisorAfter[0];
  r.ytdlpProcessesAtTrigger = ytBefore;
  r.ytdlpProcessesAfter = ytAfter;
  r.ytdlpSurvived = ytBefore.filter((p) => ytAfter.includes(p));
  r.gap = gapStats(t - 5000, Date.now());
  r.swap = {
    oldMarkerGone: !existsSync(join(ROOT3, 'app', 'old-app-marker')),
    updateTmpGone: !existsSync(tmp), updateOldGone: !existsSync(join(ROOT3, '_update_old')),
    startBatNew: existsSync(join(ROOT3, 'START.bat.new')),
    configKept: Boolean(JSON.parse(readFileSync(join(ROOT3, 'app', 'config.json'), 'utf8')).supervisor.streamKeyProtected),
    consoleSaysApplied: readFileSync(join(OUT, 'console-3-first.log'), 'utf8').includes('Update applied successfully.'),
  };
  r.streamAfter = (await nodeStatus())?.stream;
  r.ytdlpAfter = hashTree(join(ROOT3, 'yt-dlp'));
  r.ytdlpUnchanged = r.ytdlpAfter.sha256 === r.ytdlpBefore.sha256 && r.ytdlpBefore.sha256 === results.ytdlpAtInstall.sha256;
  note('step 2 swap', r);

  // The installed updater on GitHub's real release list (one read-only GET) plus a made-up 3.x prerelease.
  const real = await fetch(RELEASES_API, { headers: { 'User-Agent': 'streamloop-install-session', Accept: 'application/vnd.github.v3+json' }, signal: AbortSignal.timeout(15_000) });
  const list = await real.json();
  writeFileSync(join(OUT, 'releases-real.json'), JSON.stringify(list.map((x) => ({ tag_name: x.tag_name, draft: x.draft, prerelease: x.prerelease, assets: x.assets.map((a) => ({ name: a.name, browser_download_url: a.browser_download_url })) })), null, 2));
  const dl1 = (n) => ({ name: n, browser_download_url: `https://github.com/Sponsorn/streamloop/releases/download/v3.0.1/${n}` });
  const fake = { tag_name: 'v3.0.1', draft: false, prerelease: true, assets: ['streamloop-v3.0.1.zip', 'streamloop-v3.0.1.sha256', 'streamloop-v3.0.1-update.zip', 'streamloop-v3.0.1-update.sha256', 'node-version.txt'].map(dl1) };
  writeFileSync(join(OUT, 'releases-mocked.json'), JSON.stringify([fake, ...JSON.parse(readFileSync(join(OUT, 'releases-real.json'), 'utf8'))], null, 2));
  // fetch is stubbed inside that process: the list comes from the files, node-version.txt says 22.23.4.
  const code = `(async () => {
    const { readFileSync } = await import('node:fs');
    const lists = { real: JSON.parse(readFileSync(process.env.REAL, 'utf8')), mocked: JSON.parse(readFileSync(process.env.MOCKED, 'utf8')) };
    const calls = [];
    globalThis.fetch = async (url) => {
      calls.push(String(url));
      if (String(url).includes('api.github.com')) return new Response(JSON.stringify(lists[process.env.LIST]), { status: 200 });
      if (String(url).endsWith('node-version.txt')) return new Response('22.23.4');
      return new Response('', { status: 404 });
    };
    const m = await import(process.env.MOD);
    const out = { pickReal: m.pickRelease(lists.real, process.env.V)?.tag_name ?? null, pickMocked: m.pickRelease(lists.mocked, process.env.V)?.tag_name ?? null, pickMockedAs238: m.pickRelease(lists.mocked, '2.3.8')?.tag_name ?? null };
    for (const name of ['real', 'mocked']) { process.env.LIST = name; out[name] = await new m.Updater().checkForUpdate(); }
    out.fetched = calls;
    console.log(JSON.stringify(out));
    process.exit(0);
  })()`;
  const upd = await tsxEval(ROOT3, code, {
    REAL: join(OUT, 'releases-real.json'), MOCKED: join(OUT, 'releases-mocked.json'), V: VERSION,
    MOD: pathToFileURL(join(ROOT3, 'app', 'src', 'server', 'updater.ts')).href,
  });
  let parsed = null;
  try { parsed = JSON.parse(upd.stdout.split(/\r?\n/).find((l) => l.startsWith('{"pickReal'))); } catch { /* reported raw */ }
  r.updater = { realReleases: list.map((x) => `${x.tag_name}${x.prerelease ? ' (pre)' : ''}`), result: parsed ?? upd };
  note('step 2 updater', r.updater);
  results.step2 = r;
}

async function step3() {
  const r = {};
  const tStop = Date.now();
  r.stop = (await api('POST', '/stream/stop')).status;
  const gone = await waitFor('the supervisor to exit after Stop', async () => (await supervisors()).length === 0, 60_000, 500);
  r.supervisorExitMs = gone.ms;
  r.markerAfterStop = existsSync(join(ROOT3, 'data', 'stopped-by-user'));
  // End 3.0's Node by killing its launcher tree; the supervisor is already gone.
  const kill = await run('taskkill', ['/F', '/T', '/PID', String(bat3.pid)], { encoding: 'utf8' });
  r.treeKill = kill.stdout.trim().split(/\r?\n/);
  await waitFor('3.0 Node gone', async () => !(await ps()).some(isTestNode), 30_000, 500);
  r.after = { supervisors: await supervisors(), port4417: await portOpen(4417), port7654: await portOpen(7654) };
  note('3.0 stopped and its Node killed', r);

  // v2.3.8, read-only download, checked against its .sha256 asset
  mkdirSync(V2_CACHE, { recursive: true });
  const zip = join(V2_CACHE, `streamloop-${V2_TAG}.zip`);
  const shaText = await (await fetch(V2_URL.replace(/\.zip$/, '.sha256'), { signal: AbortSignal.timeout(30_000) })).text();
  const want = shaText.trim().split(/\s+/)[0].toLowerCase();
  if (!existsSync(zip) || sha256File(zip) !== want) {
    const res = await fetch(V2_URL, { signal: AbortSignal.timeout(600_000) });
    writeFileSync(zip, Buffer.from(await res.arrayBuffer()));
  }
  const got = sha256File(zip);
  r.v2 = { url: V2_URL, bytes: statSync(zip).size, sha256: got, expected: want, match: got === want };
  if (!r.v2.match) throw new Stop('v2.3.8 checksum mismatch');
  r.v2.extractMs = await extract(zip, S2);
  const root2 = existsSync(join(S2, 'streamloop', 'START.bat')) ? join(S2, 'streamloop') : S2;
  r.v2.root = root2;
  const cfg2 = join(root2, 'app', 'config.json');
  if (!existsSync(cfg2)) writeFileSync(cfg2, readFileSync(join(root2, 'app', 'config.example.json')));
  quietConfig(cfg2);
  bat2 = startBat(root2, '2x');
  r.v2.nodeUpMs = await nodeUp('2.x');
  const st2 = await nodeStatus();
  r.v2.dashboard = (await fetch('http://127.0.0.1:7654/admin/', { signal: AbortSignal.timeout(5000) })).status;
  r.v2.version = (await api('GET', '/update/status')).json?.currentVersion ?? null;
  r.v2.statusKeys = st2 ? Object.keys(st2) : null;
  r.v2.mpvOrObs = (await ps()).filter((p) => inInstall(p) && !['node.exe', 'cmd.exe'].includes(p.Name)).map((p) => p.Name);
  // 3.0's encoder is down, so the guard must let 2.x stream.
  r.v2.guard = await guard2(root2);
  note('2.x up', r.v2);
  const shut = await api('POST', '/shutdown');
  r.v2.shutdown = shut.status;
  const exited = await waitFor('2.x START.bat to end', async () => bat2.exitCode !== null, 30_000, 500);
  r.v2.batExitMs = exited.ms;
  r.v2.batExitCode = bat2.exitCode;
  r.v2.port7654After = await portOpen(7654);
  r.v2.consoleSaysClean = readFileSync(join(OUT, 'console-2x.log'), 'utf8').includes('Server stopped cleanly.');

  // 3.0 again: Stop holds across the restart until Start.
  bat3 = startBat(ROOT3, '3-again', ENV3);
  await nodeUp('3.0 again');
  const samples = [];
  for (let i = 0; i < 6; i += 1) { await sleep(5000); samples.push({ supervisors: (await supervisors()).length, stream: (await nodeStatus())?.stream?.state }); }
  r.whileStopped = samples;
  r.stopHeld = samples.every((x) => x.supervisors === 0 && x.stream === 'stopped');
  r.start = (await api('POST', '/stream/start')).status;
  const live = await waitLive('live after Start');
  r.startToLiveMs = live.ms;
  r.markerAfterStart = existsSync(join(ROOT3, 'data', 'stopped-by-user'));
  await sleep(20_000);
  r.readerAfterStart = gapStats(Date.now() - 20_000, Date.now());
  // The same guard while 3.0 is live: it must hold 2.x off.
  r.guardWhile3Live = await guard2(r.v2.root);
  r.stopToRestartMs = Date.now() - tStop;
  note('step 3', r);
  results.step3 = r;
}

// the gate: Discord catcher, supervisor events, faults
const CATCHER_PORT = 8790;
const WEBHOOK = `http://127.0.0.1:${CATCHER_PORT}/webhook/gate`;
const alerts = [];
let catcher = null;
if (GATE) {
  catcher = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      const embed = JSON.parse(body || '{}').embeds?.[0] ?? {};
      // a batch prefixes each message with its level's emoji
      const messages = String(embed.description ?? '').split('\n\n').map((m) => m.replace(/^(ℹ️|⚠️|\u{1F6A8}) /u, '')).filter(Boolean);
      const row = { ms: Date.now(), at: iso(), title: embed.title, footer: embed.footer?.text, messages };
      alerts.push(row);
      appendFileSync(join(OUT, 'catcher.jsonl'), `${JSON.stringify(row)}\n`);
      res.writeHead(204).end();
    });
  });
  await new Promise((r) => catcher.listen(CATCHER_PORT, '127.0.0.1', r));
}
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
function alertSummary() {
  const messages = alerts.flatMap((a) => a.messages.map((m) => ({ ms: a.ms, m })));
  const dupes = messages.filter((x, i) => messages.some((y, j) => j < i && y.m === x.m && x.ms - y.ms < 120_000));
  return { posts: alerts.length, messages: messages.length, batched: alerts.filter((a) => a.messages.length > 1).length, duplicatesWithin2Min: dupes.map((d) => d.m) };
}

function supEvents() {
  const dir = join(ROOT3, 'data', 'logs');
  if (!existsSync(dir)) return [];
  return readdirSync(dir).filter((n) => /^events-\d{4}-\d{2}-\d{2}\.jsonl$/.test(n)).sort()
    .flatMap((n) => readFileSync(join(dir, n), 'utf8').split(/\r?\n/).filter(Boolean))
    .map((l) => { try { const e = JSON.parse(l); return { ...e, ms: Date.parse(e.ts) }; } catch { return null; } }).filter(Boolean);
}
const eventsSince = (ms, kinds) => supEvents().filter((e) => e.ms >= ms && (!kinds || kinds.includes(e.kind)));
const QUIET = ['handover', 'spare-resolve', 'spare-used', 'youtube-probe', 'download-cancel'];
const brief = (e) => Object.fromEntries(Object.entries(e).filter(([k]) => !['level', 'at', 'ms'].includes(k)));
const eventsOf = (ms) => eventsSince(ms).filter((e) => !QUIET.includes(e.kind)).map(brief);
const firstEvent = (from, kind, timeoutMs, where = () => true) => waitFor(kind, async () => eventsSince(from, [kind]).find(where), timeoutMs, 500);

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
    const r = await run(join(ROOT3, 'ffmpeg', 'ffmpeg.exe'), ['-hide_banner', '-loglevel', 'error', '-i', READ_URL, '-frames:v', '1', '-vf', `${crop},format=gray`, '-f', 'rawvideo', '-'], { timeout: 30_000, encoding: 'buffer' });
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
function verdict(name, r, checks) {
  r.checks = checks;
  r.pass = Object.values(checks).every(Boolean);
  results.faults[name] = r;
  note(`${name}: ${r.pass ? 'PASS' : 'FAIL'}`, r);
  if (!r.pass) throw new Stop(`${name} failed: ${Object.keys(checks).filter((k) => !checks[k]).join(', ')}`);
}

async function faults() {
  results.faults = {};
  // Past the cold start's URL feeder before anything is injected.
  await sleep(30_000);
  results.alertsBeforeFaults = alertCheck(0, Date.now(), {});

  // 1. kill-feeder: a retry in place, covered by the read-ahead.
  {
    const room = await roomLeft(60, true);
    const f = await fault('kill-feeder');
    const retry = await firstEvent(f.ms, 'retry', 30_000);
    await sleep(20_000);
    const live = await waitFor('live after kill-feeder', liveNow, 30_000, 1000);
    const r = { at: iso(f.ms), playing: f.playing, retriesBefore: room.retries, reply: f.reply, retryMs: retry.value ? retry.value.ms - f.ms : null, retry: retry.value && brief(retry.value),
      events: eventsOf(f.ms), gap: gapStats(f.ms - 5000, Date.now()), alerts: alertCheck(f.ms, Date.now(), { error: 1 }) };
    verdict('kill-feeder', r, { accepted: f.reply?.accepted === true, retry: Boolean(retry.value), live: Boolean(live.value),
      gapUnder2s: r.gap.maxWallGapMs < 2000 && r.gap.readerReconnects === 0, oneErrorAlert: r.alerts.ok });
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
    const r = { at: iso(f.ms), playing: f.playing, retriesBefore: room.retries, reply: f.reply, watchdog: wd.value && brief(wd.value), faultToVerdictMs: wd.value ? wd.value.ms - f.ms : null,
      frozenOnAirToVerdictMs: onAir && wd.value ? wd.value.ms - onAir : null, retry: retry.value && brief(retry.value),
      pictureDiff: { before, frozen, after }, events: eventsOf(f.ms), gap: gapStats(f.ms - 5000, Date.now()), alerts: alertCheck(f.ms, Date.now(), { frozen: 1 }) };
    verdict('freeze-video', r, { accepted: f.reply?.accepted === true, verdictRetry: wd.value?.outcome === 'retry',
      // stage 2 run B acted 25.1 s after the frozen picture went out; 35 s allows for the reader's timing
      verdictInTime: r.frozenOnAirToVerdictMs !== null && r.frozenOnAirToVerdictMs <= 35_000, retried: Boolean(retry.value),
      movingAgain: after.at(-1) >= MOVING, live: await liveNow(), oneFrozenAlert: r.alerts.ok });
  }

  // 3. mute-audio: in 3.0.1 and 3.0.2 silence alone never acts, so the verdict is content and play goes on.
  {
    await roomLeft(120);
    const f = await fault('mute-audio');
    const wd = await firstEvent(f.ms, 'watchdog-silence', 150_000);
    await sleep(30_000);
    const restarts = eventsSince(f.ms, ['feeder-start']).filter((e) => e.reason === 'watchdog');
    const r = { at: iso(f.ms), playing: f.playing, reply: f.reply, watchdog: wd.value && brief(wd.value), faultToVerdictMs: wd.value ? wd.value.ms - f.ms : null,
      watchdogFeederStarts: restarts.map(brief), events: eventsOf(f.ms), gap: gapStats(f.ms - 5000, Date.now()), alerts: alertCheck(f.ms, Date.now(), {}) };
    verdict('mute-audio', r, { accepted: f.reply?.accepted === true, verdictContent: wd.value?.outcome === 'content', noWatchdogRestart: restarts.length === 0,
      noRetry: !r.events.some((e) => e.kind === 'retry'), live: await liveNow(), noGap: r.gap.maxWallGapMs < 2000 && r.gap.readerReconnects === 0, noAlert: r.alerts.ok });
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
    // Node says live only after the viewer has watched 10 s past the fault (the supervisor's step rule),
    // so on air is judged on the encoder and the reader, and Node's live time is recorded beside it.
    const r = { at: iso(f.ms), playing: f.playing, reply: f.reply, states, nodeLiveMs: back.value ? states.at(-1).ms : null,
      encoderStartMs: encStart ? encStart.ms - f.ms : null, outputBackMs: outputBack ? outputBack.ms - f.ms : null,
      generation: { before: gen, after: s?.encoder?.generation ?? null }, events: eventsOf(f.ms), gap: gapStats(f.ms - 5000, Date.now()),
      alerts: alertCheck(f.ms, Date.now(), { drop: 1, restart: 1 }) };
    verdict('kill-encoder', r, { accepted: f.reply?.accepted === true, sawRecovering: states.some((x) => x.state !== 'live'),
      onAirWithin10s: r.encoderStartMs !== null && r.encoderStartMs <= 10_000 && r.outputBackMs !== null && r.outputBackMs <= 10_000,
      nodeLive: back.value !== null, oneDropOneRestart: r.alerts.ok });
  }

  // 5. The supervisor process alone (no /T): its Job Object takes the children, Node relaunches it.
  {
    const [pid] = await supervisors();
    const playing = (await supStatus())?.playing ?? null;
    const t = Date.now();
    const kill = await run('taskkill', ['/F', '/PID', String(pid)], { encoding: 'utf8' });
    const relaunched = await waitFor('a new supervisor process', async () => (await supervisors()).some((p) => p !== pid), 60_000, 250);
    const orphans = (await ps()).filter((p) => p.ParentProcessId === pid).map((p) => `${p.Name} ${p.ProcessId}`);
    const live = await waitLive('live after the supervisor kill', 180_000);
    const liveMs = live.value ? Date.now() - t : null;
    const resume = await firstEvent(t, 'resume', 30_000);
    await sleep(20_000);
    const r = { at: iso(t), oldPid: pid, newPids: await supervisors(), kill: kill.stdout.trim(), playing, relaunchMs: relaunched.ms, orphansOfOldPid: orphans,
      liveMs, resume: resume.value && brief(resume.value), events: eventsOf(t), gap: gapStats(t - 5000, Date.now()),
      // the resume event's own alert may ride along with "back"
      alerts: alertCheck(t, Date.now(), { down: 1, back: 1 }, { resume: 1 }) };
    const order = r.alerts.messages.map((m) => m.kind);
    verdict('supervisor-kill', r, { relaunchWithin10s: relaunched.ms !== null && relaunched.ms <= 10_000, noOrphans: orphans.length === 0, live: Boolean(live.value),
      sameVideo: resume.value?.index === playing?.index && resume.value?.id === playing?.id, downThenBack: r.alerts.ok && order.indexOf('down') < order.indexOf('back') });
  }
}

async function cleanup() {
  readerStop = true;
  probe?.kill();
  if (await portOpen(7654)) {
    if (!token) await nodeUp('cleanup', 5000).catch(() => null);
    await api('POST', '/stream/stop');
    results.cleanupSupervisorExit = (await waitFor('the supervisor to exit', async () => (await supervisors()).length === 0, 60_000, 500)).ms;
    await api('POST', '/shutdown');
    await waitFor('Node to exit', async () => !(await ps()).some(isTestNode), 30_000, 500);
  }
  for (const b of [bat3, bat2]) if (b && b.exitCode === null) await run('taskkill', ['/F', '/T', '/PID', String(b.pid)]);
  // anything of ours still running from either install, then the reader and MediaMTX
  for (const p of (await ps()).filter((x) => inInstall(x) && x.Name !== 'cmd.exe')) await run('taskkill', ['/F', '/PID', String(p.ProcessId)]);
  mediamtx?.kill();
  await sleep(3000);
}

try {
  await step1();
  if (GATE) await faults();
  else { await step2(); await step3(); }
} catch (err) {
  results.failed = String(err?.stack ?? err);
  note('run stopped', { error: results.failed });
}
await cleanup();
if (existsSync(join(ROOT3, 'data', 'logs'))) cpSync(join(ROOT3, 'data', 'logs'), join(OUT, 'supervisor-logs'), { recursive: true });

const tl = await run('tasklist', ['/FO', 'CSV', '/NH'], { encoding: 'utf8' });
const names = ['streamloop-supervisor.exe', 'ffmpeg.exe', 'ffprobe.exe', 'yt-dlp.exe', 'mediamtx.exe', 'mpv.exe'];
results.tasklist = Object.fromEntries(names.map((n) => [n, tl.stdout.split(/\r?\n/).filter((l) => l.toLowerCase().startsWith(`"${n}"`)).length]));
results.testNodesLeft = (await ps()).filter(isTestNode).length;
results.tokenServerAfter = await tokenServerOk();
if (GATE) {
  results.alertsOverall = alertSummary();
  const inFault = (m) => Object.values(results.faults ?? {}).some((f) => f.alerts.messages.some((x) => x.at === m.at && x.m === m.m));
  results.alertsOutsideFaults = alertCheck(0, Date.now(), {}).messages.filter((m) => !inFault(m));
  note('alerts', { overall: results.alertsOverall, outsideFaults: results.alertsOutsideFaults });
  catcher.close();
}
results.keyGrep = { sl3: grepTree(S3, KEY), sl2: grepTree(S2, KEY), scriptOutput: grepTree(OUT, KEY) };
writeFileSync(join(OUT, 'results.json'), redact(JSON.stringify(results, null, 2), KEY));
note('key grep', results.keyGrep);
note('after cleanup', { tasklist: results.tasklist, testNodesLeft: results.testNodesLeft, tokenServer: results.tokenServerAfter });
for (const d of [S3, S2]) {
  try { rmSync(d, { recursive: true, force: true, maxRetries: 5, retryDelay: 1000 }); } catch (err) { note(`could not remove ${d}`, { error: String(err) }); }
}
note('installs removed', { sl3: !existsSync(S3), sl2: !existsSync(S2) });
process.exit(results.failed ? 1 : 0);
