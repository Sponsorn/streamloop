import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'fs';
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

function setup(opts: { root?: string | null; answering?: boolean } = {}) {
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

  it('spawns once, detached, after three missed polls', async () => {
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
    expect(spawnImpl).toHaveBeenCalledWith(exe, ['--root', root], {
      cwd: SPAWN_CWD,
      detached: true,
      stdio: 'ignore',
      windowsHide: true,
    });
    expect(children[0].unref).toHaveBeenCalled();
    expect(launcher.state()).toBe('starting');
    expect(events).toContain('supervisor-launched');
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

  it('treats a spawn that exits with code 2 while another supervisor answers as attach', async () => {
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
