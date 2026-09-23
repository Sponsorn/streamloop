import { describe, it, expect } from 'vitest';
import { calibrate, controlLatency, resumeErrorSec, deletionOk, splitCsv, median } from './lib.mjs';

describe('calibrate', () => {
  it('derives the timeline-offset -> reader-dts constant from the first seam', () => {
    // Arrange: reader dts is wall-clock-like here (dts = wallMs/1000 + 100), timeline offset 10
    const readerDtsAt = (wallMs) => wallMs / 1000 + 100;
    const seamA = { wallMs: 0, offset: 10 };
    const seamB = { wallMs: 60_000, offset: 70 };

    // Act
    const result = calibrate(seamA, seamB, readerDtsAt);

    // Assert
    expect(result.constant).toBeCloseTo(90, 6);
    expect(result.errorSec).toBeCloseTo(0, 6);
  });

  it('reports a nonzero error when the second seam drifts from the constant', () => {
    // Arrange: constant from seamA is 90 (100-10); seamB predicts 70+90=160, but drifts to 172
    const readerDtsAt = (wallMs) => (wallMs === 0 ? 100 : 172);
    const seamA = { wallMs: 0, offset: 10 };
    const seamB = { wallMs: 60_000, offset: 70 };

    // Act
    const result = calibrate(seamA, seamB, readerDtsAt);

    // Assert
    expect(result.errorSec).toBeCloseTo(12, 6);
  });

  it('returns null when the reader has no packet at or after the first seam', () => {
    // Arrange
    const readerDtsAt = () => null;

    // Act + Assert
    expect(calibrate({ wallMs: 0, offset: 10 }, { wallMs: 60_000, offset: 70 }, readerDtsAt)).toBeNull();
  });
});

describe('controlLatency', () => {
  it('measures wall time from the command to the first reader packet at or after the cut dts', () => {
    // Arrange
    const readerRows = [{ wallMs: 1000, dts: 5 }, { wallMs: 1500, dts: 9.9 }, { wallMs: 2000, dts: 10.1 }];

    // Act
    const latency = controlLatency(1000, 10, readerRows);

    // Assert: the 9.9 packet arrives before the cut dts, so the first qualifying packet is at 2000
    expect(latency).toBe(1000);
  });

  it('returns null when no reader packet ever reaches the cut dts', () => {
    // Arrange
    const readerRows = [{ wallMs: 1000, dts: 1 }, { wallMs: 2000, dts: 2 }];

    // Act + Assert
    expect(controlLatency(1000, 50, readerRows)).toBeNull();
  });
});

describe('resumeErrorSec', () => {
  it('is zero when the resumed position exactly matches P_view', () => {
    expect(resumeErrorSec(42.5, 42.5)).toBe(0);
  });

  it('is the absolute gap regardless of which side is larger', () => {
    expect(resumeErrorSec(40, 42)).toBeCloseTo(2, 6);
    expect(resumeErrorSec(42, 40)).toBeCloseTo(2, 6);
  });
});

describe('deletionOk', () => {
  it('passes when P_view has reached the end of the deleted file', () => {
    expect(deletionOk({ pView: '120', fileEndTimeline: '118' })).toBe(true);
  });

  it('fails when a file is deleted before P_view caught up to its end', () => {
    expect(deletionOk({ pView: '100', fileEndTimeline: '118' })).toBe(false);
  });

  it('passes a switch deletion, which carries no fileEndTimeline', () => {
    expect(deletionOk({ pView: '50', fileEndTimeline: '' })).toBe(true);
  });
});

describe('splitCsv', () => {
  it('keeps commas inside the last column intact', () => {
    // Arrange
    const line = '1000,next,{"a":1,"b":2}';

    // Act
    const row = splitCsv(line, ['wallMs', 'cmd', 'payload']);

    // Assert
    expect(row.payload).toBe('{"a":1,"b":2}');
  });
});

describe('median', () => {
  it('averages the two middle values of an even-length list', () => {
    expect(median([1, 2, 3, 4])).toBe(2.5);
  });
});
