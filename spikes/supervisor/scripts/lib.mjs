// Pure helpers for the supervisor spike's session/drive/analyze scripts.
// Media plumbing (feederArgs, encoderArgs, lagStats, seamLag, verdict, LOCAL_URL) stays in
// ../../continuity/lib.mjs; import from there instead of duplicating it.

export const API = 'http://127.0.0.1:4417';
export const TOKEN_SERVER_PING = 'http://127.0.0.1:4416/ping';
/** The supervisor's timeline starts every encoder generation at BASE (rules.rs); the encoder
 *  shifts its input start to 0, so reader dts = timeline offset - BASE (+ a per-run error that
 *  analyze measures on the recording). */
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

/** Epoch = one encoder generation of one supervisor process. Generation numbers restart at 0
 *  when the supervisor is relaunched (run C), so epochs are counted from encoder-start times.
 *  Returns the index of the last start at or before wallMs (-1 before the first). */
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

/** Where a slate edge really is in the recording. frames: [{t, y}] (y = a frame signature, e.g.
 *  showinfo's mean and stdev) around the predicted dts. The slate is a still, so its frames repeat
 *  a small set of signatures; learn them on the slate side (>= 0.3 s away from the prediction) and return the first frame of
 *  the other side. toSlate: true when the stream cuts from video to the slate. */
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

/** Processes the supervisor must not leave behind: any ffmpeg on a pipe (feeders use pipe:1,
 *  the encoder pipe:0; the session's recorder and ffprobe readers do not), any yt-dlp, and a token
 *  server the supervisor spawned (it passes -p 4416; the external one is started without it). */
export function leaks(processes) {
  return processes.filter((p) => {
    const name = (p.Name ?? '').toLowerCase();
    const cmd = p.CommandLine ?? '';
    if (name === 'ffmpeg.exe') return cmd.includes('pipe:');
    if (name === 'yt-dlp.exe') return true;
    if (name === 'node.exe') return /main\.js/.test(cmd) && /-p\s+4416/.test(cmd);
    return false;
  });
}

export async function apiGet(path) {
  const res = await fetch(`${API}${path}`);
  return res.json();
}

export async function apiPost(path, body) {
  const res = await fetch(`${API}${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body ?? {}) });
  return res.json();
}
