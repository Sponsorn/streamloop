import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import net from 'net';
import { mkdtempSync, rmSync, writeFileSync, readdirSync, utimesSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { MpvClient, pruneMpvLogs } from '../mpv-client.js';

vi.mock('../logger.js', () => ({
  logger: {
    info: vi.fn(),
    warn: vi.fn(),
    error: vi.fn(),
    debug: vi.fn(),
    child: vi.fn(() => ({
      info: vi.fn(),
      warn: vi.fn(),
      error: vi.fn(),
      debug: vi.fn(),
    })),
  },
}));

const TEST_PIPE = `\\\\.\\pipe\\mpv-test-${process.pid}-${Date.now()}`;

function createMockPipeServer(): net.Server {
  return net.createServer();
}

function listenOnPipe(server: net.Server, pipePath: string): Promise<void> {
  return new Promise((resolve, reject) => {
    server.listen(pipePath, () => resolve());
    server.on('error', reject);
  });
}

describe('MpvClient', () => {
  let server: net.Server;
  let client: MpvClient;
  let serverSocket: net.Socket | null = null;
  let pipePath: string;

  beforeEach(async () => {
    // Use unique pipe per test to avoid collisions
    pipePath = `${TEST_PIPE}-${Math.random().toString(36).slice(2)}`;
    server = createMockPipeServer();
    server.on('connection', (socket) => {
      serverSocket = socket;
    });
    await listenOnPipe(server, pipePath);
  });

  afterEach(async () => {
    if (client) {
      try { client.disconnect(); } catch {}
    }
    serverSocket?.destroy();
    serverSocket = null;
    await new Promise<void>((resolve) => {
      server.close(() => resolve());
    });
  });

  function createClient(overrides: Record<string, unknown> = {}) {
    client = new MpvClient({
      pipePath,
      spawn: false,
      ...overrides,
    });
    return client;
  }

  /** Wait for server to receive a connection */
  function waitForServerConnection(timeout = 5000): Promise<net.Socket> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Timed out waiting for connection')), timeout);
      if (serverSocket) {
        clearTimeout(timer);
        resolve(serverSocket);
        return;
      }
      server.once('connection', (socket) => {
        clearTimeout(timer);
        resolve(socket);
      });
    });
  }

  /** Send a line from the mock server to the client */
  function serverSend(socket: net.Socket, data: object): void {
    socket.write(JSON.stringify(data) + '\n');
  }

  /** Read a full line from the client (newline-delimited JSON) */
  function readLine(socket: net.Socket, timeout = 3000): Promise<object> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Timed out waiting for data')), timeout);
      let buffer = '';
      const onData = (chunk: Buffer) => {
        buffer += chunk.toString();
        const newlineIdx = buffer.indexOf('\n');
        if (newlineIdx !== -1) {
          clearTimeout(timer);
          socket.off('data', onData);
          const line = buffer.slice(0, newlineIdx);
          resolve(JSON.parse(line));
        }
      };
      socket.on('data', onData);
    });
  }

  describe('connect', () => {
    it('should connect to the named pipe and emit connected', async () => {
      const c = createClient();
      const connectedPromise = new Promise<void>((resolve) => c.on('connected', resolve));
      await c.connect();
      await connectedPromise;
      expect(c.isConnected()).toBe(true);
    });

    it('should retry connection when pipe is not yet available', async () => {
      // Close the server first so initial attempts fail
      const closedPipe = `${TEST_PIPE}-closed-${Math.random().toString(36).slice(2)}`;
      const c = createClient({ pipePath: closedPipe });

      // Start a server on that pipe after 600ms (first retry at 500ms will fail, second at 1000ms should succeed)
      const delayedServer = createMockPipeServer();
      setTimeout(async () => {
        await listenOnPipe(delayedServer, closedPipe);
      }, 800);

      await c.connect();
      expect(c.isConnected()).toBe(true);

      c.disconnect();
      await new Promise<void>((resolve) => delayedServer.close(() => resolve()));
    });

    it('should fail after max retries', async () => {
      const noSuchPipe = `\\\\.\\pipe\\mpv-test-nonexistent-${Date.now()}`;
      const c = createClient({ pipePath: noSuchPipe });

      await expect(c.connect(3)).rejects.toThrow(/Failed to connect.*after 3 attempts/);
      expect(c.isConnected()).toBe(false);
    });
  });

  describe('commands', () => {
    it('should send JSON commands with request_id and resolve on success', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      // Fire command, read it from server, respond
      const cmdPromise = c.command('get_property', 'volume');
      const msg = await readLine(sock) as any;

      expect(msg.command).toEqual(['get_property', 'volume']);
      expect(typeof msg.request_id).toBe('number');

      serverSend(sock, { error: 'success', data: 80, request_id: msg.request_id });
      const result = await cmdPromise;
      expect(result).toBe(80);
    });

    it('should reject on mpv error response', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const cmdPromise = c.command('set_property', 'nosuch', 42);
      const msg = await readLine(sock) as any;

      serverSend(sock, { error: 'property not found', request_id: msg.request_id });
      await expect(cmdPromise).rejects.toThrow('property not found');
    });

    it('should correlate multiple in-flight commands by request_id', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const p1 = c.command('get_property', 'volume');
      const p2 = c.command('get_property', 'pause');

      const msg1 = await readLine(sock) as any;
      const msg2 = await readLine(sock) as any;

      // Respond in reverse order
      serverSend(sock, { error: 'success', data: true, request_id: msg2.request_id });
      serverSend(sock, { error: 'success', data: 75, request_id: msg1.request_id });

      expect(await p2).toBe(true);
      expect(await p1).toBe(75);
    });
  });

  describe('events', () => {
    it('should emit fileStarted on start-file event', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const eventPromise = new Promise<void>((resolve) => c.on('fileStarted', resolve));
      serverSend(sock, { event: 'start-file' });
      await eventPromise;
    });

    it('should emit fileEnded with reason on end-file event', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const eventPromise = new Promise<string>((resolve) => c.on('fileEnded', resolve));
      serverSend(sock, { event: 'end-file', reason: 'eof' });
      const reason = await eventPromise;
      expect(reason).toBe('eof');
    });

    it('should emit fileEnded with reason and file_error on end-file event', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const eventPromise = new Promise<[string, string | undefined]>((resolve) => {
        c.on('fileEnded', (reason: string, fileError?: string) => resolve([reason, fileError]));
      });
      serverSend(sock, { event: 'end-file', reason: 'error', file_error: 'loading failed' });
      const [reason, fileError] = await eventPromise;
      expect(reason).toBe('error');
      expect(fileError).toBe('loading failed');
    });

    it('should emit fileEnded with undefined file_error when not present', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const eventPromise = new Promise<[string, string | undefined]>((resolve) => {
        c.on('fileEnded', (reason: string, fileError?: string) => resolve([reason, fileError]));
      });
      serverSend(sock, { event: 'end-file', reason: 'eof' });
      const [reason, fileError] = await eventPromise;
      expect(reason).toBe('eof');
      expect(fileError).toBeUndefined();
    });

    it('should emit fileLoaded on file-loaded event', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const eventPromise = new Promise<void>((resolve) => c.on('fileLoaded', resolve));
      serverSend(sock, { event: 'file-loaded' });
      await eventPromise;
    });

    it('should emit shutdown on shutdown event', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const eventPromise = new Promise<void>((resolve) => c.on('shutdown', resolve));
      serverSend(sock, { event: 'shutdown' });
      await eventPromise;
    });

    it('should emit propertyChange on property-change event', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const eventPromise = new Promise<{ name: string; data: unknown }>((resolve) => {
        c.on('propertyChange', (name: string, data: unknown) => resolve({ name, data }));
      });
      serverSend(sock, { event: 'property-change', name: 'volume', data: 50 });
      const { name, data } = await eventPromise;
      expect(name).toBe('volume');
      expect(data).toBe(50);
    });
  });

  describe('buffer handling', () => {
    it('should handle fragmented messages', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const eventPromise = new Promise<void>((resolve) => c.on('fileStarted', resolve));

      // Send a message in two fragments
      const full = JSON.stringify({ event: 'start-file' }) + '\n';
      sock.write(full.slice(0, 10));
      // Small delay to ensure separate TCP segments
      await new Promise((r) => setTimeout(r, 50));
      sock.write(full.slice(10));

      await eventPromise;
    });

    it('should handle multiple messages in one chunk', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const events: string[] = [];
      c.on('fileStarted', () => events.push('start'));
      c.on('fileLoaded', () => events.push('loaded'));

      const combined =
        JSON.stringify({ event: 'start-file' }) + '\n' +
        JSON.stringify({ event: 'file-loaded' }) + '\n';
      sock.write(combined);

      // Wait for both events
      await new Promise((r) => setTimeout(r, 100));
      expect(events).toEqual(['start', 'loaded']);
    });
  });

  describe('disconnect', () => {
    it('should reject pending commands on disconnect', async () => {
      const c = createClient();
      await c.connect();
      await waitForServerConnection();

      const cmdPromise = c.command('get_property', 'volume');
      c.disconnect();

      await expect(cmdPromise).rejects.toThrow(/disconnected/i);
      expect(c.isConnected()).toBe(false);
    });

    it('should emit disconnected on disconnect', async () => {
      const c = createClient();
      await c.connect();
      await waitForServerConnection();

      const disconnectedPromise = new Promise<void>((resolve) => c.on('disconnected', resolve));
      c.disconnect();
      await disconnectedPromise;
    });
  });

  describe('convenience methods', () => {
    it('getProperty should send get_property command', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const p = c.getProperty('volume');
      const msg = await readLine(sock) as any;
      expect(msg.command).toEqual(['get_property', 'volume']);
      serverSend(sock, { error: 'success', data: 80, request_id: msg.request_id });
      expect(await p).toBe(80);
    });

    it('setProperty should send set_property command', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const p = c.setProperty('volume', 50);
      const msg = await readLine(sock) as any;
      expect(msg.command).toEqual(['set_property', 'volume', 50]);
      serverSend(sock, { error: 'success', request_id: msg.request_id });
      await p;
    });

    it('loadFile should send loadfile command', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const p = c.loadFile('https://youtube.com/watch?v=test');
      const msg = await readLine(sock) as any;
      expect(msg.command).toEqual(['loadfile', 'https://youtube.com/watch?v=test']);
      serverSend(sock, { error: 'success', request_id: msg.request_id });
      await p;
    });

    it('loadPlaylist should send loadlist command', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const p = c.loadPlaylist('https://youtube.com/playlist?list=PLtest');
      const msg = await readLine(sock) as any;
      expect(msg.command).toEqual(['loadfile', 'https://youtube.com/playlist?list=PLtest', 'replace']);
      serverSend(sock, { error: 'success', request_id: msg.request_id });
      await p;
    });

    it('pause/play/togglePause should send set_property or cycle commands', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      // pause
      const p1 = c.pause();
      const m1 = await readLine(sock) as any;
      expect(m1.command).toEqual(['set_property', 'pause', true]);
      serverSend(sock, { error: 'success', request_id: m1.request_id });
      await p1;

      // play
      const p2 = c.play();
      const m2 = await readLine(sock) as any;
      expect(m2.command).toEqual(['set_property', 'pause', false]);
      serverSend(sock, { error: 'success', request_id: m2.request_id });
      await p2;

      // togglePause
      const p3 = c.togglePause();
      const m3 = await readLine(sock) as any;
      expect(m3.command).toEqual(['cycle', 'pause']);
      serverSend(sock, { error: 'success', request_id: m3.request_id });
      await p3;
    });

    it('seek should send seek command', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const p = c.seek(30, 'relative');
      const msg = await readLine(sock) as any;
      expect(msg.command).toEqual(['seek', 30, 'relative']);
      serverSend(sock, { error: 'success', request_id: msg.request_id });
      await p;
    });

    it('next/prev should send playlist-next/playlist-prev commands', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const p1 = c.next();
      const m1 = await readLine(sock) as any;
      expect(m1.command).toEqual(['playlist-next']);
      serverSend(sock, { error: 'success', request_id: m1.request_id });
      await p1;

      const p2 = c.prev();
      const m2 = await readLine(sock) as any;
      expect(m2.command).toEqual(['playlist-prev']);
      serverSend(sock, { error: 'success', request_id: m2.request_id });
      await p2;
    });

    it('jumpTo should send set_property playlist-pos', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const p = c.jumpTo(5);
      const msg = await readLine(sock) as any;
      expect(msg.command).toEqual(['set_property', 'playlist-pos', 5]);
      serverSend(sock, { error: 'success', request_id: msg.request_id });
      await p;
    });

    it('quit should send quit command', async () => {
      const c = createClient();
      await c.connect();
      const sock = await waitForServerConnection();

      const p = c.quit();
      const msg = await readLine(sock) as any;
      expect(msg.command).toEqual(['quit']);
      serverSend(sock, { error: 'success', request_id: msg.request_id });
      await p;
    });
  });

  describe('pruneMpvLogs', () => {
    let tmpLogsDir: string;

    beforeEach(() => {
      tmpLogsDir = mkdtempSync(join(tmpdir(), 'streamloop-mpvlogs-'));
    });

    afterEach(() => {
      try { rmSync(tmpLogsDir, { recursive: true, force: true }); } catch { /* ignore */ }
    });

    it('keeps only the N newest mpv-*.log files by mtime', () => {
      // Seed 8 mpv-*.log files with staggered mtimes (i=0 oldest, i=7 newest).
      const baseSec = Date.now() / 1000 - 10000;
      for (let i = 0; i < 8; i++) {
        const f = join(tmpLogsDir, `mpv-fake-${i}.log`);
        writeFileSync(f, `log ${i}`);
        utimesSync(f, baseSec + i, baseSec + i);
      }

      pruneMpvLogs(tmpLogsDir, 3);

      const remaining = readdirSync(tmpLogsDir).sort();
      expect(remaining).toEqual(['mpv-fake-5.log', 'mpv-fake-6.log', 'mpv-fake-7.log']);
    });

    it('leaves non-matching files untouched', () => {
      writeFileSync(join(tmpLogsDir, 'mpv-old.log'), 'a');
      writeFileSync(join(tmpLogsDir, 'mpv-older.log'), 'b');
      writeFileSync(join(tmpLogsDir, 'mpv-oldest.log'), 'c');
      writeFileSync(join(tmpLogsDir, 'mpv-recent.log'), 'd');
      writeFileSync(join(tmpLogsDir, 'streamloop-2026-04-16.log'), 'keep');
      writeFileSync(join(tmpLogsDir, 'unrelated.txt'), 'keep');

      pruneMpvLogs(tmpLogsDir, 1);

      const files = readdirSync(tmpLogsDir);
      expect(files).toContain('streamloop-2026-04-16.log');
      expect(files).toContain('unrelated.txt');
      // Exactly one mpv-*.log should remain.
      expect(files.filter((f) => f.startsWith('mpv-') && f.endsWith('.log'))).toHaveLength(1);
    });

    it('no-ops gracefully when directory does not exist', () => {
      expect(() => pruneMpvLogs(join(tmpLogsDir, 'does-not-exist'), 5)).not.toThrow();
    });

    it('no-ops when retention <= 0', () => {
      writeFileSync(join(tmpLogsDir, 'mpv-a.log'), 'a');
      writeFileSync(join(tmpLogsDir, 'mpv-b.log'), 'b');
      pruneMpvLogs(tmpLogsDir, 0);
      expect(readdirSync(tmpLogsDir)).toHaveLength(2);
    });
  });

  describe('getCurrentLogFile', () => {
    it('returns null before spawn', () => {
      client = new MpvClient({ spawn: false });
      expect(client.getCurrentLogFile()).toBeNull();
    });
  });
});
