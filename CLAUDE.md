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
npm run build:release       # Full Windows distributable ZIP (portable Node.js + mpv + yt-dlp/deno + app)
npm run build:release:slim  # Update-only ZIP: app + yt-dlp only, no node/ or mpv/ (streamloop-vX.Y.Z-update.zip)
```

No linter is configured. TypeScript strict mode is enabled via `tsconfig.json`.

## Release Checklist

Before pushing and building a release:

1. Bump the version in `package.json`. The version is read at startup and used by the updater to compare against GitHub Releases.
2. Update `RELEASE_NOTES.md` with the new version's changelog. This file is used when creating GitHub Releases.
3. After pushing, run `npm run build:release` to create the distributable ZIP in `dist/`. Both build modes also emit a matching `.sha256` (format: `<hash>  <filename>`), so the manual `certutil` step is optional.
4. **Build both bundles.** Run `npm run build:release` (full: `streamloop-vX.Y.Z.zip`) **and** `npm run build:release:slim` (`streamloop-vX.Y.Z-update.zip`, app + yt-dlp only). Always ship the full ZIP — first-time installs and reinstalls need `node/` + `mpv/`, which the updater never delivers, so a release whose only download can't be installed fresh is a footgun. The slim ZIP is an **update-only supplement, not a standalone release**: the updater prefers it so existing installs get a much smaller download. (Slim-only is acceptable solely for a throwaway hotfix where every target already has a working full install at a compatible base and the release touches neither Node nor mpv.)
5. Create the GitHub release with `gh release create` and upload **both ZIPs plus their matching `.sha256`**. The updater matches each checksum to its ZIP by name and verifies integrity against the chosen `.sha256` asset.

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
| `recovery.ts` | Core heartbeat monitor + layered freeze/stall detectors with in-place URL retry before escalating to RetryCurrent → RestartMpv → CriticalAlert |
| `frame-monitor.ts` | Output-freeze detector: hashes periodic OBS screenshots to catch a frozen streamed picture mpv reports as healthy |
| `event-store.ts` | Persists the recovery-event timeline to daily-rotated JSONL in `logs/` (reloaded on startup) |
| `playlist-metadata.ts` | Fetches playlist video metadata via yt-dlp, caches in memory |
| `obs-client.ts` | OBS WebSocket client for streaming control |
| `discord.ts` | Discord webhook notifications |
| `api.ts` | REST endpoints (`/api/status`, `/api/state`, `/api/config`, `/api/events`, `/api/playlist/*`, `/api/player/*`, `/api/autostart`, `/api/update/*`) |
| `updater.ts` | GitHub Releases auto-updater: checks for new versions, downloads ZIP, swaps app directory |
| `logger.ts` | Pino structured logging |

**Detection layers (recovery.ts).** The server polls mpv every 5s and runs several independent detectors, most of which try an **in-place URL retry** (re-resolve the googlevideo URL at the current position via `reloadIndex` — no mpv restart, so OBS never captures a black screen) before falling back to the full restart sequence:

- **Premature EOF / network error** (`onFileEnded`): a signed-URL expiry or HTTP/TLS error mid-playback retries in place (up to 2× per video) instead of counting as a real error.
- **Stall**: mpv claims to be playing but `time-pos` isn't advancing for 3 consecutive polls.
- **Video freeze (audio alive)**: `estimated-vf-fps < 1` or `video-bitrate ≈ 0` while audio keeps flowing — YouTube serves video/audio as separate DASH streams, so mpv never fires `end-file`. Up to 3 in-place retries, then escalate.
- **Output freeze**: `frame-monitor.ts` hashes OBS screenshots and confirms with a second frame; catches a frozen streamed picture mpv reports as healthy. Shares the video-freeze retry budget.
- **Proactive URL refresh** (`proactiveUrlRefreshMs`, default 5.5h): reload in place before YouTube's ~6h signed-URL expiry, turning an unplanned freeze into a planned rebuffer.
- **Non-playing**: mpv connected but stuck idle/buffering. Escalates after 6 polls — **except** during the post-connect **startup grace** (`initialLoadGraceMs`, default 90s), which suppresses escalation until playback is confirmed once or the window elapses. This stops a slow cold yt-dlp resume from being mistaken for a fault and restarted on every server start. The grace re-applies on every mpv connect (incl. periodic restarts) and logs a one-shot event so the hold-off is visible in the dashboard.

**Escalation sequence:** When a detector escalates, recovery runs RetryCurrent (in-place reload) → RestartMpv (kill + restart process) → CriticalAlert (Discord, wait 60s, restart the sequence). State writes are suppressed during active recovery so a transient `playlist-pos=0` can't overwrite the resume position.

**Periodic mpv restart:** Configurable via `sourceRefreshIntervalMs` (default 30 min). Proactively restarts mpv to prevent memory growth during long sessions. Skips if recovery is in progress or mpv is disconnected.

**Configuration hot-reload:** POST to `/api/config` updates `config.json` and triggers component reloading (OBS reconnect, recovery engine restart) without server restart.

## mpv IPC Protocol (Server ↔ mpv)

Communication via Windows named pipe (`\\.\pipe\mpv-streamloop`) using newline-delimited JSON.

Server sends commands: `loadlist` (playlist URL), `set_property playlist-pos` (jump to video), `seek` (position), `set_property pause` (toggle), `playlist-next`, `playlist-prev`, `get_property` (poll state)

Server polls: `time-pos`, `duration`, `pause`, `idle-active`, `playlist-pos`, `playlist-count`, `media-title`, `filename`, `video-params`, `estimated-vf-fps`, `video-bitrate`, `audio-bitrate` (the last four drive video-freeze detection)

mpv sends events: `start-file`, `end-file` (with reason: eof/error/stop), `file-loaded`, `shutdown`

## Testing

Tests live in `src/server/__tests__/` using Vitest. Tests use a `.test-tmp` directory for file fixtures. All server modules use dependency injection for mock-friendly testing.

## Platform Notes

Windows-specific: The `/api/autostart` endpoint manages a VBS script in the Windows Startup folder. The release build (`build/prepare-release.js`) bundles portable Node.js v22.12.0 win-x64, mpv, and yt-dlp + deno (deno solves YouTube's n-param JS challenge) into a standalone ZIP. mpv.exe must be placed in `build/mpv/` before running the full build. The `--slim` flag (`npm run build:release:slim`) skips node/ and mpv/ and uses the build machine's own `npm` for the dependency install.

ES modules throughout (`"type": "module"` in package.json, `"module": "ES2022"` in tsconfig).

## Auto-Updater

The updater checks GitHub Releases for `Sponsorn/streamloop` and supports one-click updates from the admin dashboard.

**API endpoints:**
- `GET /api/update/status` — current update status (version info, availability, progress)
- `POST /api/update/check` — manually trigger a version check (60s cooldown)
- `POST /api/update/apply` — download, extract, swap, and restart

**Asset selection:** `selectReleaseAssets()` picks the release ZIP, preferring a slim `-update.zip` (app + yt-dlp, no node/mpv) over the full bundle so updates download less. The matching `.sha256` is resolved by name (accepts `<file>.zip.sha256` or `<file>.sha256`) so a release carrying both bundles can't cross checksums. The updater only ever reads `app/`, `yt-dlp/`, and `START.bat` out of the ZIP — `node/` and `mpv/` are ignored even if present.

**Restart mechanism:** After applying an update, the server exits with code 75. `START.bat` detects this exit code, swaps `app/` → `_update_old/` and `_update_tmp/app/` → `app/`, also swaps `yt-dlp/` if the update bundled new binaries (retry-and-rollback, like the app swap), copies `config.json`, `state.json`, and `logs/` from the old app, verifies the new app is valid, and re-launches the server. On swap failure, it rolls back by renaming `_update_old` back to `app`.

**Safety guards:**
- Concurrent `downloadAndApply()` calls are rejected (prevents double-click corruption)
- `_update_tmp/` is cleaned up on download/extraction failure
- `START.bat` verifies swap success before cleanup; rolls back on failure
- Dashboard `waitForRestart()` times out after 60s instead of looping forever
- Checksum verification uses streaming hash (no full ZIP in memory)

**Dev mode:** When the `node/` sibling directory doesn't exist (i.e., running via `npm start`), the updater operates in check-only mode — it can detect available updates but won't download or apply them.

**Config options:** `autoUpdateCheck` (boolean, default true) and `updateCheckIntervalMs` (number, default 21600000 = 6 hours).
