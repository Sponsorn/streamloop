import { describe, it, expect, vi } from 'vitest';
import { OBSClient } from '../obs-client.js';
import type { AppConfig } from '../types.js';

function makeConfig(): AppConfig {
  return {
    port: 7654, obsWebsocketUrl: '', obsWebsocketPassword: '',
    obsBrowserSourceName: 'Video Capture', playlists: [{ id: 'PL1' }],
    discord: {
      webhookUrl: '', botName: '', avatarUrl: '', rolePing: '',
      events: { error: true, skip: true, recovery: true, critical: true, resume: true, obsDisconnect: true, obsReconnect: true, streamDrop: true, streamRestart: true, twitchMismatch: true, twitchRestart: true },
      templates: { error: '', skip: '', recovery: '', critical: '', resume: '', obsDisconnect: '', obsReconnect: '', streamDrop: '', streamRestart: '', twitchMismatch: '', twitchRestart: '' },
    },
    heartbeatIntervalMs: 5000, heartbeatTimeoutMs: 15000, maxConsecutiveErrors: 3,
    stateFilePath: './state.json', recoveryDelayMs: 5000, initialLoadGraceMs: 90000, obsAutoRestart: false,
    obsAutoStream: false, obsPath: '', autoUpdateCheck: true, autoUpdateYtdlp: true, updateCheckIntervalMs: 21600000,
    outputCheckEnabled: true, outputFreezeWindowMs: 30000, proactiveUrlRefreshMs: 19800000,
    sourceRefreshIntervalMs: 0, twitchClientId: '', twitchClientSecret: '', twitchChannel: '',
    twitchLivenessEnabled: false, twitchPollIntervalMs: 60000, mpvGeometry: '1920x1080+0+0',
    mpvYtdlFormat: 'best', mpvExtraArgs: [], ytdlCookiesFromBrowser: '',
  };
}

describe('OBSClient.getSourceScreenshot', () => {
  it('returns null when not connected', async () => {
    const client = new OBSClient(makeConfig());
    expect(await client.getSourceScreenshot()).toBeNull();
  });

  it('returns imageData when connected', async () => {
    const client = new OBSClient(makeConfig());
    (client as any).connected = true;
    (client as any).obs = { call: vi.fn(async () => ({ imageData: 'data:image/png;base64,XYZ' })) };
    expect(await client.getSourceScreenshot()).toBe('data:image/png;base64,XYZ');
  });

  it('returns null when the OBS call throws', async () => {
    const client = new OBSClient(makeConfig());
    (client as any).connected = true;
    (client as any).obs = { call: vi.fn(async () => { throw new Error('source not found'); }) };
    expect(await client.getSourceScreenshot()).toBeNull();
  });
});
