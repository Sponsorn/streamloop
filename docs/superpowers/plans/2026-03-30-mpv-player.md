# mpv Player Backend + Playlist Selector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the OBS browser source YouTube player with mpv controlled via Windows named pipe IPC, and add manual playback controls + playlist selector to the admin dashboard.

**Architecture:** Server spawns mpv as a child process, communicates via `\\.\pipe\mpv-streamloop` named pipe using JSON IPC. Recovery engine polls mpv properties instead of receiving WebSocket heartbeats. Dashboard gains playlist browser and transport controls via new REST endpoints.

**Tech Stack:** Node.js `net` module for named pipe IPC, mpv with yt-dlp for YouTube playback, existing Express REST API, existing Vitest test framework.

**Spec:** `docs/superpowers/specs/2026-03-30-mpv-player-design.md`

---

## File Structure

### New Files
| File | Responsibility |
|---|---|
| `src/server/mpv-client.ts` | Spawn mpv process, named pipe IPC, command interface, event handling |
| `src/server/__tests__/mpv-client.test.ts` | Unit tests for mpv IPC client |
| `src/server/playlist-metadata.ts` | Fetch playlist video list via yt-dlp, cache in memory |
| `src/server/__tests__/playlist-metadata.test.ts` | Unit tests for metadata fetcher |

### Modified Files
| File | Changes |
|---|---|
| `src/server/types.ts` | Remove WebSocket player/server message types, add mpv types, update RecoveryStep enum |
| `src/server/recovery.ts` | Replace WebSocket dependency with MpvClient, 3-step escalation, poll-based heartbeat |
| `src/server/index.ts` | Wire MpvClient instead of PlayerWebSocket, remove player static serving |
| `src/server/api.ts` | Add playlist/player control endpoints, update status endpoint, remove browser source wizard helpers |
| `src/server/config.ts` | Add mpv config fields (geometry, ytdlFormat) |
| `src/admin/index.html` | Playlist selector UI, transport controls, updated wizard step 3 |
| `src/admin/admin.js` | Playlist selector logic, playback control handlers, updated wizard |
| `src/server/__tests__/recovery.test.ts` | Update tests for mpv-based recovery |
| `build/prepare-release.js` | Bundle mpv + yt-dlp in release ZIP |

### Removed Files
| File | Reason |
|---|---|
| `src/server/websocket.ts` | Replaced by mpv-client.ts |
| `src/player/player.js` | No browser player needed |
| `src/player/index.html` | No browser player needed |

---

## Task 1: Create feature branch and update types

**Files:**
- Modify: `src/server/types.ts`

- [ ] **Step 1: Create feature branch**

```bash
git checkout -b feature/mpv-player
```

- [ ] **Step 2: Update types — remove WebSocket message types, add mpv types**

Replace the player/server message types and update RecoveryStep in `src/server/types.ts`. Keep PersistedState, config types, and Discord types unchanged.

Replace lines 1-96 with:

```typescript
// --- mpv IPC types ---

export interface MpvHeartbeat {
  timePos: number;
  duration: number;
  paused: boolean;
  idle: boolean;
  playlistPos: number;
  playlistCount: number;
  mediaTitle: string;
  filename: string;
}

export interface MpvPlaylistEntry {
  index: number;
  id: string;
  title: string;
  duration: number;
  current?: boolean;
}

// --- Recovery ---

export enum RecoveryStep {
  None = 'none',
  RetryCurrent = 'retryCurrent',
  RestartMpv = 'restartMpv',
  CriticalAlert = 'criticalAlert',
}
```

Keep everything from line 98 onwards (PersistedState, config types, etc.) unchanged.

- [ ] **Step 3: Commit**

```bash
git add src/server/types.ts
git commit -m "feat: update types for mpv IPC, remove WebSocket message types"
```

---

## Task 2: Build MpvClient — process spawning and IPC connection

**Files:**
- Create: `src/server/mpv-client.ts`
- Create: `src/server/__tests__/mpv-client.test.ts`

- [ ] **Step 1: Write test for MpvClient IPC command sending and response parsing**

Create `src/server/__tests__/mpv-client.test.ts`:

```typescript
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { createServer, type Server } from 'net';

// Helper: create a mock named pipe server that echoes responses
function createMockPipe(pipePath: string): { server: Server; received: string[]; respond: (data: string) => void } {
  const received: string[] = [];
  let clientSocket: any = null;
  const server = createServer((socket) => {
    clientSocket = socket;
    socket.on('data', (data) => {
      const lines = data.toString().split('\n').filter(Boolean);
      received.push(...lines);
    });
  });
  server.listen(pipePath);
  const respond = (data: string) => {
    if (clientSocket) clientSocket.write(data + '\n');
  };
  return { server, received, respond };
}

describe('MpvClient', () => {
  const PIPE_PATH = '\\\\.\\pipe\\mpv-test-' + process.pid;
  let mockPipe: ReturnType<typeof createMockPipe>;

  beforeEach(() => {
    mockPipe = createMockPipe(PIPE_PATH);
  });

  afterEach(async () => {
    await new Promise<void>((resolve) => mockPipe.server.close(() => resolve()));
  });

  it('connects to named pipe and sends JSON commands', async () => {
    // Dynamic import to avoid issues before file exists
    const { MpvClient } = await import('../mpv-client.js');
    const client = new MpvClient({ pipePath: PIPE_PATH, spawn: false });
    await client.connect();

    // Send a command — don't await since mock won't respond yet
    const promise = client.command(['get_property', 'time-pos']);

    // Wait for the command to arrive
    await new Promise((r) => setTimeout(r, 100));
    expect(mockPipe.received.length).toBe(1);
    const sent = JSON.parse(mockPipe.received[0]);
    expect(sent.command).toEqual(['get_property', 'time-pos']);
    expect(typeof sent.request_id).toBe('number');

    // Respond
    mockPipe.respond(JSON.stringify({ error: 'success', data: 42.5, request_id: sent.request_id }));
    const result = await promise;
    expect(result).toBe(42.5);

    client.disconnect();
  });

  it('rejects command promise on mpv error response', async () => {
    const { MpvClient } = await import('../mpv-client.js');
    const client = new MpvClient({ pipePath: PIPE_PATH, spawn: false });
    await client.connect();

    const promise = client.command(['get_property', 'nonexistent']);
    await new Promise((r) => setTimeout(r, 100));
    const sent = JSON.parse(mockPipe.received[0]);
    mockPipe.respond(JSON.stringify({ error: 'property not found', data: null, request_id: sent.request_id }));

    await expect(promise).rejects.toThrow('property not found');
    client.disconnect();
  });

  it('emits events for mpv lifecycle events', async () => {
    const { MpvClient } = await import('../mpv-client.js');
    const client = new MpvClient({ pipePath: PIPE_PATH, spawn: false });
    await client.connect();

    const events: any[] = [];
    client.on('fileEnded', (reason: string) => events.push({ type: 'fileEnded', reason }));
    client.on('fileStarted', () => events.push({ type: 'fileStarted' }));

    mockPipe.respond(JSON.stringify({ event: 'start-file' }));
    mockPipe.respond(JSON.stringify({ event: 'end-file', reason: 'eof' }));

    await new Promise((r) => setTimeout(r, 100));
    expect(events).toEqual([
      { type: 'fileStarted' },
      { type: 'fileEnded', reason: 'eof' },
    ]);

    client.disconnect();
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
npm test -- src/server/__tests__/mpv-client.test.ts
```

Expected: FAIL — `mpv-client.js` does not exist.

- [ ] **Step 3: Implement MpvClient**

Create `src/server/mpv-client.ts`:

```typescript
import { connect, type Socket } from 'net';
import { spawn, type ChildProcess } from 'child_process';
import { EventEmitter } from 'events';
import { logger } from './logger.js';

export interface MpvClientOptions {
  mpvPath?: string;
  pipePath?: string;
  mpvArgs?: string[];
  spawn?: boolean;
}

const DEFAULT_PIPE = '\\\\.\\pipe\\mpv-streamloop';

export class MpvClient extends EventEmitter {
  private mpvPath: string;
  private pipePath: string;
  private mpvArgs: string[];
  private shouldSpawn: boolean;
  private process: ChildProcess | null = null;
  private socket: Socket | null = null;
  private buffer = '';
  private requestId = 0;
  private pending = new Map<number, { resolve: (data: unknown) => void; reject: (err: Error) => void }>();
  private connected = false;

  constructor(options: MpvClientOptions = {}) {
    super();
    this.mpvPath = options.mpvPath ?? 'mpv.exe';
    this.pipePath = options.pipePath ?? DEFAULT_PIPE;
    this.mpvArgs = options.mpvArgs ?? [];
    this.shouldSpawn = options.spawn !== false;
  }

  async start(): Promise<void> {
    if (this.shouldSpawn) {
      await this.spawnProcess();
    }
    await this.connect();
  }

  private spawnProcess(): Promise<void> {
    return new Promise((resolve, reject) => {
      const args = [
        '--idle',
        `--input-ipc-server=${this.pipePath}`,
        ...this.mpvArgs,
      ];
      logger.info({ mpvPath: this.mpvPath, args }, 'Spawning mpv');
      this.process = spawn(this.mpvPath, args, {
        stdio: ['ignore', 'pipe', 'pipe'],
        windowsHide: false,
      });

      this.process.on('error', (err) => {
        logger.error({ err }, 'mpv process error');
        this.emit('processError', err);
        reject(err);
      });

      this.process.on('exit', (code) => {
        logger.warn({ code }, 'mpv process exited');
        this.connected = false;
        this.emit('processExit', code);
      });

      // Give mpv time to create the pipe
      setTimeout(resolve, 1500);
    });
  }

  connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      let attempts = 0;
      const maxAttempts = 10;
      const delay = 500;

      const tryConnect = () => {
        const socket = connect(this.pipePath);

        socket.on('connect', () => {
          this.socket = socket;
          this.connected = true;
          this.buffer = '';
          logger.info('Connected to mpv IPC pipe');
          this.emit('connected');
          resolve();
        });

        socket.on('data', (data) => {
          this.buffer += data.toString();
          const lines = this.buffer.split('\n');
          this.buffer = lines.pop()!;
          for (const line of lines) {
            if (!line.trim()) continue;
            this.handleMessage(line);
          }
        });

        socket.on('close', () => {
          this.connected = false;
          this.socket = null;
          this.emit('disconnected');
        });

        socket.on('error', (err) => {
          attempts++;
          if (attempts >= maxAttempts) {
            reject(new Error(`Failed to connect to mpv pipe after ${maxAttempts} attempts`));
          } else {
            setTimeout(tryConnect, delay);
          }
        });
      };

      tryConnect();
    });
  }

  private handleMessage(line: string) {
    try {
      const msg = JSON.parse(line);

      // Command response
      if ('request_id' in msg) {
        const pending = this.pending.get(msg.request_id);
        if (pending) {
          this.pending.delete(msg.request_id);
          if (msg.error === 'success') {
            pending.resolve(msg.data);
          } else {
            pending.reject(new Error(msg.error));
          }
        }
        return;
      }

      // Event
      if (msg.event) {
        switch (msg.event) {
          case 'start-file':
            this.emit('fileStarted');
            break;
          case 'end-file':
            this.emit('fileEnded', msg.reason ?? 'unknown');
            break;
          case 'file-loaded':
            this.emit('fileLoaded');
            break;
          case 'shutdown':
            this.emit('shutdown');
            break;
          case 'property-change':
            this.emit('propertyChange', msg.name, msg.data);
            break;
        }
      }
    } catch (err) {
      logger.error({ line }, 'Failed to parse mpv IPC message');
    }
  }

  command(args: unknown[]): Promise<unknown> {
    return new Promise((resolve, reject) => {
      if (!this.socket || !this.connected) {
        reject(new Error('Not connected to mpv'));
        return;
      }
      const id = ++this.requestId;
      const msg = JSON.stringify({ command: args, request_id: id }) + '\n';
      this.pending.set(id, { resolve, reject });
      this.socket.write(msg);
    });
  }

  // --- Convenience methods ---

  async getProperty(name: string): Promise<unknown> {
    return this.command(['get_property', name]);
  }

  async setProperty(name: string, value: unknown): Promise<void> {
    await this.command(['set_property', name, value]);
  }

  async loadPlaylist(url: string): Promise<void> {
    await this.command(['loadlist', url, 'replace']);
  }

  async loadFile(url: string): Promise<void> {
    await this.command(['loadfile', url, 'replace']);
  }

  async play(): Promise<void> {
    await this.setProperty('pause', false);
  }

  async pause(): Promise<void> {
    await this.setProperty('pause', true);
  }

  async togglePause(): Promise<boolean> {
    const paused = await this.getProperty('pause') as boolean;
    await this.setProperty('pause', !paused);
    return !paused;
  }

  async seek(seconds: number): Promise<void> {
    await this.command(['seek', seconds, 'absolute']);
  }

  async next(): Promise<void> {
    await this.command(['playlist-next']);
  }

  async prev(): Promise<void> {
    await this.command(['playlist-prev']);
  }

  async jumpTo(index: number): Promise<void> {
    await this.setProperty('playlist-pos', index);
  }

  async quit(): Promise<void> {
    try {
      await this.command(['quit']);
    } catch {
      // May fail if already disconnected
    }
  }

  disconnect() {
    if (this.socket) {
      this.socket.destroy();
      this.socket = null;
    }
    this.connected = false;
    // Reject all pending commands
    for (const [, p] of this.pending) {
      p.reject(new Error('Disconnected'));
    }
    this.pending.clear();
  }

  async stop(): Promise<void> {
    try {
      await this.quit();
    } catch { /* ignore */ }
    this.disconnect();
    if (this.process) {
      this.process.kill();
      this.process = null;
    }
  }

  async restart(): Promise<void> {
    await this.stop();
    await this.start();
  }

  isConnected(): boolean {
    return this.connected;
  }

  isRunning(): boolean {
    return this.process !== null && this.process.exitCode === null;
  }

  getProcess(): ChildProcess | null {
    return this.process;
  }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
npm test -- src/server/__tests__/mpv-client.test.ts
```

Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/mpv-client.ts src/server/__tests__/mpv-client.test.ts
git commit -m "feat: add MpvClient with named pipe IPC and process management"
```

---

## Task 3: Build playlist metadata fetcher

**Files:**
- Create: `src/server/playlist-metadata.ts`
- Create: `src/server/__tests__/playlist-metadata.test.ts`

- [ ] **Step 1: Write test for playlist metadata parsing**

Create `src/server/__tests__/playlist-metadata.test.ts`:

```typescript
import { describe, it, expect, vi } from 'vitest';
import { parsePlaylistOutput, type PlaylistVideo } from '../playlist-metadata.js';

describe('parsePlaylistOutput', () => {
  it('parses yt-dlp flat-playlist JSON lines into PlaylistVideo array', () => {
    const output = [
      '{"id": "abc123", "title": "First Song", "duration": 240.5}',
      '{"id": "def456", "title": "Second Song", "duration": 180.0}',
      '{"id": "ghi789", "title": "Third Song", "duration": 300.2}',
    ].join('\n');

    const result = parsePlaylistOutput(output);
    expect(result).toEqual([
      { index: 0, id: 'abc123', title: 'First Song', duration: 240.5 },
      { index: 1, id: 'def456', title: 'Second Song', duration: 180.0 },
      { index: 2, id: 'ghi789', title: 'Third Song', duration: 300.2 },
    ]);
  });

  it('handles missing fields gracefully', () => {
    const output = '{"id": "abc123"}\n{"id": "def456", "title": null, "duration": null}';
    const result = parsePlaylistOutput(output);
    expect(result).toEqual([
      { index: 0, id: 'abc123', title: '', duration: 0 },
      { index: 1, id: 'def456', title: '', duration: 0 },
    ]);
  });

  it('skips invalid JSON lines', () => {
    const output = '{"id": "abc123", "title": "Good", "duration": 100}\nnot json\n{"id": "def456", "title": "Also Good", "duration": 200}';
    const result = parsePlaylistOutput(output);
    expect(result).toHaveLength(2);
    expect(result[0].id).toBe('abc123');
    expect(result[1].id).toBe('def456');
  });

  it('returns empty array for empty output', () => {
    expect(parsePlaylistOutput('')).toEqual([]);
    expect(parsePlaylistOutput('\n\n')).toEqual([]);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

```bash
npm test -- src/server/__tests__/playlist-metadata.test.ts
```

Expected: FAIL — `playlist-metadata.js` does not exist.

- [ ] **Step 3: Implement playlist metadata module**

Create `src/server/playlist-metadata.ts`:

```typescript
import { execFile } from 'child_process';
import { logger } from './logger.js';

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
  const lines = output.split('\n').filter((l) => l.trim());
  for (const line of lines) {
    try {
      const entry = JSON.parse(line);
      videos.push({
        index: videos.length,
        id: entry.id ?? '',
        title: entry.title ?? '',
        duration: entry.duration ?? 0,
      });
    } catch {
      // Skip invalid lines
    }
  }
  return videos;
}

export class PlaylistMetadataCache {
  private cache = new Map<string, PlaylistMetadata>();
  private ytdlpPath: string;
  private fetching = new Map<string, Promise<PlaylistMetadata>>();

  constructor(ytdlpPath: string) {
    this.ytdlpPath = ytdlpPath;
  }

  async fetch(playlistId: string): Promise<PlaylistMetadata> {
    // Return cached if fresh (< 1 hour)
    const cached = this.cache.get(playlistId);
    if (cached && Date.now() - cached.fetchedAt < 3600000) {
      return cached;
    }

    // Deduplicate concurrent fetches
    const existing = this.fetching.get(playlistId);
    if (existing) return existing;

    const promise = this.doFetch(playlistId);
    this.fetching.set(playlistId, promise);
    try {
      const result = await promise;
      return result;
    } finally {
      this.fetching.delete(playlistId);
    }
  }

  private doFetch(playlistId: string): Promise<PlaylistMetadata> {
    const url = `https://www.youtube.com/playlist?list=${playlistId}`;
    logger.info({ playlistId }, 'Fetching playlist metadata via yt-dlp');

    return new Promise((resolve, reject) => {
      execFile(this.ytdlpPath, [
        '--flat-playlist',
        '--dump-json',
        '--no-warnings',
        url,
      ], { maxBuffer: 50 * 1024 * 1024, timeout: 120000 }, (err, stdout, stderr) => {
        if (err) {
          logger.error({ err, stderr }, 'yt-dlp playlist fetch failed');
          reject(err);
          return;
        }
        const videos = parsePlaylistOutput(stdout);
        const metadata: PlaylistMetadata = {
          playlistId,
          videos,
          fetchedAt: Date.now(),
        };
        this.cache.set(playlistId, metadata);
        logger.info({ playlistId, videoCount: videos.length }, 'Playlist metadata fetched');
        resolve(metadata);
      });
    });
  }

  getCached(playlistId: string): PlaylistMetadata | undefined {
    return this.cache.get(playlistId);
  }

  invalidate(playlistId: string) {
    this.cache.delete(playlistId);
  }

  clear() {
    this.cache.clear();
  }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
npm test -- src/server/__tests__/playlist-metadata.test.ts
```

Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/playlist-metadata.ts src/server/__tests__/playlist-metadata.test.ts
git commit -m "feat: add playlist metadata fetcher with yt-dlp and caching"
```

---

## Task 4: Update config for mpv settings

**Files:**
- Modify: `src/server/config.ts`
- Modify: `src/server/types.ts`

- [ ] **Step 1: Add mpv config fields to AppConfig interface**

In `src/server/types.ts`, add after `twitchPollIntervalMs` (line 167):

```typescript
  mpvGeometry: string;
  mpvYtdlFormat: string;
  mpvExtraArgs: string[];
```

- [ ] **Step 2: Add mpv fields to config schema**

In `src/server/config.ts`, add after the `twitchPollIntervalMs` line in `configSchema`:

```typescript
  mpvGeometry: z.string().default('1920x1080+0+0'),
  mpvYtdlFormat: z.string().default('bestvideo[height<=?1080]+bestaudio/best'),
  mpvExtraArgs: z.array(z.string()).default([]),
```

- [ ] **Step 3: Run all tests to verify nothing broke**

```bash
npm test
```

Expected: All tests PASS (test mocks may need the new fields added — update any mock config objects that spread `as AppConfig` to include the new fields with defaults).

- [ ] **Step 4: Fix any test mock configs**

If tests fail, add the new fields to mock config objects in test files. For example, in `src/server/__tests__/recovery.test.ts`, `discord.test.ts`, `twitch.test.ts`, add to their mock config:

```typescript
mpvGeometry: '1920x1080+0+0',
mpvYtdlFormat: 'bestvideo[height<=?1080]+bestaudio/best',
mpvExtraArgs: [],
```

- [ ] **Step 5: Run tests again**

```bash
npm test
```

Expected: All PASS.

- [ ] **Step 6: Commit**

```bash
git add src/server/types.ts src/server/config.ts src/server/__tests__/
git commit -m "feat: add mpv configuration fields (geometry, ytdl-format, extra args)"
```

---

## Task 5: Rewrite recovery engine for mpv

**Files:**
- Modify: `src/server/recovery.ts`
- Modify: `src/server/__tests__/recovery.test.ts`

This is the largest task. The recovery engine replaces its WebSocket dependency with MpvClient and changes from receiving messages to polling mpv properties.

- [ ] **Step 1: Update recovery engine constructor and imports**

In `src/server/recovery.ts`, replace the import for websocket and types:

```typescript
import { freemem, totalmem } from 'os';
import { RecoveryStep, type AppConfig, type MpvHeartbeat } from './types.js';
import type { MpvClient } from './mpv-client.js';
import type { StateManager } from './state.js';
import type { OBSClient } from './obs-client.js';
import type { DiscordNotifier } from './discord.js';
import { logger } from './logger.js';
```

Update the constructor to accept `MpvClient` instead of `PlayerWebSocket`:

```typescript
constructor(
  config: AppConfig,
  mpv: MpvClient,
  state: StateManager,
  obs: OBSClient,
  discord: DiscordNotifier,
)
```

Store `this.mpv = mpv` instead of `this.ws = ws`.

- [ ] **Step 2: Replace start() — use polling instead of message callbacks**

The `start()` method should:
1. Listen for mpv `connected`/`disconnected`/`fileEnded` events
2. Start a heartbeat poll timer that queries mpv properties every `heartbeatIntervalMs`
3. Start the periodic restart timer (replaces source refresh timer)

```typescript
start() {
  this.mpv.on('connected', () => this.onMpvConnect());
  this.mpv.on('disconnected', () => this.onMpvDisconnect());
  this.mpv.on('fileEnded', (reason: string) => this.onFileEnded(reason));
  this.mpv.on('processExit', (code: number | null) => this.onProcessExit(code));

  this.startHeartbeatPoll();
  this.startPeriodicRestartTimer();

  if (this.mpv.isConnected()) {
    this.onMpvConnect();
  }
}
```

- [ ] **Step 3: Implement heartbeat polling**

Replace the heartbeat monitor with a poll that queries mpv:

```typescript
private startHeartbeatPoll() {
  this.heartbeatPollTimer = setInterval(async () => {
    if (!this.mpv.isConnected()) return;
    try {
      const heartbeat = await this.pollMpvState();
      this.lastHeartbeatAt = Date.now();
      this.processHeartbeat(heartbeat);
    } catch (err) {
      logger.debug({ err }, 'mpv poll failed (may be restarting)');
    }
  }, this.config.heartbeatIntervalMs);
}

private async pollMpvState(): Promise<MpvHeartbeat> {
  const [timePos, duration, paused, idle, playlistPos, playlistCount, mediaTitle, filename] =
    await Promise.all([
      this.mpv.getProperty('time-pos').catch(() => 0),
      this.mpv.getProperty('duration').catch(() => 0),
      this.mpv.getProperty('pause').catch(() => false),
      this.mpv.getProperty('idle-active').catch(() => true),
      this.mpv.getProperty('playlist-pos').catch(() => 0),
      this.mpv.getProperty('playlist-count').catch(() => 0),
      this.mpv.getProperty('media-title').catch(() => ''),
      this.mpv.getProperty('filename').catch(() => ''),
    ]);
  return {
    timePos: timePos as number,
    duration: duration as number,
    paused: paused as boolean,
    idle: idle as boolean,
    playlistPos: playlistPos as number,
    playlistCount: playlistCount as number,
    mediaTitle: mediaTitle as string,
    filename: filename as string,
  };
}
```

- [ ] **Step 4: Implement processHeartbeat() — stall detection and state persistence**

Port the existing heartbeat logic. The key mappings:
- `msg.playerState === 1` → `!heartbeat.paused && !heartbeat.idle`
- `msg.currentTime` → `heartbeat.timePos`
- `msg.videoIndex` → `heartbeat.playlistPos`
- `msg.videoDuration` → `heartbeat.duration`
- `msg.videoId` → extract from `heartbeat.filename` (YouTube URL → video ID)
- `msg.videoTitle` → `heartbeat.mediaTitle`

```typescript
private processHeartbeat(hb: MpvHeartbeat) {
  const playing = !hb.paused && !hb.idle && hb.timePos > 0;
  const videoId = this.extractVideoId(hb.filename);

  if (playing) {
    if (Math.abs(hb.timePos - this.lastProgressTime) < 1) {
      this.stalledHeartbeats++;
      if (this.stalledHeartbeats >= RecoveryEngine.STALL_THRESHOLD && this.recoveryStep === RecoveryStep.None) {
        const mem = getSystemMemory();
        const stallMsg = `Player stalled at ${Math.floor(hb.timePos)}s on video #${hb.playlistPos} (${videoId}) — no progress for ${this.stalledHeartbeats} heartbeats (RAM: ${mem.usedGB}/${mem.totalGB}GB, ${mem.usedPercent}%)`;
        logger.warn({ currentTime: hb.timePos, stalledHeartbeats: this.stalledHeartbeats, videoIndex: hb.playlistPos, videoId, systemMemory: mem }, 'Player stalled — video not advancing');
        this.addEvent(stallMsg);
        this.discord.notifyRecovery('Stall detected');
        this.recoveryReason = 'stall';
        this.startRecoverySequence();
      }
    } else {
      this.stalledHeartbeats = 0;
      this.lastProgressTime = hb.timePos;
      if (this.recoveryReason !== 'quality') {
        this.resetRecovery();
      }
    }
  } else {
    this.stalledHeartbeats = 0;
    this.lastProgressTime = hb.timePos;
  }

  // Persist state
  if (this.stalledHeartbeats < RecoveryEngine.STALL_THRESHOLD) {
    const update: Record<string, unknown> = {
      videoIndex: hb.playlistPos,
      videoId,
      videoTitle: hb.mediaTitle,
      videoDuration: hb.duration,
    };
    if (playing || hb.timePos > 0) {
      update.currentTime = hb.timePos;
    }
    this.state.update(update);
  }

  this.totalVideos = hb.playlistCount;
}

private extractVideoId(filename: string): string {
  if (!filename) return '';
  const match = filename.match(/(?:v=|youtu\.be\/|\/watch\?.*v=)([^&\s]+)/);
  return match ? match[1] : filename;
}
```

- [ ] **Step 5: Update recovery escalation — 3 steps instead of 4**

Update `executeStep()`:

```typescript
private async executeStep(step: RecoveryStep) {
  this.recoveryStep = step;
  logger.info({ step }, 'Executing recovery step');
  this.addEvent(`Recovery step: ${step}`);
  await this.discord.notifyRecovery(step);

  switch (step) {
    case RecoveryStep.RetryCurrent: {
      const pos = this.state.get().videoIndex;
      try {
        await this.mpv.jumpTo(pos);
      } catch (err) {
        logger.warn({ err }, 'Retry via mpv failed');
      }
      this.scheduleNextStep(RecoveryStep.RestartMpv, this.config.recoveryDelayMs);
      break;
    }
    case RecoveryStep.RestartMpv: {
      logger.info('Restarting mpv process for recovery');
      this.addEvent('Restarting mpv process');
      try {
        await this.mpv.restart();
        await this.loadCurrentPlaylist();
      } catch (err) {
        logger.warn({ err }, 'mpv restart failed');
      }
      this.scheduleNextStep(RecoveryStep.CriticalAlert, 15000);
      break;
    }
    case RecoveryStep.CriticalAlert: {
      await this.discord.notifyCritical('All recovery steps exhausted. Waiting 60s before retrying.');
      this.recoveryTimer = setTimeout(() => {
        this.recoveryStep = RecoveryStep.None;
        this.startRecoverySequence();
      }, 60000);
      break;
    }
  }
}
```

- [ ] **Step 6: Add loadCurrentPlaylist() helper**

```typescript
async loadCurrentPlaylist() {
  const savedState = this.state.get();
  const playlistIndex = savedState.playlistIndex < this.config.playlists.length
    ? savedState.playlistIndex : 0;
  const playlist = this.config.playlists[playlistIndex];
  const url = `https://www.youtube.com/playlist?list=${playlist.id}`;

  logger.info({ playlistId: playlist.id, videoIndex: savedState.videoIndex, currentTime: savedState.currentTime }, 'Loading playlist in mpv');
  this.addEvent(`Loading playlist ${playlist.name || playlist.id}`);

  await this.mpv.loadPlaylist(url);

  // Wait for playlist to load, then jump to saved position
  setTimeout(async () => {
    try {
      if (savedState.videoIndex > 0) {
        await this.mpv.jumpTo(savedState.videoIndex);
      }
      if (savedState.currentTime > 0) {
        // Small delay to let video load before seeking
        setTimeout(async () => {
          try {
            await this.mpv.seek(savedState.currentTime);
          } catch { /* may fail if video not ready */ }
        }, 3000);
      }
    } catch (err) {
      logger.warn({ err }, 'Failed to resume playlist position');
    }
  }, 2000);
}
```

- [ ] **Step 7: Replace periodic source refresh with periodic mpv restart**

Rename `startSourceRefreshTimer` to `startPeriodicRestartTimer`:

```typescript
private startPeriodicRestartTimer() {
  this.stopPeriodicRestartTimer();
  if (this.config.sourceRefreshIntervalMs <= 0) return;
  const mins = Math.round(this.config.sourceRefreshIntervalMs / 60000);
  const label = mins >= 60 ? `${(mins / 60).toFixed(1)}h` : `${mins}m`;
  logger.info({ intervalMs: this.config.sourceRefreshIntervalMs }, `Periodic mpv restart enabled (every ${label})`);
  this.sourceRefreshTimer = setInterval(async () => {
    if (this.recoveryStep !== RecoveryStep.None) return;
    if (!this.mpv.isConnected()) return;
    const mem = getSystemMemory();
    logger.info({ systemMemory: mem }, 'Periodic mpv restart');
    this.addEvent(`Periodic mpv restart (RAM: ${mem.usedGB}/${mem.totalGB}GB, ${mem.usedPercent}%)`);
    try {
      await this.mpv.restart();
      await this.loadCurrentPlaylist();
    } catch (err) {
      logger.error({ err }, 'Periodic mpv restart failed');
    }
  }, this.config.sourceRefreshIntervalMs);
}
```

- [ ] **Step 8: Handle mpv file-ended events for multi-playlist cycling**

```typescript
private async onFileEnded(reason: string) {
  if (reason === 'error') {
    this.consecutiveErrors++;
    const videoIndex = this.state.get().videoIndex;
    const videoId = this.state.get().videoId;
    logger.error({ videoIndex, videoId }, 'mpv playback error');
    this.addEvent(`Playback error on video #${videoIndex} (${videoId})`);
    await this.discord.notifyError(videoIndex, videoId, 0, this.consecutiveErrors);
    if (this.consecutiveErrors >= this.config.maxConsecutiveErrors) {
      logger.warn('Max consecutive errors reached, skipping');
      try { await this.mpv.next(); } catch { /* ignore */ }
      this.consecutiveErrors = 0;
    }
  } else if (reason === 'eof') {
    this.consecutiveErrors = 0;
  }
}
```

- [ ] **Step 9: Update getStatus() to reflect mpv state**

```typescript
getStatus() {
  const currentState = this.state.get();
  const playlistIndex = currentState.playlistIndex < this.config.playlists.length
    ? currentState.playlistIndex : 0;
  return {
    recoveryStep: this.recoveryStep,
    lastHeartbeatAt: this.lastHeartbeatAt,
    consecutiveErrors: this.consecutiveErrors,
    totalVideos: this.totalVideos,
    uptimeMs: Date.now() - this.startedAt,
    playlistIndex,
    totalPlaylists: this.config.playlists.length,
    currentPlaylistId: this.config.playlists[playlistIndex].id,
    playbackQuality: this.playbackQuality,
    systemMemory: getSystemMemory(),
    mpvConnected: this.mpv.isConnected(),
    mpvRunning: this.mpv.isRunning(),
  };
}
```

- [ ] **Step 10: Update recovery test mocks**

In `src/server/__tests__/recovery.test.ts`, replace the WebSocket mock with an MpvClient mock:

```typescript
function mockMpv() {
  const emitter = new EventEmitter();
  return {
    ...emitter,
    on: emitter.on.bind(emitter),
    emit: emitter.emit.bind(emitter),
    isConnected: vi.fn().mockReturnValue(true),
    isRunning: vi.fn().mockReturnValue(true),
    getProperty: vi.fn().mockResolvedValue(null),
    setProperty: vi.fn().mockResolvedValue(undefined),
    loadPlaylist: vi.fn().mockResolvedValue(undefined),
    jumpTo: vi.fn().mockResolvedValue(undefined),
    seek: vi.fn().mockResolvedValue(undefined),
    next: vi.fn().mockResolvedValue(undefined),
    restart: vi.fn().mockResolvedValue(undefined),
    command: vi.fn().mockResolvedValue(null),
  };
}
```

Update test cases to trigger heartbeat polling by calling the processHeartbeat method directly (or make it accessible for testing), rather than sending WebSocket messages.

- [ ] **Step 11: Run all tests**

```bash
npm test
```

Expected: All PASS.

- [ ] **Step 12: Commit**

```bash
git add src/server/recovery.ts src/server/__tests__/recovery.test.ts
git commit -m "feat: rewrite recovery engine for mpv IPC (3-step escalation, poll-based heartbeat)"
```

---

## Task 6: Wire mpv into server entry point

**Files:**
- Modify: `src/server/index.ts`
- Delete: `src/server/websocket.ts`
- Delete: `src/player/player.js`
- Delete: `src/player/index.html`

- [ ] **Step 1: Update index.ts imports**

Replace:
```typescript
import { PlayerWebSocket } from './websocket.js';
```

With:
```typescript
import { MpvClient } from './mpv-client.js';
import { PlaylistMetadataCache } from './playlist-metadata.js';
import { resolve as resolvePath } from 'path';
```

- [ ] **Step 2: Replace WebSocket initialization with MpvClient**

Replace the WebSocket initialization (line 55) and player static serving (lines 41-42):

Remove:
```typescript
const playerDir = resolve(__dirname, '..', 'player');
app.use(express.static(playerDir));
```

```typescript
const playerWs = new PlayerWebSocket(server);
```

Replace with:
```typescript
// Resolve bundled binary paths (portable release structure)
const installRoot = resolve(__dirname, '..', '..', '..');
const mpvPath = resolve(installRoot, 'mpv', 'mpv.exe');
const ytdlpPath = resolve(installRoot, 'yt-dlp', 'yt-dlp.exe');

const mpv = new MpvClient({
  mpvPath,
  pipePath: '\\\\.\\pipe\\mpv-streamloop',
  mpvArgs: [
    '--no-border',
    '--no-osc',
    '--osd-level=0',
    `--geometry=${config.mpvGeometry}`,
    '--hwdec=d3d11va',
    '--vo=gpu',
    `--ytdl-format=${config.mpvYtdlFormat}`,
    '--loop-playlist=inf',
    '--ytdl-raw-options=yes-playlist=',
    `--script-opts=ytdl_hook-ytdl_path=${ytdlpPath}`,
    ...config.mpvExtraArgs,
  ],
});

const playlistCache = new PlaylistMetadataCache(ytdlpPath);
```

- [ ] **Step 3: Update RecoveryEngine construction**

Replace:
```typescript
let recovery = new RecoveryEngine(config, playerWs, state, obs, discord);
```

With:
```typescript
let recovery = new RecoveryEngine(config, mpv, state, obs, discord);
```

- [ ] **Step 4: Start mpv on server startup**

After the recovery engine is created, add mpv startup:

```typescript
if (!isFirstRun(config)) {
  await mpv.start();
  await obs.connect();
  twitch.start();
}
```

- [ ] **Step 5: Update reloadConfig()**

In `reloadConfig()`, replace `playerWs` references with `mpv`. The recovery engine reconstruction should pass `mpv`:

```typescript
recovery = new RecoveryEngine(config, mpv, state, obs, discord);
```

- [ ] **Step 6: Update stream monitor**

Replace `playerWs.isConnected()` with `mpv.isConnected()`:

```typescript
const startStreamMonitor = () => {
  obs.startStreamMonitor(() => {
    if (!mpv.isConnected()) return false;
    const status = recovery.getStatus();
    const heartbeatAge = Date.now() - status.lastHeartbeatAt;
    return heartbeatAge < config.heartbeatTimeoutMs;
  });
};
```

- [ ] **Step 7: Update shutdown**

Replace `playerWs.close()` with `mpv.stop()`:

```typescript
const shutdown = async () => {
  logger.info('Shutting down...');
  updater.stopAutoCheck();
  twitch.stop();
  recovery.stop();
  state.flush();
  await mpv.stop();
  obs.disconnect();
  server.close(() => {
    logger.info('Server closed');
    process.exit(0);
  });
  setTimeout(() => process.exit(1), 5000);
};
```

- [ ] **Step 8: Update API dependencies**

Replace `playerWs` in apiRouter dependencies with `mpv` and `playlistCache`:

```typescript
const apiRouter = createApiRouter({
  getConfig: () => config,
  getRecovery: () => recovery,
  mpv,
  playlistCache,
  getObs: () => obs,
  state,
  reloadConfig,
  updater,
  triggerRestart,
  getDiscord: () => discord,
  getTwitch: () => twitch,
  apiToken,
});
```

- [ ] **Step 9: Delete removed files**

```bash
rm src/server/websocket.ts src/player/player.js src/player/index.html
```

- [ ] **Step 10: Run tests**

```bash
npm test
```

Fix any remaining references to `PlayerWebSocket` or `playerWs` in test files.

- [ ] **Step 11: Commit**

```bash
git add -A
git commit -m "feat: wire MpvClient into server, remove WebSocket player"
```

---

## Task 7: Add playlist and player control API endpoints

**Files:**
- Modify: `src/server/api.ts`

- [ ] **Step 1: Update ApiDependencies interface**

Replace `playerWs` with `mpv` and `playlistCache`:

```typescript
export interface ApiDependencies {
  getConfig: () => AppConfig;
  getRecovery: () => RecoveryEngine;
  mpv: MpvClient;
  playlistCache: PlaylistMetadataCache;
  getObs: () => OBSClient;
  state: StateManager;
  reloadConfig: () => Promise<void>;
  updater: Updater;
  triggerRestart: () => void;
  getDiscord: () => DiscordNotifier;
  getTwitch: () => TwitchLivenessChecker;
  apiToken: string;
}
```

- [ ] **Step 2: Add GET /api/playlist/videos endpoint**

```typescript
router.get('/playlist/videos', async (req, res) => {
  const page = Math.max(1, Number(req.query.page) || 1);
  const perPage = Math.min(100, Math.max(1, Number(req.query.perPage) || 25));
  const config = deps.getConfig();
  const state = deps.state.get();
  const playlistIndex = state.playlistIndex < config.playlists.length ? state.playlistIndex : 0;
  const playlist = config.playlists[playlistIndex];

  try {
    const metadata = await deps.playlistCache.fetch(playlist.id);
    const start = (page - 1) * perPage;
    const videos = metadata.videos.slice(start, start + perPage);
    res.json({
      videos,
      total: metadata.videos.length,
      page,
      perPage,
      currentIndex: state.videoIndex,
    });
  } catch (err) {
    res.status(500).json({ error: 'Failed to fetch playlist metadata' });
  }
});
```

- [ ] **Step 3: Add POST /api/playlist/switch endpoint**

```typescript
router.post('/playlist/switch', async (req, res) => {
  const { playlistIndex } = req.body as { playlistIndex: number };
  const config = deps.getConfig();
  if (playlistIndex < 0 || playlistIndex >= config.playlists.length) {
    return res.status(400).json({ error: 'Invalid playlist index' });
  }
  const playlist = config.playlists[playlistIndex];
  const url = `https://www.youtube.com/playlist?list=${playlist.id}`;
  deps.state.update({ playlistIndex, videoIndex: 0, currentTime: 0, videoId: '', videoTitle: '' });
  try {
    await deps.mpv.loadPlaylist(url);
    res.json({ ok: true });
  } catch (err) {
    res.status(500).json({ error: 'Failed to switch playlist' });
  }
});
```

- [ ] **Step 4: Add player control endpoints**

```typescript
router.post('/player/jump', async (req, res) => {
  const { index } = req.body as { index: number };
  try {
    await deps.mpv.jumpTo(index);
    res.json({ ok: true });
  } catch (err) {
    res.status(500).json({ error: 'Failed to jump to video' });
  }
});

router.post('/player/next', async (_req, res) => {
  try {
    await deps.mpv.next();
    res.json({ ok: true });
  } catch (err) {
    res.status(500).json({ error: 'Failed to skip to next' });
  }
});

router.post('/player/prev', async (_req, res) => {
  try {
    await deps.mpv.prev();
    res.json({ ok: true });
  } catch (err) {
    res.status(500).json({ error: 'Failed to go to previous' });
  }
});

router.post('/player/seek', async (req, res) => {
  const { seconds } = req.body as { seconds: number };
  try {
    await deps.mpv.seek(seconds);
    res.json({ ok: true });
  } catch (err) {
    res.status(500).json({ error: 'Failed to seek' });
  }
});

router.post('/player/pause', async (_req, res) => {
  try {
    const paused = await deps.mpv.togglePause();
    res.json({ ok: true, paused });
  } catch (err) {
    res.status(500).json({ error: 'Failed to toggle pause' });
  }
});
```

- [ ] **Step 5: Add POST /api/yt-dlp/update endpoint**

```typescript
router.post('/yt-dlp/update', async (_req, res) => {
  const ytdlpPath = deps.playlistCache['ytdlpPath'];
  try {
    const { execFileSync } = await import('child_process');
    execFileSync(ytdlpPath, ['-U'], { timeout: 120000 });
    const version = execFileSync(ytdlpPath, ['--version'], { timeout: 10000 }).toString().trim();
    res.json({ ok: true, version });
  } catch (err) {
    res.status(500).json({ error: 'Failed to update yt-dlp' });
  }
});
```

- [ ] **Step 6: Update GET /api/status**

Replace `playerConnected: playerWs.isConnected()` with:

```typescript
mpvConnected: deps.mpv.isConnected(),
mpvRunning: deps.mpv.isRunning(),
```

- [ ] **Step 7: Run tests**

```bash
npm test
```

- [ ] **Step 8: Commit**

```bash
git add src/server/api.ts
git commit -m "feat: add playlist/player control API endpoints"
```

---

## Task 8: Update admin dashboard — playback controls and playlist selector

**Files:**
- Modify: `src/admin/index.html`
- Modify: `src/admin/admin.js`

- [ ] **Step 1: Add playback controls section to Monitor tab in index.html**

In `src/admin/index.html`, at the top of the Monitor tab content (before the status cards), add:

```html
<!-- Playback Controls -->
<div class="card" style="margin-bottom: 20px;">
  <div class="form-group" style="margin-bottom: 12px;">
    <label for="playlist-select">Playlist</label>
    <select id="playlist-select" class="wh-preview-select"></select>
  </div>
  <div style="display: flex; align-items: center; gap: 12px; margin-bottom: 12px;">
    <button type="button" id="btn-prev" class="btn btn-secondary" title="Previous">&#9198;</button>
    <button type="button" id="btn-pause" class="btn btn-secondary" title="Pause/Resume">&#9208;</button>
    <button type="button" id="btn-next" class="btn btn-secondary" title="Next">&#9197;</button>
    <span id="seek-time" style="font-size: 13px; color: var(--text-dim);">0:00 / 0:00</span>
    <input type="range" id="seek-bar" min="0" max="100" value="0" style="flex: 1;">
  </div>
  <div id="video-list-container">
    <table id="video-list" class="video-table" style="width: 100%; font-size: 13px;">
      <thead><tr><th>#</th><th>Title</th><th>Duration</th><th></th></tr></thead>
      <tbody id="video-list-body"></tbody>
    </table>
    <div id="video-list-pagination" style="text-align: center; margin-top: 8px;"></div>
    <div id="video-list-loading" class="hint" style="text-align: center; padding: 16px;">Loading playlist...</div>
  </div>
</div>
```

- [ ] **Step 2: Add CSS for video table**

Add to the `<style>` section in `index.html`:

```css
.video-table { border-collapse: collapse; }
.video-table th, .video-table td { padding: 6px 8px; text-align: left; border-bottom: 1px solid var(--border); }
.video-table tr.current { background: var(--accent-dim, rgba(59, 130, 246, 0.15)); }
.video-table .btn-play { padding: 2px 8px; font-size: 12px; cursor: pointer; }
```

- [ ] **Step 3: Add playback control handlers in admin.js**

Add the following functions to `admin.js`:

```javascript
// --- Playback Controls ---

let videoListPage = 1;
const VIDEO_LIST_PER_PAGE = 25;

async function loadVideoList(page) {
  videoListPage = page || 1;
  try {
    const data = await api('/api/playlist/videos?page=' + videoListPage + '&perPage=' + VIDEO_LIST_PER_PAGE);
    renderVideoList(data);
  } catch (err) {
    console.error('Failed to load video list:', err);
  }
}

function renderVideoList(data) {
  var tbody = $('#video-list-body');
  var loading = $('#video-list-loading');
  loading.style.display = 'none';

  tbody.innerHTML = '';
  data.videos.forEach(function(v) {
    var tr = document.createElement('tr');
    if (v.index === data.currentIndex) tr.className = 'current';
    var mins = Math.floor(v.duration / 60);
    var secs = Math.floor(v.duration % 60);
    var dur = mins + ':' + (secs < 10 ? '0' : '') + secs;
    tr.innerHTML = '<td>' + (v.index + 1) + '</td>' +
      '<td>' + escapeHtml(v.title || 'Untitled') + '</td>' +
      '<td>' + dur + '</td>' +
      '<td><button class="btn btn-secondary btn-play" data-index="' + v.index + '">&#9654;</button></td>';
    tbody.appendChild(tr);
  });

  // Pagination
  var totalPages = Math.ceil(data.total / data.perPage);
  var pag = $('#video-list-pagination');
  if (totalPages <= 1) { pag.innerHTML = ''; return; }
  var html = '';
  for (var i = 1; i <= totalPages; i++) {
    if (i === data.page) {
      html += '<strong style="margin: 0 4px;">[' + i + ']</strong>';
    } else {
      html += '<a href="#" style="margin: 0 4px;" data-page="' + i + '">' + i + '</a>';
    }
  }
  pag.innerHTML = html;
}

function initPlaybackControls() {
  // Transport buttons
  $('#btn-prev').addEventListener('click', function() { api('/api/player/prev', { method: 'POST' }); });
  $('#btn-next').addEventListener('click', function() { api('/api/player/next', { method: 'POST' }); });
  $('#btn-pause').addEventListener('click', function() { api('/api/player/pause', { method: 'POST' }); });

  // Playlist switcher
  $('#playlist-select').addEventListener('change', function() {
    var idx = Number(this.value);
    api('/api/playlist/switch', { method: 'POST', body: JSON.stringify({ playlistIndex: idx }) })
      .then(function() { loadVideoList(1); });
  });

  // Video list click delegation
  $('#video-list-body').addEventListener('click', function(e) {
    var btn = e.target.closest('.btn-play');
    if (btn) {
      var index = Number(btn.dataset.index);
      api('/api/player/jump', { method: 'POST', body: JSON.stringify({ index: index }) });
    }
  });

  // Pagination click delegation
  $('#video-list-pagination').addEventListener('click', function(e) {
    if (e.target.dataset.page) {
      e.preventDefault();
      loadVideoList(Number(e.target.dataset.page));
    }
  });

  // Seek bar
  var seeking = false;
  var seekBar = $('#seek-bar');
  seekBar.addEventListener('mousedown', function() { seeking = true; });
  seekBar.addEventListener('mouseup', function() {
    seeking = false;
    var seconds = Number(seekBar.value);
    api('/api/player/seek', { method: 'POST', body: JSON.stringify({ seconds: seconds }) });
  });

  // Load initial playlist selector
  loadPlaylistSelector();
  loadVideoList(1);
}

async function loadPlaylistSelector() {
  try {
    var cfg = await api('/api/config');
    var select = $('#playlist-select');
    select.innerHTML = '';
    cfg.playlists.forEach(function(p, i) {
      var opt = document.createElement('option');
      opt.value = i;
      opt.textContent = p.name || p.id;
      select.appendChild(opt);
    });
    var state = await api('/api/state');
    select.value = state.playlistIndex || 0;
  } catch (err) {
    console.error('Failed to load playlist selector:', err);
  }
}
```

- [ ] **Step 4: Update seek bar and time display during polling**

In the existing `pollOnce()` function, after rendering status, add:

```javascript
// Update seek bar
if (stateData && stateData.currentTime && stateData.videoDuration) {
  var seekBar = $('#seek-bar');
  if (!seeking) {
    seekBar.max = Math.floor(stateData.videoDuration);
    seekBar.value = Math.floor(stateData.currentTime);
  }
  var cur = formatTime(stateData.currentTime);
  var dur = formatTime(stateData.videoDuration);
  $('#seek-time').textContent = cur + ' / ' + dur;
}
```

Add `formatTime` helper:

```javascript
function formatTime(seconds) {
  var s = Math.floor(seconds);
  var h = Math.floor(s / 3600);
  var m = Math.floor((s % 3600) / 60);
  var sec = s % 60;
  if (h > 0) return h + ':' + (m < 10 ? '0' : '') + m + ':' + (sec < 10 ? '0' : '') + sec;
  return m + ':' + (sec < 10 ? '0' : '') + sec;
}
```

- [ ] **Step 5: Call initPlaybackControls() from showDashboard()**

In the existing `showDashboard()` function, add `initPlaybackControls()` call.

- [ ] **Step 6: Update status rendering — replace playerConnected with mpvConnected**

In `renderStatus()`, replace references to `playerConnected` with `mpvConnected` and `mpvRunning`.

- [ ] **Step 7: Commit**

```bash
git add src/admin/index.html src/admin/admin.js
git commit -m "feat: add playlist selector and playback controls to dashboard"
```

---

## Task 9: Update setup wizard for mpv + Window Capture

**Files:**
- Modify: `src/admin/index.html`
- Modify: `src/admin/admin.js`

- [ ] **Step 1: Replace wizard step 3 HTML**

Replace the "Browser Source" step 3 content in `index.html` with:

```html
<!-- Step 3: Window Capture -->
<div class="wizard-step" data-step="3">
  <h2>Window Capture Setup</h2>
  <p>StreamLoop uses mpv to play videos. You need to capture the mpv window in OBS.</p>
  <ol>
    <li>Click <strong>"Launch test window"</strong> below to open mpv</li>
    <li>In OBS, add a <strong>Window Capture</strong> source</li>
    <li>Select the <strong>mpv</strong> window from the dropdown</li>
    <li>Resize the capture to fit your canvas</li>
  </ol>
  <div style="margin: 16px 0;">
    <button type="button" id="wiz-launch-mpv" class="btn btn-secondary">Launch test window</button>
    <span id="wiz-mpv-result" style="margin-left: 12px;"></span>
  </div>
  <div style="margin: 16px 0;">
    <button type="button" id="wiz-check-capture" class="btn btn-secondary">Verify capture in OBS</button>
    <span id="wiz-capture-result" style="margin-left: 12px;"></span>
  </div>
</div>
```

- [ ] **Step 2: Update wizard step 3 progress label**

In the progress bar, change "Browser Source" to "Window Capture".

- [ ] **Step 3: Add wizard step 3 handlers in admin.js**

```javascript
$('#wiz-launch-mpv').addEventListener('click', async function() {
  var result = $('#wiz-mpv-result');
  result.textContent = 'Launching...';
  try {
    // The server starts mpv on startup; this just verifies it's running
    var status = await api('/api/status');
    if (status.mpvRunning) {
      result.textContent = 'mpv is running!';
      result.style.color = 'var(--success)';
    } else {
      result.textContent = 'mpv is not running. Check server logs.';
      result.style.color = 'var(--error)';
    }
  } catch (err) {
    result.textContent = 'Failed to check mpv status';
    result.style.color = 'var(--error)';
  }
});

$('#wiz-check-capture').addEventListener('click', async function() {
  var result = $('#wiz-capture-result');
  result.textContent = 'Checking...';
  try {
    var status = await api('/api/status');
    if (status.obsConnected) {
      result.textContent = 'OBS connected! Set up Window Capture manually in OBS.';
      result.style.color = 'var(--success)';
    } else {
      result.textContent = 'OBS not connected. Make sure OBS is running with WebSocket enabled.';
      result.style.color = 'var(--error)';
    }
  } catch (err) {
    result.textContent = 'Failed to check OBS status';
    result.style.color = 'var(--error)';
  }
});
```

- [ ] **Step 4: Update verification step**

In `runVerification()`, replace `playerConnected` checks with `mpvRunning` and `mpvConnected`.

- [ ] **Step 5: Commit**

```bash
git add src/admin/index.html src/admin/admin.js
git commit -m "feat: update setup wizard for mpv + Window Capture"
```

---

## Task 10: Update release build to bundle mpv + yt-dlp

**Files:**
- Modify: `build/prepare-release.js`

- [ ] **Step 1: Add mpv and yt-dlp download URLs**

After the Node.js constants at the top of `prepare-release.js`, add:

```javascript
const MPV_URL = 'https://github.com/shinchiro/mpv-winbuild-cmake/releases/latest/download/mpv-x86_64-20250101-git-abcdef.7z';
// Note: The exact URL changes per release. Use a pinned version or latest redirect.
// Alternative: download from sourceforge
const YTDLP_URL = 'https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe';
```

**Important:** The mpv shinchiro builds use 7z archives. We need to either:
- Use a pre-extracted mpv.exe or
- Add 7z extraction to the build script
- Or use the mpv.net releases which provide a ZIP

For simplicity, pin a specific mpv build URL that provides a ZIP or directly include mpv.exe. The exact URL should be determined at build time. Add a comment noting this.

- [ ] **Step 2: Add mpv and yt-dlp download steps**

After the Node.js download/extraction step, add:

```javascript
// Step 2: Download yt-dlp
console.log('Downloading yt-dlp...');
mkdirSync(join(RELEASE, 'yt-dlp'), { recursive: true });
await downloadFile(YTDLP_URL, join(RELEASE, 'yt-dlp', 'yt-dlp.exe'));

// Step 3: Download mpv
console.log('Downloading mpv...');
mkdirSync(join(RELEASE, 'mpv'), { recursive: true });
// Download and extract mpv portable build
// For now, expects mpv.exe to be placed in build/mpv/ manually or via CI
const localMpv = join(ROOT, 'build', 'mpv', 'mpv.exe');
if (existsSync(localMpv)) {
  cpSync(localMpv, join(RELEASE, 'mpv', 'mpv.exe'));
} else {
  console.warn('WARNING: mpv.exe not found at build/mpv/mpv.exe — download manually from https://github.com/shinchiro/mpv-winbuild-cmake/releases');
}

// Write default mpv.conf
writeFileSync(join(RELEASE, 'mpv', 'mpv.conf'), [
  'no-border',
  'no-osc',
  'osd-level=0',
  'hwdec=d3d11va',
  'vo=gpu',
  'gpu-api=d3d11',
  'ytdl-format=bestvideo[height<=?1080]+bestaudio/best',
  'ytdl-raw-options=yes-playlist=',
  'loop-playlist=inf',
  'keep-open=yes',
].join('\n'));
```

- [ ] **Step 3: Remove player file copying**

The build script currently copies `src/player/`. Since we've deleted those files, the copy step will naturally skip them (or fail if the directory doesn't exist). Add a check:

Remove the player directory from the source copy if it still references it.

- [ ] **Step 4: Commit**

```bash
git add build/prepare-release.js
git commit -m "feat: update release build to bundle mpv + yt-dlp"
```

---

## Task 11: Clean up and final integration test

**Files:**
- All modified files

- [ ] **Step 1: Remove ws dependency from package.json**

The `ws` package is no longer needed since we removed the WebSocket server.

```bash
npm uninstall ws @types/ws
```

- [ ] **Step 2: Run all tests**

```bash
npm test
```

Expected: All PASS.

- [ ] **Step 3: Verify TypeScript compiles**

```bash
npx tsc --noEmit
```

Expected: No errors.

- [ ] **Step 4: Manual smoke test**

Start the dev server and verify:
- mpv launches and connects via IPC
- Dashboard loads and shows mpv status
- Playlist videos appear in the video list
- Transport controls (play/pause/next/prev) work
- Seek bar updates

```bash
npm run dev
```

- [ ] **Step 5: Commit any remaining fixes**

```bash
git add -A
git commit -m "chore: clean up ws dependency, final integration fixes"
```

- [ ] **Step 6: Update CLAUDE.md architecture section**

Update the Architecture section in CLAUDE.md to reflect the new mpv-based architecture. Replace references to browser source, WebSocket player, and player.js with mpv IPC descriptions.

- [ ] **Step 7: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md for mpv architecture"
```
