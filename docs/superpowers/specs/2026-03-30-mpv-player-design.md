# StreamLoop mpv Player Backend + Playlist Selector

**Date:** 2026-03-30
**Branch:** `feature/mpv-player`
**Status:** Design

## Problem

The OBS browser source (Chromium/CEF) used to play YouTube playlists leaks memory during long sessions, growing from ~500MB to 1.8GB+ on an 8GB system. This causes periodic playback stalls every ~8 minutes as the system runs out of RAM. Hardware restarts, YouTube Premium, and proactive source refreshes are workarounds, not fixes. The root cause is Chromium's memory behavior in OBS's embedded CEF.

## Solution

Replace the browser-based YouTube IFrame player with mpv controlled via Windows named pipe IPC. mpv uses ~100-200MB RAM with hardware-accelerated decoding. OBS captures the mpv window instead of a browser source. Additionally, add manual playback controls and a playlist/video selector to the admin dashboard.

## Architecture

```
StreamLoop Server ──(named pipe IPC)──► mpv.exe ──(yt-dlp)──► YouTube
       │                                    │
       ├──(REST API)──► Dashboard           ▼
       │   (monitor + controls)        OBS Window Capture
       │                                    │
       └──(OBS WebSocket)──► OBS ───────────┘
```

### Components

| Component | Role |
|---|---|
| `mpv-client.ts` (new) | Spawns mpv, connects via named pipe IPC, sends commands, receives events/properties |
| `recovery.ts` (modified) | 3-step escalation using mpv IPC instead of WebSocket/OBS source manipulation |
| `api.ts` (modified) | New playlist/player control endpoints |
| `index.ts` (modified) | Wire mpv-client instead of websocket |
| `config.ts` (modified) | mpv-related settings |
| `types.ts` (modified) | Remove player WebSocket types, add mpv types |
| `admin.js` + `index.html` (modified) | Playlist selector, playback controls, updated setup wizard |
| `build/prepare-release.js` (modified) | Bundle mpv + yt-dlp in release ZIP |

### Removed Components

| Component | Reason |
|---|---|
| `websocket.ts` | Replaced by mpv named pipe IPC |
| `src/player/player.js` | No browser player needed |
| `src/player/index.html` | No browser player needed |
| `/ws` endpoint | No WebSocket clients |
| `/player/` static serving | No player HTML to serve |

## mpv Process Management (`mpv-client.ts`)

### Spawning

The server spawns mpv as a child process on startup:

```
mpv.exe --idle --no-border --no-osc --osd-level=0
  --geometry=1920x1080+0+0 --hwdec=d3d11va --vo=gpu
  --ytdl-format="bestvideo[height<=?1080]+bestaudio/best"
  --input-ipc-server=\\.\pipe\mpv-streamloop
  --loop-playlist=inf
  --ytdl-raw-options=yes-playlist=
```

Key flags:
- `--idle` keeps mpv running when no file is loaded (controller pattern)
- `--no-border --no-osc --osd-level=0` for clean OBS capture
- `--geometry=WxH+X+Y` configurable via settings for capture alignment
- `--hwdec=d3d11va` hardware decoding via DirectX 11
- `--loop-playlist=inf` loop current playlist indefinitely
- `--ytdl-raw-options=yes-playlist=` treat URLs as playlists

mpv binary path: `<install>/mpv/mpv.exe` (bundled in release).
yt-dlp binary path: `<install>/yt-dlp/yt-dlp.exe` (bundled in release). Passed to mpv via `--script-opts=ytdl_hook-ytdl_path=<path>`.

### IPC Connection

Connect to `\\.\pipe\mpv-streamloop` via Node.js `net.connect()`. Retry with 500ms backoff up to 10 attempts (mpv needs ~1s to create the pipe after launch).

Protocol: JSON messages terminated by `\n`. Each command includes a `request_id` for response correlation.

```typescript
// Send command
{ "command": ["loadlist", "https://youtube.com/playlist?list=PLxxx", "replace"], "request_id": 1 }

// Response
{ "error": "success", "data": null, "request_id": 1 }

// Property change event (observed)
{ "event": "property-change", "id": 1, "name": "time-pos", "data": 72.5 }

// Lifecycle event
{ "event": "end-file", "reason": "error" }
```

### Public Interface

```typescript
class MpvClient {
  // Lifecycle
  start(): Promise<void>           // Spawn mpv + connect IPC
  stop(): Promise<void>            // Graceful quit via IPC, then kill
  restart(): Promise<void>         // Stop + start, resume from state

  // Playback
  loadPlaylist(url: string, index?: number, startTime?: number): Promise<void>
  play(): Promise<void>
  pause(): Promise<void>
  seek(seconds: number): Promise<void>
  next(): Promise<void>
  prev(): Promise<void>
  jumpTo(index: number): Promise<void>

  // State queries
  getPosition(): Promise<number>
  getDuration(): Promise<number>
  isPaused(): Promise<boolean>
  isIdle(): Promise<boolean>
  getPlaylistPos(): Promise<number>
  getPlaylistCount(): Promise<number>
  getMediaTitle(): Promise<string>
  getPlaylist(): Promise<MpvPlaylistEntry[]>

  // Events
  on(event: 'connected', cb: () => void): void
  on(event: 'disconnected', cb: () => void): void
  on(event: 'fileStarted', cb: () => void): void
  on(event: 'fileEnded', cb: (reason: string) => void): void
  on(event: 'error', cb: (msg: string) => void): void

  // Status
  isConnected(): boolean
  isRunning(): boolean
}
```

### Heartbeat Polling

Every 5 seconds (matching current `heartbeatIntervalMs`), poll mpv for:

| Property | Purpose |
|---|---|
| `time-pos` | Current playback position (stall detection) |
| `duration` | Video duration (state persistence) |
| `pause` | Pause state |
| `idle-active` | Whether mpv is idle |
| `playlist-pos` | Current video index |
| `media-title` | Video title |
| `filename` | Video URL/ID |

Feed this data into the recovery engine the same way heartbeat messages are processed today. The recovery engine interface stays the same — it receives position/state updates and triggers recovery when problems are detected.

### Process Crash Handling

Monitor the child process `exit` event. On unexpected exit:
1. Log the exit code and system memory
2. Wait 2 seconds
3. Respawn mpv
4. Resume from last saved state (playlist URL + video index + time position)

## Playlist Metadata Fetching

### Approach

When a playlist is loaded (at startup or on switch), run yt-dlp in metadata-only mode to fetch all video entries:

```
yt-dlp --flat-playlist --dump-json "https://youtube.com/playlist?list=PLxxx"
```

This outputs one JSON object per line, each containing `id`, `title`, `duration`, `url`. Parse and cache in memory. A 200-video playlist takes ~5-10 seconds to resolve.

### Caching

- Cache per playlist ID in a `Map<string, PlaylistMetadata>`
- Invalidate on playlist switch or manual refresh
- Serve from cache on `/api/playlist/videos` requests

### Data Structure

```typescript
interface PlaylistVideo {
  index: number;
  id: string;
  title: string;
  duration: number;  // seconds
}

interface PlaylistMetadata {
  playlistId: string;
  videos: PlaylistVideo[];
  fetchedAt: number;  // timestamp
}
```

## API Endpoints

### New Endpoints

| Method | Path | Body | Response | Purpose |
|---|---|---|---|---|
| GET | `/api/playlist/videos` | `?page=1&perPage=25` | `{ videos: [...], total, page, perPage, currentIndex }` | Paginated video list for current playlist |
| POST | `/api/playlist/switch` | `{ playlistIndex: number }` | `{ ok: true }` | Switch to different configured playlist |
| POST | `/api/player/jump` | `{ index: number }` | `{ ok: true }` | Jump to specific video |
| POST | `/api/player/next` | — | `{ ok: true }` | Next video |
| POST | `/api/player/prev` | — | `{ ok: true }` | Previous video |
| POST | `/api/player/seek` | `{ seconds: number }` | `{ ok: true }` | Seek to position |
| POST | `/api/player/pause` | — | `{ ok: true, paused: boolean }` | Toggle pause |
| POST | `/api/yt-dlp/update` | — | `{ ok: true, version: string }` | Update yt-dlp to latest |

### Modified Endpoints

| Endpoint | Change |
|---|---|
| `GET /api/status` | Add `mpvRunning`, `mpvConnected` fields; remove `playerConnected` |
| `POST /api/config` | Handle mpv-related config changes (geometry, format) — restart mpv if needed |

### Removed Endpoints

None of the existing endpoints are removed. The browser source wizard endpoints stay in the API for backwards compatibility but the wizard UI no longer uses them.

## Recovery Engine Changes

### Escalation Steps

| Step | Action | Timeout to next |
|---|---|---|
| **RetryCurrent** | `mpv.jumpTo(currentIndex)` | `recoveryDelayMs` (5s default) |
| **RestartMpv** | Kill mpv process, respawn, resume from state | 15s |
| **CriticalAlert** | Discord notification, wait 60s, restart sequence | 60s |

**ToggleVisibility is removed** — not applicable to window capture.

### What Stays the Same

- Heartbeat timeout detection (15s default)
- Stall detection (3 consecutive heartbeats with no `time-pos` progress)
- Non-playing detection (6 heartbeats without playing state)
- Quality monitoring (check video resolution from mpv properties)
- Error skip logic (skip after `maxConsecutiveErrors`)
- State persistence (save position every heartbeat, debounced 2s)
- Discord notifications with system memory
- Event logging
- Periodic restart timer (replaces `sourceRefreshIntervalMs` — same concept, restarts mpv instead of refreshing browser source)

### Error Handling

mpv fires `end-file` with `reason: "error"` when a video fails (unavailable, geo-blocked, yt-dlp extraction error). Handle the same as current YouTube error codes 100/101/150: increment `consecutiveErrors`, skip after threshold.

### Multi-Playlist Cycling

When mpv reaches the end of a playlist (all videos played, `end-file` with `reason: "eof"` and `playlist-pos` at last index), load the next configured playlist:

```
nextPlaylistIndex = (currentPlaylistIndex + 1) % config.playlists.length
```

Same cycling logic as current implementation.

## Dashboard Changes

### Monitor Tab — Playback Controls (new section at top)

```
┌─────────────────────────────────────────────────┐
│ Playlist: [Chill Music        ▼]                │
│                                                 │
│    ⏮  ⏸  ⏭         0:42 / 3:27    ━━━━━○───  │
│                                                 │
│ ┌───┬──────────────────────────────┬────────┬──┐│
│ │ # │ Title                        │Duration│  ││
│ ├───┼──────────────────────────────┼────────┼──┤│
│ │ 1 │ Song Name One                │  3:42  │▶ ││
│ │ 2 │ Song Name Two                │  4:15  │▶ ││
│ │►3 │ Currently Playing Track      │  3:27  │▶ ││
│ │ 4 │ Another Song                 │  5:01  │▶ ││
│ │ 5 │ Yet Another Track            │  2:58  │▶ ││
│ ├───┴──────────────────────────────┴────────┴──┤│
│ │        ◄ 1 2 3 [4] 5 6 7 8 ►                ││
│ └──────────────────────────────────────────────┘│
└─────────────────────────────────────────────────┘
```

- **Playlist dropdown:** Lists all configured playlists by name/ID. Switching triggers `POST /api/playlist/switch`.
- **Transport controls:** Previous, Pause/Play toggle, Next. Mapped to the new API endpoints.
- **Seek bar:** Shows current position / duration. Clicking seeks.
- **Video list:** Paginated table (25 per page). Current video highlighted. "Play" button per row triggers `POST /api/player/jump`.
- **Pagination:** Simple page numbers below the table.

### Playback Tab

- Remove "Periodic browser source refresh" dropdown — replaced by "Periodic mpv restart" with same options
- Quality settings remain (enforce via `--ytdl-format`)

### Settings Tab

- Remove "OBS Browser Source Name" field
- Add "mpv window geometry" field (e.g. `1920x1080+0+0`)
- Add "yt-dlp format" field (e.g. `bestvideo[height<=?1080]+bestaudio/best`)
- Add "Update yt-dlp" button

### Setup Wizard

**Step 3 changes from "Browser Source" to "Window Capture":**

1. Display instructions: "StreamLoop uses mpv to play videos. You need to capture the mpv window in OBS."
2. Button: **"Launch test window"** — spawns mpv with a test pattern or short video so the user can see it
3. Instructions: "In OBS, add a Window Capture source. Select the mpv window. Resize to fit your canvas."
4. Button: **"Verify capture"** — checks OBS input list via obs-websocket for a Window Capture source
5. Save the OBS source name for reference

**Step 6 verification checklist updates:**
- Config valid ✓
- OBS connected ✓
- mpv launches successfully ✓ (spawn test)
- yt-dlp available ✓ (run `yt-dlp --version`)
- Window Capture source detected in OBS ✓

## Bundling (Release Build)

### Directory Structure

```
streamloop/
  START.bat
  README.txt
  node/           (Node.js portable — existing)
  mpv/            (new)
    mpv.exe
    mpv.conf      (default config: no-border, osd-level=0, etc.)
  yt-dlp/         (new)
    yt-dlp.exe
  app/            (application source — existing)
```

### Build Script Changes (`build/prepare-release.js`)

Add download steps:
1. Download mpv portable from `https://github.com/shinchiro/mpv-winbuild-cmake/releases` (shinchiro builds are the standard Windows mpv distribution, x86_64 variant). Extract `mpv.exe` and required DLLs.
2. Download yt-dlp.exe from `https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe`
3. Place in `dist/streamloop/mpv/` and `dist/streamloop/yt-dlp/`
4. Write `dist/streamloop/mpv/mpv.conf`:

```ini
no-border
no-osc
osd-level=0
hwdec=d3d11va
vo=gpu
gpu-api=d3d11
ytdl-format=bestvideo[height<=?1080]+bestaudio/best
ytdl-raw-options=yes-playlist=
loop-playlist=inf
keep-open=yes
```

### yt-dlp Updates

New API endpoint `POST /api/yt-dlp/update`:
1. Download latest `yt-dlp.exe` from GitHub releases
2. Replace existing binary
3. Return new version string

Surfaced as a button in the Settings tab. Can also be triggered from Discord or on a schedule in a future iteration.

## Configuration Changes

### New Config Fields

```json
{
  "mpvGeometry": "1920x1080+0+0",
  "mpvYtdlFormat": "bestvideo[height<=?1080]+bestaudio/best",
  "mpvExtraArgs": []
}
```

### Modified Fields

| Field | Change |
|---|---|
| `sourceRefreshIntervalMs` | Renamed conceptually to "periodic mpv restart interval" in the UI. Same config key for backwards compatibility. When > 0, kills and respawns mpv at this interval. |
| `obsBrowserSourceName` | No longer used for recovery. Keep in config for backwards compatibility but not required. |

### Removed Fields

None removed from the schema to avoid breaking existing config files. Unused fields are silently ignored.

## Testing

### Unit Tests

- `mpv-client.test.ts`: Mock the named pipe (Node.js net server), verify command serialization, response parsing, property observation, reconnection logic, process spawn/kill
- `recovery.test.ts`: Update existing tests to use mpv-client mock instead of websocket mock. Verify 3-step escalation. Verify stall detection still works with mpv property data.
- `api.test.ts`: Test new playlist/player endpoints with mocked mpv-client

### Integration Tests

- Spawn real mpv with `--idle`, connect via pipe, send commands, verify responses
- Load a short public YouTube playlist, verify metadata fetch via yt-dlp
- Verify process restart + resume from state

## Migration

This is a **breaking change** for existing users:
- Browser source setup no longer works
- Must set up Window Capture in OBS
- First run after update should detect the change and guide through the new wizard step 3

The setup wizard should detect if the user is migrating (has existing config with `obsBrowserSourceName` set) and show a migration message explaining the switch to mpv + Window Capture.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| mpv GPU driver memory leaks over 24h+ | Periodic restart timer (same as sourceRefreshIntervalMs) |
| yt-dlp breaks with YouTube changes | Update button in dashboard, bundled version tested before release |
| mpv window accidentally minimized/moved | `--ontop` flag, geometry config, dashboard shows mpv status |
| Named pipe connection flaky | Retry with backoff, auto-reconnect on disconnect |
| Playlist metadata fetch slow for large playlists | Background fetch, show loading state in UI, cache results |
| OBS Window Capture less reliable than Browser Source | Document troubleshooting steps, verify in wizard |
