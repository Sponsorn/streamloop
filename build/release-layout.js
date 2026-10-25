// Paths each release ZIP must hold, relative to its top folder (streamloop/).
// The full list covers every path supervisor/src/paths.rs resolves (release-layout.test.ts).

const APP = ['app/package.json', 'app/node-version.txt', 'app/src/server/index.ts', 'app/node_modules', 'START.bat'];

/** @type {string[]} */
export const FULL_LAYOUT = [
  ...APP,
  'app/config.json',
  'node/node.exe',
  'supervisor/streamloop-supervisor.exe',
  'supervisor/slate.mp4',
  'ffmpeg/ffmpeg.exe',
  'yt-dlp/yt-dlp.exe',
  'yt-dlp/deno.exe',
  'yt-dlp/plugins',
  'yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip',
  'pot/package.json',
  'pot/build/main.js',
  'pot/node_modules',
  'data',
];

/** An update only swaps app\ and the launcher. @type {string[]} */
export const UPDATE_LAYOUT = APP;

/**
 * The layout paths absent from a `tar -tf` listing; a path counts as a file or a folder.
 * @param {string[]} entries
 * @param {string[]} layout
 * @param {string} [top]
 * @returns {string[]}
 */
export function missingPaths(entries, layout, top = 'streamloop/') {
  const names = entries.map((e) => e.replace(/\\/g, '/'));
  return layout.filter((p) => !names.some((e) => e === top + p || e.startsWith(`${top}${p}/`)));
}
