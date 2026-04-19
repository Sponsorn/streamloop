# yt-dlp Cookies + Premature-EOF RetryCurrent — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `ytdlCookiesFromBrowser` config passed through to every yt-dlp call, and add a premature-EOF detector in `recovery.ts` that re-signs the googlevideo URL via `jumpTo(currentIndex)` with the saved position.

**Architecture:** Schema field in `config.ts` → plumbed into two yt-dlp sites (`index.ts` mpv args and `playlist-metadata.ts` execFile args). New recovery branch ahead of existing `onFileEnded` logic fires a position-preserving `retryCurrentAtPosition(sec)` up to twice per video before falling through to the old error/skip path. `keep-open=yes` stays — viewer sees the last frame while retry runs.

**Tech Stack:** TypeScript + Vitest, Zod schema, mpv JSON IPC, yt-dlp subprocess.

**Spec:** `docs/superpowers/specs/2026-04-19-ytdlp-cookies-and-freeze-fix-design.md`

---

## File Map

| File | Change |
|---|---|
| `src/server/config.ts` | Add `ytdlCookiesFromBrowser` to Zod schema |
| `src/server/playlist-metadata.ts` | Constructor takes `cookiesFromBrowser?: string`; `_doFetch` prepends flag |
| `src/server/mpv-client.ts` | `end-file` event emits `file_error` as third arg |
| `src/server/recovery.ts` | New `retryCurrentAtPosition`, `shouldRetryUrl`, `urlRetryCount`, `lastSeenVideoIndex`; modified `onFileEnded` and `processHeartbeat` |
| `src/server/index.ts` | Pass `config.ytdlCookiesFromBrowser` into mpv args + `PlaylistMetadataCache` |
| `src/admin/index.html` | Cookies input under Maintenance section |
| `src/admin/admin.js` | Load/save cookies field |
| `config.example.json` | Document new field |
| `src/server/__tests__/config.test.ts` | Schema test |
| `src/server/__tests__/playlist-metadata.test.ts` | Argv construction test |
| `src/server/__tests__/mpv-client.test.ts` | `file_error` passed through end-file event |
| `src/server/__tests__/recovery.test.ts` | shouldRetryUrl truth table, retry counter, reset on videoIndex change |
| `package.json` | Version bump |
| `RELEASE_NOTES.md` | Changelog |

---

## Task 1: Add `ytdlCookiesFromBrowser` to config schema

**Files:**
- Modify: `src/server/config.ts:107` (end of configSchema, before closing paren)
- Modify: `src/server/__tests__/config.test.ts` (add test)
- Modify: `config.example.json` (add field)

- [ ] **Step 1: Write the failing test**

Add to `src/server/__tests__/config.test.ts` inside the `describe('loadConfig', ...)` block (alongside the other tests):

```ts
  it('accepts ytdlCookiesFromBrowser string and defaults to empty', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PL1' }],
      obsBrowserSourceName: 'Source',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.ytdlCookiesFromBrowser).toBe('');
  });

  it('preserves ytdlCookiesFromBrowser when set', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PL1' }],
      obsBrowserSourceName: 'Source',
      ytdlCookiesFromBrowser: 'brave:Profile 1',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.ytdlCookiesFromBrowser).toBe('brave:Profile 1');
  });
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm test -- --run src/server/__tests__/config.test.ts`
Expected: Both new tests fail — first with `undefined` for default, second because field isn't in the parsed schema.

- [ ] **Step 3: Add the schema field**

In `src/server/config.ts`, inside `configSchema` (add after the `mpvExtraArgs` line at `:106`):

```ts
  mpvExtraArgs: z.array(z.string()).default([]),
  ytdlCookiesFromBrowser: z.string().default(''),
});
```

- [ ] **Step 4: Update types.ts if AppConfig is manually mirrored**

Run: `grep -n "mpvExtraArgs" src/server/types.ts`

If the grep finds an entry, add `ytdlCookiesFromBrowser: string;` right after it in `types.ts`. If it's inferred from Zod (no manual mirror), skip this step.

- [ ] **Step 5: Run test to verify it passes**

Run: `npm test -- --run src/server/__tests__/config.test.ts`
Expected: All config tests pass.

- [ ] **Step 6: Add field to config.example.json**

In `config.example.json`, change the last line from:

```json
  "twitchPollIntervalMs": 60000
}
```

to:

```json
  "twitchPollIntervalMs": 60000,
  "ytdlCookiesFromBrowser": ""
}
```

Only add the one new field — the existing mpv fields (`mpvGeometry`, `mpvYtdlFormat`, `mpvExtraArgs`) have schema defaults and aren't carried in the example.

- [ ] **Step 7: Commit**

```bash
git add src/server/config.ts src/server/__tests__/config.test.ts config.example.json src/server/types.ts
git commit -m "$(cat <<'EOF'
Add ytdlCookiesFromBrowser config field

Empty string disables. Non-empty values pass through to yt-dlp as
--cookies-from-browser. Accepts browser names and profile specs
like "brave", "chrome:Profile 1", "firefox".

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Wire cookies into PlaylistMetadataCache

**Files:**
- Modify: `src/server/playlist-metadata.ts:56-58` (constructor), `:94-103` (_doFetch)
- Modify: `src/server/__tests__/playlist-metadata.test.ts` (add argv test)

- [ ] **Step 1: Write the failing test**

Add to `src/server/__tests__/playlist-metadata.test.ts` a new `describe` block at the bottom of the file:

```ts
import { PlaylistMetadataCache } from '../playlist-metadata.js';

describe('PlaylistMetadataCache argv', () => {
  it('omits cookies flag when not configured', () => {
    const cache = new PlaylistMetadataCache('/tmp/yt-dlp');
    const argv = (cache as any).buildArgv('PL123');
    expect(argv).not.toContain('--cookies-from-browser');
  });

  it('includes cookies flag when configured', () => {
    const cache = new PlaylistMetadataCache('/tmp/yt-dlp', 'brave');
    const argv = (cache as any).buildArgv('PL123');
    const idx = argv.indexOf('--cookies-from-browser');
    expect(idx).toBeGreaterThanOrEqual(0);
    expect(argv[idx + 1]).toBe('brave');
  });

  it('includes cookies flag with profile spec', () => {
    const cache = new PlaylistMetadataCache('/tmp/yt-dlp', 'chrome:Profile 2');
    const argv = (cache as any).buildArgv('PL123');
    const idx = argv.indexOf('--cookies-from-browser');
    expect(idx).toBeGreaterThanOrEqual(0);
    expect(argv[idx + 1]).toBe('chrome:Profile 2');
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm test -- --run src/server/__tests__/playlist-metadata.test.ts`
Expected: FAIL — `buildArgv` doesn't exist, constructor takes 1 arg not 2.

- [ ] **Step 3: Modify PlaylistMetadataCache**

Replace `src/server/playlist-metadata.ts:51-58` (the `PlaylistMetadataCache` class opening through constructor):

```ts
export class PlaylistMetadataCache {
  private readonly ytdlpPath: string;
  private readonly cookiesFromBrowser: string;
  private readonly cache = new Map<string, CacheEntry>();
  private readonly inFlight = new Map<string, Promise<PlaylistMetadata>>();

  constructor(ytdlpPath: string, cookiesFromBrowser: string = '') {
    this.ytdlpPath = ytdlpPath;
    this.cookiesFromBrowser = cookiesFromBrowser;
  }

  /** Build the argv yt-dlp is invoked with. Exposed via class so tests can inspect. */
  private buildArgv(playlistId: string): string[] {
    const url = `https://www.youtube.com/playlist?list=${playlistId}`;
    const args: string[] = [];
    if (this.cookiesFromBrowser) {
      args.push('--cookies-from-browser', this.cookiesFromBrowser);
    }
    args.push('--flat-playlist', '--dump-json', '--no-warnings', url);
    return args;
  }
```

Then replace `_doFetch` (lines 94-103) so it uses `buildArgv`:

```ts
  private async _doFetch(playlistId: string): Promise<PlaylistMetadata> {
    logger.info({ playlistId }, 'Fetching playlist metadata via yt-dlp');

    const { stdout } = await execFile(
      this.ytdlpPath,
      this.buildArgv(playlistId),
      { maxBuffer: 50 * 1024 * 1024, timeout: 120_000 },
    );
```

- [ ] **Step 4: Run test to verify it passes**

Run: `npm test -- --run src/server/__tests__/playlist-metadata.test.ts`
Expected: All playlist-metadata tests pass (old parsePlaylistOutput tests + 3 new argv tests).

- [ ] **Step 5: Commit**

```bash
git add src/server/playlist-metadata.ts src/server/__tests__/playlist-metadata.test.ts
git commit -m "$(cat <<'EOF'
Pipe cookies-from-browser into PlaylistMetadataCache

Prepends --cookies-from-browser <spec> to yt-dlp argv when the
constructor receives a non-empty string. Default behavior unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Wire cookies from config into mpv args and metadata cache

**Files:**
- Modify: `src/server/index.ts:64-82` (mpv args), `:82` (PlaylistMetadataCache constructor)

- [ ] **Step 1: Edit index.ts mpv args block**

Replace `src/server/index.ts:64-80` (the whole `new MpvClient({ ... })` block). Add the cookies arg conditionally between the existing `--script-opts=...` line and `...config.mpvExtraArgs`:

```ts
  const mpvArgs = [
    '--no-border',
    '--no-osc',
    '--osd-level=0',
    `--geometry=${config.mpvGeometry}`,
    '--hwdec=auto',
    `--ytdl-format=${config.mpvYtdlFormat}`,
    '--loop-playlist=inf',
    '--ytdl-raw-options=yes-playlist=,js-runtimes=node',
    `--script-opts=ytdl_hook-ytdl_path=${ytdlpPath}`,
  ];
  if (config.ytdlCookiesFromBrowser) {
    mpvArgs.push(`--ytdl-raw-options-append=cookies-from-browser=${config.ytdlCookiesFromBrowser}`);
  }
  mpvArgs.push(...config.mpvExtraArgs);

  const mpv = new MpvClient({
    mpvPath,
    pipePath: '\\\\.\\pipe\\mpv-streamloop',
    logsDir,
    mpvArgs,
  });
```

- [ ] **Step 2: Update PlaylistMetadataCache construction**

In the same file, one line after the MpvClient block (around `:82`), replace:

```ts
  const playlistCache = new PlaylistMetadataCache(ytdlpPath);
```

with:

```ts
  const playlistCache = new PlaylistMetadataCache(ytdlpPath, config.ytdlCookiesFromBrowser);
```

- [ ] **Step 3: Sanity-check types**

Run: `npx tsc --noEmit`
Expected: No errors.

- [ ] **Step 4: Commit**

```bash
git add src/server/index.ts
git commit -m "$(cat <<'EOF'
Wire ytdlCookiesFromBrowser into mpv and playlist metadata

mpv gets --ytdl-raw-options-append=cookies-from-browser=<value> when
configured; PlaylistMetadataCache receives the spec via constructor
arg. Both paths no-op on empty default.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Admin UI — cookies input under Maintenance

**Files:**
- Modify: `src/admin/index.html:400-416` (Maintenance panel)
- Modify: `src/admin/admin.js:1352-1394` (load/save playback settings)

- [ ] **Step 1: Add input to index.html**

In `src/admin/index.html`, find the Maintenance section (starts around line 398 with `<div class="section-title">Maintenance</div>`). Inside the `<div class="settings-panel">`, after the `#pb-refresh-interval` `<div class="form-group">` block closes, add:

```html
            <div class="form-group">
              <label for="pb-ytdl-cookies">yt-dlp cookies from browser</label>
              <input type="text" id="pb-ytdl-cookies" class="wh-preview-select" placeholder="e.g. brave, chrome, firefox, chrome:Profile 2">
              <div class="hint">Pass a logged-in session to yt-dlp to avoid "Sign in to confirm you're not a bot" errors. Leave blank to disable. Browser profile optional (after a colon).</div>
            </div>
```

- [ ] **Step 2: Load the value in admin.js**

In `src/admin/admin.js`, inside `loadPlaybackSettings` (around `:1352`), after the `$('#pb-refresh-interval').value = ...` line, add:

```js
    $('#pb-refresh-interval').value = String(cfg.sourceRefreshIntervalMs || 0);
    $('#pb-ytdl-cookies').value = cfg.ytdlCookiesFromBrowser || '';
    playbackSettingsLoaded = true;
```

- [ ] **Step 3: Save the value in admin.js**

In `handlePlaybackSave` (around `:1368`), add the field to the `body` object:

```js
  const body = {
    recoveryDelayMs: Number($('#pb-recovery-delay').value),
    maxConsecutiveErrors: Number($('#pb-max-errors').value),
    qualityRecoveryEnabled: $('#pb-quality-toggle').checked,
    minQuality: $('#pb-quality-min').value,
    qualityRecoveryDelayMs: Number($('#pb-quality-delay').value),
    sourceRefreshIntervalMs: Number($('#pb-refresh-interval').value),
    ytdlCookiesFromBrowser: $('#pb-ytdl-cookies').value.trim(),
  };
```

- [ ] **Step 4: Manually verify**

Run: `npm start` (use your dev config.json). Open admin dashboard → Playback tab → enter `brave` in the new field → Save → refresh dashboard → verify the field still shows `brave`.

Expected: Field persists. `config.json` gains `"ytdlCookiesFromBrowser": "brave"`.

Stop the dev server.

- [ ] **Step 5: Commit**

```bash
git add src/admin/index.html src/admin/admin.js
git commit -m "$(cat <<'EOF'
Add ytdlCookiesFromBrowser field to admin dashboard

Sits under Playback > Maintenance alongside the periodic mpv restart
setting. Plain text input; yt-dlp does the real validation at runtime.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: mpv-client — surface file_error in end-file event

**Files:**
- Modify: `src/server/mpv-client.ts:427-429` (handleEvent end-file case)
- Modify: `src/server/__tests__/mpv-client.test.ts:209-218` (existing end-file test; add new test)

- [ ] **Step 1: Add a new test for file_error passthrough**

In `src/server/__tests__/mpv-client.test.ts`, add after the existing `should emit fileEnded with reason on end-file event` test (at `:218`):

```ts
    it('should emit fileEnded with reason and file_error on end-file event', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const eventPromise = new Promise<[string, string | undefined]>((resolve) => {
        c.on('fileEnded', (reason: string, fileError?: string) => resolve([reason, fileError]));
      });
      serverSend(sock, { event: 'end-file', reason: 'error', file_error: 'loading failed' });
      const [reason, fileError] = await eventPromise;
      expect(reason).toBe('error');
      expect(fileError).toBe('loading failed');
    });

    it('should emit fileEnded with undefined file_error when not present', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const eventPromise = new Promise<[string, string | undefined]>((resolve) => {
        c.on('fileEnded', (reason: string, fileError?: string) => resolve([reason, fileError]));
      });
      serverSend(sock, { event: 'end-file', reason: 'eof' });
      const [reason, fileError] = await eventPromise;
      expect(reason).toBe('eof');
      expect(fileError).toBeUndefined();
    });
```

- [ ] **Step 2: Run test to verify the new tests fail**

Run: `npm test -- --run src/server/__tests__/mpv-client.test.ts`
Expected: Two new tests fail — they'll see `fileError` as undefined because the emit doesn't pass it yet (the first new test fails; the second passes by accident since undefined is already undefined, but we'll cover it anyway).

- [ ] **Step 3: Modify the end-file handler**

In `src/server/mpv-client.ts`, replace lines 427-429:

```ts
      case 'end-file':
        this.emit('fileEnded', msg.reason ?? 'unknown', msg.file_error);
        break;
```

- [ ] **Step 4: Run test to verify all pass**

Run: `npm test -- --run src/server/__tests__/mpv-client.test.ts`
Expected: All mpv-client tests pass, including the new two.

- [ ] **Step 5: Commit**

```bash
git add src/server/mpv-client.ts src/server/__tests__/mpv-client.test.ts
git commit -m "$(cat <<'EOF'
Surface mpv file_error via fileEnded event

The end-file IPC message includes a file_error field on network
errors (HTTP 5xx, TLS, loading failures). Pass it through so
recovery can distinguish a signed-URL expiry from other errors.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Recovery engine — add retry counter fields and shouldRetryUrl

**Files:**
- Modify: `src/server/recovery.ts:56-60` (field declarations), bound handler `:64`, add `shouldRetryUrl` method
- Modify: `src/server/__tests__/recovery.test.ts` (add tests)

- [ ] **Step 1: Write failing tests for shouldRetryUrl**

Scroll to the end of `src/server/__tests__/recovery.test.ts` and add a new `describe` block. First check the top of the file to confirm imports match; if `RecoveryEngine` is already imported, you're set. Add:

```ts
describe('shouldRetryUrl', () => {
  let engine: RecoveryEngine;
  let mpv: MockMpv;
  let state: ReturnType<typeof mockState>;

  beforeEach(() => {
    mpv = mockMpv();
    state = mockState({ videoDuration: 600, currentTime: 120 }) as any;
    engine = new RecoveryEngine(
      makeConfig({ heartbeatIntervalMs: 5000 }),
      mpv as unknown as MpvClient,
      state as StateManager,
      mockObs(),
      { notifyError: vi.fn(), notifyRecovery: vi.fn(), notifyCritical: vi.fn(), notifyResume: vi.fn(), notifySkip: vi.fn(), notifyObsDisconnect: vi.fn(), notifyObsReconnect: vi.fn(), notifyStreamDrop: vi.fn(), notifyStreamRestart: vi.fn() } as unknown as DiscordNotifier,
    );
    // Simulate a recent heartbeat so shouldRetryUrl sees "actively playing"
    (engine as any).lastHeartbeatAt = Date.now();
  });

  it('returns true on premature eof', () => {
    const result = (engine as any).shouldRetryUrl('eof', undefined);
    expect(result).toBe(true);
  });

  it('returns false on eof at duration', () => {
    state.get = vi.fn(() => ({ playlistIndex: 0, videoIndex: 0, videoId: '', videoTitle: '', currentTime: 598, videoDuration: 600, nextVideoId: '', updatedAt: '' }));
    const result = (engine as any).shouldRetryUrl('eof', undefined);
    expect(result).toBe(false);
  });

  it('returns true on network error', () => {
    const result = (engine as any).shouldRetryUrl('error', 'loading failed');
    expect(result).toBe(true);
  });

  it('returns true on tls error string', () => {
    expect((engine as any).shouldRetryUrl('error', 'tls: IO error')).toBe(true);
  });

  it('returns false on non-network error', () => {
    expect((engine as any).shouldRetryUrl('error', 'generic decode failure')).toBe(false);
  });

  it('returns false on error with no file_error', () => {
    expect((engine as any).shouldRetryUrl('error', undefined)).toBe(false);
  });

  it('returns false if no recent heartbeat (playlist-load phase)', () => {
    (engine as any).lastHeartbeatAt = Date.now() - 60_000;
    expect((engine as any).shouldRetryUrl('eof', undefined)).toBe(false);
  });

  it('returns false if videoDuration is zero (unknown)', () => {
    state.get = vi.fn(() => ({ playlistIndex: 0, videoIndex: 0, videoId: '', videoTitle: '', currentTime: 120, videoDuration: 0, nextVideoId: '', updatedAt: '' }));
    expect((engine as any).shouldRetryUrl('eof', undefined)).toBe(false);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `npm test -- --run src/server/__tests__/recovery.test.ts -t shouldRetryUrl`
Expected: All 8 new tests fail — method doesn't exist.

- [ ] **Step 3: Add private fields and the method**

In `src/server/recovery.ts`, in the class field declaration block (around `:56-60`, near `videoFreezeHeartbeats`), add two new fields:

```ts
  private videoFreezeHeartbeats = 0;
  private urlRetryCount = 0;
  private lastSeenVideoIndex = -1;
  private static readonly STALL_THRESHOLD = 3;
```

Now add `shouldRetryUrl` as a private method. Put it near the other private helpers — below `extractVideoId` is a good spot (after line 500):

```ts
  /** True when an end-file event looks like a signed-URL / CDN failure
   *  worth retrying in place (premature EOF or network error mid-playback). */
  private shouldRetryUrl(reason: string, fileError: string | undefined): boolean {
    // Ignore events fired outside active playback (e.g. during initial
    // playlist resolution). Mirrors the `elapsed > heartbeat * 2` guard
    // already used in onFileEnded's error branch.
    const elapsed = Date.now() - this.lastHeartbeatAt;
    if (elapsed > this.config.heartbeatIntervalMs * 2) return false;

    const { currentTime, videoDuration } = this.state.get();

    if (reason === 'eof') {
      // Need a known duration to call an EOF "premature".
      if (videoDuration <= 0) return false;
      return currentTime < videoDuration - 5;
    }

    if (reason === 'error' && fileError) {
      return /http|network|loading failed|tls|ssl/i.test(fileError);
    }

    return false;
  }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `npm test -- --run src/server/__tests__/recovery.test.ts -t shouldRetryUrl`
Expected: All 8 shouldRetryUrl tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/server/recovery.ts src/server/__tests__/recovery.test.ts
git commit -m "$(cat <<'EOF'
Add shouldRetryUrl detector and retry counter fields

shouldRetryUrl recognizes premature EOF (currentTime < duration-5) and
network errors (fileError matches http|network|tls|ssl|loading failed)
occurring during active playback. Ignores events from playlist-load.
Counter state fields added but not yet wired into onFileEnded.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Recovery engine — add retryCurrentAtPosition helper

**Files:**
- Modify: `src/server/recovery.ts` (add private method)
- Modify: `src/server/__tests__/recovery.test.ts` (add test)

- [ ] **Step 1: Write failing test**

Add to `src/server/__tests__/recovery.test.ts` inside a new describe block (after the shouldRetryUrl block):

```ts
describe('retryCurrentAtPosition', () => {
  it('sets start, jumps to current video, clears start after delay', async () => {
    vi.useFakeTimers();
    const mpv = mockMpv();
    const state = mockState({ videoIndex: 5 });
    const engine = new RecoveryEngine(
      makeConfig(),
      mpv as unknown as MpvClient,
      state as StateManager,
      mockObs(),
      { notifyError: vi.fn(), notifyRecovery: vi.fn(), notifyCritical: vi.fn(), notifyResume: vi.fn(), notifySkip: vi.fn(), notifyObsDisconnect: vi.fn(), notifyObsReconnect: vi.fn(), notifyStreamDrop: vi.fn(), notifyStreamRestart: vi.fn() } as unknown as DiscordNotifier,
    );

    await (engine as any).retryCurrentAtPosition(123);

    expect(mpv.setProperty).toHaveBeenCalledWith('start', '+123');
    expect(mpv.jumpTo).toHaveBeenCalledWith(5);

    // The clear-start timer fires at 30s
    vi.advanceTimersByTime(30_000);
    await Promise.resolve();
    expect(mpv.setProperty).toHaveBeenCalledWith('start', 'none');

    vi.useRealTimers();
  });

  it('rounds sub-second seek values to integer seconds for yt-dlp', async () => {
    const mpv = mockMpv();
    const state = mockState({ videoIndex: 0 });
    const engine = new RecoveryEngine(
      makeConfig(),
      mpv as unknown as MpvClient,
      state as StateManager,
      mockObs(),
      { notifyError: vi.fn(), notifyRecovery: vi.fn(), notifyCritical: vi.fn(), notifyResume: vi.fn(), notifySkip: vi.fn(), notifyObsDisconnect: vi.fn(), notifyObsReconnect: vi.fn(), notifyStreamDrop: vi.fn(), notifyStreamRestart: vi.fn() } as unknown as DiscordNotifier,
    );
    await (engine as any).retryCurrentAtPosition(45.7);
    expect(mpv.setProperty).toHaveBeenCalledWith('start', '+45');
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm test -- --run src/server/__tests__/recovery.test.ts -t retryCurrentAtPosition`
Expected: FAIL — method doesn't exist.

- [ ] **Step 3: Implement retryCurrentAtPosition**

In `src/server/recovery.ts`, add after `shouldRetryUrl`:

```ts
  /** Reload the current playlist item at a given seek position via IPC.
   *  Triggers yt-dlp re-resolution (refreshes googlevideo URL) and
   *  resumes near the break. Clears the start flag after 30s so it
   *  doesn't leak to auto-advanced videos. */
  private async retryCurrentAtPosition(seekSeconds: number): Promise<void> {
    const pos = this.state.get().videoIndex;
    const secs = Math.floor(Math.max(0, seekSeconds));
    try { await this.mpv.setProperty('start', `+${secs}`); } catch { /* ignore */ }
    try { await this.mpv.jumpTo(pos); } catch { /* ignore */ }
    setTimeout(async () => {
      try { await this.mpv.setProperty('start', 'none'); } catch { /* ignore */ }
    }, 30_000);
  }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `npm test -- --run src/server/__tests__/recovery.test.ts -t retryCurrentAtPosition`
Expected: Both tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/server/recovery.ts src/server/__tests__/recovery.test.ts
git commit -m "$(cat <<'EOF'
Add retryCurrentAtPosition helper

Sets mpv start=+<sec>, calls jumpTo(currentIndex) to trigger ytdl_hook
re-resolution, then clears start after 30s so the flag doesn't leak
into auto-advanced videos.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Wire retry into onFileEnded

**Files:**
- Modify: `src/server/recovery.ts:63` (bound handler signature), `:248` (onFileEnded signature and body)
- Modify: `src/server/__tests__/recovery.test.ts` (add integration test)

- [ ] **Step 1: Write failing tests**

Add to `src/server/__tests__/recovery.test.ts`:

```ts
describe('onFileEnded with premature-EOF retry', () => {
  function buildEngine(stateOverrides = {}) {
    const mpv = mockMpv();
    const state = mockState({ videoIndex: 3, videoDuration: 600, currentTime: 120, ...stateOverrides });
    const engine = new RecoveryEngine(
      makeConfig(),
      mpv as unknown as MpvClient,
      state as StateManager,
      mockObs(),
      { notifyError: vi.fn(), notifyRecovery: vi.fn(), notifyCritical: vi.fn(), notifyResume: vi.fn(), notifySkip: vi.fn(), notifyObsDisconnect: vi.fn(), notifyObsReconnect: vi.fn(), notifyStreamDrop: vi.fn(), notifyStreamRestart: vi.fn() } as unknown as DiscordNotifier,
    );
    engine.start();
    (engine as any).lastHeartbeatAt = Date.now();
    return { engine, mpv, state };
  }

  it('fires retry on premature eof and increments counter', async () => {
    const { engine, mpv } = buildEngine();
    mpv._emit('fileEnded', 'eof', undefined);
    await new Promise((r) => setImmediate(r));
    expect(mpv.setProperty).toHaveBeenCalledWith('start', '+120');
    expect(mpv.jumpTo).toHaveBeenCalledWith(3);
    expect((engine as any).urlRetryCount).toBe(1);
    engine.stop();
  });

  it('fires retry on network error and increments counter', async () => {
    const { engine, mpv } = buildEngine();
    mpv._emit('fileEnded', 'error', 'loading failed');
    await new Promise((r) => setImmediate(r));
    expect(mpv.jumpTo).toHaveBeenCalledWith(3);
    expect((engine as any).urlRetryCount).toBe(1);
    engine.stop();
  });

  it('stops retrying after 2 attempts for same video and falls through', async () => {
    const { engine, mpv } = buildEngine();
    mpv._emit('fileEnded', 'eof', undefined);
    await new Promise((r) => setImmediate(r));
    mpv._emit('fileEnded', 'eof', undefined);
    await new Promise((r) => setImmediate(r));
    expect((engine as any).urlRetryCount).toBe(2);

    // Third call: should not call jumpTo again (should fall through; but
    // this video was "playing" so the error path would increment
    // consecutiveErrors instead). We just verify jumpTo wasn't called a 3rd time.
    mpv.jumpTo.mockClear();
    mpv._emit('fileEnded', 'eof', undefined);
    await new Promise((r) => setImmediate(r));
    expect(mpv.jumpTo).not.toHaveBeenCalled();
    engine.stop();
  });

  it('does not fire retry on normal eof at duration', async () => {
    const { engine, mpv } = buildEngine({ currentTime: 599, videoDuration: 600 });
    mpv._emit('fileEnded', 'eof', undefined);
    await new Promise((r) => setImmediate(r));
    expect(mpv.jumpTo).not.toHaveBeenCalled();
    expect((engine as any).urlRetryCount).toBe(0);
    engine.stop();
  });
});
```

- [ ] **Step 2: Run test to verify failures**

Run: `npm test -- --run src/server/__tests__/recovery.test.ts -t "premature-EOF retry"`
Expected: Tests fail because onFileEnded doesn't yet call retryCurrentAtPosition.

- [ ] **Step 3: Update bound handler signature**

In `src/server/recovery.ts` at `:64`:

```ts
  private boundOnFileEnded = (reason: string, fileError?: string) => this.onFileEnded(reason, fileError);
```

- [ ] **Step 4: Modify onFileEnded signature and add retry branch**

Replace the `onFileEnded` method at `src/server/recovery.ts:248`. Change its signature and insert the new branch at the top:

```ts
  private async onFileEnded(reason: string, fileError?: string) {
    // In-place URL retry for premature EOF / network errors.
    // Runs before the existing error/eof handling so a signed-URL
    // expiry doesn't burn a consecutiveErrors slot or get skipped.
    if (this.shouldRetryUrl(reason, fileError)) {
      if (this.urlRetryCount < 2) {
        this.urlRetryCount++;
        const seek = this.state.get().currentTime;
        logger.warn({ reason, fileError, seek, attempt: this.urlRetryCount }, 'Premature stream end — retrying in place');
        this.addEvent(`Premature stream end (${reason}) — retrying at ${Math.floor(seek)}s (attempt ${this.urlRetryCount}/2)`);
        await this.discord.notifyRecovery('URL retry');
        await this.retryCurrentAtPosition(seek);
        return;
      }
      // Retries exhausted for this video — fall through to existing logic
      logger.warn({ videoIndex: this.state.get().videoIndex }, 'URL retries exhausted — falling through to error handling');
      this.addEvent('URL retries exhausted — escalating to error handling');
    }

    if (reason === 'error') {
```

Keep the rest of the existing `onFileEnded` body (the `reason === 'error'` block and the `else if (reason === 'eof')` block) exactly as-is.

- [ ] **Step 5: Run tests**

Run: `npm test -- --run src/server/__tests__/recovery.test.ts`
Expected: All recovery tests pass, including the new premature-EOF retry block.

- [ ] **Step 6: Commit**

```bash
git add src/server/recovery.ts src/server/__tests__/recovery.test.ts
git commit -m "$(cat <<'EOF'
Trigger in-place retry on premature EOF / network errors

onFileEnded now runs shouldRetryUrl before the existing error/eof
logic. When it matches and urlRetryCount < 2, we fire
retryCurrentAtPosition with the last known playback time. After 2
retries for the same video, falls through to existing handling.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Reset retry counter when video changes

**Files:**
- Modify: `src/server/recovery.ts` (processHeartbeat around `:387-389`)
- Modify: `src/server/__tests__/recovery.test.ts` (add test)

- [ ] **Step 1: Write failing test**

Add to `src/server/__tests__/recovery.test.ts`:

```ts
describe('urlRetryCount reset on video change', () => {
  it('resets urlRetryCount when playlist-pos changes in heartbeat', async () => {
    const mpv = mockMpv();
    const state = mockState({ videoIndex: 3, videoDuration: 600, currentTime: 120 });
    const engine = new RecoveryEngine(
      makeConfig(),
      mpv as unknown as MpvClient,
      state as StateManager,
      mockObs(),
      { notifyError: vi.fn(), notifyRecovery: vi.fn(), notifyCritical: vi.fn(), notifyResume: vi.fn(), notifySkip: vi.fn(), notifyObsDisconnect: vi.fn(), notifyObsReconnect: vi.fn(), notifyStreamDrop: vi.fn(), notifyStreamRestart: vi.fn() } as unknown as DiscordNotifier,
    );
    (engine as any).urlRetryCount = 2;
    (engine as any).lastSeenVideoIndex = 3;

    // Heartbeat on same video — no reset
    (engine as any).processHeartbeat({
      timePos: 130, duration: 600, paused: false, idle: false,
      playlistPos: 3, playlistCount: 10, mediaTitle: 't', filename: 'f',
      hasVideo: true, vfps: 30,
    });
    expect((engine as any).urlRetryCount).toBe(2);

    // Heartbeat on a different video — counter resets
    (engine as any).processHeartbeat({
      timePos: 5, duration: 600, paused: false, idle: false,
      playlistPos: 4, playlistCount: 10, mediaTitle: 't', filename: 'f',
      hasVideo: true, vfps: 30,
    });
    expect((engine as any).urlRetryCount).toBe(0);
    expect((engine as any).lastSeenVideoIndex).toBe(4);
  });
});
```

- [ ] **Step 2: Run test to verify failure**

Run: `npm test -- --run src/server/__tests__/recovery.test.ts -t "urlRetryCount reset"`
Expected: FAIL — counter stays at 2 because the reset isn't wired yet.

- [ ] **Step 3: Wire the reset in processHeartbeat**

In `src/server/recovery.ts`, in `processHeartbeat`, find the block at `:386-389` that reads:

```ts
    // Update totalVideos from playlist count
    if (hb.playlistCount > 0) {
      this.totalVideos = hb.playlistCount;
    }
```

Add the counter reset directly above it:

```ts
    // Reset URL-retry counter whenever the playlist position changes
    // (auto-advance, successful retry that played through, manual jump).
    if (hb.playlistPos !== this.lastSeenVideoIndex) {
      this.urlRetryCount = 0;
      this.lastSeenVideoIndex = hb.playlistPos;
    }

    // Update totalVideos from playlist count
    if (hb.playlistCount > 0) {
      this.totalVideos = hb.playlistCount;
    }
```

- [ ] **Step 4: Run test**

Run: `npm test -- --run src/server/__tests__/recovery.test.ts`
Expected: All recovery tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/server/recovery.ts src/server/__tests__/recovery.test.ts
git commit -m "$(cat <<'EOF'
Reset urlRetryCount when playlist position changes

On any heartbeat with a new playlist-pos (auto-advance, post-retry
success that played through, manual jump), clear the retry counter
so the next video starts with a fresh 2-attempt budget.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Full test suite + smoke test

**Files:** none modified

- [ ] **Step 1: Run the full suite**

Run: `npm test -- --run`
Expected: All tests pass. No regressions.

- [ ] **Step 2: Type-check**

Run: `npx tsc --noEmit`
Expected: No errors.

- [ ] **Step 3: Dev server smoke test**

Run: `npm start`. In a browser, open the admin dashboard.

- Verify the dashboard loads without errors.
- In the Playback tab > Maintenance section, confirm the new "yt-dlp cookies from browser" field is visible.
- Enter a value (e.g. `brave`), click Save Playback Settings.
- Reload the page; confirm the value persisted.
- Clear the field, save again; confirm it saves as empty.

Stop the dev server.

- [ ] **Step 4: Commit (if any minor fixups)**

If steps 1-3 uncovered a typo or missing file, commit it now. Otherwise skip.

---

## Task 11: Version bump + release notes

**Files:**
- Modify: `package.json` (version)
- Modify: `RELEASE_NOTES.md` (new entry)

- [ ] **Step 1: Check current version and decide next**

Run: `node -p "require('./package.json').version"`
Recent commits use v2.1.x for bug fixes. Bump to v2.1.6 (still a patch — no API changes, two user-visible behavior changes gated behind config / existing recovery paths).

- [ ] **Step 2: Update package.json**

Edit `package.json`, change the `"version"` field to `"2.1.6"`.

- [ ] **Step 3: Update RELEASE_NOTES.md**

Open `RELEASE_NOTES.md`. At the top, above the most recent entry, add:

```markdown
## v2.1.6

- Added `ytdlCookiesFromBrowser` config (admin dashboard > Playback > Maintenance). When set (e.g. `brave`, `chrome:Profile 1`), yt-dlp passes the logged-in browser session via `--cookies-from-browser`. Resolves YouTube's "Sign in to confirm you're not a bot" challenge that blocks playback on flagged IPs.
- Freeze recovery: premature stream-EOF and network errors (HTTP 5xx, TLS) now trigger an in-place retry at the last known playback position via `jumpTo(currentIndex)`. yt-dlp re-signs the googlevideo URL and playback resumes near the break. Up to 2 retries per video before falling through to the existing skip path. `keep-open=yes` preserved so viewers see the last frame during the ~2-5s retry, not a black cut.
```

- [ ] **Step 4: Commit**

```bash
git add package.json RELEASE_NOTES.md
git commit -m "$(cat <<'EOF'
v2.1.6: yt-dlp cookies config + premature-EOF in-place retry

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Final verification

- [ ] **Step 1: Full test suite + typecheck one more time**

```bash
npm test -- --run && npx tsc --noEmit
```
Expected: All green.

- [ ] **Step 2: Review the diff**

```bash
git log --oneline master..HEAD
git diff master..HEAD --stat
```

Sanity check — touched files match the File Map at the top of this plan.

- [ ] **Step 3: Delete FREEZE_INVESTIGATION.md**

The investigation doc was a working note that led to this fix. The design spec and RELEASE_NOTES entry now carry the context. Remove it.

```bash
git rm FREEZE_INVESTIGATION.md
git commit -m "$(cat <<'EOF'
Remove FREEZE_INVESTIGATION.md

Context rolled into docs/superpowers/specs/2026-04-19-ytdlp-cookies-and-freeze-fix-design.md
and the v2.1.6 release notes.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 4: Done**

Work is ready to push. Do NOT push or build the release ZIP — that is a separate user-initiated step per the repo's release checklist in `CLAUDE.md`.
