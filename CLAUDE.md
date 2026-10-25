# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@ARCHITECTURE.md

## Keeping ARCHITECTURE.md current

- A change to process boundaries, the Node↔supervisor interface, the ffmpeg pipeline shape, the update/launch mechanism or an external integration updates `ARCHITECTURE.md` in the same commit.
- When a 3.0 part moves from `spikes/` to `src/`, update its proven/design marking there.
- Decisions and their reasoning go on the Notion pages, not in the repo; `ARCHITECTURE.md` names those pages, never links to them.

## Project Overview

StreamLoop restreams a YouTube playlist to Twitch around the clock from one Windows box. This branch holds 3.0: a Node.js/TypeScript backend (`src/server/`) drives a Rust supervisor (`supervisor/`), which plays the playlist through ffmpeg and pushes RTMP itself. There is no mpv or OBS. Node owns the config, the dashboard, Discord alerts, the Twitch checks and the updater. 2.x (mpv + OBS) lives on the `2.x` branch.

## Setup

```bash
npm install
cp config.example.json config.json   # Then edit config.json with your settings
```

`config.json` is gitignored to prevent committing credentials. `config.example.json` is the tracked template.

**Dev setup with the supervisor.** In dev, Node reads `config.json` from its working directory, and the supervisor reads `ROOT\app\config.json`. They must be the same file, so run Node from `ROOT\app`:

1. `cargo build --release` in `supervisor/`.
2. `node supervisor/scripts/dev-root.mjs` builds a dev root in the install layout (default `%TEMP%\streamloop-devroot`); put `supervisor\target\release\streamloop-supervisor.exe` in `ROOT\supervisor\`.
3. Start Node with cwd `ROOT\app` and `STREAMLOOP_ROOT=ROOT`, running the repo's tsx on the repo's `src/server/index.ts` by absolute path. `installRoot()` uses `STREAMLOOP_ROOT` when there is no `node\` folder next to the app.

Node's own logs and timeline still go to the repo's `logs/` in dev, since `logger.ts` and `index.ts` resolve them from the source folder. `supervisor/scripts/node-session.mjs` does all of this for the end-to-end runs.

Test-only environment variables, never set by `config.json` or a release:
- `STREAMLOOP_DEBUG_FAULTS=1`: the launcher starts the supervisor with `--debug-faults` (opens `POST /debug/fault`). The flag belongs to the supervisor process, not to Node: a Node restarted without the variable attaches to the running supervisor, and `/debug/fault` stays open until that supervisor is restarted.
- `STREAMLOOP_TEST_HOOKS=1`: adds `POST /api/test/exit-75`, the update restart without an update.
- `STREAMLOOP_TEST_HLS_URL`: the Twitch read-back reads this HLS instead of the channel's, and the channel counts as live, so no request reaches Twitch and Helix credentials are not needed.

## Commands

```bash
npm start            # Run server (tsx)
npm run dev          # Run with file watching (tsx watch)
npm test             # Run tests once (vitest run)
npm run test:watch   # Run tests in watch mode (vitest)
npm run build:release       # 2.x-layout ZIP (portable Node.js + mpv + yt-dlp/deno + app); no 3.0 packaging yet
npm run build:release:slim  # 2.x-layout update ZIP: app + yt-dlp only (streamloop-vX.Y.Z-update.zip)
cargo build --release       # in supervisor/: the supervisor binary
node supervisor/scripts/node-session.mjs A|B|C|treekill   # end-to-end runs of Node + supervisor against local MediaMTX
```

No linter is configured. TypeScript strict mode is enabled via `tsconfig.json`.

## Release Checklist

Packaging for 3.0 is not done yet. The build scripts still produce the 2.x layout (`node\`, `mpv\`, `yt-dlp\`, `app\`, `START.bat`), with no `supervisor\`, `ffmpeg\` or `pot\`, so this branch cannot build a 3.0 release yet. The checklist below describes the scripts as they are.

Before pushing and building a release:

1. Bump the version in `package.json`. The version is read at startup and used by the updater to compare against GitHub Releases.
2. Update `RELEASE_NOTES.md` with the new version's changelog. This file is used when creating GitHub Releases.
3. After pushing, run `npm run build:release` to create the distributable ZIP in `dist/`. Both build modes also emit a matching `.sha256` (format: `<hash>  <filename>`), so the manual `certutil` step is optional.
4. **Build both bundles.** Run `npm run build:release` (full: `streamloop-vX.Y.Z.zip`) **and** `npm run build:release:slim` (`streamloop-vX.Y.Z-update.zip`, app + yt-dlp only). Always ship the full ZIP — first-time installs and reinstalls need `node/` + `mpv/`, which the updater never delivers, so a release whose only download can't be installed fresh is a footgun. The slim ZIP is an **update-only supplement, not a standalone release**: the updater prefers it so existing installs get a much smaller download. (Slim-only is acceptable solely for a throwaway hotfix where every target already has a working full install at a compatible base and the release touches neither Node nor mpv.)
5. Create the GitHub release with `gh release create` and upload **both ZIPs plus their matching `.sha256`**. The updater matches each checksum to its ZIP by name and verifies integrity against the chosen `.sha256` asset.
6. **3.x releases are never marked latest.** Create them with `gh release create ... --latest=false`, and also `--prerelease` while 3.0 is on trial. 2.x installs older than 2.3.7 still read GitHub's "latest release" and would otherwise swap 3.0 into a 2.x folder; 2.3.7 and later only take releases with their own major version.

## Architecture

**Node backend + supervisor + admin dashboard.** `ARCHITECTURE.md` has the diagram, the supervisor's interface and the main flows.

- **Server** (`src/server/`): Express app on 127.0.0.1:7654. `index.ts` wires the components by dependency injection.
- **Supervisor** (`supervisor/`, Rust): launched by Node through `streamloop-supervisor.exe launch`, a short-lived intermediate, so it is a sibling with a dead parent: a Node crash, restart, update or tree kill leaves the stream up. Node talks to it over HTTP on 127.0.0.1:4417 and reads its events from `ROOT\data\logs\events-DATE.jsonl`.
- **Admin Dashboard** (`src/admin/`): first-run wizard, status, timeline, transport (next, previous, restart, jump), Stop and Start stream, settings, webhooks, logs, update button.

**Server modules:**

| Module | Responsibility |
|--------|---------------|
| `config.ts` / `types.ts` | Loads and saves `config.json` with Zod validation and defaults, including the `supervisor` section the supervisor reads |
| `supervisor-client.ts` | HTTP client for the supervisor (`/status`, `/control`, `/reload`, `/shutdown`); `protectStreamKey` runs `streamloop-supervisor.exe protect` so the key is stored only as a DPAPI blob |
| `supervisor-launcher.ts` | `installRoot()`; polls `/status` every 2 s, launches the supervisor after 3 misses (backoff 10 s, 30 s, 60 s, 5 min), stays down after Stop stream until Start, across Node restarts (`ROOT\data\stopped-by-user`) |
| `supervisor-events.ts` | Tails the supervisor's events file; `describe()` maps each event to timeline text and an optional Discord alert, `AlertGate` sends one alert per outage |
| `event-log.ts` / `event-store.ts` | The 100-entry dashboard timeline, persisted as daily JSONL in `logs/` |
| `twitch.ts` | Helix check that the channel is live while the encoder runs; restarts the encoder at most once per 30 min, then only alerts |
| `twitch-readback.ts` | Every 5 min, two frames of the channel's own HLS 5 s apart; an overlay clock that stands still twice asks for `restart-encoder` |
| `discord.ts` | Webhook notifications with templates, batched over 5 s, criticals sent at once |
| `playlist-metadata.ts` | Playlist video list for the dashboard, fetched with Node's own yt-dlp and cached |
| `api.ts` | REST endpoints (`/api/status`, `/api/state`, `/api/events`, `/api/config`, `/api/stream-key`, `/api/player/*`, `/api/stream/stop`, `/api/stream/start`, `/api/playlist/*`, `/api/logs`, `/api/update/*`, `/api/autostart`, `/api/discord/*`) |
| `updater.ts` / `ytdlp-updater.ts` | GitHub Releases updater; yt-dlp self-update |
| `spawn-cwd.ts` | The temp folder every child process starts in |
| `logger.ts` | Pino structured logging |

**Configuration hot-reload:** `POST /api/config` saves `config.json`, rebuilds Discord and the Twitch checks, and asks the supervisor to `POST /reload`. The reply says when a change (ingest, key, encoder, freeze or silence duration) needs a supervisor restart.

## Testing

Tests live in `src/server/__tests__/` using Vitest. Tests use a `.test-tmp` directory for file fixtures. All server modules use dependency injection for mock-friendly testing.

## Platform Notes

Windows-specific: The `/api/autostart` endpoint manages a VBS script in the Windows Startup folder. The release build (`build/prepare-release.js`) has not been changed for 3.0 yet: it still bundles portable Node.js v22.23.4 win-x64, mpv (from `build/mpv/`) and yt-dlp + deno into the 2.x layout, and nothing from `supervisor/`, ffmpeg or the token server. The `--slim` flag (`npm run build:release:slim`) skips node/ and mpv/ and uses the build machine's own `npm` for the dependency install.

ES modules throughout (`"type": "module"` in package.json, `"module": "ES2022"` in tsconfig).

## Auto-Updater

The updater checks GitHub Releases for `Sponsorn/streamloop` and supports one-click updates from the admin dashboard.

**API endpoints:**
- `GET /api/update/status` — current update status (version info, availability, progress)
- `POST /api/update/check` — manually trigger a version check (60s cooldown)
- `POST /api/update/apply` — download, extract, swap, and restart

**Release selection:** `pickRelease()` takes only releases with the running major version. 2.x takes stable releases; 3.x also takes prereleases, since 3.x ships as `--prerelease` while on trial. `compareVersions()` sorts a suffixed version such as `3.0.0-dev` below its release.

**Asset selection:** `selectReleaseAssets()` picks the release ZIP, preferring a slim `-update.zip` over the full bundle so updates download less. The matching `.sha256` is resolved by name (accepts `<file>.zip.sha256` or `<file>.sha256`) so a release carrying both bundles can't cross checksums. `stageUpdate()` stages only `app/` and `START.bat`; `yt-dlp/`, `node/` and `mpv/` in the ZIP are ignored, because the 3.0 supervisor holds `yt-dlp\` and its token server holds `node\`. When the release's `node-version.txt` names another Node than the running one, the status carries `nodeNotice`, which points at the full ZIP.

**Restart mechanism:** After applying an update, the server exits with **code 75** (`process.exit(75)` in `index.ts` after a graceful shutdown — stop the update check, the Twitch checks, the launcher and the event tail, close the HTTP server; the supervisor and the stream keep running). 75 is a sentinel, not a failure: `START.bat` reads `%ERRORLEVEL%` after the server process ends and branches on it — `75` = "exited on purpose to apply a staged update", `0` = clean shutdown (stay down), anything else = crash (auto-restart after 10s). The number is arbitrary-but-conventional (75 = `EX_TEMPFAIL` in BSD sysexits.h); what matters is node never emits it on a normal or crashing exit, so it can't be confused with either. On code 75, `START.bat` swaps `app/` → `_update_old/` and `_update_tmp/app/` → `app/`, also swaps `yt-dlp/` if the update bundled new binaries (retry-and-rollback, like the app swap), copies `config.json`, `state.json`, and `logs/` from the old app, verifies the new app is valid, and re-launches the server. On swap failure, it rolls back by renaming `_update_old` back to `app`.

**Launcher self-update (trampoline).** `START.bat` must never overwrite itself in place: cmd.exe reads a running batch file from disk by byte offset, so replacing it mid-run desyncs the read position and usually closes the window before the relaunch (the classic "update applied, terminal never reappears" hang). Instead, when the update ships a `START.bat` that differs from the running one (`fc /b` compare), the new launcher is staged to `START.bat.new`. At the end of the code-75 block, if `START.bat.new` exists, `START.bat` writes a detached one-shot `_apply_launcher.bat`, `start`s it in a **separate** process, and `exit /b 0`s. That trampoline waits ~2s for the original window to close, `move`s `START.bat.new` over `START.bat` while nothing holds it, relaunches in a fresh "StreamLoop" window (so preflight re-runs), then self-deletes. Updates that don't change the launcher take the plain `goto loop` in-place restart. Note the release build ships `START.bat` in **every** ZIP (full and slim), so the `fc /b` content check — not "did the ZIP include a launcher" — is what gates the trampoline.

**Half-swap self-heal.** The `app/` swap is non-atomic (`rename app _update_old`, then `move _update_tmp\app app`). If the process dies between those steps there is no `app/` at all, which would brick every future launch at preflight. `START.bat` guards against this at the top (before preflight): if `app/src/server/index.ts` is missing, it restores `app/` from `_update_old/`, or promotes `_update_tmp\app` if that's all that survived.

**Safety guards:**
- Concurrent `downloadAndApply()` calls are rejected (prevents double-click corruption)
- `_update_tmp/` is cleaned up on download/extraction failure
- `START.bat` verifies swap success before cleanup; rolls back on failure
- `START.bat` never replaces itself in place — launcher updates hand off to a detached trampoline (see above)
- `START.bat` self-heals a half-swapped `app/` at startup before preflight
- Inside `START.bat`'s parenthesized blocks, comments are `rem` without parentheses, never `::`. cmd parses the whole exit-code block on every server exit, and a `::` line in it aborts the launcher (window closes, no crash restart). `launcher.test.ts` enforces this.
- Every process the server starts gets `cwd: SPAWN_CWD` (`spawn-cwd.ts`, the temp folder). A child that inherits `cwd=app\` and outlives the server (browser, yt-dlp) blocks the `rename app _update_old` step. `spawn-cwd.test.ts` fails when a spawn site lacks a `cwd`.
- Dashboard `waitForRestart()` times out after 60s instead of looping forever
- Checksum verification uses streaming hash (no full ZIP in memory)

**Dev mode:** When the `node/` sibling directory doesn't exist (i.e., running via `npm start`), the updater operates in check-only mode — it can detect available updates but won't download or apply them.

**Config options:** `autoUpdateCheck` (boolean, default true) and `updateCheckIntervalMs` (number, default 21600000 = 6 hours).
