import { createServer } from 'http';
import { exec } from 'child_process';
import express from 'express';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';
import { loadConfig, isFirstRun } from './config.js';
import { logger } from './logger.js';
import { StateManager } from './state.js';
import { PlayerWebSocket } from './websocket.js';
import { OBSClient } from './obs-client.js';
import { DiscordNotifier } from './discord.js';
import { RecoveryEngine } from './recovery.js';
import { createApiRouter } from './api.js';
import { Updater } from './updater.js';

const __dirname = dirname(fileURLToPath(import.meta.url));

async function main() {
  let config = loadConfig();
  logger.info({ port: config.port, playlists: config.playlists.length }, 'Starting freeze-monitor');

  // State
  const state = new StateManager(config.stateFilePath);

  // Express + HTTP server
  const app = express();
  app.use(express.json());

  const playerDir = resolve(__dirname, '..', 'player');
  app.use(express.static(playerDir));

  // Admin dashboard static files
  const adminDir = resolve(__dirname, '..', 'admin');
  app.use('/admin', express.static(adminDir));

  // Now-playing overlay for OBS
  const overlayDir = resolve(__dirname, '..', 'overlay');
  app.use('/overlay', express.static(overlayDir));

  const server = createServer(app);

  // WebSocket
  const playerWs = new PlayerWebSocket(server);

  // OBS client
  let obs = new OBSClient(config);

  // Discord
  let discord = new DiscordNotifier(config);

  // Recovery engine
  let recovery = new RecoveryEngine(config, playerWs, state, obs, discord);
  recovery.start();

  // Updater
  const updater = new Updater();

  const triggerRestart = () => {
    logger.info('Restart requested for update');
    updater.stopAutoCheck();
    recovery.stop();
    state.flush();
    playerWs.close();
    obs.disconnect();
    server.close(() => {
      logger.info('Server closed for update restart');
    });
    // Exit code 75 signals START.bat to restart
    setTimeout(() => process.exit(75), 2000);
  };

  // API router — uses getters so reloaded components are always current
  const reloadConfig = async () => {
    const newConfig = loadConfig();
    config = newConfig;
    // Reconnect OBS if settings changed
    obs.disconnect();
    obs = new OBSClient(config);
    obs.onConnect(() => logger.info('OBS reconnected after config change'));
    obs.onDisconnect(() => logger.warn('OBS disconnected'));
    await obs.connect();
    // Recreate discord notifier
    discord = new DiscordNotifier(config);
    // Restart recovery with new config
    recovery.stop();
    recovery = new RecoveryEngine(config, playerWs, state, obs, discord);
    recovery.start();
    logger.info('Components reloaded with new config');
  };

  const apiRouter = createApiRouter({
    getConfig: () => config,
    getRecovery: () => recovery,
    playerWs,
    getObs: () => obs,
    state,
    reloadConfig,
    updater,
    triggerRestart,
    getDiscord: () => discord,
  });
  app.use('/api', apiRouter);

  // Connect to OBS
  obs.onConnect(() => {
    logger.info('OBS connected, checking player status');
    if (!playerWs.isConnected()) {
      logger.warn('Player not connected after OBS reconnect');
    }
  });

  obs.onDisconnect(() => {
    logger.warn('OBS disconnected');
  });

  if (!isFirstRun(config)) {
    await obs.connect();
  }

  // Start auto-update check if enabled
  if (config.autoUpdateCheck) {
    updater.startAutoCheck(config.updateCheckIntervalMs);
  }

  // Start HTTP server
  server.listen(config.port, '127.0.0.1', () => {
    logger.info({ port: config.port }, 'Server listening');
    logger.info(`Player URL: http://localhost:${config.port}`);
    logger.info(`Admin URL: http://localhost:${config.port}/admin`);

    // Auto-open browser to admin dashboard
    const adminUrl = `http://localhost:${config.port}/admin`;
    if (process.platform === 'win32') {
      exec(`start "" "${adminUrl}"`);
    }
  });

  // Graceful shutdown
  const shutdown = () => {
    logger.info('Shutting down...');
    updater.stopAutoCheck();
    recovery.stop();
    state.flush();
    playerWs.close();
    obs.disconnect();
    server.close(() => {
      logger.info('Server closed');
      process.exit(0);
    });
    // Force exit after 5s
    setTimeout(() => process.exit(1), 5000);
  };

  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);
}

main().catch((err) => {
  logger.fatal({ err }, 'Fatal error');
  process.exit(1);
});
