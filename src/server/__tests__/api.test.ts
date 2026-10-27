import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import express from 'express';
import type { Server } from 'http';
import type { AddressInfo } from 'net';
import { mkdirSync, rmSync, writeFileSync, utimesSync, readFileSync, existsSync } from 'fs';
import { join } from 'path';
import { createApiRouter, type ApiDependencies } from '../api.js';
import { logger } from '../logger.js';
import type { AppConfig } from '../types.js';
import type { SupervisorStatus } from '../supervisor-client.js';
import type { LauncherState } from '../supervisor-launcher.js';

const TOKEN = 'test-token';
const KEY = 'live_TESTKEY';
const BLOB = 'AQAAANCMnd8BFdERjHoAwE_fake_blob';

function makeConfig(): AppConfig {
  return {
    port: 7654,
    playlists: [{ id: 'PLaaa', name: 'A' }, { id: 'PLbbb', name: 'B' }, { id: 'PLccc', name: 'C' }],
    discord: {
      webhookUrl: 'https://discord.example/hook', botName: '', avatarUrl: '', rolePing: '',
      events: { error: true, skip: true, recovery: true, critical: true, resume: true, streamDrop: true, streamRestart: true, outputFrozen: true, twitchMismatch: true, twitchRestart: true },
      templates: { error: '', skip: '', recovery: '', critical: '', resume: '', streamDrop: '', streamRestart: '', outputFrozen: '', twitchMismatch: '', twitchRestart: '' },
    },
    supervisor: {
      ingestUrl: 'rtmp://live.twitch.tv/app', streamKeyProtected: BLOB, encoder: 'h264_nvenc', cacheBudgetMb: 300,
      readAheadSeconds: 20, playerClient: 'mweb,web_embedded', overlay: true, freezeSeconds: 20, silenceSeconds: 30,
    },
    autoUpdateCheck: true, updateCheckIntervalMs: 21600000, autoUpdateYtdlp: true, autoOpenAdmin: true,
    twitchClientId: 'id', twitchClientSecret: 'secret', twitchChannel: 'chan', twitchLivenessEnabled: false,
    twitchPollIntervalMs: 60000, ytdlCookiesFromBrowser: '',
  };
}

const tmp = join(process.cwd(), '.test-tmp', 'api');

interface Harness {
  deps: ApiDependencies;
  sup: { value: SupervisorStatus | null };
  launcherState: { value: LauncherState; stopped: boolean };
  client: { status: ReturnType<typeof vi.fn>; control: ReturnType<typeof vi.fn>; reload: ReturnType<typeof vi.fn>; shutdown: ReturnType<typeof vi.fn> };
  launcher: { state: ReturnType<typeof vi.fn>; isStoppedByUser: ReturnType<typeof vi.fn>; markStoppedByUser: ReturnType<typeof vi.fn>; launchNow: ReturnType<typeof vi.fn>; waitReason: ReturnType<typeof vi.fn>; blockReason: ReturnType<typeof vi.fn> };
  runSetupCheck: ReturnType<typeof vi.fn>;
  save: ReturnType<typeof vi.fn>;
  applyConfig: ReturnType<typeof vi.fn>;
  protect: ReturnType<typeof vi.fn>;
  config: { value: AppConfig };
}

function harness(): Harness {
  const sup = { value: null as SupervisorStatus | null };
  const launcherState = { value: 'running' as LauncherState, stopped: false };
  const config = { value: makeConfig() };
  const client = {
    status: vi.fn(async () => sup.value),
    control: vi.fn(async () => ({ ok: true })),
    reload: vi.fn(async () => ({ ok: true, restartRequired: false })),
    shutdown: vi.fn(async () => true),
  };
  const launcher = {
    state: vi.fn(() => launcherState.value),
    isStoppedByUser: vi.fn(() => launcherState.stopped),
    markStoppedByUser: vi.fn(() => { launcherState.stopped = true; }),
    launchNow: vi.fn(() => { launcherState.stopped = false; }),
    waitReason: vi.fn((): string | null => null),
    blockReason: vi.fn((): string | null => null),
  };
  const runSetupCheck = vi.fn(async () => ({ at: 'now', encoder: { chosen: 'h264_nvenc', tried: ['h264_nvenc'], software: false }, cache: { freeMb: 1, recommendedMb: 50, warning: null }, dependencies: [] }));
  const save = vi.fn((patch: Partial<AppConfig>) => {
    config.value = { ...config.value, ...patch, supervisor: { ...config.value.supervisor, ...(patch.supervisor ?? {}) } };
    return config.value;
  });
  const applyConfig = vi.fn((c: AppConfig) => { config.value = c; });
  const protect = vi.fn(async () => BLOB);
  const deps = {
    getConfig: () => config.value,
    applyConfig,
    saveConfig: save,
    client,
    launcher,
    eventLog: { getEvents: () => [{ timestamp: 't', message: 'm' }] },
    root: join(tmp, 'root'),
    nodeLogsDir: join(tmp, 'app-logs'),
    protect,
    playlistCache: { fetch: vi.fn(async () => ({ videos: [{ id: 'v1' }, { id: 'v2' }] })), clear: vi.fn() },
    ytdlpPath: 'yt-dlp.exe',
    updater: {},
    triggerRestart: vi.fn(),
    triggerShutdown: vi.fn(),
    getDiscord: () => ({}),
    getTwitch: () => ({ getStatus: () => ({ enabled: false }) }),
    getUptime: () => 1234,
    apiToken: TOKEN,
    runSetupCheck,
  } as unknown as ApiDependencies;
  return { deps, sup, launcherState, client, launcher, save, applyConfig, protect, config, runSetupCheck };
}

let server: Server;
let base = '';

async function listen(deps: ApiDependencies): Promise<void> {
  const app = express();
  app.use('/api', createApiRouter(deps));
  server = app.listen(0, '127.0.0.1');
  await new Promise((r) => server.once('listening', r));
  base = `http://127.0.0.1:${(server.address() as AddressInfo).port}/api`;
}

const get = (path: string) => fetch(base + path);
const post = (path: string, body: unknown = {}) =>
  fetch(base + path, { method: 'POST', headers: { 'content-type': 'application/json', 'x-api-token': TOKEN }, body: JSON.stringify(body) });

const playing = (extra: Partial<SupervisorStatus> & Record<string, unknown> = {}): SupervisorStatus =>
  ({ state: 'playing', reason: null, encoder: { alive: true }, recovery: { step: 'none' }, hold: null, ...extra }) as SupervisorStatus;

describe('API on the supervisor', () => {
  let h: Harness;

  beforeEach(async () => {
    rmSync(tmp, { recursive: true, force: true });
    h = harness();
    await listen(h.deps);
  });

  afterEach(async () => {
    vi.restoreAllMocks();
    server.closeAllConnections(); // fetch keeps connections alive, which would hold close() open
    await new Promise((r) => server.close(r));
    rmSync(tmp, { recursive: true, force: true });
  });

  describe('autostart handover', () => {
    const MINE = String.raw`C:\Users\Emma Ö\StreamLoop 3`;
    const OLD = String.raw`D:\old\START.bat`;
    const startup = join(tmp, 'startup');
    const vbsPath = join(startup, 'StreamLoop.vbs');
    const vbsFor = (bat: string) => `Set WshShell = CreateObject("WScript.Shell")
WshShell.Run """${bat}""", 1, False`;

    beforeEach(() => {
      mkdirSync(startup, { recursive: true });
      h.deps.startupDir = startup;
      h.deps.root = MINE;
    });

    it('writes a UTF-16LE VBS with a BOM that targets this install START.bat', async () => {
      const r = await post('/autostart', { enabled: true });
      expect(r.status).toBe(200);
      const raw = readFileSync(vbsPath);
      expect([raw[0], raw[1]]).toEqual([0xff, 0xfe]);
      const text = raw.subarray(2).toString('utf16le');
      expect(text).toContain(`WshShell.Run """${MINE}\\START.bat""", 1, False`);
      const g = await (await get('/autostart')).json();
      expect(g).toEqual({ enabled: true, target: `${MINE}\\START.bat`, thisInstall: true });
    });

    it('reads a 2.x UTF-8 VBS without a BOM as another install', async () => {
      const other = String.raw`C:\Users\Emma Ö\StreamLoop 2\START.bat`;
      writeFileSync(vbsPath, Buffer.from(vbsFor(other), 'utf8'));
      const g = await (await get('/autostart')).json();
      expect(g).toEqual({ enabled: true, target: other, thisInstall: false });
    });

    it('reads true cp1252 bytes through the ANSI fallback', async () => {
      const other = String.raw`C:UsersEmma ÖStreamLoop 2START.bat`;
      writeFileSync(vbsPath, Buffer.from(vbsFor(other), 'latin1'));
      const g = await (await get('/autostart')).json();
      expect(g.target).toBe(other);
    });

    it('leaves no temp file behind after enabling', async () => {
      await post('/autostart', { enabled: true });
      expect(existsSync(vbsPath + '.tmp')).toBe(false);
      expect(existsSync(vbsPath)).toBe(true);
    });

    it('disabling a VBS without a readable target answers 409 with its own reason', async () => {
      writeFileSync(vbsPath, 'MsgBox "hi"');
      const r = await post('/autostart', { enabled: false });
      expect(r.status).toBe(409);
      expect((await r.json()).error).toBe('StreamLoop.vbs in the Startup folder was not written by StreamLoop. Remove it from the Startup folder by hand.');
      expect(existsSync(vbsPath)).toBe(true);
    });

    it('answers 500 JSON when the VBS cannot be read', async () => {
      mkdirSync(vbsPath);
      const r = await get('/autostart');
      expect(r.status).toBe(500);
      expect(r.headers.get('content-type')).toMatch(/json/);
      expect(typeof (await r.json()).error).toBe('string');
    });

    it('reads a UTF-8 VBS with a BOM and ignores path case', async () => {
      writeFileSync(vbsPath, Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from(vbsFor(`${MINE.toLowerCase()}\\start.bat`), 'utf8')]));
      const g = await (await get('/autostart')).json();
      expect(g.thisInstall).toBe(true);
    });

    it('reports disabled when there is no VBS', async () => {
      expect(await (await get('/autostart')).json()).toEqual({ enabled: false, target: null, thisInstall: false });
    });

    it('enabling replaces another install VBS', async () => {
      writeFileSync(vbsPath, vbsFor(OLD));
      await post('/autostart', { enabled: true });
      const g = await (await get('/autostart')).json();
      expect(g.thisInstall).toBe(true);
    });

    it('disabling removes this install VBS', async () => {
      await post('/autostart', { enabled: true });
      const r = await post('/autostart', { enabled: false });
      expect(r.status).toBe(200);
      expect(existsSync(vbsPath)).toBe(false);
    });

    it('disabling leaves another install VBS and answers 409', async () => {
      writeFileSync(vbsPath, vbsFor(OLD));
      const r = await post('/autostart', { enabled: false });
      expect(r.status).toBe(409);
      expect((await r.json()).error).toBe(`Autostart points to another StreamLoop install: ${OLD}.`);
      expect(existsSync(vbsPath)).toBe(true);
    });
  });

  describe('GET /status stream.state', () => {
    it.each<[string, SupervisorStatus | null, LauncherState, string]>([
      ['live while the encoder is up and nothing recovers', playing(), 'running', 'live'],
      ['recovering during a retry', playing({ state: 'recovering', recovery: { step: 'retry' } } as never), 'running', 'recovering'],
      ['recovering while the encoder restarts', playing({ state: 'recovering', encoder: { alive: false } }), 'running', 'recovering'],
      ['recovering during a hold on the slate', playing({ hold: { reason: 'every video failed', level: 1 } }), 'running', 'recovering'],
      ['starting while the supervisor starts', playing({ state: 'starting', encoder: { alive: false } }), 'running', 'starting'],
      ['off-air without a stream key', playing({ state: 'needs-stream-key', encoder: { alive: false } }), 'running', 'off-air'],
      ['off-air without a playlist', playing({ state: 'waiting-for-playlist', encoder: { alive: false } }), 'running', 'off-air'],
      ['off-air while the supervisor stops on its own', playing({ state: 'stopping' }), 'running', 'off-air'],
      ['off-air when playing with the encoder down', playing({ encoder: { alive: false } }), 'running', 'off-air'],
      ['off-air when the supervisor does not answer', null, 'down', 'off-air'],
      ['off-air when the supervisor is missing', null, 'missing', 'off-air'],
      ['starting while the launcher starts it', null, 'starting', 'starting'],
    ])('is %s', async (_name, status, launcher, expected) => {
      // Arrange
      h.sup.value = status;
      h.launcherState.value = launcher;

      // Act
      const body = await (await get('/status')).json();

      // Assert
      expect(body.stream.state).toBe(expected);
      if (expected === 'live') expect(body.stream.reason).toBeNull();
      else expect(typeof body.stream.reason).toBe('string');
      expect(body.launcher).toBe(launcher);
      expect(body.supervisor).toEqual(status);
    });

    it('is stopped after Stop stream even while the old supervisor still answers', async () => {
      h.sup.value = playing();
      h.launcherState.value = 'running';
      h.launcherState.stopped = true;

      const body = await (await get('/status')).json();

      expect(body.stream.state).toBe('stopped');
      expect(body.stream.reason).toMatch(/stopped from the dashboard/i);
    expect(body.stream.reason).toMatch(/until you press Start/i);
    });

    it('is starting with the launcher reason while a Start waits for the stopped supervisor', async () => {
      h.sup.value = playing({ state: 'stopping' });
      h.launcherState.value = 'starting';
      h.launcher.waitReason.mockReturnValue('Waiting for the stopped supervisor to exit before starting it again.');

      const body = await (await get('/status')).json();

      expect(body.stream).toEqual({ state: 'starting', reason: 'Waiting for the stopped supervisor to exit before starting it again.' });
    });

    it('is off air with the launcher reason while the launch is held', async () => {
      h.launcherState.value = 'starting';
      h.launcher.blockReason.mockReturnValue('OBS is running.');

      const body = await (await get('/status')).json();

      expect(body.stream).toEqual({ state: 'off-air', reason: 'OBS is running.' });
    });

    it('keeps firstRun, systemMemory, uptimeMs and twitch', async () => {
      const body = await (await get('/status')).json();

      expect(body.firstRun).toBe(false);
      expect(body.uptimeMs).toBe(1234);
      expect(body.systemMemory.totalGB).toBeGreaterThan(0);
      expect(body.twitch).toEqual({ enabled: false });
    });
  });

  describe('GET /state', () => {
    it('returns now playing from the supervisor', async () => {
      h.sup.value = playing({ playing: { index: 3, id: 'abc', title: 'T', position: 12.5, duration: 600, source: 'file' } });

      const body = await (await get('/state')).json();

      expect(body).toEqual({ index: 3, id: 'abc', title: 'T', position: 12.5, duration: 600, source: 'file' });
    });

    it('returns nulls when the supervisor is unreachable', async () => {
      const body = await (await get('/state')).json();

      expect(body).toEqual({ index: null, id: null, title: null, position: null, duration: null, source: null });
    });
  });

  it('GET /events returns the event log', async () => {
    expect(await (await get('/events')).json()).toEqual([{ timestamp: 't', message: 'm' }]);
  });

  describe('POST /stream-key', () => {
    const exe = join(tmp, 'root', 'supervisor', 'streamloop-supervisor.exe');
    beforeEach(() => {
      mkdirSync(join(tmp, 'root', 'supervisor'), { recursive: true });
      writeFileSync(exe, '');
    });

    function spyLogs() {
      return (['trace', 'debug', 'info', 'warn', 'error', 'fatal'] as const).map((l) => vi.spyOn(logger, l));
    }

    it('stores only the blob and never echoes or logs the key', async () => {
      // Arrange
      const spies = spyLogs();

      // Act
      const res = await post('/stream-key', { key: KEY });
      const text = await res.text();

      // Assert
      expect(res.status).toBe(200);
      expect(JSON.parse(text)).toEqual({ ok: true, streamKeySet: true });
      expect(text).not.toContain(KEY);
      expect(h.protect).toHaveBeenCalledWith(join(tmp, 'root', 'supervisor', 'streamloop-supervisor.exe'), KEY);
      expect(h.save).toHaveBeenCalledWith({ supervisor: { streamKeyProtected: BLOB } });
      expect(JSON.stringify(h.save.mock.calls)).not.toContain(KEY);
      expect(h.client.reload).toHaveBeenCalledOnce();
      expect(JSON.stringify(spies.flatMap((s) => s.mock.calls))).not.toContain(KEY);
    });

    it('keeps the key out of the error when protect fails', async () => {
      const spies = spyLogs();
      h.protect.mockRejectedValueOnce(new Error(`protect failed: bad input ${KEY}`));

      const res = await post('/stream-key', { key: KEY });
      const text = await res.text();

      expect(res.status).toBe(500);
      expect(text).not.toContain(KEY);
      expect(h.save).not.toHaveBeenCalled();
      expect(JSON.stringify(spies.flatMap((s) => s.mock.calls))).not.toContain(KEY);
    });

    it('answers malformed JSON without echoing or logging the key', async () => {
      const spies = spyLogs();

      const res = await fetch(base + '/stream-key', {
        method: 'POST', headers: { 'content-type': 'application/json', 'x-api-token': TOKEN }, body: `{"key":${KEY}}`,
      });
      const text = await res.text();

      expect(res.status).toBe(400);
      expect(JSON.parse(text)).toEqual({ error: 'Invalid JSON' });
      expect(text).not.toContain('TESTKEY');
      expect(JSON.stringify(spies.flatMap((s) => s.mock.calls))).not.toContain('TESTKEY');
      expect(h.protect).not.toHaveBeenCalled();
    });

    it('answers 503 with only the plain sentence when the supervisor exe is missing', async () => {
      rmSync(exe);

      const res = await post('/stream-key', { key: KEY });

      expect(res.status).toBe(503);
      expect(await res.json()).toEqual({ error: 'The supervisor program was not found in the install folder. Reinstall StreamLoop from the full ZIP.' });
      expect(h.protect).not.toHaveBeenCalled();
    });

    it('rejects an empty key', async () => {
      const res = await post('/stream-key', { key: '  ' });

      expect(res.status).toBe(400);
      expect(h.protect).not.toHaveBeenCalled();
    });
  });

  describe('config', () => {
    it.each([
      ['longer than 200 characters', 'x'.repeat(201)],
      ['a control character', 'fire\nfox'],
    ])('POST /config refuses a cookie browser value with %s and saves nothing', async (why, value) => {
      const res = await post('/config', { ytdlCookiesFromBrowser: value });
      const body = await res.json();

      expect(res.status).toBe(400);
      expect(body.error).toContain(why);
      expect(h.save).not.toHaveBeenCalled();
      expect(h.client.reload).not.toHaveBeenCalled();
    });

    it('GET /config reports whether a key is set and never the blob', async () => {
      const text = await (await get('/config')).text();
      const body = JSON.parse(text);

      expect(text).not.toContain(BLOB);
      expect(body.supervisor.streamKeySet).toBe(true);
      expect(body.supervisor.streamKeyProtected).toBeUndefined();
      expect(body.twitchClientSecret).toBe('********');
      expect(body.discord.webhookUrl).toBe('********');
    });

    it('POST /config ignores a stream key in the body, applies and reloads', async () => {
      h.client.reload.mockResolvedValueOnce({ ok: true, restartRequired: true });

      const res = await post('/config', { supervisor: { encoder: 'h264_amf', streamKeyProtected: 'other', streamKeySet: false } });
      const body = await res.json();

      expect(res.status).toBe(200);
      expect(h.save).toHaveBeenCalledWith({ supervisor: { encoder: 'h264_amf', preferredEncoder: 'h264_amf' } });
      expect(h.applyConfig).toHaveBeenCalledOnce();
      expect(h.client.reload).toHaveBeenCalledOnce();
      expect(body).toMatchObject({ ok: true, restartRequired: true, reloadError: null });
      expect(JSON.stringify(body)).not.toContain(BLOB);
    });

    it('POST /config reports a failed reload', async () => {
      h.client.reload.mockResolvedValueOnce({ ok: false, error: 'fetch failed', code: 'ECONNREFUSED' });

      const body = await (await post('/config', { twitchChannel: 'x' })).json();

      expect(body).toMatchObject({ ok: true, restartRequired: false, reloadError: 'fetch failed', reloadErrorCode: 'ECONNREFUSED' });
    });

    it('POST /config gives a null reloadErrorCode when the supervisor answered with an error', async () => {
      h.client.reload.mockResolvedValueOnce({ ok: false, error: 'bad config' });

      const body = await (await post('/config', { twitchChannel: 'x' })).json();

      expect(body).toMatchObject({ reloadError: 'bad config', reloadErrorCode: null });
    });
  });

  describe('logs', () => {
    beforeEach(() => {
      const sup = join(tmp, 'root', 'data', 'logs');
      const node = join(tmp, 'app-logs');
      mkdirSync(sup, { recursive: true });
      mkdirSync(node, { recursive: true });
      writeFileSync(join(sup, 'events-2026-10-25.jsonl'), '{"kind":"x"}\n');
      writeFileSync(join(node, 'streamloop.log'), 'node line\n');
      writeFileSync(join(sup, 'secret.txt'), 'nope');
      utimesSync(join(sup, 'events-2026-10-25.jsonl'), new Date(2000), new Date(2000_000));
      utimesSync(join(node, 'streamloop.log'), new Date(3000), new Date(3000_000));
    });

    it('GET /logs lists both sources newest first', async () => {
      const body = await (await get('/logs')).json();

      expect(body.map((f: { name: string; source: string }) => [f.name, f.source])).toEqual([
        ['streamloop.log', 'node'],
        ['events-2026-10-25.jsonl', 'supervisor'],
      ]);
      expect(body[0].size).toBe(10);
    });

    it('GET /logs/:source/:name returns the file', async () => {
      const res = await get('/logs/supervisor/events-2026-10-25.jsonl');

      expect(res.status).toBe(200);
      expect(await res.text()).toBe('{"kind":"x"}\n');
    });

    it('answers 404 for a folder whose name passes the check', async () => {
      mkdirSync(join(tmp, 'app-logs', 'con.log'));

      const res = await get('/logs/node/con.log');

      expect(res.status).toBe(404);
    });

    it.each([
      ['a backslash traversal', '/logs/supervisor/..%5Cx.log'],
      ['a slash traversal', '/logs/supervisor/..%2F..%2Fx.log'],
      ['an absolute path', '/logs/node/C%3A%5Cx.log'],
      ['another extension', '/logs/supervisor/secret.txt'],
      ['an unknown source', '/logs/other/streamloop.log'],
    ])('rejects %s', async (_name, path) => {
      expect((await get(path)).status).toBe(400);
    });
  });

  describe('player controls', () => {
    it.each(['next', 'prev', 'restart'])('POST /player/%s returns 503 while the supervisor is down', async (cmd) => {
      h.launcherState.value = 'down';

      const res = await post(`/player/${cmd}`);

      expect(res.status).toBe(503);
      expect(typeof (await res.json()).error).toBe('string');
      expect(h.client.control).not.toHaveBeenCalled();
      expect(h.runSetupCheck).not.toHaveBeenCalled();
    });

    it('POST /player/jump returns 503 after Stop stream', async () => {
      h.launcherState.stopped = true;

      expect((await post('/player/jump', { index: 2 })).status).toBe(503);
    });

    it('POST /player/jump sends the index', async () => {
      const res = await post('/player/jump', { index: 2 });

      expect(res.status).toBe(200);
      expect(h.client.control).toHaveBeenCalledWith('jump', 2, undefined, undefined);
    });

    it('POST /player/jump sends the video id with the index', async () => {
      const res = await post('/player/jump', { index: 2, id: 'dQw4w9WgXc_-' });

      expect(res.status).toBe(200);
      expect(h.client.control).toHaveBeenCalledWith('jump', 2, undefined, 'dQw4w9WgXc_-');
    });

    it.each([1_000_000, -1])('POST /player/jump rejects index %j', async (index) => {
      expect((await post('/player/jump', { index })).status).toBe(400);
      expect(h.client.control).not.toHaveBeenCalled();
    });

    it.each(['', 'a b', 'x'.repeat(65), 7])('POST /player/jump rejects id %j', async (id) => {
      expect((await post('/player/jump', { index: 1, id })).status).toBe(400);
      expect(h.client.control).not.toHaveBeenCalled();
    });

    it('POST /player/next passes on a refusal as a reason', async () => {
      h.client.control.mockResolvedValueOnce({ ok: false, error: 'stopping' });

      const res = await post('/player/next');

      expect(res.status).toBe(502);
      expect((await res.json()).error).toContain('stopping');
    });
  });

  describe('stream stop and start', () => {
    it('POST /stream/stop marks the user stop and shuts the supervisor down', async () => {
      const res = await post('/stream/stop');

      expect(res.status).toBe(200);
      expect(h.launcher.markStoppedByUser).toHaveBeenCalledOnce();
      expect(h.client.shutdown).toHaveBeenCalledOnce();
    });

    it('POST /stream/start launches now', async () => {
      await post('/stream/start');

      expect(h.launcher.launchNow).toHaveBeenCalledOnce();
    });
  });

  describe('playlists', () => {
    it('POST /playlist/switch moves the playlist to the front and reloads', async () => {
      const res = await post('/playlist/switch', { playlistIndex: 2 });

      expect(res.status).toBe(200);
      expect(h.save).toHaveBeenCalledWith({ playlists: [{ id: 'PLccc', name: 'C' }, { id: 'PLaaa', name: 'A' }, { id: 'PLbbb', name: 'B' }] });
      expect(h.applyConfig).toHaveBeenCalledOnce();
      expect(h.client.reload).toHaveBeenCalledOnce();
    });

    it('POST /playlist/switch rejects a bad index', async () => {
      expect((await post('/playlist/switch', { playlistIndex: 3 })).status).toBe(400);
      expect(h.save).not.toHaveBeenCalled();
    });

    it('POST /playlist/switch passes the start video to the reload', async () => {
      const res = await post('/playlist/switch', { playlistIndex: 1, videoIndex: 4, videoId: 'abc_DEF-123' });

      expect(res.status).toBe(200);
      expect(h.save).toHaveBeenCalledWith({ playlists: [{ id: 'PLbbb', name: 'B' }, { id: 'PLaaa', name: 'A' }, { id: 'PLccc', name: 'C' }] });
      expect(h.client.reload).toHaveBeenCalledWith({ index: 4, id: 'abc_DEF-123' });
    });

    it.each(['', 'a/b', 'x'.repeat(65), 7, null])('POST /playlist/switch rejects videoId %j before saving', async (videoId) => {
      expect((await post('/playlist/switch', { playlistIndex: 1, videoIndex: 0, videoId })).status).toBe(400);
      expect(h.save).not.toHaveBeenCalled();
    });

    it.each([-1, 1.5, '2', null, 1_000_000])('POST /playlist/switch rejects videoIndex %j', async (videoIndex) => {
      expect((await post('/playlist/switch', { playlistIndex: 1, videoIndex })).status).toBe(400);
      expect(h.save).not.toHaveBeenCalled();
      expect(h.client.reload).not.toHaveBeenCalled();
    });

    it('GET /playlist/videos browses another playlist through the cache, with no current video', async () => {
      h.sup.value = playing({ playing: { index: 1 } });

      const body = await (await get('/playlist/videos?playlist=2&page=1&perPage=1')).json();

      expect(h.deps.playlistCache.fetch).toHaveBeenCalledWith('PLccc');
      expect(body).toEqual({ videos: [{ id: 'v1' }], total: 2, page: 1, perPage: 1, currentIndex: null });
    });

    it.each(['3', '-1', 'x', '1.5'])('GET /playlist/videos rejects playlist=%s', async (playlist) => {
      const res = await get('/playlist/videos?playlist=' + playlist);

      expect(res.status).toBe(400);
      expect(h.deps.playlistCache.fetch).not.toHaveBeenCalled();
    });

    it('GET /playlist/videos takes currentIndex from the supervisor', async () => {
      h.sup.value = playing({ playing: { index: 1 } });

      const body = await (await get('/playlist/videos?page=1&perPage=1')).json();

      expect(h.deps.playlistCache.fetch).toHaveBeenCalledWith('PLaaa');
      expect(body).toEqual({ videos: [{ id: 'v1' }], total: 2, page: 1, perPage: 1, currentIndex: 1 });
    });
  });

  describe('encoder preference', () => {
    it('POST /config that changes the encoder also sets preferredEncoder', async () => {
      await post('/config', { supervisor: { encoder: 'h264_amf' } });
      expect(h.save.mock.calls[0][0].supervisor).toEqual({ encoder: 'h264_amf', preferredEncoder: 'h264_amf' });
    });

    it('POST /config takes an explicit preferredEncoder, even for the encoder running now', async () => {
      h.config.value = { ...h.config.value, supervisor: { ...h.config.value.supervisor, encoder: 'libx264', preferredEncoder: 'h264_nvenc' } };
      await post('/config', { supervisor: { encoder: 'libx264', preferredEncoder: 'libx264' } });
      expect(h.save.mock.calls[0][0].supervisor).toEqual({ encoder: 'libx264', preferredEncoder: 'libx264' });
    });

    it('POST /config keeps an explicit preferredEncoder over the changed-encoder rule', async () => {
      await post('/config', { supervisor: { encoder: 'h264_amf', preferredEncoder: 'h264_qsv' } });
      expect(h.save.mock.calls[0][0].supervisor).toEqual({ encoder: 'h264_amf', preferredEncoder: 'h264_qsv' });
    });

    it('POST /config that resends the current encoder leaves the preference alone', async () => {
      await post('/config', { supervisor: { encoder: 'h264_nvenc', overlay: false } });
      expect(h.save.mock.calls[0][0].supervisor).toEqual({ encoder: 'h264_nvenc', overlay: false });
    });
  });

  describe('setup check', () => {
    it('GET /setup-check is null before the first run', async () => {
      expect(await (await get('/setup-check')).json()).toBeNull();
    });

    it('POST /setup-check/run runs the check on the install root, then GET returns the stored report', async () => {
      const report = await (await post('/setup-check/run')).json();
      mkdirSync(join(tmp, 'root', 'data'), { recursive: true });
      writeFileSync(join(tmp, 'root', 'data', 'setup-check.json'), JSON.stringify(report));

      expect(h.runSetupCheck).toHaveBeenCalledWith(join(tmp, 'root'), { durationsS: [] });
      expect(report.encoder.chosen).toBe('h264_nvenc');
      expect(await (await get('/setup-check')).json()).toEqual(report);
    });

    it("POST /setup-check/run sizes the cache from the playing playlist's durations", async () => {
      vi.mocked(h.deps.playlistCache.fetch).mockResolvedValueOnce({ playlistId: 'PLx', fetchedAt: 0, videos: [{ index: 0, id: 'a', title: 'a', duration: 100 }, { index: 1, id: 'b', title: 'b', duration: 30_474 }, { index: 2, id: 'c', title: 'c', duration: 26_953 }] });

      await post('/setup-check/run');

      expect(h.deps.playlistCache.fetch).toHaveBeenCalledWith(h.deps.getConfig().playlists[0].id);
      expect(h.runSetupCheck).toHaveBeenCalledWith(join(tmp, 'root'), { durationsS: [100, 30_474, 26_953] });
    });

    it("POST /setup-check/run uses the wizard's playlist before it is saved", async () => {
      await post('/setup-check/run', { playlistId: 'PLwizardPlaylist01' });

      expect(h.deps.playlistCache.fetch).toHaveBeenCalledWith('PLwizardPlaylist01');
    });

    it('POST /setup-check/run refuses a playlist id that is not one', async () => {
      const r = await post('/setup-check/run', { playlistId: '../x' });

      expect(r.status).toBe(400);
      expect(h.runSetupCheck).not.toHaveBeenCalled();
    });

    it('POST /setup-check/run still runs when the playlist cannot be read', async () => {
      vi.mocked(h.deps.playlistCache.fetch).mockRejectedValueOnce(new Error('bot check'));

      const r = await post('/setup-check/run');

      expect(r.status).toBe(200);
      expect(h.runSetupCheck).toHaveBeenCalledWith(join(tmp, 'root'), { durationsS: [] });
    });

    it('answers 503 with a reason when there is no install root', async () => {
      server.closeAllConnections();
      await new Promise((r) => server.close(r));
      await listen({ ...h.deps, root: null });

      const [g, p] = await Promise.all([get('/setup-check'), post('/setup-check/run')]);

      expect([g.status, p.status]).toEqual([503, 503]);
      expect((await p.json()).error).toMatch(/install folder/);
      expect(h.runSetupCheck).not.toHaveBeenCalled();
    });
  });

  it.each(['/stream/stop', '/stream/start', '/stream-key', '/playlist/switch', '/player/next', '/config', '/setup-check/run'])(
    'refuses POST %s without the API token', async (path) => {
      const res = await fetch(base + path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ key: KEY, playlistIndex: 1 }) });

      expect(res.status).toBe(403);
      expect(h.launcher.markStoppedByUser).not.toHaveBeenCalled();
      expect(h.launcher.launchNow).not.toHaveBeenCalled();
      expect(h.protect).not.toHaveBeenCalled();
      expect(h.save).not.toHaveBeenCalled();
      expect(h.client.control).not.toHaveBeenCalled();
    });

  it('has no exit-75 test hook unless test hooks are on', async () => {
    // Act
    const res = await post('/test/exit-75');

    // Assert
    expect(res.status).toBe(404);
    expect(h.deps.triggerRestart).not.toHaveBeenCalled();
  });

  it('runs the update restart from the exit-75 test hook when test hooks are on', async () => {
    // Arrange
    server.closeAllConnections();
    await new Promise((r) => server.close(r));
    await listen({ ...h.deps, testHooks: true });

    // Act
    const res = await post('/test/exit-75');

    // Assert
    expect(res.status).toBe(200);
    expect(h.deps.triggerRestart).toHaveBeenCalledOnce();
  });

  it('refuses the exit-75 test hook without the API token', async () => {
    // Arrange
    server.closeAllConnections();
    await new Promise((r) => server.close(r));
    await listen({ ...h.deps, testHooks: true });

    // Act
    const res = await fetch(base + '/test/exit-75', { method: 'POST' });

    // Assert
    expect(res.status).toBe(403);
    expect(h.deps.triggerRestart).not.toHaveBeenCalled();
  });

  it.each(['/player/seek', '/player/pause', '/player/stop', '/player/resume', '/player/restart-mpv', '/obs-path/detect'])(
    'no longer serves POST %s', async (path) => {
      expect((await post(path)).status).toBe(404);
    });
});
