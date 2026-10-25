import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdirSync, rmSync, writeFileSync, appendFileSync } from 'fs';
import { join } from 'path';
import { SupervisorEvents, AlertGate, describe as describeEvent, launcherEvent, type SupervisorEvent } from '../supervisor-events.js';

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

  it('reads a file that does not exist yet from its start once it appears', async () => {
    const bridge = make();
    bridge.start();
    await bridge.poll();

    writeFileSync(join(dir, 'events-2026-10-25.jsonl'), line('born'));
    await bridge.poll();
    bridge.stop();

    expect(seen).toEqual(['born']);
  });

  it('skips an unparseable line in the middle of a batch', async () => {
    const f = join(dir, 'events-2026-10-25.jsonl');
    writeFileSync(f, '');
    const bridge = make();
    bridge.start();

    appendFileSync(f, line('a') + 'not json\n' + line('b'));
    await bridge.poll();
    bridge.stop();

    expect(seen).toEqual(['a', 'b']);
  });

  it('keeps reading the rest of a batch when the callback throws', async () => {
    const f = join(dir, 'events-2026-10-25.jsonl');
    writeFileSync(f, '');
    const got: string[] = [];
    const bridge = new SupervisorEvents({ logsDir: dir, intervalMs: 3_600_000, now: () => now, onEvent: (e) => { if (e.kind === 'a') throw new Error('boom'); got.push(e.kind); } });
    bridge.start();

    appendFileSync(f, line('a') + line('b'));
    await bridge.poll();
    bridge.stop();

    expect(got).toEqual(['b']);
  });

  it('does not replay yesterday or today when Node starts just after midnight', async () => {
    writeFileSync(join(dir, 'events-2026-10-25.jsonl'), line('yesterday'));
    writeFileSync(join(dir, 'events-2026-10-26.jsonl'), line('earlier-today'));
    now = new Date(2026, 9, 26, 0, 0, 5);
    const bridge = make();
    bridge.start();

    await bridge.poll();
    appendFileSync(join(dir, 'events-2026-10-26.jsonl'), line('new'));
    await bridge.poll();
    bridge.stop();

    expect(seen).toEqual(['new']);
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

  it.each(['token-server-spawn', 'token-server-down', 'token-server-restart', 'token-server-adopt', 'reload', 'start', 'control', 'playlist', 'youtube-probe-failed'])(
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
    expect(reason.length).toBeLessThanOrEqual(200);
  });

  it('builds launcher events with the right level', () => {
    expect(launcherEvent('supervisor-down').level).toBe('critical');
    expect(launcherEvent('supervisor-back').level).toBe('info');
  });
});

describe('freeze pair', () => {
  it('sends one alert for a watchdog freeze and keeps the ladder retry on the timeline only', () => {
    const w = describeEvent(ev('watchdog-freeze', { index: 1, outcome: 'retry' }, 'warn'));
    const r = describeEvent(ev('retry', { index: 1, id: 'x', why: 'freeze', retry: 1, pos: 3 }, 'warn'));

    expect(w.discord?.kind).toBe('outputFrozen');
    expect(r.discord).toBeUndefined();
    expect(r.message).not.toBe('');
  });

  it('keeps the ladder skip after a freeze on the timeline only, but a critical streak still alerts', () => {
    const s = describeEvent(ev('skip', { index: 1, id: 'x', why: 'freeze', consecutive: 1 }, 'warn'));
    const c = describeEvent(ev('skip', { index: 1, id: 'x', why: 'freeze', consecutive: 3 }, 'critical'));

    expect(s.discord).toBeUndefined();
    expect(c.discord?.kind).toBe('critical');
  });
});

describe('describe details', () => {
  it('sends an unknown critical kind to Discord as critical', () => {
    expect(describeEvent(ev('brand-new', {}, 'critical')).discord).toMatchObject({ kind: 'critical', level: 'error' });
  });

  it('uses only a string detail.reason, never a stringified object', () => {
    const obj = describeEvent(ev('skip', { index: 0, id: 'a', why: 'other', detail: { since: 12.3, pos: 40 }, consecutive: 1 }, 'warn'));
    const str = describeEvent(ev('skip', { index: 0, id: 'a', why: 'unavailable', detail: { reason: 'private video' }, consecutive: 1 }, 'warn'));

    expect(obj.discord?.vars.reason).toBe('other');
    expect(str.discord?.vars.reason).toBe('unavailable: private video');
  });
});

describe('AlertGate', () => {
  const kinds = (g: AlertGate, es: SupervisorEvent[]) => es.map((e) => g.describe(e).discord?.kind);

  it('alerts once per outage: one drop, one critical, one restart, every cycle on the timeline', () => {
    const gate = new AlertGate();
    const out: Array<ReturnType<AlertGate['describe']>> = [];
    for (let i = 1; i <= 20; i++) {
      out.push(gate.describe(ev('encoder-exit', { why: 'ingest', failures: i, restartInS: 5 }, i >= 3 ? 'critical' : 'warn')));
    }
    out.push(gate.describe(ev('encoder-start', { generation: 21, restarts: 20 })));
    const sent = out.map((d) => d.discord?.kind).filter(Boolean);

    expect(sent).toEqual(['streamDrop', 'critical', 'streamRestart']);
    expect(out.every((d) => d.message !== '')).toBe(true);
    expect(out).toHaveLength(21);
  });

  it('starts a new streak after the restart', () => {
    const got = kinds(new AlertGate(), [
      ev('encoder-exit', { why: 'x', failures: 1, restartInS: 1 }, 'warn'),
      ev('encoder-exit', { why: 'x', failures: 2, restartInS: 1 }, 'warn'),
      ev('encoder-exit', { why: 'x', failures: 3, restartInS: 1 }, 'critical'),
      ev('encoder-exit', { why: 'x', failures: 4, restartInS: 1 }, 'critical'),
      ev('encoder-start', { restarts: 4 }),
      ev('encoder-exit', { why: 'x', failures: 1, restartInS: 1 }, 'warn'),
    ]);

    expect(got).toEqual(['streamDrop', undefined, 'critical', undefined, 'streamRestart', 'streamDrop']);
  });

  it('alerts on the first encoder-spawn-failed since the last start only, then one restart', () => {
    const got = kinds(new AlertGate(), [
      ev('encoder-spawn-failed', { error: 'e' }, 'critical'),
      ev('encoder-spawn-failed', { error: 'e' }, 'critical'),
      ev('encoder-spawn-failed', { error: 'e' }, 'critical'),
      ev('encoder-start', { restarts: 0 }),
      ev('encoder-spawn-failed', { error: 'e' }, 'critical'),
    ]);

    expect(got).toEqual(['critical', undefined, undefined, 'streamRestart', 'critical']);
  });

  it('sends no restart for a start that follows no alert', () => {
    expect(new AlertGate().describe(ev('encoder-start', { restarts: 2 })).discord).toBeUndefined();
  });

  it('alerts token-server-spawn-failed once until the next token-server-spawn', () => {
    const got = kinds(new AlertGate(), [
      ev('token-server-spawn-failed', { error: 'e' }, 'critical'),
      ev('token-server-spawn-failed', { error: 'e' }, 'critical'),
      ev('token-server-spawn', {}),
      ev('token-server-spawn-failed', { error: 'e' }, 'critical'),
    ]);

    expect(got).toEqual(['critical', undefined, undefined, 'critical']);
  });

  it('re-arms the token alert when the supervisor adopts a running token server', () => {
    const got = kinds(new AlertGate(), [
      ev('token-server-spawn-failed', { error: 'e' }, 'critical'),
      ev('token-server-adopt', {}),
      ev('token-server-spawn-failed', { error: 'e' }, 'critical'),
    ]);

    expect(got).toEqual(['critical', undefined, 'critical']);
  });
});

describe('launch guard events', () => {
  it('puts the OBS block on the timeline only, as a warning', () => {
    const e = launcherEvent('supervisor-blocked-by-obs');
    expect(e.level).toBe('warn');
    expect(describeEvent(e).discord).toBeUndefined();
    expect(describeEvent(e).message).toMatch(/OBS is running/);
  });

  it('sends encoder-fallback as critical with both encoders named', () => {
    const e = launcherEvent('encoder-fallback', { from: 'h264_nvenc', to: 'h264_amf' });
    expect(e.level).toBe('critical');
    expect(describeEvent(e)).toEqual({
      message: 'The h264_nvenc encoder failed its test, so the stream uses h264_amf.',
      discord: { kind: 'critical', vars: { message: 'The h264_nvenc encoder failed its test, so the stream uses h264_amf.' }, level: 'error' },
    });
  });

  it('alerts encoder-fallback once per change', () => {
    const gate = new AlertGate();
    const fb = (from: string, to: string) => gate.describe(launcherEvent('encoder-fallback', { from, to })).discord?.kind;
    expect([fb('h264_nvenc', 'h264_amf'), fb('h264_nvenc', 'h264_amf'), fb('h264_amf', 'libx264')]).toEqual(['critical', undefined, 'critical']);
  });

  it('sends no-encoder as critical', () => {
    const e = launcherEvent('no-encoder');
    expect(describeEvent(e).discord?.vars.message).toBe('No encoder works on this PC. Check the graphics driver.');
  });
});
