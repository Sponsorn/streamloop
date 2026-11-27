// The end-to-end proof: streamloop.exe's control process drives its supervisor against local
// MediaMTX. The control process runs from a launcher console (a hidden cmd running launch.bat, like
// START.bat) as ROOT\streamloop.exe, so it and the supervisor read the same ROOT\app\config.json.
// Nothing opens a window: every process is spawned hidden, never through `start`.
//   node node-session.mjs A|B|C|treekill|smoke [--port N]
// A: 30 min through the control process: the wizard's API calls, dashboard controls, freeze / feeder /
//    encoder faults, Discord alerts caught by a local webhook.
// B: control process lifecycle: crash, tree kill, exit 75, console event, supervisor killed, Stop / Start.
// C: the Twitch read-back against local HLS (STREAMLOOP_TEST_HLS_URL); nothing reaches Twitch.
// treekill: the supervisor started through `launch` has a dead parent and outlives taskkill /T /F on
//    the control process.
// --port: the dashboard API port written to the fresh config (default 7654), for when 7654 is taken.
// Processes are told apart by command line: ROOT\streamloop.exe with `supervisor` is the core, with
// `launch` the intermediate, with `control` the control process (launch.bat names the mode, since a plain
// streamloop.exe outside START.bat opens the app).
// Faults go to the supervisor's POST /debug/fault (STREAMLOOP_DEBUG_FAULTS=1 has the control process
// start it with --debug-faults).
// The dummy key is never printed: the script's own notes go through redact(); what the control process
// sends (replies, alerts, read-back status) is stored as received, so the key grep, which also reads its
// log in app\logs, can catch a leak. Each run ends with its verdict: one line per check, then PASS or FAIL.
import * as cp from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync, readFileSync, existsSync, appendFileSync, readdirSync, createWriteStream, cpSync } from 'node:fs';
import { join, relative, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import http from 'node:http';
import net from 'node:net';
import { dailyFiles, EVENTS, TOKEN_PORT, TOKEN_PORT_TAKEN, FFMPEG, FFPROBE, PLAYLIST, redact, leaks, withModes, autostartSandbox } from './lib.mjs';
import { makeDevRoot } from './dev-root.mjs';

// Every child hidden: nothing this script starts may show a window.
const spawn = (cmd, args, opts = {}) => cp.spawn(cmd, args, { windowsHide: true, ...opts });
// Never spawnSync: a blocked event loop stalls the reader (false gaps) and the HLS proxy.
const run = (cmd, args, opts = {}) => new Promise((resolve) => {
  cp.execFile(cmd, args, { windowsHide: true, maxBuffer: 64 * 1024 * 1024, ...opts }, (err, stdout) => resolve({ status: err ? (err.code ?? 1) : 0, stdout }));
});
const RUN = process.argv[2];
const portArg = process.argv.indexOf('--port');
const PORT = portArg > 0 ? Number(process.argv[portArg + 1]) : 7654;
if (!['A', 'B', 'C', 'freeze', 'treekill', 'smoke'].includes(RUN) || !Number.isInteger(PORT)) {
  console.error('usage: node node-session.mjs A|B|C|freeze|treekill|smoke [--port N]');
  process.exit(2);
}
const REPO = fileURLToPath(new URL('../..', import.meta.url));
// Autostart goes to a test key and a temp Startup folder, never the user's own entries.
autostartSandbox('node');
const TOOLS = join(REPO, 'spikes', 'tools');
const KEY = 'live_DUMMYKEY_stage3';
const API = `http://127.0.0.1:${PORT}/api`;
const SUP = 'http://127.0.0.1:4417';
const CATCHER_PORT = 8790;
const PROXY_PORT = 8899;
const UPSTREAM_HLS = `http://127.0.0.1:8888/live/${KEY}`;
const READ_URL = `rtmp://127.0.0.1:1935/live/${KEY}`;
const OUT = join(REPO, 'supervisor', 'results', `node-${RUN}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const iso = (ms = Date.now()) => new Date(ms).toISOString();

rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });
const note = (msg, data) => {
  const line = redact(`${iso()} ${msg}${data === undefined ? '' : ` ${JSON.stringify(data)}`}`, KEY);
  console.log(line);
  appendFileSync(join(OUT, 'session.log'), `${line}\n`);
};
const save = (file, value) => writeFileSync(join(OUT, file), redact(typeof value === 'string' ? value : JSON.stringify(value, null, 2), KEY));
const results = {};

// preflight
const httpOk = async (url) => { try { return (await fetch(url, { signal: AbortSignal.timeout(5000) })).ok; } catch { return false; } };
const portOpen = (port) => new Promise((resolve) => {
  const s = net.connect(port, '127.0.0.1');
  s.on('connect', () => { s.destroy(); resolve(true); });
  s.on('error', () => resolve(false));
});
const rtmpUp = () => portOpen(1935);
// Another session's supervisor or control process would be driven, and stopped by stopAll, as this run's.
for (const port of [4417, PORT]) if (await portOpen(port)) { note(`${port} already answers, so another session's StreamLoop runs: stop`); process.exit(2); }
if (await portOpen(TOKEN_PORT)) { note(TOKEN_PORT_TAKEN); process.exit(2); }
note(`preflight: ${TOKEN_PORT}, 4417 and the API port free; the supervisor runs its own token server`);
let mediamtx = null;
if (!(await rtmpUp())) {
  mediamtx = spawn(join(TOOLS, 'mediamtx.exe'), [], { cwd: TOOLS, stdio: 'ignore' });
  await sleep(3000);
  if (!(await rtmpUp())) { note('MediaMTX did not open :1935'); process.exit(2); }
  note('MediaMTX started');
}

const ROOT = makeDevRoot();
const APP = join(ROOT, 'app');
const DATA = join(ROOT, 'data');
const EXE = join(ROOT, 'streamloop.exe');
rmSync(DATA, { recursive: true, force: true });
rmSync(join(APP, 'logs'), { recursive: true, force: true });

// A first-run config (the example's placeholder playlist). Updates and the browser are off so a
// run never touches GitHub, swaps the repo's yt-dlp or opens a window.
const fresh = JSON.parse(readFileSync(join(REPO, 'config.example.json'), 'utf8'));
Object.assign(fresh, { port: PORT, autoUpdateCheck: false, autoUpdateYtdlp: false, autoOpenAdmin: false });
if (RUN === 'C' || RUN === 'freeze') {
  // Helix credentials stay empty: STREAMLOOP_TEST_HLS_URL stubs "channel is live".
  Object.assign(fresh, { twitchLivenessEnabled: true, twitchChannel: 'stage3test' });
  // The supervisor's own watchdog must not see the freeze, so only the read-back can.
  Object.assign(fresh.supervisor, { freezeSeconds: 3600, silenceSeconds: 3600 });
}
writeFileSync(join(APP, 'config.json'), JSON.stringify(fresh, null, 2));
note('fresh first-run config', { root: ROOT, run: RUN, port: PORT });

// Discord catcher
const alerts = [];
const catcher = http.createServer((req, res) => {
  let body = '';
  req.on('data', (c) => { body += c; });
  req.on('end', () => {
    const embed = JSON.parse(body || '{}').embeds?.[0] ?? {};
    // a batch prefixes each message with its level's emoji
    const messages = String(embed.description ?? '').split('\n\n').map((m) => m.replace(/^(ℹ️|⚠️|\u{1F6A8}) /u, '')).filter(Boolean);
    const row = { ms: Date.now(), at: iso(), title: embed.title, footer: embed.footer?.text, messages, roleContent: JSON.parse(body || '{}').content ?? null };
    alerts.push(row);
    appendFileSync(join(OUT, 'catcher.jsonl'), `${JSON.stringify(row)}\n`);
    res.writeHead(204).end();
  });
});
await new Promise((r) => catcher.listen(CATCHER_PORT, '127.0.0.1', r));
const WEBHOOK = `http://127.0.0.1:${CATCHER_PORT}/webhook/${RUN}`;

// processes
async function ps() {
  const r = await run('powershell', ['-NoProfile', '-Command',
    "Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('node.exe','cmd.exe','streamloop.exe','ffmpeg.exe','ffprobe.exe','yt-dlp.exe','streamloop-pot.exe') } | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine | ConvertTo-Json"],
    { encoding: 'utf8', windowsHide: true });
  const v = JSON.parse(r.stdout || '[]');
  return withModes(Array.isArray(v) ? v : [v]);
}
const samePath = (a, b) => String(a ?? '').toLowerCase().replaceAll('/', '\\') === String(b ?? '').toLowerCase().replaceAll('/', '\\');
/** This root's streamloop.exe, in any mode; another install's or worktree's is never counted. */
const isOurs = (p) => p.Name === 'streamloop.exe' && samePath(p.ExecutablePath, EXE);
const isControl = (p) => isOurs(p) && p.mode === 'control';
const isCore = (p) => isOurs(p) && p.mode === 'supervisor';
const BAT = join(ROOT, 'launch.bat');
const launcherCmd = async () => (await ps()).find((p) => p.Name === 'cmd.exe' && (p.CommandLine ?? '').includes(BAT));
const supervisors = async () => (await ps()).filter(isCore).map((p) => p.ProcessId);

// The launcher console: a hidden cmd (it gets its own console host) running launch.bat, which
// runs the control process and records its exit code. The control process inherits cmd's NUL, as under
// a hidden START.bat, so it attaches to cmd's console; a file redirect would keep it off, out of run B's
// console event. Its lines are in app\logs.
const PATH_NO_FFMPEG = (process.env.PATH ?? '').split(';').filter((d) => !/chocolatey\\bin|ffmpeg/i.test(d)).join(';');
const controlEnv = [
  `set "STREAMLOOP_ROOT=${ROOT}"`,
  'set "STREAMLOOP_DEBUG_FAULTS=1"',
  'set "STREAMLOOP_TEST_HOOKS=1"',
  `set "STREAMLOOP_TEST_AUTOSTART_KEY=${process.env.STREAMLOOP_TEST_AUTOSTART_KEY}"`,
  `set "STREAMLOOP_TEST_STARTUP_DIR=${process.env.STREAMLOOP_TEST_STARTUP_DIR}"`,
  RUN === 'C' ? `set "STREAMLOOP_TEST_HLS_URL=http://127.0.0.1:${PROXY_PORT}/index.m3u8"` : 'set "STREAMLOOP_TEST_HLS_URL="',
  `set "PATH=${PATH_NO_FFMPEG}"`,
];
writeFileSync(BAT, ['@echo off', 'title StreamLoop end-to-end test', ...controlEnv, `cd /d "${ROOT}"`,
  `"${EXE}" control`,
  `echo %DATE% %TIME% control exited %ERRORLEVEL% >> "${join(OUT, 'control-exits.log')}"`, ''].join('\r\n'));

let token = '';
async function startControl(label) {
  const t0 = Date.now();
  spawn('cmd.exe', ['/d', '/c', BAT], { windowsHide: true, stdio: 'ignore' }).unref();
  for (;;) {
    try {
      const r = await fetch(`${API}/token`, { signal: AbortSignal.timeout(2000) });
      if (r.ok) { token = (await r.json()).token; break; }
    } catch { /* not up yet */ }
    if (Date.now() - t0 > 90_000) throw new Error('The control process did not answer within 90 s');
    await sleep(500);
  }
  note(`control process up (${label})`, { ms: Date.now() - t0 });
  return t0;
}

async function api(method, path, body) {
  const headers = method === 'POST' ? { 'x-api-token': token, 'content-type': 'application/json' } : {};
  let status = 0;
  let text = '';
  try {
    const res = await fetch(API + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(30_000) });
    status = res.status;
    text = await res.text();
  } catch (err) { text = String(err); }
  // The stream key is the one request body not kept: the log records only that it was sent.
  const recorded = path === '/stream-key' ? '[stream key, not recorded]' : body;
  // Not redacted, so the key grep catches the control process echoing the key in a reply.
  appendFileSync(join(OUT, 'requests.jsonl'), `${JSON.stringify({ at: iso(), method, path, request: recorded ?? null, status, response: text.slice(0, 4000) })}\n`);
  let json = null;
  try { json = JSON.parse(text); } catch { /* not json */ }
  return { status, json };
}
const controlStatus = async () => (await api('GET', '/status')).json;
const supStatus = async () => { try { return await (await fetch(`${SUP}/status`, { signal: AbortSignal.timeout(3000) })).json(); } catch { return null; } };

async function waitFor(what, fn, timeoutMs, everyMs = 1000) {
  const t0 = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return { ms: Date.now() - t0, value: v };
    if (Date.now() - t0 > timeoutMs) { note(`timed out waiting for ${what}`, { timeoutMs }); return { ms: null, value: null }; }
    await sleep(everyMs);
  }
}

/** The dashboard wizard's calls, in its order: token, status, config, save, stream key, poll status. */
async function wizard() {
  const t0 = Date.now();
  token = (await api('GET', '/token')).json.token;
  const first = await controlStatus();
  await api('GET', '/config');
  const saved = await api('POST', '/config', { playlists: [{ id: PLAYLIST }], discord: { webhookUrl: WEBHOOK }, supervisor: { ingestUrl: 'rtmp://127.0.0.1:1935/live' } });
  const key = await api('POST', '/stream-key', { key: KEY });
  const live = await waitFor('live after the wizard', async () => (await controlStatus())?.stream?.state === 'live', 180_000, 2000);
  results.tokenServer = (await supStatus())?.tokenServer ?? null;
  note('token server', results.tokenServer);
  const r = { firstRun: first?.firstRun, saveStatus: saved.status, reloadError: saved.json?.reloadError ?? null, keyStatus: key.status, keyReply: key.json, msToLive: live.ms, totalMs: Date.now() - t0 };
  note('wizard done', r);
  return r;
}

// reader: one ffprobe on the RTMP output, reconnecting, every packet kept in memory
const packets = [];
let probe = null;
const reconnects = [];
let readerStop = false;
const readerLog = createWriteStream(join(OUT, 'reader.csv'));
function runReader() {
  if (readerStop) return;
  const segment = reconnects.length;
  reconnects.push(Date.now());
  probe = spawn(FFPROBE, ['-v', 'error', '-show_entries', 'packet=stream_index,dts_time', '-of', 'csv=p=0', READ_URL], { stdio: ['ignore', 'pipe', 'ignore'] });
  let tail = '';
  probe.stdout.on('data', (chunk) => {
    const lines = (tail + chunk).split(/\r?\n/);
    tail = lines.pop();
    const now = Date.now();
    for (const line of lines) {
      const [si, dts] = line.split(',');
      if (!dts || dts === 'N/A') continue;
      packets.push({ ms: now, s: Number(si), dts: Number(dts), seg: segment });
      readerLog.write(`${now},${Number(dts).toFixed(3)},${segment}\n`);
    }
  });
  probe.on('close', () => { if (!readerStop) sleep(1000).then(runReader); });
}
let recStop = false;
function runRecorder() {
  if (recStop) return;
  mkdirSync(join(OUT, 'rec'), { recursive: true });
  const rec = spawn(FFMPEG, ['-v', 'error', '-i', READ_URL, '-c', 'copy', '-copyts', join(OUT, 'rec', `${Date.now()}.mkv`)], { stdio: 'ignore' });
  rec.on('close', () => { if (!recStop) sleep(1000).then(runRecorder); });
}

/** What a viewer got in [fromMs, toMs]: reader reconnects, the longest stretch with no packet, the window's
 *  edges included, and the largest video dts step (a cut or a restart shows as a jump). */
function gapStats(fromMs, toMs) {
  const win = packets.filter((p) => p.ms >= fromMs && p.ms <= toMs);
  const end = Math.min(toMs, Date.now());
  let maxWallGapMs = win.length ? Math.max(win[0].ms - fromMs, end - win.at(-1).ms) : end - fromMs;
  for (let i = 1; i < win.length; i += 1) maxWallGapMs = Math.max(maxWallGapMs, win[i].ms - win[i - 1].ms);
  const video = win.filter((p) => p.s === 0);
  let maxDtsStep = 0;
  let dtsBackwards = 0;
  for (let i = 1; i < video.length; i += 1) {
    if (video[i].seg !== video[i - 1].seg) continue;
    const step = video[i].dts - video[i - 1].dts;
    if (step < 0) dtsBackwards += 1;
    maxDtsStep = Math.max(maxDtsStep, step);
  }
  const first = win[0]?.ms ?? null;
  return {
    packets: win.length, readerReconnects: reconnects.filter((t) => t > fromMs && t <= toMs).length,
    firstPacketLateMs: first === null ? null : first - fromMs, maxWallGapMs, maxVideoDtsStepS: Math.round(maxDtsStep * 1000) / 1000, dtsBackwards,
  };
}

// supervisor side
function supEvents() {
  const dir = join(DATA, 'logs');
  if (!existsSync(dir)) return [];
  return dailyFiles(readdirSync(dir), EVENTS, 'jsonl')
    .flatMap((n) => readFileSync(join(dir, n), 'utf8').split(/\r?\n/).filter(Boolean))
    .map((l) => { try { const e = JSON.parse(l); return { ...e, ms: Date.parse(e.ts) }; } catch { return null; } }).filter(Boolean);
}
const eventsSince = (ms, kinds) => supEvents().filter((e) => e.ms >= ms && (!kinds || kinds.includes(e.kind)));

async function fault(kind) {
  const before = (await api('GET', '/state')).json;
  let reply;
  try {
    reply = await (await fetch(`${SUP}/debug/fault`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ kind }) })).json();
  } catch (err) { reply = { error: String(err) }; }
  appendFileSync(join(OUT, 'drive.csv'), `${Date.now()},${kind},${JSON.stringify({ replyMs: Date.now(), before, reply })}\n`);
  note(`fault ${kind}`, { before, reply });
  return { ms: Date.now(), before, reply };
}

/** A dashboard control (/api/player/*); waits until /api/state shows the expected video. */
async function control(cmd, index) {
  const before = (await api('GET', '/state')).json;
  const path = cmd === 'jump' ? '/player/jump' : `/player/${cmd}`;
  const reply = await api('POST', path, cmd === 'jump' ? { index } : undefined);
  const replyMs = Date.now();
  const want = cmd === 'jump' ? index : cmd === 'next' ? before.index + 1 : cmd === 'prev' ? Math.max(0, before.index - 1) : before.index;
  const seen = await waitFor(`${cmd} to show`, async () => {
    const s = (await api('GET', '/state')).json;
    const ok = s?.index === want && s.source !== 'slate' && (cmd !== 'restart' || s.position < 15);
    return ok ? s : null;
  }, 30_000, 250);
  const row = { replyMs, before, reply: { status: reply.status, body: reply.json }, statusMatchMs: seen.ms, after: seen.value };
  appendFileSync(join(OUT, 'drive.csv'), `${Date.now()},${cmd},${JSON.stringify(row)}\n`);
  note(`control ${cmd}${index === undefined ? '' : ` ${index}`}`, { status: reply.status, from: before.index, to: seen.value?.index ?? null, statusMatchMs: seen.ms });
  return row;
}

async function stopAll() {
  readerStop = true; recStop = true;
  note('stopping the stream and the control process');
  const t0 = Date.now();
  await api('POST', '/stream/stop');
  const gone = await waitFor('the supervisor to exit after Stop', async () => (await supervisors()).length === 0, 30_000);
  note('supervisor exited after Stop stream', { ms: gone.ms, sinceStopMs: Date.now() - t0 });
  results.stop = { supervisorExitMs: gone.ms };
  await api('POST', '/shutdown');
  results.stop.controlExitMs = (await waitFor('the control process to exit', async () => !(await ps()).some(isControl), 20_000)).ms;
  for (const p of (await ps()).filter(isOurs)) await run('taskkill', ['/F', '/PID', String(p.ProcessId)]);
  probe?.kill();
  await sleep(3000);
}

function grepTree(dir, needle, skip = () => false) {
  const hits = [];
  let files = 0;
  const walk = (d) => {
    for (const entry of readdirSync(d, { withFileTypes: true })) {
      const p = join(d, entry.name);
      if (skip(p)) continue;
      if (entry.isDirectory()) walk(p);
      else if (entry.isFile()) { files += 1; if (readFileSync(p).includes(needle)) hits.push(relative(dir, p)); }
    }
  };
  if (existsSync(dir)) walk(dir);
  return { files, hits };
}

function alertSummary() {
  const messages = alerts.flatMap((a) => a.messages.map((m) => ({ ms: a.ms, m })));
  const dupes = messages.filter((x, i) => messages.some((y, j) => j < i && y.m === x.m && x.ms - y.ms < 120_000));
  return { posts: alerts.length, messages: messages.length, batched: alerts.filter((a) => a.messages.length > 1).length, duplicatesWithin2Min: dupes.map((d) => d.m) };
}
const alertsBetween = (from, to) => alerts.filter((a) => a.ms >= from && a.ms <= to).map((a) => ({ at: a.at, messages: a.messages }));

// C: read-back test HLS behind a proxy, so the control process never sees the key
let frozenDir = null;
let proxy = null;
let keepWarm = null;
async function startProxy() {
  proxy = http.createServer(async (req, res) => {
    const u = new URL(req.url, 'http://x');
    const file = basename(u.pathname);
    const mode = frozenDir ? 'frozen' : 'live';
    let status = 502;
    let body = Buffer.alloc(0);
    if (frozenDir) {
      const f = join(frozenDir, file);
      if (existsSync(f)) { status = 200; body = readFileSync(f); } else status = 404;
    } else {
      try {
        const up = await fetch(`${UPSTREAM_HLS}/${file}${u.search}`, { signal: AbortSignal.timeout(15_000) });
        status = up.status;
        body = Buffer.from(await up.arrayBuffer());
      } catch { /* 502 */ }
    }
    appendFileSync(join(OUT, 'proxy.log'), `${iso()} ${mode} ${req.method} ${u.pathname} ${status} ${body.length}\n`);
    res.writeHead(status).end(body);
  });
  await new Promise((r) => proxy.listen(PROXY_PORT, '127.0.0.1', r));
  // MediaMTX closes an HLS muxer after 60 s without a request; the read-back asks every 5 min.
  keepWarm = setInterval(() => { fetch(`${UPSTREAM_HLS}/index.m3u8`).then((r) => r.text()).catch(() => null); }, 15_000);
}

/** A frozen picture at "Twitch": an 8 s copy of the live HLS, served in place of it from now on. */
async function freezeProxy(n) {
  const dir = join(OUT, `snap-${n}`);
  mkdirSync(dir, { recursive: true });
  const t0 = Date.now();
  const r = await run(FFMPEG, ['-v', 'error', '-i', `${UPSTREAM_HLS}/index.m3u8`, '-t', '8', '-c', 'copy', '-f', 'hls', '-hls_time', '2', '-hls_list_size', '0', '-hls_flags', 'omit_endlist', join(dir, 'index.m3u8')], { timeout: 60_000 });
  frozenDir = dir;
  note(`proxy frozen (${n})`, { snapshotMs: Date.now() - t0, exit: r.status, files: readdirSync(dir).length });
  return Date.now();
}
function unfreezeProxy(why) {
  frozenDir = null;
  note('proxy live again', { why });
}

/** Two clock-region grabs 5 s apart through the proxy, as the read-back does it (its 1080p clock region,
 *  `CLOCK_1080` in readback.rs: the overlay clock sits top left). */
async function grabDiff(crop = 'crop=104:32:16:62') {
  const grab = async () => {
    const r = await run(join(ROOT, 'ffmpeg', 'ffmpeg.exe'), ['-hide_banner', '-loglevel', 'error', '-i', `http://127.0.0.1:${PROXY_PORT}/index.m3u8`, '-frames:v', '1', '-vf', `${crop},format=gray`, '-f', 'rawvideo', '-'], { timeout: 30_000, encoding: 'buffer' });
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

const readbackRows = [];
let readbackTimer = null;
function watchReadback() {
  let last = null;
  readbackTimer = setInterval(async () => {
    const s = await controlStatus();
    const rb = s?.twitch?.readback;
    if (!rb || rb.lastCheckAt === last) return;
    last = rb.lastCheckAt;
    const row = { at: iso(), ms: Date.now(), ...rb, playing: s.supervisor?.playing ?? null, generation: s.supervisor?.encoder?.generation ?? null };
    readbackRows.push(row);
    appendFileSync(join(OUT, 'readback.jsonl'), `${JSON.stringify(row)}\n`);
    note('read-back', { state: rb.state, lastDiff: rb.lastDiff });
  }, 5000);
}
const readbackAfter = (ms, n, timeoutMs) => waitFor(`${n} read-back result(s)`, async () => {
  const rows = readbackRows.filter((r) => r.ms > ms);
  return rows.length >= n ? rows.slice(0, n) : null;
}, timeoutMs, 2000);

// runs
const minutes = (m) => m * 60_000;
async function until(t0, m) { const wait = t0 + minutes(m) - Date.now(); if (wait > 0) await sleep(wait); }

async function runA() {
  const controlStart = await startControl('A');
  results.wizard = await wizard();
  results.wizard.controlStartToLiveMs = Date.now() - controlStart;
  const t0 = Date.now();
  results.liveAt = iso(t0);
  runReader(); runRecorder();
  results.controls = [];
  await until(t0, 2); results.controls.push(await control('next'));
  await until(t0, 4); results.controls.push(await control('prev'));
  await until(t0, 6); results.controls.push(await control('jump', 3));
  await until(t0, 8); results.controls.push(await control('restart'));
  results.faults = {};
  for (const [m, kind] of [[11, 'freeze-video'], [16, 'kill-feeder'], [21, 'kill-encoder']]) {
    await until(t0, m);
    const f = await fault(kind);
    await sleep(minutes(3) - 10_000);
    results.faults[kind] = {
      at: iso(f.ms), ms: f.ms, reply: f.reply, before: f.before,
      events: eventsSince(f.ms).filter((e) => !['feeder-start', 'handover', 'spare-resolve', 'spare-used', 'youtube-probe', 'download-cancel'].includes(e.kind)).map((e) => ({ ts: e.ts, kind: e.kind, level: e.level, outcome: e.outcome, why: e.why, failures: e.failures, restarts: e.restarts })),
      alerts: alertsBetween(f.ms, Date.now()),
      gap: gapStats(f.ms - 5000, Date.now()),
    };
    note(`${kind} alerts`, results.faults[kind].alerts);
  }
  await until(t0, 25); results.controls.push(await control('next'));
  await until(t0, 30);
  results.wholeRun = gapStats(t0 + 10_000, Date.now());
  // everything but the encoder kill, which is the one fault that drops the RTMP connection
  results.beforeEncoderKill = gapStats(t0 + 10_000, results.faults['kill-encoder'].ms - 5000);
  results.encoderStarts = eventsSince(0, ['encoder-start']).map((e) => ({ ts: e.ts, generation: e.generation }));
  results.endStatus = await controlStatus();
  await stopAll();
}

async function controlCycle(label, kill) {
  const supBefore = (await supervisors());
  const tKill = Date.now();
  const killInfo = await kill();
  const controlGone = await waitFor(`the control process gone after ${label}`, async () => !(await ps()).some(isControl), 30_000, 500);
  await sleep(10_000);
  const supWhileControlDown = (await supervisors());
  const supAliveStatus = await supStatus();
  const tStart = await startControl(label);
  const attached = await waitFor('the launcher to attach', async () => (await controlStatus())?.launcher === 'running', 30_000, 500);
  const attachMs = attached.ms === null ? null : Date.now() - tStart;
  await sleep(15_000);
  const supAfter = (await supervisors());
  const status = await controlStatus();
  const r = {
    killInfo, controlGoneMs: controlGone.ms, supervisorsBefore: supBefore, supervisorsWhileControlDown: supWhileControlDown,
    supervisorStateWhileControlDown: supAliveStatus?.state ?? null, encoderAliveWhileControlDown: supAliveStatus?.encoder?.alive ?? null,
    controlRestartToAttachMs: attachMs, supervisorsAfter: supAfter,
    samePid: supBefore.length === 1 && supAfter.length === 1 && supBefore[0] === supAfter[0],
    streamAfter: status?.stream, supervisorStarts: eventsSince(tKill, ['start']).length,
    gap: gapStats(tKill - 5000, Date.now()),
  };
  note(`${label} result`, r);
  return r;
}

async function runB() {
  await startControl('B');
  results.wizard = await wizard();
  runReader();
  await sleep(minutes(2));
  const controlPids = async () => (await ps()).filter(isControl).map((p) => p.ProcessId);
  results.crash = await controlCycle('taskkill /F on the control process', async () => {
    const pids = await controlPids();
    for (const pid of pids) await run('taskkill', ['/F', '/PID', String(pid)]);
    return { pids };
  });
  await sleep(60_000);
  results.treeKill = await controlCycle('taskkill /F /T on the launcher console tree', async () => {
    const cmd = (await launcherCmd());
    const r = await run('taskkill', ['/F', '/T', '/PID', String(cmd?.ProcessId)], { encoding: 'utf8' });
    return { cmdPid: cmd?.ProcessId ?? null, output: r.stdout.trim().split(/\r?\n/) };
  });
  await sleep(60_000);
  results.exit75 = await controlCycle('exit 75 (update restart hook)', async () => {
    const r = await api('POST', '/test/exit-75');
    await sleep(5000);
    const exits = join(OUT, 'control-exits.log');
    return { status: r.status, exits: existsSync(exits) ? readFileSync(exits, 'utf8').trim().split(/\r?\n/).slice(-1) : null };
  });
  await sleep(60_000);
  // A hidden console has no window to close, so the console gets Ctrl+Break instead: like the close
  // event, it reaches every process attached to that console, and the supervisor has none.
  results.consoleClosed = await controlCycle('launcher console control event (Ctrl+Break to every process on it)', async () => {
    const cmd = (await launcherCmd());
    const outFile = join(OUT, 'console-close.txt');
    const script = [
      'Add-Type @"',
      'using System; using System.Runtime.InteropServices;',
      'public static class Con {',
      '  [DllImport("kernel32.dll")] public static extern bool FreeConsole();',
      '  [DllImport("kernel32.dll")] public static extern bool AttachConsole(uint pid);',
      '  [DllImport("kernel32.dll")] public static extern bool GenerateConsoleCtrlEvent(uint ev, uint group);',
      '}',
      '"@',
      '[Con]::FreeConsole() | Out-Null',
      `$ok = [Con]::AttachConsole([uint32]${cmd?.ProcessId})`,
      `"attach=$ok" | Out-File -Encoding ascii '${outFile}'`,
      // this PowerShell is on that console now, so it ends here too
      '[Con]::GenerateConsoleCtrlEvent(1, 0) | Out-Null',
    ].join('\n');
    await run('powershell', ['-NoProfile', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')], { windowsHide: true, timeout: 30_000 });
    return { cmdPid: cmd?.ProcessId ?? null, result: existsSync(outFile) ? readFileSync(outFile, 'utf8').trim() : null };
  });
  await sleep(60_000);

  // The supervisor killed under a running control process: relaunch within 10 s, resume at the saved point.
  const [supPid] = (await supervisors());
  const tKill = Date.now();
  await run('taskkill', ['/F', '/PID', String(supPid)]);
  const saved = existsSync(join(DATA, 'state.json')) ? JSON.parse(readFileSync(join(DATA, 'state.json'), 'utf8')) : null;
  const relaunched = await waitFor('a new supervisor process', async () => (await supervisors()).some((p) => p !== supPid), 60_000, 250);
  const answering = await waitFor('the new supervisor to answer', async () => (await supStatus()) !== null, 60_000, 250);
  const live = await waitFor('live after the supervisor kill', async () => (await controlStatus())?.stream?.state === 'live', 180_000, 1000);
  const resumed = eventsSince(tKill, ['resume'])[0] ?? null;
  const firstPlay = await supStatus();
  await sleep(20_000);
  results.supervisorKill = {
    oldPid: supPid, newPids: (await supervisors()), relaunchMs: relaunched.ms, answeringMs: answering.ms, liveMs: live.ms, saved,
    resumeEvent: resumed && { ts: resumed.ts, index: resumed.index, pos: resumed.pos, id: resumed.id }, playingWhenLive: firstPlay?.playing ?? null,
    launcherEvents: (await api('GET', '/events')).json?.filter((e) => Date.parse(e.timestamp) >= tKill).map((e) => e.message) ?? null,
    alerts: alertsBetween(tKill, Date.now()), gap: gapStats(tKill - 5000, Date.now()),
  };
  note('supervisor kill result', results.supervisorKill);
  await sleep(30_000);

  // Stop stream: the supervisor exits and stays down until Start.
  const tStop = Date.now();
  await api('POST', '/stream/stop');
  const exited = await waitFor('the supervisor to exit after Stop', async () => (await supervisors()).length === 0, 30_000, 500);
  const samples = [];
  for (let i = 0; i < 9; i += 1) { await sleep(5000); samples.push({ s: Math.round((Date.now() - tStop) / 1000), supervisors: (await supervisors()).length, stream: (await controlStatus())?.stream?.state }); }
  const tStart = Date.now();
  await api('POST', '/stream/start');
  const back = await waitFor('a supervisor after Start', async () => (await supervisors()).length === 1, 60_000, 250);
  const liveAgain = await waitFor('live after Start', async () => (await controlStatus())?.stream?.state === 'live', 180_000, 1000);
  results.stopStart = { exitMs: exited.ms, samplesWhileStopped: samples, startToProcessMs: back.ms, startToLiveMs: liveAgain.ms, supervisorsAfter: (await supervisors()), alerts: alertsBetween(tStop, Date.now()) };
  note('stop/start result', results.stopStart);
  await sleep(30_000);
  results.encoderStarts = eventsSince(0, ['encoder-start']).map((e) => ({ ts: e.ts, generation: e.generation }));
  await stopAll();
}

async function runC() {
  await startProxy();
  await startControl('C');
  results.wizard = await wizard();
  // taskkill /T walks ParentProcessId, so this says whether killing the control process's tree reaches the supervisor.
  const all = await ps();
  results.supervisorParent = all.filter(isCore)
    .map((p) => ({ pid: p.ProcessId, parentPid: p.ParentProcessId, parentAlive: all.some((x) => x.ProcessId === p.ParentProcessId), parentIsControl: all.some((n) => n.ProcessId === p.ParentProcessId && isControl(n)) }));
  note('supervisor parent', results.supervisorParent);
  runReader();
  watchReadback();
  // MediaMTX opens the HLS muxer on the first request.
  results.hlsWarm = await waitFor('MediaMTX HLS', async () => httpOk(`http://127.0.0.1:${PROXY_PORT}/index.m3u8`), 60_000, 2000);
  results.liveGrabDiff = await grabDiff();
  note('own grab through the proxy, live', { diff: results.liveGrabDiff });
  const first = await readbackAfter(0, 1, minutes(7));
  results.firstReadback = first.value;

  // C0: freeze-video freezes the content under the overlay; the clock keeps moving.
  const f0 = await fault('freeze-video');
  const c0 = await readbackAfter(f0.ms, 2, minutes(12));
  results.freezeVideo = {
    reply: f0.reply, reads: c0.value,
    restartRequested: eventsSince(f0.ms, ['encoder-restart-requested']).length,
    watchdog: eventsSince(f0.ms, ['watchdog-freeze']).map((e) => ({ ts: e.ts, outcome: e.outcome })),
    timeline: (await api('GET', '/events')).json?.filter((e) => Date.parse(e.timestamp) >= f0.ms).map((e) => e.message),
  };
  note('freeze-video result', results.freezeVideo);
  await control('next');

  // C1: the picture at "Twitch" freezes, clock included: two bad reads, one restart, moving again.
  const gen0 = (await supStatus())?.encoder?.generation;
  const tF1 = await freezeProxy(1);
  const req = await waitFor('restart-encoder from the read-back', async () => eventsSince(tF1, ['encoder-restart-requested'])[0], minutes(12), 2000);
  const started = await waitFor('the new encoder', async () => {
    const s = await supStatus();
    return s && s.encoder?.alive && s.encoder.generation !== gen0 ? s.encoder.generation : null;
  }, 60_000, 500);
  unfreezeProxy('the encoder restarted (the reconnect gives Twitch a fresh stream)');
  const tHeal = Date.now();
  await sleep(15_000);
  const ownDiff = await grabDiff();
  const healed = await readbackAfter(tHeal, 1, minutes(7));
  results.readbackRestart = {
    readsWhileFrozen: readbackRows.filter((r) => r.ms > tF1 && r.ms < (req.value?.ms ?? Date.now()) + 10_000),
    restartRequested: req.value && { ts: req.value.ts, reason: req.value.reason }, freezeToRestartMs: req.value ? req.value.ms - tF1 : null,
    restartRequests: eventsSince(tF1, ['encoder-restart-requested']).length,
    oldGeneration: gen0 ?? null, newGeneration: started.value, ownGrabDiffAfter: ownDiff, nextRead: healed.value,
    timeline: (await api('GET', '/events')).json?.filter((e) => Date.parse(e.timestamp) >= tF1).map((e) => e.message),
    alerts: alertsBetween(tF1, Date.now()), gap: gapStats(tF1, Date.now()),
  };
  note('read-back restart result', results.readbackRestart);

  // C2: a second freeze inside 30 min: two bad reads, an alert, no restart.
  const tF2 = await freezeProxy(2);
  const two = await readbackAfter(tF2, 2, minutes(12));
  await sleep(20_000);
  results.secondFreeze = {
    minutesSinceRestart: req.value ? Math.round((Date.now() - req.value.ms) / 600) / 100 : null,
    reads: two.value, restartRequested: eventsSince(tF2, ['encoder-restart-requested']).length,
    alerts: alertsBetween(tF2, Date.now()),
    timeline: (await api('GET', '/events')).json?.filter((e) => Date.parse(e.timestamp) >= tF2).map((e) => e.message),
  };
  note('second freeze result', results.secondFreeze);
  unfreezeProxy('end of run');
  clearInterval(readbackTimer);
  clearInterval(keepWarm);
  results.encoderStarts = eventsSince(0, ['encoder-start']).map((e) => ({ ts: e.ts, generation: e.generation }));
  await stopAll();
}

/** freeze-video on a long video, then the read-back's own clock-region measure on the output:
 *  does the overlay clock keep moving over the frozen picture? */
async function runFreeze() {
  await startProxy();
  await startControl('freeze');
  results.wizard = await wizard();
  runReader();
  await control('jump', 3); // Sprite Fright, 630 s
  await sleep(30_000);
  results.beforeDiff = { clock: await grabDiff(), picture: await grabDiff('crop=400:200:760:300') };
  const f = await fault('freeze-video');
  await sleep(30_000); // the read-ahead holds about 20 s
  results.frozenDiffs = [];
  // the clock region, and a 400x200 patch of the picture well away from the overlay
  for (let i = 0; i < 3; i += 1) results.frozenDiffs.push({ clock: await grabDiff(), picture: await grabDiff('crop=400:200:760:300') });
  results.freezeEvents = eventsSince(f.ms, ['feeder-start', 'watchdog-freeze']).map((e) => ({ ts: e.ts, kind: e.kind, fault: e.fault, pos: e.pos, outcome: e.outcome }));
  note('freeze-video clock diffs', { before: results.beforeDiff, during: results.frozenDiffs, events: results.freezeEvents });
  clearInterval(keepWarm);
  await stopAll();
}

async function runTreeKill() {
  await startControl('treekill');
  results.wizard = await wizard();
  runReader();
  await sleep(60_000);
  const all = await ps();
  const sup = all.filter(isCore);
  const controls = all.filter(isControl);
  results.before = {
    supervisors: sup.map((p) => ({ pid: p.ProcessId, parentPid: p.ParentProcessId, parentAlive: all.some((x) => x.ProcessId === p.ParentProcessId), cmd: (p.CommandLine ?? '').slice(0, 200) })),
    controls: controls.map((n) => ({ pid: n.ProcessId, parentPid: n.ParentProcessId })),
    launched: eventsSince(0, ['launched']).map((e) => ({ ts: e.ts, breakaway: e.breakaway, pid: e.pid })),
  };
  note('before the tree kill', results.before);
  const tKill = Date.now();
  const r = await run('taskkill', ['/F', '/T', '/PID', String(controls[0]?.ProcessId)], { encoding: 'utf8' });
  results.taskkill = { pid: controls[0]?.ProcessId ?? null, output: r.stdout.trim().split(/\r?\n/) };
  results.controlGone = await waitFor('the control process gone after the tree kill', async () => !(await ps()).some(isControl), 30_000, 500);
  await sleep(30_000);
  const supAfter = await supStatus();
  results.after = {
    supervisors: await supervisors(), state: supAfter?.state ?? null, encoderAlive: supAfter?.encoder?.alive ?? null,
    survived: sup.length === 1 && (await supervisors()).includes(sup[0].ProcessId),
    gap: gapStats(tKill - 5000, Date.now()),
  };
  note('after the tree kill', results.after);
  // the control process again, so stopAll can stop the stream through it
  await startControl('reattach');
  results.reattach = await waitFor('the launcher to attach', async () => (await controlStatus())?.launcher === 'running', 30_000, 500);
  results.reattach.supervisors = await supervisors();
  note('reattach', results.reattach);
  results.encoderStarts = eventsSince(0, ['encoder-start']).map((e) => ({ ts: e.ts, generation: e.generation }));
  await stopAll();
}

async function runSmoke() {
  await startProxy();
  await startControl('smoke');
  results.wizard = await wizard();
  runReader();
  await sleep(30_000);
  results.control = await control('next');
  await sleep(15_000);
  results.hlsWarm = await waitFor('MediaMTX HLS', async () => httpOk(`http://127.0.0.1:${PROXY_PORT}/index.m3u8`), 60_000, 2000);
  results.liveGrabDiff = await grabDiff();
  await freezeProxy(1);
  results.frozenGrabDiff = await grabDiff();
  unfreezeProxy('smoke');
  results.gap = gapStats(packets[0]?.ms ?? Date.now(), Date.now());
  clearInterval(keepWarm);
  await stopAll();
}

/** The checks behind a run's verdict: the bar the Node backend set in stage 3 (ARCHITECTURE.md). */
function verdictChecks() {
  const checks = [];
  const check = (name, ok, figure) => checks.push({ name, ok: Boolean(ok), figure: figure ?? null });
  // Packets throughout, no reader reconnect and no stall a viewer would see.
  const noDrop = (g) => g != null && g.packets > 0 && g.readerReconnects === 0 && g.maxWallGapMs < 1000;
  const only = (list, patterns) => list.length === patterns.length && patterns.every((re) => list.filter((m) => re.test(m)).length === 1);
  const r = results;
  check('the run finished without an error', !r.error, r.error ?? null);
  if (RUN !== 'smoke' && RUN !== 'freeze') {
    check('the wizard took the stream live', r.wizard?.firstRun === true && r.wizard.saveStatus === 200 && r.wizard.keyStatus === 200 && r.wizard.msToLive !== null, { msToLive: r.wizard?.msToLive ?? null });
  }
  if (RUN === 'A') {
    const shown = (r.controls ?? []).map((c) => (c.reply.status === 200 ? c.statusMatchMs : null));
    check('five dashboard controls each shown', shown.length === 5 && shown.every((ms) => ms !== null), { statusMatchMs: shown });
    const msgs = (kind) => (r.faults?.[kind]?.alerts ?? []).flatMap((a) => a.messages);
    const f = r.faults ?? {};
    check('freeze-video: one frozen-output alert, the watchdog acted, no drop', only(msgs('freeze-video'), [/^Output frozen/]) && (f['freeze-video']?.events ?? []).some((e) => e.kind === 'watchdog-freeze') && noDrop(f['freeze-video']?.gap), { alerts: msgs('freeze-video'), gap: f['freeze-video']?.gap ?? null });
    check('kill-feeder: one playback-error alert, no drop', only(msgs('kill-feeder'), [/^Playback error/]) && noDrop(f['kill-feeder']?.gap), { alerts: msgs('kill-feeder'), gap: f['kill-feeder']?.gap ?? null });
    check('kill-encoder: one drop alert and one restart alert', only(msgs('kill-encoder'), [/Stream stopped unexpectedly/, /Stream restarted successfully/]), { alerts: (f['kill-encoder']?.alerts ?? []), gap: f['kill-encoder']?.gap ?? null });
    const inFaults = ['freeze-video', 'kill-feeder', 'kill-encoder'].reduce((n, k) => n + msgs(k).length, 0);
    // A cold start YouTube is slow to serve can alert before the stream is live; from then on only the faults may.
    const afterLive = (r.alerts?.all ?? []).filter((a) => Date.parse(a.at) >= Date.parse(r.liveAt)).flatMap((a) => a.messages);
    check('once live, no alert outside the three faults; none repeated', afterLive.length === inFaults && r.alerts?.summary.duplicatesWithin2Min.length === 0, { afterLive: afterLive.length, inFaults, summary: r.alerts?.summary ?? null });
    check('controls, freeze and feeder kill: no reader reconnect, no stall', noDrop(r.beforeEncoderKill), r.beforeEncoderKill ?? null);
    check('no video timestamp going backwards over the run', r.wholeRun?.dtsBackwards === 0, r.wholeRun ?? null);
    check('two encoder generations: the first and the one after kill-encoder', r.encoderStarts?.length === 2, { encoderStarts: r.encoderStarts?.length ?? null });
  }
  if (RUN === 'B') {
    for (const k of ['crash', 'treeKill', 'exit75', 'consoleClosed']) {
      const c = r[k];
      check(`${k}: the control process ended, the same supervisor kept streaming with no drop`, c && c.controlGoneMs !== null && c.samePid && c.supervisorStateWhileControlDown === 'playing' && c.encoderAliveWhileControlDown === true && c.supervisorStarts === 0 && noDrop(c.gap), c && { supervisor: c.supervisorsAfter, gap: c.gap });
      check(`${k}: the restarted control process attached in under 1 s`, c && c.controlRestartToAttachMs !== null && c.controlRestartToAttachMs < 1000, { attachMs: c?.controlRestartToAttachMs ?? null });
    }
    check('exit75: launch.bat saw exit code 75', /exited 75\s*$/.test(r.exit75?.killInfo?.exits?.[0] ?? ''), r.exit75?.killInfo ?? null);
    const k = r.supervisorKill;
    check('a killed supervisor was relaunched within 10 s and the stream went live again', k && k.relaunchMs !== null && k.relaunchMs <= 10_000 && k.liveMs !== null && k.newPids?.length === 1, k && { relaunchMs: k.relaunchMs, liveMs: k.liveMs, newPids: k.newPids });
    check('the relaunched supervisor resumed at the saved position', k?.resumeEvent && k.saved && k.resumeEvent.id === k.saved.id && Math.abs(k.resumeEvent.pos - k.saved.position) <= 0.5, k && { saved: k.saved, resumed: k.resumeEvent });
    const s = r.stopStart;
    check('Stop ended the supervisor and nothing relaunched while stopped', s && s.exitMs !== null && s.samplesWhileStopped.length === 9 && s.samplesWhileStopped.every((x) => x.supervisors === 0 && x.stream === 'stopped'), s && { exitMs: s.exitMs, lastSampleS: s.samplesWhileStopped.at(-1)?.s });
    check('Start launched one supervisor and the stream went live', s && s.startToProcessMs !== null && s.startToLiveMs !== null && s.supervisorsAfter.length === 1, s && { startToProcessMs: s.startToProcessMs, startToLiveMs: s.startToLiveMs });
  }
  if (RUN === 'C') {
    check('the supervisor\'s parent (the launch intermediate) has exited', r.supervisorParent?.length === 1 && !r.supervisorParent[0].parentAlive && !r.supervisorParent[0].parentIsControl, r.supervisorParent ?? null);
    check('the first read-back reads a moving picture', r.firstReadback?.[0]?.state === 'ok', r.firstReadback?.[0] && { lastDiff: r.firstReadback[0].lastDiff });
    const fv = r.freezeVideo;
    check('freeze-video: the clock keeps moving, two good reads, no restart', fv?.reads?.length === 2 && fv.reads.every((x) => x.state === 'ok') && fv.restartRequested === 0, fv && { diffs: fv.reads?.map((x) => x.lastDiff) ?? null, restartRequested: fv.restartRequested });
    const rr = r.readbackRestart;
    check('a frozen picture: two bad reads, one restart-encoder, a new encoder, moving again', rr && rr.readsWhileFrozen.filter((x) => x.state === 'frozen').length >= 2 && rr.restartRequested && rr.restartRequests === 1 && rr.newGeneration !== null && rr.newGeneration !== rr.oldGeneration && rr.ownGrabDiffAfter > 1.0, rr && { badReads: rr.readsWhileFrozen.filter((x) => x.state === 'frozen').length, restartRequests: rr.restartRequests, generations: [rr.oldGeneration, rr.newGeneration], ownGrabDiffAfter: rr.ownGrabDiffAfter });
    const sf = r.secondFreeze;
    const critical = (sf?.alerts ?? []).flatMap((a) => a.messages);
    check('a second freeze inside 30 min: two bad reads, one critical alert, no restart', sf?.reads?.length === 2 && sf.reads.every((x) => x.state === 'frozen') && sf.restartRequested === 0 && only(critical, [/CRITICAL:.*already restarted in the last 30 minutes/]), sf && { minutesSinceRestart: sf.minutesSinceRestart, alerts: critical });
    check('no alert repeated within 2 min', r.alerts?.summary.duplicatesWithin2Min.length === 0, r.alerts?.summary ?? null);
  }
  if (RUN === 'treekill') {
    const b = r.before;
    check('one supervisor, its parent exited, launched with breakaway', b?.supervisors.length === 1 && !b.supervisors[0].parentAlive && b.launched.at(-1)?.breakaway === true, b && { supervisors: b.supervisors.map((s) => s.pid), launched: b.launched });
    check('taskkill /T /F ended the control process', r.taskkill?.output.some((l) => l.startsWith('SUCCESS')) && r.controlGone?.ms !== null, r.taskkill ?? null);
    const a = r.after;
    check('the same supervisor kept streaming with no drop', a?.survived && a.state === 'playing' && a.encoderAlive === true && noDrop(a.gap), a && { supervisors: a.supervisors, gap: a.gap });
    check('a new control process attached to it', r.reattach?.value === true && r.reattach.supervisors.length === 1 && r.reattach.supervisors[0] === b?.supervisors[0]?.pid, r.reattach ?? null);
  }
  const k = r.keyGrep;
  check('the key is in no file', k && !k.config && [k.appDir, k.controlLogs, k.supervisorData, k.scriptOutput].every((g) => g.hits.length === 0), k && { appDir: k.appDir.files, controlLogs: k.controlLogs.files, supervisorData: k.supervisorData.files, scriptOutput: k.scriptOutput.files });
  if (RUN !== 'smoke' && RUN !== 'freeze') check('Stop ended the supervisor at the end of the run', r.stop?.supervisorExitMs != null, r.stop ?? null);
  check('nothing left running', r.leftovers?.length === 0, r.leftovers ?? null);
  return checks;
}

try {
  await { A: runA, B: runB, C: runC, freeze: runFreeze, treekill: runTreeKill, smoke: runSmoke }[RUN]();
} catch (err) {
  note('run failed', { error: String(err?.stack ?? err) });
  results.error = String(err?.message ?? err);
  readerStop = true; recStop = true;
  await stopAll().catch(() => null);
}

// after the run: alerts, key grep, leftovers, verdict
results.alerts = { summary: alertSummary(), all: alerts.map((a) => ({ at: a.at, messages: a.messages })) };
readerLog.end();
if (existsSync(join(DATA, 'logs'))) cpSync(join(DATA, 'logs'), join(OUT, 'supervisor-logs'), { recursive: true });
const skipRec = (p) => p.startsWith(join(OUT, 'rec'));
results.keyGrep = {
  config: readFileSync(join(APP, 'config.json')).includes(KEY),
  appDir: grepTree(APP, KEY),
  controlLogs: grepTree(join(APP, 'logs'), KEY),
  supervisorData: grepTree(DATA, KEY),
  scriptOutput: grepTree(OUT, KEY, skipRec),
};
// This run's processes: anything from the root (lib.mjs leaks), its launcher console, and its own
// readers and recorders on the stream.
const left = await ps();
results.leftovers = [...leaks(left, ROOT), ...left.filter((p) => (p.CommandLine ?? '').includes(BAT) || (p.CommandLine ?? '').includes(READ_URL))]
  .filter((p, i, all) => all.findIndex((q) => q.ProcessId === p.ProcessId) === i)
  .map((p) => ({ pid: p.ProcessId, name: p.Name, cmd: (p.CommandLine ?? '').slice(0, 160) }));
results.verdict = verdictChecks();
const passed = results.verdict.every((c) => c.ok);
save('results.json', results);
note('key grep', results.keyGrep);
note('alerts', results.alerts.summary);
note('leftover processes', results.leftovers);
for (const c of results.verdict) note(`check ${c.ok ? 'ok  ' : 'FAIL'} ${c.name}`, c.figure ?? undefined);
note(`verdict ${RUN}: ${passed ? 'PASS' : 'FAIL'}`, { checks: results.verdict.length, failed: results.verdict.filter((c) => !c.ok).length });
catcher.close();
proxy?.close();
mediamtx?.kill();
process.exit(passed ? 0 : 1);
