import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { writeFileSync, unlinkSync, mkdirSync, readFileSync } from 'fs';
import { join } from 'path';
import { loadConfig, saveConfig, isFirstRun } from '../config.js';

const tmpDir = join(import.meta.dirname, '..', '..', '..', '.test-tmp');
const tmpConfig = join(tmpDir, 'config.json');

beforeEach(() => {
  mkdirSync(tmpDir, { recursive: true });
});

afterEach(() => {
  try { unlinkSync(tmpConfig); } catch {}
});

describe('loadConfig', () => {
  it('loads valid config with playlists array', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      port: 4000,
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.playlists).toEqual([{ id: 'PLtest123' }]);
    expect(cfg.port).toBe(4000);
  });

  it('loads config with multiple playlists and names', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PL1', name: 'First' }, { id: 'PL2' }],
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.playlists).toHaveLength(2);
    expect(cfg.playlists[0]).toEqual({ id: 'PL1', name: 'First' });
    expect(cfg.playlists[1]).toEqual({ id: 'PL2' });
  });

  it('migrates old playlistId format to playlists array', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlistId: 'PLlegacy123',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.playlists).toEqual([{ id: 'PLlegacy123' }]);
    expect((cfg as any).playlistId).toBeUndefined();
  });

  it('applies defaults for optional fields', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.port).toBe(7654);
    expect(cfg.discord.webhookUrl).toBe('');
  });

  it('throws on missing required fields', () => {
    writeFileSync(tmpConfig, JSON.stringify({ port: 7654 }));
    expect(() => loadConfig(tmpConfig)).toThrow();
  });

  it('throws on invalid file', () => {
    expect(() => loadConfig('/nonexistent/config.json')).toThrow();
  });

  it('throws on empty playlists array', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [],
    }));
    expect(() => loadConfig(tmpConfig)).toThrow();
  });

  it('opens the admin panel on boot unless autoOpenAdmin is false', () => {
    // Arrange
    const base = { playlists: [{ id: 'PL1' }] };

    // Act
    writeFileSync(tmpConfig, JSON.stringify(base));
    const byDefault = loadConfig(tmpConfig);
    writeFileSync(tmpConfig, JSON.stringify({ ...base, autoOpenAdmin: false }));
    const disabled = loadConfig(tmpConfig);

    // Assert
    expect(byDefault.autoOpenAdmin).toBe(true);
    expect(disabled.autoOpenAdmin).toBe(false);
  });

  it('accepts ytdlCookiesFromBrowser string and defaults to empty', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PL1' }],
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.ytdlCookiesFromBrowser).toBe('');
  });

  it('preserves ytdlCookiesFromBrowser when set', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PL1' }],
      ytdlCookiesFromBrowser: 'brave:Profile 1',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.ytdlCookiesFromBrowser).toBe('brave:Profile 1');
  });
});

describe('isFirstRun', () => {
  it('returns true for default placeholder playlist', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLxxxxxxxxxxxxxxxx' }],
    }));
    const cfg = loadConfig(tmpConfig);
    expect(isFirstRun(cfg)).toBe(true);
  });

  it('returns false for real playlist', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLrealPlaylist' }],
    }));
    const cfg = loadConfig(tmpConfig);
    expect(isFirstRun(cfg)).toBe(false);
  });

  it('returns false for multiple playlists even if first has xxxxx', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLxxxxxxxxxxxxxxxx' }, { id: 'PLreal' }],
    }));
    const cfg = loadConfig(tmpConfig);
    expect(isFirstRun(cfg)).toBe(false);
  });
});

describe('discord config migration', () => {
  it('migrates old flat discordWebhookUrl to nested discord.webhookUrl', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      discordWebhookUrl: 'https://discord.com/api/webhooks/old',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.discord.webhookUrl).toBe('https://discord.com/api/webhooks/old');
    expect((cfg as any).discordWebhookUrl).toBeUndefined();
  });

  it('does not overwrite existing discord object during migration', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      discord: { webhookUrl: 'https://discord.com/api/webhooks/new' },
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.discord.webhookUrl).toBe('https://discord.com/api/webhooks/new');
  });
});

describe('discord config defaults', () => {
  it('applies default event toggles (all enabled)', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.discord.events.error).toBe(true);
    expect(cfg.discord.events.skip).toBe(true);
    expect(cfg.discord.events.recovery).toBe(true);
    expect(cfg.discord.events.critical).toBe(true);
    expect(cfg.discord.events.resume).toBe(true);
    expect(cfg.discord.events.outputFrozen).toBe(true);
    expect((cfg.discord.events as any).obsDisconnect).toBeUndefined();
  });

  it('applies default templates', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.discord.templates.error).toContain('{errorCode}');
    expect(cfg.discord.templates.skip).toContain('{reason}');
    expect(cfg.discord.templates.recovery).toContain('{step}');
    expect(cfg.discord.templates.critical).toContain('{message}');
    expect(cfg.discord.templates.resume).toContain('{videoIndex}');
    expect(cfg.discord.templates.outputFrozen).toContain('{kind}');
    expect(cfg.discord.templates.outputFrozen).toContain('{videoIndex}');
  });

  it('applies default empty strings for bot identity', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.discord.botName).toBe('');
    expect(cfg.discord.avatarUrl).toBe('');
    expect(cfg.discord.rolePing).toBe('');
  });

  it('preserves partial event overrides without clobbering others', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      discord: {
        webhookUrl: 'https://discord.com/api/webhooks/test',
        events: { error: false },
      },
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.discord.events.error).toBe(false);
    // Other events should still default to true
    expect(cfg.discord.events.skip).toBe(true);
    expect(cfg.discord.events.critical).toBe(true);
  });
});

describe('supervisor config', () => {
  const base = { playlists: [{ id: 'PLtest123' }] };

  it('fills supervisor defaults when the section is missing', () => {
    writeFileSync(tmpConfig, JSON.stringify(base));
    expect(loadConfig(tmpConfig).supervisor).toEqual({
      ingestUrl: 'rtmp://live.twitch.tv/app',
      streamKeyProtected: '',
      encoder: 'h264_nvenc',
      cacheBudgetMb: 300,
      readAheadSeconds: 20,
      playerClient: 'mweb,web_embedded',
      overlay: true,
      freezeSeconds: 20,
      silenceSeconds: 30,
    });
  });

  it('keeps the supervisor section across load and write-back', () => {
    writeFileSync(tmpConfig, JSON.stringify({ ...base, supervisor: { streamKeyProtected: 'blob', encoder: 'libx264' } }));
    loadConfig(tmpConfig);
    const onDisk = JSON.parse(readFileSync(tmpConfig, 'utf-8'));
    expect(onDisk.supervisor.streamKeyProtected).toBe('blob');
    expect(onDisk.supervisor.encoder).toBe('libx264');
  });

  it('drops stale 2.x OBS, mpv and recovery keys on write-back', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      ...base,
      obsBrowserSourceName: 'Source', obsWebsocketUrl: 'ws://x', obsAutoStream: true,
      mpvGeometry: '1x1+0+0', ytdlPlayerClient: 'tv', heartbeatIntervalMs: 1000, stateFilePath: './state.json',
    }));
    loadConfig(tmpConfig);
    const onDisk = JSON.parse(readFileSync(tmpConfig, 'utf-8'));
    for (const k of ['obsBrowserSourceName', 'obsWebsocketUrl', 'obsAutoStream', 'mpvGeometry', 'ytdlPlayerClient', 'heartbeatIntervalMs', 'stateFilePath']) {
      expect(onDisk).not.toHaveProperty(k);
    }
  });

  it.each([
    ['readAheadSeconds', 1.9],
    ['freezeSeconds', 4],
    ['silenceSeconds', 4],
    ['cacheBudgetMb', 49],
  ])('rejects %s below its minimum, naming the field', (key, bad) => {
    writeFileSync(tmpConfig, JSON.stringify({ ...base, supervisor: { [key]: bad } }));
    expect(() => loadConfig(tmpConfig)).toThrow(new RegExp(key));
  });

  it('rejects an unknown encoder', () => {
    writeFileSync(tmpConfig, JSON.stringify({ ...base, supervisor: { encoder: 'h265' } }));
    expect(() => loadConfig(tmpConfig)).toThrow(/encoder/);
  });

  it('keeps streamKeyProtected when saving a partial supervisor patch', () => {
    writeFileSync(tmpConfig, JSON.stringify({ ...base, supervisor: { streamKeyProtected: 'blob' } }));
    const saved = saveConfig({ supervisor: { encoder: 'h264_amf' } } as any, tmpConfig);
    expect(saved.supervisor.streamKeyProtected).toBe('blob');
    expect(saved.supervisor.encoder).toBe('h264_amf');
    expect(JSON.parse(readFileSync(tmpConfig, 'utf-8')).supervisor.streamKeyProtected).toBe('blob');
  });

  it('rejects a fractional cacheBudgetMb', () => {
    writeFileSync(tmpConfig, JSON.stringify({ ...base, supervisor: { cacheBudgetMb: 300.5 } }));
    expect(() => loadConfig(tmpConfig)).toThrow(/cacheBudgetMb/);
  });

  it('keeps streamKeyProtected when a save carries an empty one', () => {
    writeFileSync(tmpConfig, JSON.stringify({ ...base, supervisor: { streamKeyProtected: 'blob' } }));
    const saved = saveConfig({ supervisor: { streamKeyProtected: '', encoder: 'libx264' } } as any, tmpConfig);
    expect(saved.supervisor.streamKeyProtected).toBe('blob');
    expect(saved.supervisor.encoder).toBe('libx264');
  });

  it('replaces streamKeyProtected when a save carries a non-empty one', () => {
    writeFileSync(tmpConfig, JSON.stringify({ ...base, supervisor: { streamKeyProtected: 'blob' } }));
    const saved = saveConfig({ supervisor: { streamKeyProtected: 'new' } } as any, tmpConfig);
    expect(saved.supervisor.streamKeyProtected).toBe('new');
    expect(JSON.parse(readFileSync(tmpConfig, 'utf-8')).supervisor.streamKeyProtected).toBe('new');
  });

  it('saves a blank ingestUrl or playerClient as the default', () => {
    writeFileSync(tmpConfig, JSON.stringify({ ...base, supervisor: { ingestUrl: 'rtmp://custom/app', playerClient: 'tv' } }));
    const saved = saveConfig({ supervisor: { ingestUrl: '', playerClient: ' ' } } as any, tmpConfig);
    expect(saved.supervisor.ingestUrl).toBe('rtmp://live.twitch.tv/app');
    expect(saved.supervisor.playerClient).toBe('mweb,web_embedded');
  });

  it('keeps unknown supervisor keys through load and save', () => {
    writeFileSync(tmpConfig, JSON.stringify({ ...base, supervisor: { futureKnob: 7 } }));
    loadConfig(tmpConfig);
    expect(JSON.parse(readFileSync(tmpConfig, 'utf-8')).supervisor.futureKnob).toBe(7);
    saveConfig({ port: 7000 }, tmpConfig);
    expect(JSON.parse(readFileSync(tmpConfig, 'utf-8')).supervisor.futureKnob).toBe(7);
  });
});

describe('old discord keys', () => {
  it('loads a config that still has obsDisconnect and obsReconnect', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      discord: { events: { obsDisconnect: true }, templates: { obsReconnect: 'x' } },
    }));
    const cfg = loadConfig(tmpConfig);
    expect((cfg.discord.events as any).obsDisconnect).toBeUndefined();
    expect((cfg.discord.templates as any).obsReconnect).toBeUndefined();
  });
});
