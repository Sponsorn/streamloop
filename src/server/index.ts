import { createServer } from 'http';
import { randomBytes } from 'crypto';
import { exec } from 'child_process';
import { readFileSync, existsSync } from 'fs';
import express from 'express';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';
import { loadConfig, isFirstRun } from './config.js';
import { logger } from './logger.js';
import { StateManager } from './state.js';
import { MpvClient } from './mpv-client.js';
import { PlaylistMetadataCache } from './playlist-metadata.js';
import { OBSClient } from './obs-client.js';
import { DiscordNotifier } from './discord.js';
import { RecoveryEngine } from './recovery.js';
import { createApiRouter } from './api.js';
import { Updater } from './updater.js';
import { TwitchLivenessChecker } from './twitch.js';

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

  // Admin dashboard static files
  const adminDir = resolve(__dirname, '..', 'admin');
  app.use('/admin', express.static(adminDir));

  // Now-playing overlay for OBS
  const overlayDir = resolve(__dirname, '..', 'overlay');
  app.use('/overlay', express.static(overlayDir));

  const server = createServer(app);

  // Resolve binary paths: check project root first (dev), then portable release structure
  const projectRoot = resolve(__dirname, '..', '..');
  const installRoot = resolve(projectRoot, '..');
  const mpvPath = existsSync(resolve(projectRoot, 'mpv', 'mpv.exe'))
    ? resolve(projectRoot, 'mpv', 'mpv.exe')
    : resolve(installRoot, 'mpv', 'mpv.exe');
  const ytdlpPath = existsSync(resolve(projectRoot, 'yt-dlp', 'yt-dlp.exe'))
    ? resolve(projectRoot, 'yt-dlp', 'yt-dlp.exe')
    : resolve(installRoot, 'yt-dlp', 'yt-dlp.exe');

  const logsDir = resolve(projectRoot, 'logs');

  const mpv = new MpvClient({
    mpvPath,
    pipePath: '\\\\.\\pipe\\mpv-streamloop',
    logsDir,
    mpvArgs: [
      '--no-border',
      '--no-osc',
      '--osd-level=0',
      `--geometry=${config.mpvGeometry}`,
      '--hwdec=auto',
      `--ytdl-format=${config.mpvYtdlFormat}`,
      '--loop-playlist=inf',
      '--ytdl-raw-options=yes-playlist=,js-runtimes=node',
      `--script-opts=ytdl_hook-ytdl_path=${ytdlpPath}`,
      ...config.mpvExtraArgs,
    ],
  });

  const playlistCache = new PlaylistMetadataCache(ytdlpPath);

  // OBS client
  let obs = new OBSClient(config);

  const adminUrl = `http://localhost:${config.port}/admin`;

  // Discord
  let discord = new DiscordNotifier(config, appVersion, getUptime, adminUrl);

  // Recovery engine
  let recovery = new RecoveryEngine(config, mpv, state, obs, discord);
  recovery.start();

  // Twitch liveness checker
  let twitch = new TwitchLivenessChecker(config, obs, discord);

  // Updater
  const updater = new Updater();

  const triggerRestart = async () => {
    logger.info('Restart requested for update');
    updater.stopAutoCheck();
    twitch.stop();
    recovery.stop();
    state.flush();
    await mpv.stop();
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
    // Recreate discord notifier before OBS connect so callbacks use the new config
    discord = new DiscordNotifier(config, appVersion, getUptime, adminUrl);
    obs.onConnect(async () => {
      logger.info('OBS reconnected after config change');
      discord.notifyObsReconnect();
      if (config.obsAutoStream) {
        const waitForVideo = setInterval(async () => {
          const status = recovery.getStatus();
          if (status.videoConfirmed) {
            clearInterval(waitForVideo);
            logger.info('Video confirmed rendering, starting stream');
            await obs.startStreaming();
          }
        }, 2000);
        setTimeout(() => clearInterval(waitForVideo), 120000);
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
    // Restart recovery with new config
    recovery.stop();
    recovery = new RecoveryEngine(config, mpv, state, obs, discord);
    recovery.start();
    startStreamMonitor();
    // Restart Twitch liveness checker with new config
    twitch.stop();
    twitch = new TwitchLivenessChecker(config, obs, discord);
    twitch.start();
    logger.info('Components reloaded with new config');
  };

  const apiRouter = createApiRouter({
    getConfig: () => config,
    getRecovery: () => recovery,
    mpv,
    playlistCache,
    getObs: () => obs,
    state,
    reloadConfig,
    updater,
    triggerRestart,
    triggerShutdown: () => shutdown(),
    getDiscord: () => discord,
    getTwitch: () => twitch,
    apiToken,
  });
  app.use('/api', apiRouter);

  // Connect to OBS
  obs.onConnect(async () => {
    logger.info('OBS connected, checking player status');
    discord.notifyObsReconnect();
    if (!mpv.isConnected()) {
      logger.warn('Player not connected after OBS reconnect');
    }
    // Auto-start stream — wait until video is confirmed rendering
    if (config.obsAutoStream) {
      const waitForVideo = setInterval(async () => {
        const status = recovery.getStatus();
        if (status.videoConfirmed) {
          clearInterval(waitForVideo);
          logger.info('Video confirmed rendering, starting stream');
          await obs.startStreaming();
        }
      }, 2000);
      // Give up after 2 minutes
      setTimeout(() => clearInterval(waitForVideo), 120000);
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
      if (!mpv.isConnected()) return false;
      const status = recovery.getStatus();
      const heartbeatAge = Date.now() - status.lastHeartbeatAt;
      return heartbeatAge < config.heartbeatTimeoutMs;
    });
  };
  startStreamMonitor();

  if (!isFirstRun(config)) {
    await mpv.start();
    await obs.connect();
    twitch.start();
  }

  // Start auto-update check if enabled
  if (config.autoUpdateCheck) {
    updater.startAutoCheck(config.updateCheckIntervalMs);
  }

  // Start HTTP server
  server.listen(config.port, '127.0.0.1', () => {
    logger.info({ port: config.port }, 'Server listening');
    logger.info(`Admin URL: ${adminUrl}`);

    // Auto-open browser to admin dashboard
    if (process.platform === 'win32') {
      exec(`start "" "${adminUrl}"`);
    }
  });

  // Graceful shutdown
  const shutdown = async () => {
    logger.info('Shutting down...');
    updater.stopAutoCheck();
    twitch.stop();
    recovery.stop();
    state.flush();
    await mpv.stop();
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
