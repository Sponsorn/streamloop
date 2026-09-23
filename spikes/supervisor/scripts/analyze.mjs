// Computes and prints (and appends to verdict.txt via session.mjs) every number criteria 2-9 ask
// for. Reads only; nothing here talks to the supervisor. Missing supervisor-owned files (the
// binary doesn't exist yet while this is being written) degrade to 'n/a' instead of throwing.
//   node analyze.mjs <resultsDir>
import { readFileSync, existsSync, readdirSync, statSync, mkdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { median, parseCsv, calibrate, controlLatency, resumeErrorSec, deletionOk, firstEventAfter } from './lib.mjs';

const CONTINUITY_CHECK = fileURLToPath(new URL('../../continuity/check.mjs', import.meta.url));
const node = process.execPath;

const dir = process.argv[2];
if (!dir) { console.error('usage: node analyze.mjs <resultsDir>'); process.exit(2); }
const path = (f) => `${dir}/${f}`;
const readCsv = (f, columns) => (existsSync(path(f)) ? parseCsv(readFileSync(path(f), 'utf8'), columns) : []);
const readJson = (f) => (existsSync(path(f)) ? JSON.parse(readFileSync(path(f), 'utf8')) : null);
const readJsonl = (f) => (existsSync(path(f))
  ? readFileSync(path(f), 'utf8').trim().split(/\r?\n/).filter(Boolean).map((l) => JSON.parse(l)) : []);

const SEAM_COLS = ['seq', 'name', 'startedIso', 'offset', 'frames', 'code', 'exitIso', 'index', 'source', 'startPos', 'reason'];
const CONTROL_COLS = ['wallMs', 'cmd', 'index', 'targetIndex', 'targetId', 'cutOffset', 'flushedBytes'];
const ENCODER_COLS = ['wallMs', 'out_time_us', 'speed', 'generation'];
const DOWNLOAD_COLS = ['iso', 'id', 'result', 'attempt', 'bytes', 'ms', 'reason'];
const DELETE_COLS = ['iso', 'id', 'bytes', 'pView', 'fileEndTimeline'];
const BUFFER_COLS = ['iso', 'cacheBytes', 'onDisk', 'downloadingId', 'downloadingPct', 'playing', 'encoderMedia'];

const seams = readCsv('seams.csv', SEAM_COLS).map((r) => ({ ...r, offset: Number(r.offset), wallMs: Date.parse(r.startedIso) }));
const controls = readCsv('controls.csv', CONTROL_COLS).map((r) => ({ ...r, wallMs: Number(r.wallMs), cutOffset: Number(r.cutOffset) }));
const encoderRows = readCsv('encoder.csv', ENCODER_COLS)
  .map((r) => ({ wallMs: Number(r.wallMs), out_time_us: Number(r.out_time_us), generation: Number(r.generation) }))
  .filter((r) => Number.isFinite(r.wallMs) && Number.isFinite(r.out_time_us));
const downloads = readCsv('downloads.csv', DOWNLOAD_COLS).map((r) => ({ ...r, bytes: Number(r.bytes) }));
const deletes = readCsv('deletes.csv', DELETE_COLS);
const bufferRows = readCsv('buffer.csv', BUFFER_COLS).map((r) => ({ ...r, cacheBytes: Number(r.cacheBytes) }));
const events = readJsonl('events.jsonl').map((e) => ({ ...e, wallMs: e.wallMs ?? Date.parse(e.iso) }));
const driveRows = existsSync(path('drive.csv'))
  ? readFileSync(path('drive.csv'), 'utf8').trim().split(/\r?\n/).filter(Boolean).map((l) => {
    const i = l.indexOf(','); const j = l.indexOf(',', i + 1);
    return { wallMs: Number(l.slice(0, i)), cmd: l.slice(i + 1, j), payload: JSON.parse(l.slice(j + 1)) };
  }) : [];
const readerRows = readCsv('reader.csv', ['wallMs', 'dts', 'segment'])
  .map((r) => ({ wallMs: Number(r.wallMs), dts: Number(r.dts), segment: Number(r.segment) }))
  .sort((a, b) => a.wallMs - b.wallMs);
const reconnects = readCsv('reader-reconnects.csv', ['wallMs', 'segment']).map((r) => ({ wallMs: Number(r.wallMs), segment: Number(r.segment) }));
const mediamtxEvents = readCsv('mediamtx-events.csv', ['wallMs', 'action']).map((r) => ({ wallMs: Number(r.wallMs), action: r.action }));
const runSession = readJson('run-session.json') ?? {};
const supArgs = runSession.supArgs ?? [];
const flag = (name, fallback) => (supArgs.includes(`--${name}`) ? supArgs[supArgs.indexOf(`--${name}`) + 1] : fallback);

function readerDtsAt(wallMs) {
  const hit = readerRows.find((r) => r.wallMs >= wallMs);
  return hit ? hit.dts : null;
}
function generationAt(wallMs) {
  let g = encoderRows.length ? encoderRows[0].generation : 0;
  for (const r of encoderRows) { if (r.wallMs > wallMs) break; g = r.generation; }
  return g;
}
function lagRiseNear(wallMs, beforeMs = 60_000, windowMs = 5000) {
  if (!encoderRows.length) return null;
  const first = encoderRows[0];
  const lag = (r) => (r.wallMs - first.wallMs) - (r.out_time_us - first.out_time_us) / 1000;
  const baseline = encoderRows.filter((r) => r.wallMs >= wallMs - beforeMs && r.wallMs < wallMs).map(lag);
  const peak = encoderRows.filter((r) => r.wallMs >= wallMs && r.wallMs < wallMs + windowMs).map(lag);
  if (!baseline.length || !peak.length) return null;
  return Math.max(...peak) - median(baseline);
}

const out = {};
const print = (label, value) => { out[label] = value; console.log(label, JSON.stringify(value)); };

// calibration: per generation, first seam vs a later seam of the same generation
const byGeneration = new Map();
for (const s of seams) { const g = generationAt(s.wallMs); if (!byGeneration.has(g)) byGeneration.set(g, []); byGeneration.get(g).push(s); }
const calibrations = [];
for (const [generation, list] of byGeneration) {
  if (list.length < 2) continue;
  const result = calibrate(list[0], list[list.length - 1], readerDtsAt);
  if (result) calibrations.push({ generation, ...result });
}
print('calibration', calibrations);
const constantFor = (generation) => calibrations.find((c) => c.generation === generation)?.constant ?? 0;

// cold start
const firstNonSlate = seams.find((s) => s.source !== 'slate');
if (firstNonSlate && runSession.startedAt) {
  const firstPacketAfter = readerRows.find((r) => r.wallMs >= firstNonSlate.wallMs);
  print('coldStartMs', firstPacketAfter ? firstPacketAfter.wallMs - runSession.startedAt : null);
  print('coldStartPass', firstPacketAfter ? (firstPacketAfter.wallMs - runSession.startedAt) < 20_000 : false);
  print('handoverSeamLagRiseMs', lagRiseNear(firstNonSlate.wallMs));
} else {
  print('coldStartMs', 'n/a (no non-slate seam or no session start time)');
}

// per control
const controlResults = controls.map((c) => {
  const generation = generationAt(c.wallMs);
  const mappedDts = constantFor(generation) + c.cutOffset;
  const latencyMs = controlLatency(c.wallMs, mappedDts, readerRows);
  const drove = driveRows.filter((d) => Math.abs(d.wallMs - c.wallMs) < 5000 && (d.cmd === c.cmd || d.cmd === 'jump'));
  const statusMatch = drove.length
    ? drove.some((d) => String(d.payload.after?.index) === c.targetIndex || d.payload.after?.id === c.targetId)
    : null;
  return {
    seq: c.wallMs, cmd: c.cmd, targetIndex: c.targetIndex, latencyMs, statusMatch,
    lagRiseMs: lagRiseNear(c.wallMs), generationBefore: generation, generationAfter: generationAt(c.wallMs + 5000),
    generationUnchanged: generation === generationAt(c.wallMs + 5000),
  };
});
const watchEvents = existsSync(path('events.csv'))
  ? readFileSync(path('events.csv'), 'utf8').trim().split(/\r?\n/).filter(Boolean).map((l) => l.split(',')) : [];
for (const r of controlResults) {
  r.backwardsNearby = watchEvents.filter(([iso, type]) => type === 'backwards' && Math.abs(Date.parse(iso) - r.seq) < 5000).length;
  r.jumpsNearby = watchEvents.filter(([iso, type]) => type === 'jump' && Math.abs(Date.parse(iso) - r.seq) < 5000).length;
}
const latencies = controlResults.map((r) => r.latencyMs).filter((v) => v != null);
print('controls', controlResults);
print('controlLatencyMedianMs', latencies.length ? median(latencies) : null);
print('controlLatencyWorstMs', latencies.length ? Math.max(...latencies) : null);
print('controlLatencyPass', latencies.length ? Math.max(...latencies) < 5000 : null);

// per fault
const RECOVERY_EVENT_TYPES = ['retry', 'skip', 'encoder-restart', 'refresh', 'resume', 'republish', 'token-server-owned'];
const faultResults = driveRows.filter((d) => !['jump', 'next', 'prev', 'restart'].includes(d.cmd)).map((d) => {
  const recovery = firstEventAfter(events, d.wallMs, RECOVERY_EVENT_TYPES);
  return { kind: d.cmd, faultWallMs: d.wallMs, recoveryEvent: recovery, recoveryMs: recovery ? recovery.wallMs - d.wallMs : null };
});
print('faults', faultResults);

const restartSeams = seams.filter((s) => s.reason === 'encoder-restart');
print('encoderRestartResumeErrors', restartSeams.map((s) => {
  const before = driveRows.filter((d) => d.wallMs < s.wallMs).at(-1);
  const pView = before?.payload?.before?.position ?? before?.payload?.after?.position;
  return pView == null ? null : { seam: s.seq, errorSec: resumeErrorSec(pView, s.startPos), pass: resumeErrorSec(pView, s.startPos) <= 2 };
}).filter(Boolean));

if (mediamtxEvents.length) {
  const down = mediamtxEvents.find((e) => e.action === 'down');
  const up = mediamtxEvents.find((e) => e.action === 'up');
  const republish = up ? reconnects.find((r) => r.wallMs >= up.wallMs) : null;
  print('mediamtxRepublishMs', republish ? republish.wallMs - up.wallMs : null);
  print('mediamtxRepublishPass', republish ? republish.wallMs - up.wallMs < 15_000 : null);
  print('mediamtxDownMs', down && up ? up.wallMs - down.wallMs : null);
}

const tokenKill = driveRows.find((d) => d.cmd === 'kill-token-server');
if (tokenKill) {
  const owned = firstEventAfter(events, tokenKill.wallMs, ['token-server-owned', 'token-server-restart']);
  const ytdlpLog = existsSync(path('ytdlp.log')) ? readFileSync(path('ytdlp.log'), 'utf8') : '';
  const potLineAt = ytdlpLog.includes('[pot:bgutil:http]'); // presence check; per-line timing isn't logged
  print('tokenServerRecoveredMs', owned ? owned.wallMs - tokenKill.wallMs : null);
  print('tokenServerRecoveredPass', owned ? (owned.wallMs - tokenKill.wallMs) < 35_000 : null);
  print('tokenServerPotLineSeen', potLineAt);
}

// soak: shell out to spike 1's own verdict, which already reads run.json/events.csv/encoder.csv/seams.csv/rec
if (existsSync(path('run.json'))) {
  const soak = spawnSync(node, [CONTINUITY_CHECK, 'verdict', dir], { encoding: 'utf8' });
  print('soak_stdout', soak.stdout.trim());
  if (soak.stderr.trim()) print('soak_stderr', soak.stderr.trim());
} else {
  print('soak', 'n/a (no run.json yet)');
}

const budgetBytes = Number(flag('budget-mb', '0')) * 1024 * 1024;
const peakCache = bufferRows.length ? Math.max(...bufferRows.map((r) => r.cacheBytes)) : (readJson('run.json')?.maxCacheBytes ?? null);
const largestVideo = downloads.length ? Math.max(...downloads.map((r) => r.bytes || 0)) : 0;
print('peakCacheBytes', peakCache);
print('cacheWithinBudgetPass', peakCache != null && budgetBytes ? peakCache <= budgetBytes + largestVideo : null);

const badDeletes = deletes.filter((r) => !deletionOk(r));
print('deletesTotal', deletes.length);
print('deletesBadCount', badDeletes.length);
if (badDeletes.length) print('deletesBad', badDeletes);

// clips: every control cut, and every handover/refresh/retry seam
const clipsDir = path('clips');
const recDir = path('rec');
if (existsSync(recDir)) {
  mkdirSync(clipsDir, { recursive: true });
  const segments = readdirSync(recDir).filter((f) => f.endsWith('.mkv'))
    .map((f) => ({ file: `${recDir}/${f}`, start: Number(f.replace('.mkv', '')) })).sort((a, b) => a.start - b.start);
  const segmentFor = (wallMs) => {
    let seg = segments[0];
    for (const s of segments) { if (s.start > wallMs) break; seg = s; }
    return seg;
  };
  function cutClip(wallMs, reason, cmd) {
    const seg = segmentFor(wallMs);
    if (!seg) return;
    const ss = Math.max(0, (wallMs - seg.start) / 1000 - 5);
    const outFile = `${clipsDir}/${wallMs}-${reason}-${cmd}.mp4`;
    let r = spawnSync('ffmpeg', ['-v', 'error', '-y', '-ss', ss.toFixed(3), '-t', '10', '-i', seg.file, '-c', 'copy', outFile]);
    if (r.status !== 0 || !existsSync(outFile) || statSync(outFile).size === 0) {
      r = spawnSync('ffmpeg', ['-v', 'error', '-y', '-ss', ss.toFixed(3), '-t', '10', '-i', seg.file, '-c:v', 'libx264', '-c:a', 'aac', outFile]);
    }
    return r.status === 0;
  }
  let clipCount = 0;
  for (const c of controls) { if (cutClip(c.wallMs, 'control', c.cmd)) clipCount += 1; }
  for (const s of seams.filter((s) => ['handover', 'refresh', 'retry'].includes(s.reason))) { if (cutClip(s.wallMs, s.reason, '')) clipCount += 1; }
  print('clipsWritten', clipCount);
} else {
  print('clips', 'n/a (no rec/ directory)');
}

console.log('\n--- verdict ---');
console.log(JSON.stringify(out, null, 2));
