import type { AppConfig } from './types.js';
import type { OBSClient } from './obs-client.js';
import type { DiscordNotifier } from './discord.js';
import { logger } from './logger.js';

const TWITCH_TOKEN_URL = 'https://id.twitch.tv/oauth2/token';
const TWITCH_STREAMS_URL = 'https://api.twitch.tv/helix/streams';
const TOKEN_REFRESH_MARGIN_MS = 5 * 60 * 1000; // Refresh 5 min before expiry
const MISMATCH_THRESHOLD = 3;
const RESTART_DELAY_MS = 5000;
const INITIAL_CHECK_DELAY_MS = 15000;

export interface TwitchLivenessStatus {
  enabled: boolean;
  channelLive: boolean | null;
  obsStreaming: boolean | null;
  consecutiveMismatches: number;
  mismatchThreshold: number;
  lastCheckAt: number | null;
  lastRestartAt: number | null;
  restartCount: number;
}

export class TwitchLivenessChecker {
  private config: AppConfig;
  private obs: OBSClient;
  private discord: DiscordNotifier;

  private accessToken: string | null = null;
  private tokenExpiresAt = 0;

  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private initialTimer: ReturnType<typeof setTimeout> | null = null;
  private consecutiveMismatches = 0;
  private restartInProgress = false;

  private channelLive: boolean | null = null;
  private obsStreaming: boolean | null = null;
  private lastCheckAt: number | null = null;
  private lastRestartAt: number | null = null;
  private restartCount = 0;

  constructor(config: AppConfig, obs: OBSClient, discord: DiscordNotifier) {
    this.config = config;
    this.obs = obs;
    this.discord = discord;
  }

  get enabled(): boolean {
    return (
      this.config.twitchLivenessEnabled &&
      this.config.twitchClientId.length > 0 &&
      this.config.twitchClientSecret.length > 0 &&
      this.config.twitchChannel.length > 0
    );
  }

  start(): void {
    if (!this.config.twitchLivenessEnabled) {
      logger.info('Twitch liveness checker disabled');
      return;
    }
    if (!this.config.twitchClientId || !this.config.twitchClientSecret || !this.config.twitchChannel) {
      logger.warn('Twitch liveness checker enabled but missing credentials (need Client ID, Client Secret, and Channel)');
      return;
    }
    logger.info(
      { channel: this.config.twitchChannel, intervalMs: this.config.twitchPollIntervalMs },
      'Starting Twitch liveness checker',
    );
    this.initialTimer = setTimeout(() => {
      this.initialTimer = null;
      this.check();
      this.pollTimer = setInterval(() => this.check(), this.config.twitchPollIntervalMs);
    }, INITIAL_CHECK_DELAY_MS);
  }

  stop(): void {
    if (this.initialTimer) {
      clearTimeout(this.initialTimer);
      this.initialTimer = null;
    }
    if (this.pollTimer) {
      clearInterval(this.pollTimer);
      this.pollTimer = null;
    }
  }

  getStatus(): TwitchLivenessStatus {
    return {
      enabled: this.enabled,
      channelLive: this.channelLive,
      obsStreaming: this.obsStreaming,
      consecutiveMismatches: this.consecutiveMismatches,
      mismatchThreshold: MISMATCH_THRESHOLD,
      lastCheckAt: this.lastCheckAt,
      lastRestartAt: this.lastRestartAt,
      restartCount: this.restartCount,
    };
  }

  async check(): Promise<void> {
    if (this.restartInProgress) return;
    this.lastCheckAt = Date.now();

    try {
      const [obsActive, live] = await Promise.all([
        this.obs.isStreaming(),
        this.isChannelLive(),
      ]);

      this.obsStreaming = obsActive;
      this.channelLive = live;

      logger.debug(
        { obsStreaming: obsActive, channelLive: live, consecutiveMismatches: this.consecutiveMismatches },
        'Twitch liveness check',
      );

      if (live === null) {
        // API error — don't change counter
        return;
      }

      if (obsActive && !live) {
        this.consecutiveMismatches++;
        logger.warn(
          { consecutiveMismatches: this.consecutiveMismatches, threshold: MISMATCH_THRESHOLD },
          'Twitch liveness mismatch: OBS streaming but channel not live',
        );
        if (this.consecutiveMismatches >= MISMATCH_THRESHOLD) {
          await this.restartStream();
        }
      } else {
        this.consecutiveMismatches = 0;
      }
    } catch (err) {
      logger.error({ err }, 'Twitch liveness check failed');
    }
  }

  private async restartStream(): Promise<void> {
    if (this.restartInProgress) return;
    this.restartInProgress = true;
    this.consecutiveMismatches = 0;

    logger.warn('Twitch liveness: restarting stream (OBS says live, Twitch says offline)');
    await this.discord.notifyTwitchMismatch(this.config.twitchChannel);

    try {
      const stopped = await this.obs.stopStream();
      if (!stopped) {
        logger.error('Failed to stop OBS stream for Twitch liveness restart');
        await this.discord.notifyCritical('Twitch liveness restart failed: could not stop stream.');
        return;
      }

      await new Promise((r) => setTimeout(r, RESTART_DELAY_MS));

      const started = await this.obs.startStreaming();
      this.lastRestartAt = Date.now();
      this.restartCount++;

      if (started) {
        logger.info('Twitch liveness: stream restarted successfully');
        await this.discord.notifyTwitchRestart(this.config.twitchChannel);
      } else {
        logger.error('Twitch liveness: failed to restart stream');
        await this.discord.notifyCritical('Twitch liveness restart failed: could not start stream.');
      }
    } catch (err) {
      logger.error({ err }, 'Twitch liveness stream restart error');
      await this.discord.notifyCritical('Twitch liveness restart encountered an error.');
    } finally {
      this.restartInProgress = false;
    }
  }

  private async isChannelLive(): Promise<boolean | null> {
    try {
      await this.ensureToken();
      if (!this.accessToken) return null;

      const url = `${TWITCH_STREAMS_URL}?user_login=${encodeURIComponent(this.config.twitchChannel.toLowerCase())}`;
      const res = await fetch(url, {
        headers: {
          'Client-ID': this.config.twitchClientId,
          'Authorization': `Bearer ${this.accessToken}`,
        },
      });

      if (res.status === 401) {
        // Token expired or revoked — invalidate and retry once
        logger.warn('Twitch API returned 401, refreshing token');
        this.accessToken = null;
        this.tokenExpiresAt = 0;
        await this.ensureToken();
        if (!this.accessToken) return null;

        const retryRes = await fetch(url, {
          headers: {
            'Client-ID': this.config.twitchClientId,
            'Authorization': `Bearer ${this.accessToken}`,
          },
        });

        if (!retryRes.ok) {
          logger.error({ status: retryRes.status }, 'Twitch API retry failed');
          return null;
        }

        const retryData = await retryRes.json() as { data: unknown[] };
        return retryData.data.length > 0;
      }

      if (!res.ok) {
        logger.error({ status: res.status }, 'Twitch API error');
        return null;
      }

      const data = await res.json() as { data: unknown[] };
      return data.data.length > 0;
    } catch (err) {
      logger.error({ err }, 'Twitch API request failed');
      return null;
    }
  }

  private async ensureToken(): Promise<void> {
    if (this.accessToken && Date.now() < this.tokenExpiresAt - TOKEN_REFRESH_MARGIN_MS) {
      return;
    }

    try {
      const params = new URLSearchParams({
        client_id: this.config.twitchClientId,
        client_secret: this.config.twitchClientSecret,
        grant_type: 'client_credentials',
      });

      const res = await fetch(TWITCH_TOKEN_URL, {
        method: 'POST',
        body: params,
      });

      if (!res.ok) {
        logger.error({ status: res.status }, 'Failed to obtain Twitch access token');
        this.accessToken = null;
        return;
      }

      const data = await res.json() as { access_token: string; expires_in: number };
      this.accessToken = data.access_token;
      this.tokenExpiresAt = Date.now() + data.expires_in * 1000;
      logger.info('Obtained Twitch access token');
    } catch (err) {
      logger.error({ err }, 'Twitch token request failed');
      this.accessToken = null;
    }
  }
}
