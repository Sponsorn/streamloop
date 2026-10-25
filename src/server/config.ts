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
  streamDrop: 'Stream stopped unexpectedly — attempting restart (attempt {attempt}/{maxAttempts})',
  streamRestart: 'Stream restarted successfully after {attempts} attempt(s)',
  outputFrozen: 'Output frozen ({kind}) on video #{videoIndex}',
  twitchMismatch: 'Twitch liveness mismatch — the encoder reports streaming but **{channel}** is offline on Twitch. Restarting stream.',
  twitchRestart: 'Stream restarted after Twitch liveness mismatch on **{channel}**',
};

export const DISCORD_TEMPLATE_VARIABLES: Record<keyof DiscordTemplates, string[]> = {
  error: ['videoIndex', 'videoId', 'errorCode', 'attempt'],
  skip: ['videoIndex', 'videoId', 'reason'],
  recovery: ['step'],
  critical: ['message'],
  resume: ['videoIndex', 'videoId'],
  streamDrop: ['attempt', 'maxAttempts'],
  streamRestart: ['attempts'],
  outputFrozen: ['kind', 'videoIndex'],
  twitchMismatch: ['channel'],
  twitchRestart: ['channel'],
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
    streamDrop: z.boolean().default(true),
    streamRestart: z.boolean().default(true),
    outputFrozen: z.boolean().default(true),
    twitchMismatch: z.boolean().default(true),
    twitchRestart: z.boolean().default(true),
  }).default({}),
  templates: z.object({
    error: z.string().default(DEFAULT_DISCORD_TEMPLATES.error),
    skip: z.string().default(DEFAULT_DISCORD_TEMPLATES.skip),
    recovery: z.string().default(DEFAULT_DISCORD_TEMPLATES.recovery),
    critical: z.string().default(DEFAULT_DISCORD_TEMPLATES.critical),
    resume: z.string().default(DEFAULT_DISCORD_TEMPLATES.resume),
    streamDrop: z.string().default(DEFAULT_DISCORD_TEMPLATES.streamDrop),
    streamRestart: z.string().default(DEFAULT_DISCORD_TEMPLATES.streamRestart),
    outputFrozen: z.string().default(DEFAULT_DISCORD_TEMPLATES.outputFrozen),
    twitchMismatch: z.string().default(DEFAULT_DISCORD_TEMPLATES.twitchMismatch),
    twitchRestart: z.string().default(DEFAULT_DISCORD_TEMPLATES.twitchRestart),
  }).default({}),
}).default({});

// Bounds and defaults match supervisor/src/config.rs.
const supervisorSchema = z.object({
  ingestUrl: z.string().default('rtmp://live.twitch.tv/app'),
  streamKeyProtected: z.string().default(''),
  encoder: z.enum(['h264_nvenc', 'h264_amf', 'h264_qsv', 'libx264']).default('h264_nvenc'),
  cacheBudgetMb: z.number().int().min(50).default(300),
  readAheadSeconds: z.number().min(2).default(20),
  playerClient: z.string().default('mweb,web_embedded'),
  overlay: z.boolean().default(true),
  freezeSeconds: z.number().min(5).default(20),
  silenceSeconds: z.number().min(5).default(30),
}).passthrough().default({}); // passthrough: keys from a newer supervisor survive

const configSchema = z.object({
  port: z.number().int().positive().default(7654),
  playlists: z.array(z.object({ id: z.string().min(1), name: z.string().optional() })).min(1),
  discord: discordSchema,
  supervisor: supervisorSchema,
  autoUpdateCheck: z.boolean().default(true),
  updateCheckIntervalMs: z.number().int().positive().default(21600000),
  autoUpdateYtdlp: z.boolean().default(true),
  autoOpenAdmin: z.boolean().default(true),
  twitchClientId: z.string().default(''),
  twitchClientSecret: z.string().default(''),
  twitchChannel: z.string().default(''),
  twitchLivenessEnabled: z.boolean().default(false),
  twitchPollIntervalMs: z.number().int().min(30000).default(60000),
  ytdlCookiesFromBrowser: z.string().default(''),
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
      logger.debug('Config written back (new defaults or key reorder)');
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

  // Same for supervisor. A save never clears the stream key: an empty
  // streamKeyProtected is ignored, only the stream-key route writes a non-empty one.
  if (incoming.supervisor && typeof incoming.supervisor === 'object') {
    const patch = { ...(incoming.supervisor as Record<string, unknown>) };
    if (!patch.streamKeyProtected) delete patch.streamKeyProtected;
    incoming.supervisor = { ...((existing.supervisor as object) ?? {}), ...patch };
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
