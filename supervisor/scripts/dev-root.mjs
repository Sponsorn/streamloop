// A dev install root in the product layout (paths.rs), so a run never finds a tool on PATH.
//   node dev-root.mjs [root]      (default: %TEMP%\streamloop-devroot)
// Files are hard links (a copy when linking fails), folders are junctions. app\ and data\ are
// left to session.mjs. The root holds streamloop-pot.exe and the plugin zip but no node\ or pot\,
// so the supervisor owns the Rust token server and cannot fall back to the TypeScript one.
import { mkdirSync, linkSync, copyFileSync, existsSync, rmSync, statSync, lstatSync } from 'node:fs';
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
  'supervisor/slate.mp4': join(REPO, 'spikes', 'buffer', 'media', 'slate.mp4'),
};
export const DEFAULT_POT_EXE = join(REPO, 'tokenserver', 'target', 'release', 'streamloop-pot.exe');
export const DEFAULT_PLUGIN_ZIP = join(REPO, 'dist', 'pot-ts', 'bgutil-ytdlp-pot-provider.zip');

function place(file, source) {
  // a stale link would keep an old binary
  if (existsSync(file) && statSync(file).size === statSync(source).size && statSync(file).mtimeMs === statSync(source).mtimeMs) return;
  rmSync(file, { force: true });
  try { linkSync(source, file); } catch { copyFileSync(source, file); }
}

/** Removes a link or folder without following a junction into its target. */
function removeEntry(path) {
  let stat = null;
  try { stat = lstatSync(path); } catch { return; }
  if (stat.isSymbolicLink()) rmSync(path, { force: true });
  else rmSync(path, { recursive: true, force: true });
}

/** potExe null places no server (a side that adopts an outside one). */
export function makeDevRoot(root = DEFAULT_ROOT, { potExe = DEFAULT_POT_EXE, pluginZip = DEFAULT_PLUGIN_ZIP } = {}) {
  const files = { ...SOURCES, 'yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip': pluginZip };
  if (potExe !== null) files['supervisor/streamloop-pot.exe'] = potExe;
  for (const [rel, source] of Object.entries(files)) {
    if (!existsSync(source)) throw new Error(`dev root: ${source} is missing`);
  }
  // an earlier run's node\ and pot\ would let the supervisor fall back to the TypeScript server,
  // and its plugins junction pointed at the user's own plugin folder
  removeEntry(join(root, 'node'));
  removeEntry(join(root, 'pot'));
  const plugins = join(root, 'yt-dlp', 'plugins');
  if (existsSync(plugins) && lstatSync(plugins).isSymbolicLink()) rmSync(plugins, { force: true });
  if (potExe === null) rmSync(join(root, 'supervisor', 'streamloop-pot.exe'), { force: true });
  for (const [rel, source] of Object.entries(files)) {
    const target = join(root, rel);
    mkdirSync(join(target, '..'), { recursive: true });
    place(target, source);
  }
  mkdirSync(join(root, 'app'), { recursive: true });
  return root;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) console.log(makeDevRoot(process.argv[2]));
