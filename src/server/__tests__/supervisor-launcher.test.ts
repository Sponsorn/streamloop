import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import type { spawn } from 'child_process';
import { EventEmitter } from 'events';
import { installRoot, SupervisorLauncher } from '../supervisor-launcher.js';
import type { SupervisorClient, SupervisorStatus } from '../supervisor-client.js';
import { SPAWN_CWD } from '../spawn-cwd.js';
import { FFMPEG_MISSING, launchGuard, OBS_RUNNING, type Encoder, type RunFn } from '../setup-check.js';

class FakeChild extends EventEmitter {
  unref = vi.fn();
}

function setup(opts: { root?: string | null; answering?: boolean; debugFaults?: boolean; beforeLaunch?: () => Promise<string | null> } = {}) {
  let answering = opts.answering ?? false;
  const client = {
    status: vi.fn(async (): Promise<SupervisorStatus | null> => (answering ? { state: 'playing' } : null)),
    shutdown: vi.fn(async () => true),
  };
  const children: FakeChild[] = [];
  const spawnImpl = vi.fn(() => {
    const c = new FakeChild();
    children.push(c);
    return c;
  });
  const events: string[] = [];
  const launcher = new SupervisorLauncher({
    root: opts.root === undefined ? root : opts.root,
    client: client as unknown as SupervisorClient,
    spawnImpl: spawnImpl as unknown as typeof spawn,
    onEvent: (kind) => events.push(kind),
    debugFaults: opts.debugFaults,
    beforeLaunch: opts.beforeLaunch,
  });
  return { launcher, client, spawnImpl, children, events, setAnswering: (v: boolean) => { answering = v; } };
}

let root: string;
let exe: string;

beforeEach(() => {
  vi.useFakeTimers();
  root = mkdtempSync(join(tmpdir(), 'sl-launcher-'));
  mkdirSync(join(root, 'supervisor'));
  exe = join(root, 'supervisor', 'streamloop-supervisor.exe');
  writeFileSync(exe, '');
});

afterEach(() => {
  vi.useRealTimers();
  rmSync(root, { recursive: true, force: true });
});

describe('SupervisorLauncher', () => {
  it('shows why the supervisor refused config.json when it exits 3', async () => {
    // Arrange
    const { launcher, children } = setup();
    const d = new Date();
    const date = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
    mkdirSync(join(root, 'data', 'logs'), { recursive: true });
    writeFileSync(join(root, 'data', 'logs', `events-${date}.jsonl`),
      [
        { ts: 't', level: 'critical', kind: 'config-refused', reason: 'old' },
        { ts: 't', level: 'info', kind: 'start' },
        { ts: 't', level: 'critical', kind: 'config-refused', reason: 'config.json: ytdlCookiesFromBrowser is longer than 200 characters' },
      ].map((e) => JSON.stringify(e) + '\n').join(''));
    launcher.start();
    await vi.advanceTimersByTimeAsync(4_000);

    // Act
    children[0].emit('exit', 3);
    await vi.advanceTimersByTimeAsync(2_000);
    launcher.stop();

    // Assert
    expect(launcher.state()).toBe('down');
    expect(launcher.blockReason()).toBe('The supervisor refused config.json: config.json: ytdlCookiesFromBrowser is longer than 200 characters');
  });

  it('attaches without spawning when the supervisor already answers', async () => {
    // Arrange
    const { launcher, spawnImpl } = setup({ answering: true });

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(20_000);
    launcher.stop();

    // Assert
    expect(spawnImpl).not.toHaveBeenCalled();
    expect(launcher.state()).toBe('running');
  });

  it('spawns the launch intermediate once, hidden and not detached, after three missed polls', async () => {
    // Arrange
    const { launcher, spawnImpl, children, events } = setup();

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(2_000); // polls at 0 and 2 s
    const afterTwo = spawnImpl.mock.calls.length;
    await vi.advanceTimersByTimeAsync(2_000); // third miss at 4 s
    launcher.stop();

    // Assert
    expect(afterTwo).toBe(0);
    expect(spawnImpl).toHaveBeenCalledTimes(1);
    expect(spawnImpl).toHaveBeenCalledWith(exe, ['launch', '--root', root], {
      cwd: SPAWN_CWD,
      stdio: 'ignore',
      windowsHide: true,
    });
    expect(children[0].unref).toHaveBeenCalled();
    expect(launcher.state()).toBe('starting');
    expect(events).toContain('supervisor-launched');
  });

  it('passes --debug-faults only when asked to', async () => {
    // Arrange
    const plain = setup();
    const debug = setup({ debugFaults: true });

    // Act
    plain.launcher.start();
    debug.launcher.start();
    await vi.advanceTimersByTimeAsync(4_000);
    plain.launcher.stop();
    debug.launcher.stop();

    // Assert
    expect((plain.spawnImpl.mock.calls[0] as unknown[])[1]).toEqual(['launch', '--root', root]);
    expect((debug.spawnImpl.mock.calls[0] as unknown[])[1]).toEqual(['launch', '--root', root, '--debug-faults']);
  });

  it('does not spawn again while starting and inside the backoff', async () => {
    // Arrange
    const { launcher, spawnImpl } = setup();

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(4_000 + 9_000);
    const withinFirst = spawnImpl.mock.calls.length;
    await vi.advanceTimersByTimeAsync(2_000); // 10 s backoff over
    const afterFirst = spawnImpl.mock.calls.length;
    await vi.advanceTimersByTimeAsync(28_000); // 30 s backoff not over
    const withinSecond = spawnImpl.mock.calls.length;
    await vi.advanceTimersByTimeAsync(4_000);
    launcher.stop();

    // Assert
    expect(withinFirst).toBe(1);
    expect(afterFirst).toBe(2);
    expect(withinSecond).toBe(2);
    expect(spawnImpl).toHaveBeenCalledTimes(3);
  });

  it('never spawns when stopped by the user, until launchNow', async () => {
    // Arrange
    const { launcher, spawnImpl } = setup();
    launcher.markStoppedByUser();

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(600_000);
    const whileStopped = spawnImpl.mock.calls.length;
    const stateWhileStopped = launcher.state();
    launcher.launchNow();
    await vi.advanceTimersByTimeAsync(0);
    launcher.stop();

    // Assert
    expect(whileStopped).toBe(0);
    expect(stateWhileStopped).toBe('stopped-by-user');
    expect(spawnImpl).toHaveBeenCalledTimes(1);
  });

  it('stop() only stops watching and never shuts the supervisor down', async () => {
    // Arrange
    const { launcher, client } = setup({ answering: true });
    launcher.start();
    await vi.advanceTimersByTimeAsync(0);

    // Act
    launcher.stop();
    await vi.advanceTimersByTimeAsync(10_000);

    // Assert
    expect(client.shutdown).not.toHaveBeenCalled();
    expect(client.status).toHaveBeenCalledTimes(1);
  });

  it('reports missing, once, when the exe is absent at a launch attempt', async () => {
    // Arrange
    rmSync(exe);
    const { launcher, spawnImpl, events } = setup();

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(60_000);
    launcher.stop();

    // Assert
    expect(spawnImpl).not.toHaveBeenCalled();
    expect(launcher.state()).toBe('missing');
    expect(events.filter((e) => e === 'supervisor-missing')).toHaveLength(1);
  });

  it('reports missing without a root and never spawns', async () => {
    // Arrange
    const { launcher, spawnImpl } = setup({ root: null });

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(10_000);
    launcher.stop();

    // Assert
    expect(spawnImpl).not.toHaveBeenCalled();
    expect(launcher.state()).toBe('missing');
  });

  it('emits down once per outage and back when it answers again', async () => {
    // Arrange
    const { launcher, events, setAnswering } = setup({ answering: true });
    launcher.start();
    await vi.advanceTimersByTimeAsync(0);

    // Act
    setAnswering(false);
    await vi.advanceTimersByTimeAsync(30_000);
    setAnswering(true);
    await vi.advanceTimersByTimeAsync(2_000);
    launcher.stop();

    // Assert
    expect(events.filter((e) => e === 'supervisor-down')).toHaveLength(1);
    expect(events.filter((e) => e === 'supervisor-back')).toHaveLength(1);
    expect(launcher.state()).toBe('running');
  });

  it('stays starting when the intermediate exits 0 after handing off', async () => {
    // Arrange
    const { launcher, children } = setup();
    launcher.start();
    await vi.advanceTimersByTimeAsync(4_000);

    // Act
    children[0].emit('exit', 0);

    // Assert
    expect(launcher.state()).toBe('starting');
    launcher.stop();
  });

  it('treats an intermediate that relays exit 2 while another supervisor answers as attach', async () => {
    // Arrange
    const { launcher, spawnImpl, children, setAnswering } = setup();
    launcher.start();
    await vi.advanceTimersByTimeAsync(4_000);

    // Act: the port was taken by a supervisor that was still starting
    children[0].emit('exit', 2);
    setAnswering(true);
    await vi.advanceTimersByTimeAsync(2_000);
    setAnswering(false);
    await vi.advanceTimersByTimeAsync(6_000); // three misses, no backoff owed

    // Assert
    expect(spawnImpl).toHaveBeenCalledTimes(2);
  });

  it('resets the backoff after 60 s of running', async () => {
    // Arrange: four spawns (4 s, 14 s, 44 s, 104 s) put the next wait at 5 min
    const { launcher, spawnImpl, setAnswering } = setup();
    launcher.start();
    await vi.advanceTimersByTimeAsync(106_000);
    expect(spawnImpl).toHaveBeenCalledTimes(4);

    // Act
    setAnswering(true);
    await vi.advanceTimersByTimeAsync(62_000);
    setAnswering(false);
    await vi.advanceTimersByTimeAsync(6_000);
    const afterOutage = spawnImpl.mock.calls.length;
    launcher.stop();

    // Assert: the first spawn of a new outage has no backoff
    expect(afterOutage).toBe(5);
  });
});

describe('SupervisorLauncher fixes', () => {
  it('spawns exactly once when launchNow lands while a poll is in flight', async () => {
    // Arrange: stopped by the user, misses piled up, the next status() call hangs until released
    const { launcher, client, spawnImpl } = setup();
    launcher.markStoppedByUser();
    launcher.start();
    await vi.advanceTimersByTimeAsync(10_000);
    let release: (v: null) => void = () => {};
    client.status.mockImplementationOnce(() => new Promise((r) => { release = r; }));
    await vi.advanceTimersByTimeAsync(2_000); // that poll is now in flight

    // Act
    release(null); // its continuation is queued before launchNow runs
    launcher.launchNow();
    await vi.advanceTimersByTimeAsync(4_000);
    launcher.stop();

    // Assert
    expect(spawnImpl).toHaveBeenCalledTimes(1);
  });

  it('launchNow spawns at once without the backoff of earlier crashes', async () => {
    // Arrange: two spawns put the next wait at 30 s, then the user stops it
    const { launcher, spawnImpl } = setup();
    launcher.start();
    await vi.advanceTimersByTimeAsync(15_000);
    expect(spawnImpl).toHaveBeenCalledTimes(2);
    launcher.markStoppedByUser();

    // Act
    launcher.launchNow();
    await vi.advanceTimersByTimeAsync(0);
    launcher.stop();

    // Assert
    expect(spawnImpl).toHaveBeenCalledTimes(3);
  });

  it('a user stop is not an outage: no down, no back, no spawn', async () => {
    // Arrange
    const { launcher, spawnImpl, events, setAnswering } = setup({ answering: true });
    launcher.start();
    await vi.advanceTimersByTimeAsync(0);

    // Act
    launcher.markStoppedByUser();
    setAnswering(false);
    await vi.advanceTimersByTimeAsync(10_000);
    const stateAfterStop = launcher.state();
    setAnswering(true);
    launcher.launchNow();
    await vi.advanceTimersByTimeAsync(2_000);
    launcher.stop();

    // Assert
    expect(stateAfterStop).toBe('stopped-by-user');
    expect(spawnImpl).not.toHaveBeenCalled();
    expect(events).not.toContain('supervisor-down');
    expect(events).not.toContain('supervisor-back');
  });

  it('a Start soon after Stop is not an outage either', async () => {
    // Arrange: Stop, then Start before three misses
    const { launcher, spawnImpl, events, setAnswering } = setup({ answering: true });
    launcher.start();
    await vi.advanceTimersByTimeAsync(0);
    launcher.markStoppedByUser();
    setAnswering(false);
    await vi.advanceTimersByTimeAsync(2_000);

    // Act
    launcher.launchNow();
    await vi.advanceTimersByTimeAsync(6_000);
    launcher.stop();

    // Assert
    expect(spawnImpl).toHaveBeenCalledTimes(1);
    expect(events).not.toContain('supervisor-down');
  });

  it('stop then launchNow while the old one is still answering → exactly one spawn, no supervisor-down event', async () => {
    // Arrange: running, then Stop and Start back to back (the dashboard's Restart stream)
    const { launcher, spawnImpl, events, setAnswering } = setup({ answering: true });
    launcher.start();
    await vi.advanceTimersByTimeAsync(0);

    // Act: the old one keeps answering for 6 s while it shuts down, then goes away
    launcher.markStoppedByUser();
    launcher.launchNow();
    await vi.advanceTimersByTimeAsync(6_000);
    const spawnsWhileDraining = spawnImpl.mock.calls.length;
    setAnswering(false);
    await vi.advanceTimersByTimeAsync(10_000);
    launcher.stop();

    // Assert
    expect(spawnsWhileDraining).toBe(0);
    expect(spawnImpl).toHaveBeenCalledTimes(1);
    expect(events).not.toContain('supervisor-down');
    expect(launcher.isStoppedByUser()).toBe(false);
  });

  it('the old one never goes away within the bound → a plain reason, no down event', async () => {
    // Arrange
    const { launcher, spawnImpl, events, setAnswering } = setup({ answering: true });
    launcher.start();
    await vi.advanceTimersByTimeAsync(0);
    launcher.markStoppedByUser();
    launcher.launchNow();

    // Act
    await vi.advanceTimersByTimeAsync(5_000);
    const earlyReason = launcher.waitReason();
    await vi.advanceTimersByTimeAsync(25_000);
    const lateReason = launcher.waitReason();
    setAnswering(false);
    await vi.advanceTimersByTimeAsync(10_000);
    launcher.stop();

    // Assert: a reason the whole time, a different one past 20 s, and a quiet launch once it exits
    expect(earlyReason).toMatch(/waiting for the stopped supervisor/i);
    expect(lateReason).toMatch(/still running after 20 s/i);
    expect(events).not.toContain('supervisor-down');
    expect(spawnImpl).toHaveBeenCalledTimes(1);
    expect(launcher.waitReason()).toBeNull();
  });

  it("ignores an older child's exit once a newer spawn is starting", async () => {
    // Arrange: spawns at 4 s and 14 s
    const { launcher, children } = setup();
    launcher.start();
    await vi.advanceTimersByTimeAsync(15_000);

    // Act
    children[0].emit('exit', 1);

    // Assert
    expect(launcher.state()).toBe('starting');
    launcher.stop();
  });

  it('reports down when the current launch exits 1 (the supervisor crashed during the bind wait)', async () => {
    // Arrange: the first spawn at 4 s
    const { launcher, children } = setup();
    launcher.start();
    await vi.advanceTimersByTimeAsync(5_000);

    // Act
    children[0].emit('exit', 1);

    // Assert
    expect(launcher.state()).toBe('down');
    launcher.stop();
  });
});

describe('installRoot', () => {
  it('is the parent of app when a node sibling exists', () => {
    // Arrange
    mkdirSync(join(root, 'app'));
    mkdirSync(join(root, 'node'));

    // Act + Assert
    expect(installRoot(join(root, 'app'), { STREAMLOOP_ROOT: 'X:\\other' })).toBe(root);
  });

  it('comes from STREAMLOOP_ROOT in dev', () => {
    mkdirSync(join(root, 'app'));
    expect(installRoot(join(root, 'app'), { STREAMLOOP_ROOT: 'X:\\dev' })).toBe('X:\\dev');
  });

  it('is null otherwise', () => {
    mkdirSync(join(root, 'app'));
    expect(installRoot(join(root, 'app'), {})).toBeNull();
  });
});

describe('stopped-by-user marker', () => {
  const marker = (): string => join(root, 'data', 'stopped-by-user');

  it('a launcher constructed with the marker present starts stopped and never spawns', async () => {
    // Arrange
    mkdirSync(join(root, 'data'));
    writeFileSync(marker(), '');
    const { launcher, spawnImpl } = setup();

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(600_000);
    launcher.stop();

    // Assert
    expect(spawnImpl).not.toHaveBeenCalled();
    expect(launcher.state()).toBe('stopped-by-user');
    expect(launcher.isStoppedByUser()).toBe(true);
  });

  it('Stop writes the marker and Start removes it', () => {
    // Arrange
    const { launcher } = setup();

    // Act
    launcher.markStoppedByUser();
    const afterStop = existsSync(marker());
    launcher.launchNow();

    // Assert
    expect(afterStop).toBe(true);
    expect(existsSync(marker())).toBe(false);
  });

  it('Start still starts when the marker cannot be removed', () => {
    // Arrange: a folder at the marker path makes the delete throw, like a file held by antivirus
    const { launcher } = setup();
    mkdirSync(join(marker(), 'held'), { recursive: true });

    // Act
    const start = (): void => launcher.launchNow();

    // Assert
    expect(start).not.toThrow();
    expect(launcher.isStoppedByUser()).toBe(false);
  });
});
describe('launch guard', () => {
  type Sup = { encoder: Encoder; preferredEncoder?: Encoder };
  function guarded(o: {
    obs?: () => boolean;
    works?: (enc: Encoder, call: number) => boolean;
    sup?: Sup;
    answering?: boolean;
    ffmpeg?: boolean;
    guardDelayMs?: number;
  } = {}) {
    const sup = { value: o.sup ?? ({ encoder: 'h264_nvenc' } as Sup) };
    const saveEncoder = vi.fn((patch: { encoder: Encoder; preferredEncoder: Encoder }) => { sup.value = { ...sup.value, ...patch }; });
    const guardEvents: { kind: string; detail?: Record<string, unknown> }[] = [];
    let calls = 0;
    const run = vi.fn(async (_f: string, args: string[]) => {
      const enc = args[args.indexOf('-c:v') + 1] as Encoder;
      return (o.works ?? ((e: Encoder) => e === 'h264_nvenc'))(enc, ++calls);
    });
    const obsRunning = vi.fn(async () => {
      if (o.guardDelayMs) await new Promise((r) => setTimeout(r, o.guardDelayMs));
      return o.obs ? o.obs() : false;
    });
    const guard = launchGuard({
      root,
      getSupervisor: () => sup.value,
      saveEncoder,
      onEvent: (kind, detail) => guardEvents.push({ kind, detail }),
      obsRunning,
      run: run as RunFn,
      exists: () => o.ffmpeg ?? true,
    });
    return { ...setup({ beforeLaunch: guard, answering: o.answering }), saveEncoder, guardEvents, run, obsRunning, sup };
  }

  it('holds the launch while OBS runs, with the reason and one event, and launches once OBS closes', async () => {
    // Arrange
    let obs = true;
    const { launcher, spawnImpl, guardEvents } = guarded({ obs: () => obs });

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(20_000);
    const blockedReason = launcher.blockReason();
    const spawnsWhileBlocked = spawnImpl.mock.calls.length;
    obs = false;
    await vi.advanceTimersByTimeAsync(2_000);
    launcher.stop();

    // Assert
    expect(spawnsWhileBlocked).toBe(0);
    expect(blockedReason).toBe(OBS_RUNNING);
    expect(guardEvents.filter((e) => e.kind === 'supervisor-blocked-by-obs')).toHaveLength(1);
    expect(spawnImpl).toHaveBeenCalledTimes(1);
    expect(launcher.blockReason()).toBeNull();
  });

  it('falls back to amf when nvenc fails twice, keeps the preference, emits once, then spawns once', async () => {
    // Arrange
    const { launcher, spawnImpl, saveEncoder, guardEvents } = guarded({ works: (e) => e === 'h264_amf' || e === 'libx264' });

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(8_000);
    launcher.stop();

    // Assert
    expect(saveEncoder).toHaveBeenCalledWith({ encoder: 'h264_amf', preferredEncoder: 'h264_nvenc' });
    expect(guardEvents).toEqual([{ kind: 'encoder-fallback', detail: { from: 'h264_nvenc', to: 'h264_amf' } }]);
    expect(spawnImpl).toHaveBeenCalledTimes(1);
    expect(saveEncoder.mock.invocationCallOrder[0]).toBeLessThan(spawnImpl.mock.invocationCallOrder[0]);
  });

  it('keeps nvenc through a blip that passes on the retry: no fallback, no save', async () => {
    // Arrange
    const { launcher, spawnImpl, saveEncoder, guardEvents } = guarded({ works: (e, call) => e === 'h264_nvenc' && call > 1 });

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(8_000);
    launcher.stop();

    // Assert
    expect(saveEncoder).not.toHaveBeenCalled();
    expect(guardEvents).toEqual([]);
    expect(spawnImpl).toHaveBeenCalledTimes(1);
  });

  it('restores the preferred encoder once it works again, with one event', async () => {
    // Arrange: an earlier fallback left amf in use
    const { launcher, children, saveEncoder, guardEvents, sup, setAnswering } = guarded({ sup: { encoder: 'h264_amf', preferredEncoder: 'h264_nvenc' } });

    // Act: launch, then the supervisor answers, dies and is launched again
    launcher.start();
    await vi.advanceTimersByTimeAsync(5_000);
    children[0].emit('exit', 0);
    setAnswering(true);
    await vi.advanceTimersByTimeAsync(2_000);
    setAnswering(false);
    await vi.advanceTimersByTimeAsync(20_000);
    launcher.stop();

    // Assert
    expect(saveEncoder).toHaveBeenCalledTimes(1);
    expect(saveEncoder).toHaveBeenCalledWith({ encoder: 'h264_nvenc', preferredEncoder: 'h264_nvenc' });
    expect(sup.value).toEqual({ encoder: 'h264_nvenc', preferredEncoder: 'h264_nvenc' });
    expect(guardEvents).toEqual([{ kind: 'encoder-restored', detail: { from: 'h264_amf', to: 'h264_nvenc' } }]);
  });

  it('neither saves nor emits when the configured encoder works', async () => {
    // Arrange
    const { launcher, spawnImpl, saveEncoder, guardEvents } = guarded();

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(5_000);
    launcher.stop();

    // Assert
    expect(spawnImpl).toHaveBeenCalledTimes(1);
    expect(saveEncoder).not.toHaveBeenCalled();
    expect(guardEvents).toEqual([]);
  });

  it('never spawns when no encoder works: one alert, a plain reason, and at most one round of probes a minute', async () => {
    // Arrange
    const { launcher, spawnImpl, guardEvents, run } = guarded({ works: () => false });

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(30_000);
    const probesIn30s = run.mock.calls.length;
    await vi.advanceTimersByTimeAsync(40_000);
    const probesIn70s = run.mock.calls.length;
    launcher.stop();

    // Assert: 5 probes per round (nvenc twice, then the other three)
    expect(probesIn30s).toBe(5);
    expect(probesIn70s).toBe(10);
    expect(spawnImpl).not.toHaveBeenCalled();
    expect(launcher.blockReason()).toBe('No encoder works on this PC. Check the graphics driver.');
    expect(guardEvents.filter((e) => e.kind === 'no-encoder')).toHaveLength(1);
  });

  it('holds with its own reason when ffmpeg is missing, without probing', async () => {
    // Arrange
    const { launcher, spawnImpl, run, guardEvents } = guarded({ ffmpeg: false });

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(10_000);
    launcher.stop();

    // Assert
    expect(spawnImpl).not.toHaveBeenCalled();
    expect(run).not.toHaveBeenCalled();
    expect(launcher.blockReason()).toBe(FFMPEG_MISSING);
    expect(guardEvents).toEqual([{ kind: 'ffmpeg-missing', detail: undefined }]);
  });

  it('does not run on attach', async () => {
    // Arrange
    const { launcher, obsRunning, run } = guarded({ answering: true });

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(20_000);
    launcher.stop();

    // Assert
    expect(obsRunning).not.toHaveBeenCalled();
    expect(run).not.toHaveBeenCalled();
  });

  it('does not run again while a spawn waits out its backoff', async () => {
    // Arrange: the first spawn at 4 s owes a 10 s backoff
    const { launcher, obsRunning, spawnImpl } = guarded();

    // Act
    launcher.start();
    await vi.advanceTimersByTimeAsync(12_000);
    launcher.stop();

    // Assert
    expect(spawnImpl).toHaveBeenCalledTimes(1);
    expect(obsRunning).toHaveBeenCalledTimes(1);
  });

  it('does not spawn when Stop lands during a slow guard', async () => {
    // Arrange: the guard starts at 4 s and takes 3 s
    const { launcher, spawnImpl } = guarded({ guardDelayMs: 3_000 });
    launcher.start();
    await vi.advanceTimersByTimeAsync(5_000);

    // Act
    launcher.markStoppedByUser();
    await vi.advanceTimersByTimeAsync(20_000);
    launcher.stop();

    // Assert
    expect(spawnImpl).not.toHaveBeenCalled();
    expect(launcher.state()).toBe('stopped-by-user');
  });

  it('holds a Start while OBS runs and spawns on the first poll after OBS closes', async () => {
    // Arrange
    let obs = true;
    const { launcher, spawnImpl } = guarded({ obs: () => obs });
    launcher.markStoppedByUser();
    launcher.start();
    await vi.advanceTimersByTimeAsync(10_000);

    // Act
    launcher.launchNow();
    await vi.advanceTimersByTimeAsync(4_000);
    const held = launcher.blockReason();
    const spawnsWhileHeld = spawnImpl.mock.calls.length;
    obs = false;
    await vi.advanceTimersByTimeAsync(2_000);
    launcher.stop();

    // Assert
    expect(held).toBe(OBS_RUNNING);
    expect(spawnsWhileHeld).toBe(0);
    expect(spawnImpl).toHaveBeenCalledTimes(1);
  });

  it('a Start during drain that the guard holds launches on the first poll after the hold clears', async () => {
    // Arrange: running, then Stop and Start back to back, with OBS open
    let obs = true;
    const { launcher, spawnImpl, setAnswering } = guarded({ obs: () => obs, answering: true });
    launcher.start();
    await vi.advanceTimersByTimeAsync(0);
    launcher.markStoppedByUser();
    launcher.launchNow();

    // Act: the old one exits, the guard holds, then OBS closes
    setAnswering(false);
    await vi.advanceTimersByTimeAsync(4_000);
    const reasons = { wait: launcher.waitReason(), block: launcher.blockReason() };
    const spawnsWhileHeld = spawnImpl.mock.calls.length;
    obs = false;
    await vi.advanceTimersByTimeAsync(2_000);
    launcher.stop();

    // Assert
    expect(reasons).toEqual({ wait: null, block: OBS_RUNNING });
    expect(spawnsWhileHeld).toBe(0);
    expect(spawnImpl).toHaveBeenCalledTimes(1);
  });

  it('measures the port-taken window from the real spawn, after a slow guard', async () => {
    // Arrange: miss 3 at 4 s, the guard takes 3 s, so the spawn is at 7 s
    const { launcher, spawnImpl, children, setAnswering } = guarded({ guardDelayMs: 3_000 });
    launcher.start();
    await vi.advanceTimersByTimeAsync(10_000);

    // Act: exit 2 at 10 s is 3 s after the spawn but 6 s after the guard began; another supervisor answers
    children[0].emit('exit', 2);
    setAnswering(true);
    await vi.advanceTimersByTimeAsync(2_000);
    setAnswering(false);
    await vi.advanceTimersByTimeAsync(28_000);
    launcher.stop();

    // Assert: as an attach, spawn 2 (21 s) owes 10 s, not 30 s, so spawn 3 lands by 40 s
    expect(spawnImpl).toHaveBeenCalledTimes(3);
  });
});
