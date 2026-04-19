import { describe, it, expect } from 'vitest';
import { parsePlaylistOutput, PlaylistMetadataCache } from '../playlist-metadata.js';

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
