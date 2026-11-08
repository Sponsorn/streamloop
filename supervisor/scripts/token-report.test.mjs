import { describe, it, expect } from 'vitest';
import { tokenReport, splitPairs } from './token-report.mjs';

const R403 = 'ERROR: unable to download video data: HTTP Error 403: Forbidden';
const csvRow = (ts, id, result, attempt, ms, reason = '', client = 'mweb') =>
  [ts, id, result, attempt, result === 'done' ? 1000 : 0, ms, reason, client].join(',');
const jsonl = (...evs) => evs.map((e) => JSON.stringify(e)).join('\n') + '\n';

describe('tokenReport', () => {
  it('counts a try as tokened when a [pot line falls inside it', () => {
    // Arrange
    const csv = '2026-11-01T08:41:39.570Z,Km2zpzd2dPU,retry,1,0,13997,ERROR: unable to download video data: HTTP Error 403: Forbidden,mweb\n';
    const log = '2026-11-01T08:41:31.058Z [youtube] [pot:bgutil:http] Generating a gvs PO Token for mweb client via bgutil HTTP server\n';

    // Act
    const r = tokenReport({ downloadsCsv: [csv], ytdlpLog: [log], events: [] });

    // Assert
    expect(r.rows[0]).toMatchObject({ token: true, client: 'mweb', result: 'retry' });
    expect([r.tokenTries, r.refused403]).toEqual([1, 1]);
  });

  it('does not count a [pot line outside the try or a line without [pot', () => {
    // Arrange: the try ran 08:41:25.570 to 08:41:39.570; the window opens 5 s earlier
    const csv = csvRow('2026-11-01T08:41:39.570Z', 'a', 'done', 0, 14000) + '\n';
    const log = [
      '2026-11-01T08:41:19.000Z [pot] too early',
      '2026-11-01T08:41:39.571Z [pot] too late',
      '2026-11-01T08:41:30.000Z [youtube] no token here',
    ].join('\n') + '\n';

    // Act
    const r = tokenReport({ downloadsCsv: [csv], ytdlpLog: [log], events: [] });

    // Assert
    expect(r.rows[0].token).toBe(false);
    expect(r.tokenTries).toBe(0);
    expect(r.doneShare).toBeNull();
  });

  it('reads the client list from the end of a row and keeps commas in the reason', () => {
    // Arrange
    const csv = '2026-11-01T08:00:00.000Z,v1,retry,1,0,5000,ERROR: a, b,mweb,web_embedded\n';

    // Act
    const [row] = tokenReport({ downloadsCsv: [csv], ytdlpLog: [], events: [] }).rows;

    // Assert
    expect(row.reason).toBe('ERROR: a, b');
    expect(row.client).toBe('mweb,web_embedded');
  });

  it('reads seven-field rows with no client', () => {
    // Arrange
    const csv = `2026-11-01T08:00:00.000Z,v1,retry,1,0,5000,${R403}\n2026-11-01T08:10:00.000Z,v1,done,0,99,5000,\n`;

    // Act
    const r = tokenReport({ downloadsCsv: [csv], ytdlpLog: [], events: [] });

    // Assert
    expect(r.rows.map((x) => x.client)).toEqual(['', '']);
    expect(r.rows[0].reason).toBe(R403);
    expect([r.tries, r.done, r.refused403]).toEqual([2, 1, 1]);
  });

  it('counts videos refused on both tries', () => {
    // Arrange
    const csv = [
      csvRow('2026-11-01T08:00:00.000Z', 'v1', 'retry', 1, 5000, R403),
      csvRow('2026-11-01T08:00:20.000Z', 'v1', 'retry', 2, 5000, R403),
      csvRow('2026-11-01T08:10:00.000Z', 'v1', 'done', 0, 5000),
      csvRow('2026-11-01T09:00:00.000Z', 'v2', 'retry', 1, 5000, R403),
      csvRow('2026-11-01T09:10:00.000Z', 'v2', 'done', 0, 5000),
      csvRow('2026-11-01T09:20:00.000Z', 'v3', 'retry', 1, 5000, 'ERROR: other'),
    ].join('\n') + '\n';

    // Act
    const r = tokenReport({ downloadsCsv: [csv], ytdlpLog: [], events: [] });

    // Assert
    expect(r.videosRefusedTwice).toBe(1);
    expect([r.tries, r.done, r.refused403, r.otherFailed]).toEqual([6, 2, 3, 1]);
  });

  it('does not count the size-resolve skip row as a try', () => {
    // Arrange
    const csv = [
      '2026-11-01T08:00:00.000Z,v1,skip,1,0,0,ERROR: gone,mweb,web_embedded',
      csvRow('2026-11-01T08:05:00.000Z', 'v2', 'skip', 0, 4000, R403),
    ].join('\n') + '\n';

    // Act
    const r = tokenReport({ downloadsCsv: [csv], ytdlpLog: [], events: [] });

    // Assert
    expect(r.tries).toBe(1);
    expect(r.refused403).toBe(1);
  });

  it('counts only rows from fromMs up to toMs', () => {
    // Arrange
    const csv = [
      csvRow('2026-11-01T08:00:00.000Z', 'a', 'done', 0, 1000),
      csvRow('2026-11-01T09:00:00.000Z', 'b', 'done', 0, 1000),
      csvRow('2026-11-01T10:00:00.000Z', 'c', 'done', 0, 1000),
    ].join('\n') + '\n';
    const events = jsonl({ ts: '2026-11-01T08:30:00.000Z', kind: 'youtube-breaker-open' }, { ts: '2026-11-01T10:30:00.000Z', kind: 'youtube-breaker-open' });

    // Act
    const r = tokenReport({ downloadsCsv: [csv], ytdlpLog: [], events: [events] }, Date.parse('2026-11-01T09:00:00Z'), Date.parse('2026-11-01T10:00:00Z'));

    // Assert
    expect(r.rows.map((x) => x.id)).toEqual(['b']);
    expect(r.breakerOpens).toBe(0);
  });

  it('counts breaker opens, cache clears and token server events from either events file name', () => {
    // Arrange: the file name does not matter to tokenReport, only the lines
    const old = jsonl({ ts: '2026-11-01T08:00:00Z', kind: 'youtube-breaker-open' }, { ts: '2026-11-01T08:01:00Z', kind: 'token-server-down' });
    const next = jsonl(
      { ts: '2026-11-02T08:00:00Z', kind: 'token-caches-cleared' },
      { ts: '2026-11-02T08:01:00Z', kind: 'token-server-restart' },
      { ts: '2026-11-02T08:02:00Z', kind: 'token-server-spawn-failed' },
      { ts: '2026-11-02T08:03:00Z', kind: 'downloads-failing' },
      { ts: '2026-11-02T08:04:00Z', kind: 'youtube-breaker-open' },
      { ts: '2026-11-02T08:05:00Z', kind: 'seam' },
    );

    // Act
    const r = tokenReport({ downloadsCsv: [], ytdlpLog: [], events: [old, next] });

    // Assert
    expect([r.breakerOpens, r.cacheClears, r.serverDown, r.serverRestart, r.spawnFailed, r.downloadsFailing]).toEqual([2, 1, 1, 1, 1, 1]);
  });
});

describe('splitPairs', () => {
  const row = (id, result, refused) => ({ id, result, reason: refused ? R403 : '' });

  it("pairs each video's first try across two sides", () => {
    // Arrange
    const a = [row('v1', 'done'), row('v2', 'retry', true)];
    const b = [row('v1', 'retry', true), row('v2', 'retry', true), row('v3', 'done')];

    // Act
    const r = splitPairs(a, b);

    // Assert
    expect(r).toMatchObject({ aDoneBRefused: 1, bDoneARefused: 0 });
    expect(r.list).toEqual([{ id: 'v1', a: 'done', b: 'refused' }]);
  });

  it('uses only the first try of a video on each side', () => {
    // Arrange: B's second try succeeds, which does not undo the first refusal
    const a = [row('v1', 'done')];
    const b = [row('v1', 'retry', true), row('v1', 'done')];

    // Act
    const r = splitPairs(a, b);

    // Assert
    expect(r.aDoneBRefused).toBe(1);
  });

  it('counts the other direction too', () => {
    // Arrange
    const a = [row('v1', 'retry', true)];
    const b = [row('v1', 'done')];

    // Act
    const r = splitPairs(a, b);

    // Assert
    expect(r).toMatchObject({ aDoneBRefused: 0, bDoneARefused: 1 });
    expect(r.list[0]).toEqual({ id: 'v1', a: 'refused', b: 'done' });
  });
});
