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
