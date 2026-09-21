import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { normalizeVideoFilter } from '../continuity/lib.mjs';
import { defaultOverlayConfig, overlayFilters, prepareOverlayFiles } from './lib.mjs';

// node render.mjs <clip.mp4> <outName> [--title T] [--playlist P] [--position N] [--count N]
//   [--next T] [--duration S] [--times 5,20,40] [--all-elements] [--font path] [--logo path]
const argv = process.argv.slice(2);
const [clip, outName] = argv;
if (!clip || !outName) {
  console.error('usage: node render.mjs <clip.mp4> <outName> [--title T] [--duration S] [--times 5,20,40] [--all-elements] ...');
  process.exit(2);
}
const opt = (name, fallback) => (argv.includes(`--${name}`) ? argv[argv.indexOf(`--${name}`) + 1] : fallback);
const flag = (name) => argv.includes(`--${name}`);

const video = {
  title: opt('title', 'Sample Video Title'),
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

const outFile = path.join(outDir, 'render.mp4');
const render = spawnSync('ffmpeg', [
  '-v', 'error', '-y', '-i', clip,
  '-filter_script:v', filterScript,
  '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '18', '-c:a', 'aac',
  outFile,
], { stdio: 'inherit' });
if (render.status !== 0) { console.error('render failed'); process.exit(1); }
console.log(`rendered ${outFile}`);

const times = (opt('times', '') || '').split(',').filter(Boolean).map(Number);
for (const t of times) {
  const frameFile = path.join(outDir, `frame-${t}.png`);
  const shot = spawnSync('ffmpeg', ['-v', 'error', '-y', '-ss', String(t), '-i', outFile, '-frames:v', '1', frameFile]);
  if (shot.status !== 0) console.error(`frame extraction failed at t=${t}`);
  else console.log(`frame ${frameFile}`);
}
