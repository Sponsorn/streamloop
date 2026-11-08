// Runs two supervisors on one playlist side by side and reports how each side's downloads fared.
//   node pair-session.mjs NAME --playlist ID[,ID...] --hours H [--a-zip ZIP] [--b-pot EXE | --b-ts]
//        [--b-plugin ZIP] [--b-expect TEXT] [--cookies SPEC] [--b-bin EXE]
// A is the released 3.0.5 full ZIP (its TypeScript token server under its own Node, ports 4417/4416).
// B is this branch's supervisor on 4427/4426, either adopting a second TypeScript server the harness
// starts (--b-ts) or owning the Rust one (--b-pot EXE). Both push to one local MediaMTX, on paths a and b.
// Output: supervisor/logs/pair-NAME/ (pair.json, status.jsonl, memory.csv, report.json, child logs).
// Touching the file `stop` in that folder ends the run early through the normal shutdown.
// Nothing here uses a file watcher or a streaming tail: every read is a bounded read.
import { spawn, spawnSync } from 'node:child_process';
import {
  mkdirSync, rmSync, renameSync, writeFileSync, readFileSync, existsSync, appendFileSync, readdirSync,
  lstatSync, rmdirSync, copyFileSync, openSync, closeSync, createReadStream, cpSync,
} from 'node:fs';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join, resolve, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import net from 'node:net';
import { TOKEN_PORT, FFMPEG_BIN } from './lib.mjs';
import { makeDevRoot, SOURCES } from './dev-root.mjs';
import { tokenReport, splitPairs, readLogsDir, readEventsDir } from './token-report.mjs';

const SUP_DIR = fileURLToPath(new URL('..', import.meta.url));
const REPO = fileURLToPath(new URL('../..', import.meta.url));
const TOOLS_DIR = join(REPO, 'spikes', 'tools');
export const MAIN_CHECKOUT = 'C:/Users/anton/Documents/App Development/Streamloop';
export const DEFAULT_A_ZIP = `${MAIN_CHECKOUT}/dist/streamloop-v3.0.5.zip`;
export const DEFAULT_B_PLUGIN = join(REPO, 'dist', 'pot-ts', 'bgutil-ytdlp-pot-provider.zip');
export const DEFAULT_B_BIN = join(SUP_DIR, 'target', 'release', 'streamloop-supervisor.exe');
export const PLUGIN_SHA256 = 'bce874dfa25896c2798e0f4f8147b7b22e785479eb1e459ab232bf2506c95016';
export const PORTS = { aApi: 4417, aToken: TOKEN_PORT, bApi: 4427, bToken: 4426 };
const SAMPLE_EVERY_MS = 10 * 60_000;
const TICK_MS = 60_000;
const IDENTITY_LIMIT_MS = 60_000;
const EXIT_WAIT_MS = 30_000;
const NOT_PLAYING_LIMIT_MS = 10 * 60_000;
const WATCHED = ['streamloop-supervisor', 'streamloop-pot', 'bgutil-pot', 'node', 'ffmpeg', 'yt-dlp', 'mediamtx', 'tail', 'grep'];
const USAGE = 'usage: node pair-session.mjs NAME --playlist ID[,ID...] --hours H [--a-zip ZIP] [--b-pot EXE | --b-ts] [--b-plugin ZIP] [--b-expect TEXT] [--cookies SPEC] [--b-bin EXE]';


export function parseArgs(argv) {
  const [name, ...rest] = argv;
  if (!name || name.startsWith('--') || !/^[\w.-]+$/.test(name)) throw new Error(`NAME is missing or has odd characters. ${USAGE}`);
  const o = { name, playlists: [], hours: NaN, aZip: DEFAULT_A_ZIP, bPot: null, bTs: false, bPlugin: DEFAULT_B_PLUGIN, bExpect: null, cookies: '', bBin: DEFAULT_B_BIN };
  const value = (i, flag) => {
    if (i + 1 >= rest.length || rest[i + 1].startsWith('--')) throw new Error(`${flag} needs a value`);
    return rest[i + 1];
  };
  for (let i = 0; i < rest.length; i += 1) {
    const flag = rest[i];
    if (flag === '--b-ts') { o.bTs = true; continue; }
    const v = value(i, flag);
    i += 1;
    if (flag === '--playlist') o.playlists = v.split(',').map((s) => s.trim()).filter(Boolean);
    else if (flag === '--hours') o.hours = Number(v);
    else if (flag === '--a-zip') o.aZip = resolve(v);
    else if (flag === '--b-pot') o.bPot = resolve(v);
    else if (flag === '--b-plugin') o.bPlugin = resolve(v);
    else if (flag === '--b-expect') o.bExpect = v;
    else if (flag === '--cookies') o.cookies = v;
    else if (flag === '--b-bin') o.bBin = resolve(v);
    else throw new Error(`unknown flag ${flag}. ${USAGE}`);
  }
  if (!o.playlists.length) throw new Error(`--playlist is required. ${USAGE}`);
  if (!(o.hours > 0)) throw new Error(`--hours must be a positive number. ${USAGE}`);
  if (o.bTs === (o.bPot !== null)) throw new Error(`give exactly one of --b-ts and --b-pot EXE. ${USAGE}`);
  o.bExpect ??= o.bTs ? 'bgutil 2.0.0' : 'streamloop-pot';
  return o;
}

/** Reads the hash from a `<hash>  <filename>` line, whatever name the line carries. */
export function parseSha256File(text) {
  const m = /^([0-9a-fA-F]{64})\s+\*?(.+?)\s*$/m.exec(text);
  if (!m) throw new Error('not a "<hash>  <filename>" checksum file');
  return { hash: m[1].toLowerCase(), file: m[2] };
}

/** The config both sides read; --target replaces ingest and key, so neither is written. */
export function pairConfig({ playlists, cookies = '' }) {
  return {
    playlists: playlists.map((id) => ({ id })),
    ytdlCookiesFromBrowser: cookies,
    supervisor: { encoder: 'h264_nvenc', playerClient: 'mweb', cacheBudgetMb: 300, readAheadSeconds: 20, overlay: true, freezeSeconds: 20, silenceSeconds: 30 },
  };
}

/** 3.0.5 takes --root and --target without --debug-faults (checked at d4aaf01), so A runs without it. */
export function supervisorArgs(side, root) {
  const target = `rtmp://127.0.0.1:1935/live/${side}`;
  if (side === 'a') return ['--root', root, '--target', target];
  return ['--root', root, '--debug-faults', '--api-port', String(PORTS.bApi), '--token-port', String(PORTS.bToken), '--target', target];
}

export const tsServerArgs = (root) => [join(root, 'pot', 'build', 'main.js'), '-H', '127.0.0.1', '-p', String(PORTS.bToken)];

/** 'wait' while B's server is unnamed or still within the limit, 'ok' once it names `expect`, 'mismatch' at the limit. */
export function judgeIdentity(server, expect, elapsedMs, limitMs = IDENTITY_LIMIT_MS) {
  if (typeof server === 'string' && server.includes(expect)) return 'ok';
  return elapsedMs >= limitMs ? 'mismatch' : 'wait';
}

/** A's server never reports `server` in /status (3.0.5): read /ping the way the supervisor does. null when nothing answered 200. */
export function pingIdentity(httpStatus, body) {
  if (httpStatus !== 200) return null;
  let v = null;
  try { v = JSON.parse(body); } catch { /* not JSON */ }
  if (typeof v?.implementation === 'string') return v.implementation;
  if (typeof v?.version === 'string') return `bgutil ${v.version}`;
  return 'unknown';
}

/** A must run its own bgutil server: named bgutil and owned by the supervisor. */
export function judgeA(identity, tokenState, elapsedMs, limitMs = IDENTITY_LIMIT_MS) {
  const named = judgeIdentity(identity, 'bgutil', elapsedMs, limitMs);
  if (named === 'ok' && tokenState !== 'owned') return elapsedMs >= limitMs ? 'mismatch' : 'wait';
  return named;
}

/** First time a side's /status said `playing`; keeps the earlier time once set. */
export const notePlaying = (first, status, nowMs) => first ?? (status?.state === 'playing' ? nowMs : null);

/** 'fail' when either side has not played within the limit after launch. */
export function playingVerdict(firstA, firstB, elapsedMs, limitMs = NOT_PLAYING_LIMIT_MS) {
  if (firstA !== null && firstB !== null) return 'ok';
  return elapsedMs >= limitMs ? 'fail' : 'wait';
}

export const bgutilPlugins = (names) => names.filter((n) => n.toLowerCase().startsWith('bgutil'));

/** The pid of the newest `token-server-spawn` event; texts are whole daily files, oldest first. */
export function lastSpawnPid(eventTexts) {
  let pid = null;
  for (const text of eventTexts) {
    for (const line of text.split(/\r?\n/)) {
      if (!line.includes('token-server-spawn"')) continue;
      try {
        const e = JSON.parse(line);
        if (e.kind === 'token-server-spawn' && Number.isInteger(e.pid)) pid = e.pid;
      } catch { /* a half-written last line */ }
    }
  }
  return pid;
}

/** CIM rows (one object or an array) to a pid to private MB map. */
export function parsePrivateMb(json) {
  const text = (json ?? '').trim();
  const rows = text ? [].concat(JSON.parse(text)) : [];
  return new Map(rows.map((r) => [Number(r.IDProcess), Math.round((Number(r.WorkingSetPrivate) / 1048576) * 10) / 10]));
}

export function memoryCsvRow(ts, side, pid, mb) {
  return `${ts},${side},${pid ?? ''},${mb ?? ''}`;
}

/** report.json: both sides over [fromMs, toMs) with every try, the videos only one side got, and how the run ended. */
export function assembleReport(filesA, filesB, fromMs, toMs, { endedBy = null, identity = null } = {}) {
  const a = tokenReport(filesA, fromMs, toMs);
  const b = tokenReport(filesB, fromMs, toMs);
  const pairs = splitPairs(a.rows, b.rows);
  return { window: { from: new Date(fromMs).toISOString(), to: new Date(toMs).toISOString() }, endedBy, identity, a, b, pairs };
}

/**
 * Processes this run may kill: a started pid that still carries the name it was started with (a reused pid does not),
 * or anything running from under its folder. `started` maps pid to lower-case exe name; `procs` are {pid, name, path, commandLine}.
 */
export function ownProcesses(procs, started, baseDir) {
  const norm = (s) => (s ?? '').replaceAll('\\', '/').toLowerCase();
  const base = `${norm(baseDir).replace(/\/+$/, '')}/`;
  return procs.filter((p) => started.get(p.pid) === p.name.toLowerCase() || norm(p.path).startsWith(base) || norm(p.commandLine).includes(base));
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const portOpen = (port) => new Promise((resolve$) => {
  const s = net.connect(port, '127.0.0.1');
  s.on('connect', () => { s.destroy(); resolve$(true); });
  s.on('error', () => resolve$(false));
});

async function sha256File(path) {
  const h = createHash('sha256');
  for await (const chunk of createReadStream(path)) h.update(chunk);
  return h.digest('hex');
}

/** Removes a folder without following a junction or symlink into its target. */
export function removeTree(path) {
  let stat;
  try { stat = lstatSync(path); } catch { return; }
  if (stat.isSymbolicLink() || !stat.isDirectory()) { rmSync(path, { force: true }); return; }
  for (const entry of readdirSync(path)) removeTree(join(path, entry));
  rmdirSync(path);
}

const psText = (command) => {
  const r = spawnSync('powershell', ['-NoProfile', '-Command', command], { encoding: 'utf8', windowsHide: true, timeout: 60_000 });
  return (r.stdout ?? '').trim();
};

function listWatched() {
  const names = WATCHED.map((n) => `'${n}.exe'`).join(',');
  const out = psText(`Get-CimInstance Win32_Process | Where-Object { $_.Name -in @(${names}) } | Select-Object ProcessId,Name,ExecutablePath,CommandLine | ConvertTo-Json -Compress`);
  const rows = out ? [].concat(JSON.parse(out)) : [];
  return rows.map((r) => ({ pid: r.ProcessId, name: r.Name, path: r.ExecutablePath ?? '', commandLine: r.CommandLine ?? '' }));
}

function privateMb(pids) {
  const good = pids.filter(Number.isInteger);
  if (!good.length) return new Map();
  const filter = good.map((p) => `IDProcess=${p}`).join(' OR ');
  return parsePrivateMb(psText(`Get-CimInstance Win32_PerfRawData_PerfProc_Process -Filter "${filter}" | Select-Object IDProcess,WorkingSetPrivate | ConvertTo-Json -Compress`));
}

const killTree = (pid) => spawnSync('taskkill', ['/T', '/F', '/PID', String(pid)], { windowsHide: true });

async function preflight(o) {
  const problems = [];
  const plugins = join(process.env.APPDATA ?? '', 'yt-dlp', 'plugins');
  const found = existsSync(plugins) ? bgutilPlugins(readdirSync(plugins)) : [];
  // Checked first: this is the one check that needs the user, and nothing may have started when it fails.
  for (const n of found) problems.push(`Move %APPDATA%\\yt-dlp\\plugins\\${n} aside first: 3.0.5 would load it.`);
  if (found.length) return problems;
  for (const [what, port] of Object.entries(PORTS)) if (await portOpen(port)) problems.push(`Port ${port} (${what}) is already taken.`);
  if (!existsSync(o.aZip)) problems.push(`${o.aZip} is missing.`);
  else {
    const shaFile = [o.aZip.replace(/\.zip$/i, '.sha256'), `${o.aZip}.sha256`].find(existsSync);
    if (!shaFile) problems.push(`No .sha256 beside ${o.aZip}.`);
    else {
      const want = parseSha256File(readFileSync(shaFile, 'utf8')).hash;
      const got = await sha256File(o.aZip);
      if (got !== want) problems.push(`${o.aZip} hashes to ${got}, its .sha256 says ${want}.`);
    }
  }
  if (!existsSync(o.bPlugin)) problems.push(`${o.bPlugin} is missing.`);
  else if ((await sha256File(o.bPlugin)) !== PLUGIN_SHA256) problems.push(`${o.bPlugin} is not the pinned 2.0.0 plugin (SHA-256 ${PLUGIN_SHA256}).`);
  if (!existsSync(o.bBin)) problems.push(`B's supervisor is missing: ${o.bBin}. Run cargo build --release in supervisor/.`);
  if (o.bPot && !existsSync(o.bPot)) problems.push(`${o.bPot} is missing.`);
  for (const source of Object.values(SOURCES)) if (!existsSync(source)) problems.push(`${source} is missing.`);
  if (!(await portOpen(1935)) && !existsSync(join(TOOLS_DIR, 'mediamtx.exe'))) problems.push(`Nothing listens on :1935 and ${TOOLS_DIR}/mediamtx.exe is missing.`);
  return problems;
}

async function main() {
  let o;
  try { o = parseArgs(process.argv.slice(2)); } catch (err) { console.error(err.message); return 2; }
  const problems = await preflight(o);
  if (problems.length) { for (const p of problems) console.error(p); return 2; }

  const base = join(tmpdir(), `sl-pair-${o.name}`);
  const out = join(SUP_DIR, 'logs', `pair-${o.name}`);
  const aRoot = join(base, 'a');
  const bRoot = join(base, 'b');
  const children = new Map(); // label to { child, exited }
  const started = new Map(); // pid to the exe name it was started with
  const pair = { name: o.name, playing: { a: null, b: null }, args: o, roots: { a: aRoot, b: bRoot }, ports: PORTS, aDebugFaults: false, bDebugFaults: true, identity: { a: null, b: null } };
  const savePair = () => writeFileSync(join(out, 'pair.json'), JSON.stringify(pair, null, 2));
  let startMs = null;
  let ranMediaMtx = false;
  let stopReason = null;
  let shutdownPromise = null;
  let exitCode = 0;

  const launch = (label, exe, args, env = process.env) => {
    const fd = openSync(join(out, `${label}.log`), 'a');
    const child = spawn(exe, args, { cwd: tmpdir(), stdio: ['ignore', fd, fd], windowsHide: true, env });
    closeSync(fd);
    const exited = new Promise((r) => child.once('exit', (code) => r(code)));
    child.on('error', (err) => console.error(`${label} failed to start: ${err.message}`));
    children.set(label, { child, exited });
    started.set(child.pid, basename(exe).toLowerCase());
    return child;
  };

  const halt = () => { if (stopReason) throw new Error(`stopping before launch: ${stopReason}`); };

  async function stopChild(label, apiPort) {
    const entry = children.get(label);
    if (!entry || entry.child.exitCode !== null) return;
    if (apiPort) await fetch(`http://127.0.0.1:${apiPort}/shutdown`, { method: 'POST', signal: AbortSignal.timeout(5000) }).catch(() => null);
    const closed = apiPort ? await Promise.race([entry.exited.then(() => true), sleep(EXIT_WAIT_MS).then(() => false)]) : false;
    if (!closed && entry.child.exitCode === null) killTree(entry.child.pid);
  }

  function shutdown(reason) {
    shutdownPromise ??= (async () => {
      stopReason ??= reason;
      const endMs = Date.now();
      try {
        await Promise.all([stopChild('supervisor-a', PORTS.aApi), stopChild('supervisor-b', PORTS.bApi)]);
        await stopChild('ts-server-b', null);
        if (ranMediaMtx) await stopChild('mediamtx', null);
      } catch (err) { console.error(`shutdown: ${err.message}`); exitCode ||= 1; }
      pair.endedBy = stopReason;
      pair.endedAt = new Date(endMs).toISOString();
      if (startMs !== null) {
        for (const [side, root] of [['a', aRoot], ['b', bRoot]]) {
          try { cpSync(join(root, 'data', 'logs'), join(out, `${side}-logs`), { recursive: true }); } catch (err) { console.error(`copying ${side} logs: ${err.message}`); exitCode ||= 1; }
        }
        try {
          const report = assembleReport(readLogsDir(join(aRoot, 'data', 'logs')), readLogsDir(join(bRoot, 'data', 'logs')), startMs, endMs, { endedBy: stopReason, identity: pair.identity });
          writeFileSync(join(out, 'report.json'), JSON.stringify(report, null, 2));
        } catch (err) { console.error(`report: ${err.message}`); exitCode ||= 1; }
      }
      try {
        // a child seen exiting has a pid that may belong to someone else by now
        const live = new Map([...started].filter(([pid]) => ![...children.values()].some((c) => c.child.pid === pid && c.child.exitCode !== null)));
        const found = ownProcesses(listWatched(), live, base);
        for (const p of found) killTree(p.pid);
        if (found.length) await sleep(2000);
        const remaining = found.length ? ownProcesses(listWatched(), live, base) : [];
        pair.leftover = { found, remaining };
        console.log(found.length ? `leftover check: ${found.length} found, ${remaining.length} still running` : 'leftover check: none');
        if (remaining.length) { console.error(JSON.stringify(remaining, null, 2)); exitCode ||= 1; }
      } catch (err) { console.error(`leftover check: ${err.message}`); exitCode ||= 1; }
      try { savePair(); } catch { /* the folder may not exist if setup failed early */ }
    })();
    return shutdownPromise;
  }

  const onSignal = (name) => () => { stopReason ??= name; exitCode ||= 1; shutdown(name).then(() => process.exit(exitCode)); };
  process.on('SIGINT', onSignal('SIGINT'));
  process.on('SIGTERM', onSignal('SIGTERM'));
  const onCrash = (err) => { console.error(err?.stack ?? String(err)); onSignal('uncaught-error')(); };
  process.on('uncaughtException', onCrash);
  process.on('unhandledRejection', onCrash);

  try {
    // removeTree never follows a junction into the main checkout
    removeTree(base);
    removeTree(out);
    mkdirSync(base, { recursive: true });
    mkdirSync(out, { recursive: true });

    halt();
    const tar = join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe'); // bsdtar reads ZIPs; git's tar does not
    const x = spawnSync(tar, ['-xf', o.aZip, '-C', base], { encoding: 'utf8', windowsHide: true });
    if (x.status !== 0) throw new Error(`extracting ${o.aZip} failed: ${x.stderr}`);
    renameSync(join(base, 'streamloop'), aRoot);

    halt();
    makeDevRoot(bRoot, { potExe: o.bPot, pluginZip: o.bPlugin });
    // B's yt-dlp and deno are hard links into the main checkout: remove before copying, never write through them
    for (const rel of ['yt-dlp/yt-dlp.exe', 'yt-dlp/deno.exe']) {
      rmSync(join(bRoot, rel), { force: true });
      copyFileSync(join(aRoot, rel), join(bRoot, rel));
    }
    pair.ytdlpSha256 = { a: await sha256File(join(aRoot, 'yt-dlp/yt-dlp.exe')), b: await sha256File(join(bRoot, 'yt-dlp/yt-dlp.exe')) };
    if (pair.ytdlpSha256.a !== pair.ytdlpSha256.b) throw new Error('the two yt-dlp.exe differ');
    pair.pluginSha256 = {
      a: await sha256File(join(aRoot, 'yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip')).catch(() => null),
      b: await sha256File(join(bRoot, 'yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip')),
    };
    if (pair.pluginSha256.a !== PLUGIN_SHA256) throw new Error(`the plugin in A is not the pinned 2.0.0 one (${pair.pluginSha256.a})`);
    const config = JSON.stringify(pairConfig({ playlists: o.playlists, cookies: o.cookies }), null, 2);
    for (const root of [aRoot, bRoot]) { mkdirSync(join(root, 'app'), { recursive: true }); writeFileSync(join(root, 'app', 'config.json'), config); }
    savePair();

    if (!(await portOpen(1935))) {
      halt();
      const yml = join(TOOLS_DIR, 'mediamtx.yml');
      launch('mediamtx', join(TOOLS_DIR, 'mediamtx.exe'), existsSync(yml) ? [yml] : []);
      ranMediaMtx = true;
      await sleep(3000);
      if (!(await portOpen(1935))) throw new Error('MediaMTX did not open :1935');
    }

    // no ffmpeg on the PATH: both supervisors must find every tool through their roots
    const env = { ...process.env, PATH: (process.env.PATH ?? '').split(';').filter((d) => !/chocolatey\\bin|ffmpeg/i.test(d) && d.replaceAll('\\', '/') !== FFMPEG_BIN).join(';') };
    let tsServer = null;
    if (o.bTs) {
      halt();
      tsServer = launch('ts-server-b', join(aRoot, 'node', 'node.exe'), tsServerArgs(aRoot), env);
      for (let i = 0; i < 30 && !(await portOpen(PORTS.bToken)); i += 1) await sleep(1000);
      if (!(await portOpen(PORTS.bToken))) throw new Error('the second TypeScript server did not open its port');
    }
    halt();
    launch('supervisor-a', join(aRoot, 'supervisor', 'streamloop-supervisor.exe'), supervisorArgs('a', aRoot), env);
    halt();
    launch('supervisor-b', o.bBin, supervisorArgs('b', bRoot), env);
    startMs = Date.now();
    pair.startedAt = new Date(startMs).toISOString();
    pair.pids = { a: children.get('supervisor-a').child.pid, b: children.get('supervisor-b').child.pid, tsServer: tsServer?.pid ?? null };
    savePair();

    // B must have adopted or started the server the run claims to measure, and A its own bgutil one.
    const statusOf = (port) => fetch(`http://127.0.0.1:${port}/status`, { signal: AbortSignal.timeout(5000) }).then((r) => r.json()).catch(() => null);
    const pingA = () => fetch(`http://127.0.0.1:${PORTS.aToken}/ping`, { signal: AbortSignal.timeout(5000) })
      .then(async (r) => pingIdentity(r.status, await r.text())).catch(() => null);
    const notePlayingBoth = (sa, sb) => {
      const now = Date.now();
      for (const [side, st] of [['a', sa], ['b', sb]]) {
        if (pair.playing[side] === null && notePlaying(null, st, now) !== null) pair.playing[side] = new Date(now).toISOString();
      }
    };
    const exitedLabel = () => [...children].find(([label, c]) => label !== 'mediamtx' && c.child.exitCode !== null)?.[0] ?? null;
    let verdictA = 'wait';
    let verdictB = 'wait';
    while ((verdictA === 'wait' || verdictB === 'wait') && !stopReason) {
      await sleep(2000);
      const elapsed = Date.now() - startMs;
      const [sa, sb, ia] = await Promise.all([statusOf(PORTS.aApi), statusOf(PORTS.bApi), pingA()]);
      notePlayingBoth(sa, sb);
      pair.identity.a = ia ?? pair.identity.a;
      pair.identity.b = sb?.tokenServer?.server ?? pair.identity.b;
      verdictA = judgeA(ia, sa?.tokenServer?.state, elapsed);
      verdictB = judgeIdentity(sb?.tokenServer?.server, o.bExpect, elapsed);
      const dead = exitedLabel();
      if (dead) throw new Error(`${dead} exited during start-up (${children.get(dead).child.exitCode})`);
    }
    savePair();
    if (verdictA === 'mismatch' || verdictB === 'mismatch') {
      console.error(`Token servers: A ${JSON.stringify(pair.identity.a)} (want bgutil, owned), B ${JSON.stringify(pair.identity.b)} (want ${JSON.stringify(o.bExpect)}). This run would not count.`);
      exitCode = 1;
      stopReason ??= 'identity-mismatch';
    } else if (!stopReason) {
      console.log(`identities: A ${pair.identity.a}, B ${pair.identity.b}`);
    }

    // Sample every 10 min until the hours run out or a stop arrives.
    const endAt = startMs + o.hours * 3_600_000;
    let nextSample = Date.now();
    const sampleOnce = async () => {
      const ts = new Date().toISOString();
      const pids = {
        a: lastSpawnPid(readEventsDir(join(aRoot, 'data', 'logs'))),
        b: o.bTs ? tsServer.pid : lastSpawnPid(readEventsDir(join(bRoot, 'data', 'logs'))),
      };
      const [sa, sb] = await Promise.all([statusOf(PORTS.aApi), statusOf(PORTS.bApi)]);
      appendFileSync(join(out, 'status.jsonl'), `${JSON.stringify({ ts, side: 'a', status: sa })}\n${JSON.stringify({ ts, side: 'b', status: sb })}\n`);
      const mb = privateMb([pids.a, pids.b]);
      if (!existsSync(join(out, 'memory.csv'))) appendFileSync(join(out, 'memory.csv'), 'ts,side,pid,privateMb\n');
      for (const side of ['a', 'b']) appendFileSync(join(out, 'memory.csv'), `${memoryCsvRow(ts, side, pids[side], mb.get(pids[side]))}\n`);
    };
    const sample = async () => {
      try { await sampleOnce(); } catch (err) { appendFileSync(join(out, 'sample-errors.log'), `${new Date().toISOString()} ${err.stack ?? err}\n`); }
    };
    while (!stopReason && Date.now() < endAt) {
      if (Date.now() >= nextSample) { await sample(); nextSample += SAMPLE_EVERY_MS; }
      const until = Math.min(Date.now() + TICK_MS, endAt);
      while (!stopReason && Date.now() < until) await sleep(1000);
      if (existsSync(join(out, 'stop'))) { stopReason = 'stop-file'; break; }
      const dead = exitedLabel();
      if (dead) { stopReason ??= dead === 'ts-server-b' ? 'ts-server-exited' : `${dead}-exited`; exitCode ||= 1; }
      if (!stopReason) {
        notePlayingBoth(...(await Promise.all([statusOf(PORTS.aApi), statusOf(PORTS.bApi)])));
        const first = (side) => (pair.playing[side] === null ? null : Date.parse(pair.playing[side]));
        if (playingVerdict(first('a'), first('b'), Date.now() - startMs) === 'fail') { stopReason = 'not-playing'; exitCode ||= 1; }
      }
    }
    await shutdown(stopReason ?? 'hours-elapsed');
  } catch (err) {
    console.error(err.stack ?? String(err));
    exitCode ||= 1;
    await shutdown('error');
  }
  return exitCode;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  process.exit(await main());
}
