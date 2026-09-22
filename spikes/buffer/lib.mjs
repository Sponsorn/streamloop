import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export const YTDLP = fileURLToPath(new URL('../../yt-dlp/yt-dlp.exe', import.meta.url));
export const SLATE = fileURLToPath(new URL('./media/slate.mp4', import.meta.url));
export const SLATE_SECONDS = 10;
// tv answers "the page needs to be reloaded" and web_safari is SABR-only, so neither yields a
// downloadable format here; web_embedded does.
export const PLAYER_CLIENT = 'web_embedded';
// avc1 first: the feeder decodes on CPU beside the encoder, and av1 1080p costs several cores.
export const FORMAT = 'bv*[height<=1080][vcodec^=avc1]+ba[ext=m4a]/bv*[height<=1080]+ba/b[height<=1080]';
// Without a JS runtime yt-dlp cannot solve the player challenge and every client is format-less.
// ponytail: borrowed from the last release build; install deno properly if this spike grows up.
const DENO = fileURLToPath(new URL('../../dist/streamloop/yt-dlp/deno.exe', import.meta.url));

const watchUrl = (id) => `https://www.youtube.com/watch?v=${id}`;
const base = (client) => [
  '--no-warnings', '--no-update',
  ...(existsSync(DENO) ? ['--js-runtimes', `deno:${DENO}`] : []),
  '--extractor-args', `youtube:player_client=${client}`,
];

export function playlistArgs(url, client = PLAYER_CLIENT) {
  return [...base(client), '--flat-playlist', '--print', '%(id)s\t%(title)s', url];
}

/** yt-dlp sums the selected video+audio formats, so one line is the merged download's size. */
export function sizeArgs(id, client = PLAYER_CLIENT) {
  return [...base(client), '--no-playlist', '-f', FORMAT, '-O', '%(filesize,filesize_approx)s', watchUrl(id)];
}

export function downloadArgs(id, outFile, { client = PLAYER_CLIENT, limitRate } = {}) {
  return [
    ...base(client), '--no-playlist', '--newline', '--no-continue', '-f', FORMAT,
    '--merge-output-format', 'mp4',
    ...(limitRate ? ['--limit-rate', limitRate] : []),
    '-o', outFile, watchUrl(id),
  ];
}

export function slateArgs(outFile, text = 'Back in a moment') {
  return [
    '-v', 'error', '-y',
    '-f', 'lavfi', '-i', 'color=c=0x0c0c14:s=1920x1080:r=30',
    '-f', 'lavfi', '-i', 'anullsrc=r=48000:cl=stereo',
    '-t', String(SLATE_SECONDS),
    '-vf', `drawtext=fontfile='C\\:/Windows/Fonts/malgun.ttf':text='${text}':fontcolor=white:fontsize=56:x=(w-text_w)/2:y=(h-text_h)/2`,
    '-c:v', 'libx264', '-preset', 'veryfast', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-b:a', '128k',
    outFile,
  ];
}

export function parseDownloadProgress(line) {
  const m = /^\[download\]\s+([0-9.]+)%/.exec(String(line));
  return m ? Number(m[1]) : null;
}

export function bytesOf(onDisk) {
  return onDisk.reduce((n, v) => n + (v.bytes || 0), 0);
}

/** The budget is a soft cap on look-ahead, never a reason to have nothing to play. */
export function decide({ entries, cursor, onDisk, budgetBytes }) {
  if (cursor >= entries.length) return { type: 'done' };
  if (onDisk.length === 0) return { type: 'download', index: cursor };
  return bytesOf(onDisk) + (entries[cursor].bytes || 0) <= budgetBytes
    ? { type: 'download', index: cursor }
    : { type: 'full' };
}

/** Walk step. Advances past a finished video, and past a failed one only on its second try,
 *  so a private or removed entry costs one retry and never stalls the playlist. */
export function walkAfter({ cursor, attempts }, ok) {
  if (ok) return { cursor: cursor + 1, attempts: 0, skipped: false };
  if (attempts >= 2) return { cursor: cursor + 1, attempts: 0, skipped: true };
  return { cursor, attempts, skipped: false };
}

/** The cache is filled in playlist order, so the first unplayed complete file is the next one.
 *  null means the next video is not ready and the seam gets the slate. */
export function nextAtSeam(onDisk) {
  return onDisk.find((v) => v.complete && !v.played) ?? null;
}

/** Only files whose feeder has exited; unlinking under a reading feeder truncates the stream. */
export function deletable(onDisk) {
  return onDisk.filter((v) => v.played && !v.feederAlive);
}
