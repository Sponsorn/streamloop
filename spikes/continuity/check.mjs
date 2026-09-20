import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, appendFileSync, readFileSync, existsSync, readdirSync, statSync } from 'node:fs';
import { LOCAL_URL, createAnalyzer, pairOffsets, lagStats, verdict } from './lib.mjs';

const [mode, arg] = process.argv.slice(2);
mkdirSync('logs', { recursive: true });
const BIG = 256 * 1024 * 1024;

function avsync(file) {
  const video = spawnSync('ffmpeg', ['-v', 'error', '-i', file, '-an',
    '-vf', 'signalstats,metadata=mode=print:key=lavfi.signalstats.YAVG:file=-', '-f', 'null', '-'],
  { encoding: 'utf8', maxBuffer: BIG });
  const flashes = [];
  let time = null;
  let previous = 0;
  for (const line of video.stdout.split(/\r?\n/)) {
    const t = /pts_time:([0-9.]+)/.exec(line);
    if (t) { time = Number(t[1]); continue; }
    const y = /YAVG=([0-9.]+)/.exec(line);
    if (y) {
      // Dark backgrounds sit near 30, a full or pillarboxed white flash is above 170.
      if (Number(y[1]) > 128 && previous <= 128) flashes.push(time);
      previous = Number(y[1]);
    }
  }
  const audio = spawnSync('ffmpeg', ['-hide_banner', '-v', 'info', '-nostats', '-i', file, '-vn',
    '-af', 'silencedetect=noise=-35dB:d=0.2', '-f', 'null', '-'], { encoding: 'utf8', maxBuffer: BIG });
  const beeps = [...audio.stderr.matchAll(/silence_end: ([0-9.]+)/g)].map((m) => Number(m[1]));
  return { file, flashes: flashes.length, beeps: beeps.length, ...pairOffsets(flashes, beeps) };
}

if (mode === 'watch') {
  const probe = spawn('ffprobe', ['-v', 'error', '-show_entries', 'packet=stream_index,dts_time', '-of', 'csv=p=0', arg ?? LOCAL_URL],
    { stdio: ['ignore', 'pipe', 'inherit'] });
  const analyzer = createAnalyzer();
  let packets = 0;
  let rest = '';
  probe.stdout.on('data', (chunk) => {
    const lines = (rest + chunk).split(/\r?\n/);
    rest = lines.pop();
    for (const line of lines) {
      const [stream, dts] = line.split(',');
      if (!dts || dts === 'N/A') continue;
      const before = analyzer.events.length;
      analyzer.push({ stream, dts: Number(dts) });
      packets += 1;
      for (const e of analyzer.events.slice(before)) {
        const row = [new Date().toISOString(), e.type, e.stream, e.at.toFixed(3), e.value.toFixed(3)];
        appendFileSync('logs/events.csv', `${row.join(',')}\n`);
        console.log(row.join('  '));
      }
    }
  });
  setInterval(() => console.log(`${new Date().toISOString()}  packets=${packets}  events=${analyzer.events.length}`), 60_000);
  probe.on('close', (code) => {
    appendFileSync('logs/events.csv', `${new Date().toISOString()},disconnect,,,${code}\n`);
    console.error(`READER LOST THE STREAM after ${packets} packets`);
    process.exit(1);
  });
} else if (mode === 'record') {
  mkdirSync('rec', { recursive: true });
  spawn('ffmpeg', ['-v', 'error', '-i', arg ?? LOCAL_URL, '-c', 'copy', '-f', 'segment', '-segment_time', '600',
    '-reset_timestamps', '0', 'rec/seg%04d.mkv'], { stdio: 'inherit' })
    .on('close', (code) => process.exit(code ?? 1));
} else if (mode === 'avsync') {
  console.log(avsync(arg));
} else if (mode === 'verdict') {
  const run = JSON.parse(readFileSync('logs/run.json', 'utf8'));
  const events = existsSync('logs/events.csv')
    ? readFileSync('logs/events.csv', 'utf8').trim().split(/\r?\n/).filter(Boolean).map((l) => l.split(',')) : [];
  const rows = readFileSync('logs/encoder.csv', 'utf8').trim().split(/\r?\n/)
    .map((l) => l.split(',')).map(([wallMs, mediaUs]) => ({ wallMs: Number(wallMs), mediaUs: Number(mediaUs) }));
  // The reader always loses the stream when the run ends on purpose; only earlier losses count.
  const endedAt = run.endedAt ?? Date.now();
  const disconnects = events.filter(([iso, type]) => type === 'disconnect' && Date.parse(iso) < endedAt - 5000).length;
  const segments = existsSync('rec')
    ? readdirSync('rec').filter((f) => f.endsWith('.mkv')).sort().map((f) => `rec/${f}`).filter((f) => statSync(f).size > 5_000_000) : [];
  const first = segments.length ? avsync(segments[0]) : { medianMs: NaN, maxAbsMs: NaN };
  const last = segments.length ? avsync(segments[segments.length - 1]) : { medianMs: NaN, maxAbsMs: NaN };
  const measured = {
    backwards: events.filter(([, type]) => type === 'backwards').length,
    jumps: events.filter(([, type]) => type === 'jump').length,
    readerDisconnects: disconnects,
    encoderExitedEarly: run.encoderExitedEarly,
    ...lagStats(rows),
    avFirstMs: first.medianMs,
    avLastMs: last.medianMs,
    avFirstMaxMs: first.maxAbsMs,
    avLastMaxMs: last.maxAbsMs,
    hours: (endedAt - run.startedAt) / 3600_000,
    seams: run.seams,
    segments: segments.length,
  };
  console.log(measured);
  console.log(verdict(measured));
} else {
  console.error('usage: node check.mjs watch [url|file] | record [url] | avsync <file> | verdict');
  process.exit(2);
}
