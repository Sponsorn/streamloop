import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { normalizeVideoFilter } from '../continuity/lib.mjs';
import { defaultOverlayConfig, overlayFilters, prepareOverlayFiles } from './lib.mjs';

// node render.mjs <clip.mp4> <outName> [--title T | --title-file path.txt] [--playlist P] [--position N]
//   [--count N] [--next T] [--duration S] [--times 5,20,40] [--all-elements] [--font path] [--logo path]
const argv = process.argv.slice(2);
const [clip, outName] = argv;
if (!clip || !outName) {
  console.error('usage: node render.mjs <clip.mp4> <outName> [--title T] [--duration S] [--times 5,20,40] [--all-elements] ...');
  process.exit(2);
}
const opt = (name, fallback) => (argv.includes(`--${name}`) ? argv[argv.indexOf(`--${name}`) + 1] : fallback);
const flag = (name) => argv.includes(`--${name}`);

// A non-ASCII --title on the command line goes through the shell's/Windows' argv encoding
// before Node ever sees it; --title-file reads real UTF-8 bytes directly, no argv involved.
const titleFile = opt('title-file', null);
const title = titleFile ? readFileSync(titleFile, 'utf8').trim() : opt('title', 'Sample Video Title');
console.log(`title codepoints: ${[...title].map((c) => c.codePointAt(0).toString(16)).join(' ')}`);

const video = {
  title,
  playlistName: opt('playlist', 'Sample Playlist'),
  position: Number(opt('position', '1')),
  count: Number(opt('count', '1')),
  nextTitle: opt('next', 'Next Video Title'),
  durationSeconds: Number(opt('duration', '45')),
};

const config = defaultOverlayConfig();
if (flag('all-elements')) {
  config.elements.forEach((el) => { el.enabled = true; });
  const logo = config.elements.find((e) => e.type === 'logo');
  logo.path = opt('logo', path.resolve('spikes/overlay/out/test-logo.png'));
}
if (opt('font', null)) config.fontFile = opt('font', null);

const outDir = path.resolve('spikes/overlay/out', outName);
mkdirSync(outDir, { recursive: true });

const paths = prepareOverlayFiles(video, outDir, config);
const overlay = overlayFilters(config, video, paths);
const filterScript = path.join(outDir, 'filters.txt');
writeFileSync(filterScript, `${normalizeVideoFilter()},${overlay}`, 'utf8');

const probeDuration = (file) => {
  const probe = spawnSync('ffprobe', ['-v', 'error', '-show_entries', 'format=duration', '-of', 'default=nw=1:nk=1', file], { encoding: 'utf8' });
  return Number(probe.stdout.trim());
};
const inputDuration = probeDuration(clip);

const outFile = path.join(outDir, 'render.mp4');
// A hard -t cap plus a wall-clock kill are the backstop against a filtergraph that doesn't
// terminate on its own (e.g. an overlay source with no EOF and no shortest=1) running forever.
const render = spawnSync('ffmpeg', [
  '-v', 'error', '-y', '-i', clip, '-t', String(inputDuration + 2),
  '-filter_script:v', filterScript,
  '-c:v', 'libx264', '-preset', opt('preset', 'ultrafast'), '-crf', opt('crf', '30'), '-c:a', 'aac',
  outFile,
], { stdio: 'inherit', timeout: Math.max(inputDuration * 5, 30) * 1000, killSignal: 'SIGKILL' });
if (render.error?.code === 'ETIMEDOUT' || render.signal) {
  console.error(`render exceeded ${Math.max(inputDuration * 5, 30)}s (5x the clip length) and was killed - filtergraph likely never reaches EOF`);
  process.exit(1);
}
if (render.status !== 0) { console.error('render failed'); process.exit(1); }

const outputDuration = probeDuration(outFile);
const frameSeconds = 1 / 30;
if (Math.abs(outputDuration - inputDuration) > frameSeconds) {
  console.error(`rendered duration ${outputDuration}s does not match input duration ${inputDuration}s (off by more than one frame)`);
  process.exit(1);
}
console.log(`rendered ${outFile} (input ${inputDuration}s, output ${outputDuration}s)`);

const times = (opt('times', '') || '').split(',').filter(Boolean).map(Number);
for (const t of times) {
  const frameFile = path.join(outDir, `frame-${t}.png`);
  const shot = spawnSync('ffmpeg', ['-v', 'error', '-y', '-ss', String(t), '-i', outFile, '-frames:v', '1', frameFile]);
  if (shot.status !== 0) console.error(`frame extraction failed at t=${t}`);
  else console.log(`frame ${frameFile}`);
}
