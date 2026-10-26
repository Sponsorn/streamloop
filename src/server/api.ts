import express, { Router, type Response, type NextFunction, type Request } from 'express';
import { existsSync, writeFileSync, renameSync, unlinkSync, readdirSync, statSync, readFileSync, openSync, readSync, closeSync } from 'fs';
import { join, basename, resolve } from 'path';
import { freemem, totalmem } from 'os';
import { ZodError } from 'zod';
import { saveConfig as saveConfigFile, isFirstRun, getConfigPath, DEFAULT_DISCORD_TEMPLATES, DISCORD_TEMPLATE_VARIABLES } from './config.js';
import { logger } from './logger.js';
import type { AppConfig } from './types.js';
import type { PlaylistMetadataCache } from './playlist-metadata.js';
import type { Updater } from './updater.js';
import type { DiscordNotifier } from './discord.js';
import type { TwitchLivenessChecker } from './twitch.js';
import { protectStreamKey, type SupervisorClient, type SupervisorStatus } from './supervisor-client.js';
import type { LauncherState, SupervisorLauncher } from './supervisor-launcher.js';
import type { EventLog } from './event-log.js';
import { updateYtdlp } from './ytdlp-updater.js';
import { readSetupReport, runSetupCheck as runSetupCheckDefault, type SetupReport } from './setup-check.js';

export interface ApiDependencies {
  getConfig: () => AppConfig;
  /** Takes a just-saved config into the running server (Discord, Twitch, launcher). */
  applyConfig: (config: AppConfig) => void;
  saveConfig?: (patch: Partial<AppConfig>) => AppConfig;
  client: Pick<SupervisorClient, 'status' | 'control' | 'reload' | 'shutdown'>;
  launcher: Pick<SupervisorLauncher, 'state' | 'isStoppedByUser' | 'markStoppedByUser' | 'launchNow' | 'waitReason' | 'blockReason'>;
  eventLog: Pick<EventLog, 'getEvents'>;
  /** The install root holding supervisor\ and data\; null when it is unknown. */
  root: string | null;
  nodeLogsDir: string;
  protect?: (exePath: string, plain: string) => Promise<string>;
  playlistCache: PlaylistMetadataCache;
  ytdlpPath: string;
  updater: Updater;
  triggerRestart: () => void;
  triggerShutdown: () => void;
  getDiscord: () => DiscordNotifier;
  getTwitch: () => TwitchLivenessChecker;
  getUptime: () => number;
  apiToken: string;
  runSetupCheck?: (root: string) => Promise<SetupReport>;
  /** The Windows Startup folder; tests point it at a temp dir. */
  startupDir?: string;
  /** STREAMLOOP_TEST_HOOKS=1 only: adds POST /test/exit-75, the update restart without an update. */
  testHooks?: boolean;
}

export type StreamState = 'live' | 'recovering' | 'off-air' | 'starting' | 'stopped';

const STARTUP_FOLDER = join(
  process.env.APPDATA ?? '',
  'Microsoft', 'Windows', 'Start Menu', 'Programs', 'Startup',
);

/** UTF-16LE or UTF-8 with a BOM; else BOM-less UTF-8 as 2.x writes it, else ANSI (cp1252). */
function decodeVbs(raw: Buffer): string {
  if (raw[0] === 0xff && raw[1] === 0xfe) return raw.subarray(2).toString('utf16le');
  if (raw[0] === 0xef && raw[1] === 0xbb && raw[2] === 0xbf) return raw.subarray(3).toString('utf8');
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(raw);
  } catch {
    return raw.toString('latin1');
  }
}

const sameBat = (a: string, b: string): boolean => resolve(a).toLowerCase() === resolve(b).toLowerCase();

const LOG_NAME = /^[A-Za-z0-9_.-]+\.(log|jsonl|csv)$/;
const LOG_TAIL_BYTES = 1024 * 1024;
const GiB = 1024 ** 3;

function getSystemMemory() {
  const totalBytes = totalmem();
  const freeBytes = freemem();
  const usedBytes = totalBytes - freeBytes;
  return {
    totalGB: +(totalBytes / GiB).toFixed(1),
    freeGB: +(freeBytes / GiB).toFixed(1),
    usedGB: +(usedBytes / GiB).toFixed(1),
    usedPercent: Math.round((usedBytes / totalBytes) * 100),
  };
}

/** The at-a-glance answer to "is it working", with one plain sentence when it is not. */
export function streamState(
  sup: SupervisorStatus | null,
  launcher: LauncherState,
  stoppedByUser: boolean,
  waitReason: string | null = null,
  blockReason: string | null = null,
): { state: StreamState; reason: string | null } {
  // The old supervisor can answer for a moment after Stop, so the user's stop wins.
  if (stoppedByUser) return { state: 'stopped', reason: 'The stream was stopped from the dashboard. It stays stopped, even after a restart, until you press Start.' };
  // So does a Start that waits for that old supervisor to exit.
  if (waitReason) return { state: 'starting', reason: waitReason };
  if (!sup) {
    if (blockReason) return { state: 'off-air', reason: blockReason };
    if (launcher === 'starting') return { state: 'starting', reason: 'The supervisor is starting.' };
    if (launcher === 'missing') return { state: 'off-air', reason: 'The supervisor program was not found in the install folder.' };
    return { state: 'off-air', reason: 'The supervisor is not answering.' };
  }
  const alive = sup.encoder?.alive === true;
  switch (sup.state) {
    case 'starting':
      return { state: 'starting', reason: 'The stream is starting.' };
    case 'needs-stream-key':
      return { state: 'off-air', reason: 'The stream key is missing or cannot be read. Set it again in Settings.' };
    case 'waiting-for-playlist':
      return { state: 'off-air', reason: 'The playlist could not be loaded.' };
    case 'stopping':
      return { state: 'off-air', reason: 'The supervisor is shutting down.' };
    case 'recovering':
      return { state: 'recovering', reason: alive ? 'Recovering the current video.' : 'The encoder is restarting.' };
    case 'playing':
      if (!alive) return { state: 'off-air', reason: 'The encoder is not running.' };
      if (sup.hold) return { state: 'recovering', reason: 'Every video failed, so the slate airs until the next try.' };
      if (sup.recovery?.step && sup.recovery.step !== 'none') return { state: 'recovering', reason: 'Recovering the current video.' };
      return { state: 'live', reason: null };
    default:
      return { state: 'off-air', reason: `The supervisor reports an unknown state: ${String(sup.state)}.` };
  }
}

const SUPERVISOR_EXE_MISSING = 'The supervisor program was not found in the install folder. Reinstall StreamLoop from the full ZIP.';

function maskConfig(config: AppConfig): Record<string, unknown> {
  const { streamKeyProtected, ...supervisor } = config.supervisor;
  return {
    ...config,
    twitchClientSecret: config.twitchClientSecret ? '********' : '',
    discord: {
      ...config.discord,
      webhookUrl: config.discord.webhookUrl ? '********' : '',
    },
    supervisor: { ...supervisor, streamKeySet: streamKeyProtected.length > 0 },
  };
}

function errorText(err: unknown): string {
  if (err instanceof ZodError) return err.issues.map((i) => `${i.path.join('.')}: ${i.message}`).join('; ');
  return err instanceof Error ? err.message : String(err);
}

export function createApiRouter(deps: ApiDependencies): Router {
  const router = Router();
  const save = deps.saveConfig ?? ((patch: Partial<AppConfig>) => saveConfigFile(patch));
  const runSetupCheck = deps.runSetupCheck ?? ((root: string) => runSetupCheckDefault(root));
  let setupRun: Promise<SetupReport> | null = null;
  const protect = deps.protect ?? ((exe: string, plain: string) => protectStreamKey(exe, plain));
  const logDirs: Record<string, string | null> = {
    supervisor: deps.root ? join(deps.root, 'data', 'logs') : null,
    node: deps.nodeLogsDir,
  };

  /** Saves, takes the new config in and asks the supervisor to read it. */
  async function saveAndReload(patch: Partial<AppConfig>, reloadOpts?: { index?: number }) {
    const config = save(patch);
    deps.applyConfig(config);
    const reload = await (reloadOpts ? deps.client.reload(reloadOpts) : deps.client.reload());
    return {
      config,
      restartRequired: reload.restartRequired === true,
      reloadError: reload.ok ? null : reload.error ?? 'reload failed',
      reloadErrorCode: reload.ok ? null : reload.code ?? null,
    };
  }

  router.use(express.json());

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
    const sup = await deps.client.status();
    const launcher = deps.launcher.state();
    res.json({
      stream: streamState(sup, launcher, deps.launcher.isStoppedByUser(), deps.launcher.waitReason(), deps.launcher.blockReason()),
      supervisor: sup,
      launcher,
      twitch: deps.getTwitch().getStatus(),
      firstRun: isFirstRun(deps.getConfig()),
      systemMemory: getSystemMemory(),
      uptimeMs: deps.getUptime(),
    });
  });

  router.get('/state', async (_req, res) => {
    const p = (await deps.client.status())?.playing;
    res.json({
      index: p?.index ?? null,
      id: p?.id ?? null,
      title: p?.title ?? null,
      position: p?.position ?? null,
      duration: p?.duration ?? null,
      source: p?.source ?? null,
    });
  });

  router.get('/events', (_req, res) => {
    res.json(deps.eventLog.getEvents());
  });

  router.get('/config', (_req, res) => {
    res.json(maskConfig(deps.getConfig()));
  });

  router.post('/config', async (req, res) => {
    try {
      const body = req.body as Record<string, unknown>;
      if (body.twitchClientSecret === '********') delete body.twitchClientSecret;
      const discord = body.discord as Record<string, unknown> | undefined;
      if (discord && discord.webhookUrl === '********') delete discord.webhookUrl;
      // Only /stream-key sets the key; streamKeySet is the masked read-back.
      const supervisor = body.supervisor as Record<string, unknown> | undefined;
      if (supervisor) {
        delete supervisor.streamKeyProtected;
        delete supervisor.streamKeySet;
        // An explicit preferredEncoder is the user's pick. Without one, a changed encoder becomes the preference;
        // resending the current one (any save after a fallback) must not make the fallback the preference.
        if (typeof supervisor.preferredEncoder !== 'string' && typeof supervisor.encoder === 'string'
          && supervisor.encoder !== deps.getConfig().supervisor.encoder) {
          supervisor.preferredEncoder = supervisor.encoder;
        }
      }
      const r = await saveAndReload(body as Partial<AppConfig>);
      logger.info({ restartRequired: r.restartRequired, reloadError: r.reloadError }, 'Config updated via API');
      res.json({ ok: true, config: maskConfig(r.config), restartRequired: r.restartRequired, reloadError: r.reloadError, reloadErrorCode: r.reloadErrorCode });
    } catch (err) {
      logger.error({ err }, 'Failed to save config');
      res.status(400).json({ error: errorText(err) });
    }
  });

  router.post('/stream-key', async (req, res) => {
    const key = (req.body as { key?: unknown }).key;
    if (typeof key !== 'string' || !key.trim()) {
      return res.status(400).json({ error: 'No stream key given' });
    }
    if (!deps.root) {
      return res.status(503).json({ error: 'The supervisor program was not found, so the key cannot be encrypted.' });
    }
    const exe = join(deps.root, 'supervisor', 'streamloop-supervisor.exe');
    if (!existsSync(exe)) return res.status(503).json({ error: SUPERVISOR_EXE_MISSING });
    // protectStreamKey already redacts; this guards any other error text too.
    const redact = (s: string): string => s.split(key.trim()).join('[redacted]');
    try {
      const blob = await protect(exe, key);
      const r = await saveAndReload({ supervisor: { streamKeyProtected: blob } } as Partial<AppConfig>);
      logger.info({ reloadError: r.reloadError }, 'Stream key replaced');
      res.json({ ok: true, streamKeySet: true });
    } catch (err) {
      const why = redact(errorText(err));
      logger.error({ error: why }, 'Failed to set the stream key');
      res.status(500).json({ error: why });
    }
  });

  const autostartVbs = (): string => join(deps.startupDir ?? STARTUP_FOLDER, 'StreamLoop.vbs');
  const thisBat = (): string =>
    join(deps.root ?? join(getConfigPath().replace(/[/\\]config\.json$/, ''), '..'), 'START.bat');
  const autostartTarget = (): string | null => {
    if (!existsSync(autostartVbs())) return null;
    const m = /WshShell\.Run\s+"""(.+?)"""/i.exec(decodeVbs(readFileSync(autostartVbs())));
    return m ? m[1] : null;
  };

  router.get('/autostart', (_req, res) => {
    try {
      const target = autostartTarget();
      res.json({ enabled: existsSync(autostartVbs()), target, thisInstall: target !== null && sameBat(target, thisBat()) });
    } catch (err) {
      logger.error({ err }, 'Failed to read autostart');
      res.status(500).json({ error: errorText(err) });
    }
  });

  router.post('/autostart', (req, res) => {
    const { enabled } = req.body as { enabled: boolean };
    try {
      if (enabled) {
        const vbsContent = [
          'Set WshShell = CreateObject("WScript.Shell")',
          `WshShell.Run """${thisBat()}""", 1, False`,
        ].join('\r\n');
        // UTF-16LE with a BOM: WScript reads a BOM-less UTF-8 file as ANSI and misses non-ASCII paths.
        const tmpFile = autostartVbs() + '.tmp';
        writeFileSync(tmpFile, '\ufeff' + vbsContent, 'utf16le');
        renameSync(tmpFile, autostartVbs());
        logger.info('Autostart shortcut created');
      } else if (existsSync(autostartVbs())) {
        const target = autostartTarget();
        if (target === null) {
          return res.status(409).json({ error: 'StreamLoop.vbs in the Startup folder was not written by StreamLoop. Remove it from the Startup folder by hand.' });
        }
        if (!sameBat(target, thisBat())) {
          return res.status(409).json({ error: `Autostart points to another StreamLoop install: ${target}.` });
        }
        unlinkSync(autostartVbs());
        logger.info('Autostart shortcut removed');
      }
      res.json({ ok: true, enabled });
    } catch (err) {
      logger.error({ err }, 'Failed to set autostart');
      res.status(500).json({ error: errorText(err) });
    }
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

  router.post('/update/apply', (_req, res) => {
    // Respond immediately — the dashboard polls /update/status for progress
    try {
      deps.updater.downloadAndApply()
        .then(() => {
          setTimeout(() => deps.triggerRestart(), 1000);
        })
        .catch((err) => {
          logger.error({ err }, 'Failed to apply update');
        });
      res.json({ ok: true, message: 'Update started' });
    } catch (err) {
      logger.error({ err }, 'Failed to start update');
      res.status(500).json({ error: err instanceof Error ? err.message : String(err) });
    }
  });

  // --- Playlist endpoints ---

  router.get('/playlist/videos', async (req, res) => {
    const page = Math.max(1, Number(req.query.page) || 1);
    const perPage = Math.min(100, Math.max(1, Number(req.query.perPage) || 25));
    const playlists = deps.getConfig().playlists;
    const raw = req.query.playlist ?? '0';
    const which = typeof raw === 'string' && /^\d+$/.test(raw) ? Number(raw) : -1;
    if (which < 0 || which >= playlists.length) {
      return res.status(400).json({ error: 'Invalid playlist index' });
    }
    try {
      // The supervisor plays the first playlist; any other is only browsed.
      const [metadata, sup] = await Promise.all([
        deps.playlistCache.fetch(playlists[which].id),
        which === 0 ? deps.client.status() : null,
      ]);
      const start = (page - 1) * perPage;
      const videos = metadata.videos.slice(start, start + perPage);
      res.json({ videos, total: metadata.videos.length, page, perPage, currentIndex: sup?.playing?.index ?? null });
    } catch (err) {
      logger.error({ err }, 'Failed to fetch playlist metadata');
      res.status(500).json({ error: 'Failed to fetch playlist metadata' });
    }
  });

  router.post('/playlist/switch', async (req, res) => {
    const { playlistIndex, videoIndex } = req.body as { playlistIndex: unknown; videoIndex?: unknown };
    const playlists = [...deps.getConfig().playlists];
    if (typeof playlistIndex !== 'number' || !Number.isInteger(playlistIndex) || playlistIndex < 0 || playlistIndex >= playlists.length) {
      return res.status(400).json({ error: 'Invalid playlist index' });
    }
    if (videoIndex !== undefined && (typeof videoIndex !== 'number' || !Number.isInteger(videoIndex) || videoIndex < 0)) {
      return res.status(400).json({ error: 'Invalid video index' });
    }
    const [chosen] = playlists.splice(playlistIndex, 1);
    try {
      const r = await saveAndReload({ playlists: [chosen, ...playlists] }, videoIndex === undefined ? undefined : { index: videoIndex });
      res.json({ ok: true, reloadError: r.reloadError, reloadErrorCode: r.reloadErrorCode });
    } catch (err) {
      logger.error({ err }, 'Failed to switch playlist');
      res.status(500).json({ error: 'Failed to switch playlist' });
    }
  });

  // --- Player control endpoints ---

  async function control(res: Response, cmd: 'next' | 'prev' | 'restart' | 'jump', index?: number) {
    if (deps.launcher.isStoppedByUser()) {
      return res.status(503).json({ error: 'The stream is stopped. Start it first.' });
    }
    if (deps.launcher.state() !== 'running') {
      return res.status(503).json({ error: 'The supervisor is not running.' });
    }
    const r = await deps.client.control(cmd, index);
    if (!r.ok) return res.status(502).json({ error: `The supervisor refused: ${r.error ?? 'unknown error'}` });
    res.json({ ok: true });
  }

  router.post('/player/next', (_req, res) => control(res, 'next'));
  router.post('/player/prev', (_req, res) => control(res, 'prev'));
  router.post('/player/restart', (_req, res) => control(res, 'restart'));
  router.post('/player/jump', (req, res) => {
    const { index } = req.body as { index: unknown };
    if (typeof index !== 'number' || !Number.isInteger(index) || index < 0) {
      return res.status(400).json({ error: 'Invalid video index' });
    }
    return control(res, 'jump', index);
  });

  // --- Stream endpoints ---

  router.post('/stream/stop', async (_req, res) => {
    // Mark first, so the launcher does not relaunch while the supervisor shuts down.
    deps.launcher.markStoppedByUser();
    const answered = await deps.client.shutdown();
    logger.info({ answered }, 'Stream stopped from the dashboard');
    res.json({ ok: true });
  });

  router.post('/stream/start', (_req, res) => {
    deps.launcher.launchNow();
    logger.info('Stream started from the dashboard');
    res.json({ ok: true });
  });

  // --- Setup check ---

  const NO_ROOT = 'The install folder is not known, so the setup check cannot run.';

  router.get('/setup-check', (_req, res) => {
    if (!deps.root) return res.status(503).json({ error: NO_ROOT });
    res.json(readSetupReport(deps.root));
  });

  // Recommendations are not applied here; the dashboard saves them through POST /config.
  router.post('/setup-check/run', async (_req, res) => {
    const root = deps.root;
    if (!root) return res.status(503).json({ error: NO_ROOT });
    // A second click while the probes run gets the same result instead of a second set of probes.
    setupRun ??= runSetupCheck(root).finally(() => { setupRun = null; });
    try {
      res.json(await setupRun);
    } catch (err) {
      logger.error({ err }, 'Setup check failed');
      res.status(500).json({ error: errorText(err) });
    }
  });

  // --- Log endpoints ---

  router.get('/logs', (_req, res) => {
    const files = Object.entries(logDirs).flatMap(([source, dir]) => {
      if (!dir || !existsSync(dir)) return [];
      return readdirSync(dir).flatMap((name) => {
        if (!LOG_NAME.test(name)) return [];
        const stat = statSync(join(dir, name));
        return stat.isFile() ? [{ name, size: stat.size, mtime: stat.mtime.toISOString(), source }] : [];
      });
    });
    res.json(files.sort((a, b) => b.mtime.localeCompare(a.mtime)));
  });

  router.get('/logs/:source/:name', (req, res) => {
    const { source, name } = req.params;
    if (!Object.hasOwn(logDirs, source)) return res.status(400).json({ error: 'Unknown log source' });
    if (!LOG_NAME.test(name) || basename(name) !== name) return res.status(400).json({ error: 'Invalid log file name' });
    const dir = logDirs[source];
    const full = dir ? join(dir, name) : null;
    if (!full || !existsSync(full)) return res.status(404).json({ error: 'Log file not found' });
    try {
      const stat = statSync(full);
      // A folder, or a device name like con.log, passes the name check and can block a read.
      if (!stat.isFile()) return res.status(404).json({ error: 'Log file not found' });
      if (stat.size <= LOG_TAIL_BYTES) {
        res.type('text/plain').send(readFileSync(full, 'utf-8'));
      } else {
        const fd = openSync(full, 'r');
        try {
          const buf = Buffer.alloc(LOG_TAIL_BYTES);
          readSync(fd, buf, 0, LOG_TAIL_BYTES, stat.size - LOG_TAIL_BYTES);
          res.type('text/plain').send('... (truncated, showing last 1 MB) ...\n' + buf.toString('utf-8'));
        } finally {
          closeSync(fd);
        }
      }
    } catch (err) {
      res.status(500).json({ error: 'Failed to read log file' });
    }
  });

  // --- yt-dlp endpoints ---

  router.post('/yt-dlp/update', async (_req, res) => {
    const result = await updateYtdlp(deps.ytdlpPath);
    if (result.ok) {
      // A newer yt-dlp may resolve more playlist entries than the old one did,
      // so drop the cached metadata to force a fresh fetch on next request.
      deps.playlistCache.clear();
      logger.info({ version: result.version }, 'yt-dlp updated');
      res.json({ ok: true, version: result.version });
    } else {
      logger.error({ error: result.error }, 'Failed to update yt-dlp');
      res.status(500).json({ error: result.error ?? 'Failed to update yt-dlp' });
    }
  });

  if (deps.testHooks) {
    router.post('/test/exit-75', (_req, res) => {
      logger.warn('Test hook: exiting with code 75 as after an update');
      res.json({ ok: true });
      deps.triggerRestart();
    });
  }

  // --- Shutdown endpoint ---

  router.post('/shutdown', (_req, res) => {
    res.json({ ok: true, message: 'Shutting down...' });
    setTimeout(() => deps.triggerShutdown(), 500);
  });

  // Never echo the body or err.message: a JSON parse error quotes the body, which may hold the stream key.
  router.use((err: { type?: unknown; status?: unknown }, _req: Request, res: Response, next: NextFunction) => {
    if (res.headersSent) return next(err);
    const parse = err?.type === 'entity.parse.failed';
    const status = parse ? 400 : 500;
    logger.warn({ type: typeof err?.type === 'string' ? err.type : null, status }, 'API request failed');
    res.status(status).json({ error: parse ? 'Invalid JSON' : 'Internal error' });
  });

  return router;
}
