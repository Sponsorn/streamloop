// A dev install root in the product layout (paths.rs), so a run never finds a tool on PATH.
//   node dev-root.mjs [root]      (default: %TEMP%\streamloop-devroot)
// Files are hard links (a copy when linking fails), folders are junctions. app\ and data\ are
// left to session.mjs.
import { mkdirSync, linkSync, copyFileSync, symlinkSync, existsSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { FFMPEG } from './lib.mjs';

const REPO = fileURLToPath(new URL('../..', import.meta.url));
export const DEFAULT_ROOT = join(tmpdir(), 'streamloop-devroot');

export const SOURCES = {
  'ffmpeg/ffmpeg.exe': FFMPEG,
  'yt-dlp/yt-dlp.exe': join(REPO, 'yt-dlp', 'yt-dlp.exe'),
  'yt-dlp/deno.exe': join(REPO, 'yt-dlp', 'deno.exe'),
  'node/node.exe': 'C:/Program Files/nodejs/node.exe',
  'supervisor/slate.mp4': join(REPO, 'spikes', 'buffer', 'media', 'slate.mp4'),
};
export const FOLDERS = {
  'yt-dlp/plugins': join(process.env.APPDATA ?? '', 'yt-dlp', 'plugins'),
  // pot\build\main.js is the token server; its node_modules sit next to build\
  pot: 'C:/Users/anton/bgutil-ytdlp-pot-provider/server',
};

function place(file, source) {
  // a stale link would keep an old binary
  if (existsSync(file) && statSync(file).size === statSync(source).size && statSync(file).mtimeMs === statSync(source).mtimeMs) return;
  rmSync(file, { force: true });
  try { linkSync(source, file); } catch { copyFileSync(source, file); }
}

export function makeDevRoot(root = DEFAULT_ROOT) {
  for (const [rel, source] of Object.entries({ ...SOURCES, ...FOLDERS })) {
    if (!existsSync(source)) throw new Error(`dev root: ${source} is missing`);
    const target = join(root, rel);
    mkdirSync(join(target, '..'), { recursive: true });
    if (rel in FOLDERS) { if (!existsSync(target)) symlinkSync(source, target, 'junction'); } else place(target, source);
  }
  mkdirSync(join(root, 'app'), { recursive: true });
  return root;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) console.log(makeDevRoot(process.argv[2]));
