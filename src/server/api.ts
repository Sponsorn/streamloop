import { Router } from 'express';
import { existsSync, writeFileSync, unlinkSync } from 'fs';
import { join } from 'path';
import { saveConfig, isFirstRun, getConfigPath } from './config.js';
import { logger } from './logger.js';
import type { AppConfig } from './types.js';
import type { RecoveryEngine } from './recovery.js';
import type { PlayerWebSocket } from './websocket.js';
import type { OBSClient } from './obs-client.js';
import type { StateManager } from './state.js';

export interface ApiDependencies {
  getConfig: () => AppConfig;
  getRecovery: () => RecoveryEngine;
  playerWs: PlayerWebSocket;
  getObs: () => OBSClient;
  state: StateManager;
  reloadConfig: () => Promise<void>;
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
      consecutiveErrors: status.consecutiveErrors,
      totalVideos: status.totalVideos,
      uptimeMs: status.uptimeMs,
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
    };
    res.json(safe);
  });

  router.post('/config', (req, res) => {
    try {
      const body = req.body as Partial<AppConfig>;
      // Don't overwrite password with the mask
      if (body.obsWebsocketPassword === '********') {
        delete body.obsWebsocketPassword;
      }
      const updated = saveConfig(body);
      logger.info('Config updated via API');
      // Trigger reload asynchronously
      deps.reloadConfig().catch((err) => {
        logger.error({ err }, 'Failed to reload after config update');
      });
      res.json({ ok: true, config: { ...updated, obsWebsocketPassword: updated.obsWebsocketPassword ? '********' : '' } });
    } catch (err) {
      logger.error({ err }, 'Failed to save config');
      res.status(400).json({ error: String(err) });
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

  return router;
}
