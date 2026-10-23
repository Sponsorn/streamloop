// One whole supervisor run: MediaMTX + token-server check, a dev root (dev-root.mjs) with a temp
// config, the product binary, spike 1's watcher, a raw packet reader, a recorder, an optional
// drive.mjs, then the dummy-key grep, analyze + verdict.
//   node session.mjs <name> [--drive controls|faults|switch|resume] [--gap-s 60] [--key protected|target] [supervisor flags...]
// --key protected (default): a random dummy key, DPAPI-protected into the config, against
// rtmp://127.0.0.1:1935/live. --key target: --target rtmp://127.0.0.1:1935/live/spike instead.
// --gap-s only shortens the controls plan's spacing (for a shakedown); everything else goes to the supervisor.
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, renameSync, writeFileSync, readFileSync, existsSync, appendFileSync, unlinkSync, readdirSync, cpSync, createWriteStream } from 'node:fs';
import { randomBytes } from 'node:crypto';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import net from 'node:net';
import { API, TOKEN_SERVER_PING, FFMPEG, FFPROBE, FFMPEG_BIN, PLAYLIST, redact } from './lib.mjs';
import { makeDevRoot } from './dev-root.mjs';

const SUP_DIR = fileURLToPath(new URL('..', import.meta.url));
const TOOLS_DIR = fileURLToPath(new URL('../../spikes/tools', import.meta.url));
const CONTINUITY_CHECK = fileURLToPath(new URL('../../spikes/continuity/check.mjs', import.meta.url));
const BIN = fileURLToPath(new URL('../target/release/streamloop-supervisor.exe', import.meta.url));
const node = process.execPath;

const [name, ...rest] = process.argv.slice(2);
if (!name || name.startsWith('--')) {
  console.error('usage: node session.mjs <name> [--drive controls|faults|switch|resume] [--key protected|target] [supervisor flags...]');
  process.exit(2);
}
let drivePlan = null;
let gapS = '60';
let keyMode = 'protected';
const supArgs = [];
for (let i = 0; i < rest.length; i += 1) {
  if (rest[i] === '--drive') { drivePlan = rest[i += 1]; continue; }
  if (rest[i] === '--gap-s') { gapS = rest[i += 1]; continue; }
  if (rest[i] === '--key') { keyMode = rest[i += 1]; continue; }
  supArgs.push(rest[i]);
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const rtmpUp = () => new Promise((resolve) => {
  const socket = net.connect(1935, '127.0.0.1');
  socket.on('connect', () => { socket.destroy(); resolve(true); });
  socket.on('error', () => resolve(false));
});
const httpOk = async (url) => { try { return (await fetch(url)).ok; } catch { return false; } };

if (!(await httpOk(TOKEN_SERVER_PING))) {
  console.error(`PO-token server not answering at ${TOKEN_SERVER_PING}.`);
  console.error('Start it first: node C:/Users/anton/bgutil-ytdlp-pot-provider/server/build/main.js');
  process.exit(2);
}
let mediamtx = null;
if (!(await rtmpUp())) {
  if (!existsSync(`${TOOLS_DIR}/mediamtx.exe`)) { console.error(`Nothing listens on :1935 and ${TOOLS_DIR}/mediamtx.exe is missing.`); process.exit(2); }
  mediamtx = spawn(`${TOOLS_DIR}/mediamtx.exe`, [], { cwd: TOOLS_DIR, stdio: 'ignore' });
  mediamtx.on('error', (err) => console.error(`mediamtx failed to start: ${err.message}`));
  await sleep(3000);
  if (!(await rtmpUp())) { console.error('MediaMTX did not open :1935.'); process.exit(2); }
}

// dev root, fresh data, temp config
const root = makeDevRoot();
const dataDir = join(root, 'data');
const configFile = join(root, 'app', 'config.json');
rmSync(dataDir, { recursive: true, force: true });
let dummyKey = null;
let readUrl = 'rtmp://127.0.0.1:1935/live/spike';
const supervisorConfig = { ingestUrl: 'rtmp://127.0.0.1:1935/live', encoder: 'h264_nvenc', cacheBudgetMb: 300, readAheadSeconds: 20, overlay: true, freezeSeconds: 20, silenceSeconds: 30 };
if (keyMode === 'protected') {
  // long and random, so a grep for it in the data dir can only hit a leak
  dummyKey = `sl_dummy_${randomBytes(24).toString('hex')}`;
  const protect = spawnSync(BIN, ['protect'], { input: dummyKey, encoding: 'utf8' });
  if (protect.status !== 0) { console.error(`protect failed: ${protect.stderr}`); process.exit(2); }
  supervisorConfig.streamKeyProtected = protect.stdout.trim();
  readUrl = `rtmp://127.0.0.1:1935/live/${dummyKey}`;
} else {
  supArgs.unshift('--target', readUrl);
}
writeFileSync(configFile, JSON.stringify({ playlists: [{ id: PLAYLIST }], supervisor: supervisorConfig }, null, 2));

rmSync(`${SUP_DIR}/logs`, { recursive: true, force: true });
mkdirSync(`${SUP_DIR}/logs/rec`, { recursive: true });
mkdirSync(`${SUP_DIR}/results`, { recursive: true });
const log = (file, row) => appendFileSync(`${SUP_DIR}/logs/${file}`, `${row.join(',')}\n`);

// The supervisor must find every tool through its root: no ffmpeg on its PATH.
const noFfmpegPath = (process.env.PATH ?? '').split(';').filter((d) => !/chocolatey\\bin|ffmpeg/i.test(d)).join(';');
const console$ = createWriteStream(`${SUP_DIR}/logs/supervisor-console.log`, { flags: 'a' });
function launchSupervisor(args) {
  const child = spawn(BIN, ['--root', root, ...args], { cwd: SUP_DIR, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, PATH: noFfmpegPath } });
  child.stdout.pipe(console$, { end: false });
  child.stderr.pipe(console$, { end: false });
  child.on('error', (err) => console.error(`supervisor failed to start: ${err.message}`));
  return child;
}

// raw packet reader: reconnects across drops, tags each reconnect as a new segment
let readerSegment = 0;
let readerStop = false;
function runReader() {
  if (readerStop) return;
  const segment = readerSegment;
  readerSegment += 1;
  log('reader-reconnects.csv', [Date.now(), segment]);
  const probe = spawn(FFPROBE, ['-v', 'error', '-show_entries', 'packet=stream_index,dts_time', '-of', 'csv=p=0', readUrl],
    { stdio: ['ignore', 'pipe', 'ignore'] });
  probe.on('error', (err) => console.error(`reader ffprobe error: ${err.message}`));
  let tail = '';
  probe.stdout.on('data', (chunk) => {
    const lines = (tail + chunk).split(/\r?\n/);
    tail = lines.pop();
    for (const line of lines) {
      const [, dts] = line.split(',');
      if (!dts || dts === 'N/A') continue;
      log('reader.csv', [Date.now(), Number(dts).toFixed(3), segment]);
    }
  });
  probe.on('close', () => { if (!readerStop) sleep(1000).then(runReader); });
}

// spike 1's checker, unchanged, restarted whenever it exits on disconnect; it finds ffprobe on
// PATH, so the real one goes first
let watchStop = false;
function runWatch() {
  if (watchStop) return;
  const watch = spawn(node, [CONTINUITY_CHECK, 'watch', readUrl], { cwd: SUP_DIR, stdio: 'ignore', env: { ...process.env, PATH: `${FFMPEG_BIN.replaceAll('/', '\\')};${process.env.PATH}` } });
  watch.on('close', () => { if (!watchStop) sleep(1000).then(runWatch); });
}

// recorder: ffmpeg -c copy -copyts (file timestamps = reader dts, so analyze cuts clips by dts),
// restarts after a drop, file name carries wall-clock start
let recStop = false;
function runRecorder() {
  if (recStop) return;
  const outFile = `${SUP_DIR}/logs/rec/${Date.now()}.mkv`;
  const rec = spawn(FFMPEG, ['-v', 'error', '-i', readUrl, '-c', 'copy', '-copyts', outFile], { stdio: 'ignore' });
  rec.on('error', (err) => console.error(`recorder ffmpeg error: ${err.message}`));
  rec.on('close', () => { if (!recStop) sleep(1000).then(runRecorder); });
}

const DRIVE = fileURLToPath(new URL('./drive.mjs', import.meta.url));
let drive = null;
let supervisor;
let endedAt = null;
const startedAt = Date.now();

async function stopSupervisor(child) {
  // graceful first, so the run ends the way Node will end it; the kill is the fallback
  await fetch(`${API}/shutdown`, { method: 'POST' }).catch(() => null);
  const closed = await Promise.race([new Promise((resolve) => child.on('close', () => resolve(true))), sleep(15_000).then(() => false)]);
  if (!closed) spawnSync('taskkill', ['/T', '/F', '/PID', String(child.pid)]);
}

function snapshotProcesses(file) {
  const ps = spawnSync('powershell', ['-NoProfile', '-Command',
    "Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'ffmpeg.exe|yt-dlp.exe|node.exe' } | Select-Object ProcessId,Name,CommandLine | ConvertTo-Json"],
    { encoding: 'utf8' });
  writeFileSync(`${SUP_DIR}/logs/${file}`, redact(ps.stdout || '[]', dummyKey));
}

if (drivePlan === 'resume') {
  supervisor = launchSupervisor(supArgs);
  sleep(8000).then(() => { runReader(); runWatch(); runRecorder(); });
  await sleep(3 * 60_000);
  spawnSync('taskkill', ['/F', '/PID', String(supervisor.pid)]);
  // read after the kill: the dead supervisor can no longer move it, and the relaunch reads this one
  const before = JSON.parse(readFileSync(join(dataDir, 'state.json'), 'utf8'));
  writeFileSync(`${SUP_DIR}/logs/resume-before.json`, JSON.stringify(before, null, 2));
  await sleep(3000);
  snapshotProcesses('resume-leftover-processes.json');
  supervisor = launchSupervisor(supArgs);
  await sleep(5000);
  const after = await fetch(`${API}/status`).then((r) => r.json()).catch(() => null);
  writeFileSync(`${SUP_DIR}/logs/resume-after.json`, JSON.stringify({ before, after }, null, 2));
  await sleep(40_000);
  await stopSupervisor(supervisor);
  endedAt = Date.now();
} else {
  supervisor = launchSupervisor(supArgs);
  await sleep(8000); // the reader can only attach once the stream is published
  runReader(); runWatch(); runRecorder();
  if (drivePlan === 'controls' || drivePlan === 'faults' || drivePlan === 'switch') {
    drive = spawn(node, [DRIVE, drivePlan, '--gap-s', gapS, '--config', configFile], { cwd: SUP_DIR, stdio: 'inherit' });
    if (drivePlan === 'controls') {
      // Prove that killing the client leaves the stream alone: kill drive halfway between its 4th
      // and 5th command, relaunch with --resume-from.
      const sentRows = () => (existsSync(`${SUP_DIR}/logs/drive.csv`) ? readFileSync(`${SUP_DIR}/logs/drive.csv`, 'utf8').trim().split(/\r?\n/).filter(Boolean).length : 0);
      while (sentRows() < 4) await sleep(1000);
      await sleep(Number(gapS) * 500);
      drive.kill();
      log('drive-restarts.csv', [Date.now(), 'kill']);
      await sleep(2000);
      drive = spawn(node, [DRIVE, 'controls', '--gap-s', gapS, '--resume-from', String(sentRows())], { cwd: SUP_DIR, stdio: 'inherit' });
      log('drive-restarts.csv', [Date.now(), 'relaunch']);
    }
    if (drivePlan === 'faults') watchMediaMtxSignal();
  }
  const code = await new Promise((resolve) => supervisor.on('close', resolve));
  endedAt = Date.now();
  drive?.kill();
  console.log(`supervisor exited (${code})`);
}

// The "faults" plan asks the session to take MediaMTX down for a stretch; drive.mjs only ever
// talks to the session over this signal file, never touching MediaMTX itself.
function watchMediaMtxSignal() {
  const signalFile = `${SUP_DIR}/logs/mediamtx-control.json`;
  const timer = setInterval(async () => {
    if (!existsSync(signalFile)) return;
    clearInterval(timer);
    const { durationMs } = JSON.parse(readFileSync(signalFile, 'utf8'));
    unlinkSync(signalFile);
    log('mediamtx-events.csv', [Date.now(), 'down']);
    if (mediamtx) mediamtx.kill(); else spawnSync('taskkill', ['/F', '/IM', 'mediamtx.exe']);
    mediamtx = null;
    await sleep(durationMs);
    mediamtx = spawn(`${TOOLS_DIR}/mediamtx.exe`, [], { cwd: TOOLS_DIR, stdio: 'ignore' });
    log('mediamtx-events.csv', [Date.now(), 'up']);
  }, 500);
}

/** Every file under dir that contains the needle, as relative paths. */
function grepTree(dir, needle) {
  const hits = [];
  let files = 0;
  const walk = (d) => {
    for (const entry of readdirSync(d, { withFileTypes: true })) {
      const p = join(d, entry.name);
      if (entry.isDirectory()) walk(p);
      else if (entry.isFile()) { files += 1; if (readFileSync(p).includes(needle)) hits.push(relative(dir, p)); }
    }
  };
  if (existsSync(dir)) walk(dir);
  return { files, hits };
}

readerStop = true; watchStop = true; recStop = true;
await sleep(10_000); // let the reader/watcher see the stream end and flush their last rows
snapshotProcesses('leftover-processes.json');
console$.end();
await sleep(500);
if (dummyKey) {
  const data = grepTree(dataDir, dummyKey);
  const consoleHits = readFileSync(`${SUP_DIR}/logs/supervisor-console.log`).includes(dummyKey);
  writeFileSync(`${SUP_DIR}/logs/key-grep.json`, JSON.stringify({ dataFilesSearched: data.files, dataHits: data.hits, consoleHit: consoleHits }, null, 2));
}
writeFileSync(`${SUP_DIR}/logs/run-session.json`, JSON.stringify({ name, drivePlan, keyMode, supArgs, root, startedAt, endedAt }, null, 2));

const dest = `${SUP_DIR}/results/${name}`;
rmSync(dest, { recursive: true, force: true });
renameSync(`${SUP_DIR}/logs`, dest);
// the supervisor's own logs and state sit beside the session's files (no name is shared)
if (existsSync(join(dataDir, 'logs'))) cpSync(join(dataDir, 'logs'), dest, { recursive: true });
if (existsSync(join(dataDir, 'state.json'))) cpSync(join(dataDir, 'state.json'), join(dest, 'state.json'));
if (dummyKey) {
  const copy = grepTree(dest, dummyKey);
  const grep = JSON.parse(readFileSync(join(dest, 'key-grep.json'), 'utf8'));
  writeFileSync(join(dest, 'key-grep.json'), JSON.stringify({ ...grep, resultsFilesSearched: copy.files, resultsHits: copy.hits }, null, 2));
}

const ANALYZE = fileURLToPath(new URL('./analyze.mjs', import.meta.url));
const analysis = spawnSync(node, [ANALYZE, dest], { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
const report = `${new Date().toISOString()}  ${name}: ${drivePlan ?? 'plain'} ${keyMode} ${supArgs.join(' ')}\n${analysis.stdout}${analysis.stderr}`;
writeFileSync(`${dest}/verdict.txt`, report);
console.log(report);
console.log(`Saved ${dest}/`);
mediamtx?.kill();
process.exit(0);
