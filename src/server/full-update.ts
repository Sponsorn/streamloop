import { execFile } from 'child_process';
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'fs';
import { dirname, join, resolve } from 'path';
import { logger } from './logger.js';
import { SPAWN_CWD } from './spawn-cwd.js';
import { PART_KEY_FILES, SWAP_FOLDERS } from './components.js';
import { checkDependencies, type Encoder } from './setup-check.js';
import { shouldHold, type StartKind, type SupervisorLauncher } from './supervisor-launcher.js';
import type { SupervisorClient } from './supervisor-client.js';
import type { AppConfig } from './types.js';
import { failureText } from '../admin/update-text.js';
import { CHECKING_MARKER, setStep, writeUpdateState, type SwapLine, type UpdateState } from './update-state.js';

/** A step that cannot finish gets this many tries before the update falls back. */
const TRIES = 3;
/** Within a try, the condition is polled every second for up to 10 s. */
const POLL_MS = 1000;
const POLLS = 10;
/** The pause between health-check tries, as START.bat's rename retry. */
const CHECK_RETRY_MS = 3000;
const ON_AIR_POLL_MS = 2000;

const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export interface ProcessInfo {
  pid: number;
  path: string;
}

const POWERSHELL = join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
// UTF-8 out: in the console codepage a non-ASCII install path comes back mangled and never matches.
const LIST_PROCESSES = '[Console]::OutputEncoding = [Text.Encoding]::UTF8; '
  + 'Get-CimInstance Win32_Process | Select-Object ProcessId, ExecutablePath | ConvertTo-Json -Compress';

/** Matches the executable's path only: a process whose one hold on a folder is its working directory is not found. */
export function processesUnder(json: string, dirs: string[]): ProcessInfo[] {
  const parsed: unknown = JSON.parse(json);
  // One process comes out as an object, not an array.
  const rows = (Array.isArray(parsed) ? parsed : [parsed]) as Array<{ ProcessId?: unknown; ExecutablePath?: unknown }>;
  const prefixes = dirs.map((d) => resolve(d).replace(/\\+$/, '').toLowerCase() + '\\');
  const out: ProcessInfo[] = [];
  for (const row of rows) {
    const path = row?.ExecutablePath;
    const pid = row?.ProcessId;
    if (typeof path !== 'string' || typeof pid !== 'number') continue;
    const lower = path.toLowerCase();
    if (prefixes.some((p) => lower.startsWith(p))) out.push({ pid, path });
  }
  return out;
}

export async function listProcessesUnder(dirs: string[]): Promise<ProcessInfo[]> {
  const json = await new Promise<string>((res, rej) => {
    execFile(
      POWERSHELL,
      ['-NoProfile', '-NonInteractive', '-Command', LIST_PROCESSES],
      { cwd: SPAWN_CWD, windowsHide: true, timeout: 60_000, maxBuffer: 64 * 1024 * 1024 },
      (err, stdout) => (err ? rej(err) : res(stdout)),
    );
  });
  return processesUnder(json, dirs);
}

export interface CommandLineInfo {
  pid: number;
  name: string;
  commandLine: string;
}

const LIST_CMD_WINDOWS = '[Console]::OutputEncoding = [Text.Encoding]::UTF8; '
  + "Get-CimInstance Win32_Process -Filter \"Name='cmd.exe'\" | Select-Object ProcessId, Name, CommandLine | ConvertTo-Json -Compress";

export function parseCommandLines(json: string): CommandLineInfo[] {
  if (!json.trim()) return [];
  const parsed: unknown = JSON.parse(json);
  const rows = (Array.isArray(parsed) ? parsed : [parsed]) as Array<{ ProcessId?: unknown; Name?: unknown; CommandLine?: unknown }>;
  return rows.flatMap((row) => (typeof row?.ProcessId === 'number' && typeof row.Name === 'string' && typeof row.CommandLine === 'string'
    ? [{ pid: row.ProcessId, name: row.Name, commandLine: row.CommandLine }]
    : []));
}

/** The cmd.exe processes with their command lines. No path goes to PowerShell; the matching is done here. */
export async function listCmdWindows(): Promise<CommandLineInfo[]> {
  const json = await new Promise<string>((res, rej) => {
    execFile(
      POWERSHELL,
      ['-NoProfile', '-NonInteractive', '-Command', LIST_CMD_WINDOWS],
      { cwd: SPAWN_CWD, windowsHide: true, timeout: 60_000, maxBuffer: 16 * 1024 * 1024 },
      (err, stdout) => (err ? rej(err) : res(stdout)),
    );
  });
  return parseCommandLines(json);
}

/** A path character on either side means another file, such as START.bat.new or D:\x\<root>\START.bat. */
const BAT_EDGE = /[^\s"]/;

/** How many cmd.exe windows run ROOT\START.bat. A second one would heal, and so roll back, beside the swap. */
export async function countStartBatWindows(root: string, list: () => Promise<CommandLineInfo[]> = listCmdWindows): Promise<number> {
  const needle = `${resolve(root).replace(/\\+$/, '')}\\start.bat`.toLowerCase();
  return (await list()).filter((p) => {
    if (p.name.toLowerCase() !== 'cmd.exe') return false;
    const line = p.commandLine.toLowerCase();
    for (let at = line.indexOf(needle); at >= 0; at = line.indexOf(needle, at + 1)) {
      const before = line[at - 1] ?? ' ';
      const after = line[at + needle.length] ?? ' ';
      if (!BAT_EDGE.test(before) && !BAT_EDGE.test(after)) return true;
    }
    return false;
  }).length;
}

/** Ends a process at once; on Windows that is TerminateProcess. One that is already gone is fine. */
export async function killProcess(pid: number): Promise<void> {
  try {
    process.kill(pid);
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code !== 'ESRCH') logger.warn({ err, pid }, 'Could not end a process');
  }
}

export interface StopDeps {
  root: string;
  client: Pick<SupervisorClient, 'status' | 'shutdown'>;
  list: (dirs: string[]) => Promise<ProcessInfo[]>;
  kill: (pid: number) => Promise<void>;
  selfPid: number;
  /** Node's other own processes: tsx runs the server in a child of its CLI, and both run node\node.exe. */
  keepPids?: number[];
  wait: (ms: number) => Promise<void>;
}

export type StopResult = { ok: true } | { ok: false; reason: string };

async function pollFor(wait: (ms: number) => Promise<void>, condition: () => Promise<boolean>): Promise<boolean> {
  for (let i = 0; ; i++) {
    if (await condition()) return true;
    if (i === POLLS) return false;
    await wait(POLL_MS);
  }
}

/** Stops the supervisor without the stopped-by-user marker, then waits until nothing runs from the folders being swapped. */
export async function stopForSwap(deps: StopDeps, folders: string[]): Promise<StopResult> {
  const own = new Set([deps.selfPid, ...(deps.keepPids ?? [])]);
  const root = resolve(deps.root);
  // null: the list itself failed, so nothing can be said to be gone.
  const running = async (dirs: string[]): Promise<ProcessInfo[] | null> => {
    try {
      return (await deps.list(dirs)).filter((p) => !own.has(p.pid));
    } catch (err) {
      logger.warn({ err }, 'Could not list the running programs');
      return null;
    }
  };
  const named = (procs: ProcessInfo[] | null, fallback: string): string => {
    if (procs === null) return 'The running programs could not be listed.';
    const names = [...new Set(procs.map((p) => p.path.slice(root.length).replace(/^\\+/, '')))];
    if (names.length === 0) return fallback;
    return `${names.join(', ')} ${names.length > 1 ? 'are' : 'is'} still running.`;
  };
  const killAll = async (dirs: string[]): Promise<void> => {
    for (const p of (await running(dirs)) ?? []) await deps.kill(p.pid);
  };

  const supervisorDir = [join(root, 'supervisor')];
  let last: ProcessInfo[] | null = [];
  const supervisorGone = async (): Promise<boolean> => {
    const answering = (await deps.client.status()) !== null;
    last = await running(supervisorDir);
    return !answering && last !== null && last.length === 0;
  };
  let stopped = false;
  for (let t = 1; t <= TRIES && !stopped; t++) {
    if (t < TRIES) await deps.client.shutdown();
    else await killAll(supervisorDir);
    stopped = await pollFor(deps.wait, supervisorGone);
  }
  if (!stopped) return { ok: false, reason: named(last, 'The supervisor still answers on 127.0.0.1:4417.') };

  const dirs = folders.map((f) => join(root, f));
  for (let t = 1; t <= TRIES; t++) {
    if (t > 1) await killAll(dirs);
    if (await pollFor(deps.wait, async () => {
      last = await running(dirs);
      return last !== null && last.length === 0;
    })) return { ok: true };
  }
  return { ok: false, reason: named(last, 'A program from the folders being replaced is still running.') };
}

export type HealthItem = 'dependencies' | 'encoder' | 'supervisor';
export type HealthResult = { ok: true } | { ok: false; item: HealthItem; reason: string };

export interface CheckDeps {
  root: string;
  config: () => AppConfig;
  probe: (ffmpeg: string, enc: Encoder) => Promise<boolean>;
  runCheck: () => Promise<{ ok: boolean; reason: string }>;
  exists?: (p: string) => boolean;
  wait?: (ms: number) => Promise<void>;
}

/** null when one of the tries passes, else the reason of the last. */
async function threeTries(wait: (ms: number) => Promise<void>, attempt: () => Promise<string | null>): Promise<string | null> {
  let why: string | null = null;
  for (let t = 1; t <= TRIES; t++) {
    if (t > 1) await wait(CHECK_RETRY_MS);
    try {
      why = await attempt();
    } catch (err) {
      why = errorText(err);
    }
    if (why === null) return null;
  }
  return why;
}

/** The new version's check after a part swap. Each item gets 3 tries, so a blip does not roll back a good update. */
export async function runHealthCheck(deps: CheckDeps, baseline: UpdateState['baseline']): Promise<HealthResult> {
  const exists = deps.exists ?? existsSync;
  const wait = deps.wait ?? sleep;

  // Only files an update can change count; the overlay font is Windows' own.
  const inRoot = resolve(deps.root).toLowerCase() + '\\';
  const missing = await threeTries(wait, async () => {
    const gone = checkDependencies(deps.root, exists)
      .filter((d) => !d.ok && resolve(d.path).toLowerCase().startsWith(inRoot))
      .map((d) => d.name);
    return gone.length > 0 ? `Missing from the install: ${gone.join(', ')}.` : null;
  });
  if (missing) return { ok: false, item: 'dependencies', reason: missing };

  // An encoder that failed before the update is not the update's fault. Never pickEncoder: its fallback
  // would pass a broken NVENC as a silent move to libx264.
  if (baseline?.works) {
    const enc = deps.config().supervisor.encoder;
    const ffmpeg = join(deps.root, 'ffmpeg', 'ffmpeg.exe');
    const why = await threeTries(wait, async () =>
      (await deps.probe(ffmpeg, enc)) ? null : `${enc} worked before the update and fails on the new ffmpeg.`);
    if (why) return { ok: false, item: 'encoder', reason: why };
  }

  const sup = await threeTries(wait, async () => {
    const r = await deps.runCheck();
    return r.ok ? null : `The new supervisor failed its check: ${r.reason}`;
  });
  if (sup) return { ok: false, item: 'supervisor', reason: sup };
  return { ok: true };
}

/** `streamloop-supervisor.exe check --root ROOT`: exit 0 passes, else its stderr line is the reason. */
export function supervisorCheck(root: string): Promise<{ ok: boolean; reason: string }> {
  return runCheckProgram(join(root, 'supervisor', 'streamloop-supervisor.exe'), ['check', '--root', root]);
}

/** Well above the check's own worst case: up to 15 s for the token server, its last ping and ffmpeg -version. */
const CHECK_TIMEOUT_MS = 60_000;
/** After the tree kill, how long the check's output may take to close before the try is given up. */
const KILLED_CLOSE_MS = 10_000;
const TASKKILL = join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'taskkill.exe');

/** Ends a process and every process it started. */
function killTree(pid: number): Promise<void> {
  return new Promise((res) => {
    execFile(TASKKILL, ['/T', '/F', '/PID', String(pid)], { cwd: SPAWN_CWD, windowsHide: true, timeout: 30_000 }, () => res());
  });
}

/** Runs one try of the check. One that runs too long is ended with its tree: execFile's own timeout ends only
 *  the check, and the token server it started would keep port 4418, so the next try would fail on the port. */
export function runCheckProgram(exe: string, args: string[], timeoutMs = CHECK_TIMEOUT_MS): Promise<{ ok: boolean; reason: string }> {
  return new Promise((res) => {
    let settled = false;
    let timedOut = false;
    const finish = (r: { ok: boolean; reason: string }): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      res(r);
    };
    const late = { ok: false, reason: `it did not finish within ${Math.round(timeoutMs / 1000)} s` };
    const child = execFile(exe, args, { cwd: SPAWN_CWD, windowsHide: true }, (err, _stdout, stderr) => {
      if (!err) return finish({ ok: true, reason: '' });
      if (timedOut) return finish(late);
      const said = String(stderr).trim().split(/\r?\n/).pop()?.replace(/^streamloop-supervisor check:\s*/, '');
      finish({ ok: false, reason: said || errorText(err) });
    });
    const timer = setTimeout(() => {
      timedOut = true;
      const pid = child.pid;
      void (pid === undefined ? Promise.resolve() : killTree(pid)).then(() => {
        if (child.exitCode === null) child.kill();
        setTimeout(() => finish(late), KILLED_CLOSE_MS).unref();
      });
    }, timeoutMs);
  });
}

const leftoverNames = (): Array<{ name: string; keyFile: string | null }> => [
  { name: '_update_old', keyFile: null },
  ...SWAP_FOLDERS.flatMap((f) => ['_update_old_', '_update_bad_'].map((p) => ({ name: p + f, keyFile: PART_KEY_FILES[f] }))),
];

/** Before a full update: copies an earlier update left behind would be put back by a rollback, so they go now.
 *  One whose live part is not whole may be its only copy; it is kept and the update is called off. */
export function clearLeftovers(root: string): void {
  for (const { name, keyFile } of leftoverNames()) {
    const dir = join(root, name);
    if (!existsSync(dir)) continue;
    if (keyFile && !existsSync(join(root, keyFile))) {
      throw new Error(`${keyFile.split('/')[0]}\\ is not whole, so ${name} from an earlier update was kept.`);
    }
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch (err) {
      logger.warn({ err, dir }, 'Could not remove a copy left by an earlier update');
      throw new Error(`${name} from an earlier update could not be removed.`);
    }
  }
}

/** An old copy that cannot be removed now goes before the next swap. */
function removeOldCopies(root: string): void {
  for (const name of ['_update_old', ...SWAP_FOLDERS.map((f) => `_update_old_${f}`)]) {
    try {
      rmSync(join(root, name), { recursive: true, force: true });
    } catch (err) {
      logger.warn({ err, name }, 'Could not remove an old copy after the update');
    }
  }
}

/** The dashboard's failure copy, also posted to Discord once per update that did not go through. */
export function failureMessage(state: UpdateState): string {
  return failureText(state);
}

export const RESTARTED = 'StreamLoop was restarted during the update.';
export const CUT_OFF = 'The update was cut off, so the old version was put back.';
const UNFINISHED_CHECK = 'The new version did not finish its check.';

export type StartupAction =
  | { kind: 'none' }
  /** New parts are in place and unchecked. */
  | { kind: 'check' }
  /** The check passed before a restart; its clean-up and the start are still due. */
  | { kind: 'finish' }
  /** The check failed and its exit 76 never reached START.bat. */
  | { kind: 'roll-back' }
  | { kind: 'mark'; result: 'failed' | 'rolled-back'; reason: string };

function lastSwapFailure(log: SwapLine[]): string | null {
  const line = [...log].reverse().find((l) => l.status === 'failed');
  return line ? `${line.folder}: ${line.detail}` : null;
}

/** What this Node start does about an update, from START.bat's start kind, the state and data\update-checking. */
export function decideStartup(
  start: StartKind,
  state: UpdateState | null,
  checking: boolean,
  swapLog: SwapLine[] = [],
): StartupAction {
  if (start === 'rollback') {
    if (!state || state.result === 'rolled-back') return { kind: 'none' };
    return { kind: 'mark', result: 'rolled-back', reason: state.reason ?? lastSwapFailure(swapLog) ?? UNFINISHED_CHECK };
  }
  // START.bat keeps update-checking from a part swap until the check passes, also across a reboot.
  if (start === 'update' || checking) {
    if (state?.result === 'ok') return { kind: 'finish' };
    if (state?.result === 'failed' && checking) return { kind: 'roll-back' };
    return { kind: 'check' };
  }
  if (state?.result !== 'running') return { kind: 'none' };
  // Nothing was swapped yet, so the old version is in place.
  if (state.step === 'download' || state.step === 'stop') return { kind: 'mark', result: 'failed', reason: RESTARTED };
  // Heal put a swap or a rollback that was cut off back, and removed update-checking.
  return { kind: 'mark', result: 'rolled-back', reason: CUT_OFF };
}

/** Whether the launcher waits for Start: through an update's check, else as the start kind says. */
export function holdAtStart(action: StartupAction, start: StartKind, autoStartStream: boolean, waitingMarker: boolean): boolean {
  if (action.kind === 'check' || action.kind === 'finish' || action.kind === 'roll-back') return true;
  return shouldHold(start, autoStartStream, waitingMarker);
}

export interface StartupDeps {
  root: string;
  /** The running version, which is the update's target after a swap. */
  version: string;
  action: StartupAction;
  state: UpdateState | null;
  autoStartStream: () => boolean;
  launcher: Pick<SupervisorLauncher, 'release' | 'isStoppedByUser'>;
  client: Pick<SupervisorClient, 'status'>;
  /** A Discord post. */
  notify: (message: string, level: 'info' | 'warn') => void;
  check: (baseline: UpdateState['baseline']) => Promise<HealthResult>;
  /** Closes the server and exits 76, so START.bat puts the old version back. */
  rollBack: () => void;
  wait?: (ms: number) => Promise<void>;
}

export async function runStartup(deps: StartupDeps): Promise<void> {
  const action = deps.action;
  switch (action.kind) {
    case 'none':
      return;
    case 'mark':
      markResult(deps, action.result, action.reason);
      return;
    case 'roll-back':
      logger.error({ reason: deps.state?.reason }, 'The new version failed its check before this start; rolling back');
      deps.rollBack();
      return;
    case 'check':
      await checkNewVersion(deps);
      return;
    case 'finish':
      await finishUpdate(deps, deps.state);
  }
}

function markResult(deps: StartupDeps, result: 'failed' | 'rolled-back', reason: string): void {
  if (!deps.state) return;
  logger.warn({ result, reason, to: deps.state.to }, 'Recording how an earlier update ended');
  const ended: UpdateState = { ...deps.state, result, reason };
  try {
    writeUpdateState(deps.root, ended);
    // Only once recorded: an unrecorded end is recorded and posted by the next start.
    deps.notify(failureMessage(ended), 'warn');
  } catch (err) {
    logger.error({ err }, 'Could not record how the update ended');
  }
  // A staging left by an update that never swapped must not go in with a later exit 75.
  try {
    rmSync(join(deps.root, '_update_tmp'), { recursive: true, force: true });
  } catch (err) {
    logger.warn({ err }, 'Could not remove _update_tmp');
  }
}

async function checkNewVersion(deps: StartupDeps): Promise<void> {
  const marker = join(deps.root, 'data', CHECKING_MARKER);
  try {
    // START.bat writes it after the swap as well, so it usually exists already.
    mkdirSync(dirname(marker), { recursive: true });
    writeFileSync(marker, '');
  } catch (err) {
    logger.warn({ err }, 'Could not write update-checking');
  }

  // Only the update's own state is moved on; with none, the check still guards the new parts.
  let state = deps.state?.result === 'running' ? deps.state : null;
  try {
    if (state) state = setStep(deps.root, 'check') ?? state;
  } catch (err) {
    logger.error({ err }, 'Could not record the check step; rolling back');
    deps.rollBack();
    return;
  }

  logger.info({ to: state?.to ?? deps.version }, 'Checking the new version');
  const result = await deps.check(state?.baseline ?? null);
  if (!result.ok) {
    logger.error({ item: result.item, reason: result.reason }, 'The new version failed its check; rolling back');
    try {
      if (state) writeUpdateState(deps.root, { ...state, result: 'failed', reason: result.reason });
    } catch (err) {
      logger.error({ err }, 'Could not record the failed check');
    }
    // update-checking stays: START.bat's rollback removes it, and a Node that dies before exit 76 still counts toward one.
    deps.rollBack();
    return;
  }
  await finishUpdate(deps, state);
}

async function finishUpdate(deps: StartupDeps, current: UpdateState | null): Promise<void> {
  let state = current;
  try {
    if (state?.result === 'running') state = setStep(deps.root, 'start', { result: 'ok', reason: null }) ?? state;
  } catch (err) {
    // Without the result on disk, a later start would take the update as unchecked; the old version is the safe side.
    logger.error({ err }, 'Could not record the passed check; rolling back');
    deps.rollBack();
    return;
  }
  removeOldCopies(deps.root);
  // Only after the result is written: a start that finds the marker with no result checks again.
  try {
    rmSync(join(deps.root, 'data', CHECKING_MARKER), { force: true });
  } catch (err) {
    logger.warn({ err }, 'Could not remove update-checking');
  }
  logger.info({ version: deps.version }, 'The update passed its check');

  if (!deps.autoStartStream() || deps.launcher.isStoppedByUser()) return;
  deps.launcher.release();
  if (state?.result === 'ok' && state.step === 'start') await reportOnAir(deps);
}

async function reportOnAir(deps: StartupDeps): Promise<void> {
  const wait = deps.wait ?? sleep;
  for (;;) {
    // A Stop before the stream came up leaves the update at "start".
    if (deps.launcher.isStoppedByUser()) return;
    const s = await deps.client.status();
    if (s?.state === 'playing' && s.encoder?.alive === true) break;
    await wait(ON_AIR_POLL_MS);
  }
  try {
    setStep(deps.root, 'done');
  } catch (err) {
    logger.warn({ err }, 'Could not record the update as done');
  }
  deps.notify(`Back on air on ${deps.version}.`, 'info');
}
