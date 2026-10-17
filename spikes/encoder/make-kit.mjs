// Builds kit/spike2-kit/ and dist/spike2-kit.zip: everything spike 2 needs on a fresh box.
//   node spikes/encoder/make-kit.mjs
// Node and the test clips are downloaded once and reused from dist/ and media/.
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { FORMAT, PLAYER_CLIENT, YTDLP } from '../buffer/lib.mjs';

const here = fileURLToPath(new URL('.', import.meta.url));
const spikes = join(here, '..');
const KIT = join(here, 'kit', 'spike2-kit');
const DIST = join(here, 'dist');
const MEDIA = join(here, 'media');
// The real gyan build; the ffmpeg on PATH is a Chocolatey launcher shim.
const FFMPEG_BIN = 'C:\\ProgramData\\chocolatey\\lib\\ffmpeg\\tools\\ffmpeg\\bin';
const NODE = 'node-v22.12.0-win-x64';
const NODE_URL = `https://nodejs.org/dist/v22.12.0/${NODE}.zip`;
const TAR = join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe');
// Blender Foundation films (CC BY), 1080p avc1. Sections keep the set near 250 MB.
const CLIPS = [
  { name: 'sintel', id: 'eRsGyueVLvQ', section: '120-300' },
  { name: 'spring', id: 'WhWc3b3KhnY', section: '60-240' },
  { name: 'tears-of-steel', id: 'R6MlUcmOul8' },
];

const run = (cmd, args) => {
  const r = spawnSync(cmd, args, { stdio: 'inherit' });
  if (r.status !== 0) throw new Error(`${cmd} ${args.join(' ')} exited ${r.status}`);
};

mkdirSync(MEDIA, { recursive: true });
for (const clip of CLIPS) {
  const file = join(MEDIA, `${clip.name}.mp4`);
  if (existsSync(file)) continue;
  console.log(`Downloading ${clip.name}...`);
  // deno solves YouTube's player challenge; yt-dlp needs its absolute path.
  run(YTDLP, [
    '--no-warnings', '--no-update', '--no-progress', '--js-runtimes', `deno:${join(spikes, '..', 'yt-dlp', 'deno.exe')}`,
    '--extractor-args', `youtube:player_client=${PLAYER_CLIENT}`, '--ffmpeg-location', FFMPEG_BIN,
    '--no-playlist', '-f', FORMAT, '--merge-output-format', 'mp4',
    ...(clip.section ? ['--download-sections', `*${clip.section}`] : []),
    '-o', file, `https://www.youtube.com/watch?v=${clip.id}`,
  ]);
}

mkdirSync(DIST, { recursive: true });
const nodeZip = join(DIST, `${NODE}.zip`);
if (!existsSync(nodeZip)) {
  console.log(`Downloading ${NODE_URL}...`);
  const res = await fetch(NODE_URL);
  if (!res.ok) throw new Error(`${NODE_URL}: HTTP ${res.status}`);
  writeFileSync(nodeZip, Buffer.from(await res.arrayBuffer()));
}

rmSync(KIT, { recursive: true, force: true });
mkdirSync(join(KIT, 'media'), { recursive: true });
run(TAR, ['-x', '-f', nodeZip, '-C', DIST, `${NODE}/node.exe`]);
copyFileSync(join(DIST, NODE, 'node.exe'), join(KIT, 'node.exe'));
for (const exe of ['ffmpeg.exe', 'ffprobe.exe']) copyFileSync(join(FFMPEG_BIN, exe), join(KIT, exe));
for (const file of ['mediamtx.exe', 'mediamtx.yml']) copyFileSync(join(spikes, 'tools', file), join(KIT, file));
for (const clip of CLIPS) copyFileSync(join(MEDIA, `${clip.name}.mp4`), join(KIT, 'media', `${clip.name}.mp4`));

// Flat kit folder: the scripts' ../ imports point next door.
const scripts = [
  ['continuity/lib.mjs', 'lib.mjs'],
  ['continuity/run.mjs', 'run.mjs'],
  ['continuity/check.mjs', 'check.mjs'],
  ['overlay/lib.mjs', 'overlay-lib.mjs'],
  ['encoder/spike2-lib.mjs', 'spike2-lib.mjs'],
  ['encoder/spike2.mjs', 'spike2.mjs'],
];
for (const [from, to] of scripts) {
  const text = readFileSync(join(spikes, from), 'utf8')
    .replaceAll("'../overlay/lib.mjs'", "'./overlay-lib.mjs'")
    .replaceAll("'../continuity/lib.mjs'", "'./lib.mjs'");
  if (/from '\.\.\//.test(text)) throw new Error(`${from} still imports from a parent folder`);
  writeFileSync(join(KIT, to), text);
}
// cmd.exe wants CRLF line endings.
writeFileSync(join(KIT, 'run.cmd'), [
  '@echo off',
  'cd /d "%~dp0"',
  'title Spike 2',
  // Explicit path: cmd skips the current folder when NoDefaultCurrentDirectoryInExePath is set.
  '"%~dp0node.exe" "%~dp0spike2.mjs" %*',
  'echo.',
  'echo Finished. The results are in the results-* folder inside %~dp0',
  'pause',
  '',
].join('\r\n'));
writeFileSync(join(KIT, 'README.txt'), readFileSync(join(here, 'README.txt'), 'utf8').replace(/\r?\n/g, '\r\n'));

const zip = join(DIST, 'spike2-kit.zip');
rmSync(zip, { force: true });
run(TAR, ['-a', '-c', '-f', zip, '-C', join(here, 'kit'), 'spike2-kit']);
console.log(`Built ${zip} (${(statSync(zip).size / 2 ** 20).toFixed(0)} MB)`);
