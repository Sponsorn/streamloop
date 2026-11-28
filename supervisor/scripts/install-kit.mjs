// Helpers shared by install-session.mjs and R3's ui-session.mjs: an install started
// from its own START.bat, its control process's API (Node's in a 3.0 install), the RTMP reader, file
// hashes, the interrupt teardown and the sweep of what a killed harness left. Every child is spawned hidden,
// except the app, which startApp() starts as Explorer does. The process and window censuses, the foreground
// gate and the autostart reads serve the app's runs.
// useOut() names the results folder that note(), the console logs and requests.jsonl go to.
import * as cp from 'node:child_process';
import { mkdirSync, rmSync, readFileSync, writeFileSync, appendFileSync, readdirSync, openSync, closeSync, existsSync, statSync, copyFileSync, createReadStream } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, relative, basename, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import net from 'node:net';
import http from 'node:http';
import { FFPROBE, redact, exeMode, withModes } from './lib.mjs';

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
    "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('node.exe','cmd.exe','streamloop.exe','streamloop-supervisor.exe','streamloop-pot.exe','ffmpeg.exe','ffprobe.exe','yt-dlp.exe','mediamtx.exe') } | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine | ConvertTo-Json"],
    { encoding: 'utf8' });
  const v = JSON.parse(r.stdout || '[]');
  return withModes(Array.isArray(v) ? v : [v]);
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

/** PATH without the Chocolatey ffmpeg, so the install can only find its own tools. */
const pathNoFfmpeg = () => (process.env.PATH ?? '').split(';').filter((d) => !/chocolatey\\bin|ffmpeg/i.test(d)).join(';');

// One START.bat in a hidden cmd. Its console output goes to a file; stdin is NUL, so its
// `pause` lines return at once. STREAMLOOP_FROM_APP=1 says an app started it, so the control process opens no
// window of its own at its first start; a run that tests that window passes `{ fromApp: false }`.
export function startBat(root, label, extraEnv = {}, { fromApp = true } = {}) {
  const log = openSync(join(OUT, `console-${label}.log`), 'a');
  const env = { ...process.env, PATH: pathNoFfmpeg(), ...extraEnv };
  if (fromApp) env.STREAMLOOP_FROM_APP ??= '1';
  else delete env.STREAMLOOP_FROM_APP;
  const child = spawn('cmd.exe', ['/d', '/s', '/c', `""${join(root, 'START.bat')}""`], {
    windowsVerbatimArguments: true, stdio: ['ignore', log, log], cwd: root, env,
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
/** The files under `dir` that hold `needle`. A live install renames and deletes files as it goes (yt-dlp's
 *  `.ytdl` parts), so a file gone by the time it is read is skipped and not counted. */
export function grepTree(dir, needle, { read = readFileSync } = {}) {
  const hits = [];
  let files = 0;
  const walk = (d) => {
    for (const e of readdirSync(d, { withFileTypes: true })) {
      const p = join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.isFile()) {
        let bytes;
        try { bytes = read(p); } catch (err) { if (err?.code === 'ENOENT') continue; throw err; }
        files += 1;
        if (bytes.includes(needle)) hits.push(relative(dir, p));
      }
    }
  };
  if (existsSync(dir)) walk(dir);
  return { files, hits };
}

export async function extract(zip, dest) {
  // Retries: the folder has stayed locked (EPERM) for some seconds after the last run's processes exited.
  rmSync(dest, { recursive: true, force: true, maxRetries: 5, retryDelay: 1000 });
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
 *  non-ASCII install path comes back intact; each streamloop.exe also has its `mode` (`withModes`). */
export async function allProcesses() {
  const r = await run('powershell', ['-NoProfile', '-Command',
    '[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine,CreationDate | ConvertTo-Json -Compress'],
    { encoding: 'utf8' });
  return parseProcessCensus(r.stdout);
}

/** `Get-CimInstance Win32_Process | Select-Object ... | ConvertTo-Json` as `allProcesses` reads it: an array, the
 *  single object PowerShell writes for one process, or nothing. ConvertTo-Json writes a date as "/Date(<ms>)/". */
export function parseProcessCensus(text) {
  const body = String(text ?? '').replace(/^﻿/, '').trim();
  let v = null;
  try { v = JSON.parse(body); } catch { return []; }
  if (v === null || typeof v !== 'object') return [];
  return withModes((Array.isArray(v) ? v : [v]).map((p) => ({ ...p, created: Number(/\d+/.exec(String(p.CreationDate))?.[0] ?? 0) })));
}

/** An install's processes by role. `counts` holds the four a running StreamLoop has one each of. */
export function installCensus(procs) {
  const exe = (mode) => procs.filter((p) => String(p.Name ?? '').toLowerCase() === 'streamloop.exe' && (p.mode ?? exeMode(p.CommandLine)) === mode);
  const startBats = procs.filter((p) => String(p.Name ?? '').toLowerCase() === 'cmd.exe' && /\bstart\.bat\b/i.test(p.CommandLine ?? ''));
  const c = {
    apps: exe('ui'), detaches: exe('detach'), startBats, controls: exe('control'), supervisors: exe('supervisor'), launches: exe('launch'), checks: exe('check'),
  };
  return { ...c, counts: { apps: c.apps.length, startBats: c.startBats.length, controls: c.controls.length, supervisors: c.supervisors.length } };
}

// The window census, through a PowerShell script: EnumWindows for the visible top-level windows and their owners,
// and the foreground window with its monitor. Nothing here activates a window or sends input.

const WINDOW_TYPES = String.raw`using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class SlWindows {
  public class Info { public long Hwnd; public uint Pid; public string Title; public string ClassName; public int Left, Top, Right, Bottom; public bool Cloaked; public int MonLeft, MonTop, MonRight, MonBottom; }
  delegate bool EnumProc(IntPtr h, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] struct MONITORINFO { public int cbSize; public RECT rcMonitor; public RECT rcWork; public uint dwFlags; }
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr h, uint flags);
  [DllImport("user32.dll")] static extern bool GetMonitorInfoW(IntPtr m, ref MONITORINFO mi);
  [DllImport("user32.dll")] static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out int v, int size);
  static Info Describe(IntPtr h) {
    var i = new Info();
    i.Hwnd = h.ToInt64();
    uint pid; GetWindowThreadProcessId(h, out pid); i.Pid = pid;
    var title = new StringBuilder(512); GetWindowTextW(h, title, title.Capacity); i.Title = title.ToString();
    var cls = new StringBuilder(256); GetClassNameW(h, cls, cls.Capacity); i.ClassName = cls.ToString();
    RECT r; if (GetWindowRect(h, out r)) { i.Left = r.Left; i.Top = r.Top; i.Right = r.Right; i.Bottom = r.Bottom; }
    int cloaked; i.Cloaked = DwmGetWindowAttribute(h, 14, out cloaked, 4) == 0 && cloaked != 0;
    var mi = new MONITORINFO(); mi.cbSize = Marshal.SizeOf(typeof(MONITORINFO));
    if (GetMonitorInfoW(MonitorFromWindow(h, 2), ref mi)) { i.MonLeft = mi.rcMonitor.Left; i.MonTop = mi.rcMonitor.Top; i.MonRight = mi.rcMonitor.Right; i.MonBottom = mi.rcMonitor.Bottom; }
    return i;
  }
  public static List<Info> Visible() { var list = new List<Info>(); EnumWindows((h, l) => { if (IsWindowVisible(h)) list.Add(Describe(h)); return true; }, IntPtr.Zero); return list; }
  public static Info Foreground() { var h = GetForegroundWindow(); return h == IntPtr.Zero ? null : Describe(h); }
  public static bool Close(long h) { return PostMessageW(new IntPtr(h), 0x0010, IntPtr.Zero, IntPtr.Zero); }
}`;

/** The census script; it first posts WM_CLOSE to each of `close`. */
function windowScript(close = []) {
  return [
    '[Console]::OutputEncoding = [Text.Encoding]::UTF8',
    `Add-Type -TypeDefinition @'\n${WINDOW_TYPES}\n'@`,
    ...close.map((h) => `[void][SlWindows]::Close(${Number(h)})`),
    ...(close.length ? ['Start-Sleep -Milliseconds 1500'] : []),
    '$fg = [SlWindows]::Foreground()',
    '$name = $null',
    'if ($fg) { $p = Get-Process -Id $fg.Pid -ErrorAction SilentlyContinue; if ($p) { $name = $p.ProcessName } }',
    '[ordered]@{ foreground = $fg; foregroundName = $name; windows = @([SlWindows]::Visible()) } | ConvertTo-Json -Depth 3 -Compress',
  ].join('\n');
}
const encoded = (script) => Buffer.from(script, 'utf16le').toString('base64');

/** The foreground window (with its process `name`) and every visible top-level window. */
export async function windowCensus() {
  const r = await run('powershell', ['-NoProfile', '-NonInteractive', '-EncodedCommand', encoded(windowScript())], { encoding: 'utf8', timeout: 60_000 });
  return parseWindowCensus(r.stdout);
}
/** Posts WM_CLOSE to each window, as its close button does, and answers the census 1.5 s later. */
export async function closeWindows(hwnds) {
  const r = await run('powershell', ['-NoProfile', '-NonInteractive', '-EncodedCommand', encoded(windowScript(hwnds))], { encoding: 'utf8', timeout: 60_000 });
  return parseWindowCensus(r.stdout);
}

/** What the census script prints. A census that is not JSON comes back empty, with the text as `error`. */
export function parseWindowCensus(text) {
  const body = String(text ?? '').replace(/^﻿/, '').trim();
  let v = null;
  try { v = JSON.parse(body); } catch { return { foreground: null, windows: [], error: body.slice(0, 300) }; }
  const one = (w) => (w && typeof w === 'object' ? {
    hwnd: Number(w.Hwnd), pid: Number(w.Pid), title: w.Title ?? '', className: w.ClassName ?? '', cloaked: Boolean(w.Cloaked),
    rect: { left: w.Left, top: w.Top, right: w.Right, bottom: w.Bottom }, monitor: { left: w.MonLeft, top: w.MonTop, right: w.MonRight, bottom: w.MonBottom },
  } : null);
  const list = v?.windows == null ? [] : [v.windows].flat();
  const fg = one(v?.foreground);
  return { foreground: fg && { ...fg, name: v.foregroundName ?? null }, windows: list.map(one).filter(Boolean) };
}

/** The shown windows (visible and not cloaked) whose owner is one of `pids`. */
export const windowsOf = (census, pids) => census.windows.filter((w) => !w.cloaked && pids.includes(w.pid));

const SHELL_PROCESSES = new Set(['explorer', 'lockapp']);
const SHELL_CLASSES = new Set(['Progman', 'WorkerW', 'Shell_TrayWnd', 'Shell_SecondaryTrayWnd']);
/** A foreground window that covers its whole monitor and is not Explorer, the desktop or the lock screen. */
export function fullScreenApp(fg) {
  if (!fg || fg.cloaked) return false;
  if (SHELL_PROCESSES.has(String(fg.name ?? '').toLowerCase()) || SHELL_CLASSES.has(fg.className)) return false;
  const { rect: r, monitor: m } = fg;
  if (!(m?.right > m?.left && m?.bottom > m?.top)) return false;
  return r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
}

/** What owns the foreground now, for `foregroundGate`. */
export async function foregroundNow() {
  const fg = (await windowCensus()).foreground;
  return { fullScreen: fullScreenApp(fg), name: fg?.name ?? null, title: fg?.title ?? null };
}

export const SKIPPED = 'SKIPPED';
/** Before a step that opens or activates a window, whose Alt and SetForegroundWindow must never land on a game:
 *  while a full-screen app is in front this looks again every 30 s, and after 30 min answers SKIPPED. */
export async function foregroundGate({ label, probe = foregroundNow, wait = sleep, now = Date.now, limitMs = 30 * 60_000, everyMs = 30_000, say = note }) {
  const t0 = now();
  let looks = 0;
  for (;;) {
    const f = await probe();
    looks += 1;
    const waitedMs = now() - t0;
    if (!f.fullScreen) return { state: 'clear', waitedMs, looks, foreground: f };
    if (waitedMs >= limitMs) return { state: SKIPPED, waitedMs, looks, foreground: f };
    say(`${label} waits: a full-screen app is in front (${f.name ?? 'unknown'}); looking again in ${everyMs / 1000} s`);
    await wait(everyMs);
  }
}

/** The exe and the arguments of a Run value such as `"ROOT\streamloop.exe" ui --tray`; null for no value. */
export function runCommandParts(value) {
  const v = String(value ?? '').trim();
  if (!v) return null;
  const [, exe, rest] = /^"([^"]*)"\s*(.*)$/.exec(v) ?? /^(\S+)\s*(.*)$/.exec(v);
  return { exe, args: rest.split(/\s+/).filter(Boolean) };
}

/** A run's verdict from its checks: FAIL on a failed check, a stop, a failed cleanup or no checks at all;
 *  SKIPPED when a check was skipped and none failed, so a skip never reads as a plain PASS. */
export function verdictOf(checks, { stopped = false, cleanupOk = true } = {}) {
  const entries = Object.entries(checks ?? {});
  const skippedChecks = entries.filter(([, v]) => v === SKIPPED).map(([k]) => k);
  const failedChecks = entries.filter(([, v]) => v !== SKIPPED && !v).map(([k]) => k);
  const verdict = stopped || checks == null || failedChecks.length > 0 || !cleanupOk ? 'FAIL' : skippedChecks.length > 0 ? SKIPPED : 'PASS';
  return { verdict, failedChecks, skippedChecks };
}

/** Whether an app may start with its window: its gate was clear and no full-screen app is in front at this moment,
 *  since a gate taken before an install is half a minute old by the time the app starts. */
export const opensItsWindow = (gateClear, foregroundNow) => gateClear === true && foregroundNow?.fullScreen !== true;

/** Whether the app's window came to the front: another window (or none) was there before, and the app's is after. */
export const cameToTheFront = (before, after, pid) => before?.pid !== pid && after?.pid === pid;

/** Whether the timeline ends on the last `quitLine`, followed by at most one `exitReport` (the supervisor's own). */
export function endsOnTheQuitLine(lines, quitLine, exitReport) {
  const at = lines.lastIndexOf(quitLine);
  if (at < 0) return false;
  const after = lines.slice(at + 1);
  return after.length === 0 || (after.length === 1 && after[0] === exitReport);
}

/** The environment Explorer would give the app: no STREAMLOOP_START (which would make it the control process) and
 *  no STREAMLOOP_FROM_APP. */
export function explorerEnv(extra = {}) {
  const env = { ...process.env, PATH: pathNoFfmpeg(), ...extra };
  for (const k of Object.keys(env)) if (/^STREAMLOOP_(START|FROM_APP)$/i.test(k)) delete env[k];
  return env;
}
/** Starts `exe` as Explorer does, through PowerShell's Start-Process (ShellExecute): no console, its own folder as
 *  the working folder, `env` as its environment. Answers its PID, or null when it did not start. */
export async function startApp(exe, args = [], env = explorerEnv()) {
  const q = (s) => `'${String(s).replace(/'/g, "''")}'`;
  const list = args.length ? ` -ArgumentList ${args.map(q).join(',')}` : '';
  const r = await run('powershell', ['-NoProfile', '-NonInteractive', '-Command', `(Start-Process -FilePath ${q(exe)} -WorkingDirectory ${q(dirname(exe))}${list} -PassThru).Id`], { env, encoding: 'utf8', timeout: 60_000 });
  const pid = Number(String(r.stdout).trim());
  return Number.isInteger(pid) && pid > 0 ? pid : null;
}

/** A value under HKCU, read only: a string, a REG_BINARY as hex, or null when it is not there. */
export async function regValue(subkey, name) {
  const q = (s) => `'${String(s).replace(/'/g, "''")}'`;
  const script = `[Console]::OutputEncoding = [Text.Encoding]::UTF8
$v = Get-ItemProperty -LiteralPath ${q(`HKCU:\\${subkey}`)} -Name ${q(name)} -ErrorAction SilentlyContinue
if ($null -eq $v) { [Console]::Out.Write('<none>') } else { $x = $v.${q(name)}; if ($x -is [byte[]]) { $x = ($x | ForEach-Object { $_.ToString('x2') }) -join '' }; [Console]::Out.Write([string]$x) }`;
  const r = await run('powershell', ['-NoProfile', '-NonInteractive', '-EncodedCommand', encoded(script)], { encoding: 'utf8', timeout: 60_000 });
  const out = String(r.stdout ?? '').replace(/^﻿/, '');
  return out === '<none>' ? null : out;
}
/** Whether HKCU\`subkey` exists, read only. */
export async function regKeyExists(subkey) {
  const q = `'${`HKCU:\\${subkey}`.replace(/'/g, "''")}'`;
  const r = await run('powershell', ['-NoProfile', '-NonInteractive', '-EncodedCommand', encoded(`[Console]::Out.Write([string](Test-Path -LiteralPath ${q}))`)], { encoding: 'utf8', timeout: 60_000 });
  return String(r.stdout).trim() === 'True';
}

/** The user's own autostart entries, read only: StreamLoop under Run and StartupApproved\Run, StreamLoop.vbs under
 *  StartupApproved\StartupFolder, the Startup folder's files and the Run key's value names. Runs must leave this text
 *  as it was. */
export async function realAutostart() {
  const script = `[Console]::OutputEncoding = [Text.Encoding]::UTF8
$run = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run'
$approved = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run'
$v = (Get-ItemProperty -LiteralPath $run -Name StreamLoop -ErrorAction SilentlyContinue).StreamLoop
$a = (Get-ItemProperty -LiteralPath $approved -Name StreamLoop -ErrorAction SilentlyContinue).StreamLoop
if ($a -is [byte[]]) { $a = ($a | ForEach-Object { $_.ToString('x2') }) -join '' }
$folder = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\StartupFolder'
$f = (Get-ItemProperty -LiteralPath $folder -Name 'StreamLoop.vbs' -ErrorAction SilentlyContinue).'StreamLoop.vbs'
if ($f -is [byte[]]) { $f = ($f | ForEach-Object { $_.ToString('x2') }) -join '' }
$dir = [Environment]::GetFolderPath('Startup')
$lines = @("Run\\StreamLoop: $v", "StartupApproved\\Run\\StreamLoop: $a", "StartupApproved\\StartupFolder\\StreamLoop.vbs: $f", "Startup folder: $dir")
$lines += Get-ChildItem -LiteralPath $dir -Force | ForEach-Object { "  $($_.Name) $($_.Length) $($_.LastWriteTime.ToString('yyyy-MM-ddTHH:mm:ss'))" }
$lines += 'Run values: ' + ((Get-Item -LiteralPath $run).Property -join ', ')
[Console]::Out.Write(($lines -join [char]10))`;
  const r = await run('powershell', ['-NoProfile', '-NonInteractive', '-EncodedCommand', encoded(script)], { encoding: 'utf8', timeout: 60_000 });
  return String(r.stdout ?? '').replace(/^﻿/, '');
}

/** One HTTP request through node:http, which sends the Host and Origin it is given (fetch replaces a Host header
 *  with the URL's). `noHost` sends none. Answers the status and the body, status 0 when nothing answered. */
export function rawRequest({ port, method = 'GET', path, headers = {}, body = null, noHost = false, timeoutMs = 5000 }) {
  return new Promise((resolve) => {
    const req = http.request({ host: '127.0.0.1', port, method, path, headers, setHost: !noHost, timeout: timeoutMs }, (res) => {
      let text = '';
      res.setEncoding('utf8');
      res.on('data', (c) => { text += c; });
      res.on('end', () => resolve({ status: res.statusCode, body: text }));
    });
    req.on('timeout', () => req.destroy(new Error('timed out')));
    req.on('error', (err) => resolve({ status: 0, body: String(err) }));
    if (body !== null) req.write(body);
    req.end();
  });
}
/** The processes whose exe or command line names one of `dirs`; `ascii` also matches, in case a command
 *  line comes back in another codepage. */
export function under(procs, dirs, ascii = null) {
  const wanted = dirs.map((d) => d.toLowerCase());
  return procs.filter((p) => p.ProcessId !== process.pid && [p.ExecutablePath, p.CommandLine].some((x) => typeof x === 'string'
    && (wanted.some((d) => x.toLowerCase().includes(d)) || (ascii !== null && ascii.test(x)))));
}
export const runningFrom = async (dirs, ascii = null) => under(await allProcesses(), dirs, ascii);

// The app first, so it cannot start START.bat again; START.bat next, so it cannot restart the control
// process (Node in a 3.0 install); that before the supervisor, so its launcher cannot relaunch it.
// streamloop.exe ranks by its mode (the census's `mode`, else its command line alone).
const KILL_ORDER = ['streamloop.exe detach', 'streamloop.exe ui', 'cmd.exe', 'node.exe', 'streamloop.exe control', 'streamloop.exe launch', 'streamloop.exe supervisor', 'streamloop-supervisor.exe', 'streamloop-pot.exe'];
export async function killAll(procs) {
  const key = (p) => { const name = String(p.Name).toLowerCase(); return name === 'streamloop.exe' ? `${name} ${p.mode ?? exeMode(p.CommandLine)}` : name; };
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

// The test releases, shared by install-session.mjs and crates/ui/scripts/ui-session.mjs

/** The test builds after release `version`: N+1 to N+4 and the two broken N+1s, and the next minor version's
 *  .0 and .1, which a released install lands in the `from312` run. A dev version counts from its release number. */
export function variantsAfter(version) {
  const [major, minor, patch] = version.split('-')[0].split('.').map(Number);
  const v = (k) => `${major}.${minor}.${patch + k}`;
  return {
    n1: { version: v(1) },
    'n1-supervisor-fails': { version: v(1), feature: 'test-supervisor-fails' },
    'n1-control-fails': { version: v(1), feature: 'test-control-fails' },
    n2: { version: v(2) },
    n3: { version: v(3) },
    n4: { version: v(4) },
    'next-minor': { version: `${major}.${minor + 1}.0` },
    'next-minor-1': { version: `${major}.${minor + 1}.1` },
  };
}

/** The SHA-256 in a release's `.sha256` file (`<hash>  <file>`), or null when the text holds none. */
export function checksumOf(text) {
  const first = String(text ?? '').trim().split(/\s+/)[0].toLowerCase();
  return /^[0-9a-f]{64}$/.test(first) ? first : null;
}

/** What 2.x's stream guard printed as its last line, `{"streaming":true|false}`; null when it printed no verdict. */
export function guardVerdict(stdout) {
  const last = String(stdout ?? '').trim().split(/\r?\n/).at(-1) ?? '';
  try {
    const v = JSON.parse(last).streaming;
    return typeof v === 'boolean' ? v : null;
  } catch { return null; }
}
/** Above package.json's version, which names dist\'s build. */
export const VARIANTS = variantsAfter(JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8')).version);
export const exeName = (v) => `streamloop-v${v}.exe`;
/** Where the release list points the control process for each build's exe and checksum. */
export const ASSET_PORT = 8791;

/** Exit code and output of one run of a build, from the temp folder. */
async function exeRun(exe, args) {
  const r = await run(exe, args, { cwd: tmpdir(), encoding: 'utf8', timeout: 20_000 });
  return { code: r.status, out: `${r.stdout ?? ''}${r.stderr ?? ''}`.trim().split(/\r?\n/)[0]?.slice(0, 160) ?? '' };
}

/** cargo builds each of `ids` into its own target folder under `repo`, so one never rebuilds another; an
 *  unchanged one takes seconds. Each exe is checked for what it does at start, then put in `serve`\<id>\
 *  with its checksum. */
export async function buildVariants({ repo, serve, ids = Object.keys(VARIANTS) }) {
  const built = {};
  mkdirSync(serve, { recursive: true });
  for (const id of ids) {
    const v = VARIANTS[id];
    const t0 = Date.now();
    const args = ['build', '--release', '-p', 'streamloop', '--target-dir', join('target', 'install-session', id)];
    if (v.feature) args.push('--features', v.feature);
    const r = await run('cargo', args, { cwd: repo, encoding: 'utf8', timeout: 1_800_000, env: { ...process.env, STREAMLOOP_BUILD_VERSION: v.version } });
    if (r.status !== 0) throw new Stop(`cargo build of ${id} failed: ${String(r.stderr).slice(-2000)}`);
    const dir = join(serve, id);
    mkdirSync(dir, { recursive: true });
    const exe = join(dir, exeName(v.version));
    copyFileSync(join(repo, 'target', 'install-session', id, 'release', 'streamloop.exe'), exe);
    const hash = sha256File(exe);
    writeFileSync(join(dir, `${exeName(v.version)}.sha256`), `${hash}  ${exeName(v.version)}\n`);
    const runs = {
      version: await exeRun(exe, ['--version']),
      supervisor: await exeRun(exe, v.feature === 'test-supervisor-fails' ? ['supervisor', '--root', tmpdir()] : ['supervisor', '--bogus']),
      control: await exeRun(exe, v.feature === 'test-control-fails' ? ['control', '--root', tmpdir()] : ['control', '--bogus']),
    };
    // A plain build refuses a bad flag (3, 64); a broken one exits 1 in its mode before it reads anything.
    const ok = runs.version.code === 0 && runs.version.out === v.version
      && runs.supervisor.code === (v.feature === 'test-supervisor-fails' ? 1 : 3)
      && runs.control.code === (v.feature === 'test-control-fails' ? 1 : 64);
    if (!ok) throw new Stop(`the ${id} build does not behave as built: ${JSON.stringify(runs)}`);
    built[id] = { ...v, sha256: hash, bytes: statSync(exe).size, buildMs: Date.now() - t0, runs };
    note(`built ${id}`, built[id]);
  }
  return built;
}

/** The release list a control process reads (STREAMLOOP_TEST_RELEASES), written to `file`: GitHub's shape,
 *  each build's assets on 127.0.0.1:ASSET_PORT. */
export function writeReleaseList(file, ids) {
  const list = ids.map((id) => {
    const v = VARIANTS[id].version;
    const files = [`streamloop-v${v}.zip`, exeName(v), `${exeName(v)}.sha256`];
    return { tag_name: `v${v}`, draft: false, prerelease: true, html_url: '', assets: files.map((f) => ({ name: f, browser_download_url: `http://127.0.0.1:${ASSET_PORT}/${id}/${f}` })) };
  });
  writeFileSync(file, JSON.stringify(list, null, 2));
  return file;
}

/** The release host: serves `serve`\<id>\<file> for the test builds and pushes each request to `log`. */
export function assetHost(serve, log) {
  return http.createServer((req, res) => {
    const parts = decodeURIComponent(new URL(req.url, 'http://127.0.0.1').pathname).split('/').filter(Boolean);
    const file = parts.length === 2 && parts[0] in VARIANTS && !parts[1].includes('..') ? join(serve, parts[0], parts[1]) : null;
    const row = { ms: Date.now(), at: iso(), method: req.method, path: req.url };
    if (!file || !existsSync(file)) {
      res.writeHead(404).end();
      log.push({ ...row, status: 404 });
      return;
    }
    const bytes = statSync(file).size;
    res.writeHead(200, { 'content-length': bytes, 'content-type': 'application/octet-stream' });
    createReadStream(file).pipe(res);
    log.push({ ...row, status: 200, bytes });
  });
}
