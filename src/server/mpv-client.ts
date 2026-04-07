import { EventEmitter } from 'events';
import net from 'net';
import { spawn, type ChildProcess } from 'child_process';
import { logger } from './logger.js';

export interface MpvClientOptions {
  mpvPath?: string;
  pipePath?: string;
  mpvArgs?: string[];
  spawn?: boolean;
}

interface PendingCommand {
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
}

/**
 * MpvClient spawns mpv as a child process and communicates via
 * Windows named pipe IPC using mpv's JSON IPC protocol.
 *
 * Events emitted:
 *  - connected
 *  - disconnected
 *  - fileStarted
 *  - fileEnded(reason: string)
 *  - fileLoaded
 *  - shutdown
 *  - propertyChange(name: string, data: unknown)
 *  - processExit(code: number | null)
 *  - processError(err: Error)
 */
export class MpvClient extends EventEmitter {
  private static readonly COMMAND_TIMEOUT_MS = 5000;

  private readonly mpvPath: string;
  private readonly pipePath: string;
  private readonly extraArgs: string[];
  private readonly shouldSpawn: boolean;

  private process: ChildProcess | null = null;
  private socket: net.Socket | null = null;
  private connected = false;
  private buffer = '';
  private requestId = 0;
  private pending = new Map<number, PendingCommand>();

  constructor(options: MpvClientOptions = {}) {
    super();
    this.mpvPath = options.mpvPath ?? 'mpv.exe';
    this.pipePath = options.pipePath ?? '\\\\.\\pipe\\mpv-streamloop';
    this.extraArgs = options.mpvArgs ?? [];
    this.shouldSpawn = options.spawn ?? true;
  }

  // ── Lifecycle ──────────────────────────────────────────────

  /** Spawn mpv and connect to its IPC pipe. */
  async start(): Promise<void> {
    if (this.shouldSpawn) {
      this.spawnProcess();
    }
    await this.connect();
  }

  /** Disconnect and kill the mpv process. */
  stop(): void {
    this.disconnect();
    this.killProcess();
  }

  /** Stop then start again. */
  async restart(): Promise<void> {
    this.stop();
    // Brief delay to let the process fully exit and release the pipe
    await new Promise((r) => setTimeout(r, 500));
    await this.start();
  }

  /** Connect to the IPC named pipe with retry. */
  async connect(maxAttempts = 10): Promise<void> {
    const retryDelay = 500;

    for (let attempt = 1; attempt <= maxAttempts; attempt++) {
      try {
        await this.connectOnce();
        return;
      } catch (err) {
        if (attempt === maxAttempts) {
          throw new Error(`Failed to connect to mpv IPC pipe after ${maxAttempts} attempts`);
        }
        logger.debug({ attempt, maxAttempts, pipePath: this.pipePath }, 'mpv IPC connect attempt failed, retrying');
        await new Promise((r) => setTimeout(r, retryDelay));
      }
    }
  }

  /** Disconnect from the IPC pipe. */
  disconnect(): void {
    if (this.socket) {
      this.socket.removeAllListeners();
      this.socket.destroy();
      this.socket = null;
    }

    const wasConnected = this.connected;
    this.connected = false;
    this.buffer = '';

    // Reject all pending commands
    for (const [id, cmd] of this.pending) {
      cmd.reject(new Error('Disconnected from mpv IPC'));
      this.pending.delete(id);
    }

    if (wasConnected) {
      this.emit('disconnected');
    }
  }

  // ── State ──────────────────────────────────────────────────

  isConnected(): boolean {
    return this.connected;
  }

  isRunning(): boolean {
    return this.process !== null && this.process.exitCode === null;
  }

  getProcess(): ChildProcess | null {
    return this.process;
  }

  // ── Commands ───────────────────────────────────────────────

  /** Send a command to mpv and return the result. */
  command(...args: unknown[]): Promise<unknown> {
    if (!this.socket || !this.connected) {
      return Promise.reject(new Error('Not connected to mpv IPC'));
    }

    const id = ++this.requestId;
    const msg = JSON.stringify({ command: args, request_id: id }) + '\n';

    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error('mpv IPC command timed out'));
      }, MpvClient.COMMAND_TIMEOUT_MS);

      this.pending.set(id, {
        resolve: (value) => { clearTimeout(timer); resolve(value); },
        reject: (reason) => { clearTimeout(timer); reject(reason); },
      });
      this.socket!.write(msg);
    });
  }

  // ── Convenience methods ────────────────────────────────────

  getProperty(name: string): Promise<unknown> {
    return this.command('get_property', name);
  }

  setProperty(name: string, value: unknown): Promise<unknown> {
    return this.command('set_property', name, value);
  }

  loadFile(url: string, mode: string = 'replace'): Promise<unknown> {
    return this.command('loadfile', url);
  }

  loadPlaylist(url: string, mode: string = 'replace'): Promise<unknown> {
    // Use loadfile (not loadlist) so yt-dlp's ytdl_hook resolves the playlist.
    // loadlist treats the URL as a plaintext playlist file, which breaks YouTube URLs.
    return this.command('loadfile', url, mode);
  }

  play(): Promise<unknown> {
    return this.setProperty('pause', false);
  }

  pause(): Promise<unknown> {
    return this.setProperty('pause', true);
  }

  togglePause(): Promise<unknown> {
    return this.command('cycle', 'pause');
  }

  seek(seconds: number, mode: string = 'relative'): Promise<unknown> {
    return this.command('seek', seconds, mode);
  }

  next(): Promise<unknown> {
    return this.command('playlist-next');
  }

  prev(): Promise<unknown> {
    return this.command('playlist-prev');
  }

  jumpTo(index: number): Promise<unknown> {
    return this.setProperty('playlist-pos', index);
  }

  quit(): Promise<unknown> {
    return this.command('quit');
  }

  // ── Private: process management ────────────────────────────

  private spawnProcess(): void {
    const args = [
      '--idle',
      `--input-ipc-server=${this.pipePath}`,
      '--no-terminal',
      '--force-window=yes',
      ...this.extraArgs,
    ];

    logger.info({ mpvPath: this.mpvPath, args }, 'Spawning mpv');

    this.process = spawn(this.mpvPath, args, {
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: false,
    });

    this.process.on('error', (err) => {
      logger.error({ err }, 'mpv process error');
      this.emit('processError', err);
    });

    this.process.on('exit', (code) => {
      logger.info({ code }, 'mpv process exited');
      this.process = null;
      this.emit('processExit', code);
    });
  }

  private killProcess(): void {
    if (this.process && this.process.exitCode === null) {
      this.process.kill();
      this.process = null;
    }
  }

  // ── Private: IPC connection ────────────────────────────────

  private connectOnce(): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      const socket = net.connect(this.pipePath);

      const onError = (err: Error) => {
        socket.removeAllListeners();
        socket.destroy();
        reject(err);
      };

      socket.once('error', onError);

      socket.once('connect', () => {
        socket.removeListener('error', onError);
        this.socket = socket;
        this.connected = true;
        this.buffer = '';

        socket.on('data', (chunk: Buffer) => this.onData(chunk));
        socket.on('error', (err) => {
          logger.error({ err }, 'mpv IPC socket error');
        });
        socket.on('close', () => {
          if (this.connected) {
            logger.warn('mpv IPC connection closed');
            this.connected = false;
            this.rejectAllPending('Connection closed');
            this.emit('disconnected');
          }
        });

        logger.info({ pipePath: this.pipePath }, 'Connected to mpv IPC');
        this.emit('connected');
        resolve();
      });
    });
  }

  private onData(chunk: Buffer): void {
    this.buffer += chunk.toString();

    let newlineIdx: number;
    while ((newlineIdx = this.buffer.indexOf('\n')) !== -1) {
      const line = this.buffer.slice(0, newlineIdx);
      this.buffer = this.buffer.slice(newlineIdx + 1);

      if (line.trim().length === 0) continue;

      try {
        const msg = JSON.parse(line);
        this.handleMessage(msg);
      } catch {
        logger.warn({ line }, 'Failed to parse mpv IPC message');
      }
    }
  }

  private handleMessage(msg: any): void {
    // Event message (no request_id)
    if (msg.event) {
      this.handleEvent(msg);
      return;
    }

    // Command response (has request_id)
    if (msg.request_id !== undefined) {
      const pending = this.pending.get(msg.request_id);
      if (pending) {
        this.pending.delete(msg.request_id);
        if (msg.error === 'success') {
          pending.resolve(msg.data);
        } else {
          pending.reject(new Error(msg.error ?? 'Unknown mpv error'));
        }
      }
    }
  }

  private handleEvent(msg: any): void {
    switch (msg.event) {
      case 'start-file':
        this.emit('fileStarted');
        break;
      case 'end-file':
        this.emit('fileEnded', msg.reason ?? 'unknown');
        break;
      case 'file-loaded':
        this.emit('fileLoaded');
        break;
      case 'shutdown':
        this.emit('shutdown');
        break;
      case 'property-change':
        this.emit('propertyChange', msg.name, msg.data);
        break;
      default:
        logger.debug({ event: msg.event }, 'Unhandled mpv event');
        break;
    }
  }

  private rejectAllPending(reason: string): void {
    for (const [id, cmd] of this.pending) {
      cmd.reject(new Error(reason));
    }
    this.pending.clear();
  }
}
