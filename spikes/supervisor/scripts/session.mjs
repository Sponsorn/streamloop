// One whole supervisor run: MediaMTX + token-server check, the supervisor binary, spike 1's
// watcher, a raw packet reader, a recorder, an optional drive.mjs, then analyze + verdict.
//   node session.mjs <name> [--drive controls|faults|resume] [supervisor flags...]
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, renameSync, writeFileSync, readFileSync, existsSync, appendFileSync, unlinkSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import net from 'node:net';
import { API, TOKEN_SERVER_PING } from './lib.mjs';

const SUP_DIR = fileURLToPath(new URL('..', import.meta.url));
const TOOLS_DIR = fileURLToPath(new URL('../../tools', import.meta.url));
const CONTINUITY_CHECK = fileURLToPath(new URL('../../continuity/check.mjs', import.meta.url));
const BIN = fileURLToPath(new URL('../target/release/streamloop-supervisor.exe', import.meta.url));
const node = process.execPath;

const [name, ...rest] = process.argv.slice(2);
if (!name || name.startsWith('--')) {
  console.error('usage: node session.mjs <name> [--drive controls|faults|resume] [supervisor flags...]');
  process.exit(2);
}
let drivePlan = null;
const supArgs = [];
for (let i = 0; i < rest.length; i += 1) {
  if (rest[i] === '--drive') { drivePlan = rest[i += 1]; continue; }
  supArgs.push(rest[i]);
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const rtmpUp = () => new Promise((resolve) => {
  const socket = net.connect(1935, '127.0.0.1');
  socket.on('connect', () => { socket.destroy(); resolve(true); });
  socket.on('error', () => resolve(false));
});
const httpOk = async (url) => { try { return (await fetch(url)).ok; } catch { return false; } };

let mediamtx = null;
if (!(await rtmpUp())) {
  if (!existsSync(`${TOOLS_DIR}/mediamtx.exe`)) { console.error(`Nothing listens on :1935 and ${TOOLS_DIR}/mediamtx.exe is missing.`); process.exit(2); }
  mediamtx = spawn(`${TOOLS_DIR}/mediamtx.exe`, [], { cwd: TOOLS_DIR, stdio: 'ignore' });
  mediamtx.on('error', (err) => console.error(`mediamtx failed to start: ${err.message}`));
  await sleep(3000);
  if (!(await rtmpUp())) { console.error('MediaMTX did not open :1935.'); process.exit(2); }
}

if (!(await httpOk(TOKEN_SERVER_PING))) {
  console.error(`PO-token server not answering at ${TOKEN_SERVER_PING}.`);
  console.error('Start it first: node C:/Users/anton/bgutil-ytdlp-pot-provider/server/build/main.js');
  mediamtx?.kill();
  process.exit(2);
}

rmSync(`${SUP_DIR}/logs`, { recursive: true, force: true });
mkdirSync(`${SUP_DIR}/logs/rec`, { recursive: true });
mkdirSync(`${SUP_DIR}/results`, { recursive: true });
const log = (file, row) => appendFileSync(`${SUP_DIR}/logs/${file}`, `${row.join(',')}\n`);

function launchSupervisor(args) {
  const child = spawn(BIN, args, { cwd: SUP_DIR, stdio: 'inherit' });
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
  const probe = spawn('ffprobe', ['-v', 'error', '-show_entries', 'packet=stream_index,dts_time', '-of', 'csv=p=0', 'rtmp://127.0.0.1:1935/live/spike'],
    { stdio: ['ignore', 'pipe', 'ignore'] });
  probe.on('error', (err) => console.error(`reader ffprobe error: ${err.message}`));
  let rest = '';
  probe.stdout.on('data', (chunk) => {
    const lines = (rest + chunk).split(/\r?\n/);
    rest = lines.pop();
    for (const line of lines) {
      const [, dts] = line.split(',');
      if (!dts || dts === 'N/A') continue;
      log('reader.csv', [Date.now(), Number(dts).toFixed(3), segment]);
    }
  });
  probe.on('close', () => { if (!readerStop) sleep(1000).then(runReader); });
}
runReader();

// spike 1's checker, unchanged, restarted whenever it exits on disconnect
let watchStop = false;
function runWatch() {
  if (watchStop) return;
  const watch = spawn(node, [CONTINUITY_CHECK, 'watch'], { cwd: SUP_DIR, stdio: 'ignore' });
  watch.on('close', () => { if (!watchStop) sleep(1000).then(runWatch); });
}
await sleep(8000); // the reader can only attach once the stream is published
runWatch();

// recorder: ffmpeg -c copy, restarts after a drop, file name carries wall-clock start
let recStop = false;
function runRecorder() {
  if (recStop) return;
  const outFile = `${SUP_DIR}/logs/rec/${Date.now()}.mkv`;
  const rec = spawn('ffmpeg', ['-v', 'error', '-i', 'rtmp://127.0.0.1:1935/live/spike', '-c', 'copy', outFile], { stdio: 'ignore' });
  rec.on('error', (err) => console.error(`recorder ffmpeg error: ${err.message}`));
  rec.on('close', () => { if (!recStop) sleep(1000).then(runRecorder); });
}
runRecorder();

// drive.mjs, or the run-C crash/resume lifecycle, which only the session can do (it owns the PID)
const DRIVE = fileURLToPath(new URL('./drive.mjs', import.meta.url));
let drive = null;
let supervisor;
let endedAt = null;
const startedAt = Date.now();

if (drivePlan === 'resume') {
  supervisor = launchSupervisor(supArgs);
  await sleep(3 * 60_000);
  const before = JSON.parse(readFileSync(`${SUP_DIR}/state.json`, 'utf8'));
  writeFileSync(`${SUP_DIR}/logs/resume-before.json`, JSON.stringify(before, null, 2));
  spawnSync('taskkill', ['/F', '/PID', String(supervisor.pid)]);
  await sleep(3000);
  const ps = spawnSync('powershell', ['-NoProfile', '-Command',
    "Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'ffmpeg.exe|yt-dlp.exe|node.exe' } | Select-Object ProcessId,Name,CommandLine | ConvertTo-Json"],
    { encoding: 'utf8' });
  writeFileSync(`${SUP_DIR}/logs/resume-leftover-processes.json`, ps.stdout || '[]');
  supervisor = launchSupervisor([...supArgs, '--resume']);
  await sleep(5000);
  const after = await fetch(`${API}/status`).then((r) => r.json()).catch(() => null);
  writeFileSync(`${SUP_DIR}/logs/resume-after.json`, JSON.stringify({ before, after }, null, 2));
  await sleep(30_000);
  spawnSync('taskkill', ['/F', '/PID', String(supervisor.pid)]);
  endedAt = Date.now();
} else {
  supervisor = launchSupervisor(supArgs);
  if (drivePlan === 'controls' || drivePlan === 'faults') {
    await sleep(5000);
    drive = spawn(node, [DRIVE, drivePlan], { cwd: SUP_DIR, stdio: 'inherit' });
    if (drivePlan === 'controls') {
      // Prove that killing the client leaves the stream alone: kill drive midway, relaunch with --resume-from.
      await sleep(4 * 60_000);
      drive.kill();
      await sleep(2000);
      const sent = existsSync(`${SUP_DIR}/logs/drive.csv`) ? readFileSync(`${SUP_DIR}/logs/drive.csv`, 'utf8').trim().split(/\r?\n/).filter(Boolean).length : 0;
      drive = spawn(node, [DRIVE, 'controls', '--resume-from', String(sent)], { cwd: SUP_DIR, stdio: 'inherit' });
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
    mediamtx?.kill();
    mediamtx = null;
    await sleep(durationMs);
    mediamtx = spawn(`${TOOLS_DIR}/mediamtx.exe`, [], { cwd: TOOLS_DIR, stdio: 'ignore' });
    log('mediamtx-events.csv', [Date.now(), 'up']);
  }, 500);
}

readerStop = true; watchStop = true; recStop = true;
await sleep(10_000); // let the reader/watcher see the stream end and flush their last rows
writeFileSync(`${SUP_DIR}/logs/run-session.json`, JSON.stringify({ name, drivePlan, supArgs, startedAt, endedAt }, null, 2));

const dest = `${SUP_DIR}/results/${name}`;
rmSync(dest, { recursive: true, force: true });
renameSync(`${SUP_DIR}/logs`, dest);

const ANALYZE = fileURLToPath(new URL('./analyze.mjs', import.meta.url));
const analysis = spawnSync(node, [ANALYZE, dest], { encoding: 'utf8' });
const report = `${new Date().toISOString()}  ${name}: ${drivePlan ?? 'plain'} ${supArgs.join(' ')}\n${analysis.stdout}${analysis.stderr}`;
writeFileSync(`${dest}/verdict.txt`, report);
console.log(report);
console.log(`Saved ${dest}/`);
mediamtx?.kill();
process.exit(0);
