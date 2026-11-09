// Helpers shared by install-session.mjs and R3's ui-session.mjs: an install started
// from its own START.bat, its control process's API (Node's in a 3.0 install), the RTMP reader, file
// hashes, the interrupt teardown and the sweep of what a killed harness left. Every child is spawned hidden.
// useOut() names the results folder that note(), the console logs and requests.jsonl go to.
import * as cp from 'node:child_process';
import { mkdirSync, rmSync, readFileSync, writeFileSync, appendFileSync, readdirSync, openSync, closeSync, existsSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, relative, basename } from 'node:path';
import { tmpdir } from 'node:os';
import net from 'node:net';
import { FFPROBE, redact, exeMode } from './lib.mjs';

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
/** The API nodeUp() and api() talk to: NODE_API unless useApiPort() named another port. */
let apiBase = NODE_API;
export function useApiPort(port) {
  apiBase = `http://127.0.0.1:${port}/api`;
  return apiBase;
}
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
    "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('node.exe','cmd.exe','streamloop.exe','streamloop-supervisor.exe','ffmpeg.exe','ffprobe.exe','yt-dlp.exe','mediamtx.exe') } | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine | ConvertTo-Json"],
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
async function fetchToken() {
  try {
    const res = await fetch(`${apiBase}/token`, { signal: AbortSignal.timeout(2000) });
    return res.ok ? (await res.json()).token : null;
  } catch { return null; }
}
/** Waits for the API (the control process, or a 3.0 install's Node) and takes its token. */
export async function nodeUp(label, timeoutMs = 120_000) {
  const t0 = Date.now();
  const r = await waitFor(`the API (${label})`, fetchToken, timeoutMs, 500);
  if (!r.value) throw new Stop(`the API did not answer (${label})`);
  token = r.value;
  note(`API up (${label})`, { ms: Date.now() - t0 });
  return Date.now() - t0;
}
export async function api(method, path, body) {
  let status = 0;
  let text = '';
  // A restarted control process has a new token: one 403 takes the new one and tries again.
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const headers = method === 'POST' ? { 'x-api-token': token, 'content-type': 'application/json' } : {};
    try {
      const res = await fetch(apiBase + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(120_000) });
      status = res.status;
      text = await res.text();
    } catch (err) { status = 0; text = String(err); }
    if (status !== 403 || method !== 'POST' || attempt > 0) break;
    const fresh = await fetchToken();
    if (!fresh || fresh === token) break;
    token = fresh;
  }
  const recorded = path === '/stream-key' ? '[stream key, not recorded]' : body;
  // Not redacted: both harnesses grep their output folder for the key, which catches Node echoing it.
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
/** What a viewer got in [fromMs, toMs]: packets, reader reconnects and the longest stretch with no packet,
 *  the window's edges included (a window with no packet is all gap). */
export function gapStats(fromMs, toMs) {
  const win = packets.filter((p) => p.ms >= fromMs && p.ms <= toMs);
  const end = Math.min(toMs, Date.now());
  let maxWallGapMs = win.length ? Math.max(win[0].ms - fromMs, end - win.at(-1).ms) : end - fromMs;
  for (let i = 1; i < win.length; i += 1) maxWallGapMs = Math.max(maxWallGapMs, win[i].ms - win[i - 1].ms);
  return { packets: win.length, readerReconnects: reconnects.filter((t) => t > fromMs && t <= toMs).length, maxWallGapMs };
}
/** No reader reconnect, packets throughout and no stretch of `limitMs` or more without one. */
export const noDrop = (g, limitMs = 1000) => g != null && g.packets > 0 && g.readerReconnects === 0 && g.maxWallGapMs < limitMs;

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
export function grepTree(dir, needle) {
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

export async function extract(zip, dest) {
  rmSync(dest, { recursive: true, force: true });
  mkdirSync(dest, { recursive: true });
  const t0 = Date.now();
  const r = await run(TAR, ['-xf', zip, '-C', dest], { timeout: 600_000 });
  if (r.status !== 0) throw new Stop(`tar failed on ${zip}: ${r.stderr}`);
  return Date.now() - t0;
}
/** Extracts `zip` over what `dest` holds, as a user does when moving an install. A file in use is left as
 *  it was: `failed` names each entry tar could not write. */
export async function extractOver(zip, dest) {
  mkdirSync(dest, { recursive: true });
  const t0 = Date.now();
  const r = await run(TAR, ['-xf', zip, '-C', dest], { timeout: 600_000, encoding: 'utf8' });
  const failed = String(r.stderr ?? '').split(/\r?\n/).filter((l) => l.trim() && !/Error exit delayed/i.test(l));
  return { ms: Date.now() - t0, status: r.status, failed };
}
/** No browser, no yt-dlp self-update, no update check: the test must not open a window or touch yt-dlp\.
 *  `overrides` sets more keys, or turns one of these back on. */
export function quietConfig(file, overrides = {}) {
  const c = JSON.parse(readFileSync(file, 'utf8'));
  Object.assign(c, { autoOpenAdmin: false, autoUpdateYtdlp: false, autoUpdateCheck: false }, overrides);
  writeFileSync(file, JSON.stringify(c, null, 2));
}
export const portOpen = (port) => new Promise((resolve) => {
  const s = net.connect(port, '127.0.0.1');
  s.on('connect', () => { s.destroy(); resolve(true); });
  s.on('error', () => resolve(false));
});

/** Every process with its exe path, command line and start time (`created`, epoch ms), in UTF-8 so a
 *  non-ASCII install path comes back intact. */
export async function allProcesses() {
  const r = await run('powershell', ['-NoProfile', '-Command',
    '[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine,CreationDate | ConvertTo-Json -Compress'],
    { encoding: 'utf8' });
  try {
    const v = JSON.parse(r.stdout || '[]');
    // ConvertTo-Json writes a date as "/Date(<ms>)/".
    return (Array.isArray(v) ? v : [v]).map((p) => ({ ...p, created: Number(/\d+/.exec(String(p.CreationDate))?.[0] ?? 0) }));
  } catch { return []; }
}
/** The processes whose exe or command line names one of `dirs`; `ascii` also matches, in case a command
 *  line comes back in another codepage. */
export function under(procs, dirs, ascii = null) {
  const wanted = dirs.map((d) => d.toLowerCase());
  return procs.filter((p) => p.ProcessId !== process.pid && [p.ExecutablePath, p.CommandLine].some((x) => typeof x === 'string'
    && (wanted.some((d) => x.toLowerCase().includes(d)) || (ascii !== null && ascii.test(x)))));
}
export const runningFrom = async (dirs, ascii = null) => under(await allProcesses(), dirs, ascii);

// START.bat first, so it cannot restart the control process (Node in a 3.0 install); that before the
// supervisor, so its launcher cannot relaunch it. streamloop.exe ranks by its mode word.
const KILL_ORDER = ['cmd.exe', 'node.exe', 'streamloop.exe control', 'streamloop.exe launch', 'streamloop.exe supervisor', 'streamloop-supervisor.exe'];
export async function killAll(procs) {
  const key = (p) => { const name = String(p.Name).toLowerCase(); return name === 'streamloop.exe' ? `${name} ${exeMode(p.CommandLine)}` : name; };
  const rank = (p) => { const i = KILL_ORDER.indexOf(key(p)); return i < 0 ? KILL_ORDER.length : i; };
  const killed = [];
  for (const p of [...procs].sort((a, b) => rank(a) - rank(b))) {
    await run('taskkill', ['/F', '/T', '/PID', String(p.ProcessId)]);
    killed.push(`${p.Name} ${p.ProcessId}`);
  }
  return killed;
}

/** Ends what a harness killed outright left (processes under `dirs`, its orphaned MediaMTX and reader) and
 *  removes `dirs`; an install whose START.bat has a live parent is a running harness's, named in `owner`. */
export async function sweepLeftovers({ dirs, ascii = null, mediamtx }) {
  const report = { found: [], removed: [], owner: null };
  for (let pass = 0; pass < 3; pass += 1) {
    const all = await allProcesses();
    const byPid = new Map(all.map((p) => [p.ProcessId, p]));
    // A parent started after its child is a new process that took over a dead parent's PID.
    const parentOf = (p) => { const pp = byPid.get(p.ParentProcessId); return pp && pp.created <= p.created ? pp : null; };
    const inst = under(all, dirs, ascii);
    const owner = inst.filter((p) => p.Name === 'cmd.exe' && /start\.bat/i.test(p.CommandLine ?? ''))
      .map(parentOf).find((pp) => pp && !inst.includes(pp));
    if (owner) {
      report.owner = `${owner.Name} ${owner.ProcessId}`;
      return report;
    }
    const orphan = (p) => parentOf(p) === null;
    const tools = all.filter((p) => orphan(p) && ((p.Name === 'mediamtx.exe' && String(p.ExecutablePath).toLowerCase() === mediamtx.toLowerCase())
      || (p.Name === 'ffprobe.exe' && (p.CommandLine ?? '').includes(READ_URL))));
    const left = [...inst, ...tools];
    if (left.length === 0) break;
    report.found.push(...left.map((p) => `${p.Name} ${p.ProcessId}`).filter((x) => !report.found.includes(x)));
    await killAll(left);
    await sleep(2000);
  }
  for (const d of dirs.filter((x) => existsSync(x))) {
    try { rmSync(d, { recursive: true, force: true, maxRetries: 5, retryDelay: 1000 }); } catch { /* reported below */ }
    report.removed.push({ dir: basename(d), gone: !existsSync(d) });
  }
  return report;
}

/** The exe path of the process listening on 127.0.0.1:`port`, or null when nothing listens there. */
export async function listenerPath(port) {
  const r = await run('powershell', ['-NoProfile', '-Command',
    `[Console]::OutputEncoding = [Text.Encoding]::UTF8; $c = Get-NetTCPConnection -LocalPort ${Number(port)} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1; if ($c) { (Get-Process -Id $c.OwningProcess -ErrorAction SilentlyContinue).Path }`],
    { encoding: 'utf8' });
  return r.stdout.trim() || null;
}

/** First in a teardown, since a tool that interrupts can end this process within seconds. START.bat goes with
 *  its Node; /shutdown goes only to a listener whose exe lies under `installDirs`, as other sessions use 4417. */
export async function quickStop(bats, installDirs, { port = 4417, findListener = listenerPath } = {}) {
  const started = bats.filter(Boolean);
  for (const b of started) if (b.exitCode === null) await run('taskkill', ['/F', '/T', '/PID', String(b.pid)]);
  if (started.length === 0) return { shutdown: false, why: 'this harness started no START.bat' };
  const path = await findListener(port);
  const ours = path !== null && installDirs.some((d) => path.toLowerCase().startsWith(`${d.toLowerCase()}\\`));
  if (!ours) return { shutdown: false, why: path === null ? `nothing listens on ${port}` : `the ${port} listener runs from outside this harness's installs` };
  try { await fetch(`http://127.0.0.1:${port}/shutdown`, { method: 'POST', signal: AbortSignal.timeout(2000) }); } catch { /* it went away */ }
  return { shutdown: true, why: `the ${port} listener runs from this harness's install` };
}

/** Command lines naming one of the harness's temp folders (`names` are their ASCII starts). Any non-ASCII
 *  in the temp path itself matches any run of characters, so a line in another codepage still matches. */
export function tempFolders(names) {
  const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const prefix = esc(tmpdir()).replace(/[^\x00-\x7f]+/g, '[^\\\\]+');
  return new RegExp(`${prefix}\\\\(?:${names.map(esc).join('|')})`, 'i');
}

// SIGINT, SIGTERM or SIGBREAK runs every registered teardown, newest first, then exits 130. A second
// signal exits at once and leaves the rest to the next start's sweep.
const teardowns = [];
let signalled = null;
export function onInterrupt(fn) {
  teardowns.push(fn);
}
export const interrupted = () => signalled;
async function interrupt(signal) {
  if (signalled) {
    console.error(`${signal} again: exiting without finishing the teardown`);
    process.exit(130);
  }
  signalled = signal;
  try { note(`${signal}: tearing down`); } catch { console.error(`${signal}: tearing down`); }
  for (const fn of [...teardowns].reverse()) {
    try { await fn(signal); } catch (err) { console.error(`teardown failed: ${err?.stack ?? err}`); }
  }
  process.exit(130);
}
for (const s of ['SIGINT', 'SIGTERM', 'SIGBREAK']) process.on(s, () => void interrupt(s));
