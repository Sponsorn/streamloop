import { spawn } from 'node:child_process';
import { mkdirSync, appendFileSync, writeFileSync } from 'node:fs';
import {
  CLIPS, SHORT_CLIPS, BASE_OFFSET, LOCAL_URL, feederArgs, encoderArgs, offsetAfter, parseProgress,
} from './lib.mjs';

const argv = process.argv.slice(2);
const opt = (name, fallback) => (argv.includes(`--${name}`) ? argv[argv.indexOf(`--${name}`) + 1] : fallback);
const hours = Number(opt('hours', '2'));
const codec = opt('codec', 'libx264');
const baseOffset = Number(opt('base-offset', String(BASE_OFFSET)));
const short = argv.includes('--short');
const clips = short ? SHORT_CLIPS : CLIPS;

let target = opt('target', LOCAL_URL);
let label = target;
if (argv.includes('--twitch')) {
  const key = process.env.TWITCH_STREAM_KEY;
  if (!key) { console.error('Set TWITCH_STREAM_KEY first.'); process.exit(2); }
  // Twitch is the judge; the local leg keeps check.mjs working and may fail without dropping Twitch.
  target = `[f=flv]rtmp://live.twitch.tv/app/${key}?bandwidthtest=true|[f=flv:onfail=ignore]${LOCAL_URL}`;
  label = 'twitch bandwidthtest (key hidden) + local';
}

mkdirSync('logs', { recursive: true });
const log = (file, row) => appendFileSync(`logs/${file}`, `${row.join(',')}\n`);
const run = {
  startedAt: Date.now(), target: label, codec, hours, short, baseOffset,
  endedAt: null, seams: 0, encoderExitedEarly: false,
};
const saveRun = () => writeFileSync('logs/run.json', JSON.stringify(run, null, 2));
saveRun();

const notProgress = (text) => String(text).split(/\r?\n/).filter((l) => l.trim() && !/^\w+=/.test(l.trim()));

// A target in tee syntax starts with an option block, e.g. [f=flv]rtmp://...
const encoder = spawn('ffmpeg', encoderArgs(target, { codec, tee: target.startsWith('[') }), { stdio: ['pipe', 'ignore', 'pipe'] });
let encoderAlive = true;
let finishing = false;
let currentFeeder = null;
let pending = '';
encoder.stderr.on('data', (chunk) => {
  const lines = (pending + chunk).split(/\r?\n/);
  pending = lines.pop();
  const progress = parseProgress(lines.join('\n'));
  if (progress.out_time_us && progress.out_time_us !== 'N/A') log('encoder.csv', [Date.now(), progress.out_time_us, progress.speed ?? '']);
  const other = notProgress(lines.join('\n'));
  if (other.length) appendFileSync('logs/encoder.log', `${other.join('\n')}\n`);
});
// A dead encoder makes stdin writes fail; the close handler below reports it.
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
  console.log(`Done: ${run.seams} seams in ${((run.endedAt - run.startedAt) / 3600_000).toFixed(2)} h`);
});

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
    feeder.stdout.pipe(encoder.stdin, { end: false });
    feeder.on('close', (code) => resolve({ frames, code, errors }));
  });
}

console.log(`Streaming to ${label} with ${codec} for ${hours} h. Ctrl+C ends the run as a fail.`);
const deadline = run.startedAt + hours * 3600_000;
let offset = baseOffset;
while (encoderAlive && Date.now() < deadline) {
  const clip = clips[run.seams % clips.length];
  const startedIso = new Date().toISOString();
  const { frames, code, errors } = await runFeeder(`media/${clip.name}.mp4`, offset);
  log('seams.csv', [run.seams, clip.name, startedIso, offset.toFixed(6), frames, code]);
  if (code !== 0) console.error(`feeder failed on ${clip.name} (code ${code}): ${errors.trim()}`);
  offset = offsetAfter(offset, frames);
  run.seams += 1;
  if (run.seams % 25 === 0) { saveRun(); console.log(`${startedIso}  seams=${run.seams}  timeline=${offset.toFixed(1)} s`); }
}
finishing = true;
encoder.stdin.end();
