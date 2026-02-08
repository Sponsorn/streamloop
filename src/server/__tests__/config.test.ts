import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { writeFileSync, unlinkSync, mkdirSync } from 'fs';
import { join } from 'path';
import { loadConfig, isFirstRun } from '../config.js';

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
      obsBrowserSourceName: 'My Source',
      port: 4000,
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.playlists).toEqual([{ id: 'PLtest123' }]);
    expect(cfg.obsBrowserSourceName).toBe('My Source');
    expect(cfg.port).toBe(4000);
  });

  it('loads config with multiple playlists and names', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PL1', name: 'First' }, { id: 'PL2' }],
      obsBrowserSourceName: 'Source',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.playlists).toHaveLength(2);
    expect(cfg.playlists[0]).toEqual({ id: 'PL1', name: 'First' });
    expect(cfg.playlists[1]).toEqual({ id: 'PL2' });
  });

  it('migrates old playlistId format to playlists array', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlistId: 'PLlegacy123',
      obsBrowserSourceName: 'Source',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.playlists).toEqual([{ id: 'PLlegacy123' }]);
    expect((cfg as any).playlistId).toBeUndefined();
  });

  it('applies defaults for optional fields', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      obsBrowserSourceName: 'Source',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.port).toBe(3000);
    expect(cfg.heartbeatIntervalMs).toBe(5000);
    expect(cfg.heartbeatTimeoutMs).toBe(15000);
    expect(cfg.maxConsecutiveErrors).toBe(3);
    expect(cfg.recoveryDelayMs).toBe(5000);
    expect(cfg.discord.webhookUrl).toBe('');
    expect(cfg.obsWebsocketPassword).toBe('');
  });

  it('throws on missing required fields', () => {
    writeFileSync(tmpConfig, JSON.stringify({ port: 3000 }));
    expect(() => loadConfig(tmpConfig)).toThrow();
  });

  it('throws on invalid file', () => {
    expect(() => loadConfig('/nonexistent/config.json')).toThrow();
  });

  it('throws on empty playlists array', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [],
      obsBrowserSourceName: 'Source',
    }));
    expect(() => loadConfig(tmpConfig)).toThrow();
  });
});

describe('isFirstRun', () => {
  it('returns true for default placeholder playlist', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLxxxxxxxxxxxxxxxx' }],
      obsBrowserSourceName: 'Source',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(isFirstRun(cfg)).toBe(true);
  });

  it('returns false for real playlist', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLrealPlaylist' }],
      obsBrowserSourceName: 'Source',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(isFirstRun(cfg)).toBe(false);
  });

  it('returns false for multiple playlists even if first has xxxxx', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLxxxxxxxxxxxxxxxx' }, { id: 'PLreal' }],
      obsBrowserSourceName: 'Source',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(isFirstRun(cfg)).toBe(false);
  });
});

describe('discord config migration', () => {
  it('migrates old flat discordWebhookUrl to nested discord.webhookUrl', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      obsBrowserSourceName: 'Source',
      discordWebhookUrl: 'https://discord.com/api/webhooks/old',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.discord.webhookUrl).toBe('https://discord.com/api/webhooks/old');
    expect((cfg as any).discordWebhookUrl).toBeUndefined();
  });

  it('does not overwrite existing discord object during migration', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      obsBrowserSourceName: 'Source',
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
      obsBrowserSourceName: 'Source',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.discord.events.error).toBe(true);
    expect(cfg.discord.events.skip).toBe(true);
    expect(cfg.discord.events.recovery).toBe(true);
    expect(cfg.discord.events.critical).toBe(true);
    expect(cfg.discord.events.resume).toBe(true);
    expect(cfg.discord.events.obsDisconnect).toBe(true);
    expect(cfg.discord.events.obsReconnect).toBe(true);
  });

  it('applies default templates', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      obsBrowserSourceName: 'Source',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.discord.templates.error).toContain('{errorCode}');
    expect(cfg.discord.templates.skip).toContain('{reason}');
    expect(cfg.discord.templates.recovery).toContain('{step}');
    expect(cfg.discord.templates.critical).toContain('{message}');
    expect(cfg.discord.templates.resume).toContain('{videoIndex}');
    expect(cfg.discord.templates.obsDisconnect).toContain('OBS disconnected');
    expect(cfg.discord.templates.obsReconnect).toContain('OBS reconnected');
  });

  it('applies default empty strings for bot identity', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      obsBrowserSourceName: 'Source',
    }));
    const cfg = loadConfig(tmpConfig);
    expect(cfg.discord.botName).toBe('');
    expect(cfg.discord.avatarUrl).toBe('');
    expect(cfg.discord.rolePing).toBe('');
  });

  it('preserves partial event overrides without clobbering others', () => {
    writeFileSync(tmpConfig, JSON.stringify({
      playlists: [{ id: 'PLtest123' }],
      obsBrowserSourceName: 'Source',
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
