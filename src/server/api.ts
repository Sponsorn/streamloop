import { Router } from 'express';
import { existsSync, writeFileSync, unlinkSync } from 'fs';
import { join } from 'path';
import { ZodError } from 'zod';
import { saveConfig, isFirstRun, getConfigPath } from './config.js';
import { logger } from './logger.js';
import type { AppConfig } from './types.js';
import type { RecoveryEngine } from './recovery.js';
import type { PlayerWebSocket } from './websocket.js';
import type { OBSClient } from './obs-client.js';
import type { StateManager } from './state.js';
import type { Updater } from './updater.js';
import type { DiscordNotifier } from './discord.js';

export interface ApiDependencies {
  getConfig: () => AppConfig;
  getRecovery: () => RecoveryEngine;
  playerWs: PlayerWebSocket;
  getObs: () => OBSClient;
  state: StateManager;
  reloadConfig: () => Promise<void>;
  updater: Updater;
  triggerRestart: () => void;
  getDiscord: () => DiscordNotifier;
}

const STARTUP_FOLDER = join(
  process.env.APPDATA ?? '',
  'Microsoft', 'Windows', 'Start Menu', 'Programs', 'Startup',
);
const AUTOSTART_VBS = join(STARTUP_FOLDER, 'FreezeMonitor.vbs');

export function createApiRouter(deps: ApiDependencies): Router {
  const router = Router();

  router.get('/status', (_req, res) => {
    const config = deps.getConfig();
    const status = deps.getRecovery().getStatus();
    res.json({
      playerConnected: deps.playerWs.isConnected(),
      obsConnected: deps.getObs().isConnected(),
      recoveryStep: status.recoveryStep,
      lastHeartbeatAt: status.lastHeartbeatAt,
      heartbeatIntervalMs: config.heartbeatIntervalMs,
      consecutiveErrors: status.consecutiveErrors,
      totalVideos: status.totalVideos,
      uptimeMs: status.uptimeMs,
      playlistIndex: status.playlistIndex,
      totalPlaylists: status.totalPlaylists,
      firstRun: isFirstRun(config),
    });
  });

  router.get('/state', (_req, res) => {
    const state = deps.state.get();
    res.json(state);
  });

  router.get('/config', (_req, res) => {
    const config = deps.getConfig();
    const safe = {
      ...config,
      obsWebsocketPassword: config.obsWebsocketPassword ? '********' : '',
      discordWebhookUrl: config.discordWebhookUrl ? '********' : '',
    };
    res.json(safe);
  });

  router.post('/config', (req, res) => {
    try {
      const body = req.body as Partial<AppConfig>;
      // Don't overwrite masked credentials
      if (body.obsWebsocketPassword === '********') {
        delete body.obsWebsocketPassword;
      }
      if (body.discordWebhookUrl === '********') {
        delete body.discordWebhookUrl;
      }
      const updated = saveConfig(body);
      logger.info('Config updated via API');
      // Trigger reload asynchronously
      deps.reloadConfig().catch((err) => {
        logger.error({ err }, 'Failed to reload after config update');
      });
      res.json({ ok: true, config: {
        ...updated,
        obsWebsocketPassword: updated.obsWebsocketPassword ? '********' : '',
        discordWebhookUrl: updated.discordWebhookUrl ? '********' : '',
      } });
    } catch (err) {
      logger.error({ err }, 'Failed to save config');
      if (err instanceof ZodError) {
        const messages = err.issues.map(i => `${i.path.join('.')}: ${i.message}`);
        res.status(400).json({ error: messages.join('; ') });
      } else {
        res.status(400).json({ error: err instanceof Error ? err.message : String(err) });
      }
    }
  });

  router.get('/events', (_req, res) => {
    res.json(deps.getRecovery().getEvents());
  });

  router.get('/autostart', (_req, res) => {
    res.json({ enabled: existsSync(AUTOSTART_VBS) });
  });

  router.post('/autostart', (req, res) => {
    const { enabled } = req.body as { enabled: boolean };
    try {
      if (enabled) {
        const appRoot = getConfigPath().replace(/[/\\]config\.json$/, '');
        const batPath = join(appRoot, '..', 'START.bat');
        const vbsContent = [
          'Set WshShell = CreateObject("WScript.Shell")',
          `WshShell.Run """${batPath}""", 0, False`,
        ].join('\r\n');
        writeFileSync(AUTOSTART_VBS, vbsContent, 'utf-8');
        logger.info('Autostart shortcut created');
      } else {
        if (existsSync(AUTOSTART_VBS)) {
          unlinkSync(AUTOSTART_VBS);
          logger.info('Autostart shortcut removed');
        }
      }
      res.json({ ok: true, enabled });
    } catch (err) {
      logger.error({ err }, 'Failed to set autostart');
      res.status(500).json({ error: String(err) });
    }
  });

  // --- OBS path endpoints ---

  const OBS_SEARCH_PATHS = [
    'C:\\Program Files\\obs-studio\\bin\\64bit\\obs64.exe',
    'C:\\Program Files (x86)\\obs-studio\\bin\\64bit\\obs64.exe',
  ];

  router.post('/obs-path/detect', (_req, res) => {
    for (const p of OBS_SEARCH_PATHS) {
      if (existsSync(p)) {
        return res.json({ found: true, path: p });
      }
    }
    res.json({ found: false, path: '' });
  });

  router.post('/obs-path/validate', (req, res) => {
    const { path } = req.body as { path: string };
    if (!path || typeof path !== 'string') {
      return res.json({ valid: false, error: 'No path provided' });
    }
    if (!existsSync(path)) {
      return res.json({ valid: false, error: 'File not found' });
    }
    const lower = path.toLowerCase();
    if (!lower.endsWith('.exe')) {
      return res.json({ valid: false, error: 'Not an executable (.exe) file' });
    }
    if (!lower.includes('obs')) {
      return res.json({ valid: false, error: 'Doesn\'t look like an OBS executable' });
    }
    res.json({ valid: true });
  });

  // --- Discord test ---

  router.post('/discord/test', async (_req, res) => {
    const config = deps.getConfig();
    if (!config.discordWebhookUrl) {
      return res.status(400).json({ error: 'No Discord webhook URL configured' });
    }
    try {
      await deps.getDiscord().send('Test notification from Freeze Monitor. If you see this, your webhook is working!', 'info');
      res.json({ ok: true });
    } catch (err) {
      res.status(500).json({ error: String(err) });
    }
  });

  // --- Update endpoints ---

  router.get('/update/status', (_req, res) => {
    res.json(deps.updater.getStatus());
  });

  router.post('/update/check', async (_req, res) => {
    try {
      const status = await deps.updater.checkForUpdate();
      res.json(status);
    } catch (err) {
      res.status(500).json({ error: String(err) });
    }
  });

  router.post('/update/apply', async (_req, res) => {
    try {
      await deps.updater.downloadAndApply();
      res.json({ ok: true, message: 'Update applied, restarting...' });
      // Trigger restart after sending response
      setTimeout(() => deps.triggerRestart(), 1000);
    } catch (err) {
      logger.error({ err }, 'Failed to apply update');
      res.status(500).json({ error: err instanceof Error ? err.message : String(err) });
    }
  });

  return router;
}
