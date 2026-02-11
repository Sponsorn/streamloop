import type { AppConfig, DiscordConfig } from './types.js';
import { logger } from './logger.js';

type Level = 'info' | 'warn' | 'error';
const LEVEL_PRIORITY: Record<Level, number> = { info: 0, warn: 1, error: 2 };
const DEBOUNCE_MS = 5_000;

interface EmbedField {
  name: string;
  value: string;
  inline?: boolean;
}

interface QueuedMessage {
  content: string;
  level: Level;
  fields?: EmbedField[];
}

export class DiscordNotifier {
  private discord: DiscordConfig;
  private appVersion: string;
  private getUptime: () => number;
  private adminUrl: string;
  private queue: QueuedMessage[] = [];
  private flushTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(config: AppConfig, appVersion: string, getUptime: () => number, adminUrl: string) {
    this.discord = config.discord;
    this.appVersion = appVersion;
    this.getUptime = getUptime;
    this.adminUrl = adminUrl;
  }

  private get enabled(): boolean {
    return this.discord.webhookUrl.length > 0;
  }

  renderTemplate(template: string, vars: Record<string, string | number>): string {
    return template.replace(/\{(\w+)\}/g, (match, key) => {
      return key in vars ? String(vars[key]) : match;
    });
  }

  private formatUptime(ms: number): string {
    const s = Math.floor(ms / 1000);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m ${s % 60}s`;
    return `${s}s`;
  }

  private makeFooterText(extraText?: string): string {
    const parts: string[] = [];
    if (this.adminUrl) parts.push(`Dashboard: ${this.adminUrl}`);
    parts.push(`Uptime: ${this.formatUptime(this.getUptime())}`);
    parts.push(`v${this.appVersion}`);
    const footer = parts.join(' | ');
    return extraText ? `${extraText} | ${footer}` : footer;
  }

  async send(content: string, level: Level = 'info', fields?: EmbedField[]): Promise<void> {
    if (!this.enabled) return;

    this.queue.push({ content, level, fields });

    // Critical messages flush immediately
    if (level === 'error') {
      await this.flush();
      return;
    }

    // Start debounce timer if not already running
    if (!this.flushTimer) {
      this.flushTimer = setTimeout(() => this.flush(), DEBOUNCE_MS);
    }
  }

  async flush(): Promise<void> {
    if (this.flushTimer) {
      clearTimeout(this.flushTimer);
      this.flushTimer = null;
    }

    if (this.queue.length === 0) return;

    const messages = this.queue.splice(0);
    const highestLevel = messages.reduce<Level>(
      (max, m) => LEVEL_PRIORITY[m.level] > LEVEL_PRIORITY[max] ? m.level : max,
      'info',
    );

    const colorMap: Record<Level, number> = { info: 3447003, warn: 16776960, error: 15158332 };
    const emoji: Record<Level, string> = { info: '\u2139\uFE0F', warn: '\u26A0\uFE0F', error: '\uD83D\uDEA8' };

    const isBatched = messages.length > 1;
    const description = messages.map(m => {
      const prefix = isBatched ? `${emoji[m.level]} ` : '';
      return prefix + m.content;
    }).join('\n\n');

    // Use structured fields only for single messages that have them
    const embedFields = !isBatched && messages[0].fields ? messages[0].fields : undefined;

    const footerExtra = isBatched ? `${messages.length} events` : undefined;

    const embed: Record<string, unknown> = {
      title: `${emoji[highestLevel]} StreamLoop`,
      description,
      color: colorMap[highestLevel],
      timestamp: new Date().toISOString(),
      footer: { text: this.makeFooterText(footerExtra) },
    };

    if (embedFields) {
      embed.fields = embedFields;
    }

    // Build webhook body with optional bot identity
    const body: Record<string, unknown> = { embeds: [embed] };

    if (this.discord.botName) {
      body.username = this.discord.botName;
    }
    if (this.discord.avatarUrl) {
      body.avatar_url = this.discord.avatarUrl;
    }

    // Role ping on critical (error-level) messages
    if (highestLevel === 'error' && this.discord.rolePing) {
      body.content = `<@&${this.discord.rolePing}>`;
    }

    try {
      const res = await fetch(this.discord.webhookUrl, {
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
    if (!this.discord.events.error) return;
    const content = this.renderTemplate(this.discord.templates.error, {
      videoIndex, videoId, errorCode, attempt,
    });
    const fields: EmbedField[] = [
      { name: 'Error Code', value: String(errorCode), inline: true },
      { name: 'Video', value: `#${videoIndex} (\`${videoId}\`)`, inline: true },
      { name: 'Attempt', value: String(attempt), inline: true },
    ];
    await this.send(content, 'warn', fields);
  }

  async notifySkip(videoIndex: number, videoId: string, reason: string): Promise<void> {
    if (!this.discord.events.skip) return;
    const content = this.renderTemplate(this.discord.templates.skip, {
      videoIndex, videoId, reason,
    });
    await this.send(content, 'warn');
  }

  async notifyRecovery(step: string): Promise<void> {
    if (!this.discord.events.recovery) return;
    const content = this.renderTemplate(this.discord.templates.recovery, { step });
    await this.send(content, 'warn');
  }

  async notifyCritical(message: string): Promise<void> {
    if (!this.discord.events.critical) return;
    const content = this.renderTemplate(this.discord.templates.critical, { message });
    const fields: EmbedField[] = [
      { name: 'Status', value: message, inline: false },
    ];
    await this.send(content, 'error', fields);
  }

  async notifyResume(videoIndex: number, videoId: string): Promise<void> {
    if (!this.discord.events.resume) return;
    const content = this.renderTemplate(this.discord.templates.resume, {
      videoIndex, videoId,
    });
    await this.send(content, 'info');
  }

  async notifyObsDisconnect(): Promise<void> {
    if (!this.discord.events.obsDisconnect) return;
    await this.send(this.discord.templates.obsDisconnect, 'warn');
  }

  async notifyObsReconnect(): Promise<void> {
    if (!this.discord.events.obsReconnect) return;
    await this.send(this.discord.templates.obsReconnect, 'info');
  }
}
