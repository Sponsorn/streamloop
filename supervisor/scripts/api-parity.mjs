// Records the dashboard API's answers from a running server, so the Rust server can be checked
// against the Node backend's. The request script below is data: the Rust run replays it unchanged.
//   node api-parity.mjs record --server node|rust --out FILE
//   node api-parity.mjs compare A B        exit 0 when equal, else prints each differing request, exit 1
// Nothing leaves the machine: a fake supervisor answers on 4417, a local catcher takes the Discord
// webhook, and the config turns off every outbound check. The server runs against a temp root.
// Node runs as an install does: the repo's src\ is copied to ROOT\app\src, so its logs and timeline
// land in ROOT\app\logs, where the Rust control process puts them too.
import * as cp from 'node:child_process';
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, rmdirSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = fileURLToPath(new URL('../..', import.meta.url));
const API_PORT = 7654;
const SUPERVISOR_PORT = 4417;
const CATCHER_PORT = 8791;
const API = `http://127.0.0.1:${API_PORT}/api`;

/** Fixed config text. Playlist ids avoid the first-run placeholder; every outbound check is off. */
const CONFIG = {
  playlists: [{ id: 'PLparity000000001', name: 'First' }, { id: 'PLparity000000002' }],
  port: API_PORT,
  discord: { webhookUrl: `http://127.0.0.1:${CATCHER_PORT}/webhook`, botName: 'Parity', avatarUrl: '', rolePing: '' },
  supervisor: { ingestUrl: 'rtmp://127.0.0.1:1935/live', streamKeyProtected: '', encoder: 'libx264', cacheBudgetMb: 300, readAheadSeconds: 20, playerClient: 'mweb,web_embedded', overlay: true, freezeSeconds: 20, silenceSeconds: 30 },
  autoUpdateCheck: false,
  autoUpdateYtdlp: false,
  autoOpenAdmin: false,
  twitchLivenessEnabled: false,
  ytdlCookiesFromBrowser: '',
};

/** What the fake supervisor answers on GET /status. */
const FAKE_STATUS = {
  state: 'playing',
  reason: null,
  streamKeyProblem: null,
  playing: { index: 2, playlistId: 'PLparity000000002', id: 'abc123XYZ_-', title: 'Parity video', source: 'file', position: 12.5, duration: 300 },
  encoder: { alive: true, speed: 1, cpuPercent: 3, restarts: 0, generation: 1, mediaTime: 42.5 },
  recovery: { step: 'none', retries: 0 },
  watchdog: { last: null },
  hold: null,
  youtube: { breaker: 'closed', retryAt: null, cookieProblem: null },
  playlistCount: 5,
  tokenServer: { state: 'running', restarts: 0 },
};
/** The fake refuses this index, to cover the 502 path of control and switch. */
const REFUSED_INDEX = 777;

// The request script, as data. body: a string is sent as is, an object as JSON, null sends none.
// token: false leaves out x-api-token. contentType defaults to application/json when there is a body.
// pauseMs waits instead of sending (the Discord batch window is 5 s).
const KEY_PLACEHOLDER = 'live_TESTKEY';
export const REQUESTS = [
  { method: 'GET', path: '/api/token' },
  { method: 'GET', path: '/api/status' },
  { method: 'GET', path: '/api/state' },
  { method: 'GET', path: '/api/events' },
  { method: 'GET', path: '/api/config' },
  { method: 'GET', path: '/api/discord/defaults' },
  { method: 'GET', path: '/api/update/status' },
  { method: 'GET', path: '/api/setup-check' },
  { method: 'GET', path: '/api/logs' },
  { method: 'GET', path: '/api/logs/node/old-run.log' },
  { method: 'GET', path: '/api/logs/supervisor/seams-2026-01-01.csv' },
  { method: 'GET', path: '/api/logs/supervisor/supervisor-events-2026-01-01.jsonl' },
  { method: 'GET', path: '/api/logs/node/missing.log' },
  { method: 'GET', path: '/api/logs/node/notes.txt' },
  { method: 'GET', path: '/api/logs/node/..%5Cconfig.json' },
  { method: 'GET', path: '/api/logs/other/old-run.log' },
  // Discord: one test through the catcher, then the batch window passes
  { method: 'POST', path: '/api/discord/test', body: null },
  { pauseMs: 6000 },
  // config
  { method: 'POST', path: '/api/config', body: { supervisor: { cacheBudgetMb: 400, freezeSeconds: 25 }, discord: { botName: 'Parity two' }, twitchClientSecret: '********' } },
  { method: 'POST', path: '/api/config', body: { supervisor: { encoder: 'h264_amf' } } },
  { method: 'POST', path: '/api/config', body: { supervisor: { cacheBudgetMb: 10 } } },
  { method: 'POST', path: '/api/config', body: { ytdlCookiesFromBrowser: 'x'.repeat(201) } },
  { method: 'POST', path: '/api/config', body: { ytdlCookiesFromBrowser: 'firefox\u0007' } },
  { method: 'POST', path: '/api/config', body: { ytdlCookiesFromBrowser: 'firefox:default' } },
  { method: 'POST', path: '/api/config', body: '{"supervisor":' },
  { method: 'POST', path: '/api/config', body: { discord: { webhookUrl: '' } } },
  { method: 'GET', path: '/api/config' },
  { method: 'POST', path: '/api/discord/test', body: null },
  // stream key
  { method: 'POST', path: '/api/stream-key', body: '{"key":' },
  { method: 'POST', path: '/api/stream-key', body: { key: '   ' } },
  { method: 'POST', path: '/api/stream-key', body: {} },
  { method: 'POST', path: '/api/stream-key', body: { key: KEY_PLACEHOLDER } },
  // player
  { method: 'POST', path: '/api/player/next', body: null },
  { method: 'POST', path: '/api/player/prev', body: null },
  { method: 'POST', path: '/api/player/restart', body: null },
  { method: 'POST', path: '/api/player/jump', body: { index: 3 } },
  { method: 'POST', path: '/api/player/jump', body: { index: 3, id: 'abc123XYZ_-' } },
  { method: 'POST', path: '/api/player/jump', body: { index: REFUSED_INDEX } },
  { method: 'POST', path: '/api/player/jump', body: { index: -1 } },
  { method: 'POST', path: '/api/player/jump', body: { index: 1.5 } },
  { method: 'POST', path: '/api/player/jump', body: { index: 1000000 } },
  { method: 'POST', path: '/api/player/jump', body: { index: '3' } },
  { method: 'POST', path: '/api/player/jump', body: { index: 3, id: 'bad id!' } },
  { method: 'POST', path: '/api/player/jump', body: { index: 3, id: 'x'.repeat(65) } },
  { method: 'POST', path: '/api/player/jump', body: '{"index":3}', contentType: 'text/plain' },
  // playlist
  { method: 'POST', path: '/api/playlist/switch', body: { playlistIndex: 1 } },
  { method: 'POST', path: '/api/playlist/switch', body: { playlistIndex: 0, videoIndex: 4, videoId: 'abc123XYZ_-' } },
  { method: 'POST', path: '/api/playlist/switch', body: { playlistIndex: 0, videoIndex: REFUSED_INDEX } },
  { method: 'POST', path: '/api/playlist/switch', body: { playlistIndex: 2 } },
  { method: 'POST', path: '/api/playlist/switch', body: { playlistIndex: -1 } },
  { method: 'POST', path: '/api/playlist/switch', body: { playlistIndex: '0' } },
  { method: 'POST', path: '/api/playlist/switch', body: { playlistIndex: 0, videoIndex: -1 } },
  { method: 'POST', path: '/api/playlist/switch', body: { playlistIndex: 0, videoIndex: 1000000 } },
  { method: 'POST', path: '/api/playlist/switch', body: { playlistIndex: 0, videoId: 'bad id!' } },
  { method: 'GET', path: '/api/playlist/videos?playlist=5' },
  { method: 'GET', path: '/api/playlist/videos?playlist=-1' },
  { method: 'GET', path: '/api/playlist/videos?playlist=abc' },
  { method: 'GET', path: '/api/playlist/videos?playlist=1.5' },
  // every POST once without the token
  ...[
    '/api/config', '/api/stream-key', '/api/autostart', '/api/discord/test', '/api/update/check', '/api/update/apply',
    '/api/playlist/switch', '/api/player/next', '/api/player/prev', '/api/player/restart', '/api/player/jump',
    '/api/stream/stop', '/api/stream/start', '/api/setup-check/run', '/api/yt-dlp/update', '/api/test/exit-75', '/api/shutdown',
  ].map((path) => ({ method: 'POST', path, body: {}, token: false })),
  // the six routes 2.x had and 3.x does not
  ...['/api/obs-path/detect', '/api/obs-path/validate', '/api/player/seek', '/api/player/pause', '/api/player/resume', '/api/player/restart-mpv']
    .map((path) => ({ method: 'POST', path, body: {} })),
  // stop and start, then the shutdown last
  { method: 'POST', path: '/api/stream/stop', body: null },
  { method: 'GET', path: '/api/status' },
  { method: 'POST', path: '/api/player/next', body: null },
  { method: 'POST', path: '/api/playlist/switch', body: { playlistIndex: 0 } },
  { method: 'POST', path: '/api/stream/start', body: null },
  { method: 'POST', path: '/api/shutdown', body: null },
];


const PLACEHOLDER = { token: '<token>', time: '<time>', ms: '<ms>', memory: '<memory>', version: '<version>', size: '<size>' };
const TIME_KEYS = new Set(['timestamp', 'at', 'mtime', 'lastCheckAt', 'lastRestartAt']);
/** A string at a time key that is null stays null; a number or string becomes a placeholder. */
function normaliseValue(key, value) {
  if (value === null) return null;
  if (key === 'token' && typeof value === 'string') return PLACEHOLDER.token;
  if (TIME_KEYS.has(key)) return PLACEHOLDER.time;
  if (key === 'uptimeMs') return PLACEHOLDER.ms;
  if (key === 'systemMemory') return PLACEHOLDER.memory;
  if (/version/i.test(key) && typeof value === 'string' && /\d+\.\d+/.test(value)) return PLACEHOLDER.version;
  return undefined;
}
export function normaliseJson(node, key = '') {
  const replaced = normaliseValue(key, node);
  if (replaced !== undefined) return replaced;
  if (Array.isArray(node)) return node.map((x) => normaliseJson(x, key));
  if (node && typeof node === 'object') return Object.fromEntries(Object.entries(node).map(([k, v]) => [k, normaliseJson(v, k)]));
  return node;
}

/** The server's own live log files change size by the second and share the newest mtime; their
 *  names carry the date. The seeded files keep their sizes and the mtime order the seeds set. */
const LIVE_LOG = /^(streamloop|events)-\d{4}-\d{2}-\d{2}\.(log|jsonl)$/;
export function normaliseLogList(list) {
  if (!Array.isArray(list)) return list;
  const mapped = list.map((f) => (f && LIVE_LOG.test(f.name)
    ? { ...f, name: f.name.replace(/\d{4}-\d{2}-\d{2}/, '<date>'), size: PLACEHOLDER.size, live: true }
    : f));
  const live = mapped.filter((f) => f.live).sort((a, b) => a.name.localeCompare(b.name));
  const seeded = mapped.filter((f) => !f.live);
  return [...live, ...seeded].map(({ live: _live, ...f }) => normaliseJson(f));
}

const sha256 = (buf) => createHash('sha256').update(buf).digest('hex');

export function normaliseResponse(request, status, contentType, buffer) {
  const type = (contentType ?? '').split(';')[0].trim().toLowerCase();
  if (type === 'application/json') {
    let json;
    try { json = JSON.parse(buffer.toString('utf8')); } catch { return { raw: buffer.toString('utf8') }; }
    if (request.path === '/api/logs') return normaliseLogList(json);
    return normaliseJson(json);
  }
  if (type === 'text/html' || type === 'text/javascript' || type === 'application/javascript') {
    return { sha256: sha256(buffer), length: buffer.length };
  }
  return buffer.toString('utf8');
}


/** Key order does not matter to a client, so objects compare by sorted keys. */
export function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map((k) => [k, canonical(value[k])]));
  return value;
}
const same = (a, b) => JSON.stringify(canonical(a)) === JSON.stringify(canonical(b));
const label = (r) => `${r.method} ${r.path}${r.token === false ? ' (no token)' : ''}${r.body === undefined || r.body === null ? '' : ` ${typeof r.body === 'string' ? r.body : JSON.stringify(r.body)}`}`;

/** The differing requests as text lines; empty when the recordings agree. */
export function diffRecordings(a, b) {
  const lines = [];
  const n = Math.max(a.requests.length, b.requests.length);
  for (let i = 0; i < n; i += 1) {
    const x = a.requests[i];
    const y = b.requests[i];
    if (!x || !y) { lines.push(`#${i}: only in ${x ? 'A' : 'B'}: ${label(x ?? y)}`); continue; }
    const fields = ['method', 'path', 'request', 'status', 'contentType', 'response'].filter((f) => !same(x[f], y[f]));
    if (fields.length) lines.push(`#${i} ${label(x)}: differs in ${fields.join(', ')}`);
  }
  const m = Math.max(a.supervisorRequests.length, b.supervisorRequests.length);
  for (let i = 0; i < m; i += 1) {
    const x = a.supervisorRequests[i];
    const y = b.supervisorRequests[i];
    if (!x || !y) lines.push(`supervisor #${i}: only in ${x ? 'A' : 'B'}: ${JSON.stringify(x ?? y)}`);
    else if (!same(x, y)) lines.push(`supervisor #${i} ${x.method} ${x.path}: differs in ${['method', 'path', 'body'].filter((f) => !same(x[f], y[f])).join(', ')}`);
  }
  if (a.catcherRequests !== b.catcherRequests) lines.push(`Discord catcher: ${a.catcherRequests} requests in A, ${b.catcherRequests} in B`);
  return lines;
}


const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const portFree = (port) => new Promise((resolve) => {
  const s = net.connect(port, '127.0.0.1');
  s.on('connect', () => { s.destroy(); resolve(false); });
  s.on('error', () => resolve(true));
});
const readBody = (req) => new Promise((resolve) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
});
const listen = (server, port) => new Promise((resolve, reject) => { server.once('error', reject); server.listen(port, '127.0.0.1', resolve); });

function startFakeSupervisor(received) {
  const server = http.createServer(async (req, res) => {
    const send = (status, body) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(body)); };
    if (req.method === 'GET' && req.url === '/status') return send(200, FAKE_STATUS);
    const text = await readBody(req);
    let body = null;
    try { body = JSON.parse(text); } catch { body = text; }
    received.push({ method: req.method, path: req.url, body });
    if (req.url === '/control') return body?.index === REFUSED_INDEX ? send(400, { error: 'index out of range' }) : send(200, { ok: true });
    if (req.url === '/reload') return body?.index === REFUSED_INDEX ? send(400, { error: 'index out of range' }) : send(200, { ok: true, restartRequired: false });
    if (req.url === '/shutdown') return send(200, { ok: true });
    return send(404, { error: 'not found' });
  });
  return server;
}

function startCatcher(counter) {
  return http.createServer(async (req, res) => {
    await readBody(req);
    counter.n += 1;
    res.writeHead(204);
    res.end();
  });
}

/** Unlinks the node_modules junction first, so removing the root never walks into the repo. */
function removeRoot(root) {
  try { rmdirSync(join(root, 'app', 'node_modules')); } catch { /* absent or not a junction */ }
  rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}

function seedRoot(root) {
  removeRoot(root);
  const app = join(root, 'app');
  const logs = join(root, 'data', 'logs');
  mkdirSync(join(app, 'logs'), { recursive: true });
  mkdirSync(logs, { recursive: true });
  writeFileSync(join(app, 'config.json'), JSON.stringify(CONFIG, null, 2));
  const old = new Date('2026-01-01T00:00:00Z');
  const seed = (file, text, atMs) => { writeFileSync(file, text); utimesSync(file, new Date(atMs), new Date(atMs)); };
  const base = old.getTime();
  seed(join(logs, 'supervisor-events-2026-01-01.jsonl'), '{"t":"2026-01-01T00:00:00Z","kind":"started"}\n{"t":"2026-01-01T00:00:05Z","kind":"seam"}\n', base + 3000);
  seed(join(logs, 'seams-2026-01-01.csv'), 'wallMs,videoNumber,id\n1000,1,abc123XYZ_-\n2000,2,def456UVW_-\n', base + 2000);
  seed(join(app, 'logs', 'old-run.log'), '2026-01-01 00:00:00.000 INFO  Starting StreamLoop\n2026-01-01 00:00:01.000 INFO  Server listening\n', base + 1000);
  // The timeline of today, which Node reads at start.
  const d = new Date();
  const today = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  writeFileSync(join(app, 'logs', `events-${today}.jsonl`),
    '{"timestamp":"2026-01-01T00:00:00.000Z","message":"Seeded timeline entry one"}\n{"timestamp":"2026-01-01T00:00:01.000Z","message":"Seeded timeline entry two"}\n');
  writeFileSync(join(root, 'data', 'setup-check.json'), JSON.stringify({
    at: '2026-01-01T00:00:00.000Z',
    encoder: { chosen: 'libx264', tried: ['libx264'], software: true, works: true },
    cache: { freeMb: 100000, recommendedMb: 4096, warning: null },
    dependencies: [{ name: 'ffmpeg', ok: true, path: 'ffmpeg\\ffmpeg.exe' }],
  }, null, 2));
}

function launchServer(kind, root) {
  const env = { ...process.env, STREAMLOOP_ROOT: root, STREAMLOOP_TEST_HOOKS: '1', LOG_LEVEL: 'info' };
  for (const k of ['STREAMLOOP_DEBUG_FAULTS', 'STREAMLOOP_TEST_HLS_URL', 'STREAMLOOP_TEST_RELEASES']) delete env[k];
  if (kind === 'node') {
    const app = join(root, 'app');
    cpSync(join(REPO, 'src'), join(app, 'src'), { recursive: true, filter: (p) => !p.includes('__tests__') });
    cpSync(join(REPO, 'package.json'), join(app, 'package.json'));
    if (existsSync(join(REPO, 'tsconfig.json'))) cpSync(join(REPO, 'tsconfig.json'), join(app, 'tsconfig.json'));
    symlinkSync(join(REPO, 'node_modules'), join(app, 'node_modules'), 'junction');
    const tsx = join(REPO, 'node_modules', 'tsx', 'dist', 'cli.mjs');
    return cp.spawn(process.execPath, [tsx, join(app, 'src', 'server', 'index.ts')], { cwd: app, env, windowsHide: true, stdio: 'ignore' });
  }
  const exe = join(REPO, 'target', 'release', 'streamloop.exe');
  if (!existsSync(exe)) throw new Error(`${exe} is missing: build it with cargo build --release`);
  return cp.spawn(exe, ['--root', root], { cwd: join(root, 'app'), env, windowsHide: true, stdio: 'ignore' });
}

/** What the run has started; the exit hooks end all of it. */
const live = { child: null, servers: [], root: null };

/** Synchronous, so it also runs from an exit hook. Never throws: cleanup must not hide the real error. */
function cleanupSync() {
  const { child, servers, root } = live;
  live.child = null;
  live.servers = [];
  live.root = null;
  if (child?.pid) {
    try { cp.execFileSync('taskkill', ['/T', '/F', '/PID', String(child.pid)], { windowsHide: true, stdio: 'ignore' }); } catch { /* the pid is gone */ }
  }
  for (const s of servers) {
    try { s.closeAllConnections?.(); s.close(); } catch { /* already closed */ }
  }
  if (root) {
    try { removeRoot(root); } catch (e) { console.error(`could not remove ${root}: ${e.message}`); }
  }
}

process.on('exit', cleanupSync);
for (const sig of ['SIGINT', 'SIGTERM', 'SIGBREAK']) process.on(sig, () => { cleanupSync(); process.exit(130); });

async function send(request) {
  const body = request.body === undefined || request.body === null ? undefined : typeof request.body === 'string' ? request.body : JSON.stringify(request.body);
  const headers = {};
  if (body !== undefined) headers['content-type'] = request.contentType ?? 'application/json';
  if (request.token !== false && request.method === 'POST') headers['x-api-token'] = send.token;
  const res = await fetch(`http://127.0.0.1:${API_PORT}${request.path}`, { method: request.method, headers, body, redirect: 'manual', signal: AbortSignal.timeout(30_000) });
  return { status: res.status, contentType: res.headers.get('content-type'), buffer: Buffer.from(await res.arrayBuffer()) };
}

async function record(kind, out) {
  for (const port of [API_PORT, SUPERVISOR_PORT, CATCHER_PORT]) {
    if (!(await portFree(port))) throw new Error(`port ${port} is in use: not ours, so nothing was started`);
  }
  const root = join(tmpdir(), 'streamloop-api-parity');
  const received = [];
  const catcherCount = { n: 0 };
  const fake = startFakeSupervisor(received);
  const catcher = startCatcher(catcherCount);
  let child = null;
  live.servers = [fake, catcher];
  live.root = root;
  try {
    seedRoot(root);
    await listen(fake, SUPERVISOR_PORT);
    await listen(catcher, CATCHER_PORT);
    child = launchServer(kind, root);
    live.child = child;
    const exited = new Promise((resolve) => child.once('exit', resolve));
    // up: the token answers; ready: the launcher has seen the fake supervisor
    let up = false;
    for (let i = 0; i < 120 && !up && child.exitCode === null; i += 1) {
      try { send.token = (await (await fetch(`${API}/token`)).json()).token; up = true; } catch { await sleep(500); }
    }
    if (!up) throw new Error(`the ${kind} server did not come up`);
    let ready = false;
    for (let i = 0; i < 40 && !ready; i += 1) {
      try { ready = (await (await fetch(`${API}/status`)).json()).launcher === 'running'; } catch { /* retry */ }
      if (!ready) await sleep(500);
    }
    if (!ready) throw new Error('the launcher never saw the fake supervisor');
    // The log file is created by a worker a moment after start; /api/logs must see it every run.
    for (let i = 0; i < 20 && !readdirSync(join(root, 'app', 'logs')).some((f) => /^streamloop-.*\.log$/.test(f)); i += 1) await sleep(250);

    const requests = [];
    for (const step of REQUESTS) {
      if (step.pauseMs) { await sleep(step.pauseMs); continue; }
      const reply = await send(step);
      const entry = { method: step.method, path: step.path, request: step.body ?? null, status: reply.status, contentType: reply.contentType, response: normaliseResponse(step, reply.status, reply.contentType, reply.buffer) };
      if (step.token === false) entry.token = false;
      if (step.contentType) entry.requestContentType = step.contentType;
      requests.push(entry);
    }
    // the shutdown was the last request: the server exits by itself
    await Promise.race([exited, sleep(10_000)]);
    const result = { server: kind, requests, supervisorRequests: received, catcherRequests: catcherCount.n };
    mkdirSync(join(out, '..'), { recursive: true });
    writeFileSync(out, `${JSON.stringify(result, null, 2)}\n`);
    return result;
  } finally {
    cleanupSync();
  }
}

function main() {
  const [cmd, ...args] = process.argv.slice(2);
  const opt = (name) => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; };
  if (cmd === 'record') {
    const kind = opt('--server');
    const out = opt('--out');
    if (!['node', 'rust'].includes(kind) || !out) { console.error('usage: api-parity.mjs record --server node|rust --out FILE'); process.exit(2); }
    record(kind, out).then((r) => console.log(`recorded ${r.requests.length} requests and ${r.supervisorRequests.length} supervisor requests to ${out}`))
      .catch((e) => { console.error(e.message); process.exit(1); });
  } else if (cmd === 'compare' && args.length === 2) {
    const [a, b] = args.map((f) => JSON.parse(readFileSync(f, 'utf8')));
    const lines = diffRecordings(a, b);
    if (lines.length === 0) { console.log(`equal: ${a.requests.length} requests`); process.exit(0); }
    console.log(lines.join('\n'));
    process.exit(1);
  } else {
    console.error('usage: api-parity.mjs record --server node|rust --out FILE | compare A B');
    process.exit(2);
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
