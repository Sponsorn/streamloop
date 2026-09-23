// Computes every number criteria 2-9 ask for from one results directory. Reads only; nothing
// here talks to the supervisor.
//   node analyze.mjs <resultsDir>
//
// "When did X reach the viewer" always means: X's timeline offset -> reader dts (offset - BASE,
// corrected per epoch by the error measured on the recording's slate edges) -> the first reader
// packet of that epoch whose dts reaches it. Never "the first packet after X's feeder started":
// the feeder runs up to 20 s ahead of the viewer.
import { readFileSync, existsSync, readdirSync, mkdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import {
  BASE, median, parseCsv, epochAt, effectiveSeams, viewAt, readerReach, riseAt, lagSpikes, slateEdge,
  resumeErrorSec, deletionOk, firstEventAfter, leaks,
} from './lib.mjs';

const dir = process.argv[2];
if (!dir) { console.error('usage: node analyze.mjs <resultsDir>'); process.exit(2); }
const path = (f) => `${dir}/${f}`;
const read = (f) => (existsSync(path(f)) ? readFileSync(path(f), 'utf8') : '');
const readCsv = (f, columns) => parseCsv(read(f), columns);
const readJson = (f) => (existsSync(path(f)) ? JSON.parse(readFileSync(path(f), 'utf8')) : null);
const lines = (f) => read(f).trim().split(/\r?\n/).filter(Boolean);
const iso = (ms) => new Date(ms).toISOString();
const r0 = (v) => (v == null ? null : Math.round(v));
const r3 = (v) => (v == null ? null : Math.round(v * 1000) / 1000);
const stats = (values) => {
  const v = values.filter((x) => x != null);
  return v.length ? { n: v.length, median: r0(median(v)), worst: r0(Math.max(...v)) } : { n: 0 };
};

const out = {};
const print = (label, value) => { out[label] = value; console.log(label, JSON.stringify(value)); };

// --- inputs -----------------------------------------------------------------------------------
const events = lines('events.jsonl').map((l) => JSON.parse(l)).map((e) => ({ ...e, wallMs: Date.parse(e.ts) }));
const epochStarts = events.filter((e) => e.kind === 'encoder-start').map((e) => e.wallMs);
const processStarts = events.filter((e) => e.kind === 'start').map((e) => e.wallMs);
const ep = (wallMs) => epochAt(epochStarts, wallMs);

// Seams from the feeder-start events, not seams.csv: seams.csv gets its row when a feeder exits,
// so a supervisor killed with taskkill /F (run C) never writes the rows of its live feeders.
const seams = events.filter((e) => e.kind === 'feeder-start')
  .map((e) => ({ seq: e.seq, name: e.name, startedIso: e.ts, offset: e.offset, index: e.index, source: e.source, startPos: e.pos, reason: e.reason, wallMs: e.wallMs }))
  .map((r) => ({ ...r, epoch: ep(r.wallMs) }))
  .sort((a, b) => a.wallMs - b.wallMs || a.seq - b.seq);
const epochCount = epochStarts.length;
const tables = Array.from({ length: epochCount }, (_, e) => effectiveSeams(seams.filter((s) => s.epoch === e)));

const controls = readCsv('controls.csv', ['wallMs', 'cmd', 'index', 'targetIndex', 'targetId', 'cutOffset', 'flushedBytes'])
  .map((r) => ({ ...r, wallMs: Number(r.wallMs), targetIndex: Number(r.targetIndex), cutOffset: Number(r.cutOffset) }));
const encoderRows = readCsv('encoder.csv', ['wallMs', 'us', 'speed', 'generation'])
  .map((r) => ({ wallMs: Number(r.wallMs), us: Number(r.us) })).filter((r) => Number.isFinite(r.wallMs) && Number.isFinite(r.us));
const points = Array.from({ length: epochCount }, () => []);
for (const r of encoderRows) { const e = ep(r.wallMs); if (e >= 0) points[e].push({ t: r.us / 1e6, lag: r.wallMs - r.us / 1000, wallMs: r.wallMs }); }

const downloads = readCsv('downloads.csv', ['iso', 'id', 'result', 'attempt', 'bytes', 'ms', 'reason']).map((r) => ({ ...r, bytes: Number(r.bytes) }));
const deletes = readCsv('deletes.csv', ['iso', 'id', 'bytes', 'pView', 'fileEndTimeline']);
const bufferRows = readCsv('buffer.csv', ['iso', 'cacheBytes', 'videosOnDisk', 'downloadingId', 'percent', 'playingName', 'encoderMedia', 'readAheadSeconds'])
  .map((r) => ({ ...r, cacheBytes: Number(r.cacheBytes), readAheadSeconds: Number(r.readAheadSeconds) }));
const driveRows = lines('drive.csv').map((l) => {
  const i = l.indexOf(','); const j = l.indexOf(',', i + 1);
  return { wallMs: Number(l.slice(0, i)), cmd: l.slice(i + 1, j), payload: JSON.parse(l.slice(j + 1)) };
});
const mediamtxEvents = readCsv('mediamtx-events.csv', ['wallMs', 'action']).map((r) => ({ wallMs: Number(r.wallMs), action: r.action }));
const driveRestarts = readCsv('drive-restarts.csv', ['wallMs', 'action']).map((r) => ({ wallMs: Number(r.wallMs), action: r.action }));
const runSession = readJson('run-session.json') ?? {};
const run = readJson('run.json') ?? {};
const endedAt = runSession.endedAt ?? run.endedAt ?? Infinity;

// reader packets, grouped by epoch through their connection (segment): one publish per segment
const readerAll = readCsv('reader.csv', ['wallMs', 'dts', 'segment'])
  .map((r) => ({ wallMs: Number(r.wallMs), dts: Number(r.dts), segment: Number(r.segment) })).sort((a, b) => a.wallMs - b.wallMs);
const segmentEpoch = new Map();
for (const r of readerAll) if (!segmentEpoch.has(r.segment)) segmentEpoch.set(r.segment, ep(r.wallMs));
const reader = Array.from({ length: epochCount }, () => []);
for (const r of readerAll) { const e = segmentEpoch.get(r.segment); if (e >= 0) reader[e].push(r); }

// spike 1's watcher: backwards / jump / disconnect, with the dts they happened at
const watch = lines('events.csv').map((l) => l.split(',')).map(([t, type, stream, at, value]) => ({ wallMs: Date.parse(t), type, stream, at: Number(at), value: Number(value) }));
const firstPacketMs = readerAll[0]?.wallMs ?? Infinity;
const watchNear = (e, fromDts, toDts) => watch.filter((w) => (w.type === 'backwards' || w.type === 'jump') && ep(w.wallMs) === e && w.at >= fromDts && w.at <= toDts)
  .map((w) => ({ type: w.type, stream: w.stream, at: w.at, value: w.value }));

// --- recording and dts calibration ------------------------------------------------------------
const recDir = path('rec');
const recFiles = existsSync(recDir) ? readdirSync(recDir).filter((f) => f.endsWith('.mkv')).map((f) => {
  const file = `${recDir}/${f}`;
  const [start, duration] = spawnSync('ffprobe', ['-v', 'error', '-show_entries', 'format=start_time,duration', '-of', 'csv=p=0', file], { encoding: 'utf8' })
    .stdout.trim().split(',').map(Number);
  return { file, wallMs: Number(f.replace('.mkv', '')), start, end: start + duration };
}).filter((r) => Number.isFinite(r.start)).map((r) => ({ ...r, epoch: ep(r.wallMs) })) : [];
const recFor = (e, dts) => recFiles.find((r) => r.epoch === e && r.start <= dts && r.end >= dts) ?? null;

function framesAround(e, dts, span = 1.2) {
  const rec = recFor(e, dts - span) ?? recFor(e, dts);
  if (!rec) return [];
  const ss = Math.max(0, dts - span - rec.start);
  const r = spawnSync('ffmpeg', ['-hide_banner', '-v', 'info', '-copyts', '-ss', ss.toFixed(3), '-t', (2 * span).toFixed(3), '-i', rec.file, '-an', '-vf', 'showinfo', '-f', 'null', '-'],
    { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, timeout: 60_000 });
  // the slate's checksums drift from frame to frame after encoding, and its chroma settles over a
  // few frames after a cut; its luma mean and stdev hold from the first frame
  return [...(r.stderr ?? '').matchAll(/pts_time:([0-9.]+).*?mean:\[(\d+).*?stdev:\[([0-9.]+)/g)].map((m) => ({ t: Number(m[1]), y: `${m[2]}/${m[3]}` }));
}

const corr = Array(epochCount).fill(0);
const calibration = [];
for (let e = 0; e < epochCount; e += 1) {
  const table = tables[e];
  const errors = [];
  for (let k = 1; k < table.length && errors.length < 6; k += 1) {
    const [prev, cur] = [table[k - 1], table[k]];
    const toSlate = cur.source === 'slate' && prev.source !== 'slate';
    const fromSlate = cur.source !== 'slate' && prev.source === 'slate';
    if (!toSlate && !fromSlate) continue;
    // the slate side (back-to-back slate seams count as one) must last long enough to learn from
    let j = k - 1;
    if (fromSlate) { while (j > 0 && table[j - 1].source === 'slate') j -= 1; if (cur.offset - table[j].offset < 0.6) continue; }
    if (toSlate) { j = k; while (j < table.length - 1 && table[j + 1].source === 'slate') j += 1; if (table[j + 1] && table[j + 1].offset - cur.offset < 0.6) continue; }
    const predicted = cur.offset - BASE;
    const actual = slateEdge(framesAround(e, predicted), predicted, toSlate);
    if (actual != null) errors.push({ offset: cur.offset, toSlate, errorSec: r3(actual - predicted) });
  }
  if (errors.length) corr[e] = median(errors.map((x) => x.errorSec));
  calibration.push({ epoch: e, edges: errors, appliedSec: r3(corr[e]) });
}
print('calibration', calibration);

const dtsOf = (e, offset) => offset - BASE + corr[e];
const reach = (e, offset) => (e >= 0 ? readerReach(reader[e], dtsOf(e, offset)) : null);
const reachMs = (e, offset, fromMs) => { const r = reach(e, offset); return r ? r.wallMs - fromMs : null; };
const lastSeen = (e) => {
  const last = reader[e]?.[reader[e].length - 1];
  if (!last) return null;
  const v = viewAt(tables[e], last.dts + BASE - corr[e]);
  return v ? { wallMs: last.wallMs, dts: last.dts, index: v.seam.index, name: v.seam.name, source: v.seam.source, pos: r3(v.pos) } : null;
};
const rise = (e, offset) => (e >= 0 ? r0(riseAt(points[e], offset - BASE)) : null);

// clips, cut by dts from the -copyts recording
const clipsDir = path('clips');
const clips = [];
function clip(e, dts, label) {
  const rec = recFor(e, dts);
  if (!rec) return null;
  mkdirSync(clipsDir, { recursive: true });
  const file = `${clipsDir}/${label}.mp4`;
  const r = spawnSync('ffmpeg', ['-v', 'error', '-y', '-ss', Math.max(0, dts - 5 - rec.start).toFixed(3), '-i', rec.file, '-t', '10', '-c', 'copy', file], { timeout: 60_000 });
  if (r.status !== 0) return null;
  clips.push(file);
  return file;
}

// --- criterion 2: cold start and the URL-to-file handover -------------------------------------
const firstStart = processStarts[0];
const firstVideo = tables[0]?.find((s) => s.source !== 'slate');
if (firstVideo && firstStart) {
  const ms = reachMs(0, firstVideo.offset, firstStart);
  print('coldStart', { firstVideo: firstVideo.name, source: firstVideo.source, offset: firstVideo.offset, readerMs: ms, pass: ms != null && ms < 20_000 });
} else print('coldStart', 'n/a (no non-slate seam)');

const handovers = [];
for (let e = 0; e < epochCount; e += 1) {
  tables[e].forEach((s, k) => {
    if (s.reason !== 'handover' || k === 0) return;
    const prev = tables[e][k - 1];
    const dts = dtsOf(e, s.offset);
    const near = watchNear(e, dts - 1, dts + 5);
    handovers.push({
      epoch: e, name: s.name, offset: s.offset, startPos: s.startPos, from: prev.source,
      // 0 when the file starts exactly where the URL feeder's last frame ended on the timeline
      continuityMs: r0(((s.offset - prev.offset) - (s.startPos - prev.startPos)) * 1000),
      backwards: near.filter((w) => w.type === 'backwards').length, jumps: near.filter((w) => w.type === 'jump').length,
      lagRiseMs: rise(e, s.offset), clip: clip(e, dts, `handover-${e}-${s.name}-${s.offset.toFixed(1)}`),
    });
  });
}
print('handovers', handovers);

// --- criterion 3: controls --------------------------------------------------------------------
const controlResults = controls.map((c, k) => {
  const e = ep(c.wallMs);
  const drove = driveRows.filter((d) => d.cmd === c.cmd && Math.abs((d.payload.replyMs ?? d.wallMs) - c.wallMs) < 5000).at(-1);
  const replyMs = drove?.payload?.replyMs ?? c.wallMs;
  const cutDts = dtsOf(e, c.cutOffset);
  const visible = reach(e, c.cutOffset);
  const nextControl = controls[k + 1]?.wallMs ?? Infinity;
  const target = tables[e]?.find((s) => s.offset >= c.cutOffset - 0.001 && s.source !== 'slate' && s.index === c.targetIndex && s.wallMs >= c.wallMs - 50 && s.wallMs < nextControl);
  const onAir = target ? reach(e, target.offset) : null;
  // what the command had to wait for: a URL feeder started for the target (it may have been
  // handed over to the just-finished file at the same offset before a frame of it aired)
  const viaUrl = target && seams.some((s) => s.epoch === e && s.index === c.targetIndex && s.source === 'url' && s.wallMs >= c.wallMs - 50 && s.wallMs <= target.wallMs);
  const near = watchNear(e, cutDts - 1, (target ? dtsOf(e, target.offset) : cutDts) + 5);
  const label = `control-${k + 1}-${c.cmd}-${c.targetIndex}`;
  return {
    n: k + 1, cmd: c.cmd, at: iso(c.wallMs), targetIndex: c.targetIndex, targetId: c.targetId, targetSource: target ? (viaUrl ? 'url' : target.source) : null, airedFrom: target?.source ?? null,
    visibleMs: visible ? visible.wallMs - replyMs : null,
    onAirMs: onAir ? onAir.wallMs - replyMs : null,
    statusNamedTargetMs: drove?.payload?.statusMatchMs ?? null,
    backwards: near.filter((w) => w.type === 'backwards').length, jumps: near.filter((w) => w.type === 'jump').length,
    lagRiseCutMs: rise(e, c.cutOffset), lagRiseTargetMs: target && target.offset !== c.cutOffset ? rise(e, target.offset) : null,
    encoderSameEpoch: ep((onAir ?? visible)?.wallMs ?? c.wallMs) === e,
    clipCut: clip(e, cutDts, label),
    clipTarget: target && target.offset - c.cutOffset > 1 ? clip(e, dtsOf(e, target.offset), `${label}-onair`) : null,
  };
});
print('controls', controlResults);
if (controlResults.length) {
  const visible = controlResults.map((c) => c.visibleMs);
  print('controlVisible', { ...stats(visible), pass: visible.every((v) => v != null && v < 5000) });
  print('controlOnAirFile', stats(controlResults.filter((c) => c.targetSource === 'file').map((c) => c.onAirMs)));
  print('controlOnAirUrl', stats(controlResults.filter((c) => c.targetSource === 'url').map((c) => c.onAirMs)));
  print('controlChecks', {
    statusAllNamedTarget: controlResults.every((c) => c.statusNamedTargetMs != null),
    backwards: controlResults.reduce((a, c) => a + c.backwards, 0), jumps: controlResults.reduce((a, c) => a + c.jumps, 0),
    worstLagRiseMs: Math.max(...controlResults.flatMap((c) => [c.lagRiseCutMs, c.lagRiseTargetMs]).filter((v) => v != null)),
    encoderNeverRestarted: controlResults.every((c) => c.encoderSameEpoch),
  });
}

// --- criteria 5-7: faults ---------------------------------------------------------------------
const FAULTS = ['break-url', 'kill-feeder', 'stall-feeder', 'kill-encoder', 'kill-token-server'];
const firstReaderAfter = (wallMs) => readerAll.find((r) => r.wallMs >= wallMs && segmentEpoch.get(r.segment) > ep(wallMs - 1)) ?? null;

/** After an encoder loss at epoch e: the new epoch's first reader packet and the resume check. */
function encoderResume(e, fromMs) {
  const seen = lastSeen(e);
  // failed restarts while the ingest is down are epochs too, with nothing at the reader
  const k = reader.findIndex((rows, i) => i > e && rows.length > 0);
  const next = k >= 0 ? k : null;
  const resumed = next != null ? tables[next].find((s) => s.source !== 'slate') : null;
  const exit = events.find((x) => x.kind === 'encoder-exit' && ep(x.wallMs) === e);
  const back = next != null ? reader[next][0] : null;
  return {
    backAtReaderMs: back ? back.wallMs - fromMs : null,
    lastSeen: seen,
    supervisorResume: exit ? { index: exit.resumeIndex, pos: r3(exit.resumePos) } : null,
    resumedSeam: resumed ? { index: resumed.index, name: resumed.name, source: resumed.source, startPos: resumed.startPos } : null,
    resumeErrorVsViewerSec: seen && resumed && seen.index === resumed.index ? r3(resumeErrorSec(seen.pos, resumed.startPos)) : null,
    sameVideo: seen && resumed ? seen.index === resumed.index : null,
    resumedOnAirMs: resumed ? reachMs(next, resumed.offset, fromMs) : null,
  };
}

const faultResults = driveRows.filter((d) => FAULTS.includes(d.cmd)).map((d) => {
  const at = d.payload.replyMs ?? d.wallMs;
  const e = ep(at);
  const index = d.payload.before?.index;
  const r = { kind: d.cmd, count: d.payload.count ?? null, at: iso(at), index, reply: d.payload.reply };
  if (d.cmd === 'kill-feeder' || d.cmd === 'stall-feeder') {
    const retry = firstEventAfter(events, at, ['retry', 'skip']);
    r.detection = retry ? { kind: retry.kind, why: retry.why, ms: retry.wallMs - at } : null;
    const after = tables[e].filter((s) => s.wallMs > at && s.index === index);
    const back = after.find((s) => s.source !== 'slate');
    const k = back ? tables[e].indexOf(back) : -1;
    const prev = k > 0 ? tables[e][k - 1] : null;
    r.retriesUntilBack = events.filter((x) => x.kind === 'retry' && x.wallMs >= at && (!back || x.wallMs <= back.wallMs)).map((x) => ({ why: x.why, ms: x.wallMs - at, pos: r3(x.pos) }));
    r.back = back ? { source: back.source, startPos: back.startPos, feederStartMs: back.wallMs - at, onAirMs: reachMs(e, back.offset, at),
      slateAired: tables[e].some((s) => s.source === 'slate' && s.wallMs > at && s.wallMs < back.wallMs),
      // in place at P_fed: the retried feeder picks up exactly where the last aired frame of the old one ended
      continuityMs: prev && prev.source !== 'slate' && prev.index === index ? r0(((back.offset - prev.offset) - (back.startPos - prev.startPos)) * 1000) : null,
      lagRiseMs: rise(e, back.offset), clip: clip(e, dtsOf(e, back.offset), `retry-${d.cmd}-${back.name}-${back.offset.toFixed(1)}`) } : null;
    const dts = back ? dtsOf(e, back.offset) : null;
    r.nearby = dts != null ? watchNear(e, dts - 15, dts + 5) : null;
  } else if (d.cmd === 'break-url' && d.payload.count === 'always') {
    const skip = firstEventAfter(events, at, ['skip']);
    // a broken URL only bites at the next resolve (a refresh or a retry), not at the fault itself
    // (before the fault events carried a 'fault' field, their payload overwrote 'kind')
    const applied = events.find((x) => x.wallMs >= at && (x.kind === 'fault-applied' || (x.kind === 'break-url' && 'index' in x)));
    r.firstBrokenResolveMs = applied ? applied.wallMs - at : null;
    r.retriesBeforeSkip = events.filter((x) => x.kind === 'retry' && x.wallMs >= at && (!skip || x.wallMs <= skip.wallMs)).map((x) => ({ why: x.why, ms: x.wallMs - at }));
    r.skip = skip ? { ms: skip.wallMs - at, index: skip.index, why: skip.why } : null;
    const nextVideo = skip ? tables[ep(skip.wallMs)].find((s) => s.wallMs > skip.wallMs && s.source !== 'slate') : null;
    r.nextVideo = nextVideo ? { index: nextVideo.index, name: nextVideo.name, source: nextVideo.source, onAirMs: reachMs(ep(skip.wallMs), nextVideo.offset, at),
      clip: clip(ep(skip.wallMs), dtsOf(ep(skip.wallMs), nextVideo.offset), `skip-next-${nextVideo.name}`) } : null;
  } else if (d.cmd === 'kill-encoder') {
    const exit = firstEventAfter(events, at, ['encoder-exit']);
    r.detectionMs = exit ? exit.wallMs - at : null;
    r.resume = encoderResume(e, at);
  } else if (d.cmd === 'kill-token-server') {
    const down = firstEventAfter(events, at, ['token-server-down']);
    const spawn = firstEventAfter(events, at, ['token-server-spawn']);
    r.downMs = down ? down.wallMs - at : null;
    r.ownedUpMs = spawn ? spawn.wallMs - at : null;
    r.pass = spawn ? spawn.wallMs - at < 35_000 : false;
    const pot = lines('ytdlp.log').map((l) => ({ wallMs: Date.parse(l.slice(0, 24)), l })).find((x) => spawn && x.wallMs >= spawn.wallMs && x.l.includes('[pot:bgutil:http]'));
    r.potLineAfterMs = pot ? pot.wallMs - at : null;
  }
  return r;
});
print('faults', faultResults);

// URL refreshes (URL-to-URL handover at P_fed)
const refreshes = [];
for (let e = 0; e < epochCount; e += 1) {
  tables[e].forEach((s, k) => {
    if (s.reason !== 'refresh' || k === 0) return;
    const prev = tables[e][k - 1];
    const dts = dtsOf(e, s.offset);
    const near = watchNear(e, dts - 1, dts + 5);
    const asked = events.filter((x) => x.kind === 'url-refresh' && x.wallMs <= s.wallMs).at(-1);
    refreshes.push({ epoch: e, name: s.name, offset: s.offset, startPos: s.startPos, askedToHandoverMs: asked ? s.wallMs - asked.wallMs : null,
      continuityMs: r0(((s.offset - prev.offset) - (s.startPos - prev.startPos)) * 1000),
      backwards: near.filter((w) => w.type === 'backwards').length, jumps: near.filter((w) => w.type === 'jump').length,
      lagRiseMs: rise(e, s.offset), clip: clip(e, dts, `refresh-${s.name}-${s.offset.toFixed(1)}`) });
  });
}
if (refreshes.length || events.some((x) => x.kind === 'url-refresh')) print('refreshes', refreshes);

if (mediamtxEvents.length) {
  const down = mediamtxEvents.find((x) => x.action === 'down');
  const up = mediamtxEvents.find((x) => x.action === 'up');
  const back = up ? firstReaderAfter(up.wallMs) : null;
  const e = down ? ep(down.wallMs) : -1;
  print('mediamtx', { downMs: down && up ? up.wallMs - down.wallMs : null, republishMs: back ? back.wallMs - up.wallMs : null,
    pass: back ? back.wallMs - up.wallMs < 15_000 : false, resume: e >= 0 ? encoderResume(e, up.wallMs) : null });
}

// --- criterion 4: soak ------------------------------------------------------------------------
const disconnects = watch.filter((w) => w.type === 'disconnect' && w.wallMs > firstPacketMs && w.wallMs < endedAt - 5000).map((w) => iso(w.wallMs));
const tsEvents = watch.filter((w) => w.type === 'backwards' || w.type === 'jump').map((w) => ({ at: iso(w.wallMs), epoch: ep(w.wallMs), type: w.type, stream: w.stream, dts: w.at, value: w.value }));
const seamRises = [];
const spikes = [];
const speeds = [];
for (let e = 0; e < epochCount; e += 1) {
  const seamTimes = tables[e].map((s) => s.offset - BASE);
  for (const s of tables[e]) { const v = riseAt(points[e], s.offset - BASE); if (v != null && s.offset - BASE >= 90) seamRises.push({ epoch: e, name: s.name, reason: s.reason, t: r3(s.offset - BASE), riseMs: r0(v) }); }
  for (const sp of lagSpikes(points[e], seamTimes)) {
    const row = points[e].find((p) => p.t >= sp.t);
    const v = viewAt(tables[e], sp.t + BASE);
    spikes.push({ epoch: e, mediaT: sp.t, seconds: sp.lastT - sp.t + 1, riseMs: r0(sp.riseMs), at: row ? iso(row.wallMs) : null, video: v?.seam.name, pos: v ? r3(v.pos) : null });
  }
  const p = points[e];
  if (p.length > 1) speeds.push({ epoch: e, speed: r3(((p.at(-1).t - p[0].t) * 1000) / (p.at(-1).wallMs - p[0].wallMs)) });
}
const budget = run.budgetBytes ?? null;
const peakCache = Math.max(run.maxCacheBytes ?? 0, ...bufferRows.map((r) => r.cacheBytes));
const largest = Math.max(0, ...downloads.map((d) => d.bytes || 0));
const deleteResults = deletes.map((row) => {
  const e = ep(Date.parse(row.iso));
  const v = e >= 0 ? viewAt(tables[e], Number(row.pView)) : null;
  return { ...row, onAir: v?.seam.name ?? null, ok: deletionOk(row, v?.seam.name) };
});
print('soak', {
  encoderExits: events.filter((x) => x.kind === 'encoder-exit').length,
  readerDisconnects: disconnects,
  backwards: tsEvents.filter((x) => x.type === 'backwards').length,
  jumps: tsEvents.filter((x) => x.type === 'jump').length,
  seamRise: { n: seamRises.length, medianMs: seamRises.length ? r0(median(seamRises.map((s) => s.riseMs))) : null, worst: seamRises.sort((a, b) => b.riseMs - a.riseMs).slice(0, 3), over500: seamRises.filter((s) => s.riseMs >= 500).length },
  speeds,
  peakCacheBytes: peakCache, budgetBytes: budget, largestVideoBytes: largest, cachePass: budget != null ? peakCache <= budget + largest : null,
  deletes: deleteResults.length, badDeletes: deleteResults.filter((d) => !d.ok),
});
if (tsEvents.length) print('timestampEvents', tsEvents.slice(0, 50));
print('lagSpikesOffSeam', spikes);

// the 7077df3 bug: a slate feeder restarted at the same offset, every ~200 ms
const slateLoops = seams.filter((x, k) => x.source === 'slate' && k > 0 && seams[k - 1].source === 'slate' && seams[k - 1].offset === x.offset && seams[k - 1].epoch === x.epoch);
print('slateRestartLoops', slateLoops.map((x) => ({ at: x.startedIso, offset: x.offset })));

// YouTube breaker: open, quiet, probe, close
const spawns = lines('ytdlp.log').filter((l) => l.includes('[spawn]')).map((l) => Date.parse(l.slice(0, 24)));
const breakerEvents = events.filter((x) => x.kind.startsWith('youtube-') || x.kind === 'breaker-retarget');
if (breakerEvents.length) {
  const opened = breakerEvents.filter((x) => x.kind === 'youtube-breaker-open');
  print('breaker', opened.map((o) => {
    const close = events.find((x) => x.kind === 'youtube-breaker-closed' && x.wallMs > o.wallMs);
    const end = close?.wallMs ?? endedAt;
    const probes = events.filter((x) => x.kind === 'youtube-probe' && x.wallMs > o.wallMs && x.wallMs <= end);
    return { openedAt: o.ts, reason: o.reason, retryAt: o.retryAt, closedAfterMs: close ? close.wallMs - o.wallMs : null, probes: probes.map((p) => ({ at: p.ts, level: p.level })),
      ytdlpSpawnsWhileOpen: spawns.filter((t) => t > o.wallMs && t <= end).length, probeFailures: events.filter((x) => x.kind === 'youtube-probe-failed' && x.wallMs > o.wallMs && x.wallMs <= end).length };
  }));
}
print('ytdlpSpawns', spawns.length);

// --- criterion 8: crash and resume (run C) ---------------------------------------------------
const resumeInfo = readJson('resume-after.json');
if (resumeInfo) {
  const before = resumeInfo.before;
  const relaunch = processStarts[1];
  const e = relaunch ? ep(relaunch + 1) : -1;
  const epochAfter = epochStarts.findIndex((t) => t >= relaunch);
  const seam = epochAfter >= 0 ? tables[epochAfter].find((s) => s.source !== 'slate') : null;
  const leftover = readJson('resume-leftover-processes.json');
  const list = leftover == null ? [] : Array.isArray(leftover) ? leftover : [leftover];
  print('resume', {
    saved: before, resumedSeam: seam ? { index: seam.index, name: seam.name, source: seam.source, startPos: seam.startPos, reason: seam.reason } : null,
    sameVideo: seam ? seam.name === before.id : null, errorSec: seam ? r3(resumeErrorSec(before.position, seam.startPos)) : null,
    viewerLastSeen: e >= 0 ? lastSeen(e) : null,
    fromUrl: seam ? seam.source === 'url' : null,
    onAirAfterRelaunchMs: seam && relaunch ? reachMs(epochAfter, seam.offset, relaunch) : null,
    leaksAfter3s: leaks(list).map((p) => ({ pid: p.ProcessId, name: p.Name, cmd: (p.CommandLine ?? '').slice(0, 160) })),
    statusAfter5s: resumeInfo.after?.playing ?? null, epochBeforeRelaunch: e,
  });
}

// --- criterion 9 and leaks --------------------------------------------------------------------
for (const r of driveRestarts) {
  const e = ep(r.wallMs);
  const near = watch.filter((w) => Math.abs(w.wallMs - r.wallMs) < 15_000);
  print(`driveRestart-${r.action}`, { at: iso(r.wallMs), watcherEventsWithin15s: near.map((w) => ({ type: w.type, at: iso(w.wallMs) })), encoderSameEpoch: ep(r.wallMs + 15_000) === e });
}
const leftover = readJson('leftover-processes.json');
if (leftover != null) {
  const list = Array.isArray(leftover) ? leftover : [leftover];
  print('leaksAfterRun', leaks(list).map((p) => ({ pid: p.ProcessId, name: p.Name, cmd: (p.CommandLine ?? '').slice(0, 160) })));
}

const kinds = {};
for (const x of events) kinds[x.kind] = (kinds[x.kind] ?? 0) + 1;
print('eventCounts', kinds);
print('clips', clips);
