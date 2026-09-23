// Pure helpers for the supervisor spike's session/drive/analyze scripts.
// Media plumbing (feederArgs, encoderArgs, lagStats, seamLag, verdict, LOCAL_URL) stays in
// ../../continuity/lib.mjs; import from there instead of duplicating it.

export const API = 'http://127.0.0.1:4417';
export const TOKEN_SERVER_PING = 'http://127.0.0.1:4416/ping';

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

/** Calibrates the constant timeline-offset -> reader-dts mapping from one seam, then checks it
 *  against a second seam of the same encoder generation. readerDtsAt(wallMs) looks up the first
 *  reader packet dts at/after that wall time (or null). Positive errorSec means the later seam's
 *  actual dts arrived later than the constant predicted. */
export function calibrate(seamA, seamB, readerDtsAt) {
  const dtsA = readerDtsAt(seamA.wallMs);
  if (dtsA == null) return null;
  const constant = dtsA - seamA.offset;
  const predicted = seamB.offset + constant;
  const actual = readerDtsAt(seamB.wallMs);
  return actual == null ? { constant, predicted, actual: null, errorSec: null } : { constant, predicted, actual, errorSec: actual - predicted };
}

/** Latency of a control cut: reader wall time of the first packet at/after the mapped cut dts,
 *  minus the command's own wall time. readerRows: [{wallMs, dts}], ascending wallMs. */
export function controlLatency(commandWallMs, mappedCutDts, readerRows) {
  const hit = readerRows.find((r) => r.wallMs >= commandWallMs && r.dts >= mappedCutDts);
  return hit ? hit.wallMs - commandWallMs : null;
}

export function resumeErrorSec(pView, startPos) {
  return Math.abs(Number(pView) - Number(startPos));
}

/** A deleted file must either have been fully watched (pView caught up to its end) or have been
 *  left behind by a switch. deletes.csv carries both fields; a switch deletion has no fileEndTimeline. */
export function deletionOk(row) {
  if (row.fileEndTimeline === '' || row.fileEndTimeline == null) return true;
  return Number(row.pView) >= Number(row.fileEndTimeline);
}

/** First events.jsonl entry after a fault's wall time whose type matches one of `types`. */
export function firstEventAfter(events, wallMs, types) {
  return events.find((e) => e.wallMs >= wallMs && types.includes(e.type)) ?? null;
}

export async function apiGet(path) {
  const res = await fetch(`${API}${path}`);
  return res.json();
}

export async function apiPost(path, body) {
  const res = await fetch(`${API}${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body ?? {}) });
  return res.json();
}
