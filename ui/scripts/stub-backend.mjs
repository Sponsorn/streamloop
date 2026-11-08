// Stand-in for the Node backend on 127.0.0.1:7654 for the GPUI memory spike. The Tauri shell
// (the real /admin from src/admin) and the GPUI window poll it, so both draw the same data.
// Run: node ui/scripts/stub-backend.mjs
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { dirname, extname, join, normalize, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { freemem, totalmem } from 'node:os';

const PORT = Number(process.env.STUB_PORT) || 7654;
const TOKEN = 'stub-token';
const SEAM_EVERY_S = 120;
const EXTRA_EVENT_EVERY_S = 15;
const MAX_EVENTS = 100;

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..', '..');
const adminDir = join(repo, 'src', 'admin');
const example = JSON.parse(await readFile(join(repo, 'config.example.json'), 'utf8'));

// Seeded, so every run serves the same titles and durations.
let seed = 42;
const rand = () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648);
const pick = (xs) => xs[Math.floor(rand() * xs.length)];
const ID_CHARS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-';
const videoId = () => Array.from({ length: 11 }, () => pick(ID_CHARS)).join('');

const TOPICS = ['Cozy farming sim', 'Late-night chat and chill', 'Speedrun practice', 'Community game night',
  'Horror marathon with chat', 'Art stream: commissions', 'Retro RPG blind playthrough', 'Subathon day',
  'Puzzle games and coffee', 'Charity stream', 'Q&A and channel news', 'Building a new base'];

function makeList(id, name, count) {
  const videos = Array.from({ length: count }, (_, index) => {
    const day = 1 + Math.floor(rand() * 28);
    const month = 1 + Math.floor(rand() * 10);
    return {
      index,
      id: videoId(),
      title: `${pick(TOPICS)} (VOD 2026-${String(month).padStart(2, '0')}-${String(day).padStart(2, '0')}, part ${1 + Math.floor(rand() * 4)})`,
      duration: Math.round((5 + rand() * 3) * 3600),
    };
  });
  return { id, name, videos };
}

const lists = [makeList('PLstubMainVods0001', 'Main VODs', 300), makeList('PLstubHighlights02', 'Highlights', 120)];
const started = Date.now();
const state = { list: 0, index: 0, position: 0, stopped: false, encoderRestarts: 0 };
let events = [];

function addEvent(message, at = Date.now()) {
  events.push({ timestamp: new Date(at).toISOString(), message });
  if (events.length > MAX_EVENTS) events = events.slice(-MAX_EVENTS);
}

function extraEvent() {
  const n = 1 + Math.floor(rand() * lists[state.list].videos.length);
  return pick([
    `Downloaded video ${n} (${200 + Math.floor(rand() * 1800)} MB) in ${20 + Math.floor(rand() * 300)} s`,
    `Silence for 30 s on video ${state.index + 1}, reported only`,
    `Video ${n} is in the cache`,
    'Twitch read-back: the picture is moving',
    `Cache: removed ${1 + Math.floor(rand() * 4)} old files to stay under the budget`,
    'Token server answered in 0.4 s',
  ]);
}

const current = () => lists[state.list].videos[state.index];

// Two log files: a small Node log, and a yt-dlp log over 1 MB with å ä ö, emoji and CJK in it.
const LOG_TAIL_BYTES = 1024 * 1024;
const nodeLog = Array.from({ length: 20 }, (_, i) =>
  `{"level":30,"time":"2026-11-08T13:${String(10 + i).padStart(2, '0')}:00.000Z","msg":"Poll answered in ${40 + i} ms"}\n`).join('');
let ytdlpLog = '';
for (let i = 1; Buffer.byteLength(ytdlpLog) < 1.3 * LOG_TAIL_BYTES; i++) {
  ytdlpLog += `[2026-11-07 ${String(Math.floor(i / 3600) % 24).padStart(2, '0')}:${String(Math.floor(i / 60) % 60).padStart(2, '0')}:${String(i % 60).padStart(2, '0')}] `
    + `[download] Kaffe på Söder: å ä ö 🎥 日本の配信 part ${i} - ${(i * 7) % 100}.${i % 10}% of 1.2 GiB\n`;
}
const logFiles = [
  { source: 'node', name: 'streamloop-2026-11-08.log', text: nodeLog, mtime: new Date(started).toISOString() },
  { source: 'supervisor', name: 'ytdlp-2026-11-07.log', text: ytdlpLog, mtime: new Date(started - 20 * 3_600_000).toISOString() },
];

/** `GET /api/logs/:source/:name` as Node answers it: the file, or its last 1 MB after a header line. */
function logText(source, name) {
  const file = logFiles.find((f) => f.source === source && f.name === name);
  if (!file) return null;
  const bytes = Buffer.from(file.text, 'utf-8');
  if (bytes.length <= LOG_TAIL_BYTES) return file.text;
  return '... (truncated, showing last 1 MB) ...\n' + bytes.subarray(bytes.length - LOG_TAIL_BYTES).toString('utf-8');
}

for (let i = MAX_EVENTS; i > 0; i--) addEvent(extraEvent(), started - i * 60_000);
addEvent('Supervisor started');
addEvent(`Now playing video ${state.index + 1}: ${current().title}`);

setInterval(() => {
  if (state.stopped) return;
  state.position += 1;
  if (state.position >= SEAM_EVERY_S) {
    state.index = (state.index + 1) % lists[state.list].videos.length;
    state.position = 0;
    addEvent(`Now playing video ${state.index + 1}: ${current().title}`);
  }
  if (state.position % EXTRA_EVENT_EVERY_S === 0) addEvent(extraEvent());
}, 1000);

function status() {
  const list = lists[state.list];
  const v = current();
  const gib = 1024 ** 3;
  const total = totalmem();
  const free = freemem();
  return {
    stream: state.stopped
      ? { state: 'stopped', reason: 'The stream was stopped from the dashboard. It stays stopped, even after a restart, until you press Start.' }
      : { state: 'live', reason: null },
    supervisor: state.stopped ? null : {
      state: 'playing',
      reason: null,
      streamKeyProblem: null,
      playing: { index: state.index, playlistId: list.id, id: v.id, title: v.title, source: 'file', position: state.position, duration: v.duration },
      encoder: { alive: true, speed: 0.99 + rand() * 0.02, cpuPercent: 1.6, restarts: state.encoderRestarts, generation: 1 + state.encoderRestarts, mediaTime: (Date.now() - started) / 1000 },
      recovery: { step: 'none', retries: 0 },
      watchdog: { last: null },
      hold: null,
      youtube: { breaker: 'closed', retryAt: null, cookieProblem: null },
      playlistCount: list.videos.length,
      tokenServer: { state: 'running', restarts: 0 },
    },
    launcher: state.stopped ? 'stopped' : 'running',
    twitch: {
      enabled: true, channelLive: !state.stopped, consecutiveMismatches: 0, mismatchThreshold: 3,
      readback: { state: 'moving', lastCheckAt: new Date().toISOString() },
    },
    firstRun: false,
    systemMemory: {
      totalGB: +(total / gib).toFixed(1), freeGB: +(free / gib).toFixed(1),
      usedGB: +((total - free) / gib).toFixed(1), usedPercent: Math.round(((total - free) / total) * 100),
    },
    uptimeMs: Date.now() - started,
  };
}

function maskedConfig() {
  const { streamKeyProtected, ...supervisor } = example.supervisor;
  return {
    ...example,
    playlists: lists.map(({ id, name }) => ({ id, name })),
    twitchClientSecret: '********',
    twitchChannel: 'stubchannel',
    discord: { ...example.discord, webhookUrl: '********' },
    supervisor: { ...supervisor, streamKeySet: true },
  };
}

// The config a save changes, as Node keeps config.json.
const config = maskedConfig();

// The saved webhook URL, kept apart from `config` so every answer masks it as Node does.
let webhookUrl = 'https://discord.com/api/webhooks/0/stub';

/** `GET /api/config`, with the webhook URL masked. */
function servedConfig() {
  return { ...config, discord: { ...config.discord, webhookUrl: webhookUrl ? '********' : '' } };
}

/** `POST /api/config`: merges the patch as Node's saveAndReload (discord's events and templates one
 *  level deeper, `********` keeps the URL). An encoder change answers restartRequired. */
function saveConfig(patch) {
  const before = config.supervisor.encoder;
  if (patch.discord) {
    const { webhookUrl: url, ...discord } = patch.discord;
    if (url !== undefined && url !== '********') webhookUrl = url;
    patch = {
      ...patch,
      discord: {
        ...discord,
        events: { ...config.discord.events, ...discord.events },
        templates: { ...config.discord.templates, ...discord.templates },
      },
    };
  }
  for (const [key, value] of Object.entries(patch)) {
    const nested = value && typeof value === 'object' && !Array.isArray(value) && config[key] && typeof config[key] === 'object';
    config[key] = nested ? { ...config[key], ...value } : value;
  }
  const restartRequired = config.supervisor.encoder !== before;
  return [200, { ok: true, config: servedConfig(), restartRequired, reloadError: null, reloadErrorCode: null }];
}

// src/server/config.ts DEFAULT_DISCORD_TEMPLATES and DISCORD_TEMPLATE_VARIABLES.
const DISCORD_DEFAULTS = {
  templates: {
    error: 'Playback error **{errorCode}** on video #{videoIndex} (`{videoId}`)\nRetry attempt: {attempt}',
    skip: 'Skipping video #{videoIndex} (`{videoId}`)\nReason: {reason}',
    recovery: 'Recovery action: **{step}**',
    critical: '**CRITICAL:** {message}',
    resume: 'Playback resumed at video #{videoIndex} (`{videoId}`)',
    streamDrop: 'Stream stopped unexpectedly — attempting restart (attempt {attempt}/{maxAttempts})',
    streamRestart: 'Stream restarted successfully after {attempts} attempt(s)',
    outputFrozen: 'Output frozen ({kind}) on video #{videoIndex}',
    twitchMismatch: 'Twitch liveness mismatch — the encoder reports streaming but **{channel}** is offline on Twitch. Restarting stream.',
    twitchRestart: 'Stream restarted after Twitch liveness mismatch on **{channel}**',
  },
  variables: {
    error: ['videoIndex', 'videoId', 'errorCode', 'attempt'],
    skip: ['videoIndex', 'videoId', 'reason'],
    recovery: ['step'],
    critical: ['message'],
    resume: ['videoIndex', 'videoId'],
    streamDrop: ['attempt', 'maxAttempts'],
    streamRestart: ['attempts'],
    outputFrozen: ['kind', 'videoIndex'],
    twitchMismatch: ['channel'],
    twitchRestart: ['channel'],
  },
};

/** `POST /api/discord/test`: Node's answer, without sending anything to Discord. */
function discordTest() {
  if (!webhookUrl) return [400, { error: 'No Discord webhook URL configured' }];
  return [200, { ok: true }];
}

// No check has run until the first POST /api/setup-check/run, as on a fresh install.
const INSTALL = 'C:\\StreamLoop3\\streamloop';
const SETUP_CHECK_DELAY_MS = 3000;
let setupReport = null;

async function runSetupCheck() {
  await new Promise((resolve) => setTimeout(resolve, SETUP_CHECK_DELAY_MS));
  setupReport = {
    at: new Date().toISOString(),
    encoder: { chosen: 'h264_nvenc', tried: ['h264_nvenc'], software: false, works: true },
    cache: { freeMb: 182340, recommendedMb: 4096, warning: null },
    dependencies: [
      ['Supervisor', 'supervisor\\streamloop-supervisor.exe'],
      ['Standby slate', 'supervisor\\slate.mp4'],
      ['ffmpeg', 'ffmpeg\\ffmpeg.exe'],
      ['yt-dlp', 'yt-dlp\\yt-dlp.exe'],
      ['Deno', 'yt-dlp\\deno.exe'],
      ['PO-token plugin', 'yt-dlp\\plugins\\bgutil-ytdlp-pot-provider.zip'],
      ['Token server', 'pot\\build\\main.js'],
      ['Token server modules', 'pot\\node_modules'],
    ].map(([name, rel]) => ({ name, ok: name !== 'Token server modules', path: `${INSTALL}\\${rel}` }))
      .concat({ name: 'Overlay font', ok: true, path: 'C:\\Windows\\Fonts\\malgun.ttf' }),
  };
  return [200, setupReport];
}

function videoPage(query) {
  const which = Number(query.get('playlist') ?? '0');
  const list = lists[which];
  if (!list) return [400, { error: 'Invalid playlist index' }];
  const page = Math.max(1, Number(query.get('page')) || 1);
  const perPage = Math.min(100, Math.max(1, Number(query.get('perPage')) || 25));
  const start = (page - 1) * perPage;
  return [200, {
    videos: list.videos.slice(start, start + perPage),
    currentIndex: which === state.list ? state.index : null,
    total: list.videos.length,
    page,
    perPage,
  }];
}

const GETS = {
  '/api/token': () => [200, { token: TOKEN }],
  '/api/status': () => [200, status()],
  '/api/events': () => [200, events],
  '/api/update/status': () => [200, { currentVersion: '3.0.5', latestVersion: '3.0.5', updateAvailable: false, status: 'idle', isDevMode: true, nodeNotice: null }],
  '/api/config': () => [200, servedConfig()],
  '/api/autostart': () => [200, { enabled: true, target: 'C:\\StreamLoop3\\streamloop\\START.bat', thisInstall: true }],
  '/api/setup-check': () => [200, setupReport],
  '/api/discord/defaults': () => [200, DISCORD_DEFAULTS],
  '/api/logs': () => [200, logFiles.map(({ source, name, text, mtime }) => ({ name, size: Buffer.byteLength(text), mtime, source }))],
};

const POSTS = {
  '/api/stream/stop': () => { state.stopped = true; addEvent('Stream stopped from the dashboard'); },
  '/api/stream/start': () => { state.stopped = false; addEvent('Stream started from the dashboard'); },
  '/api/player/next': () => seek(state.index + 1),
  '/api/player/prev': () => seek(state.index - 1),
  '/api/player/restart': () => seek(state.index),
  '/api/player/jump': (body) => seek(Number(body.index) || 0),
  '/api/playlist/switch': (body) => {
    const list = lists[body.playlistIndex];
    if (!Number.isInteger(body.playlistIndex) || !list) return [400, { error: 'Invalid playlist index' }];
    state.list = body.playlistIndex;
    const byId = list.videos.findIndex((v) => v.id === body.videoId);
    seek(byId >= 0 ? byId : Number(body.videoIndex) || 0);
  },
  '/api/config': saveConfig,
  '/api/setup-check/run': runSetupCheck,
  '/api/discord/test': discordTest,
};

function seek(index) {
  const n = lists[state.list].videos.length;
  state.index = ((index % n) + n) % n;
  state.position = 0;
  addEvent(`Now playing video ${state.index + 1}: ${current().title}`);
}

const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8' };

async function serveAdmin(path, res) {
  const rel = path === '/admin/' ? 'index.html' : path.slice('/admin/'.length);
  const file = normalize(join(adminDir, rel));
  if (!file.startsWith(adminDir + sep)) return send(res, 404, { error: 'Not found' });
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'Content-Type': MIME[extname(file)] ?? 'application/octet-stream' });
    res.end(body);
  } catch {
    send(res, 404, { error: 'Not found' });
  }
}

function send(res, code, json) {
  res.writeHead(code, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify(json));
}

async function readBody(req) {
  let raw = '';
  for await (const chunk of req) raw += chunk;
  try {
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
}

createServer(async (req, res) => {
  const url = new URL(req.url ?? '/', `http://127.0.0.1:${PORT}`);
  const path = url.pathname;
  if (path === '/' || path === '/admin') {
    res.writeHead(302, { Location: '/admin/' });
    return res.end();
  }
  if (path.startsWith('/admin/')) return serveAdmin(path, res);
  if (req.method === 'GET' && path === '/api/playlist/videos') return send(res, ...videoPage(url.searchParams));
  const log = req.method === 'GET' && path.match(/^\/api\/logs\/([^/]+)\/([^/]+)$/);
  if (log) {
    const text = logText(decodeURIComponent(log[1]), decodeURIComponent(log[2]));
    if (text === null) return send(res, 404, { error: 'Log file not found' });
    res.writeHead(200, { 'Content-Type': 'text/plain; charset=utf-8' });
    return res.end(text);
  }
  if (req.method === 'GET' && GETS[path]) return send(res, ...GETS[path]());
  if (req.method === 'POST' && POSTS[path]) {
    if (req.headers['x-api-token'] !== TOKEN) return send(res, 403, { error: 'Forbidden: invalid API token' });
    // A handler returns [code, body] for an answer of its own, or nothing for { ok: true }.
    const answer = await POSTS[path](await readBody(req));
    if (answer) return send(res, ...answer);
    return send(res, 200, { ok: true });
  }
  send(res, 404, { error: 'Not found' });
}).listen(PORT, '127.0.0.1', () => {
  process.stdout.write(`stub backend on http://127.0.0.1:${PORT}/admin/\n`);
});
