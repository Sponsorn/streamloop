import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import type { spawn } from 'child_process';
import { EventEmitter } from 'events';
import { installRoot, SupervisorLauncher } from '../supervisor-launcher.js';
import type { SupervisorClient, SupervisorStatus } from '../supervisor-client.js';
import { SPAWN_CWD } from '../spawn-cwd.js';

class FakeChild extends EventEmitter {
  unref = vi.fn();
}

function setup(opts: { root?: string | null; answering?: boolean; debugFaults?: boolean } = {}) {
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
});