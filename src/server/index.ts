import { createServer } from 'http';
import { randomBytes } from 'crypto';
import { exec } from 'child_process';
import { readFileSync, existsSync } from 'fs';
import express from 'express';
import { resolve, dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { loadConfig, isFirstRun, saveConfig } from './config.js';
import { logger } from './logger.js';
import { PlaylistMetadataCache } from './playlist-metadata.js';
import { DiscordNotifier } from './discord.js';
import { createApiRouter } from './api.js';
import { Updater } from './updater.js';
import { TwitchLivenessChecker } from './twitch.js';
import { EventStore } from './event-store.js';
import { EventLog } from './event-log.js';
import { updateYtdlp } from './ytdlp-updater.js';
import { SPAWN_CWD } from './spawn-cwd.js';
import { SupervisorClient } from './supervisor-client.js';
import { SupervisorLauncher, installRoot } from './supervisor-launcher.js';
import { AlertGate, SupervisorEvents, launcherEvent, type SupervisorEvent } from './supervisor-events.js';
import { launchGuard } from './setup-check.js';
import type { AppConfig } from './types.js';

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

  const app = express();

  // Admin dashboard static files
  const adminDir = resolve(__dirname, '..', 'admin');
  app.use('/admin', express.static(adminDir));

  const server = createServer(app);

  // app\ in a release, the repo in dev; ROOT holds supervisor\ and data\.
  const appDir = resolve(__dirname, '..', '..');
  const root = installRoot(appDir, process.env);
  const ytdlpPath = existsSync(resolve(appDir, 'yt-dlp', 'yt-dlp.exe'))
    ? resolve(appDir, 'yt-dlp', 'yt-dlp.exe')
    : resolve(appDir, '..', 'yt-dlp', 'yt-dlp.exe');

  const nodeLogsDir = resolve(appDir, 'logs');
  const eventLog = new EventLog(new EventStore({ dir: nodeLogsDir }));
  const playlistCache = new PlaylistMetadataCache(ytdlpPath, config.ytdlCookiesFromBrowser);
  const client = new SupervisorClient();

  const adminUrl = `http://localhost:${config.port}/admin`;
  let discord = new DiscordNotifier(config, appVersion, getUptime, adminUrl);
  // The Twitch read-back pulls frames with the install's ffmpeg; without a ROOT it stays off.
  // STREAMLOOP_TEST_HLS_URL is for the end-to-end runs against local MediaMTX; config.json cannot set it.
  const testHlsUrl = process.env.STREAMLOOP_TEST_HLS_URL || null;
  if (testHlsUrl) logger.warn('STREAMLOOP_TEST_HLS_URL is set: the Twitch read-back reads a local test stream and Twitch is never asked');
  const twitchOpts = { ffmpegPath: root ? join(root, 'ffmpeg', 'ffmpeg.exe') : null, addEvent: (m: string) => eventLog.addEvent(m), restartClock: { lastAttemptAt: null, alertedAt: null }, testHlsUrl };
  let twitch = new TwitchLivenessChecker(config, client, discord, twitchOpts);

  // One gate for the process lifetime, so supervisor and launcher alerts share its streaks.
  const gate = new AlertGate();
  const onEvent = (e: SupervisorEvent): void => {
    const d = gate.describe(e);
    if (d.message) eventLog.addEvent(d.message);
    if (d.discord) void discord.notifyEvent(d.discord);
  };

  const debugFaults = process.env.STREAMLOOP_DEBUG_FAULTS === '1';
  if (debugFaults) logger.warn('STREAMLOOP_DEBUG_FAULTS=1: the supervisor starts with --debug-faults');
  const launcher = new SupervisorLauncher({
    root,
    client,
    debugFaults,
    onEvent: (kind, detail) => onEvent(launcherEvent(kind, detail)),
    beforeLaunch: root
      ? launchGuard({
        root,
        getEncoder: () => config.supervisor.encoder,
        saveEncoder: (encoder) => {
          config = saveConfig({ supervisor: { encoder } } as Partial<AppConfig>);
        },
        onEvent: (kind, detail) => onEvent(launcherEvent(kind, detail)),
      })
      : undefined,
  });
  const supervisorEvents = root ? new SupervisorEvents({ logsDir: join(root, 'data', 'logs'), onEvent }) : null;
  if (!root) logger.warn('No install root (set STREAMLOOP_ROOT in dev): the supervisor cannot be launched');

  const updater = new Updater();

  // A first run has only the placeholder playlist: nothing starts until the wizard saves a real one.
  let started = false;
  const startServices = (): void => {
    if (started || isFirstRun(config)) return;
    started = true;
    launcher.start();
    supervisorEvents?.start();
    twitch.start();
  };

  const applyConfig = (next: AppConfig): void => {
    config = next;
    discord = new DiscordNotifier(config, appVersion, getUptime, adminUrl);
    twitch.stop();
    twitch = new TwitchLivenessChecker(config, client, discord, twitchOpts);
    if (started) twitch.start();
    else startServices();
    logger.info('Components reloaded with new config');
  };

  // Node's own timers only: the supervisor and the stream keep running.
  const stopNode = (): void => {
    updater.stopAutoCheck();
    twitch.stop();
    launcher.stop();
    supervisorEvents?.stop();
  };

  const triggerRestart = (): void => {
    logger.info('Restart requested for update');
    stopNode();
    server.close(() => {
      logger.info('Server closed for update restart');
    });
    // Exit code 75 signals START.bat to restart
    setTimeout(() => process.exit(75), 2000);
  };

  const shutdown = (): void => {
    logger.info('Shutting down...');
    stopNode();
    server.close(() => {
      logger.info('Server closed');
      process.exit(0);
    });
    // Force exit after 5s
    setTimeout(() => process.exit(1), 5000);
  };

  app.use('/api', createApiRouter({
    getConfig: () => config,
    applyConfig,
    client,
    launcher,
    eventLog,
    root,
    nodeLogsDir,
    playlistCache,
    ytdlpPath,
    updater,
    triggerRestart,
    triggerShutdown: shutdown,
    getDiscord: () => discord,
    getTwitch: () => twitch,
    getUptime,
    apiToken,
    testHooks: process.env.STREAMLOOP_TEST_HOOKS === '1',
  }));

  // 2.x binds the same port: while it runs, exit and let START.bat retry until it is stopped.
  server.on('error', (err: NodeJS.ErrnoException) => {
    if (err.code === 'EADDRINUSE') {
      logger.fatal(`Port ${config.port} is in use. Is StreamLoop 2.x running? Stop it and 3.0 starts by itself.`);
      process.exit(1);
    }
    logger.fatal({ err }, 'Server error');
    process.exit(1);
  });

  // Nothing starts before the port is ours, so a running 2.x never gets a second stream beside it.
  server.listen(config.port, '127.0.0.1', () => {
    logger.info({ port: config.port }, 'Server listening');
    logger.info(`Admin URL: ${adminUrl}`);

    startServices();

    if (config.autoUpdateCheck) {
      updater.startAutoCheck(config.updateCheckIntervalMs);
    }

    // Keep yt-dlp current. An outdated yt-dlp silently truncates YouTube playlists
    // (it stops after the first ~100-entry page), so refresh it in the background on
    // startup. Fire-and-forget: it must never block server boot or playback.
    if (config.autoUpdateYtdlp) {
      void updateYtdlp(ytdlpPath).then((result) => {
        if (result.ok) {
          logger.info({ version: result.version }, 'yt-dlp auto-update complete');
          playlistCache.clear();
        } else {
          logger.warn({ error: result.error }, 'yt-dlp auto-update skipped');
        }
      });
    }

    // Auto-open browser to admin dashboard
    if (config.autoOpenAdmin && process.platform === 'win32') {
      exec(`start "" "${adminUrl}"`, { cwd: SPAWN_CWD });
    }
  });

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
