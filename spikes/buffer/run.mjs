// Spike 4 supervisor: walk a YouTube playlist within a byte budget, feed the encoder from the
// cache, and fall back to a slate clip whenever the next video is not on disk at a seam.
import { spawn, spawnSync } from 'node:child_process';
import { PassThrough } from 'node:stream';
import { mkdirSync, rmSync, appendFileSync, writeFileSync, readdirSync, statSync, existsSync, unlinkSync } from 'node:fs';
import { BASE_OFFSET, LOCAL_URL, feederArgs, encoderArgs, offsetAfter, parseProgress } from '../continuity/lib.mjs';
import {
  YTDLP, SLATE, PLAYER_CLIENT, playlistArgs, sizeArgs, downloadArgs, parseDownloadProgress,
  bytesOf, decide, walkAfter, nextAtSeam, deletable,
} from './lib.mjs';

const argv = process.argv.slice(2);
const opt = (name, fallback) => (argv.includes(`--${name}`) ? argv[argv.indexOf(`--${name}`) + 1] : fallback);
const playlist = opt('playlist', 'https://www.youtube.com/playlist?list=PL6B3937A5D230E335');
const budgetBytes = Number(opt('budget-mb', '300')) * 1024 * 1024;
const limitRate = opt('limit-rate', '');
const hours = Number(opt('hours', '1'));
const target = opt('target', LOCAL_URL);
const codec = opt('codec', 'libx264');
const client = opt('player-client', PLAYER_CLIENT);
const bufferMb = Number(opt('buffer-mb', '192'));
// A static slate compresses so well that 192 MB of pipe holds ten minutes of it. Capping the
// read-ahead in media seconds keeps every seam decision close to what the viewer is watching.
const readAheadSeconds = Number(opt('read-ahead-seconds', '20'));
// Criterion 7: a bogus id after the first entry, to prove a dead video is retried once and skipped.
const injectBroken = argv.includes('--inject-broken');
// --start/--max-videos cut the playlist down to the slice a test needs (one big video, four short ones).
const start = Number(opt('start', '0'));
const maxVideos = Number(opt('max-videos', '0'));

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const CACHE = 'cache';
mkdirSync('logs', { recursive: true });
rmSync(CACHE, { recursive: true, force: true });
mkdirSync(CACHE, { recursive: true });
const log = (file, row) => appendFileSync(`logs/${file}`, `${row.join(',')}\n`);
const notProgress = (text) => String(text).split(/\r?\n/).filter((l) => l.trim() && !/^\w+=/.test(l.trim()));
const cacheBytes = () => readdirSync(CACHE).reduce((n, f) => n + statSync(`${CACHE}/${f}`).size, 0);

const resolved = spawnSync(YTDLP, playlistArgs(playlist, client), { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
const entries = resolved.stdout.trim().split(/\r?\n/).filter(Boolean)
  .map((line) => { const [id, title] = line.split('\t'); return { id, title: title ?? '' }; });
if (!entries.length) { console.error(`yt-dlp resolved no entries:\n${resolved.stderr}`); process.exit(2); }
if (start) entries.splice(0, start);
if (injectBroken) entries.splice(1, 0, { id: 'aaaaaaaaaaa', title: 'INJECTED BROKEN ENTRY' });
if (maxVideos) entries.length = Math.min(entries.length, maxVideos);
writeFileSync('logs/playlist.json', JSON.stringify(entries, null, 2));
console.log(`${entries.length} entries, budget ${(budgetBytes / 1048576).toFixed(0)} MB${limitRate ? `, limit-rate ${limitRate}` : ''}`);

const run = {
  startedAt: Date.now(), target, codec, hours, baseOffset: BASE_OFFSET, bufferMb, readAheadSeconds,
  playlist, budgetBytes, limitRate, client, injectBroken, start, maxVideos, entries: entries.length,
  endedAt: null, seams: 0, encoderExitedEarly: false, maxCacheBytes: 0, slateSeams: 0, videoSeams: 0, skipped: [],
};
const saveRun = () => writeFileSync('logs/run.json', JSON.stringify(run, null, 2));
saveRun();

const encoder = spawn('ffmpeg', encoderArgs(target, { codec }), { stdio: ['pipe', 'ignore', 'pipe'] });
const readAhead = new PassThrough({ highWaterMark: Math.max(1, bufferMb) * 1024 * 1024 });
readAhead.pipe(encoder.stdin);
let encoderAlive = true;
let encoderMedia = 0;
let encoderStarted = false;
let finishing = false;
let currentFeeder = null;
let pending = '';
encoder.stderr.on('data', (chunk) => {
  const lines = (pending + chunk).split(/\r?\n/);
  pending = lines.pop();
  const progress = parseProgress(lines.join('\n'));
  if (progress.out_time_us && progress.out_time_us !== 'N/A') {
    encoderMedia = Number(progress.out_time_us) / 1e6;
    encoderStarted = true;
    log('encoder.csv', [Date.now(), progress.out_time_us, progress.speed ?? '']);
  }
  const other = notProgress(lines.join('\n'));
  if (other.length) appendFileSync('logs/encoder.log', `${other.join('\n')}\n`);
});
encoder.stdin.on('error', () => {});
encoder.on('close', (code) => {
  encoderAlive = false;
  run.endedAt = Date.now();
  run.encoderExitedEarly = !finishing;
  saveRun();
  if (!finishing) {
    console.error(`ENCODER EXITED EARLY with code ${code}. This run is a fail. See logs/encoder.log`);
    currentFeeder?.kill();
    process.exit(1);
  }
});

/** onDisk entries carry their own lifecycle flags; the pure rules in lib.mjs read them. */
const onDisk = [];
let downloading = null;
let walkDone = false;
let playing = 'none';

function runFeeder(file, offset) {
  return new Promise((resolve) => {
    const feeder = spawn('ffmpeg', feederArgs(file, offset), { stdio: ['ignore', 'pipe', 'pipe'] });
    currentFeeder = feeder;
    let frames = 0;
    let errors = '';
    feeder.stderr.on('data', (chunk) => {
      const progress = parseProgress(chunk);
      if (progress.frame) frames = Number(progress.frame);
      errors += notProgress(chunk).join('\n');
    });
    feeder.stdout.pipe(readAhead, { end: false });
    feeder.on('close', (code) => resolve({ frames, code, errors }));
  });
}

let currentYtdlp = null;

function ytdlp(args, onLine) {
  return new Promise((resolve) => {
    const proc = spawn(YTDLP, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    currentYtdlp = proc;
    let out = '';
    let err = '';
    let rest = '';
    proc.stdout.on('data', (chunk) => {
      out += chunk;
      if (!onLine) return;
      const lines = (rest + chunk).split(/\r?\n/);
      rest = lines.pop();
      for (const line of lines) onLine(line);
    });
    // Kept whole: a successful download can still hide a rename or merge that limped.
    proc.stderr.on('data', (chunk) => { err += chunk; appendFileSync('logs/ytdlp.log', chunk); });
    proc.on('close', (code) => resolve({ code, out, err }));
  });
}

// ponytail: downloads run in this one async loop, so "one at a time" needs no lock.
async function downloader() {
  let state = { cursor: 0, attempts: 0 };
  while (encoderAlive && !finishing) {
    const decision = decide({ entries, cursor: state.cursor, onDisk, budgetBytes });
    if (decision.type === 'done') { walkDone = true; return; }
    if (decision.type === 'full') { await sleep(1000); continue; }
    const entry = entries[decision.index];
    state = { ...state, attempts: state.attempts + 1 };
    const startedAt = Date.now();
    let ok = false;
    let reason = '';
    let bytes = 0;
    if (entry.bytes === undefined) {
      const probe = await ytdlp(sizeArgs(entry.id, client));
      const size = Number(probe.out.trim().split(/\r?\n/)[0]);
      if (probe.code !== 0 || !Number.isFinite(size)) {
        reason = (probe.err.trim().split(/\r?\n/).pop() ?? 'size probe failed').slice(0, 200);
      } else {
        entry.bytes = size;
        // The budget may not allow it now that its size is known; re-decide before downloading.
        if (decide({ entries, cursor: state.cursor, onDisk, budgetBytes }).type === 'full') {
          state = { ...state, attempts: state.attempts - 1 };
          await sleep(1000);
          continue;
        }
      }
    }
    if (entry.bytes !== undefined && !reason) {
      const file = `${CACHE}/${entry.id}.mp4`;
      downloading = { id: entry.id, percent: 0 };
      const dl = await ytdlp(downloadArgs(entry.id, file, { client, limitRate }),
        (line) => { const p = parseDownloadProgress(line); if (p !== null) downloading.percent = p; });
      downloading = null;
      ok = dl.code === 0 && existsSync(file);
      // yt-dlp leaves a .part behind even on a clean merge, and it would count against the
      // budget for the rest of the run. Everything but the merged file goes, either way.
      for (const f of readdirSync(CACHE)) {
        if (f.startsWith(entry.id) && `${CACHE}/${f}` !== file) unlinkSync(`${CACHE}/${f}`);
      }
      if (ok) {
        bytes = statSync(file).size;
        onDisk.push({ id: entry.id, title: entry.title, file, bytes, complete: true, played: false, feederAlive: false });
      } else {
        reason = (dl.err.trim().split(/\r?\n/).pop() ?? `yt-dlp exit ${dl.code}`).slice(0, 200);
        if (existsSync(file)) unlinkSync(file);
      }
    }
    const next = walkAfter(state, ok);
    log('downloads.csv', [new Date().toISOString(), entry.id, ok ? 'done' : (next.skipped ? 'skip' : 'retry'),
      state.attempts, bytes, Date.now() - startedAt, reason.replace(/[,\r\n]/g, ' ')]);
    if (next.skipped) { run.skipped.push(entry.id); console.error(`skipped ${entry.id}: ${reason}`); }
    state = next;
  }
}

const bufferLogger = setInterval(() => {
  const bytes = cacheBytes();
  run.maxCacheBytes = Math.max(run.maxCacheBytes, bytes);
  log('buffer.csv', [new Date().toISOString(), bytes, onDisk.length,
    downloading?.id ?? '', downloading ? downloading.percent.toFixed(1) : '', playing, encoderMedia.toFixed(1)]);
}, 10_000);
// The 10 s log can miss a peak between two samples; this catches the real maximum.
const bytesPoller = setInterval(() => { run.maxCacheBytes = Math.max(run.maxCacheBytes, cacheBytes()); }, 1000);

console.log(`Streaming to ${target} with ${codec} for up to ${hours} h. Ctrl+C ends the run as a fail.`);
const deadline = run.startedAt + hours * 3600_000;
downloader();

let offset = BASE_OFFSET;
while (encoderAlive && Date.now() < deadline) {
  // Hold the seam until the encoder is close behind, so slate/video is chosen on live state.
  // encoderMedia reads 0 until ffmpeg's first progress row, which would wave several clips through.
  while (encoderAlive && ((run.seams > 0 && !encoderStarted) || offset - BASE_OFFSET - encoderMedia > readAheadSeconds)) await sleep(250);
  if (!encoderAlive) break;
  const video = nextAtSeam(onDisk);
  if (!video && walkDone && !downloading) break;
  const name = video?.id ?? 'slate';
  if (video) { video.played = true; video.feederAlive = true; }
  playing = name;
  const startedIso = new Date().toISOString();
  const { frames, code, errors } = await runFeeder(video?.file ?? SLATE, offset);
  if (video) video.feederAlive = false;
  // check.mjs reads column 3 as the timeline offset; the exit time is appended for the delete audit.
  log('seams.csv', [run.seams, name, startedIso, offset.toFixed(6), frames, code, new Date().toISOString()]);
  if (code !== 0) console.error(`feeder failed on ${name} (code ${code}): ${errors.trim()}`);
  offset = offsetAfter(offset, frames);
  run.seams += 1;
  run[video ? 'videoSeams' : 'slateSeams'] += 1;
  for (const done of deletable(onDisk)) {
    unlinkSync(done.file);
    onDisk.splice(onDisk.indexOf(done), 1);
    // Timestamped so "no file deleted while a feeder read it" is checkable against seams.csv.
    log('deletes.csv', [new Date().toISOString(), done.id, done.bytes]);
  }
  saveRun();
  console.log(`${startedIso}  seam=${run.seams} ${name}  timeline=${offset.toFixed(1)}s  cache=${(cacheBytes() / 1048576).toFixed(0)}MB`);
}

finishing = true;
// The downloader only re-reads `finishing` between videos, so a throttled fetch would hang the run.
// ponytail: taskkill /T, because killing the PyInstaller stub alone orphans the real worker.
if (currentYtdlp?.pid) spawnSync('taskkill', ['/PID', String(currentYtdlp.pid), '/T', '/F'], { stdio: 'ignore' });
clearInterval(bufferLogger);
clearInterval(bytesPoller);
readAhead.end();
run.endedAt = Date.now();
saveRun();
console.log(`Done: ${run.seams} seams (${run.videoSeams} video, ${run.slateSeams} slate) in ${((run.endedAt - run.startedAt) / 60_000).toFixed(1)} min, peak cache ${(run.maxCacheBytes / 1048576).toFixed(1)} MB`);
