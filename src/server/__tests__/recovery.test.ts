import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { EventEmitter } from 'events';
import { RecoveryEngine } from '../recovery.js';
import { RecoveryStep, type AppConfig } from '../types.js';
import type { MpvClient } from '../mpv-client.js';
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
    mpvGeometry: '1920x1080+0+0',
    mpvYtdlFormat: 'bestvideo[height<=?1080]+bestaudio/best',
    mpvExtraArgs: [],
    ...overrides,
  };
}

function mockMpv() {
  const emitter = new EventEmitter();
  const mock = {
    on: vi.fn((event: string, handler: (...args: any[]) => void) => {
      emitter.on(event, handler);
      return mock;
    }),
    isConnected: vi.fn(() => true),
    isRunning: vi.fn(() => true),
    getProperty: vi.fn(async (name: string) => {
      switch (name) {
        case 'time-pos': return 0;
        case 'duration': return 0;
        case 'pause': return false;
        case 'idle-active': return false;
        case 'playlist-pos': return 0;
        case 'playlist-count': return 0;
        case 'media-title': return '';
        case 'filename': return '';
        default: return null;
      }
    }),
    setProperty: vi.fn(async () => {}),
    loadPlaylist: vi.fn(async () => {}),
    jumpTo: vi.fn(async () => {}),
    seek: vi.fn(async () => {}),
    next: vi.fn(async () => {}),
    restart: vi.fn(async () => {}),
    _emit: (event: string, ...args: any[]) => emitter.emit(event, ...args),
  };
  return mock;
}

type MockMpv = ReturnType<typeof mockMpv>;

function mockState(overrides: Partial<{ playlistIndex: number; videoIndex: number; videoId: string; videoTitle: string; currentTime: number; videoDuration: number; nextVideoId: string; updatedAt: string }> = {}) {
  const state = {
    playlistIndex: 0,
    videoIndex: 0,
    videoId: '',
    videoTitle: '',
    currentTime: 0,
    videoDuration: 0,
    nextVideoId: '',
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

  it('loads playlist on mpv connect', async () => {
    const mpv = mockMpv();
    const state = mockState({ videoIndex: 5, currentTime: 42 });
    const engine = new RecoveryEngine(makeConfig(), mpv as unknown as MpvClient, state, mockObs(), mockDiscord());
    engine.start();

    mpv._emit('connected');
    await vi.advanceTimersByTimeAsync(0);

    expect(mpv.loadPlaylist).toHaveBeenCalledWith(
      'https://www.youtube.com/playlist?list=PL123'
    );
  });

  it('loads correct playlist based on playlistIndex', async () => {
    const mpv = mockMpv();
    const state = mockState({ playlistIndex: 1, videoIndex: 3 });
    const config = makeConfig({ playlists: [{ id: 'PLA' }, { id: 'PLB' }] });
    const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), mockDiscord());
    engine.start();

    mpv._emit('connected');
    await vi.advanceTimersByTimeAsync(0);

    expect(mpv.loadPlaylist).toHaveBeenCalledWith(
      'https://www.youtube.com/playlist?list=PLB'
    );
  });

  it('clamps out-of-range playlistIndex to 0', async () => {
    const mpv = mockMpv();
    const state = mockState({ playlistIndex: 5, videoIndex: 0 });
    const config = makeConfig({ playlists: [{ id: 'PLA' }] });
    const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), mockDiscord());
    engine.start();

    mpv._emit('connected');
    await vi.advanceTimersByTimeAsync(0);

    expect(mpv.loadPlaylist).toHaveBeenCalledWith(
      'https://www.youtube.com/playlist?list=PLA'
    );
  });

  it('jumps to saved video index after playlist load delay', async () => {
    const mpv = mockMpv();
    const state = mockState({ videoIndex: 5, currentTime: 42 });
    const engine = new RecoveryEngine(makeConfig(), mpv as unknown as MpvClient, state, mockObs(), mockDiscord());
    engine.start();

    mpv._emit('connected');
    await vi.advanceTimersByTimeAsync(0);

    // After 2s delay, should jump to index 5
    await vi.advanceTimersByTimeAsync(2000);
    expect(mpv.jumpTo).toHaveBeenCalledWith(5);

    // After another 3s delay, should seek to 42s
    await vi.advanceTimersByTimeAsync(3000);
    expect(mpv.seek).toHaveBeenCalledWith(42);
  });

  it('updates state from heartbeat poll', async () => {
    const mpv = mockMpv();
    const state = mockState();
    const config = makeConfig({ heartbeatIntervalMs: 5000 });
    const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), mockDiscord());

    // Configure getProperty to return playing state
    mpv.getProperty.mockImplementation(async (name: string) => {
      switch (name) {
        case 'time-pos': return 99;
        case 'duration': return 300;
        case 'pause': return false;
        case 'idle-active': return false;
        case 'playlist-pos': return 3;
        case 'playlist-count': return 10;
        case 'media-title': return 'Test Video';
        case 'filename': return 'https://www.youtube.com/watch?v=dQw4w9WgXcQ';
        default: return null;
      }
    });

    engine.start();

    // Advance one heartbeat interval
    await vi.advanceTimersByTimeAsync(5000);

    expect(state.update).toHaveBeenCalledWith(
      expect.objectContaining({
        videoIndex: 3,
        videoId: 'dQw4w9WgXcQ',
        videoTitle: 'Test Video',
        videoDuration: 300,
        currentTime: 99,
      })
    );
  });

  it('skips heartbeat poll when mpv is not connected', async () => {
    const mpv = mockMpv();
    mpv.isConnected.mockReturnValue(false);
    const state = mockState();
    const config = makeConfig({ heartbeatIntervalMs: 5000 });
    const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), mockDiscord());
    engine.start();

    await vi.advanceTimersByTimeAsync(5000);

    expect(mpv.getProperty).not.toHaveBeenCalled();
  });

  it('handles fileEnded error by incrementing errors', async () => {
    const mpv = mockMpv();
    const discord = mockDiscord();
    const state = mockState({ videoIndex: 2, videoId: 'v1' });
    const engine = new RecoveryEngine(makeConfig(), mpv as unknown as MpvClient, state, mockObs(), discord);
    engine.start();

    mpv._emit('fileEnded', 'error');
    await vi.advanceTimersByTimeAsync(0);

    expect(discord.notifyError).toHaveBeenCalledWith(2, 'v1', 0, 1);
  });

  it('skips to next video after maxConsecutiveErrors file-ended errors', async () => {
    const mpv = mockMpv();
    const discord = mockDiscord();
    const state = mockState({ videoIndex: 1, videoId: 'v1' });
    const config = makeConfig({ maxConsecutiveErrors: 2 });
    const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), discord);
    engine.start();

    // First error
    mpv._emit('fileEnded', 'error');
    await vi.advanceTimersByTimeAsync(0);
    expect(mpv.next).not.toHaveBeenCalled();

    // Second error -> skip
    mpv._emit('fileEnded', 'error');
    await vi.advanceTimersByTimeAsync(0);
    expect(mpv.next).toHaveBeenCalled();
  });

  it('resets consecutive errors on eof', async () => {
    const mpv = mockMpv();
    const discord = mockDiscord();
    const state = mockState({ videoIndex: 1, videoId: 'v1' });
    const config = makeConfig({ maxConsecutiveErrors: 3 });
    const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), discord);
    engine.start();

    // One error
    mpv._emit('fileEnded', 'error');
    await vi.advanceTimersByTimeAsync(0);
    expect(discord.notifyError).toHaveBeenCalledTimes(1);

    // EOF resets counter
    mpv._emit('fileEnded', 'eof');
    await vi.advanceTimersByTimeAsync(0);

    // Another error — should be count 1, not 2
    mpv._emit('fileEnded', 'error');
    await vi.advanceTimersByTimeAsync(0);
    expect(discord.notifyError).toHaveBeenLastCalledWith(1, 'v1', 0, 1);
  });

  describe('stall detection', () => {
    function setupStallTest(configOverrides: Partial<AppConfig> = {}) {
      const mpv = mockMpv();
      const discord = mockDiscord();
      const state = mockState();
      const config = makeConfig({ heartbeatIntervalMs: 5000, ...configOverrides });
      const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), discord);

      let currentTimePos = 10;
      mpv.getProperty.mockImplementation(async (name: string) => {
        switch (name) {
          case 'time-pos': return currentTimePos;
          case 'duration': return 300;
          case 'pause': return false;
          case 'idle-active': return false;
          case 'playlist-pos': return 0;
          case 'playlist-count': return 10;
          case 'media-title': return 'Test';
          case 'filename': return 'test';
          default: return null;
        }
      });

      return {
        mpv, discord, state, engine,
        setTimePos: (t: number) => { currentTimePos = t; },
      };
    }

    it('triggers recovery after 3 stalled heartbeats', async () => {
      const { mpv, discord, engine, setTimePos } = setupStallTest();
      engine.start();

      // First heartbeat — establishes baseline
      setTimePos(50);
      await vi.advanceTimersByTimeAsync(5000);

      // Next 3 heartbeats at same position — stalled
      for (let i = 0; i < 3; i++) {
        await vi.advanceTimersByTimeAsync(5000);
      }

      expect(discord.notifyRecovery).toHaveBeenCalledWith('Stall detected');
    });

    it('does not trigger recovery when time advances', async () => {
      const { mpv, discord, engine, setTimePos } = setupStallTest();
      engine.start();

      setTimePos(50);
      await vi.advanceTimersByTimeAsync(5000);

      setTimePos(55);
      await vi.advanceTimersByTimeAsync(5000);

      setTimePos(60);
      await vi.advanceTimersByTimeAsync(5000);

      setTimePos(65);
      await vi.advanceTimersByTimeAsync(5000);

      // Only the standard recovery step notifications, not stall detection
      expect(discord.notifyRecovery).not.toHaveBeenCalledWith('Stall detected');
    });

    it('resets stall counter when progress resumes', async () => {
      const { discord, engine, setTimePos } = setupStallTest();
      engine.start();

      // Establish baseline
      setTimePos(50);
      await vi.advanceTimersByTimeAsync(5000);

      // 2 stalled heartbeats (not at threshold yet)
      await vi.advanceTimersByTimeAsync(5000);
      await vi.advanceTimersByTimeAsync(5000);

      // Progress resumes
      setTimePos(55);
      await vi.advanceTimersByTimeAsync(5000);

      // 2 more stalled heartbeats — should NOT trigger (counter was reset)
      await vi.advanceTimersByTimeAsync(5000);
      await vi.advanceTimersByTimeAsync(5000);

      expect(discord.notifyRecovery).not.toHaveBeenCalledWith('Stall detected');
    });
  });

  it('getStatus includes playlist info and mpv state', () => {
    const mpv = mockMpv();
    mpv.isConnected.mockReturnValue(true);
    mpv.isRunning.mockReturnValue(true);
    const state = mockState({ playlistIndex: 1 });
    const config = makeConfig({ playlists: [{ id: 'PLA' }, { id: 'PLB' }, { id: 'PLC' }] });
    const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), mockDiscord());
    engine.start();

    const status = engine.getStatus();
    expect(status.playlistIndex).toBe(1);
    expect(status.totalPlaylists).toBe(3);
    expect(status.currentPlaylistId).toBe('PLB');
    expect(status.mpvConnected).toBe(true);
    expect(status.mpvRunning).toBe(true);
  });

  it('stops cleanly', () => {
    const mpv = mockMpv();
    const engine = new RecoveryEngine(makeConfig(), mpv as unknown as MpvClient, mockState(), mockObs(), mockDiscord());
    engine.start();
    engine.stop();
    // Should not throw
  });

  describe('recovery escalation', () => {
    it('escalates through RetryCurrent -> RestartMpv -> CriticalAlert', async () => {
      const mpv = mockMpv();
      mpv.isConnected.mockReturnValue(false); // prevent heartbeat polls from interfering
      const discord = mockDiscord();
      const config = makeConfig({ recoveryDelayMs: 5000, heartbeatTimeoutMs: 15000 });
      const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, mockState(), mockObs(), discord);
      engine.start();

      // Trigger heartbeat timeout: disconnect mpv and wait
      await vi.advanceTimersByTimeAsync(20000);

      // Should have started recovery with RetryCurrent
      expect(discord.notifyRecovery).toHaveBeenCalledWith(RecoveryStep.RetryCurrent);
      expect(mpv.jumpTo).toHaveBeenCalled();

      // Advance past recoveryDelayMs for next step
      await vi.advanceTimersByTimeAsync(5000);
      expect(discord.notifyRecovery).toHaveBeenCalledWith(RecoveryStep.RestartMpv);
      expect(mpv.restart).toHaveBeenCalled();

      // Advance 15s for CriticalAlert
      await vi.advanceTimersByTimeAsync(15000);
      expect(discord.notifyCritical).toHaveBeenCalled();
    });
  });

  describe('playlist advancement on eof', () => {
    it('advances to next playlist when last video ends with eof', async () => {
      const mpv = mockMpv();
      const state = mockState({ playlistIndex: 0 });
      const config = makeConfig({ playlists: [{ id: 'PLA' }, { id: 'PLB' }], heartbeatIntervalMs: 5000 });
      const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), mockDiscord());

      // Set up poll to return last video in playlist
      mpv.getProperty.mockImplementation(async (name: string) => {
        switch (name) {
          case 'time-pos': return 100;
          case 'duration': return 300;
          case 'pause': return false;
          case 'idle-active': return false;
          case 'playlist-pos': return 4;
          case 'playlist-count': return 5;
          case 'media-title': return 'Last Video';
          case 'filename': return 'last';
          default: return null;
        }
      });

      engine.start();

      // Let a heartbeat tick to set totalVideos
      await vi.advanceTimersByTimeAsync(5000);

      // Now emit eof on last video
      // Update state to match what processHeartbeat would have set
      (state.get as any).mockReturnValue({ playlistIndex: 0, videoIndex: 4, videoId: 'last', videoTitle: 'Last Video', currentTime: 100, videoDuration: 300, nextVideoId: '', updatedAt: '' });

      mpv._emit('fileEnded', 'eof');
      await vi.advanceTimersByTimeAsync(0);

      expect(mpv.loadPlaylist).toHaveBeenCalledWith(
        'https://www.youtube.com/playlist?list=PLB'
      );
      expect(state.update).toHaveBeenCalledWith(
        expect.objectContaining({
          playlistIndex: 1,
          videoIndex: 0,
        })
      );
    });

    it('does not advance on eof with single playlist', async () => {
      const mpv = mockMpv();
      const state = mockState({ playlistIndex: 0 });
      const config = makeConfig({ playlists: [{ id: 'PLonly' }], heartbeatIntervalMs: 5000 });
      const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), mockDiscord());

      mpv.getProperty.mockImplementation(async (name: string) => {
        switch (name) {
          case 'time-pos': return 100;
          case 'duration': return 300;
          case 'pause': return false;
          case 'idle-active': return false;
          case 'playlist-pos': return 1;
          case 'playlist-count': return 2;
          case 'media-title': return 'Last';
          case 'filename': return 'last';
          default: return null;
        }
      });

      engine.start();
      await vi.advanceTimersByTimeAsync(5000);

      (state.get as any).mockReturnValue({ playlistIndex: 0, videoIndex: 1, videoId: 'last', videoTitle: 'Last', currentTime: 100, videoDuration: 300, nextVideoId: '', updatedAt: '' });

      mpv.loadPlaylist.mockClear();
      mpv._emit('fileEnded', 'eof');
      await vi.advanceTimersByTimeAsync(0);

      // Should NOT load a new playlist
      expect(mpv.loadPlaylist).not.toHaveBeenCalled();
    });
  });

  it('extracts video ID from YouTube URLs in filename', async () => {
    const mpv = mockMpv();
    const state = mockState();
    const config = makeConfig({ heartbeatIntervalMs: 5000 });
    const engine = new RecoveryEngine(config, mpv as unknown as MpvClient, state, mockObs(), mockDiscord());

    mpv.getProperty.mockImplementation(async (name: string) => {
      switch (name) {
        case 'time-pos': return 50;
        case 'duration': return 300;
        case 'pause': return false;
        case 'idle-active': return false;
        case 'playlist-pos': return 0;
        case 'playlist-count': return 5;
        case 'media-title': return 'My Video';
        case 'filename': return 'https://www.youtube.com/watch?v=dQw4w9WgXcQ';
        default: return null;
      }
    });

    engine.start();
    await vi.advanceTimersByTimeAsync(5000);

    expect(state.update).toHaveBeenCalledWith(
      expect.objectContaining({
        videoId: 'dQw4w9WgXcQ',
      })
    );
  });
});
