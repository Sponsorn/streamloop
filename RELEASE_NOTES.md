## v2.1.7

- Bundle Deno with the release. YouTube's player JavaScript now requires solving an "n-parameter" challenge before video URLs resolve. yt-dlp's EJS subsystem can solve it but needs a JS runtime — we ship `deno.exe` alongside `yt-dlp.exe` and yt-dlp picks it up automatically. Without this, v2.1.6 fails with "Only images are available for download" on every video.
- Switch bundled yt-dlp from stable to nightly builds. YouTube rotates challenge shapes faster than yt-dlp's stable cadence; nightly tracks them more reliably. If nightly ever breaks, manually replacing `yt-dlp/yt-dlp.exe` with a stable build is always a valid fallback.

**Upgrade note for existing v2.1.6 installs:** the admin dashboard auto-updater only swaps the `app/` directory, so it won't drop a new `deno.exe` into your existing `yt-dlp/` folder. After auto-updating, either manually download `deno.exe` from https://github.com/denoland/deno/releases/latest (Windows x64) and place it at `streamloop/yt-dlp/deno.exe`, or do a clean reinstall from the v2.1.7 ZIP.

---

## v2.1.6

- Added `ytdlCookiesFromBrowser` config (admin dashboard > Playback > Maintenance). When set (e.g. `brave`, `chrome:Profile 1`), yt-dlp passes the logged-in browser session via `--cookies-from-browser`. Resolves YouTube's "Sign in to confirm you're not a bot" challenge that blocks playback on flagged IPs.
- Freeze recovery: premature stream-EOF and network errors (HTTP 5xx, TLS) now trigger an in-place retry at the last known playback position via `jumpTo(currentIndex)`. yt-dlp re-signs the googlevideo URL and playback resumes near the break. Up to 2 retries per video before falling through to the existing skip path. `keep-open=yes` preserved so viewers see the last frame during the ~2-5s retry, not a black cut.

---

## v2.1.5

### Bug Fixes

- **Infinite recovery loop when saved resume position exceeds the target video's duration.** If a long-playing video (e.g. a 6-hour VOD) left a stale `currentTime` in `state.json` and recovery then landed on a shorter video, every resume attempt applied an impossible `--start=+<too-large>` seek and failed immediately, triggering another restart — forever. Resume time is now discarded when it exceeds the last-known `videoDuration`, and mpv's `start` property is cleared immediately on seek-failure so a bad seek can't propagate to auto-advanced videos instead of waiting for the 30s cleanup timer.
- **Non-playing recovery fired during legitimate broken-video skipping.** When a run of broken/unavailable videos appeared in the playlist, mpv would fire `end-file` errors every ~4-5s and try to skip past them — but the 30s non-playing threshold tripped first, restarting mpv and throwing away all skip progress back to position 0. The `nonPlayingHeartbeats` counter now resets on each `end-file` error (mpv is clearly not stuck when it's actively erroring), letting the skip mechanism do its job. Heartbeat-timeout and stall detection are unchanged.

### Improvements

- **mpv diagnostic logs.** Each mpv spawn now writes a dedicated `logs/mpv-<timestamp>.log` with `--msg-level=ytdl_hook=v`, capturing the actual yt-dlp extractor output (HTTP status codes, signature errors, network failures) that was previously invisible. The last 10 spawn logs are retained. New "mpv Diagnostic Logs" section on the dashboard Overview tab lets you pick any past spawn and view its contents — the next recovery loop self-documents instead of leaving you guessing.

---

## v2.1.4

### Bug Fixes

- **Video freeze recovery loops forever instead of escalating.** When video froze but audio kept playing, recovery started `retryCurrent` but never escalated to `restartMpv` because advancing `time-pos` (from audio) kept cancelling the recovery. Now freeze-triggered recovery properly escalates through the full sequence.
- **Resume position lost after recovery restart.** During recovery, heartbeat polls could overwrite the saved `videoIndex` with a transient `playlistPos=0` from mpv mid-reload. State writes are now suppressed during active recovery.
- **`mpv.restart()` didn't wait for process exit.** The `stop()` call inside `restart()` was not awaited, so the old mpv process might not have fully exited before spawning a new one.

### Improvements

- **Much faster update extraction.** Switched from PowerShell `Expand-Archive` to Windows built-in `tar.exe` (bsdtar) for both the updater and build script. Extraction that took 30+ minutes now completes in seconds.

---

## v2.1.3

### Bug Fixes

- **Update swap fails with "Access is denied".** `mpv.stop()` now waits for the process to fully exit (with 5s SIGKILL fallback) instead of fire-and-forget. START.bat also retries the app directory rename up to 5 times (with 3s delays) as a safety net.

---

## v2.1.2

### New Features

- **"This is not live!" overlay badge.** Red banner in the top-right corner of the OBS overlay so viewers know the stream is pre-recorded.

---

## v2.1.1

### Bug Fixes

- **Fixed triple event handling after config reload.** Recovery engine event listeners accumulated on the mpv client each time the config was reloaded, causing 3x duplicate log messages and 3x competing playlist loads on every mpv restart. Listeners are now properly removed on stop.
- **Video freeze detection.** When mpv's video output froze but audio continued playing, recovery never triggered because `time-pos` was still advancing. Now monitors `estimated-vf-fps` and triggers recovery when video framerate drops to zero for 4 consecutive heartbeats.
- **Resume lands on wrong video after restart.** The mpv `start` property (seek position) was set before loading the playlist even when the target video wasn't index 0, causing video 0 to attempt an impossible seek and error out before the jump to the correct video could happen. Now only pre-sets seek for video 0; for other indices, seek is set right before the jump.
- **IPC command timeout.** mpv IPC commands now time out after 5 seconds instead of hanging forever when mpv is unresponsive, allowing heartbeat timeout detection to work correctly.

### Improvements

- **Now-playing overlay readability.** Darkened overlay background from white 30% to black 70% opacity.

---

## v2.1.0

### New Features

- **Stream starts only after video is visible.** OBS auto-stream now waits until mpv is confirmed rendering video frames (not just connected), preventing viewers from seeing a black screen on startup or recovery.
- **Restart mpv button.** One-click restart in the dashboard if the mpv window disappears or gets stuck.

### Bug Fixes

- **Resume position after restart.** Uses mpv's `start` property to request the correct byte range from YouTube, instead of post-load seeking which YouTube rejected.
- **Seek failure recovery.** If resuming at a saved position fails, replays from the beginning instead of entering an error loop.
- **mpv window visibility.** Reverted detached spawn, added `--force-window=yes`, switched to `--hwdec=auto` for broader hardware compatibility.
- **Overlay opacity** increased to 0.30 for better readability.

---

## v2.0.5

### Bug Fixes

- **mpv window visible again.** Reverted detached spawn that caused mpv to open without a window on some systems. Uses `--force-window=yes` and `--hwdec=auto` for broader hardware compatibility.

---

## v2.0.4

### Bug Fixes

- **mpv window now opens visible.** Spawns mpv as a detached process so it gets its own window instead of inheriting the parent's hidden state.
- **Test files excluded from release ZIP.** Reduces download size.

---

## v2.0.3

### Bug Fixes

- **Auto-updater extraction timeout increased to 10 minutes.** The larger v2.0.x ZIP (with bundled mpv) was timing out during extraction on low-RAM systems.

---

## v2.0.2

### Bug Fixes

- **Resumes at correct timestamp after recovery.** Fixed race condition where playlist loaded multiple times after mpv restart, preventing the seek to saved position. Now uses event-based waiting instead of fragile timeouts.

- **Play button in video list works.** (Also in v2.0.1)

---

## v2.0.1

### Bug Fixes

- **Play button in video list now works.** Fixed missing Content-Type header on POST requests from dashboard playback controls.

---

## v2.0.0

### Breaking Changes

- **mpv replaces the OBS Browser Source player.** StreamLoop now uses mpv (bundled) to play YouTube playlists, controlled via Windows named pipe IPC. This eliminates the Chromium memory leak that caused playback stalls on low-RAM systems. **You must set up a Window Capture source in OBS instead of a Browser Source.** Download the full v2.0.0 ZIP — the auto-updater from v1.x cannot upgrade to this version.

- **Now Playing overlay requires a separate Browser Source.** Add a Browser Source in OBS pointing to `http://localhost:7654/overlay` and layer it above the Window Capture.

### New Features

- **Playlist selector and playback controls.** The dashboard Monitor tab now has a playlist dropdown, transport controls (previous/pause/next/stop), seek bar, and a paginated video list. Click any video to play it.

- **Stop button.** Intentionally pause playback without the server auto-resuming. The stop button prevents recovery from restarting the video.

- **Shutdown button.** Cleanly stop StreamLoop (mpv, server, all processes) from the Settings tab.

- **System memory in Discord webhooks.** Recovery and critical alerts include RAM usage.

- **Zombie OBS process detection.** If OBS is running but unresponsive to WebSocket after 5 attempts, it's force-killed and relaunched.

- **yt-dlp update from dashboard.** Update yt-dlp to the latest version from the Settings tab to keep up with YouTube changes.

### Migration from v1.x

1. Download the full v2.0.0 ZIP and extract to a new folder
2. Copy `config.json` and `state.json` from your old installation
3. In OBS: remove the old Browser Source, add a **Window Capture** (select mpv window) and a **Browser Source** for the overlay (`http://localhost:7654/overlay`)
4. Run `START.bat`

---

## v1.4.3

### Bug Fixes

- **Fixed resume position after source refresh.** The YouTube player now receives `startSeconds` in the `loadPlaylist()` call, so playback starts at the correct position immediately. Previously, the player briefly started at 0:00 before seeking, which could cause state.json to save an incorrect position if a heartbeat fired during that window.

### Improvements

- **Proactive source refresh enabled by default (every 30 minutes).** Periodically reloads the OBS browser source to prevent Chromium memory buildup during long sessions. Particularly important on systems with 8-16GB RAM where the browser source can grow to 1-2GB over time. Set `sourceRefreshIntervalMs` to `0` to disable.

- **System memory logged on stalls, timeouts, and recovery events.** RAM usage (used/total GB and percentage) is now included in log messages and Discord webhook notifications when recovery triggers, making it easier to diagnose memory-pressure-related playback issues.

- **Dashboard status includes system memory.** The `/api/status` endpoint now returns `systemMemory` with current RAM usage.

---

## v1.4.2

### Bug Fixes

- **Quality recovery now properly escalates.** Previously, quality recovery could only ever attempt a browser source refresh — escalation to toggle visibility and critical alert was silently cancelled because the escalation check didn't account for quality-triggered recovery. A new `recoveryReason` field tracks why recovery started, allowing correct escalation decisions.

- **Quality recovery no longer cancelled by normal playback.** When quality was low but the video was still playing and advancing, the heartbeat progress detection would cancel the in-progress quality recovery. Now playback progress correctly skips the reset when the recovery reason is quality.

- **Error retry timer no longer leaks on config reload or shutdown.** The delayed `retryCurrent` after a playback error was an untracked `setTimeout` that could fire after `stop()`, sending a ghost message on a stale WebSocket. Now tracked and cleared properly.

- **Detection counters reset on playlist transition.** Stall, quality, non-playing, and paused heartbeat counters now reset when advancing to the next playlist, preventing stale counts from triggering false recovery on the new playlist.

### Improvements

- **Quality threshold uses configured heartbeat interval.** The low-quality detection threshold now derives from `heartbeatIntervalMs` in config instead of a hardcoded 5000ms divisor, so custom heartbeat intervals calculate correctly.

- **Gradual quality counter decay.** When quality improves above the minimum, the low-quality heartbeat counter now decrements by 1 instead of resetting to 0. This prevents quality oscillating at the threshold boundary from indefinitely delaying recovery.

- **Unknown YouTube quality strings are logged.** If the YouTube API sends an unrecognized quality string, a warning is logged instead of silently ignoring it.

- **Discord quality notifications include details.** Quality recovery Discord messages now include the actual quality, minimum expected quality, and video index (e.g. "Low quality recovery: medium (minimum: hd720) on video #3").

- **More accurate recovery cancellation log message.** Changed from "Heartbeat restored" to "Recovery condition resolved" with the recovery reason included in structured log output.

---

## v1.4.1

### Bug Fixes

- **Quality recovery no longer restarts the video from the beginning.** Previously, recovering from sustained low quality would trigger `RetryCurrent`, which restarted the video at 0:00. Now it skips straight to refreshing the OBS browser source, which reloads the player and resumes at the saved position.

- **WebSocket reconnection no longer triggers false recovery.** When the OBS browser source reconnected, the old WebSocket was left open. When it eventually closed, it would fire a disconnect signal and trigger unnecessary recovery steps even though the new connection was healthy. The old connection is now explicitly closed, and only the active client's disconnect fires the callback.

### Improvements

- **Discord rate-limit handling.** The Discord notifier now respects HTTP 429 responses and waits for the `retry-after` duration before continuing, preventing dropped notifications during bursts of recovery events.

- **Safer update extraction.** The auto-updater now uses `execFileSync` instead of `execSync` for the PowerShell extraction step, avoiding potential issues with special characters in file paths.

- **Resilient state loading.** Missing or corrupt fields in `state.json` now fall back to safe defaults instead of propagating bad values at runtime.

- **Reduced log noise.** Config rewrite-on-load messages (triggered by key reordering, not actual changes) downgraded from `info` to `debug`.

- **Fixed TypeScript errors in test mocks.** Added missing `twitchMismatch` and `twitchRestart` fields to test config mocks in `recovery.test.ts` and `discord.test.ts`.
