import { describe, it, expect, vi, beforeEach, afterEach, beforeAll, afterAll } from 'vitest';
import { spawn, type ChildProcess } from 'child_process';
import { copyFileSync, existsSync, linkSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join, resolve } from 'path';
import {
  CUT_OFF,
  RESTARTED,
  countStartBatWindows,
  decideStartup,
  failureMessage,
  holdAtStart,
  listProcessesUnder,
  parseCommandLines,
  processesUnder,
  runCheckProgram,
  runHealthCheck,
  runStartup,
  stopForSwap,
  type CheckDeps,
  type CommandLineInfo,
  type ProcessInfo,
  type StartupAction,
  type StartupDeps,
  type StopDeps,
} from '../full-update.js';
import { CHECKING_MARKER, readUpdateState, writeUpdateState, type SwapLine, type UpdateState } from '../update-state.js';
import type { AppConfig } from '../types.js';

const R = String.raw`C:\Users\Emma Ö\StreamLoop 3`;
const exe = (rel: string) => `${R}\\${rel}`;

interface World {
  answering: boolean;
  procs: ProcessInfo[];
  shutdownWorks: boolean;
  killable: (pid: number) => boolean;
}

const SUPERVISOR: ProcessInfo = { pid: 10, path: exe('supervisor\\streamloop-supervisor.exe') };

type FakeStop = StopDeps & {
  client: { status: ReturnType<typeof vi.fn>; shutdown: ReturnType<typeof vi.fn> };
  kill: ReturnType<typeof vi.fn>;
  wait: ReturnType<typeof vi.fn>;
};

function fakeStop(world: World): FakeStop {
  const dropSupervisor = () => {
    world.answering = false;
    world.procs = world.procs.filter((p) => p.pid !== SUPERVISOR.pid);
  };
  return {
    root: R,
    client: {
      status: vi.fn(async () => (world.answering ? { state: 'playing' } : null)),
      shutdown: vi.fn(async () => {
        if (world.shutdownWorks) dropSupervisor();
        return world.answering;
      }),
    },
    list: vi.fn(async (dirs: string[]) =>
      world.procs.filter((p) => dirs.some((d) => p.path.toLowerCase().startsWith(d.toLowerCase() + '\\')))),
    kill: vi.fn(async (pid: number) => {
      if (!world.killable(pid)) return;
      if (pid === SUPERVISOR.pid) dropSupervisor();
      world.procs = world.procs.filter((p) => p.pid !== pid);
    }),
    selfPid: 1,
    keepPids: [2],
    wait: vi.fn(async () => {}),
  };
}

describe('stopForSwap', () => {
  it('stops on the first try when the supervisor exits', async () => {
    // Arrange
    const deps = fakeStop({ answering: true, procs: [SUPERVISOR], shutdownWorks: true, killable: () => true });

    // Act
    const r = await stopForSwap(deps, ['supervisor', 'yt-dlp']);

    // Assert
    expect(r).toEqual({ ok: true });
    expect(deps.client.shutdown).toHaveBeenCalledTimes(1);
    expect(deps.kill).not.toHaveBeenCalled();
  });

  it('ends the supervisor by force on the third try', async () => {
    // Arrange
    const deps = fakeStop({ answering: true, procs: [SUPERVISOR], shutdownWorks: false, killable: () => true });

    // Act
    const r = await stopForSwap(deps, ['supervisor', 'yt-dlp']);

    // Assert
    expect(r).toEqual({ ok: true });
    expect(deps.client.shutdown).toHaveBeenCalledTimes(2);
    expect(deps.kill.mock.calls).toEqual([[SUPERVISOR.pid]]);
  });

  it('never kills its own process', async () => {
    // Arrange: the server and the tsx process that started it both run node\node.exe.
    const deps = fakeStop({
      answering: false,
      procs: [
        { pid: 1, path: exe('node\\node.exe') },
        { pid: 2, path: exe('node\\node.exe') },
        { pid: 30, path: exe('node\\node.exe') },
        { pid: 31, path: exe('yt-dlp\\yt-dlp.exe') },
      ],
      shutdownWorks: true,
      killable: () => true,
    });

    // Act
    const r = await stopForSwap(deps, ['node', 'yt-dlp']);

    // Assert
    expect(r).toEqual({ ok: true });
    expect(deps.kill.mock.calls.map(([pid]) => pid).sort()).toEqual([30, 31]);
  });

  it('gives up after three tries and names what is still running', async () => {
    // Arrange
    const deps = fakeStop({
      answering: false,
      procs: [{ pid: 40, path: exe('ffmpeg\\ffmpeg.exe') }],
      shutdownWorks: true,
      killable: () => false,
    });

    // Act
    const r = await stopForSwap(deps, ['ffmpeg', 'yt-dlp']);

    // Assert
    expect(r).toEqual({ ok: false, reason: 'ffmpeg\\ffmpeg.exe is still running.' });
    expect(deps.kill).toHaveBeenCalledTimes(2);
  });

  it('gives up when the supervisor survives the forced end and names it', async () => {
    // Arrange
    const deps = fakeStop({ answering: true, procs: [SUPERVISOR], shutdownWorks: false, killable: () => false });

    // Act
    const r = await stopForSwap(deps, ['supervisor', 'yt-dlp']);

    // Assert
    expect(r).toEqual({ ok: false, reason: 'supervisor\\streamloop-supervisor.exe is still running.' });
  });

  it('waits about 10 s for each try, polling every second', async () => {
    // Arrange
    const deps = fakeStop({ answering: true, procs: [SUPERVISOR], shutdownWorks: false, killable: () => false });

    // Act
    await stopForSwap(deps, ['supervisor', 'yt-dlp']);

    // Assert: three tries of ten 1 s pauses, then nothing for the folders.
    expect(deps.wait).toHaveBeenCalledTimes(30);
    expect(new Set(deps.wait.mock.calls.map(([ms]) => ms))).toEqual(new Set([1000]));
  });
});

describe('processesUnder', () => {
  it('matches only programs under the folders, ignoring case', () => {
    // Arrange
    const json = JSON.stringify([
      { ProcessId: 5, ExecutablePath: exe('FFMPEG\\ffmpeg.exe') },
      { ProcessId: 6, ExecutablePath: exe('ffmpeg2\\ffmpeg.exe') },
      { ProcessId: 7, ExecutablePath: null },
      { ProcessId: 8, ExecutablePath: 'C:\\Windows\\System32\\cmd.exe' },
    ]);

    // Act
    const found = processesUnder(json, [`${R}\\ffmpeg`]);

    // Assert
    expect(found).toEqual([{ pid: 5, path: exe('FFMPEG\\ffmpeg.exe') }]);
  });

  it('reads a single process, which PowerShell prints as an object', () => {
    // Act
    const found = processesUnder(JSON.stringify({ ProcessId: 9, ExecutablePath: exe('pot\\x.exe') }), [`${R}\\pot\\`]);

    // Assert
    expect(found).toEqual([{ pid: 9, path: exe('pot\\x.exe') }]);
  });
});

describe('countStartBatWindows', () => {
  const cmd = (pid: number, commandLine: string): CommandLineInfo => ({ pid, name: 'cmd.exe', commandLine });

  it("counts the cmd.exe windows running this install's START.bat, however they were started and ignoring case", async () => {
    // Arrange: Explorer or the autostart VBS, the trampoline's start, a plain cmd /c, and look-alikes.
    const list = vi.fn(async () => [
      cmd(1, `C:\\WINDOWS\\system32\\cmd.exe /c ""${R}\\START.bat" "`),
      cmd(2, `C:\\WINDOWS\\system32\\cmd.exe  /K "${R.toUpperCase()}\\start.bat" update`),
      cmd(3, `cmd.exe /d /s /c ""${R}\\START.bat""`),
      cmd(4, `cmd.exe /c "${R}\\START.bat.new"`),
      cmd(5, `cmd.exe /c "D:\\Other\\StreamLoop 3\\START.bat"`),
      cmd(6, `cmd.exe /c "${R}\\app\\START.bat"`),
      cmd(7, `cmd.exe /c "${R}\\_apply_launcher.bat"`),
      { pid: 8, name: 'node.exe', commandLine: `node.exe "${R}\\START.bat"` },
    ]);

    // Act
    const n = await countStartBatWindows(`${R}\\`, list);

    // Assert
    expect(n).toBe(3);
  });

  it('reads a single process, which PowerShell prints as an object, and skips one without a command line', () => {
    // Act
    const one = parseCommandLines(JSON.stringify({ ProcessId: 9, Name: 'cmd.exe', CommandLine: 'cmd.exe /c x.bat' }));
    const none = parseCommandLines(JSON.stringify([{ ProcessId: 10, Name: 'cmd.exe', CommandLine: null }]));

    // Assert
    expect(one).toEqual([{ pid: 9, name: 'cmd.exe', commandLine: 'cmd.exe /c x.bat' }]);
    expect(none).toEqual([]);
  });
});

describe.skipIf(process.platform !== 'win32')('countStartBatWindows on this machine', () => {
  let dir = '';
  let window: ChildProcess | null = null;

  beforeAll(async () => {
    dir = mkdtempSync(join(tmpdir(), 'sl window Ö-'));
    writeFileSync(join(dir, 'START.bat'), '@echo off\r\nping -n 60 127.0.0.1 >nul\r\n');
    window = spawn('cmd.exe', ['/d', '/s', '/c', `""${join(dir, 'START.bat')}""`], { cwd: dir, stdio: 'ignore', windowsHide: true, windowsVerbatimArguments: true });
    await new Promise((r) => setTimeout(r, 500));
  });

  afterAll(() => {
    if (window?.pid) spawn('taskkill', ['/T', '/F', '/PID', String(window.pid)], { stdio: 'ignore', windowsHide: true });
    rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  });

  it('finds a START.bat window in a folder with a space and an Ö in its path', async () => {
    // Act
    const n = await countStartBatWindows(dir);
    const elsewhere = await countStartBatWindows(join(dir, 'app'));

    // Assert
    expect(n).toBe(1);
    expect(elsewhere).toBe(0);
  }, 30_000);
});

describe.skipIf(process.platform !== 'win32')('listProcessesUnder', () => {
  let dir = '';
  let child: ChildProcess | null = null;

  beforeAll(() => {
    dir = mkdtempSync(join(tmpdir(), 'sl list Ö-'));
    mkdirSync(join(dir, 'ffmpeg'));
    const copy = join(dir, 'ffmpeg', 'holder.exe');
    try {
      linkSync(process.execPath, copy);
    } catch {
      copyFileSync(process.execPath, copy);
    }
    child = spawn(copy, ['-e', 'setInterval(() => {}, 1e9)'], { cwd: dir, stdio: 'ignore', windowsHide: true });
  });

  afterAll(() => {
    child?.kill();
    rmSync(dir, { recursive: true, force: true, maxRetries: 5 });
  });

  it('finds a program running from a folder with a space and an Ö in its path', async () => {
    // Act
    const found = await listProcessesUnder([join(dir, 'ffmpeg')]);
    const elsewhere = await listProcessesUnder([join(dir, 'pot')]);

    // Assert
    expect(found.map((p) => p.pid)).toContain(child?.pid);
    expect(found.find((p) => p.pid === child?.pid)?.path.toLowerCase()).toBe(join(dir, 'ffmpeg', 'holder.exe').toLowerCase());
    expect(elsewhere).toEqual([]);
  }, 30_000);
});

describe.skipIf(process.platform !== 'win32')('runCheckProgram', () => {
  const alive = (pid: number): boolean => {
    try {
      process.kill(pid, 0);
      return true;
    } catch {
      return false;
    }
  };
  let dir = '';
  let grandchild = 0;

  beforeAll(() => {
    dir = mkdtempSync(join(tmpdir(), 'sl check Ö-'));
  });

  afterAll(() => {
    if (grandchild && alive(grandchild)) process.kill(grandchild);
    rmSync(dir, { recursive: true, force: true, maxRetries: 5 });
  });

  it('ends a check that does not finish in time together with the token server it started', async () => {
    // Arrange: a check that starts a child sharing its output and then hangs. Detached keeps the child out of
    // the check's own job, as the supervisor's token server is.
    const pidFile = join(dir, 'token-server.pid');
    const script = "const c = require('child_process').spawn(process.execPath, ['-e', 'setInterval(() => {}, 1e9)'], { stdio: 'inherit', detached: true });"
      + " require('fs').writeFileSync(process.argv[1], String(c.pid)); setInterval(() => {}, 1e9);";

    // Act
    const result = await runCheckProgram(process.execPath, ['-e', script, pidFile], 3000);

    // Assert
    grandchild = Number(readFileSync(pidFile, 'utf8'));
    expect(result).toEqual({ ok: false, reason: 'it did not finish within 3 s' });
    await vi.waitFor(() => expect(alive(grandchild)).toBe(false), { timeout: 5000 });
  }, 30_000);
});

const config = (encoder: AppConfig['supervisor']['encoder'] = 'h264_nvenc') =>
  ({ supervisor: { encoder } }) as unknown as AppConfig;

function fakeCheck(over: Partial<CheckDeps> = {}): CheckDeps & { probe: ReturnType<typeof vi.fn>; runCheck: ReturnType<typeof vi.fn> } {
  return {
    root: R,
    config: () => config(),
    probe: vi.fn(async () => true),
    runCheck: vi.fn(async () => ({ ok: true, reason: '' })),
    exists: () => true,
    wait: vi.fn(async () => {}),
    ...over,
  } as CheckDeps & { probe: ReturnType<typeof vi.fn>; runCheck: ReturnType<typeof vi.fn> };
}

describe('runHealthCheck', () => {
  it('passes when every item passes', async () => {
    // Arrange
    const deps = fakeCheck();

    // Act
    const r = await runHealthCheck(deps, { encoder: 'h264_nvenc', works: true });

    // Assert
    expect(r).toEqual({ ok: true });
    expect(deps.probe).toHaveBeenCalledWith(exe('ffmpeg\\ffmpeg.exe'), 'h264_nvenc');
    expect(deps.runCheck).toHaveBeenCalledTimes(1);
  });

  it('retries a failing item and passes when the second try works', async () => {
    // Arrange
    const deps = fakeCheck({
      runCheck: vi.fn()
        .mockResolvedValueOnce({ ok: false, reason: 'the token server did not answer' })
        .mockResolvedValue({ ok: true, reason: '' }),
    });

    // Act
    const r = await runHealthCheck(deps, { encoder: 'h264_nvenc', works: true });

    // Assert
    expect(r).toEqual({ ok: true });
    expect(deps.runCheck).toHaveBeenCalledTimes(2);
  });

  it('fails after three tries and names the item', async () => {
    // Arrange
    const deps = fakeCheck({ runCheck: vi.fn(async () => ({ ok: false, reason: 'ffmpeg -version failed' })) });

    // Act
    const r = await runHealthCheck(deps, null);

    // Assert
    expect(r).toEqual({ ok: false, item: 'supervisor', reason: 'The new supervisor failed its check: ffmpeg -version failed' });
    expect(deps.runCheck).toHaveBeenCalledTimes(3);
  });

  it('fails when a file the supervisor needs is missing', async () => {
    // Arrange
    const deps = fakeCheck({ exists: (p) => !p.endsWith('main.js') });

    // Act
    const r = await runHealthCheck(deps, null);

    // Assert
    expect(r).toEqual({ ok: false, item: 'dependencies', reason: 'Missing from the install: Token server.' });
    expect(deps.runCheck).not.toHaveBeenCalled();
  });

  it('fails when NVENC worked before the update and not after', async () => {
    // Arrange
    const deps = fakeCheck({ probe: vi.fn(async () => false) });

    // Act
    const r = await runHealthCheck(deps, { encoder: 'h264_nvenc', works: true });

    // Assert
    expect(r).toEqual({ ok: false, item: 'encoder', reason: 'h264_nvenc worked before the update and fails on the new ffmpeg.' });
    expect(deps.probe).toHaveBeenCalledTimes(3);
  });

  it('does not blame the update for an encoder that already failed', async () => {
    // Arrange
    const deps = fakeCheck({ probe: vi.fn(async () => false) });

    // Act
    const r = await runHealthCheck(deps, { encoder: 'h264_nvenc', works: false });

    // Assert
    expect(r).toEqual({ ok: true });
    expect(deps.probe).not.toHaveBeenCalled();
  });

  it('does not fail an update over a missing overlay font, which no update changes', async () => {
    // Arrange
    const deps = fakeCheck({ exists: (p) => !p.toLowerCase().endsWith('malgun.ttf') });

    // Act
    const r = await runHealthCheck(deps, null);

    // Assert
    expect(r).toEqual({ ok: true });
  });

  it('tests the configured encoder, never a fallback', async () => {
    // Arrange
    const deps = fakeCheck({ config: () => config('h264_amf') });

    // Act
    await runHealthCheck(deps, { encoder: 'h264_amf', works: true });

    // Assert
    expect(deps.probe.mock.calls.map(([, enc]) => enc)).toEqual(['h264_amf']);
  });
});

function state(over: Partial<UpdateState> = {}): UpdateState {
  return {
    from: '3.0.5',
    to: '3.0.6',
    parts: ['supervisor', 'ffmpeg'],
    folders: ['supervisor', 'ffmpeg', 'yt-dlp'],
    step: 'swap',
    result: 'running',
    reason: null,
    startedAt: '2026-11-08T10:00:00.000Z',
    stepStartedAt: '2026-11-08T10:02:00.000Z',
    baseline: { encoder: 'h264_nvenc', works: true },
    ...over,
  };
}

describe('decideStartup', () => {
  it('checks the new version on an update start with a running state', () => {
    // Act + Assert
    expect(decideStartup('update', state(), true)).toEqual({ kind: 'check' });
    expect(decideStartup('update', state(), false)).toEqual({ kind: 'check' });
  });

  it('checks the new version on any start that finds update-checking with a running state', () => {
    // Act + Assert: a reboot or a closed window during the check leaves new parts unchecked.
    expect(decideStartup('first', state({ step: 'check' }), true)).toEqual({ kind: 'check' });
    expect(decideStartup('restart', state({ step: 'swap' }), true)).toEqual({ kind: 'check' });
  });

  it('marks a rollback start rolled back and keeps the reason', () => {
    // Act
    const a = decideStartup('rollback', state({ step: 'check', result: 'failed', reason: 'The new supervisor failed its check: x' }), false);

    // Assert
    expect(a).toEqual({ kind: 'mark', result: 'rolled-back', reason: 'The new supervisor failed its check: x' });
  });

  it('takes the reason of a rollback from the last failed swap line when the state has none', () => {
    // Arrange
    const log: SwapLine[] = [
      { folder: 'supervisor', status: 'ok', detail: 'swapped' },
      { folder: 'ffmpeg', status: 'retry', detail: 'try 2 of 3 after a failed rename' },
      { folder: 'ffmpeg', status: 'failed', detail: 'the folder stayed in use after 3 tries' },
      { folder: 'supervisor', status: 'restored', detail: 'the previous version' },
    ];

    // Act
    const a = decideStartup('rollback', state({ step: 'swap' }), false, log);

    // Assert
    expect(a).toEqual({ kind: 'mark', result: 'rolled-back', reason: 'ffmpeg: the folder stayed in use after 3 tries' });
  });

  it('marks a rollback after crashes during the check with a reason of its own', () => {
    // Act
    const a = decideStartup('rollback', state({ step: 'check' }), false, []);

    // Assert
    expect(a).toEqual({ kind: 'mark', result: 'rolled-back', reason: 'The new version did not finish its check.' });
  });

  it('marks an update cut off during the swap or check as rolled back when heal removed update-checking', () => {
    // Act + Assert
    expect(decideStartup('first', state({ step: 'swap' }), false)).toEqual({ kind: 'mark', result: 'rolled-back', reason: CUT_OFF });
    expect(decideStartup('restart', state({ step: 'check' }), false)).toEqual({ kind: 'mark', result: 'rolled-back', reason: CUT_OFF });
  });

  it('marks a crash restart during the download or the stop as a failed update', () => {
    // Act + Assert
    expect(decideStartup('restart', state({ step: 'download' }), false)).toEqual({ kind: 'mark', result: 'failed', reason: RESTARTED });
    expect(decideStartup('restart', state({ step: 'stop' }), false)).toEqual({ kind: 'mark', result: 'failed', reason: RESTARTED });
  });

  it('rolls back again when the failed check is still marked', () => {
    // Act + Assert: the new Node wrote its failure and died before exit 76 reached START.bat.
    expect(decideStartup('update', state({ step: 'check', result: 'failed', reason: 'x' }), true)).toEqual({ kind: 'roll-back' });
  });

  it('finishes an update whose check passed before a restart', () => {
    // Act + Assert
    expect(decideStartup('update', state({ step: 'start', result: 'ok' }), true)).toEqual({ kind: 'finish' });
  });

  it('does not take a stale update-checking for new parts while nothing was swapped', () => {
    // Act + Assert: a marker an earlier update could not remove, then an update cut off before its swap.
    for (const start of ['first', 'restart', 'update'] as const) {
      expect(decideStartup(start, state({ step: 'download' }), true)).toEqual({ kind: 'mark', result: 'failed', reason: RESTARTED });
      expect(decideStartup(start, state({ step: 'stop' }), true)).toEqual({ kind: 'mark', result: 'failed', reason: RESTARTED });
    }
    expect(decideStartup('first', state({ step: 'stop', result: 'failed', reason: 'x' }), true)).toEqual({ kind: 'none' });
    expect(decideStartup('first', null, true)).toEqual({ kind: 'check' });
  });

  it('leaves a finished or settled update alone', () => {
    // Act + Assert
    expect(decideStartup('first', state({ step: 'done', result: 'ok' }), false)).toEqual({ kind: 'none' });
    expect(decideStartup('restart', state({ step: 'check', result: 'rolled-back', reason: 'x' }), false)).toEqual({ kind: 'none' });
    expect(decideStartup('first', null, false)).toEqual({ kind: 'none' });
  });
});

describe('failureMessage', () => {
  it('uses the dashboard failure copy without doubling the reason full stop', () => {
    // Act
    const text = failureMessage(state({ step: 'stop', result: 'failed', reason: 'ffmpeg\\ffmpeg.exe is still running.' }));

    // Assert
    expect(text).toBe('The update to 3.0.6 failed at stopping the stream: ffmpeg\\ffmpeg.exe is still running. Still on 3.0.5.');
  });
});

describe('holdAtStart', () => {
  it('holds while an update is checked or finished, and otherwise follows the start', () => {
    // Act + Assert
    expect(holdAtStart({ kind: 'check' }, 'first', true, false)).toBe(true);
    expect(holdAtStart({ kind: 'finish' }, 'update', true, false)).toBe(true);
    expect(holdAtStart({ kind: 'roll-back' }, 'update', true, false)).toBe(true);
    expect(holdAtStart({ kind: 'mark', result: 'failed', reason: RESTARTED }, 'first', true, false)).toBe(false);
    expect(holdAtStart({ kind: 'mark', result: 'failed', reason: RESTARTED }, 'first', false, false)).toBe(true);
    expect(holdAtStart({ kind: 'mark', result: 'rolled-back', reason: 'x' }, 'rollback', true, false)).toBe(false);
    expect(holdAtStart({ kind: 'none' }, 'restart', true, true)).toBe(true);
  });
});

const ROOT = resolve('.test-tmp', 'full-update');
const marker = join(ROOT, 'data', CHECKING_MARKER);

function fakeStartup(action: StartupAction, s: UpdateState | null, over: Partial<StartupDeps> = {}) {
  const deps = {
    root: ROOT,
    version: '3.0.6',
    action,
    state: s,
    autoStartStream: () => true,
    launcher: { release: vi.fn(), isStoppedByUser: vi.fn(() => false) },
    client: { status: vi.fn(async () => ({ state: 'playing', encoder: { alive: true } })) },
    notify: vi.fn(),
    check: vi.fn(async () => ({ ok: true }) as const),
    rollBack: vi.fn(),
    wait: vi.fn(async () => {}),
    ...over,
  };
  return deps as typeof deps & StartupDeps;
}

describe('runStartup', () => {
  beforeEach(() => {
    rmSync(ROOT, { recursive: true, force: true });
    mkdirSync(join(ROOT, 'data'), { recursive: true });
  });
  afterEach(() => rmSync(ROOT, { recursive: true, force: true }));

  function swapped(s: UpdateState): void {
    writeUpdateState(ROOT, s);
    writeFileSync(marker, '');
    for (const dir of ['_update_old', '_update_old_supervisor', '_update_old_ffmpeg', '_update_old_yt-dlp']) {
      mkdirSync(join(ROOT, dir, 'x'), { recursive: true });
    }
  }

  it('finishes a passed check: removes the old copies and update-checking, starts the stream and reports it on air', async () => {
    // Arrange: START.bat already wrote update-checking after the swap.
    swapped(state());
    const deps = fakeStartup({ kind: 'check' }, state());

    // Act
    await runStartup(deps);

    // Assert
    expect(deps.check).toHaveBeenCalledWith({ encoder: 'h264_nvenc', works: true });
    for (const dir of ['_update_old', '_update_old_supervisor', '_update_old_ffmpeg', '_update_old_yt-dlp']) {
      expect(existsSync(join(ROOT, dir))).toBe(false);
    }
    expect(existsSync(marker)).toBe(false);
    expect(deps.launcher.release).toHaveBeenCalledTimes(1);
    expect(readUpdateState(ROOT)).toMatchObject({ result: 'ok', step: 'done', reason: null });
    expect(vi.mocked(deps.notify).mock.calls).toEqual([['Back on air on 3.0.6.', 'info']]);
    expect(deps.rollBack).not.toHaveBeenCalled();
  });

  it('removes update-checking before the old copies, so later crashes cannot roll a passed update back', async () => {
    // Arrange
    swapped(state());
    const markerWhenCopiesGo: boolean[] = [];
    const deps = fakeStartup({ kind: 'check' }, state(), {
      removeOldCopies: vi.fn(() => { markerWhenCopiesGo.push(existsSync(marker)); }),
    });

    // Act
    await runStartup(deps);

    // Assert
    expect(markerWhenCopiesGo).toEqual([false]);
    expect(readUpdateState(ROOT)).toMatchObject({ result: 'ok' });
  });

  it('writes the check step before the check runs', async () => {
    // Arrange
    swapped(state());
    let stepDuringCheck: string | undefined;
    const deps = fakeStartup({ kind: 'check' }, state(), {
      check: vi.fn(async () => {
        stepDuringCheck = readUpdateState(ROOT)?.step;
        return { ok: true } as const;
      }),
    });

    // Act
    await runStartup(deps);

    // Assert
    expect(stepDuringCheck).toBe('check');
  });

  it('waits for playing with a live encoder before it reports the update done', async () => {
    // Arrange
    swapped(state());
    const status = vi.fn()
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({ state: 'starting', encoder: { alive: true } })
      .mockResolvedValueOnce({ state: 'playing', encoder: { alive: false } })
      .mockResolvedValue({ state: 'playing', encoder: { alive: true } });
    const deps = fakeStartup({ kind: 'check' }, state(), { client: { status } });

    // Act
    await runStartup(deps);

    // Assert
    expect(status).toHaveBeenCalledTimes(4);
    expect(readUpdateState(ROOT)?.step).toBe('done');
  });

  it('does not start the stream after an update when it was stopped by the user', async () => {
    for (const autoStart of [true, false]) {
      // Arrange
      swapped(state());
      const deps = fakeStartup({ kind: 'check' }, state(), {
        autoStartStream: () => autoStart,
        launcher: { release: vi.fn(), isStoppedByUser: vi.fn(() => true) },
      });

      // Act
      await runStartup(deps);

      // Assert
      expect(deps.launcher.release).not.toHaveBeenCalled();
      expect(readUpdateState(ROOT)).toMatchObject({ result: 'ok', step: 'start' });
      expect(existsSync(marker)).toBe(false);
      expect(deps.notify).not.toHaveBeenCalled();
    }
  });

  it('stays ready after a passed check when the stream does not start by itself', async () => {
    // Arrange
    swapped(state());
    const deps = fakeStartup({ kind: 'check' }, state(), { autoStartStream: () => false });

    // Act
    await runStartup(deps);

    // Assert
    expect(deps.launcher.release).not.toHaveBeenCalled();
    expect(readUpdateState(ROOT)).toMatchObject({ result: 'ok', step: 'start' });
  });

  it('keeps update-checking and exits 76 when the check fails', async () => {
    // Arrange
    swapped(state());
    const deps = fakeStartup({ kind: 'check' }, state(), {
      check: vi.fn(async () => ({ ok: false, item: 'supervisor', reason: 'The new supervisor failed its check: boom' }) as const),
    });

    // Act
    await runStartup(deps);

    // Assert
    expect(readUpdateState(ROOT)).toMatchObject({ step: 'check', result: 'failed', reason: 'The new supervisor failed its check: boom' });
    expect(existsSync(marker)).toBe(true);
    expect(existsSync(join(ROOT, '_update_old_supervisor'))).toBe(true);
    expect(deps.rollBack).toHaveBeenCalledTimes(1);
    expect(deps.launcher.release).not.toHaveBeenCalled();
    // The old version posts the failure on its rollback start.
    expect(deps.notify).not.toHaveBeenCalled();
  });

  it('rolls back instead of running unchecked when the check step cannot be written', async () => {
    // Arrange: a folder where the temp file goes makes every state write throw.
    swapped(state());
    mkdirSync(join(ROOT, 'data', 'update-state.json.tmp'));
    const deps = fakeStartup({ kind: 'check' }, state());

    // Act
    await runStartup(deps);

    // Assert
    expect(deps.check).not.toHaveBeenCalled();
    expect(deps.rollBack).toHaveBeenCalledTimes(1);
    expect(existsSync(marker)).toBe(true);
  });

  it('keeps update-checking until the passed result is written', async () => {
    // Arrange: the state write after the check throws.
    swapped(state());
    const deps = fakeStartup({ kind: 'check' }, state(), {
      check: vi.fn(async () => {
        mkdirSync(join(ROOT, 'data', 'update-state.json.tmp'));
        return { ok: true } as const;
      }),
    });

    // Act
    await runStartup(deps);

    // Assert
    expect(existsSync(marker)).toBe(true);
    expect(existsSync(join(ROOT, '_update_old_supervisor'))).toBe(true);
    expect(deps.launcher.release).not.toHaveBeenCalled();
    expect(deps.rollBack).toHaveBeenCalledTimes(1);
  });

  it('exits 76 again for a failed check that is still marked', async () => {
    // Arrange
    const s = state({ step: 'check', result: 'failed', reason: 'x' });
    swapped(s);
    const deps = fakeStartup({ kind: 'roll-back' }, s);

    // Act
    await runStartup(deps);

    // Assert
    expect(deps.check).not.toHaveBeenCalled();
    expect(deps.rollBack).toHaveBeenCalledTimes(1);
    expect(readUpdateState(ROOT)).toEqual(s);
    expect(deps.notify).not.toHaveBeenCalled();
  });

  it('finishes an update whose check passed before a restart', async () => {
    // Arrange
    const s = state({ step: 'start', result: 'ok' });
    swapped(s);
    const deps = fakeStartup({ kind: 'finish' }, s);

    // Act
    await runStartup(deps);

    // Assert
    expect(deps.check).not.toHaveBeenCalled();
    expect(existsSync(join(ROOT, '_update_old_ffmpeg'))).toBe(false);
    expect(existsSync(marker)).toBe(false);
    expect(deps.launcher.release).toHaveBeenCalledTimes(1);
    expect(readUpdateState(ROOT)?.step).toBe('done');
  });

  it('marks an interrupted update as failed on a first start', async () => {
    // Arrange: the window was closed while the update downloaded.
    const s = state({ step: 'download' });
    writeUpdateState(ROOT, s);
    mkdirSync(join(ROOT, '_update_tmp', 'supervisor'), { recursive: true });
    const action = decideStartup('first', readUpdateState(ROOT), existsSync(marker));
    const deps = fakeStartup(action, s);

    // Act
    await runStartup(deps);

    // Assert
    expect(action).toEqual({ kind: 'mark', result: 'failed', reason: RESTARTED });
    expect(holdAtStart(action, 'first', true, false)).toBe(false);
    expect(readUpdateState(ROOT)).toMatchObject({ step: 'download', result: 'failed', reason: 'StreamLoop was restarted during the update.' });
    expect(existsSync(join(ROOT, '_update_tmp'))).toBe(false);
    expect(deps.launcher.release).not.toHaveBeenCalled();
    expect(vi.mocked(deps.notify).mock.calls).toEqual([
      ['The update to 3.0.6 failed at downloading: StreamLoop was restarted during the update. Still on 3.0.5.', 'warn'],
    ]);
  });

  it('shows a rollback start as rolled back', async () => {
    // Arrange
    const s = state({ step: 'check', result: 'failed', reason: 'The new supervisor failed its check: boom' });
    writeUpdateState(ROOT, s);
    const deps = fakeStartup(decideStartup('rollback', s, false), s);

    // Act
    await runStartup(deps);

    // Assert
    expect(readUpdateState(ROOT)).toMatchObject({ step: 'check', result: 'rolled-back', reason: 'The new supervisor failed its check: boom' });
    expect(deps.rollBack).not.toHaveBeenCalled();
    expect(vi.mocked(deps.notify).mock.calls).toEqual([
      ['The update to 3.0.6 failed at checking the new version: The new supervisor failed its check: boom. Still on 3.0.5.', 'warn'],
    ]);
  });

  it('posts an update cut off during the swap once, as put back', async () => {
    // Arrange
    const s = state({ step: 'swap' });
    writeUpdateState(ROOT, s);
    const deps = fakeStartup(decideStartup('first', s, false), s);

    // Act
    await runStartup(deps);

    // Assert
    expect(vi.mocked(deps.notify).mock.calls).toEqual([
      ['The update to 3.0.6 failed at replacing files: The update was cut off, so the old version was put back. Still on 3.0.5.', 'warn'],
    ]);
  });

  it('posts nothing for an update that already has its result', async () => {
    // Arrange
    const s = state({ step: 'check', result: 'rolled-back', reason: 'x' });
    writeUpdateState(ROOT, s);
    const deps = fakeStartup(decideStartup('rollback', s, false), s);

    // Act
    await runStartup(deps);

    // Assert
    expect(deps.notify).not.toHaveBeenCalled();
  });

  it('survives a result it cannot write', async () => {
    // Arrange
    const s = state({ step: 'stop' });
    writeUpdateState(ROOT, s);
    mkdirSync(join(ROOT, 'data', 'update-state.json.tmp'));
    const deps = fakeStartup({ kind: 'mark', result: 'failed', reason: RESTARTED }, s);

    // Act + Assert: unrecorded, the next start records and posts it, so it is posted once.
    await expect(runStartup(deps)).resolves.toBeUndefined();
    expect(JSON.parse(readFileSync(join(ROOT, 'data', 'update-state.json'), 'utf8'))).toMatchObject({ result: 'running' });
    expect(deps.notify).not.toHaveBeenCalled();
  });
});
