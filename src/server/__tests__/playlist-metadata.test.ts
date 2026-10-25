import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { parsePlaylistOutput, PlaylistMetadataCache } from '../playlist-metadata.js';

const execFileMock = vi.hoisted(() => vi.fn());
vi.mock('child_process', async (importOriginal) => ({
  ...(await importOriginal<typeof import('child_process')>()),
  execFile: execFileMock,
}));

describe('parsePlaylistOutput', () => {
  it('parses valid JSON lines into PlaylistVideo array with correct indices', () => {
    const output = [
      JSON.stringify({ id: 'abc123', title: 'Video One', duration: 120 }),
      JSON.stringify({ id: 'def456', title: 'Video Two', duration: 300 }),
      JSON.stringify({ id: 'ghi789', title: 'Video Three', duration: 60 }),
    ].join('\n');

    const result = parsePlaylistOutput(output);

    expect(result).toHaveLength(3);
    expect(result[0]).toEqual({ index: 0, id: 'abc123', title: 'Video One', duration: 120 });
    expect(result[1]).toEqual({ index: 1, id: 'def456', title: 'Video Two', duration: 300 });
    expect(result[2]).toEqual({ index: 2, id: 'ghi789', title: 'Video Three', duration: 60 });
  });

  it('handles missing/null fields with default values', () => {
    const output = [
      JSON.stringify({ id: 'abc123' }),
      JSON.stringify({ id: 'def456', title: null, duration: null }),
      JSON.stringify({ title: 'No ID', duration: 90 }),
    ].join('\n');

    const result = parsePlaylistOutput(output);

    expect(result).toHaveLength(3);
    expect(result[0]).toEqual({ index: 0, id: 'abc123', title: '', duration: 0 });
    expect(result[1]).toEqual({ index: 1, id: 'def456', title: '', duration: 0 });
    expect(result[2]).toEqual({ index: 2, id: '', title: 'No ID', duration: 90 });
  });

  it('skips invalid JSON lines silently', () => {
    const output = [
      JSON.stringify({ id: 'abc123', title: 'Valid One', duration: 60 }),
      'not valid json',
      '{broken json',
      JSON.stringify({ id: 'def456', title: 'Valid Two', duration: 120 }),
      '',
      '   ',
    ].join('\n');

    const result = parsePlaylistOutput(output);

    expect(result).toHaveLength(2);
    expect(result[0]).toEqual({ index: 0, id: 'abc123', title: 'Valid One', duration: 60 });
    expect(result[1]).toEqual({ index: 1, id: 'def456', title: 'Valid Two', duration: 120 });
  });

  it('returns empty array for empty or whitespace-only input', () => {
    expect(parsePlaylistOutput('')).toEqual([]);
    expect(parsePlaylistOutput('   ')).toEqual([]);
    expect(parsePlaylistOutput('\n\n\n')).toEqual([]);
    expect(parsePlaylistOutput('  \n  \n  ')).toEqual([]);
  });
});

describe('PlaylistMetadataCache argv', () => {
  it('omits cookies flag when not configured', () => {
    const cache = new PlaylistMetadataCache('/tmp/yt-dlp');
    const argv = (cache as any).buildArgv('PL123');
    expect(argv).not.toContain('--cookies-from-browser');
  });

  it('includes cookies flag when configured', () => {
    const cache = new PlaylistMetadataCache('/tmp/yt-dlp', 'brave');
    const argv = (cache as any).buildArgv('PL123');
    const idx = argv.indexOf('--cookies-from-browser');
    expect(idx).toBeGreaterThanOrEqual(0);
    expect(argv[idx + 1]).toBe('brave');
  });

  it('includes cookies flag with profile spec', () => {
    const cache = new PlaylistMetadataCache('/tmp/yt-dlp', 'chrome:Profile 2');
    const argv = (cache as any).buildArgv('PL123');
    const idx = argv.indexOf('--cookies-from-browser');
    expect(idx).toBeGreaterThanOrEqual(0);
    expect(argv[idx + 1]).toBe('chrome:Profile 2');
  });
});

describe('PlaylistMetadataCache failures', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    execFileMock.mockReset();
    execFileMock.mockImplementation((_f: string, _a: string[], _o: object, cb: (e: Error) => void) => cb(new Error('Sign in to confirm you are not a bot')));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('returns the cached error for 5 min without running yt-dlp again', async () => {
    // Arrange
    const cache = new PlaylistMetadataCache('/tmp/yt-dlp');
    await expect(cache.fetch('PL123')).rejects.toThrow(/bot/);

    // Act
    vi.advanceTimersByTime(4 * 60 * 1000);
    const again = cache.fetch('PL123');

    // Assert
    await expect(again).rejects.toThrow(/bot/);
    expect(execFileMock).toHaveBeenCalledOnce();
  });

  it('runs yt-dlp again after 5 min, and per playlist id', async () => {
    // Arrange
    const cache = new PlaylistMetadataCache('/tmp/yt-dlp');
    await expect(cache.fetch('PL123')).rejects.toThrow();

    // Act
    await expect(cache.fetch('PL999')).rejects.toThrow();
    vi.advanceTimersByTime(5 * 60 * 1000);
    await expect(cache.fetch('PL123')).rejects.toThrow();

    // Assert
    expect(execFileMock).toHaveBeenCalledTimes(3);
  });

  it('clear() drops cached failures, so a yt-dlp update retries at once', async () => {
    // Arrange
    const cache = new PlaylistMetadataCache('/tmp/yt-dlp');
    await expect(cache.fetch('PL123')).rejects.toThrow();

    // Act
    cache.clear();
    await expect(cache.fetch('PL123')).rejects.toThrow();

    // Assert
    expect(execFileMock).toHaveBeenCalledTimes(2);
  });
});
