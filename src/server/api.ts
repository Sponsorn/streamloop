import { Router } from 'express';
import { existsSync, writeFileSync, unlinkSync } from 'fs';
import { join } from 'path';
import { ZodError } from 'zod';
import { saveConfig, isFirstRun, getConfigPath, DEFAULT_DISCORD_TEMPLATES, DISCORD_TEMPLATE_VARIABLES } from './config.js';
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
  apiToken: string;
}

const STARTUP_FOLDER = join(
  process.env.APPDATA ?? '',
  'Microsoft', 'Windows', 'Start Menu', 'Programs', 'Startup',
);
const AUTOSTART_VBS = join(STARTUP_FOLDER, 'StreamLoop.vbs');

function maskConfig(config: AppConfig): Record<string, unknown> {
  return {
    ...config,
    obsWebsocketPassword: config.obsWebsocketPassword ? '********' : '',
    discord: {
      ...config.discord,
      webhookUrl: config.discord.webhookUrl ? '********' : '',
    },
  };
}

export function createApiRouter(deps: ApiDependencies): Router {
  const router = Router();

  // CSRF/auth protection: require API token on all state-changing requests.
  // Token is served via GET /token (same-origin only — no CORS headers).
  router.use((req, res, next) => {
    if (req.method === 'POST') {
      const token = req.headers['x-api-token'];
      if (token !== deps.apiToken) {
        return res.status(403).json({ error: 'Forbidden: invalid API token' });
      }
    }
    next();
  });

  router.get('/token', (_req, res) => {
    res.json({ token: deps.apiToken });
  });

  router.get('/status', async (_req, res) => {
    const config = deps.getConfig();
    const status = deps.getRecovery().getStatus();
    const obs = deps.getObs();
    const obsConnected = obs.isConnected();
    const obsStreaming = obsConnected ? await obs.isStreaming() : false;
    res.json({
      playerConnected: deps.playerWs.isConnected(),
      obsConnected,
      obsStreaming,
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
    res.json(maskConfig(deps.getConfig()));
  });

  router.post('/config', (req, res) => {
    try {
      const body = req.body as Partial<AppConfig>;
      // Don't overwrite masked credentials
      if (body.obsWebsocketPassword === '********') {
        delete body.obsWebsocketPassword;
      }
      // Handle nested discord webhook masking
      if (body.discord?.webhookUrl === '********') {
        delete body.discord.webhookUrl;
      }
      const updated = saveConfig(body);
      logger.info('Config updated via API');
      // Trigger reload asynchronously
      deps.reloadConfig().catch((err) => {
        logger.error({ err }, 'Failed to reload after config update');
      });
      res.json({ ok: true, config: maskConfig(updated) });
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

  // --- Discord endpoints ---

  router.post('/discord/test', async (_req, res) => {
    const config = deps.getConfig();
    if (!config.discord.webhookUrl) {
      return res.status(400).json({ error: 'No Discord webhook URL configured' });
    }
    try {
      await deps.getDiscord().send('Test notification from StreamLoop. If you see this, your webhook is working!', 'info');
      res.json({ ok: true });
    } catch (err) {
      res.status(500).json({ error: String(err) });
    }
  });

  router.get('/discord/defaults', (_req, res) => {
    res.json({
      templates: DEFAULT_DISCORD_TEMPLATES,
      variables: DISCORD_TEMPLATE_VARIABLES,
    });
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
