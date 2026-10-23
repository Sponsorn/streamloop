// Pure helpers for the supervisor's session/drive/analyze scripts (adapted from spike 5's).

export const API = 'http://127.0.0.1:4417';
/** The real ffmpeg: the one on PATH is a Chocolatey shim, which a kill or suspend does not reach. */
export const FFMPEG_BIN = 'C:/ProgramData/chocolatey/lib/ffmpeg/tools/ffmpeg/bin';
export const FFMPEG = `${FFMPEG_BIN}/ffmpeg.exe`;
export const FFPROBE = `${FFMPEG_BIN}/ffprobe.exe`;
/** Spike 5's Blender Foundation playlist, and a one-video Blender playlist for the live switch. */
export const PLAYLIST = 'PL6B3937A5D230E335';
export const SECOND_PLAYLIST = 'PLa1F2ddGya_8NhFM-rx_xh5SlIGbHyrZZ';
export const TOKEN_SERVER_PING = 'http://127.0.0.1:4416/ping';
/** Every encoder generation starts its timeline at BASE (rules.rs) but its input at 0, so
 *  reader dts = timeline offset - BASE (+ a per-run error analyze measures on the recording). */
export const BASE = 10;

export function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** Splits a CSV line into exactly `columns.length` fields; anything past the last named column
 *  (a JSON reply, a free-text reason) is rejoined so an embedded comma doesn't shift the row. */
export function splitCsv(line, columns) {
  const parts = line.split(',');
  const head = parts.slice(0, columns.length - 1);
  const tail = parts.slice(columns.length - 1).join(',');
  return Object.fromEntries(columns.map((c, i) => [c, i < head.length ? head[i] : tail]));
}

export function parseCsv(text, columns) {
  return text.trim().length ? text.trim().split(/\r?\n/).map((line) => splitCsv(line, columns)) : [];
}

/** Epoch = one encoder generation of one supervisor process; generation numbers restart at 0 on
 *  relaunch (run C), so epochs are counted from encoder-start times instead. -1 before the first. */
export function epochAt(startsMs, wallMs) {
  let e = -1;
  for (let i = 0; i < startsMs.length && startsMs[i] <= wallMs; i += 1) e = i;
  return e;
}

/** The seam table as the viewer got it: a seam starting at offset O drops every earlier seam at
 *  or past O (rules.rs push_seam), which is what a flush does. Input in feeder start order. */
export function effectiveSeams(seamsInStartOrder) {
  const table = [];
  for (const s of seamsInStartOrder) {
    while (table.length && table[table.length - 1].offset >= s.offset) table.pop();
    table.push(s);
  }
  return table;
}

/** Inverse of the dts mapping: which seam and video position the viewer saw at timeline t. */
export function viewAt(table, t) {
  const s = [...table].reverse().find((x) => x.offset <= t);
  if (!s) return null;
  return { seam: s, pos: s.source === 'slate' ? s.startPos : s.startPos + (t - s.offset) };
}

/** First reader packet of one epoch whose dts reaches `dts`. rows ascending by wallMs. */
export function readerReach(rows, dts) {
  return rows.find((r) => r.dts >= dts) ?? null;
}

/** Encoder read lag rise at media time t (spike 1's seam rule): peak lag in [t, t+4) minus the
 *  median lag in [t-60, t-1). points: [{t, lag}] with t = encoder media seconds, lag in ms. */
export function riseAt(points, t, after = 4) {
  const baseline = points.filter((p) => p.t >= t - 60 && p.t < t - 1).map((p) => p.lag);
  const peak = points.filter((p) => p.t >= t && p.t < t + after).map((p) => p.lag);
  if (!baseline.length || !peak.length) return null;
  return peak.reduce((a, b) => Math.max(a, b), -Infinity) - median(baseline);
}

/** Lag rises over thresholdMs away from any seam (the watch list's unexplained mid-clip spikes).
 *  Adjacent seconds merge into one spike. Skips the first 90 s of start-up burst. */
export function lagSpikes(points, seamTimes, thresholdMs = 200) {
  if (!points.length) return [];
  const end = Math.floor(points[points.length - 1].t);
  const near = (t) => seamTimes.some((s) => t >= s - 2 && t <= s + 6);
  const spikes = [];
  for (let t = 90; t < end; t += 1) {
    if (near(t)) continue;
    const rise = riseAt(points, t, 1);
    if (rise == null || rise < thresholdMs) continue;
    const last = spikes[spikes.length - 1];
    if (last && t - last.lastT <= 1) { last.lastT = t; last.riseMs = Math.max(last.riseMs, rise); } else spikes.push({ t, lastT: t, riseMs: rise });
  }
  return spikes;
}

/** Where a slate edge really is in the recording. frames: [{t, y}] (y = a frame signature) around
 *  the predicted dts; the slate is a still, so its side's signatures are learned then the first frame off that set is returned. */
export function slateEdge(frames, predicted, toSlate) {
  const slateSide = toSlate ? frames.filter((f) => f.t >= predicted + 0.3) : frames.filter((f) => f.t <= predicted - 0.3);
  if (slateSide.length < 3) return null;
  const slate = new Set(slateSide.map((f) => f.y));
  if (slate.size > 3) return null; // not a still: nothing to learn from
  const scan = frames.filter((f) => f.t > predicted - 0.3 && f.t < predicted + 0.3);
  const hit = scan.find((f) => (toSlate ? slate.has(f.y) : !slate.has(f.y)));
  return hit ? hit.t : null;
}

export function resumeErrorSec(pView, startPos) {
  return Math.abs(Number(pView) - Number(startPos));
}

/** A deleted file must have been fully watched (pView at or past the end of its last airing),
 *  or never aired this generation (blank end) and not be on air at the moment of deletion. */
export function deletionOk(row, onAirId) {
  if (row.fileEndTimeline === '' || row.fileEndTimeline == null) return row.id !== onAirId;
  return Number(row.pView) >= Number(row.fileEndTimeline);
}

/** First events.jsonl entry at or after wallMs whose kind is one of `kinds`. */
export function firstEventAfter(events, wallMs, kinds) {
  return events.find((e) => e.wallMs >= wallMs && kinds.includes(e.kind)) ?? null;
}

/** Processes the supervisor must not leave behind: any ffmpeg on a pipe or started from the dev
 *  root (feeders/encoder; the session's recorder and readers aren't), any yt-dlp, and a token
 *  server it owns (-p 4416). */
export function leaks(processes, root = null) {
  const norm = (text) => text.toLowerCase().replaceAll('\\', '/');
  const fromRoot = (cmd) => root != null && norm(cmd).includes(norm(root));
  return processes.filter((p) => {
    const name = (p.Name ?? '').toLowerCase();
    const cmd = p.CommandLine ?? '';
    if (name === 'ffmpeg.exe') return cmd.includes('pipe:') || fromRoot(cmd);
    if (name === 'yt-dlp.exe') return true;
    if (name === 'node.exe') return /main\.js/.test(cmd) && /-p\s+4416/.test(cmd);
    return false;
  });
}

/** The supervisor's daily logs of one kind (`events-2026-10-23.jsonl`), oldest first. A run that
 *  crosses midnight has two. The `encoder-` prefix is shared with the per-process
 *  `encoder-YYYYMMDD-HHMMSS-gen.log` files, which the exact date pattern leaves out. */
export function dailyFiles(names, base, ext) {
  const re = new RegExp(`^${base}-\\d{4}-\\d{2}-\\d{2}\\.${ext}$`);
  return names.filter((n) => re.test(n)).sort();
}

export function redact(text, secret) {
  return secret ? text.split(secret).join('***') : text;
}

/** CPU sample rows of encoder-DATE.csv: every 10 s, column 5 onward filled. */
export function cpuRows(rows) {
  return rows.filter((r) => r.speedNum !== '' && r.speedNum != null)
    .map((r) => ({ wallMs: Number(r.wallMs), speed: Number(r.speedNum), encoderCpu: Number(r.encoderCpu), feederCpu: Number(r.feederCpu), systemCpu: Number(r.systemCpu) }));
}

/** Where a watchdog stretch began: the seam on air at encoder media time `since` (timeline =
 *  since + BASE) and how far into it. A verdict on the slate or within 5 s of a seam breaks the rule. */
export function watchdogPlace(table, since) {
  const v = viewAt(table, since + BASE);
  if (!v) return null;
  return { seam: v.seam, intoSeamSec: since + BASE - v.seam.offset, onSlate: v.seam.source === 'slate' };
}

/** Wall time at which the encoder's media clock first reached mediaT. points: [{t, wallMs}] ascending. */
export function wallAtMedia(points, mediaT) {
  return points.find((p) => p.t >= mediaT)?.wallMs ?? null;
}

export async function apiGet(path) {
  const res = await fetch(`${API}${path}`);
  return res.json();
}

export async function apiPost(path, body) {
  const res = await fetch(`${API}${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body ?? {}) });
  return res.json();
}
