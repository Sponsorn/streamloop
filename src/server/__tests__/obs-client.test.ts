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
    obsAutoStream: false, obsPath: '', autoUpdateCheck: true, autoUpdateYtdlp: true, autoOpenAdmin: true, updateCheckIntervalMs: 21600000,
    outputCheckEnabled: true, outputFreezeWindowMs: 30000, proactiveUrlRefreshMs: 19800000,
    sourceRefreshIntervalMs: 0, twitchClientId: '', twitchClientSecret: '', twitchChannel: '',
    twitchLivenessEnabled: false, twitchPollIntervalMs: 60000, mpvGeometry: '1920x1080+0+0',
    mpvYtdlFormat: 'best', mpvExtraArgs: [], ytdlCookiesFromBrowser: '', ytdlPlayerClient: '',
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

function streamingClient(config: AppConfig, guardBlocks: () => boolean) {
  const client = new OBSClient(config);
  const call = vi.fn(async (req: string) => {
    if (req === 'GetStreamStatus') return { outputActive: false };
    if (req === 'GetCurrentProgramScene') return { currentProgramSceneName: 'Main' };
    if (req === 'GetSceneItemList') return { sceneItems: [{ sourceName: config.obsBrowserSourceName, sceneItemId: 1 }] };
    if (req === 'GetSceneItemEnabled') return { sceneItemEnabled: true };
    return {};
  });
  (client as any).connected = true;
  (client as any).obs = { call };
  client.setStartGuard(async () => guardBlocks());
  const startCalls = () => call.mock.calls.filter(([req]) => req === 'StartStream').length;
  return { client, startCalls };
}

describe('OBSClient start guard (3.0 streaming on the same PC)', () => {
  it('startStreaming does not call StartStream while the guard blocks', async () => {
    const { client, startCalls } = streamingClient(makeConfig(), () => true);
    expect(await client.startStreaming()).toBe(false);
    expect(startCalls()).toBe(0);
  });

  it('startStreaming starts normally when the guard allows', async () => {
    const { client, startCalls } = streamingClient(makeConfig(), () => false);
    expect(await client.startStreaming()).toBe(true);
    expect(startCalls()).toBe(1);
  });

  it('onStartBlocked fires once per episode', async () => {
    // Arrange
    let blocked = true;
    const { client } = streamingClient(makeConfig(), () => blocked);
    const seen: boolean[] = [];
    client.onStartBlocked((b) => seen.push(b));
    // Act
    await client.startStreaming();
    await client.startStreaming();
    await client.startStreaming();
    blocked = false;
    await client.startStreaming();
    // Assert
    expect(seen).toEqual([true, false]);
  });

  it('the scheduled restart respects the guard', async () => {
    vi.useFakeTimers();
    try {
      const config = { ...makeConfig(), obsAutoStream: true };
      const { client, startCalls } = streamingClient(config, () => true);
      (client as any).isPlayerHealthy = () => true;
      (client as any).handleStreamStateChanged({ outputActive: false, outputState: 'OBS_WEBSOCKET_OUTPUT_STOPPED' });
      await vi.advanceTimersByTimeAsync(10_000);
      expect(startCalls()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('OBSClient after a guard-blocked restart', () => {
  it('does not report the later start as a successful restart', async () => {
    vi.useFakeTimers();
    try {
      // Arrange: a drop while 3.0 streams, so the scheduled restart is refused
      let blocked = true;
      const config = { ...makeConfig(), obsAutoStream: true };
      const { client } = streamingClient(config, () => blocked);
      (client as any).isPlayerHealthy = () => true;
      const restarts: number[] = [];
      client.onStreamRestart((attempts) => restarts.push(attempts));
      (client as any).handleStreamStateChanged({ outputActive: false, outputState: 'OBS_WEBSOCKET_OUTPUT_STOPPED' });
      await vi.advanceTimersByTimeAsync(10_000);
      // Act: 3.0 stops and the health check's start succeeds
      blocked = false;
      await client.startStreaming();
      (client as any).handleStreamStateChanged({ outputActive: true, outputState: 'OBS_WEBSOCKET_OUTPUT_STARTED' });
      // Assert
      expect(restarts).toEqual([]);
    } finally {
      vi.useRealTimers();
    }
  });
});
