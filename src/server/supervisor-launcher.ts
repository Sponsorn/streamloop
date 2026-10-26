import { spawn } from 'child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'fs';
import { dirname, join } from 'path';
import { logger } from './logger.js';
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
  /** Dev and test runs only (STREAMLOOP_DEBUG_FAULTS=1): adds --debug-faults, which opens POST /debug/fault. */
  debugFaults?: boolean;
  /** Runs right before a spawn, never on attach. A string holds the launch with that reason until a later poll. */
  beforeLaunch?: () => Promise<string | null>;
}

const MISSES_BEFORE_LAUNCH = 3;
const BACKOFF_MS = [10_000, 30_000, 60_000, 300_000];
const BACKOFF_RESET_MS = 60_000;
const PORT_TAKEN_EXIT = 2;
const CONFIG_REFUSED_EXIT = 3;
const PORT_TAKEN_WINDOW_MS = 5_000;
const DRAIN_LIMIT_MS = 20_000;

/** The release layout is ROOT\app + ROOT\node; in dev the root comes from STREAMLOOP_ROOT. */
export function installRoot(appDir: string, env: NodeJS.ProcessEnv): string | null {
  const parent = dirname(appDir);
  if (existsSync(join(parent, 'node'))) return parent;
  return env.STREAMLOOP_ROOT || null;
}

/** The reason of the last config-refused event in today's supervisor events file, if any. */
export function lastConfigRefusal(root: string, now: Date = new Date()): string | null {
  const date = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
  let text: string;
  try {
    text = readFileSync(join(root, 'data', 'logs', `events-${date}.jsonl`), 'utf-8');
  } catch {
    return null;
  }
  for (const line of text.split('\n').reverse()) {
    try {
      const e = JSON.parse(line) as { kind?: unknown; reason?: unknown };
      if (e.kind === 'config-refused') return typeof e.reason === 'string' ? e.reason : null;
    } catch {
      // a partial or blank line
    }
  }
  return null;
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
  private readonly debugFaults: boolean;
  private readonly beforeLaunch: (() => Promise<string | null>) | null;

  private timer: ReturnType<typeof setInterval> | null = null;
  private polling = false;
  private phase: LauncherState = 'starting';
  private stoppedByUser = false;
  /** Stop was sent and the old supervisor has not been seen gone yet. */
  private draining = false;
  /** When a Start came in while draining; the launch waits for the old one to exit. */
  private startWaitSince: number | null = null;
  private misses = 0;
  private inOutage = false;
  private missingReported = false;
  private runningSince = 0;
  private spawns = 0;
  private spawnSeq = 0;
  private lastSpawnAt = 0;
  private lastExit: { code: number | null; at: number; spawnedAt: number } | null = null;
  private held: string | null = null;
  /** Why the last supervisor refused config.json (exit 3), until one answers. */
  private refused: string | null = null;

  constructor(opts: SupervisorLauncherOptions) {
    this.root = opts.root;
    this.client = opts.client;
    this.spawnImpl = opts.spawnImpl ?? spawn;
    this.now = opts.now ?? Date.now;
    this.intervalMs = opts.intervalMs ?? 2000;
    this.onEvent = opts.onEvent ?? (() => {});
    this.debugFaults = opts.debugFaults ?? false;
    this.beforeLaunch = opts.beforeLaunch ?? null;
    // A Stop survives a Node restart: the marker keeps the supervisor down until Start.
    const marker = this.marker();
    this.stoppedByUser = marker !== null && existsSync(marker);
  }

  private marker(): string | null {
    return this.root ? join(this.root, 'data', 'stopped-by-user') : null;
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
    this.draining = true;
    const marker = this.marker();
    if (!marker) return;
    try {
      mkdirSync(dirname(marker), { recursive: true });
      writeFileSync(marker, '');
    } catch {
      // ponytail: the Stop still holds until Node restarts; only its persistence is lost.
    }
  }

  /** Why a Start has not launched yet, while it waits for the stopped supervisor to exit. */
  waitReason(): string | null {
    if (this.startWaitSince === null) return null;
    return this.now() - this.startWaitSince >= DRAIN_LIMIT_MS
      ? 'The stopped supervisor is still running after 20 s. The new one starts when it exits.'
      : 'Waiting for the stopped supervisor to exit before starting it again.';
  }

  /** Why beforeLaunch is holding the launch, until a spawn or an answering supervisor clears it. */
  blockReason(): string | null {
    return this.held ?? this.refused;
  }

  /** An operator Start: no backoff, and the spawn still goes through the one guarded poll. */
  launchNow(): void {
    this.stoppedByUser = false;
    const marker = this.marker();
    try {
      if (marker) rmSync(marker, { force: true });
    } catch {
      // ponytail: the Start still holds; a Node restart before the marker goes would come up stopped.
    }
    this.spawns = 0;
    if (this.draining) {
      // The old supervisor still holds the port; launch once it stops answering, with no outage.
      this.startWaitSince = this.now();
      this.phase = 'starting';
      if (this.timer) void this.poll();
      return;
    }
    this.misses = MISSES_BEFORE_LAUNCH;
    // a supervisor still answering after Stop must not turn the next miss into an outage
    if (this.phase === 'running') this.phase = 'starting';
    if (this.timer) void this.poll();
  }

  private async poll(): Promise<void> {
    if (this.polling) return;
    this.polling = true;
    try {
      const answer = await this.client.status();
      if (this.timer) await this.onPoll(answer !== null);
    } finally {
      this.polling = false;
    }
  }

  private async onPoll(answered: boolean): Promise<void> {
    const t = this.now();
    if (!answered) this.draining = false;
    if (this.startWaitSince !== null) {
      if (answered) return;
      this.startWaitSince = null;
      // If beforeLaunch holds this launch, the next poll retries it instead of counting misses again.
      this.misses = MISSES_BEFORE_LAUNCH;
      await this.launch();
      return;
    }
    if (answered) {
      this.misses = 0;
      this.held = null;
      this.refused = null;
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
    await this.launch();
  }

  private async launch(): Promise<void> {
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

    if (this.beforeLaunch) {
      // The poll stays guarded while this runs, so no second launch can start beside it.
      let held: string | null = null;
      try {
        held = await this.beforeLaunch();
      } catch (err) {
        logger.warn({ err }, 'The pre-launch check failed; launching anyway');
      }
      if (!this.timer || this.stoppedByUser) return;
      this.held = held;
      if (held) return;
    }
    // The guard can take seconds; the port-taken window and the backoff count from the real spawn.
    const spawnedAt = this.now();

    // `launch` starts the supervisor detached and exits, so no Node exit or tree kill reaches it.
    // Its exit code is the supervisor's port-taken 2 or refused-config 3, relayed; 1 when the supervisor crashed
    // during the bind wait or could not spawn; otherwise 0.
    const args = ['launch', '--root', this.root];
    if (this.debugFaults) args.push('--debug-faults');
    const child = this.spawnImpl(exe, args, {
      cwd: SPAWN_CWD,
      stdio: 'ignore',
      windowsHide: true,
    });
    const seq = ++this.spawnSeq;
    child.on('error', () => {
      if (seq === this.spawnSeq && this.phase === 'starting') this.phase = 'down';
    });
    child.on('exit', (code: number | null) => {
      if (seq !== this.spawnSeq) return;
      this.lastExit = { code, at: this.now(), spawnedAt };
      if (code === CONFIG_REFUSED_EXIT && this.root) {
        const why = lastConfigRefusal(this.root);
        this.refused = `The supervisor refused config.json${why ? `: ${why}` : '.'}`;
      }
      // 0 is the intermediate handing off to a running supervisor
      if (code !== 0 && this.phase === 'starting') this.phase = 'down';
    });
    child.unref();

    this.spawns++;
    this.lastSpawnAt = spawnedAt;
    this.phase = 'starting';
    this.onEvent('supervisor-launched', { exe });
  }
}
