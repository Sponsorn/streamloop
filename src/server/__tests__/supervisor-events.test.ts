import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdirSync, rmSync, writeFileSync, appendFileSync } from 'fs';
import { join } from 'path';
import { SupervisorEvents, describe as describeEvent, launcherEvent, type SupervisorEvent } from '../supervisor-events.js';

const dir = join(process.cwd(), '.test-tmp', 'supervisor-events');
const ev = (kind: string, extra: Record<string, unknown> = {}, level: SupervisorEvent['level'] = 'info'): SupervisorEvent =>
  ({ ts: '2026-10-25T10:00:00Z', level, kind, ...extra });
const line = (kind: string) => JSON.stringify(ev(kind)) + '\n';

describe('SupervisorEvents', () => {
  let now = new Date(2026, 9, 25, 12, 0, 0);
  let seen: string[];
  const make = () => new SupervisorEvents({ logsDir: dir, onEvent: (e) => seen.push(e.kind), intervalMs: 3_600_000, now: () => now });

  beforeEach(() => {
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(dir, { recursive: true });
    now = new Date(2026, 9, 25, 12, 0, 0);
    seen = [];
  });
  afterEach(() => { rmSync(dir, { recursive: true, force: true }); });

  it('reads appended lines once and skips history on first start', async () => {
    const f = join(dir, 'events-2026-10-25.jsonl');
    writeFileSync(f, line('old'));
    const bridge = make();
    bridge.start();

    appendFileSync(f, line('a') + line('b'));
    await bridge.poll();
    await bridge.poll();
    bridge.stop();

    expect(seen).toEqual(['a', 'b']);
  });

  it('holds a half-written line until it is finished', async () => {
    const f = join(dir, 'events-2026-10-25.jsonl');
    writeFileSync(f, '');
    const bridge = make();
    bridge.start();

    const full = line('late');
    appendFileSync(f, full.slice(0, 10));
    await bridge.poll();
    expect(seen).toEqual([]);
    appendFileSync(f, full.slice(10));
    await bridge.poll();
    bridge.stop();

    expect(seen).toEqual(['late']);
  });

  it('finishes yesterday then reads today from the start at midnight', async () => {
    const y = join(dir, 'events-2026-10-25.jsonl');
    writeFileSync(y, '');
    const bridge = make();
    bridge.start();

    appendFileSync(y, line('last-of-day'));
    writeFileSync(join(dir, 'events-2026-10-26.jsonl'), line('first-of-day'));
    now = new Date(2026, 9, 26, 0, 0, 1);
    await bridge.poll();
    bridge.stop();

    expect(seen).toEqual(['last-of-day', 'first-of-day']);
  });

  it('rereads from 0 when the file shrank', async () => {
    const f = join(dir, 'events-2026-10-25.jsonl');
    writeFileSync(f, line('before') + line('before2'));
    const bridge = make();
    bridge.start();

    writeFileSync(f, line('new'));
    await bridge.poll();
    bridge.stop();

    expect(seen).toEqual(['new']);
  });
});

describe('describe', () => {
  it('maps encoder-exit to streamDrop and a third failure to critical', () => {
    const warn = describeEvent(ev('encoder-exit', { why: 'crash', failures: 1, restartInS: 2 }, 'warn'));
    const crit = describeEvent(ev('encoder-exit', { why: 'crash', failures: 3, restartInS: 5 }, 'critical'));

    expect(warn.discord).toMatchObject({ kind: 'streamDrop', level: 'warn', vars: { attempt: 1 } });
    expect(crit.discord).toMatchObject({ kind: 'critical', level: 'error' });
  });

  it('maps encoder-start to streamRestart only after an exit', () => {
    expect(describeEvent(ev('encoder-start', { generation: 1, restarts: 0 })).discord).toBeUndefined();
    expect(describeEvent(ev('encoder-start', { generation: 2, restarts: 1 })).discord).toMatchObject({ kind: 'streamRestart', vars: { attempts: 1 } });
  });

  it('maps skip, retry and resume with 1-based video numbers', () => {
    expect(describeEvent(ev('skip', { index: 3, id: 'abc', why: 'unavailable', consecutive: 1 }, 'warn')).discord)
      .toMatchObject({ kind: 'skip', vars: { videoIndex: 4, videoId: 'abc', reason: 'unavailable' } });
    expect(describeEvent(ev('retry', { index: 0, id: 'x', why: 'stall', retry: 2, pos: 12.5 }, 'warn')).discord)
      .toMatchObject({ kind: 'error', vars: { videoIndex: 1, errorCode: 'stall', attempt: 2 } });
    expect(describeEvent(ev('resume', { index: 1, id: 'r', pos: 30 })).discord)
      .toMatchObject({ kind: 'resume', vars: { videoIndex: 2, videoId: 'r' } });
  });

  it('sends a third skip in a row as critical', () => {
    const d = describeEvent(ev('skip', { index: 2, id: 'z', why: 'x', consecutive: 3 }, 'critical'));
    expect(d.discord?.kind).toBe('critical');
  });

  it('sends watchdog retries and skips to outputFrozen, content and ignored to the timeline only', () => {
    const w = (kind: string, outcome: string) => describeEvent(ev(kind, { index: 1, outcome }, 'warn'));
    expect(w('watchdog-freeze', 'retry').discord).toMatchObject({ kind: 'outputFrozen', vars: { kind: 'freeze', videoIndex: 2 } });
    expect(w('watchdog-silence', 'skip').discord).toMatchObject({ kind: 'outputFrozen', vars: { kind: 'silence' } });
    expect(w('watchdog-silence', 'content').discord).toBeUndefined();
    expect(w('watchdog-freeze', 'ignored').discord).toBeUndefined();
    expect(w('watchdog-silence', 'content').message).not.toBe('');
  });

  it.each([
    ['all-skipped', {}], ['stream-key-unreadable', { reason: 'r' }], ['stream-key-missing', {}],
    ['youtube-breaker-open', { reason: 'bot' }], ['downloads-failing', { inARow: 3 }],
    ['feeder-spawn-failed', { index: 0, error: 'e' }], ['encoder-spawn-failed', { error: 'e' }],
    ['supervisor-down', {}], ['supervisor-missing', {}],
  ])('sends %s as critical', (kind, extra) => {
    expect(describeEvent(ev(kind, extra, 'critical')).discord).toMatchObject({ kind: 'critical', level: 'error' });
  });

  it('sends playlist-failed as critical only for the first load', () => {
    expect(describeEvent(ev('playlist-failed', { reason: 'r' }, 'critical')).discord?.kind).toBe('critical');
    expect(describeEvent(ev('playlist-failed', { reason: 'r' }, 'warn')).discord).toBeUndefined();
  });

  it('maps supervisor-back to an info alert and supervisor-launched to the timeline', () => {
    expect(describeEvent(ev('supervisor-back')).discord).toMatchObject({ kind: 'recovery', level: 'info', vars: { step: 'Supervisor answering again' } });
    expect(describeEvent(ev('supervisor-launched')).discord).toBeUndefined();
  });

  it.each(['token-server-spawn', 'token-server-down', 'token-server-restart', 'token-server-adopt', 'token-server-spawn-failed', 'reload', 'start', 'control', 'playlist', 'youtube-probe-failed'])(
    'keeps %s off Discord', (kind) => {
      expect(describeEvent(ev(kind, { error: 'e', reason: 'r' })).discord).toBeUndefined();
    });

  it.each(['feeder-start', 'handover', 'spare-resolve', 'spare-used', 'youtube-probe', 'download-cancel'])(
    'keeps %s out of the timeline and Discord', (kind) => {
      const d = describeEvent(ev(kind));
      expect(d.message).toBe('');
      expect(d.discord).toBeUndefined();
    });

  it('puts an unknown kind on the timeline by name', () => {
    expect(describeEvent(ev('something-new'))).toEqual({ message: 'something-new' });
  });

  it('trims detail to one line of at most 200 characters', () => {
    const d = describeEvent(ev('skip', { index: 0, id: 'a', why: 'x', detail: 'first\n' + 'y'.repeat(500), consecutive: 1 }, 'warn'));
    const reason = String(d.discord?.vars.reason);
    expect(reason).not.toContain('\n');
    expect(reason.length).toBeLessThanOrEqual(210);
  });

  it('builds launcher events with the right level', () => {
    expect(launcherEvent('supervisor-down').level).toBe('critical');
    expect(launcherEvent('supervisor-back').level).toBe('info');
  });
});
