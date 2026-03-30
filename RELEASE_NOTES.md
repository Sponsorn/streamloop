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
