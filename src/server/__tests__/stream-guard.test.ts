import { describe, it, expect } from 'vitest';
import { encoderIsLive, streamloop3IsStreaming } from '../stream-guard.js';

function respond(status: number, body: string): typeof fetch {
  return (async () => new Response(body, { status })) as typeof fetch;
}

describe('encoderIsLive', () => {
  it('is true only when encoder.alive is exactly true', () => {
    expect(encoderIsLive({ encoder: { alive: true } })).toBe(true);
    for (const body of [{ encoder: { alive: false } }, { encoder: {} }, {}, null, 'alive', { encoder: { alive: 'true' } }]) {
      expect(encoderIsLive(body)).toBe(false);
    }
  });
});

describe('streamloop3IsStreaming', () => {
  it('reports streaming when the supervisor says its encoder is alive', async () => {
    expect(await streamloop3IsStreaming(respond(200, JSON.stringify({ encoder: { alive: true } })))).toBe(true);
  });

  it('allows the stream when the encoder is not alive', async () => {
    expect(await streamloop3IsStreaming(respond(200, JSON.stringify({ encoder: { alive: false } })))).toBe(false);
  });

  it('allows the stream when an unrelated program answers on the port', async () => {
    expect(await streamloop3IsStreaming(respond(404, '<html>Not found</html>'))).toBe(false);
    expect(await streamloop3IsStreaming(respond(200, 'hello'))).toBe(false);
  });

  it('allows the stream when nothing listens on the port', async () => {
    const refused = (async () => { throw new TypeError('fetch failed'); }) as typeof fetch;
    expect(await streamloop3IsStreaming(refused)).toBe(false);
  });

  it('allows the stream when the port hangs past the timeout', async () => {
    // Arrange: a fetch that only settles when its signal aborts
    const hang = ((_url: string, init?: RequestInit) => new Promise((_resolve, reject) => {
      init?.signal?.addEventListener('abort', () => reject(new Error('aborted')));
    })) as typeof fetch;
    const started = Date.now();
    // Act
    const result = await streamloop3IsStreaming(hang, 50);
    // Assert
    expect(result).toBe(false);
    expect(Date.now() - started).toBeLessThan(500);
  });
});
