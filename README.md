# StreamLoop

24/7 YouTube playlist streamer for OBS with automatic recovery. StreamLoop plays a YouTube playlist in **mpv** (resolved via yt-dlp) and OBS captures the mpv window with a **Window Capture** source. A background server watches for playback freezes, stalls, network errors, and signed-URL expiry, and recovers automatically — preferring an in-place URL reload (so the stream never shows a black screen) before escalating to an mpv restart. It can optionally drive OBS streaming, send Discord alerts, and cross-check Twitch liveness.

> **Windows only.** StreamLoop controls mpv over a Windows named pipe, captures the mpv window in OBS, and uses a VBS script for autostart.

## How it works

```
yt-dlp ──resolves──> mpv (plays playlist) ──Window Capture──> OBS ──streams──> YouTube/Twitch/…
                       ▲                                        ▲
                       └──────── StreamLoop server ────────────┘
                         named-pipe IPC + OBS WebSocket
                         (heartbeat monitor, recovery, dashboard)
```

The server polls mpv every few seconds and runs several independent freeze/stall detectors. Most try an **in-place URL retry** (re-resolve the video at the current position — no mpv restart) before falling back to the full restart sequence: RetryCurrent → RestartMpv → CriticalAlert.

## Features

- **Automatic recovery** from stalls, video-freeze-while-audio-continues, output freezes (detected by hashing OBS screenshots), premature EOF, and YouTube's ~6h signed-URL expiry — in-place reload first, mpv restart only as a fallback
- **Persistent playback position** across restarts (`state.json`)
- **Admin dashboard** for live monitoring, recovery-event history, playback controls (playlist selector, transport, seek, video list), and live config editing — no restart needed
- **Now-playing overlay** for OBS at `/overlay`
- **Discord webhook alerts** with customizable per-event templates
- **OBS streaming control** with optional auto-start and auto-restart on stream drop
- **Twitch liveness check** — restarts the stream if OBS reports streaming but the channel is offline
- **Windows autostart** for unattended operation
- **One-click auto-updater** from the dashboard (GitHub Releases)

## Quick start (end users)

Download the latest `streamloop-vX.Y.Z.zip` from [Releases](https://github.com/Sponsorn/streamloop/releases), extract it anywhere, and run `START.bat`. The bundle is self-contained — it includes portable Node.js, mpv, and yt-dlp, so nothing needs to be installed. On first run, open `http://localhost:7654/admin` and the setup wizard walks you through playlist, OBS, and Discord settings. After that, updates are one click from the dashboard.

## Development setup (from source)

Requires Node.js 22+ and, because StreamLoop drives the real player, local copies of the Windows binaries:

```bash
npm install
cp config.example.json config.json      # then edit config.json
```

Place `mpv.exe` in `mpv/` and `yt-dlp.exe` in `yt-dlp/` at the project root (the server looks there first in dev, then falls back to the portable release layout). `config.json` is gitignored; `config.example.json` is the tracked template.

```bash
npm start       # run the server (tsx)
npm run dev     # run with file watching (tsx watch)
```

## OBS configuration

1. Start StreamLoop — it launches mpv playing your playlist in its own window.
2. In OBS, add a **Window Capture** source targeting the mpv window.
3. Set that source's name as `obsBrowserSourceName` in config (the field name is legacy; it's the source the output-freeze detector screenshots). The setup wizard does this for you.
4. Enable the OBS WebSocket server: **Tools → WebSocket Server Settings → Enable**, then put the URL and password in config so StreamLoop can monitor and control streaming.
5. (Optional) Add a **Browser Source** pointing at `http://localhost:7654/overlay` for a now-playing overlay.

## Admin dashboard

Open `http://localhost:7654/admin` to monitor player status, review recovery events, control playback, and edit every setting live.

## Configuration

`config.json` is validated against a Zod schema with sensible defaults — see [`config.example.json`](config.example.json) for the complete set, and edit most of it live from the dashboard. Key fields:

| Field | Default | Description |
|-------|---------|-------------|
| `playlists` | — | Array of `{ id, name? }` YouTube playlist objects (at least one) |
| `obsBrowserSourceName` | — | Name of the OBS source capturing the mpv window (legacy field name) |
| `port` | `7654` | HTTP server / dashboard port |
| `obsWebsocketUrl` | `ws://127.0.0.1:4455` | OBS WebSocket server URL |
| `obsWebsocketPassword` | `""` | OBS WebSocket password |
| `discord.webhookUrl` | `""` | Discord webhook for alerts (per-event toggles + templates under `discord`) |
| `obsAutoStream` / `obsAutoRestart` | `false` | Auto-start streaming on connect / auto-restart on stream drop |
| `proactiveUrlRefreshMs` | `19800000` (5.5h) | Reload the current video before YouTube's signed-URL expiry; `0` disables |
| `initialLoadGraceMs` | `90000` | Grace window after each mpv connect before the non-playing watchdog escalates |
| `sourceRefreshIntervalMs` | `1800000` (30m) | Periodic proactive mpv restart to curb memory growth; `0` disables |
| `twitchLivenessEnabled` | `false` | Cross-check Twitch liveness (needs `twitchClientId`/`Secret`/`Channel`) |
| `autoUpdateCheck` | `true` | Periodically check GitHub Releases for updates |
| `mpvYtdlFormat` | `bestvideo[height<=?1080]+bestaudio/best` | yt-dlp format string passed to mpv |
| `ytdlCookiesFromBrowser` | `""` | Browser name to pull cookies from (for age/region-gated videos) |

## Windows release build

```bash
npm run build:release        # full self-contained ZIP (node + mpv + yt-dlp/deno + app)
npm run build:release:slim   # update-only ZIP (app + yt-dlp, no node/mpv) — smaller updates
```

Both emit a matching `.sha256`. The full build requires `mpv.exe` in `build/mpv/`. Always ship the full ZIP (first-time installs need `node/` + `mpv/`); the slim ZIP is an update-only supplement the auto-updater prefers so existing installs download less.

## Testing

```bash
npm test             # run once (vitest)
npm run test:watch   # watch mode
```

## License

GPL-3.0-only
