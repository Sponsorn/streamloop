import { readFileSync, writeFileSync, renameSync } from 'fs';
import { resolve } from 'path';
import { z } from 'zod';
import type { AppConfig, DiscordTemplates } from './types.js';
import { logger } from './logger.js';

export const DEFAULT_DISCORD_TEMPLATES: DiscordTemplates = {
  error: 'Playback error **{errorCode}** on video #{videoIndex} (`{videoId}`)\nRetry attempt: {attempt}',
  skip: 'Skipping video #{videoIndex} (`{videoId}`)\nReason: {reason}',
  recovery: 'Recovery action: **{step}**',
  critical: '**CRITICAL:** {message}',
  resume: 'Playback resumed at video #{videoIndex} (`{videoId}`)',
  obsDisconnect: 'OBS disconnected — attempting to reconnect',
  obsReconnect: 'OBS reconnected',
  streamDrop: 'OBS stream stopped unexpectedly — attempting restart (attempt {attempt}/{maxAttempts})',
  streamRestart: 'OBS stream restarted successfully after {attempts} attempt(s)',
};

export const DISCORD_TEMPLATE_VARIABLES: Record<keyof DiscordTemplates, string[]> = {
  error: ['videoIndex', 'videoId', 'errorCode', 'attempt'],
  skip: ['videoIndex', 'videoId', 'reason'],
  recovery: ['step'],
  critical: ['message'],
  resume: ['videoIndex', 'videoId'],
  obsDisconnect: [],
  obsReconnect: [],
  streamDrop: ['attempt', 'maxAttempts'],
  streamRestart: ['attempts'],
};

const discordSchema = z.object({
  webhookUrl: z.string().default(''),
  botName: z.string().default(''),
  avatarUrl: z.string().default(''),
  rolePing: z.string().default(''),
  events: z.object({
    error: z.boolean().default(true),
    skip: z.boolean().default(true),
    recovery: z.boolean().default(true),
    critical: z.boolean().default(true),
    resume: z.boolean().default(true),
    obsDisconnect: z.boolean().default(true),
    obsReconnect: z.boolean().default(true),
    streamDrop: z.boolean().default(true),
    streamRestart: z.boolean().default(true),
  }).default({}),
  templates: z.object({
    error: z.string().default(DEFAULT_DISCORD_TEMPLATES.error),
    skip: z.string().default(DEFAULT_DISCORD_TEMPLATES.skip),
    recovery: z.string().default(DEFAULT_DISCORD_TEMPLATES.recovery),
    critical: z.string().default(DEFAULT_DISCORD_TEMPLATES.critical),
    resume: z.string().default(DEFAULT_DISCORD_TEMPLATES.resume),
    obsDisconnect: z.string().default(DEFAULT_DISCORD_TEMPLATES.obsDisconnect),
    obsReconnect: z.string().default(DEFAULT_DISCORD_TEMPLATES.obsReconnect),
    streamDrop: z.string().default(DEFAULT_DISCORD_TEMPLATES.streamDrop),
    streamRestart: z.string().default(DEFAULT_DISCORD_TEMPLATES.streamRestart),
  }).default({}),
}).default({});

const configSchema = z.object({
  port: z.number().int().positive().default(7654),
  obsWebsocketUrl: z.string().default('ws://127.0.0.1:4455'),
  obsWebsocketPassword: z.string().default(''),
  obsBrowserSourceName: z.string().min(1),
  playlists: z.array(z.object({ id: z.string().min(1), name: z.string().optional() })).min(1),
  discord: discordSchema,
  heartbeatIntervalMs: z.number().int().positive().default(5000),
  heartbeatTimeoutMs: z.number().int().positive().default(15000),
  stateFilePath: z.string().default('./state.json').refine(
    (p) => !p.includes('..') && /^\.?[/\\]?[\w.\-]+\.json$/i.test(p),
    { message: 'stateFilePath must be a simple filename like ./state.json' },
  ),
  maxConsecutiveErrors: z.number().int().positive().default(3),
  recoveryDelayMs: z.number().int().positive().default(5000),
  obsAutoRestart: z.boolean().default(false),
  obsAutoStream: z.boolean().default(false),
  obsPath: z.string().default('').refine(
    (p) => {
      if (!p) return true;
      const lower = p.toLowerCase();
      return lower.endsWith('.exe') && lower.includes('obs');
    },
    { message: 'obsPath must point to an OBS executable (e.g. obs64.exe)' },
  ),
  autoUpdateCheck: z.boolean().default(true),
  updateCheckIntervalMs: z.number().int().positive().default(21600000),
  qualityRecoveryEnabled: z.boolean().default(true),
  minQuality: z.string().default('hd720'),
  qualityRecoveryDelayMs: z.number().int().positive().default(120000),
});

let resolvedConfigPath = '';

function migrateJson(json: Record<string, unknown>): void {
  // Migrate old single-playlist format
  if (json.playlistId && !json.playlists) {
    json.playlists = [{ id: json.playlistId }];
    delete json.playlistId;
  }
  // Migrate old flat discordWebhookUrl to nested discord object
  if (json.discordWebhookUrl !== undefined && !json.discord) {
    json.discord = { webhookUrl: json.discordWebhookUrl };
    delete json.discordWebhookUrl;
  }
}

export function loadConfig(path?: string): AppConfig {
  resolvedConfigPath = resolve(path ?? 'config.json');
  const raw = readFileSync(resolvedConfigPath, 'utf-8');
  const json = JSON.parse(raw);
  migrateJson(json);
  const validated = configSchema.parse(json);

  // Write back if Zod filled in new defaults (e.g. after an update adds new fields)
  const validatedStr = JSON.stringify(validated, null, 2);
  if (validatedStr !== JSON.stringify(json, null, 2)) {
    try {
      const tmpPath = resolvedConfigPath + '.tmp';
      writeFileSync(tmpPath, validatedStr, 'utf-8');
      renameSync(tmpPath, resolvedConfigPath);
      logger.info('Config updated with new default fields');
    } catch (err) {
      logger.warn({ err }, 'Failed to write back config defaults');
    }
  }

  return validated;
}

export function saveConfig(config: Partial<AppConfig>, path?: string): AppConfig {
  const targetPath = resolve(path ?? (resolvedConfigPath || 'config.json'));
  // Read existing config to merge with
  let existing: Record<string, unknown> = {};
  try {
    existing = JSON.parse(readFileSync(targetPath, 'utf-8'));
  } catch {
    // No existing file, start fresh
  }
  migrateJson(existing);

  // Deep-merge discord object so partial saves don't clobber nested fields
  const incoming = config as Record<string, unknown>;
  if (incoming.discord && typeof incoming.discord === 'object' && existing.discord && typeof existing.discord === 'object') {
    const existingDiscord = existing.discord as Record<string, unknown>;
    const incomingDiscord = incoming.discord as Record<string, unknown>;
    incoming.discord = {
      ...existingDiscord,
      ...incomingDiscord,
      events: { ...(existingDiscord.events as object || {}), ...(incomingDiscord.events as object || {}) },
      templates: { ...(existingDiscord.templates as object || {}), ...(incomingDiscord.templates as object || {}) },
    };
  }

  const merged = { ...existing, ...incoming } as Record<string, unknown>;
  migrateJson(merged);
  const validated = configSchema.parse(merged);
  const tmpPath = targetPath + '.tmp';
  writeFileSync(tmpPath, JSON.stringify(validated, null, 2), 'utf-8');
  renameSync(tmpPath, targetPath);
  return validated;
}

export function isFirstRun(config: AppConfig): boolean {
  return config.playlists.length === 1 && config.playlists[0].id.includes('x'.repeat(5));
}

export function getConfigPath(): string {
  return resolvedConfigPath;
}
