// Counts the supervisor's download tries, and which of them used a PO token, from a copy of its logs (read-only).
//   node token-report.mjs <logsDir> [--from ISO] [--to ISO]
import { readFileSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { dailyFiles, EVENTS } from './lib.mjs';

const TOKEN_LEAD_MS = 5000;
const EVENT_FIELDS = {
  'youtube-breaker-open': 'breakerOpens',
  'token-caches-cleared': 'cacheClears',
  'token-server-down': 'serverDown',
  'token-server-restart': 'serverRestart',
  'token-server-spawn-failed': 'spawnFailed',
  'downloads-failing': 'downloadsFailing',
};

const lines = (text) => text.split(/\r?\n/).filter(Boolean);
const isRefused = (row) => row.reason.includes('HTTP Error 403');

/** Fields 0 to 5 are ts, id, result, attempt, bytes, ms; trailing lower-case words are the client list,
 *  the fields between are the reason (commas kept). Rows before 3.0.5 have seven fields and no client. */
function parseRow(line) {
  const f = line.split(',');
  let end = f.length;
  while (end > 7 && /^[a-z_]+$/.test(f[end - 1])) end--;
  const client = f.slice(end).join(',');
  return {
    ts: f[0], id: f[1], result: f[2], attempt: Number(f[3]), ms: Number(f[5]),
    reason: f.slice(6, end).join(','), client,
  };
}

/** `files` holds whole daily files, oldest first; rows and events count from `fromMs` (inclusive) to `toMs` (exclusive). */
export function tokenReport(files, fromMs = -Infinity, toMs = Infinity) {
  const inWindow = (ts) => {
    const t = Date.parse(ts);
    return t >= fromMs && t < toMs;
  };
  const potAt = files.ytdlpLog.flatMap(lines).filter((l) => l.includes('[pot')).map((l) => Date.parse(l.slice(0, 24)));

  // A try is a download attempt that ran. The "skip" row of a failed size lookup runs nothing (ms 0).
  const rows = files.downloadsCsv.flatMap(lines).map(parseRow)
    .filter((r) => inWindow(r.ts) && !(r.result === 'skip' && r.ms === 0))
    .map((r) => {
      const end = Date.parse(r.ts);
      return { ...r, token: potAt.some((t) => t >= end - r.ms - TOKEN_LEAD_MS && t <= end) };
    });

  const out = {
    tries: rows.length, tokenTries: 0, done: 0, refused403: 0, otherFailed: 0, doneShare: null,
    videosRefusedTwice: 0, breakerOpens: 0, cacheClears: 0, serverDown: 0, serverRestart: 0,
    spawnFailed: 0, downloadsFailing: 0, rows,
  };
  let tokenDone = 0;
  for (const r of rows) {
    if (r.token) out.tokenTries++;
    if (r.result === 'done') {
      out.done++;
      if (r.token) tokenDone++;
    } else if (isRefused(r)) out.refused403++;
    else out.otherFailed++;
  }
  out.doneShare = out.tokenTries ? tokenDone / out.tokenTries : null;

  const byId = new Map();
  for (const r of rows) byId.set(r.id, [...(byId.get(r.id) ?? []), r]);
  for (const tries of byId.values()) {
    if (tries.some((r, i) => i > 0 && isRefused(r) && isRefused(tries[i - 1]))) out.videosRefusedTwice++;
  }

  for (const text of files.events) {
    for (const l of lines(text)) {
      const e = JSON.parse(l);
      if (EVENT_FIELDS[e.kind] && inWindow(e.ts)) out[EVENT_FIELDS[e.kind]]++;
    }
  }
  return out;
}

/** Each video's first try on side A against side B; only videos both sides tried can split. */
export function splitPairs(rowsA, rowsB) {
  const firsts = (rows) => {
    const m = new Map();
    for (const r of rows) if (!m.has(r.id)) m.set(r.id, r);
    return m;
  };
  const a = firsts(rowsA);
  const b = firsts(rowsB);
  const outcome = (r) => (r.result === 'done' ? 'done' : isRefused(r) ? 'refused' : 'failed');
  const out = { aDoneBRefused: 0, bDoneARefused: 0, list: [] };
  for (const [id, ra] of a) {
    const rb = b.get(id);
    if (!rb) continue;
    const [oa, ob] = [outcome(ra), outcome(rb)];
    if (oa === 'done' && ob === 'refused') out.aDoneBRefused++;
    else if (oa === 'refused' && ob === 'done') out.bDoneARefused++;
    else continue;
    out.list.push({ id, a: oa, b: ob });
  }
  return out;
}

function main() {
  const [dir, ...rest] = process.argv.slice(2);
  if (!dir) {
    console.error('usage: node token-report.mjs <logsDir> [--from ISO] [--to ISO]');
    process.exit(2);
  }
  const flag = (name) => {
    const i = rest.indexOf(name);
    return i >= 0 ? Date.parse(rest[i + 1]) : undefined;
  };
  const names = readdirSync(dir);
  const read = (base, ext) => dailyFiles(names, base, ext).map((f) => readFileSync(join(dir, f), 'utf8'));
  const files = { downloadsCsv: read('downloads', 'csv'), ytdlpLog: read('ytdlp', 'log'), events: read(EVENTS, 'jsonl') };
  console.log(JSON.stringify(tokenReport(files, flag('--from'), flag('--to')), null, 2));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) main();
