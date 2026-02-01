import type { AppConfig } from './types.js';
import { logger } from './logger.js';

export class DiscordNotifier {
  private webhookUrl: string;

  constructor(config: AppConfig) {
    this.webhookUrl = config.discordWebhookUrl;
  }

  private get enabled(): boolean {
    return this.webhookUrl.length > 0;
  }

  async send(content: string, level: 'info' | 'warn' | 'error' = 'info'): Promise<void> {
    if (!this.enabled) return;

    const colorMap = { info: 3447003, warn: 16776960, error: 15158332 };
    const emoji = { info: 'ℹ️', warn: '⚠️', error: '🚨' };

    const body = {
      embeds: [
        {
          title: `${emoji[level]} Freeze Monitor`,
          description: content,
          color: colorMap[level],
          timestamp: new Date().toISOString(),
        },
      ],
    };

    try {
      const res = await fetch(this.webhookUrl, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        logger.error({ status: res.status }, 'Discord webhook failed');
      }
    } catch (err) {
      logger.error({ err }, 'Discord webhook error');
    }
  }

  async notifyError(videoIndex: number, videoId: string, errorCode: number, attempt: number): Promise<void> {
    await this.send(
      `Playback error **${errorCode}** on video #${videoIndex} (\`${videoId}\`)\nRetry attempt: ${attempt}`,
      'warn',
    );
  }

  async notifySkip(videoIndex: number, videoId: string, reason: string): Promise<void> {
    await this.send(
      `Skipping video #${videoIndex} (\`${videoId}\`)\nReason: ${reason}`,
      'warn',
    );
  }

  async notifyRecovery(step: string): Promise<void> {
    await this.send(`Recovery action: **${step}**`, 'warn');
  }

  async notifyCritical(message: string): Promise<void> {
    await this.send(`**CRITICAL:** ${message}`, 'error');
  }

  async notifyResume(videoIndex: number, videoId: string): Promise<void> {
    await this.send(
      `Playback resumed at video #${videoIndex} (\`${videoId}\`)`,
      'info',
    );
  }
}
