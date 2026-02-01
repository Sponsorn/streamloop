import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { RecoveryEngine } from '../recovery.js';
import type { AppConfig } from '../types.js';
import type { PlayerWebSocket } from '../websocket.js';
import type { StateManager } from '../state.js';
import type { OBSClient } from '../obs-client.js';
import type { DiscordNotifier } from '../discord.js';

function makeConfig(overrides: Partial<AppConfig> = {}): AppConfig {
  return {
    port: 3000,
    obsWebsocketUrl: '',
    obsWebsocketPassword: '',
    obsBrowserSourceName: 'Source',
    playlists: [{ id: 'PL123' }],
    discordWebhookUrl: '',
    heartbeatIntervalMs: 5000,
    heartbeatTimeoutMs: 15000,
    maxConsecutiveErrors: 3,
    stateFilePath: './state.json',
    recoveryDelayMs: 5000,
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
    isConnected: vi.fn(() => true),
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

function mockState(overrides: Partial<{ playlistIndex: number; videoIndex: number; videoId: string; currentTime: number; updatedAt: string }> = {}) {
  const state = {
    playlistIndex: 0,
    videoIndex: 0,
    videoId: '',
    currentTime: 0,
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
      playerState: 1,
      currentTime: 99,
    });

    expect(state.update).toHaveBeenCalledWith({
      videoIndex: 3,
      videoId: 'abc',
      currentTime: 99,
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
    ws._triggerMessage({ type: 'stateChange', playerState: 0, videoIndex: 4, videoId: 'last' });

    await vi.advanceTimersByTimeAsync(0);

    expect((ws as any).send).toHaveBeenCalledWith({
      type: 'loadPlaylist',
      playlistId: 'PLB',
      index: 0,
    });
  });

  it('stateChange ENDED on last video does NOT advance with single playlist', async () => {
    const ws = mockWs();
    const state = mockState({ playlistIndex: 0 });
    const config = makeConfig({ playlists: [{ id: 'PLonly' }] });
    const engine = new RecoveryEngine(config, ws, state, mockObs(), mockDiscord());
    engine.start();

    ws._triggerMessage({ type: 'playlistLoaded', totalVideos: 3 });

    ws._triggerMessage({ type: 'stateChange', playerState: 0, videoIndex: 2, videoId: 'last' });

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
});
