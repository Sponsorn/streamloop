import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { RecoveryEngine } from '../recovery.js';
import type { AppConfig } from '../types.js';
import type { PlayerWebSocket } from '../websocket.js';
import type { StateManager } from '../state.js';
import type { OBSClient } from '../obs-client.js';
import type { DiscordNotifier } from '../discord.js';

function makeConfig(overrides: Partial<AppConfig> = {}): AppConfig {
  return {
    port: 7654,
    obsWebsocketUrl: '',
    obsWebsocketPassword: '',
    obsBrowserSourceName: 'Source',
    playlists: [{ id: 'PL123' }],
    discord: {
      webhookUrl: '',
      botName: '',
      avatarUrl: '',
      rolePing: '',
      events: { error: true, skip: true, recovery: true, critical: true, resume: true, obsDisconnect: true, obsReconnect: true, streamDrop: true, streamRestart: true, twitchMismatch: true, twitchRestart: true },
      templates: {
        error: '', skip: '', recovery: '', critical: '', resume: '',
        obsDisconnect: '', obsReconnect: '', streamDrop: '', streamRestart: '',
        twitchMismatch: '', twitchRestart: '',
      },
    },
    heartbeatIntervalMs: 5000,
    heartbeatTimeoutMs: 15000,
    maxConsecutiveErrors: 3,
    stateFilePath: './state.json',
    recoveryDelayMs: 5000,
    obsAutoRestart: false,
    obsAutoStream: false,
    obsPath: '',
    autoUpdateCheck: true,
    updateCheckIntervalMs: 21600000,
    qualityRecoveryEnabled: true,
    minQuality: 'hd720',
    qualityRecoveryDelayMs: 120000,
    sourceRefreshIntervalMs: 0,
    twitchClientId: '',
    twitchClientSecret: '',
    twitchChannel: '',
    twitchLivenessEnabled: false,
    twitchPollIntervalMs: 60000,
    ...overrides,
  };
}

function mockWs() {
  let msgCb: Function = () => {};
  let connectCb: Function = () => {};
  let disconnectCb: Function = () => {};
  return {
    onMessage: vi.fn((cb: Function) => { msgCb = cb; }),
    onConnect: vi.fn((cb: Function) => { connectCb = cb; }),
    onDisconnect: vi.fn((cb: Function) => { disconnectCb = cb; }),
    send: vi.fn(),
    isConnected: vi.fn(() => false),
    close: vi.fn(),
    _triggerMessage: (msg: any) => msgCb(msg),
    _triggerConnect: () => connectCb(),
    _triggerDisconnect: () => disconnectCb(),
  } as unknown as PlayerWebSocket & {
    _triggerMessage: (msg: any) => void;
    _triggerConnect: () => void;
    _triggerDisconnect: () => void;
  };
}

function mockState(overrides: Partial<{ playlistIndex: number; videoIndex: number; videoId: string; videoTitle: string; currentTime: number; videoDuration: number; updatedAt: string }> = {}) {
  const state = {
    playlistIndex: 0,
    videoIndex: 0,
    videoId: '',
    videoTitle: '',
    currentTime: 0,
    videoDuration: 0,
    updatedAt: '',
    ...overrides,
  };
  return {
    get: vi.fn(() => ({ ...state })),
    update: vi.fn((partial: any) => Object.assign(state, partial)),
    flush: vi.fn(),
  } as unknown as StateManager;
}

function mockObs() {
  return {
    refreshBrowserSource: vi.fn(async () => true),
    toggleBrowserSource: vi.fn(async () => true),
  } as unknown as OBSClient;
}

function mockDiscord() {
  return {
    send: vi.fn(async () => {}),
    notifyError: vi.fn(async () => {}),
    notifySkip: vi.fn(async () => {}),
    notifyRecovery: vi.fn(async () => {}),
    notifyCritical: vi.fn(async () => {}),
    notifyResume: vi.fn(async () => {}),
  } as unknown as DiscordNotifier;
}

describe('RecoveryEngine', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('sends loadPlaylist on player connect with first playlist', () => {
    const ws = mockWs();
    const state = mockState({ videoIndex: 5 });
    const engine = new RecoveryEngine(makeConfig(), ws, state, mockObs(), mockDiscord());
    engine.start();

    ws._triggerConnect();

    expect((ws as any).send).toHaveBeenCalledWith({
      type: 'loadPlaylist',
      playlistId: 'PL123',
      index: 5,
      loop: true,
      startTime: 0,
    });
  });

  it('sends loadPlaylist with correct playlist based on playlistIndex', () => {
    const ws = mockWs();
    const state = mockState({ playlistIndex: 1, videoIndex: 3 });
    const config = makeConfig({ playlists: [{ id: 'PLA' }, { id: 'PLB' }] });
    const engine = new RecoveryEngine(config, ws, state, mockObs(), mockDiscord());
    engine.start();

    ws._triggerConnect();

    expect((ws as any).send).toHaveBeenCalledWith({
      type: 'loadPlaylist',
      playlistId: 'PLB',
      index: 3,
      loop: false,
      startTime: 0,
    });
  });

  it('clamps out-of-range playlistIndex to 0', () => {
    const ws = mockWs();
    const state = mockState({ playlistIndex: 5, videoIndex: 0 });
    const config = makeConfig({ playlists: [{ id: 'PLA' }] });
    const engine = new RecoveryEngine(config, ws, state, mockObs(), mockDiscord());
    engine.start();

    ws._triggerConnect();

    expect((ws as any).send).toHaveBeenCalledWith({
      type: 'loadPlaylist',
      playlistId: 'PLA',
      index: 0,
      loop: true,
      startTime: 0,
    });
  });

  it('updates state on heartbeat', () => {
    const ws = mockWs();
    const state = mockState();
    const engine = new RecoveryEngine(makeConfig(), ws, state, mockObs(), mockDiscord());
    engine.start();

    ws._triggerMessage({
      type: 'heartbeat',
      videoIndex: 3,
      videoId: 'abc',
      videoTitle: 'Test Video',
      playerState: 1,
      currentTime: 99,
      videoDuration: 300,
    });

    expect(state.update).toHaveBeenCalledWith({
      videoIndex: 3,
      videoId: 'abc',
      videoTitle: 'Test Video',
      currentTime: 99,
      videoDuration: 300,
      nextVideoId: '',
    });
  });

  it('skips immediately on error codes 100, 101, 150', async () => {
    const ws = mockWs();
    const discord = mockDiscord();
    const state = mockState();
    const engine = new RecoveryEngine(makeConfig(), ws, state, mockObs(), discord);
    engine.start();

    // Simulate playlistLoaded so totalVideos is set
    ws._triggerMessage({ type: 'playlistLoaded', totalVideos: 10 });

    ws._triggerMessage({ type: 'error', errorCode: 150, videoIndex: 2, videoId: 'bad' });

    await vi.advanceTimersByTimeAsync(0);

    expect(discord.notifySkip).toHaveBeenCalled();
    expect((ws as any).send).toHaveBeenCalledWith({ type: 'skip', index: 3 });
  });

  it('retries on non-skip errors up to maxConsecutiveErrors then skips', async () => {
    const ws = mockWs();
    const discord = mockDiscord();
    const state = mockState();
    const config = makeConfig({ maxConsecutiveErrors: 2, recoveryDelayMs: 100 });
    const engine = new RecoveryEngine(config, ws, state, mockObs(), discord);
    engine.start();

    ws._triggerMessage({ type: 'playlistLoaded', totalVideos: 10 });

    // First error → retry
    ws._triggerMessage({ type: 'error', errorCode: 5, videoIndex: 1, videoId: 'v1' });
    await vi.advanceTimersByTimeAsync(0);
    expect(discord.notifyError).toHaveBeenCalledTimes(1);

    // Second error → skip
    ws._triggerMessage({ type: 'error', errorCode: 5, videoIndex: 1, videoId: 'v1' });
    await vi.advanceTimersByTimeAsync(0);
    expect(discord.notifySkip).toHaveBeenCalledTimes(1);
  });

  it('advances to next playlist when skipping past last video with 2 playlists', async () => {
    const ws = mockWs();
    const state = mockState({ playlistIndex: 0 });
    const config = makeConfig({ playlists: [{ id: 'PLA' }, { id: 'PLB' }] });
    const engine = new RecoveryEngine(config, ws, state, mockObs(), mockDiscord());
    engine.start();

    ws._triggerMessage({ type: 'playlistLoaded', totalVideos: 3 });

    // Skip error on last video (index 2, totalVideos 3)
    ws._triggerMessage({ type: 'error', errorCode: 150, videoIndex: 2, videoId: 'last' });
    await vi.advanceTimersByTimeAsync(0);

    // Should send loadPlaylist for PLB at index 0
    expect((ws as any).send).toHaveBeenCalledWith({
      type: 'loadPlaylist',
      playlistId: 'PLB',
      index: 0,
      loop: false,
    });
    // State should be updated to playlist 1
    expect(state.update).toHaveBeenCalledWith({
      playlistIndex: 1,
      videoIndex: 0,
      videoId: '',
      currentTime: 0,
    });
  });

  it('single playlist wraps back to itself when skipping past last video', async () => {
    const ws = mockWs();
    const state = mockState({ playlistIndex: 0 });
    const config = makeConfig({ playlists: [{ id: 'PLonly' }] });
    const engine = new RecoveryEngine(config, ws, state, mockObs(), mockDiscord());
    engine.start();

    ws._triggerMessage({ type: 'playlistLoaded', totalVideos: 2 });

    ws._triggerMessage({ type: 'error', errorCode: 100, videoIndex: 1, videoId: 'last' });
    await vi.advanceTimersByTimeAsync(0);

    // Should reload same playlist at index 0 (wrap: (0+1) % 1 = 0)
    expect((ws as any).send).toHaveBeenCalledWith({
      type: 'loadPlaylist',
      playlistId: 'PLonly',
      index: 0,
      loop: true,
    });
  });

  it('stateChange ENDED on last video advances to next playlist', async () => {
    const ws = mockWs();
    const state = mockState({ playlistIndex: 0 });
    const config = makeConfig({ playlists: [{ id: 'PLA' }, { id: 'PLB' }] });
    const engine = new RecoveryEngine(config, ws, state, mockObs(), mockDiscord());
    engine.start();

    ws._triggerMessage({ type: 'playlistLoaded', totalVideos: 5 });

    // Simulate ENDED (playerState 0) on last video (index 4)
    ws._triggerMessage({ type: 'stateChange', playerState: 0, videoIndex: 4, videoId: 'last', videoTitle: '' });

    await vi.advanceTimersByTimeAsync(0);

    expect((ws as any).send).toHaveBeenCalledWith({
      type: 'loadPlaylist',
      playlistId: 'PLB',
      index: 0,
      loop: false,
    });
  });

  it('stateChange ENDED on last video does NOT advance with single playlist', async () => {
    const ws = mockWs();
    const state = mockState({ playlistIndex: 0 });
    const config = makeConfig({ playlists: [{ id: 'PLonly' }] });
    const engine = new RecoveryEngine(config, ws, state, mockObs(), mockDiscord());
    engine.start();

    ws._triggerMessage({ type: 'playlistLoaded', totalVideos: 3 });

    ws._triggerMessage({ type: 'stateChange', playerState: 0, videoIndex: 2, videoId: 'last', videoTitle: '' });

    await vi.advanceTimersByTimeAsync(0);

    // Should NOT send loadPlaylist (YouTube auto-loops single playlists)
    const loadCalls = (ws as any).send.mock.calls.filter(
      (c: any) => c[0].type === 'loadPlaylist'
    );
    expect(loadCalls).toHaveLength(0);
  });

  it('getStatus includes playlist info', () => {
    const ws = mockWs();
    const state = mockState({ playlistIndex: 1 });
    const config = makeConfig({ playlists: [{ id: 'PLA' }, { id: 'PLB' }, { id: 'PLC' }] });
    const engine = new RecoveryEngine(config, ws, state, mockObs(), mockDiscord());
    engine.start();

    const status = engine.getStatus();
    expect(status.playlistIndex).toBe(1);
    expect(status.totalPlaylists).toBe(3);
    expect(status.currentPlaylistId).toBe('PLB');
  });

  it('stops cleanly', () => {
    const ws = mockWs();
    const engine = new RecoveryEngine(makeConfig(), ws, mockState(), mockObs(), mockDiscord());
    engine.start();
    engine.stop();
    // Should not throw
  });

  describe('quality recovery', () => {
    let _timeCounter = 100;
    function sendHeartbeat(ws: ReturnType<typeof mockWs>, quality: string, overrides: Record<string, unknown> = {}) {
      _timeCounter += 10;
      ws._triggerMessage({
        type: 'heartbeat',
        videoIndex: 0,
        videoId: 'vid1',
        videoTitle: 'Test',
        playerState: 1,
        currentTime: _timeCounter,
        videoDuration: 300,
        playbackQuality: quality,
        ...overrides,
      });
    }

    it('triggers RefreshSource after sustained low quality', async () => {
      const ws = mockWs();
      const obs = mockObs();
      const discord = mockDiscord();
      // Short delay: 3 heartbeats at 5000ms interval
      const config = makeConfig({ qualityRecoveryDelayMs: 15000, heartbeatIntervalMs: 5000 });
      const engine = new RecoveryEngine(config, ws, mockState(), obs, discord);
      engine.start();

      // Send 3 low-quality heartbeats (threshold = ceil(15000/5000) = 3)
      let time = 10;
      for (let i = 0; i < 3; i++) {
        time += 5;
        sendHeartbeat(ws, 'medium', { currentTime: time });
      }

      await vi.advanceTimersByTimeAsync(0);
      expect(obs.refreshBrowserSource).toHaveBeenCalled();
      expect(discord.notifyRecovery).toHaveBeenCalledWith(
        expect.stringContaining('medium')
      );
    });

    it('uses heartbeatIntervalMs for threshold calculation, not hardcoded 5000', async () => {
      const ws = mockWs();
      const obs = mockObs();
      // 10000ms delay, 2000ms heartbeat interval → threshold = 5
      const config = makeConfig({ qualityRecoveryDelayMs: 10000, heartbeatIntervalMs: 2000 });
      const engine = new RecoveryEngine(config, ws, mockState(), obs, mockDiscord());
      engine.start();

      let time = 10;
      // 4 heartbeats should NOT trigger (threshold is 5)
      for (let i = 0; i < 4; i++) {
        time += 5;
        sendHeartbeat(ws, 'small', { currentTime: time });
      }
      expect(obs.refreshBrowserSource).not.toHaveBeenCalled();

      // 5th heartbeat should trigger
      sendHeartbeat(ws, 'small', { currentTime: time + 5 });
      await vi.advanceTimersByTimeAsync(0);
      expect(obs.refreshBrowserSource).toHaveBeenCalled();
    });

    it('decays counter gradually when quality improves instead of hard reset', async () => {
      const ws = mockWs();
      const obs = mockObs();
      // threshold = ceil(15000/5000) = 3
      const config = makeConfig({ qualityRecoveryDelayMs: 15000, heartbeatIntervalMs: 5000 });
      const engine = new RecoveryEngine(config, ws, mockState(), obs, mockDiscord());
      engine.start();

      let time = 10;
      // 2 low quality heartbeats
      sendHeartbeat(ws, 'medium', { currentTime: time += 5 });
      sendHeartbeat(ws, 'medium', { currentTime: time += 5 });

      // 1 good heartbeat — decays by 1 (from 2 to 1), not reset to 0
      sendHeartbeat(ws, 'hd720', { currentTime: time += 5 });

      // 1 more low quality — counter goes from 1 to 2, not threshold yet
      sendHeartbeat(ws, 'medium', { currentTime: time += 5 });
      expect(obs.refreshBrowserSource).not.toHaveBeenCalled();

      // 1 more low quality — counter goes from 2 to 3, hits threshold
      sendHeartbeat(ws, 'medium', { currentTime: time += 5 });
      await vi.advanceTimersByTimeAsync(0);
      expect(obs.refreshBrowserSource).toHaveBeenCalled();
    });

    it('quality recovery escalates through ToggleVisibility when quality stays low', async () => {
      const ws = mockWs();
      const obs = mockObs();
      const discord = mockDiscord();
      const config = makeConfig({ qualityRecoveryDelayMs: 15000, heartbeatIntervalMs: 5000 });
      const engine = new RecoveryEngine(config, ws, mockState(), obs, discord);
      engine.start();

      let time = 10;
      // Trigger quality recovery (3 heartbeats)
      for (let i = 0; i < 3; i++) {
        sendHeartbeat(ws, 'small', { currentTime: time += 5 });
      }
      await vi.advanceTimersByTimeAsync(0);
      expect(obs.refreshBrowserSource).toHaveBeenCalledTimes(1);

      // Keep sending low quality heartbeats during recovery to keep playbackQuality low
      sendHeartbeat(ws, 'small', { currentTime: time += 5 });

      // Advance past the 15s scheduled next step
      await vi.advanceTimersByTimeAsync(15000);

      // Should have escalated to ToggleVisibility
      expect(obs.toggleBrowserSource).toHaveBeenCalled();
    });

    it('quality recovery cancels escalation when quality improves', async () => {
      const ws = mockWs();
      const obs = mockObs();
      const config = makeConfig({ qualityRecoveryDelayMs: 15000, heartbeatIntervalMs: 5000 });
      const engine = new RecoveryEngine(config, ws, mockState(), obs, mockDiscord());
      engine.start();

      let time = 10;
      // Trigger quality recovery
      for (let i = 0; i < 3; i++) {
        sendHeartbeat(ws, 'small', { currentTime: time += 5 });
      }
      await vi.advanceTimersByTimeAsync(0);
      expect(obs.refreshBrowserSource).toHaveBeenCalledTimes(1);

      // Quality improves — update playbackQuality via heartbeat
      sendHeartbeat(ws, 'hd1080', { currentTime: time += 5 });

      // Advance past the scheduled escalation
      await vi.advanceTimersByTimeAsync(15000);

      // Should NOT have escalated — recovery cancelled
      expect(obs.toggleBrowserSource).not.toHaveBeenCalled();
    });

    it('Discord notification includes quality details', async () => {
      const ws = mockWs();
      const discord = mockDiscord();
      const config = makeConfig({ qualityRecoveryDelayMs: 15000, heartbeatIntervalMs: 5000, minQuality: 'hd720' });
      const engine = new RecoveryEngine(config, ws, mockState(), mockObs(), discord);
      engine.start();

      let time = 10;
      for (let i = 0; i < 3; i++) {
        sendHeartbeat(ws, 'large', { currentTime: time += 5 });
      }

      await vi.advanceTimersByTimeAsync(0);
      expect(discord.notifyRecovery).toHaveBeenCalledWith(
        expect.stringMatching(/large.*hd720/)
      );
    });
  });
});
