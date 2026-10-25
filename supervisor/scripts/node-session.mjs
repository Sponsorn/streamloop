// Stage 3's end-to-end proof: the real Node server (tsx) drives the supervisor against local
// MediaMTX. Node runs from a launcher console (a hidden cmd running launch.bat, like START.bat)
// with cwd ROOT\app and STREAMLOOP_ROOT=ROOT, so Node and the supervisor read the same config.json.
// Nothing opens a window: every process is spawned hidden, never through `start`.
//   node node-session.mjs A|B|C|smoke
// A: 30 min through Node: the wizard's API calls, dashboard controls, freeze / feeder / encoder faults,
//    Discord alerts caught by a local webhook.
// B: Node lifecycle: crash, tree kill, exit 75, console closed, supervisor killed, Stop / Start.
// C: the Twitch read-back against local HLS (STREAMLOOP_TEST_HLS_URL); nothing reaches Twitch.
// Faults go to the supervisor's POST /debug/fault (Node starts it with STREAMLOOP_DEBUG_FAULTS=1).
// The dummy key is never printed: the script's own notes go through redact(); what Node sends
// (replies, alerts, read-back status, its console) is stored as received, so the key grep can catch a leak.
import * as cp from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync, readFileSync, existsSync, appendFileSync, readdirSync, createWriteStream, linkSync, copyFileSync, cpSync } from 'node:fs';
import { join, relative, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import http from 'node:http';
import net from 'node:net';
import { TOKEN_SERVER_PING, FFMPEG, FFPROBE, PLAYLIST, redact } from './lib.mjs';
import { makeDevRoot } from './dev-root.mjs';

// Every child hidden: nothing this script starts may show a window.
const spawn = (cmd, args, opts = {}) => cp.spawn(cmd, args, { windowsHide: true, ...opts });
// Never spawnSync: a blocked event loop stalls the reader (false gaps) and the HLS proxy.
const run = (cmd, args, opts = {}) => new Promise((resolve) => {
  cp.execFile(cmd, args, { windowsHide: true, maxBuffer: 64 * 1024 * 1024, ...opts }, (err, stdout) => resolve({ status: err ? (err.code ?? 1) : 0, stdout }));
});
const RUN = process.argv[2];
if (!['A', 'B', 'C', 'freeze', 'smoke'].includes(RUN)) { console.error('usage: node node-session.mjs A|B|C|freeze|smoke'); process.exit(2); }
const REPO = fileURLToPath(new URL('../..', import.meta.url));
const BIN = join(REPO, 'supervisor', 'target', 'release', 'streamloop-supervisor.exe');
const TOOLS = join(REPO, 'spikes', 'tools');
const KEY = 'live_DUMMYKEY_stage3';
const NODE_API = 'http://127.0.0.1:7654/api';
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
const rtmpUp = () => new Promise((resolve) => {
  const s = net.connect(1935, '127.0.0.1');
  s.on('connect', () => { s.destroy(); resolve(true); });
  s.on('error', () => resolve(false));
});
if (!(await httpOk(TOKEN_SERVER_PING))) { note('PO-token server not answering: stop'); process.exit(2); }
note('token server answers /ping');
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
const supExe = join(ROOT, 'supervisor', 'streamloop-supervisor.exe');
rmSync(supExe, { force: true });
try { linkSync(BIN, supExe); } catch { copyFileSync(BIN, supExe); }
rmSync(DATA, { recursive: true, force: true });
rmSync(join(APP, 'logs'), { recursive: true, force: true });

// A first-run config (the example's placeholder playlist). Updates and the browser are off so a
// run never touches GitHub, swaps the repo's yt-dlp or opens a window.
const fresh = JSON.parse(readFileSync(join(REPO, 'config.example.json'), 'utf8'));
Object.assign(fresh, { autoUpdateCheck: false, autoUpdateYtdlp: false, autoOpenAdmin: false });
if (RUN === 'C' || RUN === 'freeze') {
  // Helix credentials stay empty: STREAMLOOP_TEST_HLS_URL stubs "channel is live".
  Object.assign(fresh, { twitchLivenessEnabled: true, twitchChannel: 'stage3test' });
  // The supervisor's own watchdog must not see the freeze, so only the read-back can.
  Object.assign(fresh.supervisor, { freezeSeconds: 3600, silenceSeconds: 3600 });
}
writeFileSync(join(APP, 'config.json'), JSON.stringify(fresh, null, 2));
note('fresh first-run config', { root: ROOT, run: RUN });

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
    "Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('node.exe','cmd.exe','streamloop-supervisor.exe','ffmpeg.exe','ffprobe.exe','yt-dlp.exe') } | Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json"],
    { encoding: 'utf8', windowsHide: true });
  const v = JSON.parse(r.stdout || '[]');
  return Array.isArray(v) ? v : [v];
}
const isOurNode = (p) => p.Name === 'node.exe' && /src[\\/]server[\\/]index\.ts/.test(p.CommandLine ?? '');
const launcherCmd = async () => (await ps()).find((p) => p.Name === 'cmd.exe' && (p.CommandLine ?? '').includes('launch.bat'));
const tasklistSupervisors = async () => (await run('tasklist', ['/FI', 'IMAGENAME eq streamloop-supervisor.exe', '/FO', 'CSV', '/NH'], { encoding: 'utf8' })).stdout
  .split(/\r?\n/).filter((l) => l.includes('streamloop-supervisor.exe')).map((l) => Number(l.split('","')[1]));

// The launcher console: a hidden cmd (it gets its own console host) running launch.bat, which
// runs Node and records its exit code.
const PATH_NO_FFMPEG = (process.env.PATH ?? '').split(';').filter((d) => !/chocolatey\\bin|ffmpeg/i.test(d)).join(';');
const nodeEnv = [
  `set "STREAMLOOP_ROOT=${ROOT}"`,
  'set "STREAMLOOP_DEBUG_FAULTS=1"',
  'set "STREAMLOOP_TEST_HOOKS=1"',
  RUN === 'C' ? `set "STREAMLOOP_TEST_HLS_URL=http://127.0.0.1:${PROXY_PORT}/index.m3u8"` : 'set "STREAMLOOP_TEST_HLS_URL="',
  `set "PATH=${PATH_NO_FFMPEG}"`,
];
const BAT = join(ROOT, 'launch.bat');
writeFileSync(BAT, ['@echo off', 'title StreamLoop stage 3 test', ...nodeEnv, `cd /d "${APP}"`,
  `"${process.execPath}" "${join(REPO, 'node_modules', 'tsx', 'dist', 'cli.mjs')}" "${join(REPO, 'src', 'server', 'index.ts')}" >> "${join(OUT, 'node-console.log')}" 2>&1`,
  `echo %DATE% %TIME% node exited %ERRORLEVEL% >> "${join(OUT, 'node-exits.log')}"`, ''].join('\r\n'));

let token = '';
async function startNode(label) {
  const t0 = Date.now();
  spawn('cmd.exe', ['/d', '/c', BAT], { windowsHide: true, stdio: 'ignore' }).unref();
  for (;;) {
    try {
      const r = await fetch(`${NODE_API}/token`, { signal: AbortSignal.timeout(2000) });
      if (r.ok) { token = (await r.json()).token; break; }
    } catch { /* not up yet */ }
    if (Date.now() - t0 > 90_000) throw new Error('Node did not answer within 90 s');
    await sleep(500);
  }
  note(`Node up (${label})`, { ms: Date.now() - t0 });
  return t0;
}

async function api(method, path, body) {
  const headers = method === 'POST' ? { 'x-api-token': token, 'content-type': 'application/json' } : {};
  let status = 0;
  let text = '';
  try {
    const res = await fetch(NODE_API + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(30_000) });
    status = res.status;
    text = await res.text();
  } catch (err) { text = String(err); }
  // The stream key is the one request body not kept: the log records only that it was sent.
  const recorded = path === '/stream-key' ? '[stream key, not recorded]' : body;
  // Not redacted, so the key grep catches Node echoing the key in a reply.
  appendFileSync(join(OUT, 'requests.jsonl'), `${JSON.stringify({ at: iso(), method, path, request: recorded ?? null, status, response: text.slice(0, 4000) })}\n`);
  let json = null;
  try { json = JSON.parse(text); } catch { /* not json */ }
  return { status, json };
}
const nodeStatus = async () => (await api('GET', '/status')).json;
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
  const first = await nodeStatus();
  await api('GET', '/config');
  const saved = await api('POST', '/config', { playlists: [{ id: PLAYLIST }], discord: { webhookUrl: WEBHOOK }, supervisor: { ingestUrl: 'rtmp://127.0.0.1:1935/live' } });
  const key = await api('POST', '/stream-key', { key: KEY });
  const live = await waitFor('live after the wizard', async () => (await nodeStatus())?.stream?.state === 'live', 180_000, 2000);
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

/** What a viewer got in [fromMs, toMs]: reader reconnects, the longest wall-clock stretch with no
 *  packet, and the largest video dts step (a cut or a restart shows as a jump). */
function gapStats(fromMs, toMs) {
  const win = packets.filter((p) => p.ms >= fromMs && p.ms <= toMs);
  let maxWallGapMs = 0;
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
  return readdirSync(dir).filter((n) => /^events-\d{4}-\d{2}-\d{2}\.jsonl$/.test(n)).sort()
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

/** A dashboard control (Node's /api/player/*); waits until /api/state shows the expected video. */
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
  note('stopping the stream and Node');
  const t0 = Date.now();
  await api('POST', '/stream/stop');
  const gone = await waitFor('the supervisor to exit after Stop', async () => (await tasklistSupervisors()).length === 0, 30_000);
  note('supervisor exited after Stop stream', { ms: gone.ms, sinceStopMs: Date.now() - t0 });
  await api('POST', '/shutdown');
  await waitFor('Node to exit', async () => !(await ps()).some(isOurNode), 20_000);
  for (const p of (await ps()).filter((x) => isOurNode(x) || x.Name === 'streamloop-supervisor.exe')) await run('taskkill', ['/F', '/PID', String(p.ProcessId)]);
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

// C: read-back test HLS behind a proxy, so Node never sees the key
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

/** Two clock-region grabs 5 s apart through the proxy, as the read-back does it. */
async function grabDiff(crop = 'crop=104:32:16:1019') {
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
    const s = await nodeStatus();
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
  const nodeStart = await startNode('A');
  results.wizard = await wizard();
  results.wizard.nodeStartToLiveMs = Date.now() - nodeStart;
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
      at: iso(f.ms), reply: f.reply, before: f.before,
      events: eventsSince(f.ms).filter((e) => !['feeder-start', 'handover', 'spare-resolve', 'spare-used', 'youtube-probe', 'download-cancel'].includes(e.kind)).map((e) => ({ ts: e.ts, kind: e.kind, level: e.level, outcome: e.outcome, why: e.why, failures: e.failures, restarts: e.restarts })),
      alerts: alertsBetween(f.ms, Date.now()),
      gap: gapStats(f.ms - 5000, Date.now()),
    };
    note(`${kind} alerts`, results.faults[kind].alerts);
  }
  await until(t0, 25); results.controls.push(await control('next'));
  await until(t0, 30);
  results.wholeRun = gapStats(t0 + 10_000, Date.now());
  results.endStatus = await nodeStatus();
  await stopAll();
}

async function nodeCycle(label, kill) {
  const supBefore = (await tasklistSupervisors());
  const tKill = Date.now();
  const killInfo = await kill();
  const nodeGone = await waitFor(`Node gone after ${label}`, async () => !(await ps()).some(isOurNode), 30_000, 500);
  await sleep(10_000);
  const supWhileNodeDown = (await tasklistSupervisors());
  const supAliveStatus = await supStatus();
  const tStart = await startNode(label);
  const attached = await waitFor('the launcher to attach', async () => (await nodeStatus())?.launcher === 'running', 30_000, 500);
  await sleep(15_000);
  const supAfter = (await tasklistSupervisors());
  const status = await nodeStatus();
  const r = {
    killInfo, nodeGoneMs: nodeGone.ms, supervisorsBefore: supBefore, supervisorsWhileNodeDown: supWhileNodeDown,
    supervisorStateWhileNodeDown: supAliveStatus?.state ?? null, encoderAliveWhileNodeDown: supAliveStatus?.encoder?.alive ?? null,
    nodeRestartToAttachMs: attached.ms === null ? null : Date.now() - tStart - 15_000, supervisorsAfter: supAfter,
    samePid: supBefore.length === 1 && supAfter.length === 1 && supBefore[0] === supAfter[0],
    streamAfter: status?.stream, launcherLaunched: eventsSince(tKill, ['start']).length,
    gap: gapStats(tKill - 5000, Date.now()),
  };
  note(`${label} result`, r);
  return r;
}

async function runB() {
  await startNode('B');
  results.wizard = await wizard();
  runReader();
  await sleep(minutes(2));
  const nodePids = async () => (await ps()).filter(isOurNode).map((p) => p.ProcessId);
  results.crash = await nodeCycle('taskkill /F on the Node processes', async () => {
    const pids = await nodePids();
    for (const pid of pids) await run('taskkill', ['/F', '/PID', String(pid)]);
    return { pids };
  });
  await sleep(60_000);
  results.treeKill = await nodeCycle('taskkill /F /T on the launcher console tree', async () => {
    const cmd = (await launcherCmd());
    const r = await run('taskkill', ['/F', '/T', '/PID', String(cmd?.ProcessId)], { encoding: 'utf8' });
    return { cmdPid: cmd?.ProcessId ?? null, output: r.stdout.trim().split(/\r?\n/) };
  });
  await sleep(60_000);
  results.exit75 = await nodeCycle('exit 75 (update restart hook)', async () => {
    const r = await api('POST', '/test/exit-75');
    await sleep(5000);
    return { status: r.status, exits: existsSync(join(OUT, 'node-exits.log')) ? readFileSync(join(OUT, 'node-exits.log'), 'utf8').trim().split(/\r?\n/).slice(-1) : null };
  });
  await sleep(60_000);
  // A hidden console has no window to close, so the console gets Ctrl+Break instead: like the close
  // event, it reaches every process attached to that console, and the supervisor has none.
  results.consoleClosed = await nodeCycle('launcher console control event (Ctrl+Break to every process on it)', async () => {
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

  // The supervisor killed under a running Node: relaunch within 10 s, resume at the saved point.
  const [supPid] = (await tasklistSupervisors());
  const tKill = Date.now();
  await run('taskkill', ['/F', '/PID', String(supPid)]);
  const saved = existsSync(join(DATA, 'state.json')) ? JSON.parse(readFileSync(join(DATA, 'state.json'), 'utf8')) : null;
  const relaunched = await waitFor('a new supervisor process', async () => (await tasklistSupervisors()).some((p) => p !== supPid), 60_000, 250);
  const answering = await waitFor('the new supervisor to answer', async () => (await supStatus()) !== null, 60_000, 250);
  const live = await waitFor('live after the supervisor kill', async () => (await nodeStatus())?.stream?.state === 'live', 180_000, 1000);
  const resumed = eventsSince(tKill, ['resume'])[0] ?? null;
  const firstPlay = await supStatus();
  await sleep(20_000);
  results.supervisorKill = {
    oldPid: supPid, newPids: (await tasklistSupervisors()), relaunchMs: relaunched.ms, answeringMs: answering.ms, liveMs: live.ms, saved,
    resumeEvent: resumed && { ts: resumed.ts, index: resumed.index, pos: resumed.pos, id: resumed.id }, playingWhenLive: firstPlay?.playing ?? null,
    launcherEvents: (await api('GET', '/events')).json?.filter((e) => Date.parse(e.timestamp) >= tKill).map((e) => e.message) ?? null,
    alerts: alertsBetween(tKill, Date.now()), gap: gapStats(tKill - 5000, Date.now()),
  };
  note('supervisor kill result', results.supervisorKill);
  await sleep(30_000);

  // Stop stream: the supervisor exits and stays down until Start.
  const tStop = Date.now();
  await api('POST', '/stream/stop');
  const exited = await waitFor('the supervisor to exit after Stop', async () => (await tasklistSupervisors()).length === 0, 30_000, 500);
  const samples = [];
  for (let i = 0; i < 9; i += 1) { await sleep(5000); samples.push({ s: Math.round((Date.now() - tStop) / 1000), supervisors: (await tasklistSupervisors()).length, stream: (await nodeStatus())?.stream?.state }); }
  const tStart = Date.now();
  await api('POST', '/stream/start');
  const back = await waitFor('a supervisor after Start', async () => (await tasklistSupervisors()).length === 1, 60_000, 250);
  const liveAgain = await waitFor('live after Start', async () => (await nodeStatus())?.stream?.state === 'live', 180_000, 1000);
  results.stopStart = { exitMs: exited.ms, samplesWhileStopped: samples, startToProcessMs: back.ms, startToLiveMs: liveAgain.ms, supervisorsAfter: (await tasklistSupervisors()), alerts: alertsBetween(tStop, Date.now()) };
  note('stop/start result', results.stopStart);
  await sleep(30_000);
  await stopAll();
}

async function runC() {
  await startProxy();
  await startNode('C');
  results.wizard = await wizard();
  // taskkill /T walks ParentProcessId, so this says whether killing Node's tree reaches the supervisor.
  const all = await ps();
  results.supervisorParent = all.filter((p) => p.Name === 'streamloop-supervisor.exe')
    .map((p) => ({ pid: p.ProcessId, parentPid: p.ParentProcessId, parentIsOurNode: all.some((n) => n.ProcessId === p.ParentProcessId && isOurNode(n)) }));
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
    newGeneration: started.value, ownGrabDiffAfter: ownDiff, nextRead: healed.value,
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
  await stopAll();
}

/** freeze-video on a long video, then the read-back's own clock-region measure on the output:
 *  does the overlay clock keep moving over the frozen picture? */
async function runFreeze() {
  await startProxy();
  await startNode('freeze');
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

async function runSmoke() {
  await startProxy();
  await startNode('smoke');
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
  results.gap = gapStats(0, Date.now());
  clearInterval(keepWarm);
  await stopAll();
}

try {
  await { A: runA, B: runB, C: runC, freeze: runFreeze, smoke: runSmoke }[RUN]();
} catch (err) {
  note('run failed', { error: String(err?.stack ?? err) });
  readerStop = true; recStop = true;
  await stopAll().catch(() => null);
}

// after the run: alerts, key grep, leftovers
results.alerts = { summary: alertSummary(), all: alerts.map((a) => ({ at: a.at, messages: a.messages })) };
readerLog.end();
if (existsSync(join(DATA, 'logs'))) cpSync(join(DATA, 'logs'), join(OUT, 'supervisor-logs'), { recursive: true });
const nodeLogs = join(REPO, 'logs');
const skipRec = (p) => p.startsWith(join(OUT, 'rec'));
results.keyGrep = {
  config: readFileSync(join(APP, 'config.json')).includes(KEY),
  appDir: grepTree(APP, KEY),
  nodeLogs: grepTree(nodeLogs, KEY),
  supervisorData: grepTree(DATA, KEY),
  scriptOutput: grepTree(OUT, KEY, skipRec),
};
results.leftovers = (await ps()).filter((p) => isOurNode(p) || ['streamloop-supervisor.exe', 'ffmpeg.exe', 'ffprobe.exe', 'yt-dlp.exe'].includes(p.Name))
  .map((p) => ({ pid: p.ProcessId, name: p.Name, cmd: (p.CommandLine ?? '').slice(0, 160) }));
save('results.json', results);
note('key grep', results.keyGrep);
note('alerts', results.alerts.summary);
note('leftover processes', results.leftovers);
catcher.close();
proxy?.close();
mediamtx?.kill();
process.exit(0);
