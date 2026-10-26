// Turns a copy of the box's supervisor logs into the stage 5 numbers (read-only).
//   node trial-report.mjs <supervisorLogsDir> [--from ISO] [--to ISO]
// Off air is counted from the supervisor's own encoder events, never from Node's, so a Node
// restart inside the window changes nothing.
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { dailyFiles, parseCsv, cpuRows, median } from './lib.mjs';

const ENCODER_COLUMNS = ['wallMs', 'us', 'speed', 'generation', 'speedNum', 'encoderCpu', 'feederCpu', 'systemCpu'];
const DOWN = new Set(['encoder-exit', 'encoder-spawn-failed', 'shutdown']);
const r3 = (v) => Math.round(v * 1000) / 1000;
const iso = (ms) => new Date(ms).toISOString();

function spread(values) {
  const v = values.filter(Number.isFinite);
  return v.length ? { median: r3(median(v)), min: r3(Math.min(...v)), max: r3(Math.max(...v)) } : null;
}

/** `files` holds whole daily files, oldest first; `from`/`to` are epoch ms. */
export function trialReport(files, from, to) {
  const all = files.events
    .flatMap((text) => text.split(/\r?\n/).filter(Boolean).map((l) => JSON.parse(l)))
    .map((e) => ({ ...e, ms: Date.parse(e.ts) }))
    .sort((a, b) => a.ms - b.ms);

  const encoderRows = files.encoderCsv.flatMap((text) => parseCsv(text, ENCODER_COLUMNS));
  // Off air: an exit, a failed spawn or a shutdown, until the next encoder-start. A supervisor
  // start with no shutdown before it is a relaunch after a crash, which writes no event of its
  // own: it began at the encoder's last progress row. The run's first start is not an outage.
  const progressMs = encoderRows.map((r) => Number(r.wallMs)).filter(Number.isFinite).sort((a, b) => a - b);
  const lastProgressBefore = (ms) => {
    let last = null;
    for (const t of progressMs) {
      if (t >= ms) break;
      last = t;
    }
    return last;
  };
  const gaps = [];
  let down = null;
  let encoderEverStarted = false;
  for (const e of all) {
    if (e.kind === 'encoder-start') {
      if (down) gaps.push({ ...down, to: e.ms });
      down = null;
      encoderEverStarted = true;
    } else if (!down && DOWN.has(e.kind)) {
      down = { from: e.ms, why: e.kind };
    } else if (!down && e.kind === 'start' && encoderEverStarted) {
      down = { from: lastProgressBefore(e.ms) ?? e.ms, why: 'crash' };
    }
  }
  if (down) gaps.push({ ...down, to });
  const clipped = gaps
    .map((g) => ({ from: Math.max(g.from, from), to: Math.min(g.to, to), why: g.why }))
    .filter((g) => g.to > g.from)
    .map((g) => ({ from: iso(g.from), to: iso(g.to), seconds: r3((g.to - g.from) / 1000), why: g.why }));

  const inside = all.filter((e) => e.ms >= from && e.ms <= to);
  const counts = {};
  for (const e of inside) counts[e.kind] = (counts[e.kind] ?? 0) + 1;
  const n = (kind) => counts[kind] ?? 0;

  const watchdog = { freeze: 0, silence: 0, content: 0 };
  for (const e of inside) {
    if (e.kind !== 'watchdog-freeze' && e.kind !== 'watchdog-silence') continue;
    if (e.outcome === 'content') watchdog.content++;
    else if (e.outcome === 'retry' || e.outcome === 'skip') watchdog[e.kind === 'watchdog-freeze' ? 'freeze' : 'silence']++;
  }

  // An encoder-start right after a supervisor start is that start's first encoder, not a restart.
  let encoderRestarts = 0;
  let afterStart = false;
  for (const e of inside) {
    if (e.kind === 'start') afterStart = true;
    else if (e.kind === 'encoder-start') {
      if (!afterStart) encoderRestarts++;
      afterStart = false;
    }
  }

  const cpu = cpuRows(encoderRows)
    .filter((c) => c.wallMs >= from && c.wallMs <= to);

  return {
    window: { from: iso(from), to: iso(to), hours: r3((to - from) / 3_600_000) },
    offAir: { seconds: r3(clipped.reduce((s, g) => s + g.seconds, 0)), gaps: clipped },
    counts,
    criticals: inside.filter((e) => e.level === 'critical').map((e) => ({ ts: e.ts, kind: e.kind })),
    watchdog,
    skips: n('skip'),
    retries: n('retry'),
    breaker: { opens: n('youtube-breaker-open') },
    encoder: {
      samples: cpu.length,
      speed: spread(cpu.map((c) => c.speed)),
      encoderCpu: spread(cpu.map((c) => c.encoderCpu)),
      feederCpu: spread(cpu.map((c) => c.feederCpu)),
      systemCpu: spread(cpu.map((c) => c.systemCpu)),
    },
    restarts: { supervisorStarts: n('start'), encoder: encoderRestarts },
  };
}

function main() {
  const [dir, ...rest] = process.argv.slice(2);
  if (!dir) {
    console.error('usage: node trial-report.mjs <supervisorLogsDir> [--from ISO] [--to ISO]');
    process.exit(2);
  }
  const flag = (name) => {
    const i = rest.indexOf(name);
    return i >= 0 ? Date.parse(rest[i + 1]) : null;
  };
  const names = readdirSync(dir);
  const read = (base, ext) => dailyFiles(names, base, ext).map((f) => readFileSync(join(dir, f), 'utf8'));
  const files = { events: read('events', 'jsonl'), encoderCsv: read('encoder', 'csv') };
  const times = files.events.flatMap((t) => t.split(/\r?\n/).filter(Boolean).map((l) => Date.parse(JSON.parse(l).ts)));
  const from = flag('--from') ?? Math.min(...times);
  const to = flag('--to') ?? Math.max(...times);
  const report = trialReport(files, from, to);
  writeFileSync(join(dir, 'trial-report.json'), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) main();
