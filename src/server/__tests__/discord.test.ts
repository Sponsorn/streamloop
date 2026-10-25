import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { DiscordNotifier } from '../discord.js';
import type { AppConfig, DiscordConfig } from '../types.js';

const defaultDiscord: DiscordConfig = {
  webhookUrl: '',
  botName: '',
  avatarUrl: '',
  rolePing: '',
  events: {
    error: true,
    skip: true,
    recovery: true,
    critical: true,
    resume: true,
    streamDrop: true,
    streamRestart: true, outputFrozen: true, twitchMismatch: true, twitchRestart: true,
  },
  templates: {
    error: 'Playback error **{errorCode}** on video #{videoIndex} (`{videoId}`)\nRetry attempt: {attempt}',
    skip: 'Skipping video #{videoIndex} (`{videoId}`)\nReason: {reason}',
    recovery: 'Recovery action: **{step}**',
    critical: '**CRITICAL:** {message}',
    resume: 'Playback resumed at video #{videoIndex} (`{videoId}`)',
    streamDrop: 'Stream dropped (attempt {attempt}/{maxAttempts})',
    streamRestart: 'Stream restarted after {attempts} attempt(s)',
    outputFrozen: 'Frozen ({kind}) on #{videoIndex}',
    twitchMismatch: 'Twitch mismatch on {channel}',
    twitchRestart: 'Stream restarted after Twitch mismatch on {channel}',
  },
};

function makeConfig(overrides: Partial<AppConfig> = {}, discordOverrides: Partial<DiscordConfig> = {}): AppConfig {
  return {
    port: 7654,
    playlists: [{ id: 'PL123' }],
    discord: {
      ...defaultDiscord,
      ...discordOverrides,
      events: { ...defaultDiscord.events, ...discordOverrides.events },
      templates: { ...defaultDiscord.templates, ...discordOverrides.templates },
    },
    autoUpdateCheck: true,
    autoUpdateYtdlp: true,
    autoOpenAdmin: true,
    updateCheckIntervalMs: 21600000,
    twitchClientId: '',
    twitchClientSecret: '',
    twitchChannel: '',
    twitchLivenessEnabled: false,
    twitchPollIntervalMs: 60000,
    ytdlCookiesFromBrowser: '',
    supervisor: { ingestUrl: '', streamKeyProtected: '', encoder: 'libx264', cacheBudgetMb: 300, readAheadSeconds: 20, playerClient: '', overlay: true, freezeSeconds: 20, silenceSeconds: 30 },
    ...overrides,
  };
}

function makeNotifier(discordOverrides: Partial<DiscordConfig> = {}) {
  return new DiscordNotifier(
    makeConfig({}, discordOverrides),
    '1.0.0',
    () => 3600000,
    'http://localhost:7654/admin',
  );
}

describe('DiscordNotifier', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('does nothing when webhook URL is empty', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch');
    const notifier = makeNotifier();
    await notifier.send('test message');
    await vi.advanceTimersByTimeAsync(60000);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('sends a webhook after debounce window', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    await notifier.send('Hello', 'info');
    expect(fetchSpy).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(30000);
    expect(fetchSpy).toHaveBeenCalledOnce();
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].description).toBe('Hello');
  });

  it('flushes immediately on error level', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    await notifier.notifyCritical('All steps exhausted');
    expect(fetchSpy).toHaveBeenCalledOnce();
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].description).toContain('CRITICAL');
  });

  it('batches multiple messages into one embed', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    await notifier.send('First', 'info');
    await notifier.send('Second', 'warn');
    expect(fetchSpy).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(30000);
    expect(fetchSpy).toHaveBeenCalledOnce();
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].description).toContain('First');
    expect(body.embeds[0].description).toContain('Second');
    expect(body.embeds[0].footer.text).toContain('2 events');
    // Highest level is warn → yellow color
    expect(body.embeds[0].color).toBe(16776960);
  });

  it('error message flushes queued info/warn messages too', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    await notifier.send('queued info', 'info');
    await notifier.send('queued warn', 'warn');
    await notifier.notifyCritical('urgent');
    expect(fetchSpy).toHaveBeenCalledOnce();
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].description).toContain('queued info');
    expect(body.embeds[0].description).toContain('urgent');
    expect(body.embeds[0].footer.text).toContain('3 events');
  });

  it('handles fetch failure gracefully', async () => {
    vi.spyOn(globalThis, 'fetch').mockRejectedValue(new Error('network'));
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    // Should not throw
    await notifier.send('test', 'error');
  });

  // --- New tests for templates, toggles, identity, footer, fields, role ping ---

  it('renders template with variables', () => {
    const notifier = makeNotifier();
    const result = notifier.renderTemplate('Error {errorCode} on video #{videoIndex}', {
      errorCode: 150,
      videoIndex: 3,
    });
    expect(result).toBe('Error 150 on video #3');
  });

  it('leaves unknown placeholders intact', () => {
    const notifier = makeNotifier();
    const result = notifier.renderTemplate('Hello {name}, {unknown}', { name: 'world' });
    expect(result).toBe('Hello world, {unknown}');
  });

  it('respects event toggle - disabled event does not send', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({
      webhookUrl: 'https://discord.com/api/webhooks/test',
      events: { ...defaultDiscord.events, error: false },
    });
    await notifier.notifyError(1, 'abc', 'loading failed', 1);
    await vi.advanceTimersByTimeAsync(60000);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('respects event toggle - disabled skip does not send', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({
      webhookUrl: 'https://discord.com/api/webhooks/test',
      events: { ...defaultDiscord.events, skip: false },
    });
    await notifier.notifySkip(1, 'abc', 'test');
    await vi.advanceTimersByTimeAsync(60000);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('includes bot identity in payload when set', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({
      webhookUrl: 'https://discord.com/api/webhooks/test',
      botName: 'MyBot',
      avatarUrl: 'https://example.com/avatar.png',
    });
    await notifier.send('test', 'error');
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.username).toBe('MyBot');
    expect(body.avatar_url).toBe('https://example.com/avatar.png');
  });

  it('does not include bot identity when empty', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    await notifier.send('test', 'error');
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.username).toBeUndefined();
    expect(body.avatar_url).toBeUndefined();
  });

  it('includes footer with dashboard URL, uptime, and version', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    await notifier.send('test', 'error');
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    const footer = body.embeds[0].footer.text;
    expect(footer).toContain('Dashboard: http://localhost:7654/admin');
    expect(footer).toContain('Uptime:');
    expect(footer).toContain('v1.0.0');
  });

  it('includes structured fields on error notification', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    // notifyError uses warn level, need to flush manually
    await notifier.notifyError(3, 'abc123', 'HTTP error 403', 2);
    await vi.advanceTimersByTimeAsync(30000);
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].fields).toBeDefined();
    expect(body.embeds[0].fields).toHaveLength(3);
    expect(body.embeds[0].fields[0].name).toBe('Reason');
    expect(body.embeds[0].fields[0].value).toBe('HTTP error 403');
  });

  it('includes structured fields on critical notification', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    await notifier.notifyCritical('All recovery steps exhausted');
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].fields).toBeDefined();
    expect(body.embeds[0].fields[0].name).toBe('Status');
  });

  it('adds role ping on critical (error-level) messages', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({
      webhookUrl: 'https://discord.com/api/webhooks/test',
      rolePing: '123456789',
    });
    await notifier.notifyCritical('All steps exhausted');
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.content).toBe('<@&123456789>');
  });

  it('does not add role ping on non-critical messages', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({
      webhookUrl: 'https://discord.com/api/webhooks/test',
      rolePing: '123456789',
    });
    await notifier.send('info message', 'info');
    await vi.advanceTimersByTimeAsync(30000);
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.content).toBeUndefined();
  });

  it('uses custom template for notifications', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({
      webhookUrl: 'https://discord.com/api/webhooks/test',
      templates: {
        ...defaultDiscord.templates,
        recovery: 'Custom recovery: {step} happened!',
      },
    });
    await notifier.notifyRecovery('refreshSource');
    await vi.advanceTimersByTimeAsync(30000);
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].description).toBe('Custom recovery: refreshSource happened!');
  });

  it('renders the output-frozen template with kind and video', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(null, { status: 200 }));
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    await notifier.notifyOutputFrozen('freeze', 4);
    await vi.advanceTimersByTimeAsync(30000);
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].description).toBe('Frozen (freeze) on #4');
  });

  it('skips the output-frozen alert when its event is off', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(null, { status: 200 }));
    const notifier = makeNotifier({
      webhookUrl: 'https://discord.com/api/webhooks/test',
      events: { ...defaultDiscord.events, outputFrozen: false },
    });
    await notifier.notifyOutputFrozen('silence', 1);
    await vi.advanceTimersByTimeAsync(30000);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('sends a described supervisor event with its template and level', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(null, { status: 200 }));
    const notifier = makeNotifier({ webhookUrl: 'https://discord.com/api/webhooks/test' });
    await notifier.notifyEvent({ kind: 'critical', vars: { message: 'All videos failed' }, level: 'error' });
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].description).toBe('**CRITICAL:** All videos failed');
    expect(body.embeds[0].color).toBe(15158332);
  });

  it('skips a described event whose toggle is off', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(null, { status: 200 }));
    const notifier = makeNotifier({
      webhookUrl: 'https://discord.com/api/webhooks/test',
      events: { ...defaultDiscord.events, skip: false },
    });
    await notifier.notifyEvent({ kind: 'skip', vars: { videoIndex: 1, videoId: 'x', reason: 'r' }, level: 'warn' });
    await vi.advanceTimersByTimeAsync(30000);
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
