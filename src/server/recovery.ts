import { freemem, totalmem } from 'os';
import { RecoveryStep, type AppConfig, type MpvHeartbeat } from './types.js';
import type { MpvClient } from './mpv-client.js';
import type { StateManager } from './state.js';
import type { OBSClient } from './obs-client.js';
import type { DiscordNotifier } from './discord.js';
import { logger } from './logger.js';

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

export interface EventLogEntry {
  timestamp: string;
  message: string;
}

export class RecoveryEngine {
  private config: AppConfig;
  private mpv: MpvClient;
  private state: StateManager;
  private obs: OBSClient;
  private discord: DiscordNotifier;

  private consecutiveErrors = 0;
  private lastHeartbeatAt = Date.now();
  private heartbeatPollTimer: ReturnType<typeof setInterval> | null = null;
  private recoveryStep = RecoveryStep.None;
  private recoveryReason: 'heartbeat' | 'stall' | 'non-playing' | null = null;
  private recoveryTimer: ReturnType<typeof setTimeout> | null = null;
  private errorRetryTimer: ReturnType<typeof setTimeout> | null = null;
  private totalVideos = 0;
  private startedAt = Date.now();
  private eventLog: EventLogEntry[] = [];
  private lastProgressTime = 0;
  private stalledHeartbeats = 0;
  private playbackQuality = '';
  private lowQualityHeartbeats = 0;
  private nonPlayingHeartbeats = 0;
  private consecutivePausedHeartbeats = 0;
  private intentionallyStopped = false;
  private lastKnownPaused = false;
  private seekPending = false;
  private videoConfirmed = false;
  private periodicRestartTimer: ReturnType<typeof setInterval> | null = null;
  private videoFreezeHeartbeats = 0;
  private urlRetryCount = 0;
  private lastSeenVideoIndex = -1;
  private static readonly STALL_THRESHOLD = 3;
  private static readonly NON_PLAYING_THRESHOLD = 6;
  private static readonly VIDEO_FREEZE_THRESHOLD = 4;

  // Bound handlers so we can remove them from mpv EventEmitter
  private boundOnConnect = () => this.onMpvConnect();
  private boundOnDisconnect = () => this.onMpvDisconnect();
  private boundOnFileEnded = (reason: string) => this.onFileEnded(reason);
  private boundOnProcessExit = () => this.onProcessExit();
  private static readonly QUALITY_RANKS: Record<string, number> = {
    small: 0, medium: 1, large: 2, hd720: 3, hd1080: 4, hd1440: 5, hd2160: 6, highres: 7,
  };

  constructor(
    config: AppConfig,
    mpv: MpvClient,
    state: StateManager,
    obs: OBSClient,
    discord: DiscordNotifier,
  ) {
    this.config = config;
    this.mpv = mpv;
    this.state = state;
    this.obs = obs;
    this.discord = discord;
  }

  start() {
    // Remove any stale listeners (e.g. from a previous RecoveryEngine on the same mpv)
    this.removeMpvListeners();

    this.mpv.on('connected', this.boundOnConnect);
    this.mpv.on('disconnected', this.boundOnDisconnect);
    this.mpv.on('fileEnded', this.boundOnFileEnded);
    this.mpv.on('processExit', this.boundOnProcessExit);
    this.startHeartbeatPoll();
    this.startPeriodicRestartTimer();
    if (this.mpv.isConnected()) {
      this.onMpvConnect();
    }
  }

  stop() {
    this.removeMpvListeners();
    this.stopHeartbeatPoll();
    this.stopPeriodicRestartTimer();
    this.clearRecoveryTimer();
    this.clearErrorRetryTimer();
  }

  private removeMpvListeners() {
    this.mpv.removeListener('connected', this.boundOnConnect);
    this.mpv.removeListener('disconnected', this.boundOnDisconnect);
    this.mpv.removeListener('fileEnded', this.boundOnFileEnded);
    this.mpv.removeListener('processExit', this.boundOnProcessExit);
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
      playbackQuality: this.playbackQuality,
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
    this.eventLog.push({ timestamp: new Date().toISOString(), message });
    if (this.eventLog.length > MAX_EVENT_LOG) {
      this.eventLog.shift();
    }
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
    await this.loadCurrentPlaylist();
  }

  private onMpvDisconnect() {
    logger.warn('mpv disconnected');
    this.addEvent('mpv disconnected');
    this.videoConfirmed = false;
    // Heartbeat poll will detect timeout and trigger recovery
  }

  private async onFileEnded(reason: string) {
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
    const [timePos, duration, paused, idle, playlistPos, playlistCount, mediaTitle, filename, videoParams, vfps] =
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
    const videoId = this.extractVideoId(hb.filename);
    const mediaTitle = this.sanitizeTitle(hb.mediaTitle);

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

    // Video freeze detection: audio plays (time-pos advances) but video output has stopped.
    // estimated-vf-fps drops to 0 when the video rendering pipeline freezes.
    if (isPlaying && this.videoConfirmed && hb.vfps < 1) {
      this.videoFreezeHeartbeats++;
      if (this.videoFreezeHeartbeats >= RecoveryEngine.VIDEO_FREEZE_THRESHOLD && this.recoveryStep === RecoveryStep.None) {
        const mem = getSystemMemory();
        const freezeMsg = `Video freeze detected at ${Math.floor(hb.timePos)}s on video #${hb.playlistPos} (${videoId}) — audio playing but vfps=${hb.vfps} for ${this.videoFreezeHeartbeats} heartbeats (RAM: ${mem.usedGB}/${mem.totalGB}GB, ${mem.usedPercent}%)`;
        logger.warn({ timePos: hb.timePos, vfps: hb.vfps, videoFreezeHeartbeats: this.videoFreezeHeartbeats, playlistPos: hb.playlistPos, videoId, systemMemory: mem }, 'Video freeze — audio playing but video output stopped');
        this.addEvent(freezeMsg);
        this.discord.notifyRecovery('Video freeze detected');
        this.recoveryReason = 'stall';
        this.startRecoverySequence();
      }
    } else {
      this.videoFreezeHeartbeats = 0;
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

  // --- Private: skip and playlist advancement ---

  private async skipVideo(fromIndex: number, videoId: string, reason: string) {
    if (this.totalVideos > 0 && fromIndex + 1 >= this.totalVideos) {
      await this.advanceToNextPlaylist(reason);
      return;
    }

    const nextIndex = this.totalVideos > 0
      ? (fromIndex + 1) % this.totalVideos
      : fromIndex + 1;

    logger.warn({ fromIndex, nextIndex, reason }, 'Skipping video');
    this.addEvent(`Skipping video #${fromIndex} (${videoId}): ${reason}`);
    await this.discord.notifySkip(fromIndex, videoId, reason);

    try { await this.mpv.jumpTo(nextIndex); } catch { /* ignore */ }
    this.state.update({ videoIndex: nextIndex });
    this.consecutiveErrors = 0;
  }

  private async advanceToNextPlaylist(reason?: string) {
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
    this.lowQualityHeartbeats = 0;
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
        try { await this.mpv.jumpTo(pos); } catch { /* may fail */ }
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

  private clearErrorRetryTimer() {
    if (this.errorRetryTimer) {
      clearTimeout(this.errorRetryTimer);
      this.errorRetryTimer = null;
    }
  }
}
