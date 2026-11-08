// Helpers shared by install-session.mjs and update-session.mjs: an install started from its own
// START.bat, its Node API, the RTMP reader and file hashes. Every child is spawned hidden.
// useOut() names the results folder that note(), the console logs and requests.jsonl go to.
import * as cp from 'node:child_process';
import { mkdirSync, rmSync, readFileSync, writeFileSync, appendFileSync, readdirSync, openSync, closeSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, relative } from 'node:path';
import net from 'node:net';
import { FFPROBE, redact } from './lib.mjs';

export const spawn = (cmd, args, opts = {}) => cp.spawn(cmd, args, { windowsHide: true, ...opts });
export const run = (cmd, args, opts = {}) => new Promise((resolve) => {
  cp.execFile(cmd, args, { windowsHide: true, maxBuffer: 64 * 1024 * 1024, ...opts }, (err, stdout, stderr) => resolve({ status: err ? (err.code ?? 1) : 0, stdout, stderr }));
});
export const TAR = 'C:\\Windows\\System32\\tar.exe';
export const KEY = 'live_TESTKEY';
export const INGEST = 'rtmp://127.0.0.1:1935/live';
export const READ_URL = `${INGEST}/${KEY}`;
export const NODE_API = 'http://127.0.0.1:7654/api';
export const SUP = 'http://127.0.0.1:4417';
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const iso = (ms = Date.now()) => new Date(ms).toISOString();
export class Stop extends Error {}

let OUT = null;
/** Empties `dir` and sends the output from here on to it. */
export function useOut(dir) {
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  OUT = dir;
}
export const note = (msg, data) => {
  const line = redact(`${iso()} ${msg}${data === undefined ? '' : ` ${JSON.stringify(data)}`}`, KEY);
  console.log(line);
  appendFileSync(join(OUT, 'session.log'), `${line}\n`);
};

export async function ps() {
  const r = await run('powershell', ['-NoProfile', '-Command',
    "Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('node.exe','cmd.exe','streamloop-supervisor.exe','ffmpeg.exe','ffprobe.exe','yt-dlp.exe','mediamtx.exe') } | Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json"],
    { encoding: 'utf8' });
  const v = JSON.parse(r.stdout || '[]');
  return Array.isArray(v) ? v : [v];
}

export async function waitFor(what, fn, timeoutMs, everyMs = 1000) {
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
export function startBat(root, label, extraEnv = {}) {
  const log = openSync(join(OUT, `console-${label}.log`), 'a');
  const pathNoFfmpeg = (process.env.PATH ?? '').split(';').filter((d) => !/chocolatey\\bin|ffmpeg/i.test(d)).join(';');
  const child = spawn('cmd.exe', ['/d', '/s', '/c', `""${join(root, 'START.bat')}""`], {
    windowsVerbatimArguments: true, stdio: ['ignore', log, log], cwd: root,
    env: { ...process.env, PATH: pathNoFfmpeg, ...extraEnv },
  });
  child.on('exit', (code) => { closeSync(log); note(`START.bat (${label}) exited`, { code }); });
  return child;
}

export let token = '';
export async function nodeUp(label, timeoutMs = 120_000) {
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
export async function api(method, path, body) {
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
export const nodeStatus = async () => (await api('GET', '/status')).json;
export const supStatus = async () => { try { return await (await fetch(`${SUP}/status`, { signal: AbortSignal.timeout(3000) })).json(); } catch { return null; } };

/** Waits for live; a YouTube breaker that opens stops the run (no retry loop against YouTube). */
export async function waitLive(what, timeoutMs = 240_000) {
  return waitFor(what, async () => {
    const s = await nodeStatus();
    if (s?.supervisor?.youtube?.breaker === 'open') throw new Stop(`YouTube breaker open (${what}): ${JSON.stringify(s.supervisor.youtube)}`);
    return s?.stream?.state === 'live' ? s : null;
  }, timeoutMs, 2000);
}

// reader: one ffprobe on the RTMP output, reconnecting
export const packets = [];
export const reconnects = [];
let readerStop = false;
let probe = null;
// A reconnect still pending from a stopped reader must not start beside the next one.
let readerGen = 0;
export function runReader(gen = readerGen) {
  if (readerStop || gen !== readerGen) return;
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
  probe.on('close', () => { if (!readerStop) sleep(1000).then(() => runReader(gen)); });
}
export function stopReader() {
  readerStop = true;
  probe?.kill();
}
/** Forgets the packets of a stopped reader, so the next runReader() starts a fresh record. */
export function resetReader() {
  readerGen += 1;
  readerStop = false;
  packets.length = 0;
  reconnects.length = 0;
}
export function gapStats(fromMs, toMs) {
  const win = packets.filter((p) => p.ms >= fromMs && p.ms <= toMs);
  let maxWallGapMs = 0;
  for (let i = 1; i < win.length; i += 1) maxWallGapMs = Math.max(maxWallGapMs, win[i].ms - win[i - 1].ms);
  return { packets: win.length, readerReconnects: reconnects.filter((t) => t > fromMs && t <= toMs).length, maxWallGapMs };
}

export function hashTree(dir) {
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
export const sha256File = (f) => createHash('sha256').update(readFileSync(f)).digest('hex');

export async function extract(zip, dest) {
  rmSync(dest, { recursive: true, force: true });
  mkdirSync(dest, { recursive: true });
  const t0 = Date.now();
  const r = await run(TAR, ['-xf', zip, '-C', dest], { timeout: 600_000 });
  if (r.status !== 0) throw new Stop(`tar failed on ${zip}: ${r.stderr}`);
  return Date.now() - t0;
}
/** No browser, no yt-dlp self-update, no update check: the test must not open a window or touch yt-dlp\. */
export function quietConfig(file) {
  const c = JSON.parse(readFileSync(file, 'utf8'));
  Object.assign(c, { autoOpenAdmin: false, autoUpdateYtdlp: false, autoUpdateCheck: false });
  writeFileSync(file, JSON.stringify(c, null, 2));
}
/** Runs a snippet under an install's own Node and tsx, from that app's folder. */
export async function tsxEval(root, code, env = {}) {
  const r = await run(join(root, 'node', 'node.exe'), [join(root, 'app', 'node_modules', 'tsx', 'dist', 'cli.mjs'), '-e', code],
    { cwd: join(root, 'app'), encoding: 'utf8', timeout: 60_000, env: { ...process.env, ...env } });
  return { status: r.status, stdout: r.stdout.trim(), stderr: r.stderr.trim().slice(0, 2000) };
}

export const portOpen = (port) => new Promise((resolve) => {
  const s = net.connect(port, '127.0.0.1');
  s.on('connect', () => { s.destroy(); resolve(true); });
  s.on('error', () => resolve(false));
});
