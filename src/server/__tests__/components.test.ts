import { describe, it, expect, beforeEach, afterAll } from 'vitest';
import { mkdirSync, rmSync, writeFileSync, utimesSync } from 'fs';
import { join, dirname } from 'path';
import { hashPart, hashParts, diffParts, foldersFor, PARTS, type Part } from '../components.js';

const base = join(process.cwd(), '.test-tmp', 'components');
let n = 0;
let root: string;

function put(rel: string, content: string) {
  const p = join(root, rel);
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, content);
  return p;
}

describe('components', () => {
  beforeEach(() => {
    root = join(base, String(n++));
    rmSync(root, { recursive: true, force: true });
    mkdirSync(root, { recursive: true });
  });
  afterAll(() => rmSync(base, { recursive: true, force: true }));

  it('gives the same hash for the same files whatever their mtimes or creation order', async () => {
    // Arrange
    const a = put('pot/build/main.js', 'main');
    put('pot/b.txt', 'bee');
    const first = await hashPart(root, 'pot');
    // Act
    rmSync(join(root, 'pot'), { recursive: true });
    put('pot/b.txt', 'bee');
    const again = put('pot/build/main.js', 'main');
    utimesSync(again, new Date(2001, 1, 1), new Date(2001, 1, 1));
    const second = await hashPart(root, 'pot');
    // Assert
    expect(a).toBe(again);
    expect(second).toBe(first);
  });

  it('gives a different hash when one byte of one file changes', async () => {
    put('ffmpeg/ffmpeg.exe', 'abcd');
    const before = await hashPart(root, 'ffmpeg');
    put('ffmpeg/ffmpeg.exe', 'abce');
    expect(await hashPart(root, 'ffmpeg')).not.toBe(before);
  });

  it('gives a different hash when a file is renamed', async () => {
    put('node/a.txt', 'same');
    const before = await hashPart(root, 'node');
    rmSync(join(root, 'node/a.txt'));
    put('node/b.txt', 'same');
    expect(await hashPart(root, 'node')).not.toBe(before);
  });

  it('returns null for a missing part folder', async () => {
    expect(await hashPart(root, 'supervisor')).toBeNull();
    expect((await hashParts(root)).node).toBeNull();
  });

  it('hashes yt-dlp plugins only, so a new yt-dlp.exe or deno.exe changes nothing', async () => {
    put('yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip', 'zip');
    put('yt-dlp/yt-dlp.exe', 'one');
    put('yt-dlp/deno.exe', 'deno1');
    const before = await hashPart(root, 'ytdlp-plugin');
    put('yt-dlp/yt-dlp.exe', 'two');
    put('yt-dlp/deno.exe', 'deno2');
    expect(await hashPart(root, 'ytdlp-plugin')).toBe(before);
    expect(before).not.toBeNull();
  });

  it('diffParts lists changed and missing parts in PARTS order and ignores parts the release does not name', () => {
    const install = { node: 'n', supervisor: null, ffmpeg: 'f', pot: 'p', 'ytdlp-plugin': 'y' } as Record<Part, string | null>;
    const release = { 'ytdlp-plugin': 'y2', supervisor: 's', ffmpeg: 'f', pot: 'p' };
    expect(diffParts(install, release)).toEqual(['supervisor', 'ytdlp-plugin']);
    expect(PARTS.indexOf('supervisor')).toBeLessThan(PARTS.indexOf('ytdlp-plugin'));
  });

  it('foldersFor always includes yt-dlp and maps ytdlp-plugin to it', () => {
    expect(foldersFor(['ffmpeg'])).toEqual(['ffmpeg', 'yt-dlp']);
    expect(foldersFor(['ytdlp-plugin', 'node'])).toEqual(['node', 'yt-dlp']);
  });
});
