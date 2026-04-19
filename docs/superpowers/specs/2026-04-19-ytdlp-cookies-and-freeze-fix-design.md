# yt-dlp Cookies + Premature-EOF RetryCurrent

**Date:** 2026-04-19
**Branch:** TBD
**Status:** Design

## Problem

Two failures are blocking Emma's rig:

**1. YouTube bot-detection.** Every yt-dlp resolution fails with `ERROR: [youtube] <id>: Sign in to confirm you're not a bot`. Log evidence: 600 identical errors in a 15-minute mpv log, different video IDs each time. StreamLoop chews through the playlist at ~15 videos/minute without playing anything. `consecutiveErrors` cycles 1→2→3 → `RestartMpv` → same IP/fingerprint → same challenge, forever. This is a machine-level flag on YouTube's side; playlist visibility (public vs. private) doesn't affect it.

**2. Premature-EOF freeze** (FREEZE_INVESTIGATION.md). After ~37 hours of runtime, a googlevideo CDN URL expired mid-video. ffmpeg hit HTTP 503 on reconnect, mkv demuxer reported EOF, and `keep-open=yes` parked mpv on the last frame. No playlist advance, no visible recovery — just a frozen rig. StreamLoop's existing heartbeat/non-playing paths don't catch this because `paused=true` keeps the non-playing counter at zero, and the auto-resume loop fights the EOF park.

## Solution

Two independent, small changes shipped together:

1. **Config-driven yt-dlp cookies** passed through to both mpv's ytdl_hook and the standalone `playlist-metadata` yt-dlp calls.
2. **Premature-EOF detection in `recovery.ts`** that fires `RetryCurrent` with the saved playback position, causing yt-dlp to re-sign the googlevideo URL and mpv to resume near the break. `keep-open=yes` stays — the parked last frame is the intended viewer experience while retry runs.

Neither change touches the existing escalation ladder (RetryCurrent → RestartMpv → CriticalAlert). The new branch sits *in front of* the existing `onFileEnded` logic and only triggers on the specific premature-EOF / network-error shape.

## Part 1 — yt-dlp Cookies

### Config schema (`src/server/config.ts`)

Add one field to `configSchema`:

```ts
ytdlCookiesFromBrowser: z.string().default('')
```

Accepted values: empty string (disabled), or a yt-dlp `--cookies-from-browser` spec like `brave`, `chrome`, `firefox`, `brave:Profile 1`, `chrome:Default`. No format validation beyond a string — yt-dlp does the real parsing and surfaces its own error if the value is wrong.

### mpv plumbing (`src/server/index.ts`)

When `ytdlCookiesFromBrowser` is non-empty, append to mpv args:

```ts
`--ytdl-raw-options-append=cookies-from-browser=${config.ytdlCookiesFromBrowser}`
```

Using `-append` (not `--ytdl-raw-options=...`) so it stacks cleanly with the existing `--ytdl-raw-options=yes-playlist=,js-runtimes=node`. mpv translates each comma-separated `key=value` into `--key value` on the yt-dlp command line.

When empty, the flag is omitted entirely.

### Playlist metadata plumbing (`src/server/playlist-metadata.ts`)

`PlaylistMetadataCache` currently spawns `yt-dlp` directly for the dashboard's playlist view. Without cookies here, the view fails even when mpv playback works. Changes:

- Constructor gains a second arg: `cookiesFromBrowser?: string`.
- `_doFetch` prepends `--cookies-from-browser <value>` to the argv when set.
- `index.ts` passes `config.ytdlCookiesFromBrowser` through.

### Admin UI (`src/admin/admin.js`, `index.html`)

Add a text input under the existing mpv settings block, labelled "yt-dlp cookies from browser" with a helper line: `Examples: brave, chrome, firefox, chrome:Profile 2. Leave blank to disable.` Wired into the existing config save flow — no new API endpoint.

### Config reload

Cookies change applies after a component reload (existing `reloadConfig` path in `index.ts`). mpv itself isn't restarted, so cookies only take effect for yt-dlp calls made after the reload. For mpv, cookies apply on the *next* mpv restart (periodic or via recovery). Documented behavior, no special handling.

## Part 2 — Premature-EOF RetryCurrent

### mpv event plumbing (`src/server/mpv-client.ts`)

`handleEvent` currently emits:

```ts
this.emit('fileEnded', msg.reason ?? 'unknown');
```

Extend to include mpv's `file_error` field (populated on HTTP 503, TLS errors, loading failures):

```ts
this.emit('fileEnded', msg.reason ?? 'unknown', msg.file_error);
```

Consumers that only read `reason` (tests, etc.) keep working — TypeScript signature becomes `(reason: string, fileError?: string) => void`.

### Recovery detection (`src/server/recovery.ts`)

A new private helper fires an in-place retry:

```ts
private async retryCurrentAtPosition(seekSeconds: number): Promise<void>
```

Behavior:
- `setProperty('start', '+<seek>')`
- `jumpTo(currentIndex)` — mpv treats this as a file reload, runs ytdl_hook, re-signs the URL
- After 30s, `setProperty('start', 'none')` to avoid the start flag leaking to auto-advanced videos

New retry counter and helpers on `RecoveryEngine`:
- `urlRetryCount: number` — number of premature-EOF retries fired for the *current* video
- `lastSeenVideoIndex: number` — last playlist index observed in `processHeartbeat`
- Reset `urlRetryCount = 0` in `processHeartbeat` whenever `hb.playlistPos !== lastSeenVideoIndex` (video changed — either auto-advance, jumpTo after retry to a different video, or playlist wrap)
- Max 2 retries per video before falling through to existing error-handling

A single video that keeps failing will burn both retries back-to-back (same `playlistPos` throughout), then the third `onFileEnded` call falls through to the existing `consecutiveErrors`/skip path. A retry that succeeds plays the video to completion; when mpv auto-advances to the next playlist entry, `playlistPos` changes and the counter resets for the new video.

Update `onFileEnded`:

```ts
private async onFileEnded(reason: string, fileError?: string) {
  if (this.shouldRetryUrl(reason, fileError)) {
    if (this.urlRetryCount < 2) {
      this.urlRetryCount++;
      const seek = this.state.get().currentTime;
      this.addEvent(`Premature stream end detected — retrying in place (attempt ${this.urlRetryCount}/2)`);
      await this.retryCurrentAtPosition(seek);
      return;
    }
    // fall through: skip/escalate via existing logic
  }
  // ... existing 'error' and 'eof' branches unchanged
}
```

`shouldRetryUrl` returns true when:
- We were actively playing within the last heartbeat interval (otherwise this is the playlist-load phase — existing code ignores those), AND
- One of:
  - **Premature EOF**: `reason === 'eof'` AND `state.videoDuration > 0` AND `state.currentTime < state.videoDuration - 5`
  - **Network error**: `reason === 'error'` AND `fileError` matches `/http|network|loading failed|tls|ssl/i`

Counter reset is handled in `processHeartbeat`: when `hb.playlistPos !== lastSeenIndex`, reset `urlRetryCount = 0` and update `lastSeenIndex`.

### mpv.conf

Unchanged. `keep-open=yes` is deliberately preserved so mpv holds the last frame while yt-dlp re-resolves (~2-5s), giving the viewer a frozen frame instead of a black cut.

## Data Flow: Freeze Recovery

```
mpv stream dies (HTTP 503 / EOF at 117MB / 125MB)
  │
  ▼
mpv fires end-file { reason: 'error', file_error: 'loading failed' }
  │ OR       end-file { reason: 'eof' }  (if demuxer reports EOF before network gives up)
  ▼
mpv-client emits ('fileEnded', reason, fileError)
  │
  ▼
recovery.onFileEnded
  │
  ├─ shouldRetryUrl? ────► YES (was playing, reason + position fit premature shape)
  │                            │
  │                            ▼
  │                         urlRetryCount < 2
  │                            │
  │                            ▼
  │                         retryCurrentAtPosition(state.currentTime)
  │                            │  sets start=+<sec>, jumpTo(idx), clears start after 30s
  │                            ▼
  │                         mpv reloads same video ─► ytdl_hook ─► fresh googlevideo URL
  │                            │                                    │
  │                            ▼                                    ▼
  │                         playback resumes near break        viewer sees ~2-5s frozen frame
  │
  └─ shouldRetryUrl? ────► NO (or retries exhausted)
                               │
                               ▼
                            existing 'error'/'eof' logic — consecutiveErrors, next video, etc.
```

## Testing

### Cookies

- `config.ts`: schema round-trip — empty default, accepts valid strings, preserves on save.
- `playlist-metadata.ts`: `new PlaylistMetadataCache(path, 'brave')` produces argv containing `--cookies-from-browser`, `brave`. `new PlaylistMetadataCache(path)` does not.
- Manual: flip the config on Emma's rig, confirm fresh mpv log is free of bot errors, confirm playlist view works.

### Premature-EOF RetryCurrent

- Unit: `shouldRetryUrl` truth table — premature EOF triggers, eof-at-duration doesn't, network error triggers, non-network error (`generic decode failure`) doesn't, pre-playback events don't.
- Unit: retry counter — two retries fire, third falls through to existing error path. Counter resets when `playlistPos` changes in heartbeat.
- Unit: `retryCurrentAtPosition` sets/clears `start` in the right order (existing mpv-client tests pattern).
- Manual: hardest to reproduce on demand. Emma's rig should confirm via a multi-day run that doesn't freeze.

## Risks and Non-Goals

- **Cookie expiry.** YouTube rotates session tokens. When the throwaway Brave account's session dies, bot errors return. No automatic detection — user sees it in logs and re-logs in Brave. Not worth automating now; the manual step is infrequent and obvious.
- **Retry against a genuinely-dead stream.** If the video is truly gone (deleted, geo-blocked, age-gated without cookies), retry burns 2 attempts before the existing skip path fires. Acceptable — same total failure time as today, just with two more re-resolution attempts in front.
- **`keep-open=yes` still freezes on *non*-network EOF pathologies** we haven't seen yet. Not in scope. If we see one, revisit then.
- **yt-dlp re-auth flow.** `--cookies-from-browser` handles DPAPI decryption on Windows, but needs yt-dlp run as the same user that created the Brave profile. Documented in the admin UI helper text.
