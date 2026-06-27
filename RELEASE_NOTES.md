## v2.3.1

- **Recovery and restarts now resume directly on the saved video instead of walking the playlist head.** On every reload/restart mpv used to start at playlist position 0 and play through videos #0..#N before a deferred jump fired. With the playlist head now frequently failing (expired URLs / 403s), each recovery produced a burst of "Playback error on video #0/1/2/3" and ~20–35s of viewer disruption — and because the jump listener was torn down after 15s, a slow or failing head could drop the resume jump entirely and silently resume on the wrong video at the wrong position. StreamLoop now sets `playlist-start` to the resume index before loading (so mpv begins on the target entry and never walks the head), uses `reloadIndex` for the resume jump so the seek still applies, and raises the jump-listener teardown from 15s to 120s so a slow/failing head can't silently drop the resume.
- **Real mpv error reason in events and Discord alerts.** Playback-error events and their Discord notifications now carry mpv's actual `file_error` string instead of a hardcoded `0` (the confusing "Playback error **0**").
- **Fixed the "update applied, terminal never reappears" hang.** When an update shipped a new `START.bat`, the running launcher copied the new file over its own path mid-execution. cmd.exe reads a batch file from disk by byte offset as it runs, so replacing it desynced the read position and usually closed the window before the relaunch — the update's `app/` swap had already happened, but the server never came back up. `START.bat` now **never overwrites itself in place**: it stages a changed launcher to `START.bat.new` (only when it actually differs, via `fc /b`) and hands off to a detached one-shot script that swaps the launcher and relaunches in a fresh window after the original exits.
- **Self-heal a half-swapped install on launch.** The `app/` update swap is non-atomic (rename old app aside, then move the new app into place). If the process was interrupted between those steps there was no `app/` at all, and every subsequent launch died at the preflight check — one bad update bricked the box. `START.bat` now restores `app/` from `_update_old/` (or promotes the staged `_update_tmp\app`) at startup before preflight, so an interrupted update recovers itself.

  > **Note for existing installs:** this update is applied by your *current* (pre-fix) `START.bat`, so the delivering update can still hit the old self-overwrite hang. If the window closes and the server doesn't come back, the new `app/` is already in place — just relaunch `START.bat` once. After that, the new trampoline launcher makes all future updates safe. For a guaranteed-clean transition, replace `START.bat` (or reinstall from the full ZIP) out-of-band.

- **Internal:** decomposed `recovery.ts`'s ~180-line `processHeartbeat` into intent-named steps and removed dead code (behavior unchanged, all tests pass); rewrote the README for the current mpv + OBS Window Capture architecture.

---

## v2.3.0

- **In-place freeze/EOF recovery actually re-resolves the stream now.** The "URL retry" used by freeze and premature-EOF recovery reloaded via `jumpTo(pos)` (`set_property playlist-pos`). During a video-stream freeze mpv is still "playing" the current index, so setting `playlist-pos` to its own value was a **no-op** — mpv never re-resolved the URL, and the frozen frame persisted (logs showed ~8 min, until the audio track ran out) while recovery silently burned through to a full mpv restart. Both `retryCurrentAtPosition` and the `RetryCurrent` recovery step now reload via `playlist-play-index` (`reloadIndex`), which restarts the current entry even when it's already current — so the in-place retry gets a real chance to fix the freeze before any restart.
- **Proactive signed-URL refresh.** YouTube googlevideo URLs expire ~6h after yt-dlp resolves them, so any video longer than the TTL freezes around the 6h mark. StreamLoop now tracks URL age on a monotonic clock (immune to NTP/RTC steps after a power outage) and forces an in-place reload at `proactiveUrlRefreshMs` (default 5.5h), turning an unplanned freeze into a planned few-second rebuffer. Short videos never trigger it; it's gated on healthy playback and skips near end-of-file; `0` disables it. Exposed in the dashboard under **Playback > Recovery** as a preset dropdown (Disabled / 4–5.5h), hot-reloaded through the config path.
- **No more spurious mpv restart on every server start.** On startup the recovery engine resumes the saved position, which on a cold yt-dlp run (playlist resolution, then a second resolution for a deep-playlist video, then the seek) keeps mpv non-playing well past the ~30s non-playing budget. The non-playing watchdog mistook that for a fault and escalated to an mpv restart on **every** start — a black screen for viewers — before settling. A configurable **startup grace** (`initialLoadGraceMs`, default 90s) now suppresses the non-playing watchdog until playback is confirmed once since the current mpv connect, or the window elapses. It re-applies on every connect (including periodic restarts), so genuine mid-stream stalls still recover exactly as before, and it logs a one-shot event ("holding off recovery during startup grace") so the hold-off is visible in the dashboard rather than looking like a hang. The stall, video-freeze, output-freeze, heartbeat-timeout, and file-error paths are unchanged.
- **yt-dlp auto-update + manual button.** An outdated yt-dlp silently truncates YouTube playlists after the first ~100-entry page, so longer playlists looked incomplete in the dashboard. yt-dlp now self-updates in the background on startup (gated by the new `autoUpdateYtdlp` config option, default on), and the dashboard's **Update yt-dlp** button is wired to the previously-unreachable endpoint. After a manual update the playlist-metadata cache is cleared so corrected counts appear without a restart.
- **Slim "update-only" release bundles.** `npm run build:release:slim` produces `streamloop-vX.Y.Z-update.zip` containing only `app/` + `yt-dlp/` — no portable Node.js or mpv, which the auto-updater never swaps anyway. The updater now **prefers the `-update.zip` asset** over the full bundle, so existing installs get a much smaller download; the full ZIP is still used for first-time installs. Checksums are matched to the chosen ZIP by name, so a release can safely carry both bundles.
- Cut mpv log-file noise ~98%: cplayer's `video EOF (status=4)` was logged thousands of times per second during every freeze. cplayer is now capped at `verbose` while `ytdl_hook=v` is kept for yt-dlp extractor errors.

---

## v2.2.0

- **Ground-truth output-freeze detection.** A new watchdog screenshots the OBS capture source every 10s and detects when the *streamed picture itself* is frozen — catching failures the mpv-side detectors can't see (a "video unavailable" still that mpv reports as healthy playback, or OBS losing the window capture). It compares frame hashes only — **no screenshots are ever written to disk**. When the picture stays byte-identical for a configurable window it takes a second confirmation screenshot a moment later before acting, then recovers **in place** at the current position (re-resolves a fresh stream) — **no mpv restart**, so OBS never captures a black screen. It shares the existing in-place URL-retry budget with the bitrate/vfps freeze detector and escalates to the old restart sequence only after those retries are exhausted. Two new settings under **Playback > Recovery**: *Detect frozen output* (on by default) and *Output freeze window* (default 30s — raise it if you ever stream legitimately static content). The window is editable from the dashboard, so it can be tuned without a new release.
- **Persistent recovery event log.** The dashboard's recovery-event timeline is now written to daily-rotated JSONL files in `logs/` (7-day retention) and reloaded on startup, so the history survives auto-updates, mpv restarts, and crashes instead of resetting to empty each time.
- **"Video skipped" Discord alerts now actually fire.** When a video is dropped after repeated playback errors (`maxConsecutiveErrors`), a skip notification is sent — previously the skip event was advertised in the dashboard but never triggered.
- Removed the orphaned quality-recovery feature (config keys, engine code, and the dashboard "Quality" display + "Quality Recovery" settings). It was v1-era scaffolding built around the YouTube IFrame player's quality signal, which mpv doesn't expose, so it never did anything. Existing `config.json` files migrate automatically — the stale keys are dropped on next load.

---

## v2.1.9

- **Video-freeze recovery now fixes the freeze in place instead of restarting mpv.** YouTube serves video and audio as separate streams; when the *video* stream stalls or EOFs but audio keeps playing, mpv holds the last frame and never fires a file-level `end-file` — so the existing URL-retry never triggered, and the heartbeat detector almost never caught it (Emma's logs: the condition occurred constantly but auto-recovery fired ~once a week, so the app had to be restarted by hand). The detector now:
  - trips on `estimated-vf-fps < 1` **or** `video-bitrate ≈ 0` (two independent signals — a frozen video stream shows up in at least one);
  - ignores the brief normal video-EOF burst at a video's natural end (`duration − time-pos < 10s`) and tolerates single-heartbeat blips, cutting false positives;
  - responds with an **in-place URL retry at the current position** (re-resolves a fresh stream, resumes near the break) — **no mpv restart**, so OBS never captures a black screen — escalating to the old restart sequence only after 3 in-place retries fail;
  - logs `vfps`, `video-bitrate`, `audio-bitrate`, `time-pos`, and `duration` whenever it trips, so each freeze self-documents in the logs.
- Separately confirmed the recurring ~6-hour `Premature stream end (eof)` events are normal YouTube signed-URL expiry, already handled cleanly by the existing in-place URL retry — left unchanged.

---

## v2.1.8

- Auto-updater now refreshes the bundled `yt-dlp/` directory (yt-dlp.exe + deno.exe) on every update, not just `app/`. YouTube rotates its player JS challenge faster than our release cadence — without this, updating wouldn't carry new yt-dlp/deno binaries into existing installs. Same retry-and-rollback safety as the existing app swap.

---

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
