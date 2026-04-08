# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

StreamLoop is a Node.js/TypeScript application that plays YouTube playlists via mpv (controlled over Windows named pipe IPC) and automatically recovers from playback freezes, errors, and failures. OBS captures the mpv window via Window Capture. The server connects to OBS via obs-websocket-js and optionally sends alerts to Discord webhooks.

## Setup

```bash
npm install
cp config.example.json config.json   # Then edit config.json with your settings
```

`config.json` is gitignored to prevent committing credentials. `config.example.json` is the tracked template.

## Commands

```bash
npm start            # Run server (tsx)
npm run dev          # Run with file watching (tsx watch)
npm test             # Run tests once (vitest run)
npm run test:watch   # Run tests in watch mode (vitest)
npm run build:release  # Package Windows distributable ZIP with portable Node.js
```

No linter is configured. TypeScript strict mode is enabled via `tsconfig.json`.

## Release Checklist

Before pushing and building a release:

1. Bump the version in `package.json`. The version is read at startup and used by the updater to compare against GitHub Releases.
2. Update `RELEASE_NOTES.md` with the new version's changelog. This file is used when creating GitHub Releases.
3. After pushing, run `npm run build:release` to create the distributable ZIP in `dist/`.
4. Generate a SHA-256 checksum file: `certutil -hashfile dist/streamloop-vX.Y.Z.zip SHA256` and save as `dist/streamloop-vX.Y.Z.zip.sha256` (format: `<hash>  <filename>`).
5. Create the GitHub release with `gh release create`, then upload **both** the `.zip` and `.sha256` files. The updater verifies integrity using the `.sha256` asset.

## Architecture

**Server + mpv player + admin dashboard:**

- **Server** (`src/server/`): Express.js app that orchestrates everything. Entry point is `index.ts`, which wires components together via dependency injection.
- **mpv Player**: Spawned as a child process, controlled via Windows named pipe IPC (`\\.\pipe\mpv-streamloop`). Uses yt-dlp for YouTube playlist/video resolution.
- **Admin Dashboard** (`src/admin/admin.js`): Browser UI for monitoring status, viewing recovery events, playback controls (playlist selector, transport, seek, video list), and editing config via REST API.

**Key server modules:**

| Module | Responsibility |
|--------|---------------|
| `config.ts` | Loads/saves `config.json` with Zod schema validation and defaults |
| `state.ts` | Persists playback position (`state.json`) with debounced 2s writes |
| `mpv-client.ts` | Spawns mpv process, connects via named pipe IPC, sends commands, receives events |
| `recovery.ts` | Core heartbeat monitor with escalating recovery: RetryCurrent → RestartMpv → CriticalAlert |
| `playlist-metadata.ts` | Fetches playlist video metadata via yt-dlp, caches in memory |
| `obs-client.ts` | OBS WebSocket client for streaming control |
| `discord.ts` | Discord webhook notifications |
| `api.ts` | REST endpoints (`/api/status`, `/api/state`, `/api/config`, `/api/events`, `/api/playlist/*`, `/api/player/*`, `/api/autostart`, `/api/update/*`) |
| `updater.ts` | GitHub Releases auto-updater: checks for new versions, downloads ZIP, swaps app directory |
| `logger.ts` | Pino structured logging |

**Recovery escalation sequence:** The server polls mpv properties every 5 seconds. When stalls are detected (no `time-pos` progress for 3 consecutive polls), recovery escalates: retry current video via IPC → kill and restart mpv process → send critical Discord alert, wait 60s, then restart the sequence.

**Periodic mpv restart:** Configurable via `sourceRefreshIntervalMs` (default 30 min). Proactively restarts mpv to prevent memory growth during long sessions. Skips if recovery is in progress or mpv is disconnected.

**Configuration hot-reload:** POST to `/api/config` updates `config.json` and triggers component reloading (OBS reconnect, recovery engine restart) without server restart.

## mpv IPC Protocol (Server ↔ mpv)

Communication via Windows named pipe (`\\.\pipe\mpv-streamloop`) using newline-delimited JSON.

Server sends commands: `loadlist` (playlist URL), `set_property playlist-pos` (jump to video), `seek` (position), `set_property pause` (toggle), `playlist-next`, `playlist-prev`, `get_property` (poll state)

Server polls: `time-pos`, `duration`, `pause`, `idle-active`, `playlist-pos`, `playlist-count`, `media-title`, `filename`

mpv sends events: `start-file`, `end-file` (with reason: eof/error/stop), `file-loaded`, `shutdown`

## Testing

Tests live in `src/server/__tests__/` using Vitest. Tests use a `.test-tmp` directory for file fixtures. All server modules use dependency injection for mock-friendly testing.

## Platform Notes

Windows-specific: The `/api/autostart` endpoint manages a VBS script in the Windows Startup folder. The release build (`build/prepare-release.js`) bundles portable Node.js v22.12.0 win-x64, mpv, and yt-dlp into a standalone ZIP. mpv.exe must be placed in `build/mpv/` before running the build script.

ES modules throughout (`"type": "module"` in package.json, `"module": "ES2022"` in tsconfig).

## Auto-Updater

The updater checks GitHub Releases for `Sponsorn/streamloop` and supports one-click updates from the admin dashboard.

**API endpoints:**
- `GET /api/update/status` — current update status (version info, availability, progress)
- `POST /api/update/check` — manually trigger a version check (60s cooldown)
- `POST /api/update/apply` — download, extract, swap, and restart

**Restart mechanism:** After applying an update, the server exits with code 75. `START.bat` detects this exit code, swaps `app/` → `_update_old/` and `_update_tmp/app/` → `app/`, copies `config.json` and `state.json` from the old app, verifies the new app is valid, and re-launches the server. On swap failure, it rolls back by renaming `_update_old` back to `app`.

**Safety guards:**
- Concurrent `downloadAndApply()` calls are rejected (prevents double-click corruption)
- `_update_tmp/` is cleaned up on download/extraction failure
- `START.bat` verifies swap success before cleanup; rolls back on failure
- Dashboard `waitForRestart()` times out after 60s instead of looping forever
- Checksum verification uses streaming hash (no full ZIP in memory)

**Dev mode:** When the `node/` sibling directory doesn't exist (i.e., running via `npm start`), the updater operates in check-only mode — it can detect available updates but won't download or apply them.

**Config options:** `autoUpdateCheck` (boolean, default true) and `updateCheckIntervalMs` (number, default 21600000 = 6 hours).
