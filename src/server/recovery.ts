import { RecoveryStep, type AppConfig, type PlayerMessage } from './types.js';
import type { PlayerWebSocket } from './websocket.js';
import type { StateManager } from './state.js';
import type { OBSClient } from './obs-client.js';
import type { DiscordNotifier } from './discord.js';
import { logger } from './logger.js';

const SKIP_ERROR_CODES = new Set([100, 101, 150]);
const MAX_EVENT_LOG = 100;

export interface EventLogEntry {
  timestamp: string;
  message: string;
}

export class RecoveryEngine {
  private config: AppConfig;
  private ws: PlayerWebSocket;
  private state: StateManager;
  private obs: OBSClient;
  private discord: DiscordNotifier;

  private consecutiveErrors = 0;
  private lastHeartbeatAt = Date.now();
  private heartbeatCheckTimer: ReturnType<typeof setInterval> | null = null;
  private recoveryStep = RecoveryStep.None;
  private recoveryTimer: ReturnType<typeof setTimeout> | null = null;
  private totalVideos = 0;
  private startedAt = Date.now();
  private eventLog: EventLogEntry[] = [];
  private consecutivePausedHeartbeats = 0;
  private lastProgressTime = 0;
  private stalledHeartbeats = 0;
  private playbackQuality = '';
  private lowQualityHeartbeats = 0;
  private nonPlayingHeartbeats = 0;
  private sourceRefreshTimer: ReturnType<typeof setInterval> | null = null;
  private static readonly STALL_THRESHOLD = 3; // consecutive heartbeats with no progress while "playing"
  private static readonly NON_PLAYING_THRESHOLD = 6; // ~30s of heartbeats without reaching playing state
  private static readonly QUALITY_RANKS: Record<string, number> = {
    small: 0, medium: 1, large: 2, hd720: 3, hd1080: 4, hd1440: 5, hd2160: 6, highres: 7,
  };

  constructor(
    config: AppConfig,
    ws: PlayerWebSocket,
    state: StateManager,
    obs: OBSClient,
    discord: DiscordNotifier,
  ) {
    this.config = config;
    this.ws = ws;
    this.state = state;
    this.obs = obs;
    this.discord = discord;
  }

  start() {
    this.ws.onMessage((msg) => this.handlePlayerMessage(msg));
    this.ws.onConnect(() => this.onPlayerConnect());
    this.ws.onDisconnect(() => this.onPlayerDisconnect());
    this.startHeartbeatMonitor();
    this.startSourceRefreshTimer();
    // If player is already connected (e.g. after config reload), send playlist now
    if (this.ws.isConnected()) {
      this.onPlayerConnect();
    }
  }

  stop() {
    this.stopHeartbeatMonitor();
    this.stopSourceRefreshTimer();
    this.clearRecoveryTimer();
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
    };
  }

  getEvents(): EventLogEntry[] {
    return [...this.eventLog];
  }

  private addEvent(message: string) {
    this.eventLog.push({ timestamp: new Date().toISOString(), message });
    if (this.eventLog.length > MAX_EVENT_LOG) {
      this.eventLog.shift();
    }
  }

  private onPlayerConnect() {
    const resumeInfo = { videoIndex: this.state.get().videoIndex, currentTime: this.state.get().currentTime };
    logger.info(resumeInfo, 'Player connected, sending playlist load');
    this.addEvent(`Player connected — resuming video #${resumeInfo.videoIndex} at ${Math.floor(resumeInfo.currentTime)}s`);
    this.resetRecovery();
    this.lastHeartbeatAt = Date.now();
    this.nonPlayingHeartbeats = 0;

    const savedState = this.state.get();
    const playlistIndex = savedState.playlistIndex < this.config.playlists.length
      ? savedState.playlistIndex : 0;
    const playlist = this.config.playlists[playlistIndex];
    this.ws.send({
      type: 'loadPlaylist',
      playlistId: playlist.id,
      index: savedState.videoIndex,
      loop: this.config.playlists.length === 1,
      startTime: savedState.currentTime || 0,
    });
  }

  private onPlayerDisconnect() {
    logger.warn('Player disconnected');
    this.addEvent('Player disconnected');
    // Heartbeat monitor will trigger recovery
  }

  private handlePaused() {
    logger.info('Player paused, sending resume');
    this.addEvent('Player paused — auto-resuming');
    this.ws.send({ type: 'resume' });
  }

  private handlePlayerMessage(msg: PlayerMessage) {
    switch (msg.type) {
      case 'ready':
        // Handled by onPlayerConnect
        break;

      case 'heartbeat':
        this.lastHeartbeatAt = Date.now();
        // Stall detection: player claims to be playing but currentTime isn't advancing
        // YT.PlayerState.PLAYING === 1
        if (msg.playerState === 1 && msg.currentTime > 0) {
          if (Math.abs(msg.currentTime - this.lastProgressTime) < 1) {
            this.stalledHeartbeats++;
            if (this.stalledHeartbeats >= RecoveryEngine.STALL_THRESHOLD && this.recoveryStep === RecoveryStep.None) {
              const stallMsg = `Player stalled at ${Math.floor(msg.currentTime)}s on video #${msg.videoIndex} (${msg.videoId}) — no progress for ${this.stalledHeartbeats} heartbeats`;
              logger.warn({ currentTime: msg.currentTime, stalledHeartbeats: this.stalledHeartbeats, videoIndex: msg.videoIndex, videoId: msg.videoId }, 'Player stalled — video not advancing');
              this.addEvent(stallMsg);
              this.discord.notifyRecovery(`Stall detected — ${stallMsg}`);
              this.startRecoverySequence();
            }
          } else {
            this.stalledHeartbeats = 0;
            this.lastProgressTime = msg.currentTime;
            this.resetRecovery();
          }
        } else {
          this.stalledHeartbeats = 0;
          this.lastProgressTime = msg.currentTime;
          if (this.recoveryStep !== RecoveryStep.None && msg.playerState === 1) {
            this.resetRecovery();
          }
        }
        // Track playback quality for dashboard display
        if (msg.playbackQuality) {
          this.playbackQuality = msg.playbackQuality;
        }
        // Quality recovery: detect sustained low quality while playing
        if (this.config.qualityRecoveryEnabled && msg.playbackQuality && msg.playerState === 1) {
          const currentRank = RecoveryEngine.QUALITY_RANKS[msg.playbackQuality] ?? -1;
          const minRank = RecoveryEngine.QUALITY_RANKS[this.config.minQuality] ?? 3;
          if (currentRank >= 0 && currentRank < minRank) {
            this.lowQualityHeartbeats++;
            const threshold = Math.ceil(this.config.qualityRecoveryDelayMs / 5000);
            if (this.lowQualityHeartbeats >= threshold && this.recoveryStep === RecoveryStep.None) {
              const qualityMsg = `Low quality (${msg.playbackQuality}) sustained for ${this.lowQualityHeartbeats} heartbeats on video #${msg.videoIndex} (${msg.videoId})`;
              logger.warn({ quality: msg.playbackQuality, heartbeats: this.lowQualityHeartbeats, videoIndex: msg.videoIndex, videoId: msg.videoId }, 'Low quality detected — triggering recovery');
              this.addEvent(qualityMsg);
              this.discord.notifyRecovery(`Low quality recovery — ${qualityMsg}`);
              this.lowQualityHeartbeats = 0;
              this.startRecoverySequence();
            }
          } else {
            this.lowQualityHeartbeats = 0;
          }
        } else if (msg.playerState !== 1) {
          this.lowQualityHeartbeats = 0;
        }
        // Only write state when video is making progress — skip stale writes during stalls
        if (this.stalledHeartbeats < RecoveryEngine.STALL_THRESHOLD) {
          const update: Record<string, unknown> = {
            videoIndex: msg.videoIndex,
            videoId: msg.videoId,
            videoTitle: msg.videoTitle,
            videoDuration: msg.videoDuration,
            nextVideoId: msg.nextVideoId || '',
          };
          // Only update currentTime when playing or paused — don't overwrite
          // a valid resume position with 0 during buffering/loading
          if (msg.playerState === 1 || msg.playerState === 2 || msg.currentTime > 0) {
            update.currentTime = msg.currentTime;
          }
          this.state.update(update);
        }
        // YT.PlayerState.PAUSED === 2 — only auto-resume after 2 consecutive paused heartbeats
        if (msg.playerState === 2) {
          this.consecutivePausedHeartbeats++;
          if (this.consecutivePausedHeartbeats >= 2) {
            this.handlePaused();
          }
        } else {
          this.consecutivePausedHeartbeats = 0;
        }
        // Non-playing detection: player is connected and sending heartbeats but stuck in buffering/unstarted
        if (msg.playerState === 1) {
          this.nonPlayingHeartbeats = 0;
        } else if (msg.playerState !== 2) { // paused is already handled above
          this.nonPlayingHeartbeats++;
          if (this.nonPlayingHeartbeats >= RecoveryEngine.NON_PLAYING_THRESHOLD && this.recoveryStep === RecoveryStep.None) {
            const npMsg = `Player not playing (state ${msg.playerState}) for ${this.nonPlayingHeartbeats} heartbeats on video #${msg.videoIndex} (${msg.videoId})`;
            logger.warn({ playerState: msg.playerState, heartbeats: this.nonPlayingHeartbeats, videoIndex: msg.videoIndex, videoId: msg.videoId }, 'Player stuck in non-playing state');
            this.addEvent(npMsg);
            this.discord.notifyRecovery(`Non-playing recovery — ${npMsg}`);
            this.startRecoverySequence();
          }
        }
        break;

      case 'stateChange':
        this.lastHeartbeatAt = Date.now();
        this.state.update({
          videoIndex: msg.videoIndex,
          videoId: msg.videoId,
          videoTitle: msg.videoTitle,
        });
        // YT.PlayerState.PLAYING === 1
        if (msg.playerState === 1) {
          this.consecutiveErrors = 0;
        }
        // YT.PlayerState.ENDED === 0 — detect natural end of last video
        if (msg.playerState === 0 && this.totalVideos > 0
            && msg.videoIndex === this.totalVideos - 1
            && this.config.playlists.length > 1) {
          this.advanceToNextPlaylist();
        }
        break;

      case 'playlistLoaded':
        this.totalVideos = msg.totalVideos;
        this.nonPlayingHeartbeats = 0;
        logger.info({ totalVideos: msg.totalVideos }, 'Playlist loaded');
        this.addEvent(`Playlist loaded with ${msg.totalVideos} videos`);
        // Clamp videoIndex if out of bounds (e.g. state from a different playlist)
        const currentVideoIndex = this.state.get().videoIndex;
        if (msg.totalVideos > 0 && currentVideoIndex >= msg.totalVideos) {
          logger.warn({ videoIndex: currentVideoIndex, totalVideos: msg.totalVideos }, 'videoIndex out of bounds, resetting to 0');
          this.state.update({ videoIndex: 0 });
          this.ws.send({ type: 'skip', index: 0 });
        }
        break;

      case 'error':
        this.handlePlaybackError(msg.errorCode, msg.videoIndex, msg.videoId);
        break;
    }
  }

  private async handlePlaybackError(errorCode: number, videoIndex: number, videoId: string) {
    logger.error({ errorCode, videoIndex, videoId }, 'Playback error');
    this.addEvent(`Playback error ${errorCode} on video #${videoIndex} (${videoId})`);

    if (SKIP_ERROR_CODES.has(errorCode)) {
      await this.skipVideo(videoIndex, videoId, `Error ${errorCode} (unavailable/not embeddable)`);
      return;
    }

    this.consecutiveErrors++;
    await this.discord.notifyError(videoIndex, videoId, errorCode, this.consecutiveErrors);

    if (this.consecutiveErrors >= this.config.maxConsecutiveErrors) {
      await this.skipVideo(videoIndex, videoId, `${this.consecutiveErrors} consecutive errors`);
      this.consecutiveErrors = 0;
      return;
    }

    // Retry current video after delay
    logger.info({ attempt: this.consecutiveErrors }, 'Retrying current video');
    setTimeout(() => {
      this.ws.send({ type: 'retryCurrent' });
    }, this.config.recoveryDelayMs);
  }

  private async skipVideo(fromIndex: number, videoId: string, reason: string) {
    // If at last video in playlist, advance to next playlist
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

    this.ws.send({ type: 'skip', index: nextIndex });
    this.state.update({ videoIndex: nextIndex });
    this.consecutiveErrors = 0;
  }

  private async advanceToNextPlaylist(reason?: string) {
    const current = this.state.get();
    const next = (current.playlistIndex + 1) % this.config.playlists.length;
    const playlist = this.config.playlists[next];
    this.addEvent(`Playlist finished. Advancing to ${next + 1}/${this.config.playlists.length}: ${playlist.name || playlist.id}`);
    this.state.update({ playlistIndex: next, videoIndex: 0, videoId: '', currentTime: 0 });
    this.state.flush(); // Critical transition — write immediately, don't wait for debounce
    this.totalVideos = 0;
    this.ws.send({ type: 'loadPlaylist', playlistId: playlist.id, index: 0, loop: this.config.playlists.length === 1 });
    this.consecutiveErrors = 0;
  }

  // --- Heartbeat monitoring ---

  private startHeartbeatMonitor() {
    this.lastHeartbeatAt = Date.now();
    this.heartbeatCheckTimer = setInterval(() => {
      if (!this.ws.isConnected()) return;

      const elapsed = Date.now() - this.lastHeartbeatAt;
      if (elapsed > this.config.heartbeatTimeoutMs && this.recoveryStep === RecoveryStep.None) {
        logger.warn({ elapsedMs: elapsed }, 'Heartbeat timeout, starting recovery');
        this.addEvent(`Heartbeat timeout (${Math.round(elapsed / 1000)}s), starting recovery`);
        this.startRecoverySequence();
      }
    }, 5000);
  }

  private stopHeartbeatMonitor() {
    if (this.heartbeatCheckTimer) {
      clearInterval(this.heartbeatCheckTimer);
      this.heartbeatCheckTimer = null;
    }
  }

  // --- Periodic source refresh ---

  private startSourceRefreshTimer() {
    this.stopSourceRefreshTimer();
    if (this.config.sourceRefreshIntervalMs <= 0) return;
    const hours = (this.config.sourceRefreshIntervalMs / 3600000).toFixed(1);
    logger.info({ intervalMs: this.config.sourceRefreshIntervalMs }, `Periodic source refresh enabled (every ${hours}h)`);
    this.sourceRefreshTimer = setInterval(() => {
      if (this.recoveryStep !== RecoveryStep.None) return;
      if (!this.ws.isConnected()) return;
      logger.info('Periodic browser source refresh');
      this.addEvent('Periodic browser source refresh (maintenance)');
      this.obs.refreshBrowserSource();
    }, this.config.sourceRefreshIntervalMs);
  }

  private stopSourceRefreshTimer() {
    if (this.sourceRefreshTimer) {
      clearInterval(this.sourceRefreshTimer);
      this.sourceRefreshTimer = null;
    }
  }

  // --- Recovery escalation ---

  private async startRecoverySequence() {
    await this.executeStep(RecoveryStep.RetryCurrent);
  }

  private async executeStep(step: RecoveryStep) {
    this.recoveryStep = step;
    logger.info({ step }, 'Executing recovery step');
    this.addEvent(`Recovery step: ${step}`);
    await this.discord.notifyRecovery(step);

    switch (step) {
      case RecoveryStep.RetryCurrent:
        this.ws.send({ type: 'retryCurrent' });
        this.scheduleNextStep(RecoveryStep.RefreshSource, this.config.recoveryDelayMs);
        break;

      case RecoveryStep.RefreshSource: {
        const refreshed = await this.obs.refreshBrowserSource();
        if (!refreshed) {
          logger.warn('Refresh failed, escalating');
        }
        this.scheduleNextStep(RecoveryStep.ToggleVisibility, 15000);
        break;
      }

      case RecoveryStep.ToggleVisibility: {
        const toggled = await this.obs.toggleBrowserSource();
        if (!toggled) {
          logger.warn('Toggle failed, escalating');
        }
        this.scheduleNextStep(RecoveryStep.CriticalAlert, 15000);
        break;
      }

      case RecoveryStep.CriticalAlert:
        await this.discord.notifyCritical(
          'All recovery steps exhausted. Player may be unresponsive. Will retry full sequence in 60s.',
        );
        // Retry full sequence after 60s
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
      // Check if recovery was cancelled (heartbeat came back)
      if (this.recoveryStep === RecoveryStep.None) return;
      // Check if the problem persists (heartbeat timeout OR stall still ongoing)
      const elapsed = Date.now() - this.lastHeartbeatAt;
      const stillStalled = this.stalledHeartbeats >= RecoveryEngine.STALL_THRESHOLD;
      const stillNotPlaying = this.nonPlayingHeartbeats >= RecoveryEngine.NON_PLAYING_THRESHOLD;
      if (elapsed > this.config.heartbeatTimeoutMs || stillStalled || stillNotPlaying) {
        this.executeStep(nextStep);
      } else {
        logger.info('Heartbeat restored, cancelling recovery');
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
    this.clearRecoveryTimer();
  }

  private clearRecoveryTimer() {
    if (this.recoveryTimer) {
      clearTimeout(this.recoveryTimer);
      this.recoveryTimer = null;
    }
  }
}
