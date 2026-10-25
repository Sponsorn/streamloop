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
// node-session.mjs runs its session on import, so the few helpers needed are repeated here.
// Nothing opens a window: every child is spawned hidden.
import * as cp from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync, readFileSync, existsSync, appendFileSync, readdirSync, statSync, openSync, closeSync, cpSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import net from 'node:net';
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
const S3 = join(tmpdir(), 'sl3 install Ö');
const S2 = join(tmpdir(), 'sl2 install');
const ROOT3 = join(S3, 'streamloop');
const OUT = join(REPO, 'supervisor', 'results', 'install');
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
const inInstall = (p) => /sl[23] install/.test(p.CommandLine ?? '');
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
const ENV3 = { STREAMLOOP_TEST_HOOKS: '1' };

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
  const save = await api('POST', '/config', { playlists: [{ id: PLAYLIST }], discord: { webhookUrl: '' }, supervisor: { ingestUrl: INGEST } });
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
  await step2();
  await step3();
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
results.keyGrep = { sl3: grepTree(S3, KEY), sl2: grepTree(S2, KEY), scriptOutput: grepTree(OUT, KEY) };
writeFileSync(join(OUT, 'results.json'), redact(JSON.stringify(results, null, 2), KEY));
note('key grep', results.keyGrep);
note('after cleanup', { tasklist: results.tasklist, testNodesLeft: results.testNodesLeft, tokenServer: results.tokenServerAfter });
for (const d of [S3, S2]) {
  try { rmSync(d, { recursive: true, force: true, maxRetries: 5, retryDelay: 1000 }); } catch (err) { note(`could not remove ${d}`, { error: String(err) }); }
}
note('installs removed', { sl3: !existsSync(S3), sl2: !existsSync(S2) });
process.exit(results.failed ? 1 : 0);
