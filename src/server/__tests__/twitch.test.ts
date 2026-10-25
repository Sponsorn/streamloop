import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { TwitchLivenessChecker } from '../twitch.js';
import type { AppConfig } from '../types.js';
import type { SupervisorClient } from '../supervisor-client.js';
import type { DiscordNotifier } from '../discord.js';

function makeConfig(overrides: Partial<AppConfig> = {}): AppConfig {
  return {
    port: 7654,
    playlists: [{ id: 'PL123' }],
    discord: {
      webhookUrl: '',
      botName: '',
      avatarUrl: '',
      rolePing: '',
      events: { error: true, skip: true, recovery: true, critical: true, resume: true, streamDrop: true, streamRestart: true, outputFrozen: true, twitchMismatch: true, twitchRestart: true },
      templates: {
        error: '', skip: '', recovery: '', critical: '', resume: '',
        streamDrop: '', streamRestart: '', outputFrozen: '',
        twitchMismatch: '', twitchRestart: '',
      },
    },
    supervisor: {
      ingestUrl: 'rtmp://live.twitch.tv/app', streamKeyProtected: '', encoder: 'h264_nvenc', cacheBudgetMb: 300,
      readAheadSeconds: 20, playerClient: 'mweb,web_embedded', overlay: true, freezeSeconds: 20, silenceSeconds: 30,
    },
    autoUpdateCheck: true,
    autoUpdateYtdlp: true,
    autoOpenAdmin: true,
    updateCheckIntervalMs: 21600000,
    twitchClientId: 'test-client-id',
    twitchClientSecret: 'test-client-secret',
    twitchChannel: 'testchannel',
    twitchLivenessEnabled: true,
    twitchPollIntervalMs: 60000,
    ytdlCookiesFromBrowser: '',
    ...overrides,
  };
}

function mockClient(alive = false) {
  return {
    status: vi.fn(() => Promise.resolve({ state: 'playing', encoder: { alive } })),
    control: vi.fn(() => Promise.resolve({ ok: true })),
  } as unknown as SupervisorClient & { status: ReturnType<typeof vi.fn>; control: ReturnType<typeof vi.fn> };
}

function mockDiscord() {
  return {
    send: vi.fn(() => Promise.resolve()),
    notifyTwitchMismatch: vi.fn(() => Promise.resolve()),
    notifyTwitchRestart: vi.fn(() => Promise.resolve()),
    notifyCritical: vi.fn(() => Promise.resolve()),
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
        mockClient(),
        mockDiscord(),
      );
      expect(checker.enabled).toBe(false);
    });

    it('returns false when clientId is empty', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchClientId: '' }),
        mockClient(),
        mockDiscord(),
      );
      expect(checker.enabled).toBe(false);
    });

    it('returns false when clientSecret is empty', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchClientSecret: '' }),
        mockClient(),
        mockDiscord(),
      );
      expect(checker.enabled).toBe(false);
    });

    it('returns false when channel is empty', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchChannel: '' }),
        mockClient(),
        mockDiscord(),
      );
      expect(checker.enabled).toBe(false);
    });

    it('needs no Helix credentials with a test HLS URL, and never asks Twitch', async () => {
      // Arrange
      const fetchMock = vi.fn();
      vi.stubGlobal('fetch', fetchMock);
      const client = mockClient(true);
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchClientId: '', twitchClientSecret: '' }),
        client,
        mockDiscord(),
        { testHlsUrl: 'http://127.0.0.1:8899/index.m3u8' },
      );

      // Act
      await checker.check();

      // Assert
      expect(checker.enabled).toBe(true);
      expect(checker.getStatus().channelLive).toBe(true);
      expect(fetchMock).not.toHaveBeenCalled();
    });

    it('stays off without credentials when no test HLS URL is set', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchClientId: '', twitchClientSecret: '' }),
        mockClient(),
        mockDiscord(),
        { testHlsUrl: '' },
      );
      expect(checker.enabled).toBe(false);
    });

    it('returns true when all credentials are present and toggle is on', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig(),
        mockClient(),
        mockDiscord(),
      );
      expect(checker.enabled).toBe(true);
    });
  });

  describe('getStatus()', () => {
    it('returns correct initial structure', () => {
      const checker = new TwitchLivenessChecker(makeConfig(), mockClient(), mockDiscord());
      const status = checker.getStatus();
      expect(status).toEqual({
        enabled: true,
        channelLive: null,
        encoderAlive: null,
        consecutiveMismatches: 0,
        mismatchThreshold: 3,
        lastCheckAt: null,
        lastRestartAt: null,
        restartCount: 0,
        readback: { lastCheckAt: null, lastDiff: null, state: 'off' },
      });
    });

    it('reports enabled false when disabled', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchLivenessEnabled: false }),
        mockClient(),
        mockDiscord(),
      );
      expect(checker.getStatus().enabled).toBe(false);
    });
  });

  describe('start() / stop()', () => {
    it('start sets up timers and stop clears them', () => {
      const checker = new TwitchLivenessChecker(makeConfig(), mockClient(), mockDiscord());
      checker.start();
      // Timer should be scheduled
      checker.stop();
      // No errors — timers cleared
    });

    it('start does nothing when disabled', () => {
      const checker = new TwitchLivenessChecker(
        makeConfig({ twitchLivenessEnabled: false }),
        mockClient(),
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

    it('increments on mismatch (encoder up, Twitch offline)', async () => {
      const client = mockClient(true);
      setupFetch({ data: [] }); // channel not live

      const checker = new TwitchLivenessChecker(makeConfig(), client, mockDiscord());
      await checker.check();

      expect(checker.getStatus().consecutiveMismatches).toBe(1);
    });

    it('resets on non-mismatch (both streaming)', async () => {
      const client = mockClient(true);
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

      const checker = new TwitchLivenessChecker(makeConfig(), client, mockDiscord());
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

    it('resets on non-mismatch (encoder down)', async () => {
      const client = mockClient(false);
      setupFetch({ data: [] });

      const checker = new TwitchLivenessChecker(makeConfig(), client, mockDiscord());
      await checker.check();

      expect(checker.getStatus().consecutiveMismatches).toBe(0);
    });

    it('triggers restart at threshold (3 mismatches)', async () => {
      const client = mockClient(true);
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

      const checker = new TwitchLivenessChecker(makeConfig(), client, discord);

      await checker.check();
      expect(checker.getStatus().consecutiveMismatches).toBe(1);
      await checker.check();
      expect(checker.getStatus().consecutiveMismatches).toBe(2);
      expect(client.control).not.toHaveBeenCalled();

      await checker.check();

      expect(checker.getStatus().consecutiveMismatches).toBe(0);
      expect(client.control).toHaveBeenCalledOnce();
      expect(client.control).toHaveBeenCalledWith('restart-encoder', undefined, expect.any(String));
      expect(discord.notifyTwitchMismatch).toHaveBeenCalled();
      expect(discord.notifyTwitchRestart).toHaveBeenCalled();
      expect(checker.getStatus().restartCount).toBe(1);
    });
  });

  it('names a missing stream key in plain words when the encoder cannot restart', async () => {
    const client = mockClient(true);
    client.control.mockResolvedValue({ ok: false, error: 'needs-stream-key: no encoder runs until a reload brings a working key' });
    const discord = mockDiscord();
    const fetchMock = vi.fn();
    fetchMock.mockResolvedValueOnce({ ok: true, json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }) });
    for (let i = 0; i < 3; i++) fetchMock.mockResolvedValueOnce({ ok: true, status: 200, json: () => Promise.resolve({ data: [] }) });
    vi.stubGlobal('fetch', fetchMock);
    const checker = new TwitchLivenessChecker(makeConfig(), client, discord);

    for (let i = 0; i < 3; i++) await checker.check();

    expect(discord.notifyTwitchRestart).not.toHaveBeenCalled();
    expect(discord.notifyCritical).toHaveBeenCalledWith(expect.stringContaining('no stream key is set'));
    expect(checker.getStatus().restartCount).toBe(0);
  });

  it('treats the supervisor not answering as encoder down', async () => {
    const client = mockClient(true);
    client.status.mockResolvedValue(null);
    vi.stubGlobal('fetch', vi.fn()
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }) })
      .mockResolvedValueOnce({ ok: true, status: 200, json: () => Promise.resolve({ data: [] }) }));
    const checker = new TwitchLivenessChecker(makeConfig(), client, mockDiscord());

    await checker.check();

    expect(checker.getStatus().encoderAlive).toBe(false);
    expect(checker.getStatus().consecutiveMismatches).toBe(0);
  });

  describe('API errors', () => {
    it('does not increment counter on API error', async () => {
      const client = mockClient(true);

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

      const checker = new TwitchLivenessChecker(makeConfig(), client, mockDiscord());
      await checker.check();

      expect(checker.getStatus().consecutiveMismatches).toBe(0);
    });

    it('does not increment counter on fetch exception', async () => {
      const client = mockClient(true);

      // Token succeeds
      const fetchMock = vi.fn();
      fetchMock.mockResolvedValueOnce({
        ok: true,
        json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }),
      });
      // Streams API throws network error
      fetchMock.mockRejectedValueOnce(new Error('network error'));
      vi.stubGlobal('fetch', fetchMock);

      const checker = new TwitchLivenessChecker(makeConfig(), client, mockDiscord());
      await checker.check();

      expect(checker.getStatus().consecutiveMismatches).toBe(0);
    });
  });

  describe('token management', () => {
    it('fetches a token on first check', async () => {
      const client = mockClient(false);

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

      const checker = new TwitchLivenessChecker(makeConfig(), client, mockDiscord());
      await checker.check();

      // First call should be token request
      expect(fetchMock).toHaveBeenCalledTimes(2);
      const tokenCall = fetchMock.mock.calls[0];
      expect(tokenCall[0]).toBe('https://id.twitch.tv/oauth2/token');
    });

    it('reuses cached token for subsequent checks', async () => {
      const client = mockClient(false);

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

      const checker = new TwitchLivenessChecker(makeConfig(), client, mockDiscord());
      await checker.check();
      await checker.check();

      // 2 token + 1 streams = 3 total... wait no:
      // check 1: token(1) + streams(2) = 2 calls
      // check 2: no token (cached) + streams(3) = 1 call
      expect(fetchMock).toHaveBeenCalledTimes(3);
    });

    it('refreshes token on 401 response', async () => {
      const client = mockClient(false);

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

      const checker = new TwitchLivenessChecker(makeConfig(), client, mockDiscord());
      await checker.check();

      expect(fetchMock).toHaveBeenCalledTimes(4);
      expect(checker.getStatus().channelLive).toBe(true);
    });
  });

  describe('encoder restarts from Twitch checks', () => {
    function offlineFetch(checks: number) {
      const fetchMock = vi.fn();
      fetchMock.mockResolvedValueOnce({ ok: true, json: () => Promise.resolve({ access_token: 'tok123', expires_in: 3600 }) });
      for (let i = 0; i < checks; i++) fetchMock.mockResolvedValueOnce({ ok: true, status: 200, json: () => Promise.resolve({ data: [] }) });
      vi.stubGlobal('fetch', fetchMock);
    }

    it('restarts at most once per 30 min and only alerts in between', async () => {
      // Arrange
      const client = mockClient(true);
      const discord = mockDiscord();
      offlineFetch(9);
      const checker = new TwitchLivenessChecker(makeConfig(), client, discord);

      // Act: a restart, then a second mismatch streak inside 30 min
      for (let i = 0; i < 6; i++) await checker.check();

      // Assert
      expect(client.control).toHaveBeenCalledOnce();
      expect(discord.notifyCritical).toHaveBeenCalledOnce();

      // Act: a third streak after 30 min
      vi.advanceTimersByTime(30 * 60 * 1000);
      for (let i = 0; i < 3; i++) await checker.check();

      // Assert
      expect(client.control).toHaveBeenCalledTimes(2);
    });

    it('a third streak inside the window sends no Discord call and goes to the timeline', async () => {
      // Arrange
      const client = mockClient(true);
      const discord = mockDiscord();
      const addEvent = vi.fn();
      offlineFetch(9);
      const checker = new TwitchLivenessChecker(makeConfig(), client, discord, { addEvent });
      for (let i = 0; i < 6; i++) await checker.check();
      const criticalsAfterSecond = vi.mocked(discord.notifyCritical).mock.calls.length;

      // Act
      for (let i = 0; i < 3; i++) await checker.check();

      // Assert
      expect(criticalsAfterSecond).toBe(1);
      expect(discord.notifyCritical).toHaveBeenCalledOnce();
      expect(client.control).toHaveBeenCalledOnce();
      expect(addEvent).toHaveBeenCalledWith(expect.stringContaining('already restarted'));
    });

    it('sends exactly one critical per 30 min window across Helix and read-back triggers', async () => {
      // Arrange
      const client = mockClient(true);
      const discord = mockDiscord();
      const addEvent = vi.fn();
      offlineFetch(12);
      const checker = new TwitchLivenessChecker(makeConfig(), client, discord, { addEvent });
      const discordCalls = (): number =>
        vi.mocked(discord.notifyCritical).mock.calls.length +
        vi.mocked(discord.notifyTwitchMismatch).mock.calls.length +
        vi.mocked(discord.notifyTwitchRestart).mock.calls.length;

      // Act: window 1 opens with a Helix restart, then four more triggers inside it
      for (let i = 0; i < 3; i++) await checker.check(); // restart: mismatch + restart notices
      await checker.handleFrozen(); // first cap hit: one critical
      for (let i = 0; i < 3; i++) await checker.check(); // timeline only
      await checker.handleFrozen(); // timeline only
      const inFirstWindow = discordCalls();
      // window 2 opens with a read-back restart, then one Helix streak inside it
      vi.advanceTimersByTime(30 * 60 * 1000);
      await checker.handleFrozen(); // restart, no Discord notice
      for (let i = 0; i < 3; i++) await checker.check(); // first cap hit: one critical
      await checker.handleFrozen(); // timeline only

      // Assert
      expect(inFirstWindow).toBe(3);
      expect(discordCalls()).toBe(4);
      expect(discord.notifyCritical).toHaveBeenCalledTimes(2);
      expect(client.control).toHaveBeenCalledTimes(2);
    });

    it('a frozen read-back restarts the encoder and shares the 30 min limit', async () => {
      const client = mockClient(true);
      const discord = mockDiscord();
      const addEvent = vi.fn();
      const checker = new TwitchLivenessChecker(makeConfig(), client, discord, { addEvent });

      await checker.handleFrozen();
      await checker.handleFrozen();

      expect(client.control).toHaveBeenCalledOnce();
      expect(client.control).toHaveBeenCalledWith('restart-encoder', undefined, expect.stringContaining('frozen'));
      expect(addEvent).toHaveBeenCalledWith(expect.stringContaining('frozen'));
      expect(discord.notifyCritical).toHaveBeenCalledOnce();
    });

    it('an unavailable read-back only records a timeline event', () => {
      const client = mockClient(true);
      const discord = mockDiscord();
      const addEvent = vi.fn();
      const checker = new TwitchLivenessChecker(makeConfig(), client, discord, { addEvent });

      checker.handleUnavailable('the channel playlist could not be fetched');

      expect(addEvent).toHaveBeenCalledOnce();
      expect(addEvent.mock.calls[0][0]).toContain('the channel playlist could not be fetched');
      expect(client.control).not.toHaveBeenCalled();
      expect(discord.notifyCritical).not.toHaveBeenCalled();
    });

    it('keeps the 30 min limit across a config reload', async () => {
      // Arrange: index.ts passes the same options object to every new checker
      const client = mockClient(true);
      const discord = mockDiscord();
      const opts = { restartClock: { lastAttemptAt: null, alertedAt: null } };
      await new TwitchLivenessChecker(makeConfig(), client, discord, opts).handleFrozen();

      // Act: the cap hit and its critical, then a later hit after another reload
      await new TwitchLivenessChecker(makeConfig(), client, discord, opts).handleFrozen();
      await new TwitchLivenessChecker(makeConfig(), client, discord, opts).handleFrozen();

      // Assert
      expect(client.control).toHaveBeenCalledOnce();
      expect(discord.notifyCritical).toHaveBeenCalledOnce();
    });

    it('a still whole frame only records a timeline event', () => {
      const client = mockClient(true);
      const discord = mockDiscord();
      const addEvent = vi.fn();
      const checker = new TwitchLivenessChecker(makeConfig(), client, discord, { addEvent });

      checker.handleStill();

      expect(addEvent).toHaveBeenCalledOnce();
      expect(client.control).not.toHaveBeenCalled();
      expect(discord.notifyCritical).not.toHaveBeenCalled();
    });

    it('runs no read-back without an ffmpeg path', () => {
      const checker = new TwitchLivenessChecker(makeConfig(), mockClient(true), mockDiscord(), { ffmpegPath: null });
      expect(checker.getStatus().readback.state).toBe('off');
    });
  });
});
