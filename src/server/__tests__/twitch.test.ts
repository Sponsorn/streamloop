import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { TwitchLivenessChecker } from '../twitch.js';
import type { AppConfig } from '../types.js';
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
      events: { error: true, skip: true, recovery: true, critical: true, resume: true, obsDisconnect: true, obsReconnect: true, streamDrop: true, streamRestart: true },
      templates: {
        error: '', skip: '', recovery: '', critical: '', resume: '',
        obsDisconnect: '', obsReconnect: '', streamDrop: '', streamRestart: '',
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
    twitchClientId: 'test-client-id',
    twitchClientSecret: 'test-client-secret',
    twitchChannel: 'testchannel',
    twitchLivenessEnabled: true,
    twitchPollIntervalMs: 60000,
    ...overrides,
  };
}

function mockObs() {
  return {
    isStreaming: vi.fn(() => Promise.resolve(false)),
    stopStream: vi.fn(() => Promise.resolve(true)),
    startStreaming: vi.fn(() => Promise.resolve(true)),
    isConnected: vi.fn(() => true),
  } as unknown as OBSClient;
}

function mockDiscord() {
  return {
    send: vi.fn(() => Promise.resolve()),
  } as unknown as DiscordNotifier;
}

describe('TwitchLivenessChecker', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // Mock global fetch
    vi.stubGlobal('fetch', vi.fn());
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  describe('enabled getter', () => {
    it('returns false when toggle is off', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchLivenessEnabled: false }),
        mockObs(),
        mockDiscord(),
      );
      expect(checker.enabled).toBe(false);
    });

    it('returns false when clientId is empty', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchClientId: '' }),
        mockObs(),
        mockDiscord(),
      );
      expect(checker.enabled).toBe(false);
    });

    it('returns false when clientSecret is empty', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchClientSecret: '' }),
        mockObs(),
        mockDiscord(),
      );
      expect(checker.enabled).toBe(false);
    });

    it('returns false when channel is empty', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchChannel: '' }),
        mockObs(),
        mockDiscord(),
      );
      expect(checker.enabled).toBe(false);
    });

    it('returns true when all credentials are present and toggle is on', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig(),
        mockObs(),
        mockDiscord(),
      );
      expect(checker.enabled).toBe(true);
    });
  });

  describe('getStatus()', () => {
    it('returns correct initial structure', () => {
      const checker = new TwitchLivenessChecker(makeConfig(), mockObs(), mockDiscord());
      const status = checker.getStatus();
      expect(status).toEqual({
        enabled: true,
        channelLive: null,
        obsStreaming: null,
        consecutiveMismatches: 0,
        mismatchThreshold: 3,
        lastCheckAt: null,
        lastRestartAt: null,
        restartCount: 0,
      });
    });

    it('reports enabled false when disabled', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchLivenessEnabled: false }),
        mockObs(),
        mockDiscord(),
      );
      expect(checker.getStatus().enabled).toBe(false);
    });
  });

  describe('start() / stop()', () => {
    it('start sets up timers and stop clears them', () => {
      const checker = new TwitchLivenessChecker(makeConfig(), mockObs(), mockDiscord());
      checker.start();
      // Timer should be scheduled
      checker.stop();
      // No errors — timers cleared
    });

    it('start does nothing when disabled', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchLivenessEnabled: false }),
        mockObs(),
        mockDiscord(),
      );
      checker.start();
      checker.stop();
    });
  });

  describe('mismatch counting', () => {
    function setupFetch(liveData: { data: unknown[] } | null, tokenOk = true) {
      const fetchMock = vi.fn();
      // Token request
      if (tokenOk) {
        fetchMock.mockResolvedValueOnce({
          ok: true,
          json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }),
        });
      }
      // Streams request
      if (liveData !== null) {
        fetchMock.mockResolvedValueOnce({
          ok: true,
          status: 200,
          json: () => Promise.resolve(liveData),
        });
      }
      vi.stubGlobal('fetch', fetchMock);
      return fetchMock;
    }

    it('increments on mismatch (OBS streaming, Twitch offline)', async () => {
      const obs = mockObs();
      (obs.isStreaming as any).mockResolvedValue(true);
      setupFetch({ data: [] }); // channel not live

      const checker = new TwitchLivenessChecker(makeConfig(), obs, mockDiscord());
      await checker.check();

      expect(checker.getStatus().consecutiveMismatches).toBe(1);
    });

    it('resets on non-mismatch (both streaming)', async () => {
      const obs = mockObs();
      (obs.isStreaming as any).mockResolvedValue(true);
      const fetchMock = vi.fn();

      // First check: mismatch (channel offline)
      fetchMock.mockResolvedValueOnce({
        ok: true,
        json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }),
      });
      fetchMock.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: () => Promise.resolve({ data: [] }),
      });
      vi.stubGlobal('fetch', fetchMock);

      const checker = new TwitchLivenessChecker(makeConfig(), obs, mockDiscord());
      await checker.check();
      expect(checker.getStatus().consecutiveMismatches).toBe(1);

      // Second check: channel is live now
      fetchMock.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: () => Promise.resolve({ data: [{ type: 'live' }] }),
      });

      await checker.check();
      expect(checker.getStatus().consecutiveMismatches).toBe(0);
    });

    it('resets on non-mismatch (OBS not streaming)', async () => {
      const obs = mockObs();
      (obs.isStreaming as any).mockResolvedValue(false);
      setupFetch({ data: [] });

      const checker = new TwitchLivenessChecker(makeConfig(), obs, mockDiscord());
      await checker.check();

      expect(checker.getStatus().consecutiveMismatches).toBe(0);
    });

    it('triggers restart at threshold (3 mismatches)', async () => {
      const obs = mockObs();
      (obs.isStreaming as any).mockResolvedValue(true);
      const discord = mockDiscord();

      const fetchMock = vi.fn();
      // Token (shared for all checks since it's cached after first)
      fetchMock.mockResolvedValueOnce({
        ok: true,
        json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }),
      });
      // 3 stream checks — all say offline
      for (let i = 0; i < 3; i++) {
        fetchMock.mockResolvedValueOnce({
          ok: true,
          status: 200,
          json: () => Promise.resolve({ data: [] }),
        });
      }
      vi.stubGlobal('fetch', fetchMock);

      const checker = new TwitchLivenessChecker(makeConfig(), obs, discord);

      await checker.check();
      expect(checker.getStatus().consecutiveMismatches).toBe(1);
      expect(obs.stopStream).not.toHaveBeenCalled();

      await checker.check();
      expect(checker.getStatus().consecutiveMismatches).toBe(2);
      expect(obs.stopStream).not.toHaveBeenCalled();

      // Third check triggers restart which has a 5s delay inside
      const checkPromise = checker.check();
      // Advance past the restart delay
      await vi.advanceTimersByTimeAsync(6000);
      await checkPromise;

      // After restart, counter resets to 0
      expect(checker.getStatus().consecutiveMismatches).toBe(0);
      expect(obs.stopStream).toHaveBeenCalledOnce();
      expect(obs.startStreaming).toHaveBeenCalledOnce();
      expect(discord.send).toHaveBeenCalled();
      expect(checker.getStatus().restartCount).toBe(1);
    });
  });

  describe('API errors', () => {
    it('does not increment counter on API error', async () => {
      const obs = mockObs();
      (obs.isStreaming as any).mockResolvedValue(true);

      // Token succeeds
      const fetchMock = vi.fn();
      fetchMock.mockResolvedValueOnce({
        ok: true,
        json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }),
      });
      // Streams API returns 500
      fetchMock.mockResolvedValueOnce({
        ok: false,
        status: 500,
      });
      vi.stubGlobal('fetch', fetchMock);

      const checker = new TwitchLivenessChecker(makeConfig(), obs, mockDiscord());
      await checker.check();

      expect(checker.getStatus().consecutiveMismatches).toBe(0);
    });

    it('does not increment counter on fetch exception', async () => {
      const obs = mockObs();
      (obs.isStreaming as any).mockResolvedValue(true);

      // Token succeeds
      const fetchMock = vi.fn();
      fetchMock.mockResolvedValueOnce({
        ok: true,
        json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }),
      });
      // Streams API throws network error
      fetchMock.mockRejectedValueOnce(new Error('network error'));
      vi.stubGlobal('fetch', fetchMock);

      const checker = new TwitchLivenessChecker(makeConfig(), obs, mockDiscord());
      await checker.check();

      expect(checker.getStatus().consecutiveMismatches).toBe(0);
    });
  });

  describe('token management', () => {
    it('fetches a token on first check', async () => {
      const obs = mockObs();
      (obs.isStreaming as any).mockResolvedValue(false);

      const fetchMock = vi.fn();
      fetchMock.mockResolvedValueOnce({
        ok: true,
        json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }),
      });
      fetchMock.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: () => Promise.resolve({ data: [] }),
      });
      vi.stubGlobal('fetch', fetchMock);

      const checker = new TwitchLivenessChecker(makeConfig(), obs, mockDiscord());
      await checker.check();

      // First call should be token request
      expect(fetchMock).toHaveBeenCalledTimes(2);
      const tokenCall = fetchMock.mock.calls[0];
      expect(tokenCall[0]).toBe('https://id.twitch.tv/oauth2/token');
    });

    it('reuses cached token for subsequent checks', async () => {
      const obs = mockObs();
      (obs.isStreaming as any).mockResolvedValue(false);

      const fetchMock = vi.fn();
      // First check: token + streams
      fetchMock.mockResolvedValueOnce({
        ok: true,
        json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }),
      });
      fetchMock.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: () => Promise.resolve({ data: [] }),
      });
      // Second check: only streams (token cached)
      fetchMock.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: () => Promise.resolve({ data: [] }),
      });
      vi.stubGlobal('fetch', fetchMock);

      const checker = new TwitchLivenessChecker(makeConfig(), obs, mockDiscord());
      await checker.check();
      await checker.check();

      // 2 token + 1 streams = 3 total... wait no:
      // check 1: token(1) + streams(2) = 2 calls
      // check 2: no token (cached) + streams(3) = 1 call
      expect(fetchMock).toHaveBeenCalledTimes(3);
    });

    it('refreshes token on 401 response', async () => {
      const obs = mockObs();
      (obs.isStreaming as any).mockResolvedValue(false);

      const fetchMock = vi.fn();
      // Initial token
      fetchMock.mockResolvedValueOnce({
        ok: true,
        json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }),
      });
      // Streams returns 401
      fetchMock.mockResolvedValueOnce({
        ok: false,
        status: 401,
      });
      // Re-fetch token
      fetchMock.mockResolvedValueOnce({
        ok: true,
        json: () => Promise.resolve({ access_token: 'tok456', expires_in: 3600 }),
      });
      // Retry streams
      fetchMock.mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: () => Promise.resolve({ data: [{ type: 'live' }] }),
      });
      vi.stubGlobal('fetch', fetchMock);

      const checker = new TwitchLivenessChecker(makeConfig(), obs, mockDiscord());
      await checker.check();

      expect(fetchMock).toHaveBeenCalledTimes(4);
      expect(checker.getStatus().channelLive).toBe(true);
    });
  });
});
