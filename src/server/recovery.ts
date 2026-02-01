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
  }

  stop() {
    this.stopHeartbeatMonitor();
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
    logger.info('Player connected, sending playlist load');
    this.addEvent('Player connected');
    this.resetRecovery();
    this.lastHeartbeatAt = Date.now();

    const savedState = this.state.get();
    const playlistIndex = savedState.playlistIndex < this.config.playlists.length
      ? savedState.playlistIndex : 0;
    const playlist = this.config.playlists[playlistIndex];
    this.ws.send({
      type: 'loadPlaylist',
      playlistId: playlist.id,
      index: savedState.videoIndex,
    });
  }

  private onPlayerDisconnect() {
    logger.warn('Player disconnected');
    this.addEvent('Player disconnected');
    // Heartbeat monitor will trigger recovery
  }

  private handlePlayerMessage(msg: PlayerMessage) {
    switch (msg.type) {
      case 'ready':
        // Handled by onPlayerConnect
        break;

      case 'heartbeat':
        this.lastHeartbeatAt = Date.now();
        this.resetRecovery();
        this.state.update({
          videoIndex: msg.videoIndex,
          videoId: msg.videoId,
          currentTime: msg.currentTime,
        });
        break;

      case 'stateChange':
        this.lastHeartbeatAt = Date.now();
        this.state.update({
          videoIndex: msg.videoIndex,
          videoId: msg.videoId,
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
        logger.info({ totalVideos: msg.totalVideos }, 'Playlist loaded');
        this.addEvent(`Playlist loaded with ${msg.totalVideos} videos`);
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
    this.totalVideos = 0;
    this.ws.send({ type: 'loadPlaylist', playlistId: playlist.id, index: 0 });
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
      // Check if heartbeat is still missing
      const elapsed = Date.now() - this.lastHeartbeatAt;
      if (elapsed > this.config.heartbeatTimeoutMs) {
        this.executeStep(nextStep);
      } else {
        logger.info('Heartbeat restored, cancelling recovery');
        this.resetRecovery();
      }
    }, delayMs);
  }

  private resetRecovery() {
    if (this.recoveryStep !== RecoveryStep.None) {
      logger.info({ previousStep: this.recoveryStep }, 'Recovery cancelled');
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
