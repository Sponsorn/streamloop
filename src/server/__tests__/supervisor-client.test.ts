import { describe, it, expect } from 'vitest';
import { SupervisorClient, protectStreamKey, type RunFn } from '../supervisor-client.js';

// Shape of the supervisor's status() in supervisor/src/main.rs (head merged with body).
const FULL_STATUS = {
  state: 'playing',
  reason: null,
  streamKeyProblem: null,
  playing: { index: 3, id: 'abc123', title: 'A video', source: 'file', position: 12.345, duration: 300, spareReady: false },
  timeline: 1234.5,
  readAheadSeconds: 18.2,
  cacheBytes: 1000,
  downloading: { index: 4, percent: 40 },
  encoder: { alive: true, restarts: 1, generation: 2, mediaTime: 99.1, speed: 1.0, cpuPercent: 12.5 },
  cpu: { encoder: 12.5, feeder: 3, system: 30 },
  tokenServer: { state: 'running', restarts: 0 },
  recovery: { step: 'none', retries: 0 },
  watchdog: { last: null },
  playlistCount: 50,
  hold: null,
  youtube: { breaker: 'closed', retryAt: null, level: 0, failures: 0 },
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(typeof body === 'string' ? body : JSON.stringify(body), { status });
}

function client(fetchImpl: typeof fetch, timeoutMs = 50): SupervisorClient {
  return new SupervisorClient({ fetchImpl, timeoutMs });
}

describe('SupervisorClient.status', () => {
  it('parses a full status sample', async () => {
    const c = client((async () => jsonResponse(FULL_STATUS)) as typeof fetch);
    const s = await c.status();
    expect(s?.state).toBe('playing');
    expect(s?.playing?.title).toBe('A video');
    expect(s?.encoder?.alive).toBe(true);
    expect(s?.youtube?.breaker).toBe('closed');
    expect(s?.playlistCount).toBe(50);
  });

  it('tolerates missing and unknown fields', async () => {
    const c = client((async () => jsonResponse({ state: 'starting', somethingNew: 1 })) as typeof fetch);
    const s = await c.status();
    expect(s?.state).toBe('starting');
    expect(s?.encoder).toBeUndefined();
  });

  it('returns null when the connection is refused', async () => {
    const c = client((async () => { throw new TypeError('fetch failed'); }) as typeof fetch);
    expect(await c.status()).toBeNull();
  });

  it('returns null when the supervisor does not answer in time', async () => {
    const hang = ((_u: unknown, init?: RequestInit) =>
      new Promise((_res, rej) => init?.signal?.addEventListener('abort', () => rej(new Error('aborted'))))) as typeof fetch;
    expect(await client(hang).status()).toBeNull();
  });

  it('returns null on a non-200 answer', async () => {
    const c = client((async () => jsonResponse({ error: 'x' }, 500)) as typeof fetch);
    expect(await c.status()).toBeNull();
  });

  it('returns null when the body is not JSON', async () => {
    const c = client((async () => jsonResponse('<html>')) as typeof fetch);
    expect(await c.status()).toBeNull();
  });

  it('returns null when the body is JSON but not an object', async () => {
    const c = client((async () => jsonResponse('null')) as typeof fetch);
    expect(await c.status()).toBeNull();
  });
});

describe('SupervisorClient.control', () => {
  function recorder(res: Response) {
    const calls: { url: string; init: RequestInit }[] = [];
    const f = (async (url: string, init: RequestInit) => { calls.push({ url, init }); return res; }) as unknown as typeof fetch;
    return { calls, f };
  }

  it('posts the exact body for jump', async () => {
    const { calls, f } = recorder(jsonResponse({ accepted: true }));
    const r = await client(f).control('jump', 7);
    expect(r).toEqual({ ok: true });
    expect(calls[0].url).toBe('http://127.0.0.1:4417/control');
    expect(calls[0].init.method).toBe('POST');
    expect(JSON.parse(calls[0].init.body as string)).toEqual({ cmd: 'jump', index: 7 });
  });

  it('omits index when none is given', async () => {
    const { calls, f } = recorder(jsonResponse({ accepted: true }));
    await client(f).control('next');
    expect(calls[0].init.body).toBe('{"cmd":"next"}');
  });

  it('sends the caller reason with restart-encoder', async () => {
    const { calls, f } = recorder(jsonResponse({ accepted: true }));
    await client(f).control('restart-encoder', undefined, 'Twitch shows the channel offline');
    expect(JSON.parse(calls[0].init.body as string)).toEqual({ cmd: 'restart-encoder', reason: 'Twitch shows the channel offline' });
  });

  it('reports alreadyRestarting for a pending encoder restart', async () => {
    const { f } = recorder(jsonResponse({ accepted: true, alreadyRestarting: true }));
    expect(await client(f).control('restart-encoder')).toEqual({ ok: true, alreadyRestarting: true });
  });

  it('returns the error text on a 400', async () => {
    const { f } = recorder(jsonResponse({ error: 'needs-stream-key: x' }, 400));
    expect(await client(f).control('next')).toEqual({ ok: false, error: 'needs-stream-key: x' });
  });

  it('resolves with an error when the network fails', async () => {
    const f = (async () => { throw new TypeError('fetch failed'); }) as typeof fetch;
    const r = await client(f).control('next');
    expect(r.ok).toBe(false);
    expect(r.error).toContain('fetch failed');
  });
});

describe('SupervisorClient.reload and shutdown', () => {
  it('returns restartRequired from reload', async () => {
    const f = (async () => jsonResponse({ applied: true, restartRequired: true })) as typeof fetch;
    expect(await client(f).reload()).toEqual({ ok: true, restartRequired: true });
  });

  it('returns the error from a rejected reload', async () => {
    const f = (async () => jsonResponse({ error: 'bad config' }, 400)) as typeof fetch;
    expect(await client(f).reload()).toEqual({ ok: false, error: 'bad config' });
  });

  it('shutdown is true on 200 and false on failure', async () => {
    expect(await client((async () => jsonResponse({})) as typeof fetch).shutdown()).toBe(true);
    expect(await client((async () => { throw new Error('x'); }) as typeof fetch).shutdown()).toBe(false);
  });
});

describe('protectStreamKey', () => {
  const KEY = 'live_123456_SECRETsecret';

  it('passes the key only on stdin and returns the blob', async () => {
    const seen: { exe: string; args: string[]; stdin: string }[] = [];
    const run: RunFn = async (exe, args, stdin) => {
      seen.push({ exe, args, stdin });
      return { code: 0, stdout: 'QkxPQg==\n', stderr: '' };
    };
    const blob = await protectStreamKey('sup.exe', `  ${KEY}\n`, run);
    expect(blob).toBe('QkxPQg==');
    expect(seen[0]).toEqual({ exe: 'sup.exe', args: ['protect'], stdin: KEY });
  });

  it('rejects an empty key without spawning', async () => {
    let spawned = false;
    const run: RunFn = async () => { spawned = true; return { code: 0, stdout: 'x', stderr: '' }; };
    await expect(protectStreamKey('sup.exe', '  \n', run)).rejects.toThrow(/empty/i);
    expect(spawned).toBe(false);
  });

  it('keeps the key out of the error text', async () => {
    const run: RunFn = async () => ({ code: 1, stdout: '', stderr: `cannot protect ${KEY} here` });
    const err = await protectStreamKey('sup.exe', KEY, run).catch((e: Error) => e);
    expect((err as Error).message).toContain('exit 1');
    expect((err as Error).message).not.toContain(KEY);
  });

  it('keeps the key out of the error when the run itself throws', async () => {
    const run: RunFn = async () => { throw new Error(`spawn failed for ${KEY}`); };
    const err = await protectStreamKey('sup.exe', KEY, run).catch((e: Error) => e);
    expect((err as Error).message).not.toContain(KEY);
  });

  it('rejects when the supervisor prints no blob', async () => {
    const run: RunFn = async () => ({ code: 0, stdout: '\n', stderr: '' });
    await expect(protectStreamKey('sup.exe', KEY, run)).rejects.toThrow();
  });
});
