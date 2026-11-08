import { describe, it, expect } from 'vitest';
import { mkdtempSync, mkdirSync, writeFileSync, existsSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  parseArgs, parseSha256File, pairConfig, supervisorArgs, tsServerArgs, judgeIdentity, bgutilPlugins,
  lastSpawnPid, parsePrivateMb, memoryCsvRow, assembleReport, ownProcesses, removeTree, PORTS,
  pingIdentity, judgeA, notePlaying, playingVerdict,
} from './pair-session.mjs';

const R403 = 'ERROR: unable to download video data: HTTP Error 403: Forbidden';
const row = (ts, id, result, ms = 1000, reason = '') => [ts, id, result, 0, result === 'done' ? 1 : 0, ms, reason, 'mweb'].join(',');

describe('parseArgs', () => {
  it('reads a calibration run with the second TypeScript server', () => {
    // Arrange / Act
    const o = parseArgs(['cal', '--playlist', 'PLa,PLb', '--hours', '2', '--b-ts']);

    // Assert
    expect(o).toMatchObject({ name: 'cal', playlists: ['PLa', 'PLb'], hours: 2, bTs: true, bPot: null, bExpect: 'bgutil 2.0.0', cookies: '' });
  });

  it('expects the Rust server by default when B owns one', () => {
    const o = parseArgs(['q', '--playlist', 'PLa', '--hours', '0.25', '--b-pot', 'x.exe']);

    expect(o.bTs).toBe(false);
    expect(o.bExpect).toBe('streamloop-pot');
  });

  it('refuses a run that names both or neither server option', () => {
    expect(() => parseArgs(['n', '--playlist', 'P', '--hours', '1'])).toThrow(/exactly one/);
    expect(() => parseArgs(['n', '--playlist', 'P', '--hours', '1', '--b-ts', '--b-pot', 'x'])).toThrow(/exactly one/);
  });

  it('refuses a missing playlist, a bad duration and an unknown flag', () => {
    expect(() => parseArgs(['n', '--hours', '1', '--b-ts'])).toThrow(/--playlist/);
    expect(() => parseArgs(['n', '--playlist', 'P', '--hours', 'abc', '--b-ts'])).toThrow(/--hours/);
    expect(() => parseArgs(['n', '--playlist', 'P', '--hours', '1', '--b-ts', '--nope', 'x'])).toThrow(/unknown flag/);
  });
});

describe('parseSha256File', () => {
  it('takes the hash and the file name from a "<hash>  <filename>" line', () => {
    const hash = 'F'.repeat(64);

    expect(parseSha256File(`${hash}  streamloop-v3.0.5.zip\n`)).toEqual({ hash: 'f'.repeat(64), file: 'streamloop-v3.0.5.zip' });
  });

  it('throws on a line that is not a checksum', () => {
    expect(() => parseSha256File('hello')).toThrow();
  });
});

describe('pairConfig', () => {
  it('writes the brief values and an empty cookie setting by default', () => {
    const c = pairConfig({ playlists: ['PLa', 'PLb'] });

    expect(c).toEqual({
      playlists: [{ id: 'PLa' }, { id: 'PLb' }],
      ytdlCookiesFromBrowser: '',
      supervisor: { encoder: 'h264_nvenc', playerClient: 'mweb', cacheBudgetMb: 300, readAheadSeconds: 20, overlay: true, freezeSeconds: 20, silenceSeconds: 30 },
    });
  });

  it('puts --cookies in place of the empty setting', () => {
    expect(pairConfig({ playlists: ['P'], cookies: 'firefox' }).ytdlCookiesFromBrowser).toBe('firefox');
  });
});

describe('supervisorArgs', () => {
  it('starts A on default ports with no debug flag', () => {
    expect(supervisorArgs('a', 'R')).toEqual(['--root', 'R', '--target', 'rtmp://127.0.0.1:1935/live/a']);
  });

  it('starts B on its own ports behind --debug-faults', () => {
    const args = supervisorArgs('b', 'R');

    expect(args).toEqual(['--root', 'R', '--debug-faults', '--api-port', '4427', '--token-port', '4426', '--target', 'rtmp://127.0.0.1:1935/live/b']);
    expect([PORTS.bApi, PORTS.bToken]).toEqual([4427, 4426]);
  });

  it('starts the second TypeScript server on B\'s token port from A\'s install', () => {
    const args = tsServerArgs('A');

    expect(args.slice(1)).toEqual(['-H', '127.0.0.1', '-p', '4426']);
    expect(args[0].replaceAll('\\', '/')).toBe('A/pot/build/main.js');
  });
});

describe('judgeIdentity', () => {
  it('waits while B has not named a server yet', () => {
    expect(judgeIdentity(null, 'bgutil 2.0.0', 5000)).toBe('wait');
  });

  it('accepts a server whose name contains the expected text', () => {
    expect(judgeIdentity('bgutil 2.0.0', 'bgutil 2.0.0', 3000)).toBe('ok');
    expect(judgeIdentity('streamloop-pot 0.1.0', 'streamloop-pot', 3000)).toBe('ok');
  });

  it('calls the wrong server a mismatch only at the 60 s mark', () => {
    expect(judgeIdentity('unknown', 'bgutil 2.0.0', 59_000)).toBe('wait');
    expect(judgeIdentity('unknown', 'bgutil 2.0.0', 60_000)).toBe('mismatch');
    expect(judgeIdentity('streamloop-pot 0.1.0', 'bgutil 2.0.0', 61_000)).toBe('mismatch');
  });

  it('treats a server that never named itself as a mismatch at the limit', () => {
    expect(judgeIdentity(null, 'bgutil 2.0.0', 60_000)).toBe('mismatch');
  });
});

describe('bgutilPlugins', () => {
  it('lists plugin files starting with bgutil, whatever the case', () => {
    expect(bgutilPlugins(['bgutil-ytdlp-pot-provider.zip', 'Other.zip', 'BGUTIL-x.zip'])).toEqual(['bgutil-ytdlp-pot-provider.zip', 'BGUTIL-x.zip']);
  });

  it('lists nothing for an unrelated folder', () => {
    expect(bgutilPlugins(['other.zip'])).toEqual([]);
  });
});

describe('lastSpawnPid', () => {
  it('follows the newest spawn event across files', () => {
    const e = (kind, pid) => JSON.stringify({ ts: 't', kind, pid });
    const day1 = `${e('token-server-spawn', 100)}\n${e('start', 1)}\n`;
    const day2 = `${e('token-server-spawn', 200)}\n{"half`;

    expect(lastSpawnPid([day1, day2])).toBe(200);
  });

  it('returns null when no server was spawned', () => {
    expect(lastSpawnPid(['{"kind":"start"}\n'])).toBeNull();
  });
});

describe('memory sampling', () => {
  it('turns CIM working-set bytes into megabytes per pid, for one row or several', () => {
    const one = parsePrivateMb('{"IDProcess":10,"WorkingSetPrivate":104857600}');
    const two = parsePrivateMb('[{"IDProcess":10,"WorkingSetPrivate":104857600},{"IDProcess":11,"WorkingSetPrivate":1572864}]');

    expect(one.get(10)).toBe(100);
    expect([two.get(10), two.get(11)]).toEqual([100, 1.5]);
  });

  it('returns an empty map for an empty answer', () => {
    expect(parsePrivateMb('').size).toBe(0);
  });

  it('writes a row with empty cells for a pid that is not known', () => {
    expect(memoryCsvRow('T', 'a', 7, 12.5)).toBe('T,a,7,12.5');
    expect(memoryCsvRow('T', 'b', null, undefined)).toBe('T,b,,');
  });
});

describe('assembleReport', () => {
  const files = (csv) => ({ downloadsCsv: [csv], ytdlpLog: [], events: [] });
  const from = Date.parse('2026-11-08T10:00:00Z');
  const to = Date.parse('2026-11-08T12:00:00Z');

  it('reports each side over the window and the videos only one side got', () => {
    // Arrange
    const a = files(`${row('2026-11-08T10:30:00.000Z', 'v1', 'done')}\n${row('2026-11-08T13:30:00.000Z', 'v9', 'done')}\n`);
    const b = files(`${row('2026-11-08T10:30:00.000Z', 'v1', 'retry', 1000, R403)}\n`);

    // Act
    const r = assembleReport(a, b, from, to);

    // Assert
    expect(r.window).toEqual({ from: '2026-11-08T10:00:00.000Z', to: '2026-11-08T12:00:00.000Z' });
    expect([r.a.tries, r.b.tries]).toEqual([1, 1]);
    expect(r.pairs.aDoneBRefused).toBe(1);
    expect(r.pairs.list).toEqual([{ id: 'v1', a: 'done', b: 'refused' }]);
  });

  it('keeps every try row however many there are, with how the run ended', () => {
    // Arrange
    const many = Array.from({ length: 300 }, (_, i) => row(`2026-11-08T10:30:00.${String(i).padStart(3, '0')}Z`, `v${i}`, 'done')).join('\n') + '\n';
    const b = files(`${row('2026-11-08T10:30:00.000Z', 'v0', 'retry', 1000, R403)}\n`);

    // Act
    const r = assembleReport(files(many), b, from, to, { endedBy: 'not-playing', identity: { a: 'bgutil 2.0.0', b: 'unknown' } });

    // Assert
    expect(r.a.rows).toHaveLength(300);
    expect(r.b.rows).toHaveLength(1);
    expect(r.pairs.list).toHaveLength(1);
    expect(r).toMatchObject({ endedBy: 'not-playing', identity: { a: 'bgutil 2.0.0', b: 'unknown' } });
  });
});

describe('ownProcesses', () => {
  const procs = [
    { pid: 1, name: 'node.exe', path: 'C:\\Users\\x\\AppData\\Local\\Temp\\sl-pair-t\\a\\node\\node.exe', commandLine: '' },
    { pid: 2, name: 'node.exe', path: 'C:\\Program Files\\nodejs\\node.exe', commandLine: 'node other.js' },
    { pid: 3, name: 'mediamtx.exe', path: 'D:\\tools\\mediamtx.exe', commandLine: '' },
    { pid: 4, name: 'ffmpeg.exe', path: '', commandLine: '"c:/users/x/appdata/local/temp/sl-pair-t/b/ffmpeg/ffmpeg.exe" -i y' },
  ];
  const base = 'C:\\Users\\x\\AppData\\Local\\Temp\\sl-pair-t';

  it('keeps processes the run started or that run from under its folder, whatever the slash style', () => {
    expect(ownProcesses(procs, new Map([[3, 'mediamtx.exe']]), base).map((p) => p.pid)).toEqual([1, 3, 4]);
  });

  it('leaves out a process that is not the run\'s', () => {
    expect(ownProcesses(procs, new Map(), base).map((p) => p.pid)).toEqual([1, 4]);
  });

  it('does not take a started pid that now belongs to a process with another name', () => {
    expect(ownProcesses(procs, new Map([[2, 'streamloop-supervisor.exe']]), base).map((p) => p.pid)).toEqual([1, 4]);
  });

  it('does not match a sibling folder that shares the prefix', () => {
    const sibling = [
      { pid: 5, name: 'node.exe', path: 'C:\\Users\\x\\AppData\\Local\\Temp\\sl-pair-t2\\a\\node\\node.exe', commandLine: '' },
      { pid: 6, name: 'node.exe', path: '', commandLine: 'node C:/Users/x/AppData/Local/Temp/sl-pair-t2/a/pot/build/main.js' },
    ];

    expect(ownProcesses(sibling, new Map(), base)).toEqual([]);
  });
});

describe('pingIdentity', () => {
  it('names the implementation when /ping carries one', () => {
    expect(pingIdentity(200, '{"version":"2.0.0","implementation":"streamloop-pot 0.1.0"}')).toBe('streamloop-pot 0.1.0');
  });

  it('names bgutil with the version when only the version is there', () => {
    expect(pingIdentity(200, '{"server_uptime":3,"version":"2.0.0"}')).toBe('bgutil 2.0.0');
  });

  it('says unknown for a 200 that is not JSON or names nothing', () => {
    expect(pingIdentity(200, 'pong')).toBe('unknown');
    expect(pingIdentity(200, '{}')).toBe('unknown');
  });

  it('is null when nothing answered 200', () => {
    expect(pingIdentity(500, '{"version":"2.0.0"}')).toBeNull();
  });
});

describe('judgeA', () => {
  it('accepts a bgutil server the supervisor owns', () => {
    expect(judgeA('bgutil 2.0.0', 'owned', 4000)).toBe('ok');
  });

  it('waits for ownership and then calls an adopted bgutil server a mismatch at the limit', () => {
    expect(judgeA('bgutil 2.0.0', 'adopted', 10_000)).toBe('wait');
    expect(judgeA('bgutil 2.0.0', 'adopted', 60_000)).toBe('mismatch');
  });

  it('calls another server a mismatch at the limit', () => {
    expect(judgeA('streamloop-pot 0.1.0', 'owned', 60_000)).toBe('mismatch');
  });
});

describe('playing checks', () => {
  it('records the first time a side said playing and keeps it', () => {
    expect(notePlaying(null, { state: 'starting' }, 5)).toBeNull();
    expect(notePlaying(null, { state: 'playing' }, 7)).toBe(7);
    expect(notePlaying(7, { state: 'recovering' }, 9)).toBe(7);
    expect(notePlaying(null, null, 9)).toBeNull();
  });

  it('fails only when a side has not played by the limit', () => {
    expect(playingVerdict(1, 2, 999_999_999)).toBe('ok');
    expect(playingVerdict(1, null, 599_000)).toBe('wait');
    expect(playingVerdict(1, null, 600_000)).toBe('fail');
    expect(playingVerdict(null, null, 600_000)).toBe('fail');
  });
});

describe('removeTree', () => {
  it('removes a folder but leaves the target of a link inside it alone', () => {
    // Arrange
    const dir = mkdtempSync(join(tmpdir(), 'pair-rm-'));
    const target = join(dir, 'target');
    const victim = join(dir, 'victim');
    mkdirSync(target);
    writeFileSync(join(target, 'keep.txt'), 'x');
    mkdirSync(join(victim, 'sub'), { recursive: true });
    writeFileSync(join(victim, 'sub', 'f.txt'), 'y');
    symlinkSync(target, join(victim, 'link'), 'junction');

    // Act
    removeTree(victim);

    // Assert
    expect(existsSync(victim)).toBe(false);
    expect(existsSync(join(target, 'keep.txt'))).toBe(true);
    removeTree(dir);
  });
});
