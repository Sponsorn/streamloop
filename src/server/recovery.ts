import { freemem, totalmem } from 'os';
import { performance } from 'node:perf_hooks';
import { RecoveryStep, type AppConfig, type MpvHeartbeat, type EventLogEntry } from './types.js';
import type { MpvClient } from './mpv-client.js';
import type { StateManager } from './state.js';
import type { OBSClient } from './obs-client.js';
import type { DiscordNotifier } from './discord.js';
import { logger } from './logger.js';
import { FrameMonitor } from './frame-monitor.js';
import type { EventStore } from './event-store.js';

function getSystemMemory() {
  const totalBytes = totalmem();
  const freeBytes = freemem();
  const usedBytes = totalBytes - freeBytes;
  return {
    totalGB: +(totalBytes / 1073741824).toFixed(1),
    freeGB: +(freeBytes / 1073741824).toFixed(1),
    usedGB: +(usedBytes / 1073741824).toFixed(1),
    usedPercent: Math.round((usedBytes / totalBytes) * 100),
  };
}

const MAX_EVENT_LOG = 100;

export class RecoveryEngine {
  private config: AppConfig;
  private mpv: MpvClient;
  private state: StateManager;
  private obs: OBSClient;
  private discord: DiscordNotifier;
  private eventStore: EventStore | null;

  private consecutiveErrors = 0;
  private lastHeartbeatAt = Date.now();
  private heartbeatPollTimer: ReturnType<typeof setInterval> | null = null;
  private recoveryStep = RecoveryStep.None;
  private recoveryReason: 'heartbeat' | 'stall' | 'non-playing' | null = null;
  private recoveryTimer: ReturnType<typeof setTimeout> | null = null;
  private totalVideos = 0;
  private startedAt = Date.now();
  private eventLog: EventLogEntry[] = [];
  private lastProgressTime = 0;
  private stalledHeartbeats = 0;
  private nonPlayingHeartbeats = 0;
  private consecutivePausedHeartbeats = 0;
  private intentionallyStopped = false;
  private lastKnownPaused = false;
  private lastPlaying = false;
  private lastTimePos = 0;
  private frameMonitor: FrameMonitor | null = null;
  private seekPending = false;
  private videoConfirmed = false;
  private periodicRestartTimer: ReturnType<typeof setInterval> | null = null;
  private videoFreezeHeartbeats = 0;
  private urlRetryCount = 0;
  private videoFreezeRetryCount = 0;
  private lastSeenVideoIndex = -1;
  /** Monotonic timestamp (performance.now(), ms) of when the current video's URL
   *  was last resolved (file-loaded). Drives the proactive pre-expiry refresh.
   *  Monotonic, NOT Date.now(): a wall-clock step (NTP correcting a bad RTC after
   *  a power outage) must not make a 6h-old URL look fresh, or a fresh one look
   *  expired. perf clock ticks at real rate and is immune to clock adjustments. */
  private urlResolvedAt = performance.now();
  private static readonly STALL_THRESHOLD = 3;
  private static readonly NON_PLAYING_THRESHOLD = 6;
  private static readonly VIDEO_FREEZE_THRESHOLD = 4;
  /** Don't treat the brief video-EOF burst at a video's natural end as a freeze. */
  private static readonly VIDEO_FREEZE_END_GUARD_SEC = 10;
  /** video-bitrate (bits/s) at or below this means no video bytes are arriving. */
  private static readonly FROZEN_VIDEO_BITRATE = 1000;
  /** In-place URL retries for a video freeze before escalating to a restart. */
  private static readonly MAX_VIDEO_FREEZE_RETRIES = 3;

  // Bound handlers so we can remove them from mpv EventEmitter
  private boundOnConnect = () => this.onMpvConnect();
  private boundOnDisconnect = () => this.onMpvDisconnect();
  private boundOnFileEnded = (reason: string, fileError?: string) => this.onFileEnded(reason, fileError);
  private boundOnProcessExit = () => this.onProcessExit();
  private boundOnFileLoaded = () => { this.urlResolvedAt = performance.now(); };

  constructor(
    config: AppConfig,
    mpv: MpvClient,
    state: StateManager,
    obs: OBSClient,
    discord: DiscordNotifier,
    eventStore?: EventStore,
  ) {
    this.config = config;
    this.mpv = mpv;
    this.state = state;
    this.obs = obs;
    this.discord = discord;
    this.eventStore = eventStore ?? null;
    if (this.eventStore) {
      this.eventLog = this.eventStore.loadRecent(MAX_EVENT_LOG);
    }
  }

  start() {
    // Remove any stale listeners (e.g. from a previous RecoveryEngine on the same mpv)
    this.removeMpvListeners();

    this.mpv.on('connected', this.boundOnConnect);
    this.mpv.on('disconnected', this.boundOnDisconnect);
    this.mpv.on('fileEnded', this.boundOnFileEnded);
    this.mpv.on('processExit', this.boundOnProcessExit);
    this.mpv.on('fileLoaded', this.boundOnFileLoaded);
    this.startHeartbeatPoll();
    this.startPeriodicRestartTimer();
    this.startFrameMonitor();
    if (this.mpv.isConnected()) {
      this.onMpvConnect();
    }
  }

  stop() {
    this.removeMpvListeners();
    this.stopHeartbeatPoll();
    this.stopPeriodicRestartTimer();
    this.frameMonitor?.stop();
    this.frameMonitor = null;
    this.clearRecoveryTimer();
  }

  private removeMpvListeners() {
    this.mpv.removeListener('connected', this.boundOnConnect);
    this.mpv.removeListener('disconnected', this.boundOnDisconnect);
    this.mpv.removeListener('fileEnded', this.boundOnFileEnded);
    this.mpv.removeListener('processExit', this.boundOnProcessExit);
    this.mpv.removeListener('fileLoaded', this.boundOnFileLoaded);
  }

  getStatus() {
    const currentState = this.state.get();
    const playlistIndex = currentState.playlistIndex < this.config.playlists.length
      ? currentState.playlistIndex : 0;
    return {
      recoveryStep: this.recoveryStep,
      lastHeartbeatAt: this.lastHeartbeatAt,
      consecutiveErrors: this.consecutiveErrors,
      totalVideos: this.totalVideos,
      uptimeMs: Date.now() - this.startedAt,
      playlistIndex,
      totalPlaylists: this.config.playlists.length,
      currentPlaylistId: this.config.playlists[playlistIndex].id,
      systemMemory: getSystemMemory(),
      mpvConnected: this.mpv.isConnected(),
      mpvRunning: this.mpv.isRunning(),
      paused: this.lastKnownPaused,
      intentionallyStopped: this.intentionallyStopped,
      videoConfirmed: this.videoConfirmed,
    };
  }

  getEvents(): EventLogEntry[] {
    return [...this.eventLog];
  }

  /** Mark playback as intentionally stopped — disables auto-resume */
  setIntentionallyStopped(stopped: boolean) {
    this.intentionallyStopped = stopped;
    if (stopped) {
      this.addEvent('Playback intentionally stopped');
    } else {
      this.addEvent('Playback resumed from intentional stop');
    }
  }

  isIntentionallyStopped(): boolean {
    return this.intentionallyStopped;
  }

  // --- Public: load playlist into mpv ---

  async loadCurrentPlaylist() {
    const savedState = this.state.get();
    const playlistIndex = savedState.playlistIndex < this.config.playlists.length
      ? savedState.playlistIndex : 0;
    const playlist = this.config.playlists[playlistIndex];
    const url = `https://www.youtube.com/playlist?list=${playlist.id}`;

    let seekTime = savedState.currentTime;
    const jumpIndex = savedState.videoIndex;

    // Guard against corrupted state: if the saved currentTime exceeds the
    // last known video duration, the seek would be impossible and every
    // video load would fail with start=+<too-large>, causing an infinite
    // recovery loop. Duration=0 means unknown (livestream or not yet loaded),
    // so we leave seekTime alone in that case.
    if (seekTime > 0 && savedState.videoDuration > 0 && seekTime >= savedState.videoDuration) {
      logger.warn({ seekTime, videoDuration: savedState.videoDuration }, 'Saved currentTime exceeds known video duration — discarding resume position');
      this.addEvent(`Discarding stale resume position (${Math.floor(seekTime)}s > ${Math.floor(savedState.videoDuration)}s video)`);
      this.state.update({ currentTime: 0 });
      seekTime = 0;
    }

    logger.info({ playlistId: playlist.id, videoIndex: jumpIndex, currentTime: seekTime }, 'Loading playlist in mpv');
    this.addEvent(`Loading playlist ${playlist.name || playlist.id}`);

    // Only pre-set start when targeting video 0.
    // For jumpIndex > 0, setting start here would cause video 0 to attempt
    // an impossible seek (e.g. 6 hours into a 10-minute video), triggering
    // end-file errors and preventing the fileLoaded → jumpTo sequence.
    if (seekTime > 0 && jumpIndex === 0) {
      logger.info({ seekTime }, 'Setting start position for resume');
      this.seekPending = true;
      await this.mpv.setProperty('start', `+${seekTime}`).catch(() => {});
    }

    await this.mpv.loadPlaylist(url);

    // Jump to saved video index after playlist loads
    if (jumpIndex > 0) {
      const onFileLoaded = async () => {
        this.mpv.removeListener('fileLoaded', onFileLoaded);
        try {
          // Set start position before jumping so yt-dlp requests the correct byte range
          if (seekTime > 0) {
            this.seekPending = true;
            await this.mpv.setProperty('start', `+${seekTime}`).catch(() => {});
          }
          await this.mpv.jumpTo(jumpIndex);
        } catch { /* ignore */ }
      };
      this.mpv.on('fileLoaded', onFileLoaded);
      setTimeout(() => {
        this.mpv.removeListener('fileLoaded', onFileLoaded);
      }, 15000);
    }

    // Clear start property after initial load so future videos start at 0
    setTimeout(async () => {
      await this.mpv.setProperty('start', 'none').catch(() => {});
    }, 30000);
  }

  // --- Private: event log ---

  private addEvent(message: string) {
    const entry = { timestamp: new Date().toISOString(), message };
    this.eventLog.push(entry);
    if (this.eventLog.length > MAX_EVENT_LOG) {
      this.eventLog.shift();
    }
    this.eventStore?.append(entry);
  }

  // --- Private: mpv event handlers ---

  private async onMpvConnect() {
    const resumeInfo = { videoIndex: this.state.get().videoIndex, currentTime: this.state.get().currentTime };
    logger.info(resumeInfo, 'mpv connected, loading playlist');
    this.addEvent(`mpv connected — resuming video #${resumeInfo.videoIndex} at ${Math.floor(resumeInfo.currentTime)}s`);
    this.resetRecovery();
    this.lastHeartbeatAt = Date.now();
    this.nonPlayingHeartbeats = 0;
    this.videoFreezeHeartbeats = 0;
    this.videoFreezeRetryCount = 0;
    this.urlResolvedAt = performance.now();
    await this.loadCurrentPlaylist();
  }

  private onMpvDisconnect() {
    logger.warn('mpv disconnected');
    this.addEvent('mpv disconnected');
    this.videoConfirmed = false;
    // Heartbeat poll will detect timeout and trigger recovery
  }

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
      // If a seek was pending, the error is likely due to YouTube rejecting the seek position.
      // Retry the same video from the beginning instead of counting it as a real error.
      if (this.seekPending) {
        this.seekPending = false;
        logger.warn('Seek failed (YouTube may have rejected the position), replaying from start');
        this.addEvent('Seek to saved position failed — replaying from start');
        this.state.update({ currentTime: 0 });
        // Clear mpv's start property so the bad seek isn't re-applied to
        // subsequent video loads (otherwise every auto-advanced video fails
        // the same way until the 30s cleanup timer fires).
        await this.mpv.setProperty('start', 'none').catch(() => {});
        return;
      }
      // Ignore errors during initial playlist load or when nothing was playing
      const elapsed = Date.now() - this.lastHeartbeatAt;
      if (elapsed > this.config.heartbeatIntervalMs * 2) {
        logger.debug({ reason }, 'Ignoring end-file error (no recent heartbeat, likely playlist loading)');
        return;
      }
      this.consecutiveErrors++;
      const { videoIndex, videoId } = this.state.get();
      logger.error({ videoIndex, videoId, consecutiveErrors: this.consecutiveErrors }, 'mpv playback error');
      this.addEvent(`Playback error on video #${videoIndex} (${videoId})`);
      await this.discord.notifyError(videoIndex, videoId, 0, this.consecutiveErrors);
      // mpv is actively cycling through videos — not stuck — so don't let
      // the non-playing counter escalate to a restart that would throw away
      // the skip progress and start the cycle over from position 0.
      this.nonPlayingHeartbeats = 0;
      if (this.consecutiveErrors >= this.config.maxConsecutiveErrors) {
        const reason = `${this.consecutiveErrors} consecutive playback errors`;
        this.addEvent(`Skipping video #${videoIndex} (${videoId}): ${reason}`);
        await this.discord.notifySkip(videoIndex, videoId, reason);
        try { await this.mpv.next(); } catch { /* ignore */ }
        this.consecutiveErrors = 0;
      }
    } else if (reason === 'eof') {
      this.consecutiveErrors = 0;
      // Check if we need to advance to next playlist
      if (this.totalVideos > 0 && this.config.playlists.length > 1) {
        const currentState = this.state.get();
        if (currentState.videoIndex >= this.totalVideos - 1) {
          await this.advanceToNextPlaylist();
        }
      }
    }
  }

  private onProcessExit() {
    logger.warn('mpv process exited');
    this.addEvent('mpv process exited');
  }

  // --- Private: heartbeat polling ---

  private startHeartbeatPoll() {
    this.lastHeartbeatAt = Date.now();
    this.heartbeatPollTimer = setInterval(async () => {
      // Check for heartbeat timeout regardless of connection state.
      // When mpv freezes (alive but unresponsive), the socket stays open
      // so isConnected() is true, but IPC queries hang and lastHeartbeatAt
      // never updates. This catches that case.
      const elapsed = Date.now() - this.lastHeartbeatAt;
      if (elapsed > this.config.heartbeatTimeoutMs && this.recoveryStep === RecoveryStep.None) {
        const mem = getSystemMemory();
        logger.warn({ elapsedMs: elapsed, systemMemory: mem }, 'Heartbeat timeout, starting recovery');
        this.addEvent(`Heartbeat timeout (${Math.round(elapsed / 1000)}s), starting recovery (RAM: ${mem.usedGB}/${mem.totalGB}GB, ${mem.usedPercent}%)`);
        this.recoveryReason = 'heartbeat';
        this.startRecoverySequence();
        return;
      }
      if (!this.mpv.isConnected()) return;
      try {
        const hb = await this.pollMpvState();
        this.lastHeartbeatAt = Date.now();
        this.processHeartbeat(hb);
      } catch {
        // mpv may be restarting or unresponsive — timeout check above will catch it
      }
    }, this.config.heartbeatIntervalMs);
  }

  private stopHeartbeatPoll() {
    if (this.heartbeatPollTimer) {
      clearInterval(this.heartbeatPollTimer);
      this.heartbeatPollTimer = null;
    }
  }

  private async pollMpvState(): Promise<MpvHeartbeat> {
    const [timePos, duration, paused, idle, playlistPos, playlistCount, mediaTitle, filename, videoParams, vfps, videoBitrate, audioBitrate] =
      await Promise.all([
        this.mpv.getProperty('time-pos').catch(() => 0),
        this.mpv.getProperty('duration').catch(() => 0),
        this.mpv.getProperty('pause').catch(() => false),
        this.mpv.getProperty('idle-active').catch(() => true),
        this.mpv.getProperty('playlist-pos').catch(() => 0),
        this.mpv.getProperty('playlist-count').catch(() => 0),
        this.mpv.getProperty('media-title').catch(() => ''),
        this.mpv.getProperty('filename').catch(() => ''),
        this.mpv.getProperty('video-params').catch(() => null),
        this.mpv.getProperty('estimated-vf-fps').catch(() => 0),
        this.mpv.getProperty('video-bitrate').catch(() => null),
        this.mpv.getProperty('audio-bitrate').catch(() => null),
      ]);
    return {
      timePos: timePos as number,
      duration: duration as number,
      paused: paused as boolean,
      idle: idle as boolean,
      playlistPos: playlistPos as number,
      playlistCount: playlistCount as number,
      mediaTitle: mediaTitle as string,
      filename: filename as string,
      hasVideo: videoParams != null,
      vfps: (vfps as number) || 0,
      // -1 means "unknown" so the freeze check never trips on a missing reading.
      videoBitrate: typeof videoBitrate === 'number' ? videoBitrate : -1,
      audioBitrate: typeof audioBitrate === 'number' ? audioBitrate : -1,
    };
  }

  // --- Private: heartbeat processing ---

  /** @internal — exposed name for testing via poll timer */
  private processHeartbeat(hb: MpvHeartbeat) {
    const isPlaying = !hb.paused && !hb.idle && hb.timePos > 0;

    // Track whether video is confirmed rendering (not just audio/black screen)
    if (isPlaying && hb.hasVideo) {
      if (!this.videoConfirmed) {
        this.videoConfirmed = true;
        logger.info('Video confirmed rendering — playback is visible');
        this.addEvent('Video confirmed rendering');
      }
    }
    // Clear seekPending once playback is confirmed — prevents stale flag
    // from swallowing a real error later
    if (isPlaying) {
      this.seekPending = false;
    }
    this.lastKnownPaused = hb.paused;
    this.lastPlaying = isPlaying;
    this.lastTimePos = hb.timePos;
    const videoId = this.extractVideoId(hb.filename);
    const mediaTitle = this.sanitizeTitle(hb.mediaTitle);

    // Reset URL-retry counter whenever the playlist position changes
    // (auto-advance, successful retry that played through, manual jump).
    if (hb.playlistPos !== this.lastSeenVideoIndex) {
      this.urlRetryCount = 0;
      this.videoFreezeRetryCount = 0;
      this.lastSeenVideoIndex = hb.playlistPos;
    }

    // Update totalVideos from playlist count
    if (hb.playlistCount > 0) {
      this.totalVideos = hb.playlistCount;
    }

    // Stall detection: mpv claims to be playing but timePos isn't advancing
    if (isPlaying) {
      if (Math.abs(hb.timePos - this.lastProgressTime) < 1) {
        this.stalledHeartbeats++;
        if (this.stalledHeartbeats >= RecoveryEngine.STALL_THRESHOLD && this.recoveryStep === RecoveryStep.None) {
          const mem = getSystemMemory();
          const stallMsg = `Player stalled at ${Math.floor(hb.timePos)}s on video #${hb.playlistPos} (${videoId}) — no progress for ${this.stalledHeartbeats} heartbeats (RAM: ${mem.usedGB}/${mem.totalGB}GB, ${mem.usedPercent}%)`;
          logger.warn({ timePos: hb.timePos, stalledHeartbeats: this.stalledHeartbeats, playlistPos: hb.playlistPos, videoId, systemMemory: mem }, 'Player stalled — video not advancing');
          this.addEvent(stallMsg);
          this.discord.notifyRecovery('Stall detected');
          this.recoveryReason = 'stall';
          this.startRecoverySequence();
        }
      } else {
        this.stalledHeartbeats = 0;
        this.lastProgressTime = hb.timePos;
        // Don't cancel recovery if video freeze is still active —
        // audio advancing doesn't mean the freeze resolved
        if (this.videoFreezeHeartbeats < RecoveryEngine.VIDEO_FREEZE_THRESHOLD) {
          this.resetRecovery();
        }
      }
    } else {
      this.stalledHeartbeats = 0;
      this.lastProgressTime = hb.timePos;
      if (this.recoveryStep !== RecoveryStep.None && isPlaying) {
        this.resetRecovery();
      }
    }

    // Video freeze detection: audio keeps flowing (time-pos advances) but the
    // video stream has stalled. YouTube serves video and audio as separate DASH
    // streams; when the *video* stream stalls/EOFs, mpv holds the last frame and
    // never emits a file-level end-file (the file stays alive on audio), so this
    // heartbeat check is the only thing that can catch it. Two independent
    // signals: estimated-vf-fps collapses, and/or video-bitrate drops to ~0
    // while audio-bitrate keeps flowing.
    const nearEndOfFile = hb.duration > 0
      && hb.duration - hb.timePos < RecoveryEngine.VIDEO_FREEZE_END_GUARD_SEC;
    const videoFramesStalled = hb.vfps < 1;
    const videoBytesStalled = hb.videoBitrate >= 0 && hb.videoBitrate < RecoveryEngine.FROZEN_VIDEO_BITRATE;
    const videoStalled = videoFramesStalled || videoBytesStalled;

    if (isPlaying && this.videoConfirmed && videoStalled && !nearEndOfFile) {
      this.videoFreezeHeartbeats++;
      if (this.videoFreezeHeartbeats >= RecoveryEngine.VIDEO_FREEZE_THRESHOLD && this.recoveryStep === RecoveryStep.None) {
        this.handleVideoFreeze(hb.timePos, 'Video freeze', {
          vfps: hb.vfps, videoBitrate: hb.videoBitrate, audioBitrate: hb.audioBitrate,
          videoFreezeHeartbeats: this.videoFreezeHeartbeats, playlistPos: hb.playlistPos, videoId,
        });
      }
    } else if (this.videoFreezeHeartbeats > 0) {
      // Tolerate a single healthy heartbeat (vfps/bitrate can blip) without
      // fully resetting — decrement so a real sustained freeze still escalates.
      this.videoFreezeHeartbeats--;
    }

    // Proactive signed-URL refresh: a YouTube googlevideo URL expires ~6h after
    // yt-dlp resolves it, so any video longer than the TTL freezes/EOFs mid-play
    // around the 6h mark. Pre-empt it with an in-place reload once the URL has
    // aged past the configured threshold, turning an unplanned freeze (+ detection
    // lag, + possible restart) into a planned ~few-second rebuffer. Short videos
    // end and reset urlResolvedAt long before the threshold, so this never fires
    // for them. nearEndOfFile skips a pointless reload right before a natural end.
    if (this.config.proactiveUrlRefreshMs > 0
      && isPlaying && this.videoConfirmed && !hb.paused
      && !this.intentionallyStopped && this.recoveryStep === RecoveryStep.None
      && !nearEndOfFile
      && performance.now() - this.urlResolvedAt >= this.config.proactiveUrlRefreshMs) {
      const ageMin = Math.round((performance.now() - this.urlResolvedAt) / 60000);
      logger.info({ ageMin, timePos: hb.timePos, playlistPos: hb.playlistPos, videoId }, 'Proactive signed-URL refresh before TTL expiry');
      this.addEvent(`Proactive URL refresh at ${Math.floor(hb.timePos)}s (URL ~${ageMin}min old) — pre-empting signed-URL expiry`);
      // Stamp now so we don't re-fire on the next heartbeat while the reload is
      // in flight; the resulting file-loaded resets it to the precise moment.
      this.urlResolvedAt = performance.now();
      this.retryCurrentAtPosition(hb.timePos);
    }

    // Only write state when video is making progress and not in recovery
    // (during recovery, mpv may temporarily report playlistPos=0 which
    // would overwrite the correct resume position)
    if (this.stalledHeartbeats < RecoveryEngine.STALL_THRESHOLD && this.recoveryStep === RecoveryStep.None) {
      const update: Record<string, unknown> = {
        videoIndex: hb.playlistPos,
        videoId,
        videoTitle: mediaTitle,
        videoDuration: hb.duration,
      };
      if (isPlaying || hb.timePos > 0) {
        update.currentTime = hb.timePos;
      }
      this.state.update(update);
    }

    // Auto-resume if paused (unless intentionally stopped via dashboard)
    if (hb.paused && !this.intentionallyStopped) {
      this.consecutivePausedHeartbeats++;
      if (this.consecutivePausedHeartbeats >= 2) {
        logger.info('Video paused, auto-resuming');
        this.addEvent('Video paused — auto-resuming');
        this.mpv.play().catch(() => {});
        this.consecutivePausedHeartbeats = 0;
      }
    } else {
      this.consecutivePausedHeartbeats = 0;
    }

    // Non-playing detection: mpv is connected but stuck in idle/buffering
    if (isPlaying || hb.paused) {
      this.nonPlayingHeartbeats = 0;
    } else if (this.totalVideos > 0) {
      this.nonPlayingHeartbeats++;
      if (this.nonPlayingHeartbeats >= RecoveryEngine.NON_PLAYING_THRESHOLD && this.recoveryStep === RecoveryStep.None) {
        const npMsg = `Player not playing for ${this.nonPlayingHeartbeats} heartbeats on video #${hb.playlistPos} (${videoId})`;
        logger.warn({ paused: hb.paused, idle: hb.idle, heartbeats: this.nonPlayingHeartbeats, playlistPos: hb.playlistPos, videoId }, 'Player stuck in non-playing state');
        this.addEvent(npMsg);
        this.discord.notifyRecovery('Non-playing recovery');
        this.recoveryReason = 'non-playing';
        this.startRecoverySequence();
      }
    }
  }

  private sanitizeTitle(title: string): string {
    if (!title) return '';
    // Discard titles that look like HTML/CSS content (yt-dlp resolution failure)
    if (/[{}<>]/.test(title) || /^\s*(body|html|div|span|a,)\b/i.test(title) || /:\s*\d+%/.test(title)) {
      return '';
    }
    return title;
  }

  private extractVideoId(filename: string): string {
    if (!filename) return '';
    // Try YouTube URL patterns
    const match = filename.match(/(?:v=|youtu\.be\/|\/watch\?.*v=)([A-Za-z0-9_-]{11})/);
    if (match) return match[1];
    // Try bare 11-char video ID (mpv sometimes reports just the ID)
    const bare = filename.match(/^([A-Za-z0-9_-]{11})$/);
    return bare ? bare[1] : '';
  }

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

  /** Reload the current playlist item at a given seek position via IPC.
   *  Triggers yt-dlp re-resolution (refreshes googlevideo URL) and
   *  resumes near the break. Clears the start flag after 30s so it
   *  doesn't leak to auto-advanced videos. */
  private async retryCurrentAtPosition(seekSeconds: number): Promise<void> {
    const pos = this.state.get().videoIndex;
    const secs = Math.floor(Math.max(0, seekSeconds));
    // Order matters: the start position must be registered before the reload
    // triggers yt-dlp re-resolution, or the seek silently won't apply.
    try { await this.mpv.setProperty('start', `+${secs}`); } catch { /* ignore */ }
    // Force a reload via playlist-play-index, NOT jumpTo. During a video-stream
    // freeze mpv is still "playing" this same index, so setting playlist-pos to
    // its current value is a no-op and the stream never re-resolves — the frozen
    // frame just persists until the audio track EOFs. playlist-play-index restarts
    // the entry unconditionally, which is what actually refreshes the URL.
    try { await this.mpv.reloadIndex(pos); } catch { /* ignore */ }
    setTimeout(async () => {
      try { await this.mpv.setProperty('start', 'none'); } catch { /* ignore */ }
    }, 30_000);
  }

  /** Shared freeze-recovery path for both the bitrate/vfps detector and the
   *  screenshot detector. Spends the in-place URL-retry budget first, then
   *  escalates to the standard recovery sequence. No-op if already recovering. */
  private handleVideoFreeze(seekSeconds: number, label: 'Video freeze' | 'Output freeze', detail: Record<string, unknown>) {
    if (this.recoveryStep !== RecoveryStep.None) return;
    const mem = getSystemMemory();
    const pos = Math.floor(seekSeconds);
    // The bitrate/vfps detector sees audio advancing while video bytes stall;
    // the screenshot detector sees a frozen picture without knowing the audio state.
    const symptom = label === 'Output freeze' ? 'streamed picture frozen' : 'audio playing but video stalled';
    if (this.videoFreezeRetryCount < RecoveryEngine.MAX_VIDEO_FREEZE_RETRIES) {
      this.videoFreezeRetryCount++;
      logger.warn({ ...detail, timePos: seekSeconds, attempt: this.videoFreezeRetryCount, systemMemory: mem }, `${label} — retrying URL in place`);
      this.addEvent(`${label} at ${pos}s — ${symptom} — URL retry in place (attempt ${this.videoFreezeRetryCount}/${RecoveryEngine.MAX_VIDEO_FREEZE_RETRIES})`);
      this.discord.notifyRecovery(`${label} — URL retry`);
      this.videoFreezeHeartbeats = 0; // cooldown: require a fresh window before re-firing
      this.retryCurrentAtPosition(seekSeconds);
    } else {
      logger.warn({ ...detail, timePos: seekSeconds, systemMemory: mem }, `${label} URL retries exhausted — escalating to recovery sequence`);
      this.addEvent(`${label} retries exhausted — escalating recovery`);
      this.recoveryReason = 'stall';
      this.startRecoverySequence();
    }
  }

  // --- Private: output (screenshot) freeze detection ---

  private startFrameMonitor() {
    this.frameMonitor?.stop();
    this.frameMonitor = null;
    if (!this.config.outputCheckEnabled) {
      logger.info('Output freeze monitor disabled by config');
      return;
    }
    logger.info({ windowMs: this.config.outputFreezeWindowMs }, 'Output freeze monitor enabled');
    this.frameMonitor = new FrameMonitor({
      captureFrame: () => this.obs.getSourceScreenshot(),
      shouldCapture: () => this.canCheckOutput(),
      onFreeze: () => this.onOutputFreeze(),
      onSuspect: () => logger.debug('Output appears static — confirming with a second screenshot'),
      onFalseAlarm: () => logger.info({ windowMs: this.config.outputFreezeWindowMs }, 'Output freeze suspected but confirmation frame changed — false alarm'),
      getWindowMs: () => this.config.outputFreezeWindowMs,
    });
    this.frameMonitor.start();
  }

  /** Gate for screenshot capture: only meaningful during confirmed live playback. */
  private canCheckOutput(): boolean {
    return this.mpv.isConnected()
      && this.lastPlaying
      && this.videoConfirmed
      && !this.lastKnownPaused
      && !this.intentionallyStopped
      && this.recoveryStep === RecoveryStep.None;
  }

  private onOutputFreeze() {
    if (!this.canCheckOutput()) return; // re-check at fire time
    logger.debug('Output freeze confirmed by screenshot — entering recovery');
    this.handleVideoFreeze(this.lastTimePos, 'Output freeze', {
      detectedBy: 'screenshot', playlistPos: this.lastSeenVideoIndex,
    });
  }

  // --- Private: playlist advancement ---

  private async advanceToNextPlaylist() {
    const current = this.state.get();
    const next = (current.playlistIndex + 1) % this.config.playlists.length;
    const playlist = this.config.playlists[next];
    this.addEvent(`Playlist finished. Advancing to ${next + 1}/${this.config.playlists.length}: ${playlist.name || playlist.id}`);
    this.state.update({ playlistIndex: next, videoIndex: 0, videoId: '', currentTime: 0 });
    this.state.flush();
    this.totalVideos = 0;

    const url = `https://www.youtube.com/playlist?list=${playlist.id}`;
    try { await this.mpv.loadPlaylist(url); } catch { /* ignore */ }
    this.consecutiveErrors = 0;
    this.stalledHeartbeats = 0;
    this.nonPlayingHeartbeats = 0;
  }

  // --- Private: periodic mpv restart ---

  private startPeriodicRestartTimer() {
    this.stopPeriodicRestartTimer();
    if (this.config.sourceRefreshIntervalMs <= 0) return;
    const mins = Math.round(this.config.sourceRefreshIntervalMs / 60000);
    const label = mins >= 60 ? `${(mins / 60).toFixed(1)}h` : `${mins}m`;
    logger.info({ intervalMs: this.config.sourceRefreshIntervalMs }, `Periodic mpv restart enabled (every ${label})`);
    this.periodicRestartTimer = setInterval(async () => {
      if (this.recoveryStep !== RecoveryStep.None) return;
      if (!this.mpv.isConnected()) return;
      const mem = getSystemMemory();
      logger.info({ systemMemory: mem }, 'Periodic mpv restart');
      this.addEvent(`Periodic mpv restart (RAM: ${mem.usedGB}/${mem.totalGB}GB, ${mem.usedPercent}%)`);
      try {
        await this.mpv.restart();
        // onMpvConnect will call loadCurrentPlaylist
      } catch (err) {
        logger.error({ err }, 'Periodic mpv restart failed');
      }
    }, this.config.sourceRefreshIntervalMs);
  }

  private stopPeriodicRestartTimer() {
    if (this.periodicRestartTimer) {
      clearInterval(this.periodicRestartTimer);
      this.periodicRestartTimer = null;
    }
  }

  // --- Private: recovery escalation ---

  private async startRecoverySequence() {
    await this.executeStep(RecoveryStep.RetryCurrent);
  }

  private async executeStep(step: RecoveryStep) {
    this.recoveryStep = step;
    logger.info({ step }, 'Executing recovery step');
    this.addEvent(`Recovery step: ${step}`);
    await this.discord.notifyRecovery(step);

    switch (step) {
      case RecoveryStep.RetryCurrent: {
        const pos = this.state.get().videoIndex;
        // Force a reload, not jumpTo: when a stall leaves mpv "playing" the
        // current index, setting playlist-pos to that same value is a no-op,
        // so RetryCurrent did nothing and always burned through to RestartMpv
        // (a full mpv restart = black screen). reloadIndex re-resolves the URL
        // in place, giving this step a real chance to fix things first.
        try { await this.mpv.reloadIndex(pos); } catch { /* may fail */ }
        this.scheduleNextStep(RecoveryStep.RestartMpv, this.config.recoveryDelayMs);
        break;
      }

      case RecoveryStep.RestartMpv: {
        try {
          await this.mpv.restart();
          // onMpvConnect will call loadCurrentPlaylist
        } catch (err) {
          logger.warn({ err }, 'mpv restart failed');
        }
        this.scheduleNextStep(RecoveryStep.CriticalAlert, 15000);
        break;
      }

      case RecoveryStep.CriticalAlert:
        await this.discord.notifyCritical(
          'All recovery steps exhausted. Waiting 60s before retrying.',
        );
        this.recoveryTimer = setTimeout(() => {
          this.recoveryStep = RecoveryStep.None;
          this.startRecoverySequence();
        }, 60000);
        break;
    }
  }

  private scheduleNextStep(nextStep: RecoveryStep, delayMs: number) {
    this.clearRecoveryTimer();
    this.recoveryTimer = setTimeout(() => {
      if (this.recoveryStep === RecoveryStep.None) return;
      const elapsed = Date.now() - this.lastHeartbeatAt;
      const stillStalled = this.stalledHeartbeats >= RecoveryEngine.STALL_THRESHOLD;
      const stillNotPlaying = this.nonPlayingHeartbeats >= RecoveryEngine.NON_PLAYING_THRESHOLD;
      const stillFrozen = this.videoFreezeHeartbeats >= RecoveryEngine.VIDEO_FREEZE_THRESHOLD;
      if (elapsed > this.config.heartbeatTimeoutMs || stillStalled || stillNotPlaying || stillFrozen) {
        this.executeStep(nextStep);
      } else {
        logger.info({ reason: this.recoveryReason }, 'Recovery condition resolved, cancelling recovery');
        this.resetRecovery();
      }
    }, delayMs);
  }

  private resetRecovery() {
    if (this.recoveryStep !== RecoveryStep.None) {
      logger.info({ previousStep: this.recoveryStep }, 'Recovery resolved');
      this.addEvent('Recovery resolved — playback resumed');
      const currentState = this.state.get();
      this.discord.notifyResume(currentState.videoIndex, currentState.videoId);
    }
    this.recoveryStep = RecoveryStep.None;
    this.recoveryReason = null;
    this.clearRecoveryTimer();
  }

  private clearRecoveryTimer() {
    if (this.recoveryTimer) {
      clearTimeout(this.recoveryTimer);
      this.recoveryTimer = null;
    }
  }
}
