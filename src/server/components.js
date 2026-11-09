import { createHash } from 'crypto';
import { createReadStream } from 'fs';
import { readdir, stat } from 'fs/promises';
import { join } from 'path';

/** @typedef {'node'|'supervisor'|'ffmpeg'|'pot'|'ytdlp-plugin'} Part */

/** @type {Part[]} */
export const PARTS = ['node', 'supervisor', 'ffmpeg', 'pot', 'ytdlp-plugin'];

/** @type {Record<Part, string>} */
export const PART_DIRS = {
  node: 'node',
  supervisor: 'supervisor',
  ffmpeg: 'ffmpeg',
  pot: 'pot',
  'ytdlp-plugin': 'yt-dlp/plugins',
};

export const SWAP_FOLDERS = ['node', 'supervisor', 'ffmpeg', 'pot', 'yt-dlp'];

/** @type {Record<string, string>} */
export const PART_KEY_FILES = {
  node: 'node/node.exe',
  supervisor: 'supervisor/streamloop-supervisor.exe',
  ffmpeg: 'ffmpeg/ffmpeg.exe',
  pot: 'pot/build/main.js',
  'yt-dlp': 'yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip',
};

/**
 * Folders a full update swaps for these parts. yt-dlp is always included:
 * the plugin lives inside it and its exe is carried over.
 * @param {Part[]} parts
 * @returns {string[]}
 */
export function foldersFor(parts) {
  const wanted = new Set(parts.map((p) => PART_DIRS[p].split('/')[0]));
  wanted.add('yt-dlp');
  return SWAP_FOLDERS.filter((f) => wanted.has(f));
}

/** @param {import('crypto').Hash} hash @param {string} file */
function feedFile(hash, file) {
  return new Promise((resolve, reject) => {
    const s = createReadStream(file);
    s.on('data', (chunk) => hash.update(chunk));
    s.on('error', reject);
    s.on('end', resolve);
  });
}

/**
 * SHA-256 over every file under the part's folder, by sorted relative path.
 * Content and names only, never mtimes.
 * @param {string} root
 * @param {Part} part
 * @returns {Promise<string | null>} null when the folder is missing
 */
export async function hashPart(root, part) {
  const dir = join(root, PART_DIRS[part]);
  let entries;
  try {
    entries = await readdir(dir, { recursive: true });
  } catch (err) {
    if (err && err.code === 'ENOENT') return null;
    throw err;
  }
  const files = [];
  for (const e of entries) {
    if ((await stat(join(dir, e))).isFile()) files.push(e.replaceAll('\\', '/'));
  }
  files.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  const hash = createHash('sha256');
  for (const rel of files) {
    hash.update(rel + '\0');
    await feedFile(hash, join(dir, rel));
    hash.update('\0');
  }
  return hash.digest('hex');
}

/**
 * @param {string} root
 * @returns {Promise<Record<Part, string | null>>}
 */
export async function hashParts(root) {
  /** @type {Record<string, string | null>} */
  const out = {};
  for (const part of PARTS) out[part] = await hashPart(root, part);
  return /** @type {Record<Part, string | null>} */ (out);
}

/**
 * Parts the release names whose install hash differs or is missing.
 * @param {Record<Part, string | null>} install
 * @param {Partial<Record<Part, string>>} release
 * @returns {Part[]}
 */
export function diffParts(install, release) {
  return PARTS.filter((p) => release[p] !== undefined && install[p] !== release[p]);
}
