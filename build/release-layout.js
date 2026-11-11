// Paths the release ZIP must hold, relative to its top folder (streamloop/).
// crates/streamloop/tests/release_layout.rs checks that this list covers every path
// the Layout struct in crates/common/src/paths.rs names, except the ones made at run time:
// app/config.json (created from the example on first start, so a reinstall keeps the user's),
// the update files and the logs.

/** @type {string[]} */
export const FULL_LAYOUT = [
  'START.bat',
  'streamloop.exe',
  'app/config.example.json',
  'supervisor/streamloop-pot.exe',
  'supervisor/slate.mp4',
  'ffmpeg/ffmpeg.exe',
  'yt-dlp/yt-dlp.exe',
  'yt-dlp/deno.exe',
  'yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip',
  'pot/package.json',
  'pot/build/main.js',
  'pot/node_modules',
  'node/node.exe',
  'data',
];

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
