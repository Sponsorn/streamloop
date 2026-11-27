# StreamLoop

Streams a YouTube playlist to Twitch around the clock from one Windows PC. StreamLoop 3 needs no OBS and no mpv: one program, `streamloop.exe`, downloads the videos with yt-dlp, plays them through ffmpeg and sends the stream to Twitch itself. It retries or skips a video that fails, restarts the encoder when it stops, sends alerts to Discord, and checks that Twitch really shows a moving picture.

> **Windows only.** StreamLoop uses Windows' DPAPI to store the stream key, a Windows Job Object to keep its ffmpeg processes together, and a VBS script for autostart.

## How it works

```
YouTube ──yt-dlp──> download cache ──> feeder ffmpeg ──> encoder ffmpeg ──RTMP──> Twitch
                                         (one video     (one connection
                                          at a time)     for the whole stream)
                         ▲
        streamloop.exe supervisor: plays the playlist, retries, skips, restarts
                         ▲  local HTTP
        streamloop.exe: dashboard, Discord alerts, Twitch checks, updates
```

`streamloop.exe` runs as two processes. The supervisor keeps the pipeline going: one long-lived encoder holds the Twitch connection while a feeder per video hands over to the next, so a new video never drops the stream. The control process serves the dashboard on `http://localhost:7654/admin`, starts the supervisor, and starts it again if it stops. The supervisor does not depend on the control process: the stream keeps running while the control process restarts.

## Features

- **No gaps between videos**: the Twitch connection stays up from one video to the next, and from the end of one playlist to the start of the next
- **Automatic recovery** from a video that fails, stalls or freezes (retried, then skipped), an encoder that stops (restarted) and a supervisor that stops (started again, resuming where it was)
- **Download cache** with a size limit, and a standby slate while nothing is ready
- **Hardware encoding** on NVIDIA, AMD or Intel, with software encoding as the last resort; the first-run check finds the one that works
- **Now-playing overlay** drawn into the stream
- **Dashboard** for status, the event timeline, Stop and Start, skip and jump, playlists, settings and logs
- **Discord webhook alerts** with a template per event
- **Twitch checks** that the channel is live and that its own stream shows a moving picture
- **Windows autostart** for unattended operation
- **Updates from the dashboard**: a new version replaces `streamloop.exe` just before Twitch's 48-hour cut, or at once with Update Now, and the previous version comes back if the new one does not start

## Quick start

1. Download `streamloop-vX.Y.Z.zip` from [Releases](https://github.com/Sponsorn/streamloop/releases). 3.x versions are marked as pre-releases.
2. Make a new, empty folder, for example `C:\StreamLoop3`, and extract the ZIP into it. The ZIP holds one folder, `streamloop`.
3. Double-click `START.bat` in `C:\StreamLoop3\streamloop`.

Everything StreamLoop needs is in the folder: ffmpeg, yt-dlp and the YouTube token server. The dashboard opens at `http://localhost:7654/admin` and walks you through the playlist, your Twitch stream key (stored encrypted for your Windows user), Discord, and a system check that picks the encoder and the cache size.

Keep the `START.bat` window open: it runs the dashboard. Closing it does not stop the stream; start `START.bat` again to get the dashboard back.

StreamLoop 2.x (mpv and OBS) lives on the `2.x` branch, and 3.0 on the `3.0` branch.

## Development setup (from source)

Requires Rust stable (MSVC toolchain) and Node.js 22 or later, for the tests and the build scripts.

```bash
cargo build --release   # target\release\streamloop.exe
(cd tokenserver && cargo build --release)   # tokenserver\target\release\streamloop-pot.exe
cargo test --workspace  # the Rust tests
npm install
npm test                # the dashboard's tests and the test harness's (vitest)
```

`streamloop.exe` needs an install folder around it (ffmpeg, yt-dlp, the token server and `app\config.json`). `node supervisor/scripts/dev-root.mjs` builds one from local copies of the tools; [CLAUDE.md](CLAUDE.md) has the details, and [ARCHITECTURE.md](ARCHITECTURE.md) how the parts fit.

## Configuration

`app\config.json` is created from `app\config.example.json` on first start and edited from the dashboard. Most settings apply live; a new ingest, key, encoder, or freeze or silence time needs a stream restart, and the dashboard says so when you save. Key fields:

| Field | Default | Description |
|-------|---------|-------------|
| `playlists` | — | YouTube playlists as `{ id, name? }`, at least one, in play order |
| `port` | `7654` | Dashboard and API port |
| `supervisor.ingestUrl` | `rtmp://live.twitch.tv/app` | Twitch ingest |
| `supervisor.streamKeyProtected` | `""` | The stream key, encrypted; set it from the dashboard |
| `supervisor.encoder` | `h264_nvenc` | `h264_nvenc`, `h264_amf`, `h264_qsv` or `libx264`; the system check picks one |
| `supervisor.cacheBudgetMb` | `300` | Disk space for downloaded videos |
| `supervisor.overlay` | `true` | Draw the now-playing overlay into the stream |
| `supervisor.freezeSeconds` / `silenceSeconds` | `20` / `30` | How long a frozen picture or silence lasts before StreamLoop acts or reports it |
| `discord.webhookUrl` | `""` | Discord webhook for alerts (per-event toggles and templates under `discord`) |
| `twitchLivenessEnabled` | `false` | Check that the channel is live (needs `twitchClientId`, `twitchClientSecret`, `twitchChannel`) |
| `autoUpdateCheck` | `true` | Check GitHub Releases for updates and install them at the planned break |
| `autoUpdateYtdlp` | `true` | Update yt-dlp each time StreamLoop starts |
| `autoStartStream` | `true` | Start the stream when StreamLoop starts; when off, it waits for Start |
| `ytdlCookiesFromBrowser` | `""` | Browser to take YouTube cookies from (for age- or region-gated videos) |

## Windows release build

```bash
npm run build:release
```

It builds `streamloop.exe` and, in `tokenserver\`, `streamloop-pot.exe` with cargo, and writes, in `dist\`, the full ZIP `streamloop-vX.Y.Z.zip` (the install), `streamloop-vX.Y.Z.exe` (what the updater downloads), and a `.sha256` for each, then checks the ZIP with `build/verify-release.mjs`. There is no separate update ZIP: an update replaces the exe, and a release that changes ffmpeg or the token server says in its notes that it needs the full ZIP.

## License

GPL-3.0-only
