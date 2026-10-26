import { spawn } from 'child_process';
import { SPAWN_CWD } from './spawn-cwd.js';

/** The part of the supervisor's /status that Node reads. Every field may be absent. */
export interface SupervisorStatus {
  state?: string;
  reason?: string | null;
  streamKeyProblem?: string | null;
  playing?: {
    index?: number;
    id?: string | null;
    title?: string | null;
    source?: string;
    position?: number;
    duration?: number | null;
  };
  encoder?: { alive?: boolean; speed?: number | null; cpuPercent?: number; restarts?: number; generation?: number; mediaTime?: number | null };
  recovery?: { step?: string; retries?: number };
  watchdog?: { last?: unknown };
  hold?: { reason?: string; level?: number; until?: string } | null;
  youtube?: { breaker?: string; retryAt?: string | null };
  playlistCount?: number;
  tokenServer?: { state?: string; restarts?: number };
}

export type ControlCmd = 'next' | 'prev' | 'restart' | 'jump' | 'restart-encoder';

export interface ControlResult {
  ok: boolean;
  error?: string;
  alreadyRestarting?: true;
}

export interface SupervisorClientOptions {
  baseUrl?: string;
  fetchImpl?: typeof fetch;
  timeoutMs?: number;
}

function msg(e: unknown): string {
  return e instanceof Error ? e.message : 'unknown error';
}

/** undici's "fetch failed" carries the socket error code (ECONNREFUSED, ...) on its cause. */
function causeCode(e: unknown): string | undefined {
  const code = e instanceof Error ? (e.cause as { code?: unknown } | undefined)?.code : undefined;
  return typeof code === 'string' ? code : undefined;
}

/** Never throws: callers run on timers. */
export class SupervisorClient {
  private readonly baseUrl: string;
  private readonly fetchImpl: typeof fetch;
  private readonly timeoutMs: number;

  constructor(opts: SupervisorClientOptions = {}) {
    this.baseUrl = opts.baseUrl ?? 'http://127.0.0.1:4417';
    this.fetchImpl = opts.fetchImpl ?? fetch;
    this.timeoutMs = opts.timeoutMs ?? 1000;
  }

  private async call(path: string, body?: unknown): Promise<{ status: number; json: Record<string, unknown> | null }> {
    const res = await this.fetchImpl(this.baseUrl + path, {
      method: body === undefined ? 'GET' : 'POST',
      headers: body === undefined ? undefined : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    let json: Record<string, unknown> | null = null;
    try {
      const parsed: unknown = JSON.parse(await res.text());
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) json = parsed as Record<string, unknown>;
    } catch { /* non-JSON body stays null */ }
    return { status: res.status, json };
  }

  async status(): Promise<SupervisorStatus | null> {
    try {
      const r = await this.call('/status');
      return r.status === 200 ? (r.json as SupervisorStatus | null) : null;
    } catch {
      return null;
    }
  }

  /** `reason` is a note for `restart-encoder`, which the supervisor logs; other commands ignore it. */
  async control(cmd: ControlCmd, index?: number, reason?: string): Promise<ControlResult> {
    try {
      const r = await this.call('/control', { cmd, index, reason });
      if (r.status !== 200) return { ok: false, error: String(r.json?.error ?? `HTTP ${r.status}`) };
      return r.json?.alreadyRestarting ? { ok: true, alreadyRestarting: true } : { ok: true };
    } catch (e) {
      return { ok: false, error: msg(e) };
    }
  }

  /** `index` is where a switch to a new playlists[0] starts; a supervisor before 3.0.1 ignores it. */
  async reload(opts: { index?: number } = {}): Promise<{ ok: boolean; restartRequired?: boolean; error?: string; code?: string }> {
    try {
      // JSON.stringify drops an undefined index, so the body is {} without one.
      const r = await this.call('/reload', { index: opts.index });
      if (r.status !== 200) return { ok: false, error: String(r.json?.error ?? `HTTP ${r.status}`) };
      return { ok: true, restartRequired: r.json?.restartRequired === true };
    } catch (e) {
      const code = causeCode(e);
      return code ? { ok: false, error: msg(e), code } : { ok: false, error: msg(e) };
    }
  }

  async shutdown(): Promise<boolean> {
    try {
      return (await this.call('/shutdown', {})).status === 200;
    } catch {
      return false;
    }
  }
}

export type RunFn = (exe: string, args: string[], stdin: string) => Promise<{ code: number | null; stdout: string; stderr: string }>;

const runProcess: RunFn = (exe, args, stdin) =>
  new Promise((resolve, reject) => {
    const child = spawn(exe, args, { cwd: SPAWN_CWD, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    const timer = setTimeout(() => { child.kill(); reject(new Error('timed out after 10 s')); }, 10_000);
    child.stdout.on('data', (d: Buffer) => { stdout += d; });
    child.stderr.on('data', (d: Buffer) => { stderr += d; });
    child.on('error', (e) => { clearTimeout(timer); reject(e); });
    child.on('close', (code) => { clearTimeout(timer); resolve({ code, stdout, stderr }); });
    child.stdin.on('error', () => { /* the child exited early; close reports it */ });
    child.stdin.end(stdin);
  });

/** Encrypts the stream key with the supervisor's DPAPI helper. The key goes in on stdin only
 *  and never appears in an error message. */
export async function protectStreamKey(exePath: string, plain: string, run: RunFn = runProcess): Promise<string> {
  const key = plain.trim();
  if (!key) throw new Error('stream key is empty');
  const redact = (s: string): string => s.split(key).join('[redacted]');
  let out;
  try {
    out = await run(exePath, ['protect'], key);
  } catch (e) {
    throw new Error(`protect failed: ${redact(msg(e))}`);
  }
  if (out.code !== 0) {
    throw new Error(`protect failed (exit ${out.code}): ${redact(out.stderr.trim())}`);
  }
  const blob = out.stdout.trim();
  if (!blob) throw new Error('protect printed no blob');
  return blob;
}
