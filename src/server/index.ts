import { createServer } from 'http';
import { randomBytes } from 'crypto';
import { exec } from 'child_process';
import { readFileSync } from 'fs';
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

// Read version from package.json
const pkgPath = resolve(__dirname, '..', '..', 'package.json');
const appVersion = JSON.parse(readFileSync(pkgPath, 'utf-8')).version as string;

async function main() {
  let config = loadConfig();
  logger.info({ port: config.port, playlists: config.playlists.length }, 'Starting StreamLoop');

  const apiToken = randomBytes(32).toString('hex');

  const startedAt = Date.now();
  const getUptime = () => Date.now() - startedAt;

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

  const adminUrl = `http://localhost:${config.port}/admin`;

  // Discord
  let discord = new DiscordNotifier(config, appVersion, getUptime, adminUrl);

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
    obs.onConnect(async () => {
      logger.info('OBS reconnected after config change');
      discord.notifyObsReconnect();
      if (config.obsAutoStream && playerWs.isConnected()) {
        setTimeout(async () => {
          await obs.startStreaming();
        }, 5000);
      }
    });
    obs.onDisconnect(() => {
      logger.warn('OBS disconnected');
      discord.notifyObsDisconnect();
    });
    obs.onStreamDrop((attempt, maxAttempts) => {
      discord.notifyStreamDrop(attempt, maxAttempts);
    });
    obs.onStreamRestart((attempts) => {
      discord.notifyStreamRestart(attempts);
    });
    obs.onStreamRestartFailed(() => {
      discord.notifyCritical('Stream restart failed after all attempts. Manual intervention required.');
    });
    await obs.connect();
    // Recreate discord notifier
    discord = new DiscordNotifier(config, appVersion, getUptime, adminUrl);
    // Restart recovery with new config
    recovery.stop();
    recovery = new RecoveryEngine(config, playerWs, state, obs, discord);
    recovery.start();
    startStreamMonitor();
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
    apiToken,
  });
  app.use('/api', apiRouter);

  // Connect to OBS
  obs.onConnect(async () => {
    logger.info('OBS connected, checking player status');
    discord.notifyObsReconnect();
    if (!playerWs.isConnected()) {
      logger.warn('Player not connected after OBS reconnect');
    }
    // Auto-start stream after OBS reconnect (e.g. after crash relaunch)
    if (config.obsAutoStream && playerWs.isConnected()) {
      // Give OBS a moment to fully initialize after connecting
      setTimeout(async () => {
        logger.info('Attempting auto-start stream');
        await obs.startStreaming();
      }, 5000);
    }
  });

  obs.onDisconnect(() => {
    logger.warn('OBS disconnected');
    discord.notifyObsDisconnect();
  });

  obs.onStreamDrop((attempt, maxAttempts) => {
    discord.notifyStreamDrop(attempt, maxAttempts);
  });

  obs.onStreamRestart((attempts) => {
    discord.notifyStreamRestart(attempts);
  });

  obs.onStreamRestartFailed(() => {
    discord.notifyCritical('Stream restart failed after all attempts. Manual intervention required.');
  });

  // Stream health monitor — restarts stream if it drops while player is healthy
  const startStreamMonitor = () => {
    obs.startStreamMonitor(() => {
      if (!playerWs.isConnected()) return false;
      const status = recovery.getStatus();
      const heartbeatAge = Date.now() - status.lastHeartbeatAt;
      return heartbeatAge < config.heartbeatTimeoutMs;
    });
  };
  startStreamMonitor();

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
    logger.info(`Admin URL: ${adminUrl}`);

    // Auto-open browser to admin dashboard
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

process.on('unhandledRejection', (reason) => {
  logger.error({ err: reason }, 'Unhandled promise rejection');
});

process.on('uncaughtException', (err) => {
  logger.fatal({ err }, 'Uncaught exception');
  process.exit(1);
});

main().catch((err) => {
  logger.fatal({ err }, 'Fatal error');
  process.exit(1);
});
