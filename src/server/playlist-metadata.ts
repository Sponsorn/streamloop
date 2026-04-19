import { execFile as execFileCb } from 'child_process';
import { promisify } from 'util';
import { logger } from './logger.js';

const execFile = promisify(execFileCb);

export interface PlaylistVideo {
  index: number;
  id: string;
  title: string;
  duration: number;
}

export interface PlaylistMetadata {
  playlistId: string;
  videos: PlaylistVideo[];
  fetchedAt: number;
}

export function parsePlaylistOutput(output: string): PlaylistVideo[] {
  const videos: PlaylistVideo[] = [];
  let index = 0;

  for (const line of output.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;

    try {
      const obj = JSON.parse(trimmed) as Record<string, unknown>;
      videos.push({
        index: index++,
        id: typeof obj['id'] === 'string' ? obj['id'] : '',
        title: typeof obj['title'] === 'string' ? obj['title'] : '',
        duration: typeof obj['duration'] === 'number' ? obj['duration'] : 0,
      });
    } catch {
      // Skip invalid JSON lines silently
    }
  }

  return videos;
}

const CACHE_TTL_MS = 60 * 60 * 1000; // 1 hour

interface CacheEntry {
  metadata: PlaylistMetadata;
  fetchedAt: number;
}

export class PlaylistMetadataCache {
  private readonly ytdlpPath: string;
  private readonly cookiesFromBrowser: string;
  private readonly cache = new Map<string, CacheEntry>();
  private readonly inFlight = new Map<string, Promise<PlaylistMetadata>>();

  constructor(ytdlpPath: string, cookiesFromBrowser: string = '') {
    this.ytdlpPath = ytdlpPath;
    this.cookiesFromBrowser = cookiesFromBrowser;
  }

  /** Build the argv yt-dlp is invoked with. Exposed via class so tests can inspect. */
  private buildArgv(playlistId: string): string[] {
    const url = `https://www.youtube.com/playlist?list=${playlistId}`;
    const args: string[] = [];
    if (this.cookiesFromBrowser) {
      args.push('--cookies-from-browser', this.cookiesFromBrowser);
    }
    args.push('--flat-playlist', '--dump-json', '--no-warnings', url);
    return args;
  }

  async fetch(playlistId: string): Promise<PlaylistMetadata> {
    const cached = this.cache.get(playlistId);
    if (cached && Date.now() - cached.fetchedAt < CACHE_TTL_MS) {
      return cached.metadata;
    }

    const inflight = this.inFlight.get(playlistId);
    if (inflight) {
      return inflight;
    }

    const promise = this._doFetch(playlistId).finally(() => {
      this.inFlight.delete(playlistId);
    });

    this.inFlight.set(playlistId, promise);
    return promise;
  }

  getCached(playlistId: string): PlaylistMetadata | undefined {
    const entry = this.cache.get(playlistId);
    if (!entry) return undefined;
    if (Date.now() - entry.fetchedAt >= CACHE_TTL_MS) return undefined;
    return entry.metadata;
  }

  invalidate(playlistId: string): void {
    this.cache.delete(playlistId);
  }

  clear(): void {
    this.cache.clear();
  }

  private async _doFetch(playlistId: string): Promise<PlaylistMetadata> {
    logger.info({ playlistId }, 'Fetching playlist metadata via yt-dlp');

    const { stdout } = await execFile(
      this.ytdlpPath,
      this.buildArgv(playlistId),
      { maxBuffer: 50 * 1024 * 1024, timeout: 120_000 },
    );

    const videos = parsePlaylistOutput(stdout);
    const fetchedAt = Date.now();
    const metadata: PlaylistMetadata = { playlistId, videos, fetchedAt };

    this.cache.set(playlistId, { metadata, fetchedAt });
    logger.info({ playlistId, videoCount: videos.length }, 'Playlist metadata cached');

    return metadata;
  }
}
