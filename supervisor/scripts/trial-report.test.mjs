import { describe, it, expect } from 'vitest';
import { trialReport } from './trial-report.mjs';

const T0 = Date.parse('2026-10-26T22:00:00Z');
const at = (s) => new Date(T0 + s * 1000).toISOString();
const ev = (s, kind, level = 'info', extra = {}) => JSON.stringify({ ts: at(s), level, kind, ...extra });
const jsonl = (...lines) => lines.join('\n') + '\n';
// encoder-DATE.csv: a progress row leaves the CPU columns empty, a 10 s row fills them
const cpuRow = (s, speed, enc, feed, sys) => `${T0 + s * 1000},0,${speed}x,0,${speed},${enc},${feed},${sys}`;
const progressRow = (s) => `${T0 + s * 1000},0,1.0x,0,,,,`;

describe('trialReport', () => {
  it('measures an encoder outage from its exit to the next start, with the reason', () => {
    // Arrange
    const events = [jsonl(ev(0, 'start'), ev(1, 'encoder-start'), ev(3600, 'encoder-exit', 'warn', { failures: 1 }), ev(3607, 'encoder-start'))];

    // Act
    const r = trialReport({ events, encoderCsv: [] }, T0, T0 + 7200 * 1000);

    // Assert
    expect(r.offAir.seconds).toBe(7);
    expect(r.offAir.gaps).toEqual([{ from: at(3600), to: at(3607), seconds: 7, why: 'encoder-exit' }]);
  });

  it('closes an outage still open at the end of the window there', () => {
    // Arrange
    const events = [jsonl(ev(0, 'encoder-start'), ev(100, 'encoder-exit', 'critical', { failures: 3 }), ev(105, 'encoder-spawn-failed', 'critical'))];

    // Act
    const r = trialReport({ events, encoderCsv: [] }, T0, T0 + 160 * 1000);

    // Assert
    expect(r.offAir.gaps).toEqual([{ from: at(100), to: at(160), seconds: 60, why: 'encoder-exit' }]);
  });

  it('counts a window that starts during an outage from the window start', () => {
    // Arrange
    const events = [jsonl(ev(0, 'encoder-start'), ev(50, 'shutdown'), ev(200, 'start'), ev(202, 'encoder-start'))];

    // Act
    const r = trialReport({ events, encoderCsv: [] }, T0 + 100 * 1000, T0 + 300 * 1000);

    // Assert
    expect(r.offAir.gaps).toEqual([{ from: at(100), to: at(202), seconds: 102, why: 'shutdown' }]);
  });

  it('reads days split at midnight the same as one joined log', () => {
    // Arrange: one run across two daily files
    const day1 = jsonl(ev(0, 'start'), ev(2, 'encoder-start'), ev(7000, 'retry', 'warn'), ev(7100, 'encoder-exit', 'warn'));
    const day2 = jsonl(ev(7110, 'encoder-start'), ev(9000, 'skip', 'warn'), ev(9500, 'youtube-breaker-open', 'critical'));
    const csv1 = [cpuRow(10, 1.0, 5, 3, 20), progressRow(11)].join('\n');
    const csv2 = [cpuRow(9000, 0.98, 6, 4, 25)].join('\n');
    const window = [T0, T0 + 10_000 * 1000];

    // Act
    const split = trialReport({ events: [day1, day2], encoderCsv: [csv1, csv2] }, ...window);
    const joined = trialReport({ events: [day1 + day2], encoderCsv: [csv1 + '\n' + csv2] }, ...window);

    // Assert
    expect(split).toEqual(joined);
    expect(split.offAir.seconds).toBe(10);
    expect(split.retries).toBe(1);
    expect(split.skips).toBe(1);
    expect(split.breaker.opens).toBe(1);
    expect(split.criticals).toEqual([{ ts: at(9500), kind: 'youtube-breaker-open' }]);
  });

  it('counts a relaunched supervisor as a start without an encoder restart or a broken gap', () => {
    // Arrange: the supervisor dies at 500 and is relaunched; its encoder generations restart at 0
    const events = [jsonl(
      ev(0, 'start'), ev(2, 'encoder-start', 'info', { generation: 0 }),
      ev(500, 'shutdown'),
      ev(504, 'start'), ev(506, 'encoder-start', 'info', { generation: 0 }),
      ev(900, 'encoder-exit', 'warn'), ev(903, 'encoder-start', 'info', { generation: 1 }),
    )];

    // Act
    const r = trialReport({ events, encoderCsv: [] }, T0, T0 + 1000 * 1000);

    // Assert
    expect(r.restarts).toEqual({ supervisorStarts: 2, encoder: 1 });
    // the cold start at the window's beginning is not an outage
    expect(r.offAir.gaps.map((g) => g.seconds)).toEqual([6, 3]);
    expect(r.offAir.gaps.every((g) => g.seconds >= 0)).toBe(true);
  });

  it('counts only events inside the window', () => {
    // Arrange
    const events = [jsonl(ev(0, 'retry', 'warn'), ev(100, 'retry', 'warn'), ev(300, 'retry', 'warn'))];

    // Act
    const r = trialReport({ events, encoderCsv: [] }, T0 + 50 * 1000, T0 + 200 * 1000);

    // Assert
    expect(r.retries).toBe(1);
    expect(r.counts).toEqual({ retry: 1 });
  });

  it('splits watchdog verdicts by what was done', () => {
    // Arrange
    const events = [jsonl(
      ev(10, 'watchdog-freeze', 'warn', { outcome: 'retry' }),
      ev(20, 'watchdog-freeze', 'warn', { outcome: 'skip' }),
      ev(30, 'watchdog-silence', 'info', { outcome: 'content' }),
      ev(40, 'watchdog-freeze', 'info', { outcome: 'ignored' }),
    )];

    // Act
    const r = trialReport({ events, encoderCsv: [] }, T0, T0 + 100 * 1000);

    // Assert
    expect(r.watchdog).toEqual({ freeze: 2, silence: 0, content: 1 });
  });

  it('takes encoder speed and CPU from the 10 s rows only', () => {
    // Arrange
    const csv = [progressRow(1), cpuRow(10, 1.0, 4, 2, 10), progressRow(11), cpuRow(20, 0.9, 8, 6, 30), cpuRow(30, 1.1, 6, 4, 20)].join('\n');

    // Act
    const r = trialReport({ events: [], encoderCsv: [csv] }, T0, T0 + 100 * 1000);

    // Assert
    expect(r.encoder.samples).toBe(3);
    expect(r.encoder.speed).toEqual({ median: 1, min: 0.9, max: 1.1 });
    expect(r.encoder.encoderCpu).toEqual({ median: 6, min: 4, max: 8 });
    expect(r.encoder.systemCpu).toEqual({ median: 20, min: 10, max: 30 });
  });
});
