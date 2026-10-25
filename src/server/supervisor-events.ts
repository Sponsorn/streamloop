import { closeSync, openSync, readSync, statSync } from 'fs';
import { join } from 'path';
import type { DiscordConfig } from './types.js';
import { logger } from './logger.js';

export interface SupervisorEvent {
  ts: string;
  level: 'info' | 'warn' | 'critical';
  kind: string;
  [extra: string]: unknown;
}

export type DiscordEventKind = keyof DiscordConfig['events'];

export interface Described {
  /** Timeline text. Empty means the event is too frequent for the 100-entry timeline: skip it. */
  message: string;
  discord?: { kind: DiscordEventKind; vars: Record<string, string | number>; level: 'info' | 'warn' | 'error' };
}

const DETAIL_MAX = 200;

/** One line, at most 200 characters: safe to put in a Discord message. */
function one(v: unknown): string {
  if (v === undefined || v === null) return '';
  const s = typeof v === 'string' ? v : JSON.stringify(v);
  const line = s.split(/\r?\n/)[0].trim();
  return line.length > DETAIL_MAX ? line.slice(0, DETAIL_MAX - 3) + '...' : line;
}

function num(e: SupervisorEvent, key: string): number {
  const v = e[key];
  return typeof v === 'number' ? v : 0;
}

/** The supervisor counts videos from 0; people count from 1. */
function video(e: SupervisorEvent): number {
  return num(e, 'index') + 1;
}

function critical(message: string, timeline = message): Described {
  return { message: timeline, discord: { kind: 'critical', vars: { message }, level: 'error' } };
}

/** One per video, seam or probe: they would push everything else out of the 100-entry timeline. */
const QUIET = new Set(['feeder-start', 'handover', 'spare-resolve', 'spare-used', 'youtube-probe', 'download-cancel']);

/** The one mapping from a supervisor or launcher event to timeline text and an optional Discord alert. */
export function describe(e: SupervisorEvent): Described {
  if (QUIET.has(e.kind)) return { message: '' };
  const n = video(e);
  switch (e.kind) {
    case 'encoder-exit': {
      const failures = num(e, 'failures');
      const msg = `Encoder stopped (${one(e.why)}); restarting in ${num(e, 'restartInS')}s`;
      if (e.level === 'critical') return critical(`The encoder has stopped ${failures} times in a row (${one(e.why)}).`, msg);
      // 3 failures in a row is where the supervisor goes critical, so it is the limit shown here.
      return { message: msg, discord: { kind: 'streamDrop', vars: { attempt: failures, maxAttempts: 3 }, level: 'warn' } };
    }
    case 'encoder-start': {
      const restarts = num(e, 'restarts');
      if (restarts === 0) return { message: 'Encoder started' };
      return { message: `Encoder restarted (restart ${restarts})`, discord: { kind: 'streamRestart', vars: { attempts: restarts }, level: 'info' } };
    }
    case 'encoder-spawn-failed':
      return critical(`The encoder could not start: ${one(e.error)}`);
    case 'skip': {
      const detail = e.detail && typeof e.detail === 'object' ? (e.detail as Record<string, unknown>).reason ?? e.detail : e.detail;
      const why = one(e.why);
      const reason = one(detail) ? `${why}: ${one(detail)}` : why;
      const msg = `Skipped video ${n} (${reason})`;
      if (e.level === 'critical') return critical(`${num(e, 'consecutive')} videos in a row were skipped. The last was video ${n} (${reason}).`, msg);
      return { message: msg, discord: { kind: 'skip', vars: { videoIndex: n, videoId: one(e.id), reason }, level: 'warn' } };
    }
    case 'retry':
      return {
        message: `Retrying video ${n} at ${Math.floor(num(e, 'pos'))}s (${one(e.why)}, attempt ${num(e, 'retry')})`,
        discord: { kind: 'error', vars: { videoIndex: n, videoId: one(e.id), errorCode: one(e.why), attempt: num(e, 'retry') }, level: 'warn' },
      };
    case 'watchdog-freeze':
    case 'watchdog-silence': {
      const kind = e.kind === 'watchdog-freeze' ? 'freeze' : 'silence';
      const outcome = one(e.outcome);
      const msg = `Output ${kind} on video ${n} (${outcome})`;
      if (outcome === 'retry' || outcome === 'skip') {
        return { message: msg, discord: { kind: 'outputFrozen', vars: { kind, videoIndex: n }, level: 'warn' } };
      }
      return { message: msg };
    }
    case 'resume':
      return {
        message: `Resumed video ${n} at ${Math.floor(num(e, 'pos'))}s`,
        discord: { kind: 'resume', vars: { videoIndex: n, videoId: one(e.id) }, level: 'info' },
      };
    case 'all-skipped':
      return critical(`Every video was skipped. Holding for ${num(e, 'holdS')}s before trying again.`);
    case 'playlist-failed': {
      const msg = `Playlist could not be loaded: ${one(e.reason)}`;
      // Only a first load is critical; a failed switch keeps the old list playing.
      return e.level === 'critical' ? critical(msg) : { message: msg };
    }
    case 'stream-key-unreadable':
      return critical(`The stream key cannot be read: ${one(e.reason)}`);
    case 'stream-key-missing':
      return critical('No stream key is set. The stream cannot start.');
    case 'youtube-breaker-open':
      return critical(`YouTube is blocking requests (${one(e.reason)}). Downloads and lookups are paused.`);
    case 'youtube-breaker-closed':
      return { message: 'YouTube requests resumed' };
    case 'downloads-failing':
      return critical(`${num(e, 'inARow')} downloads failed in a row (${one(e.reason)}).`);
    case 'feeder-spawn-failed':
      return critical(`Video ${n} could not start: ${one(e.error)}`);
    case 'feeder-spawn-recovered':
      return { message: `Video ${n} started after an earlier failure` };
    case 'supervisor-down':
      return critical('The supervisor has stopped answering.');
    case 'supervisor-back':
      return { message: 'Supervisor is answering again', discord: { kind: 'streamRestart', vars: { attempts: 1 }, level: 'info' } };
    case 'supervisor-missing':
      return critical('The supervisor program is missing from the install.');
    case 'supervisor-launched': return { message: 'Supervisor launched' };
    case 'start': return { message: 'Supervisor started' };
    case 'shutdown': return { message: 'Supervisor shut down' };
    case 'reload': return { message: 'Configuration reloaded' };
    case 'control': return { message: `Control: ${one(e.cmd)}` };
    case 'playlist': return { message: `Playlist loaded (${num(e, 'count')} videos)` };
    case 'playlist-recovered': return { message: 'Playlist loaded after an earlier failure' };
    case 'playlist-switch-failed': return { message: `Playlist switch failed: ${one(e.reason)}` };
    case 'download-failed': return { message: `Download of video ${n} failed: ${one(e.reason)}` };
    case 'resolve-failed': return { message: `Lookup of video ${n} failed: ${one(e.reason)}` };
    case 'url-refresh': return { message: `Refreshed the address of video ${n}` };
    case 'slate-stall': return { message: 'Standby slate stalled' };
    case 'breaker-retarget': return { message: 'Moved to a playable video while YouTube is blocked' };
    case 'youtube-probe-failed': return { message: `YouTube probe failed: ${one(e.reason)}` };
    case 'encoder-restart-requested': return { message: `Encoder restart requested: ${one(e.reason)}` };
    case 'overlay-failed': return { message: `Overlay failed: ${one(e.error)}` };
    case 'log-prune-failed': return { message: `Old logs could not be removed: ${one(e.first)}` };
    case 'deadline': return { message: `Test run reached its ${num(e, 'hours')} hour limit` };
    case 'fault':
    case 'fault-applied': return { message: `Test fault: ${one(e.fault)}` };
    case 'token-server-spawn': return { message: 'Token server started' };
    case 'token-server-adopt': return { message: 'Token server already running' };
    case 'token-server-down': return { message: 'Token server stopped answering' };
    case 'token-server-restart': return { message: 'Token server restarted' };
    case 'token-server-spawn-failed': return { message: `Token server could not start: ${one(e.error)}` };
    default:
      return { message: e.kind };
  }
}

/** Shape a launcher callback as a SupervisorEvent, so one function routes both sources. */
export function launcherEvent(kind: string, detail?: Record<string, unknown>): SupervisorEvent {
  const level = kind === 'supervisor-down' || kind === 'supervisor-missing' ? 'critical' : 'info';
  return { ts: new Date().toISOString(), level, kind, ...detail };
}

function localDate(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

export interface SupervisorEventsOptions {
  /** The supervisor's own `data\logs` folder, never Node's `logs\`. */
  logsDir: string;
  onEvent: (e: SupervisorEvent) => void;
  intervalMs?: number;
  now?: () => Date;
}

/** Tails the supervisor's daily events file and hands each new line to onEvent. */
export class SupervisorEvents {
  private timer: ReturnType<typeof setInterval> | null = null;
  private date = '';
  private offset = 0;
  private readonly now: () => Date;

  constructor(private readonly opts: SupervisorEventsOptions) {
    this.now = opts.now ?? (() => new Date());
  }

  private file(date: string): string {
    return join(this.opts.logsDir, `events-${date}.jsonl`);
  }

  private size(date: string): number {
    try { return statSync(this.file(date)).size; } catch { return -1; }
  }

  start(): void {
    if (this.timer) return;
    this.date = localDate(this.now());
    // No replay: begin at the end of today's file.
    this.offset = Math.max(0, this.size(this.date));
    this.timer = setInterval(() => this.poll(), this.opts.intervalMs ?? 2000);
    this.timer.unref();
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  /** One read pass. Public so tests do not wait for the timer. */
  poll(): void {
    try {
      for (;;) {
        this.readNew();
        const today = localDate(this.now());
        if (today === this.date) break;
        // Yesterday is finished; a partial last line there will never complete.
        this.date = today;
        this.offset = 0;
      }
    } catch (err) {
      logger.warn({ err }, 'Reading supervisor events failed');
    }
  }

  private readNew(): void {
    const size = this.size(this.date);
    if (size < 0) return;
    if (size < this.offset) this.offset = 0;
    if (size === this.offset) return;
    const buf = Buffer.alloc(size - this.offset);
    const fd = openSync(this.file(this.date), 'r');
    try {
      readSync(fd, buf, 0, buf.length, this.offset);
    } finally {
      closeSync(fd);
    }
    const end = buf.lastIndexOf(0x0a);
    if (end < 0) return;
    this.offset += end + 1;
    for (const line of buf.subarray(0, end).toString('utf-8').split('\n')) {
      if (!line.trim()) continue;
      let e: SupervisorEvent;
      try { e = JSON.parse(line) as SupervisorEvent; } catch { continue; }
      if (e && typeof e.kind === 'string') this.opts.onEvent(e);
    }
  }
}
