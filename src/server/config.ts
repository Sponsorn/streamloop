import { readFileSync, writeFileSync, renameSync } from 'fs';
import { resolve } from 'path';
import { z } from 'zod';
import type { AppConfig } from './types.js';

const configSchema = z.object({
  port: z.number().int().positive().default(3000),
  obsWebsocketUrl: z.string().default('ws://127.0.0.1:4455'),
  obsWebsocketPassword: z.string().default(''),
  obsBrowserSourceName: z.string().min(1),
  playlists: z.array(z.object({ id: z.string().min(1), name: z.string().optional() })).min(1),
  discordWebhookUrl: z.string().default(''),
  heartbeatIntervalMs: z.number().int().positive().default(5000),
  heartbeatTimeoutMs: z.number().int().positive().default(15000),
  stateFilePath: z.string().default('./state.json').refine(
    (p) => !p.includes('..') && /^\.?[/\\]?[\w.\-]+\.json$/i.test(p),
    { message: 'stateFilePath must be a simple filename like ./state.json' },
  ),
  maxConsecutiveErrors: z.number().int().positive().default(3),
  recoveryDelayMs: z.number().int().positive().default(5000),
  obsAutoRestart: z.boolean().default(false),
  obsPath: z.string().default(''),
  autoUpdateCheck: z.boolean().default(true),
  updateCheckIntervalMs: z.number().int().positive().default(21600000),
});

let resolvedConfigPath = '';

export function loadConfig(path?: string): AppConfig {
  resolvedConfigPath = resolve(path ?? 'config.json');
  const raw = readFileSync(resolvedConfigPath, 'utf-8');
  const json = JSON.parse(raw);
  // Migrate old single-playlist format
  if (json.playlistId && !json.playlists) {
    json.playlists = [{ id: json.playlistId }];
    delete json.playlistId;
  }
  return configSchema.parse(json);
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
  const merged = { ...existing, ...config } as Record<string, unknown>;
  // Migrate old single-playlist format
  if (merged.playlistId && !merged.playlists) {
    merged.playlists = [{ id: merged.playlistId }];
    delete merged.playlistId;
  }
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
