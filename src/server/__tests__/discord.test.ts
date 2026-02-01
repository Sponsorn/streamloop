import { describe, it, expect, vi, beforeEach } from 'vitest';
import { DiscordNotifier } from '../discord.js';
import type { AppConfig } from '../types.js';

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

describe('DiscordNotifier', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it('does nothing when webhook URL is empty', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch');
    const notifier = new DiscordNotifier(makeConfig());
    await notifier.send('test message');
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('sends a webhook when URL is configured', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = new DiscordNotifier(makeConfig({
      discordWebhookUrl: 'https://discord.com/api/webhooks/test',
    }));
    await notifier.send('Hello', 'info');
    expect(fetchSpy).toHaveBeenCalledOnce();
    const [url, opts] = fetchSpy.mock.calls[0];
    expect(url).toBe('https://discord.com/api/webhooks/test');
    const body = JSON.parse((opts as any).body);
    expect(body.embeds[0].description).toBe('Hello');
  });

  it('notifyError sends correct content', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(null, { status: 200 }),
    );
    const notifier = new DiscordNotifier(makeConfig({
      discordWebhookUrl: 'https://discord.com/api/webhooks/test',
    }));
    await notifier.notifyError(3, 'vid123', 150, 2);
    const body = JSON.parse((fetchSpy.mock.calls[0][1] as any).body);
    expect(body.embeds[0].description).toContain('150');
    expect(body.embeds[0].description).toContain('vid123');
  });

  it('handles fetch failure gracefully', async () => {
    vi.spyOn(globalThis, 'fetch').mockRejectedValue(new Error('network'));
    const notifier = new DiscordNotifier(makeConfig({
      discordWebhookUrl: 'https://discord.com/api/webhooks/test',
    }));
    // Should not throw
    await notifier.send('test');
  });
});
