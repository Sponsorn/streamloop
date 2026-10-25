import { spawn } from 'child_process';
import { existsSync } from 'fs';
import { dirname, join } from 'path';
import { SPAWN_CWD } from './spawn-cwd.js';
import type { SupervisorClient } from './supervisor-client.js';

export type LauncherState = 'missing' | 'starting' | 'running' | 'stopped-by-user' | 'down';
export type LauncherEvent = 'supervisor-launched' | 'supervisor-down' | 'supervisor-back' | 'supervisor-missing';

export interface SupervisorLauncherOptions {
  root: string | null;
  client: SupervisorClient;
  spawnImpl?: typeof spawn;
  now?: () => number;
  intervalMs?: number;
  onEvent?: (kind: LauncherEvent, detail?: Record<string, unknown>) => void;
}

const MISSES_BEFORE_LAUNCH = 3;
const BACKOFF_MS = [10_000, 30_000, 60_000, 300_000];
const BACKOFF_RESET_MS = 60_000;
const PORT_TAKEN_EXIT = 2;
const PORT_TAKEN_WINDOW_MS = 5_000;

/** The release layout is ROOT\app + ROOT\node; in dev the root comes from STREAMLOOP_ROOT. */
export function installRoot(appDir: string, env: NodeJS.ProcessEnv): string | null {
  const parent = dirname(appDir);
  if (existsSync(join(parent, 'node'))) return parent;
  return env.STREAMLOOP_ROOT || null;
}

/** Starts the supervisor as a sibling of Node and relaunches it when it stops answering.
 *  It never stops the supervisor: only the dashboard's "Stop stream" does that. */
export class SupervisorLauncher {
  private readonly root: string | null;
  private readonly client: SupervisorClient;
  private readonly spawnImpl: typeof spawn;
  private readonly now: () => number;
  private readonly intervalMs: number;
  private readonly onEvent: (kind: LauncherEvent, detail?: Record<string, unknown>) => void;

  private timer: ReturnType<typeof setInterval> | null = null;
  private polling = false;
  private phase: LauncherState = 'starting';
  private stoppedByUser = false;
  private misses = 0;
  private inOutage = false;
  private missingReported = false;
  private runningSince = 0;
  private spawns = 0;
  private spawnSeq = 0;
  private lastSpawnAt = 0;
  private lastExit: { code: number | null; at: number; spawnedAt: number } | null = null;

  constructor(opts: SupervisorLauncherOptions) {
    this.root = opts.root;
    this.client = opts.client;
    this.spawnImpl = opts.spawnImpl ?? spawn;
    this.now = opts.now ?? Date.now;
    this.intervalMs = opts.intervalMs ?? 2000;
    this.onEvent = opts.onEvent ?? (() => {});
  }

  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => void this.poll(), this.intervalMs);
    void this.poll();
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  state(): LauncherState {
    if (this.phase !== 'running' && this.stoppedByUser) return 'stopped-by-user';
    return this.phase;
  }

  /** Set by Stop stream until the next Start, even while the old supervisor still answers. */
  isStoppedByUser(): boolean {
    return this.stoppedByUser;
  }

  markStoppedByUser(): void {
    this.stoppedByUser = true;
  }

  /** An operator Start: no backoff, and the spawn still goes through the one guarded poll. */
  launchNow(): void {
    this.stoppedByUser = false;
    this.misses = MISSES_BEFORE_LAUNCH;
    this.spawns = 0;
    // a supervisor still answering after Stop must not turn the next miss into an outage
    if (this.phase === 'running') this.phase = 'starting';
    if (this.timer) void this.poll();
  }

  private async poll(): Promise<void> {
    if (this.polling) return;
    this.polling = true;
    try {
      const answer = await this.client.status();
      if (this.timer) this.onPoll(answer !== null);
    } finally {
      this.polling = false;
    }
  }

  private onPoll(answered: boolean): void {
    const t = this.now();
    if (answered) {
      this.misses = 0;
      const exit = this.lastExit;
      if (exit && exit.code === PORT_TAKEN_EXIT && exit.at - exit.spawnedAt < PORT_TAKEN_WINDOW_MS) {
        // Another supervisor held the port: we attached, the spawn did not fail.
        this.spawns = Math.max(0, this.spawns - 1);
        this.lastSpawnAt = 0;
      }
      this.lastExit = null;
      if (this.phase !== 'running') {
        this.phase = 'running';
        this.runningSince = t;
        if (this.inOutage) this.onEvent('supervisor-back');
        this.inOutage = false;
      }
      if (t - this.runningSince >= BACKOFF_RESET_MS) this.spawns = 0;
      return;
    }

    this.misses++;
    if (this.misses < MISSES_BEFORE_LAUNCH) return;
    if (this.stoppedByUser) {
      this.phase = 'stopped-by-user';
      return;
    }
    if (this.phase === 'running') {
      this.phase = 'down';
      if (!this.inOutage) this.onEvent('supervisor-down');
      this.inOutage = true;
    }
    this.launch();
  }

  private launch(): void {
    const t = this.now();
    if (this.spawns > 0) {
      const wait = BACKOFF_MS[Math.min(this.spawns - 1, BACKOFF_MS.length - 1)];
      if (t - this.lastSpawnAt < wait) return;
    }
    const exe = this.root ? join(this.root, 'supervisor', 'streamloop-supervisor.exe') : null;
    if (!this.root || !exe || !existsSync(exe)) {
      this.phase = 'missing';
      if (!this.missingReported) this.onEvent('supervisor-missing', { root: this.root });
      this.missingReported = true;
      return;
    }
    this.missingReported = false;

    // detached: own process group and no console, so closing the launcher window or a Node
    // exit (crash, exit 75) leaves the supervisor and the stream running.
    const child = this.spawnImpl(exe, ['--root', this.root], {
      cwd: SPAWN_CWD,
      detached: true,
      stdio: 'ignore',
      windowsHide: true,
    });
    const spawnedAt = t;
    const seq = ++this.spawnSeq;
    child.on('error', () => {
      if (seq === this.spawnSeq && this.phase === 'starting') this.phase = 'down';
    });
    child.on('exit', (code: number | null) => {
      if (seq !== this.spawnSeq) return;
      this.lastExit = { code, at: this.now(), spawnedAt };
      if (this.phase === 'starting') this.phase = 'down';
    });
    child.unref();

    this.spawns++;
    this.lastSpawnAt = t;
    this.phase = 'starting';
    this.onEvent('supervisor-launched', { exe });
  }
}
