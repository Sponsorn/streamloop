// Spike 2 on the box: inventory, encoder detection, headroom, realtime pipeline, x264 reference and,
// when TWITCH_STREAM_KEY is set and the chosen encoder is not NVENC, a Twitch leg.
//   node.exe spike2.mjs [--force <codec>] [--minutes <n>] [--skip-twitch]
// Runs from the kit folder only: ffmpeg, ffprobe, mediamtx and the spike 1 scripts sit next to it.
import { spawn } from 'node:child_process';
import { appendFileSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { LOCAL_URL, normalizeVideoFilter, videoArgs } from '../continuity/lib.mjs';
import {
  HARDWARE, SAMPLER_PS, chooseEncoder, judge, parseSample, pipelineChecks, pipelineMeasures, stats, streamCheck, wsGrowthMb,
} from './spike2-lib.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const FFMPEG = join(here, 'ffmpeg.exe');
const FFPROBE = join(here, 'ffprobe.exe');
const POWERSHELL = join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
const MEDIA = join(here, 'media');

const argv = process.argv.slice(2);
const opt = (name, fallback) => (argv.includes(`--${name}`) ? argv[argv.indexOf(`--${name}`) + 1] : fallback);
const force = opt('force', null);
const minutes = Number(opt('minutes', '30'));
const skipTwitch = argv.includes('--skip-twitch');

for (const file of [FFMPEG, FFPROBE, join(here, 'mediamtx.exe'), join(here, 'run.mjs'), join(here, 'check.mjs'), POWERSHELL]) {
  if (!existsSync(file)) { console.error(`Missing ${file}. Run this from the unzipped kit folder.`); process.exit(2); }
}
const clips = readdirSync(MEDIA).filter((f) => f.endsWith('.mp4')).sort();
if (!clips.length) { console.error(`No .mp4 clips in ${MEDIA}.`); process.exit(2); }

const pad = (n) => String(n).padStart(2, '0');
const now = new Date();
const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}`;
const OUT = join(here, `results-${process.env.COMPUTERNAME ?? os.hostname()}-${stamp}`);
mkdirSync(OUT, { recursive: true });
// The spike 1 scripts call plain "ffmpeg" and "ffprobe"; the kit's copies go first.
const childEnv = { ...process.env, PATH: `${here};${process.env.PATH ?? ''}` };
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const say = (text) => { console.log(text); appendFileSync(join(OUT, 'spike2.log'), `${new Date().toISOString()}  ${text}\n`); };
const save = (name, data) => writeFileSync(join(OUT, name), typeof data === 'string' ? data : JSON.stringify(data, null, 2));

/** Runs to completion, killed after timeoutMs. */
function exec(cmd, args, { timeoutMs = 120_000, cwd = here } = {}) {
  return new Promise((resolve) => {
    const started = Date.now();
    const child = spawn(cmd, args, { cwd, env: childEnv, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d) => { stdout += d; });
    child.stderr.on('data', (d) => { stderr += d; });
    const timer = setTimeout(() => { stderr += `\n(killed after ${timeoutMs / 1000} s)`; child.kill(); }, timeoutMs);
    child.on('error', (e) => { stderr += String(e); });
    child.on('close', (code) => { clearTimeout(timer); resolve({ code, stdout, stderr, ms: Date.now() - started }); });
  });
}
const psArgs = (script) => ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')];

const rtmpUp = () => new Promise((resolve) => {
  const socket = net.connect(1935, '127.0.0.1');
  socket.on('connect', () => { socket.destroy(); resolve(true); });
  socket.on('error', () => resolve(false));
});

// ---- 1. Inventory
say(`Spike 2 kit. Results go to ${OUT}`);
say('Step 1: inventory');
const inventoryPs = `
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$os = Get-CimInstance Win32_OperatingSystem
ConvertTo-Json -Depth 4 @{
  cpu = @(Get-CimInstance Win32_Processor | ForEach-Object { @{ name = $_.Name.Trim(); cores = $_.NumberOfCores; threads = $_.NumberOfLogicalProcessors; maxMHz = $_.MaxClockSpeed } })
  gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object { @{ name = $_.Name; driverVersion = $_.DriverVersion; adapterRamMb = [math]::Round($_.AdapterRAM / 1MB); status = $_.Status } })
  ramGb = [math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 1)
  drives = @(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' | ForEach-Object { @{ drive = $_.DeviceID; freeGb = [math]::Round($_.FreeSpace / 1GB, 1); sizeGb = [math]::Round($_.Size / 1GB, 1) } })
  os = @{ caption = $os.Caption; version = $os.Version; build = $os.BuildNumber }
  culture = (Get-Culture).Name
}`;
const inv = await exec(POWERSHELL, psArgs(inventoryPs), { timeoutMs: 60_000 });
let inventory;
try { inventory = JSON.parse(inv.stdout); } catch { inventory = { error: inv.stderr || inv.stdout }; }
const version = await exec(FFMPEG, ['-hide_banner', '-version']);
const encoders = await exec(FFMPEG, ['-hide_banner', '-encoders']);
const hwaccels = await exec(FFMPEG, ['-hide_banner', '-hwaccels']);
Object.assign(inventory, {
  ffmpeg: version.stdout.split(/\r?\n/)[0],
  encoders: encoders.stdout.split(/\r?\n/).filter((l) => /^ V/.test(l)).map((l) => l.trim()),
  hwaccels: hwaccels.stdout.split(/\r?\n/).slice(1).map((l) => l.trim()).filter(Boolean),
  node: process.version,
  logicalCores: os.availableParallelism(),
});
save('inventory.json', inventory);
const gpuNames = (inventory.gpus ?? []).map((g) => g.name);
say(`  CPU ${inventory.cpu?.map((c) => c.name).join(', ')}; GPUs ${gpuNames.join(', ')}; RAM ${inventory.ramGb} GB`);

// ---- 2. Detection: only an encode that exits 0 and decodes back to 60 frames counts.
say('Step 2: detection');
mkdirSync(join(OUT, 'detection'), { recursive: true });
const detection = [];
for (const codec of HARDWARE) {
  const file = join(OUT, 'detection', `${codec}.mp4`);
  const enc = await exec(FFMPEG, ['-hide_banner', '-v', 'warning', '-y', '-f', 'lavfi', '-i', 'testsrc2=s=1920x1080:r=30', '-t', '2', ...videoArgs(codec), '-an', file], { timeoutMs: 60_000 });
  let ok = enc.code === 0 && existsSync(file);
  let error = enc.stderr.trim();
  if (ok) {
    const dec = await exec(FFPROBE, ['-v', 'error', '-count_frames', '-select_streams', 'v:0', '-show_entries', 'stream=nb_read_frames', '-of', 'csv=p=0', file]);
    const frames = Number(dec.stdout.trim());
    ok = dec.code === 0 && frames === 60;
    if (!ok) error = `decoded ${frames} frames (want 60): ${dec.stderr.trim()}`;
  }
  detection.push({ codec, ok, exitCode: enc.code, seconds: enc.ms / 1000, error });
  say(`  ${codec}: ${ok ? 'works' : `fails (${error.split('\n').filter(Boolean).slice(-1)[0] ?? `exit ${enc.code}`})`}`);
}
const chosen = chooseEncoder(detection, force);
save('detection.json', { detection, chosen, forced: Boolean(force) });
say(`  chosen: ${chosen ?? 'none'}${force ? ' (forced)' : ''}`);

// ---- 3. Headroom: 60 s of a real clip, unthrottled.
say('Step 3: headroom');
const headroom = [];
const measured = [...new Set([...detection.filter((d) => d.ok).map((d) => d.codec), ...(force && HARDWARE.includes(force) ? [force] : [])])];
for (const codec of measured) {
  const r = await exec(FFMPEG, ['-hide_banner', '-v', 'error', '-nostats', '-progress', 'pipe:1', '-i', join(MEDIA, clips[0]), '-t', '60',
    '-vf', normalizeVideoFilter(), ...videoArgs(codec), '-an', '-f', 'null', '-'], { timeoutMs: 600_000 });
  const frames = Number([...r.stdout.matchAll(/^frame=(\d+)/gm)].pop()?.[1] ?? 0);
  const fps = r.code === 0 ? frames / (r.ms / 1000) : NaN;
  headroom.push({ codec, clip: clips[0], frames, seconds: r.ms / 1000, fps, exitCode: r.code, error: r.code ? r.stderr.trim() : undefined });
  say(`  ${codec}: ${Number.isFinite(fps) ? `${fps.toFixed(0)} fps` : `failed: ${r.stderr.trim().split('\n').pop()}`}`);
}
save('headroom.json', headroom);

// ---- MediaMTX on loopback only, RTMP only: nothing listens on the network, so no firewall prompt.
let mediamtx = null;
if (!(await rtmpUp())) {
  mediamtx = spawn(join(here, 'mediamtx.exe'), [], {
    cwd: here, stdio: 'ignore', windowsHide: true,
    env: { ...childEnv, MTX_RTMPADDRESS: '127.0.0.1:1935', MTX_RTSP: 'no', MTX_HLS: 'no', MTX_WEBRTC: 'no', MTX_SRT: 'no', MTX_LOGDESTINATIONS: 'file', MTX_LOGFILE: join(OUT, 'mediamtx.log') },
  });
  await sleep(3000);
}
if (!(await rtmpUp())) { say('MediaMTX did not open 127.0.0.1:1935; steps 4 to 6 cannot run.'); }

const readCsv = (file) => (existsSync(file) ? readFileSync(file, 'utf8').trim().split(/\r?\n/).filter(Boolean).map((l) => l.split(',')) : []);

/** Spike 1's run.mjs and check.mjs watch with the real clips, plus counters every 5 s. */
async function pipeline(name, codec, runMinutes, { record = false, twitch = false } = {}) {
  const dir = join(OUT, name);
  mkdirSync(dir, { recursive: true });
  say(`  ${name}: ${codec} for ${runMinutes} min${twitch ? ' to Twitch (bandwidth test) and local' : ''}`);
  const cores = os.availableParallelism();
  let runInfo = {};
  const sampleRows = [];
  const sampler = spawn(POWERSHELL, psArgs(SAMPLER_PS), { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
  let samplerErr = '';
  sampler.stderr.on('data', (d) => { samplerErr += d; });
  let rest = '';
  let recorder = null;
  sampler.stdout.on('data', (chunk) => {
    const lines = (rest + chunk).split(/\r?\n/);
    rest = lines.pop();
    for (const line of lines.filter((l) => l.startsWith('{'))) {
      try {
        if (!runInfo.encoderPid) runInfo = JSON.parse(readFileSync(join(dir, 'logs', 'run.json'), 'utf8'));
      } catch { /* run.mjs has not written it yet */ }
      try {
        sampleRows.push({ t: Date.now(), ...parseSample(line, { encoderPid: runInfo.encoderPid, recorderPid: recorder?.pid, cores }) });
      } catch { /* a torn line */ }
    }
  });

  const runArgs = [join(here, 'run.mjs'), '--codec', codec, '--hours', String(runMinutes / 60), '--media', MEDIA, ...(twitch ? ['--twitch'] : [])];
  const runner = spawn(process.execPath, runArgs, { cwd: dir, env: childEnv, stdio: ['ignore', 'inherit', 'inherit'] });
  const done = new Promise((resolve) => runner.on('close', resolve));
  // The readers can only attach once the stream is published.
  await sleep(8000);
  const watch = spawn(process.execPath, [join(here, 'check.mjs'), 'watch'], { cwd: dir, env: childEnv, stdio: 'ignore', windowsHide: true });
  const recordSeconds = Math.min(300, runMinutes * 60 - 60);
  if (record && recordSeconds > 30) {
    await sleep(22_000);
    recorder = spawn(FFMPEG, ['-v', 'error', '-y', '-i', LOCAL_URL, '-t', String(recordSeconds), '-c', 'copy', join(dir, 'recording.mkv')], { stdio: 'ignore', windowsHide: true });
  }
  const code = await done;
  // Let the reader see the stream end and flush its last rows.
  await sleep(10_000);
  watch.kill();
  sampler.kill();
  recorder?.kill();

  const logs = join(dir, 'logs');
  const run = JSON.parse(readFileSync(join(logs, 'run.json'), 'utf8'));
  const rows = sampleRows.filter((r) => r.t >= run.startedAt && r.t <= (run.endedAt ?? Infinity));
  writeFileSync(join(dir, 'samples.csv'), [
    'time,cpu_total_pct,feeder_cpu_pct,encoder_cpu_pct,encoder_ws_private_mb,encode_engine_pct,encoder_engines',
    ...rows.map((r) => [new Date(r.t).toISOString(), r.cpuTotal.toFixed(2), r.feederCpu.toFixed(2), r.encoderCpu.toFixed(2),
      r.encoderWsMb?.toFixed(1) ?? '', r.encodeEngine.toFixed(2), Object.entries(r.encoderEngines).map(([k, v]) => `${k}=${v.toFixed(1)}`).join(' ')].join(',')),
  ].join('\n'));
  if (samplerErr.trim()) writeFileSync(join(dir, 'sampler-errors.txt'), samplerErr);

  const progress = readCsv(join(logs, 'encoder.csv')).map(([wallMs, mediaUs]) => ({ wallMs: Number(wallMs), mediaUs: Number(mediaUs) }));
  const m = progress.length > 1
    ? pipelineMeasures({ run, events: readCsv(join(logs, 'events.csv')), rows: progress, seamOffsets: readCsv(join(logs, 'seams.csv')).map((r) => Number(r[3])) })
    : { encoderExitedEarly: run.encoderExitedEarly, readerDisconnects: NaN, backwards: NaN, maxSeamRiseMs: NaN, overallSpeed: NaN, minutes: 0 };
  const usage = {
    cpu: stats(rows.map((r) => r.cpuTotal)),
    feederCpu: stats(rows.map((r) => r.feederCpu)),
    encoderCpu: stats(rows.map((r) => r.encoderCpu)),
    engine: stats(rows.map((r) => r.encodeEngine)),
    wsStartMb: rows.find((r) => Number.isFinite(r.encoderWsMb))?.encoderWsMb,
    wsGrowthMb: wsGrowthMb(rows),
  };
  const checks = pipelineChecks(m, usage);
  const summary = { name, codec, minutes: runMinutes, exitCode: code, m, usage, checks };
  writeFileSync(join(dir, 'summary.json'), JSON.stringify(summary, null, 2));
  say(`    speed ${m.overallSpeed?.toFixed(4)}, seam lag max ${m.maxSeamRiseMs?.toFixed(0)} ms, CPU avg ${usage.cpu.avg.toFixed(1)}% p95 ${usage.cpu.p95.toFixed(1)}%, engine avg ${usage.engine.avg.toFixed(1)}%, WS +${usage.wsGrowthMb.toFixed(1)} MB, spike 1 checks ${checks.hardPass ? 'pass' : 'FAIL'}`);
  return summary;
}

let main = null;
let stream = null;
let twitchResult = null;
if (chosen && await rtmpUp()) {
  say(`Step 4: realtime pipeline, ${minutes} min`);
  main = await pipeline(`pipeline-${chosen}`, chosen, minutes, { record: true });
  const recording = join(OUT, `pipeline-${chosen}`, 'recording.mkv');
  if (existsSync(recording) && statSync(recording).size > 0) {
    const s = await exec(FFPROBE, ['-v', 'error', '-select_streams', 'v:0', '-show_entries', 'stream=profile,width,height,avg_frame_rate,r_frame_rate,pix_fmt', '-of', 'json', recording]);
    const p = await exec(FFPROBE, ['-v', 'error', '-select_streams', 'v:0', '-show_entries', 'packet=pts_time,size,flags', '-of', 'json', recording], { timeoutMs: 300_000 });
    stream = streamCheck(JSON.parse(s.stdout).streams[0], JSON.parse(p.stdout).packets);
    save('streamcheck.json', { codec: chosen, recording, ...stream });
    say(`  stream: ${stream.profile} ${stream.size} ${stream.fps.toFixed(2)} fps ${stream.pixFmt}, ${stream.avgKbps.toFixed(0)} kbit/s, GOP ${[...new Set(stream.keyframeIntervals)].join('/')} s`);
  } else say('  no recording; stream format not checked');

  say('Step 5: x264 reference, 10 min');
  await pipeline('reference-libx264', 'libx264', 10);

  const key = process.env.TWITCH_STREAM_KEY;
  if (skipTwitch) twitchResult = 'NOT RUN (--skip-twitch)';
  else if (chosen === 'h264_nvenc') twitchResult = 'NOT RUN (NVENC, already proven on Twitch)';
  else if (!key) twitchResult = 'NOT RUN (TWITCH_STREAM_KEY not set)';
  else {
    say('Step 6: Twitch bandwidth test, 15 min');
    await pipeline(`twitch-${chosen}`, chosen, 15, { twitch: true });
    // ffmpeg prints the output URL in its errors; keep the key out of everything sent back.
    const scrub = (dir) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const file = join(dir, entry.name);
        if (entry.isDirectory()) scrub(file);
        else if (/\.(log|csv|json|txt)$/.test(entry.name)) {
          const text = readFileSync(file, 'utf8');
          if (text.includes(key)) writeFileSync(file, text.split(key).join('<key>'));
        }
      }
    };
    scrub(OUT);
    twitchResult = 'RAN: check Twitch Inspector for a stable 15 min session';
  }
}

const verdict = judge({ gpus: gpuNames, detection, chosen, headroom, pipeline: main, stream, twitch: twitchResult });
save('verdict.txt', verdict.text);
mediamtx?.kill();
console.log(`\n${verdict.text}`);
say(`Done. Zip and send back this folder: ${OUT}`);
