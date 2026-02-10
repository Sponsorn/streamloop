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
    obsDisconnect: true,
    obsReconnect: true,
  },
  templates: {
    error: 'Playback error **{errorCode}** on video #{videoIndex} (`{videoId}`)\nRetry attempt: {attempt}',
    skip: 'Skipping video #{videoIndex} (`{videoId}`)\nReason: {reason}',
    recovery: 'Recovery action: **{step}**',
    critical: '**CRITICAL:** {message}',
    resume: 'Playback resumed at video #{videoIndex} (`{videoId}`)',
    obsDisconnect: 'OBS disconnected — attempting to reconnect',
    obsReconnect: 'OBS reconnected',
  },
};

function makeConfig(overrides: Partial<AppConfig> = {}, discordOverrides: Partial<DiscordConfig> = {}): AppConfig {
  return {
    port: 7654,
    obsWebsocketUrl: '',
    obsWebsocketPassword: '',
    obsBrowserSourceName: 'Source',
    playlists: [{ id: 'PL123' }],
    discord: {
      ...defaultDiscord,
      ...discordOverrides,
      events: { ...defaultDiscord.events, ...discordOverrides.events },
      templates: { ...defaultDiscord.templates, ...discordOverrides.templates },
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
    await notifier.notifyError(1, 'abc', 5, 1);
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

  it('respects event toggle - disabled obsDisconnect does not send', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = makeNotifier({
      webhookUrl: 'https://discord.com/api/webhooks/test',
      events: { ...defaultDiscord.events, obsDisconnect: false },
    });
    await notifier.notifyObsDisconnect();
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
    await notifier.notifyError(3, 'abc123', 150, 2);
    await vi.advanceTimersByTimeAsync(30000);
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].fields).toBeDefined();
    expect(body.embeds[0].fields).toHaveLength(3);
    expect(body.embeds[0].fields[0].name).toBe('Error Code');
    expect(body.embeds[0].fields[0].value).toBe('150');
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
});
