import { describe, it, expect } from 'vitest';
import {
  epochAt, effectiveSeams, viewAt, readerReach, riseAt, lagSpikes, slateEdge, resumeErrorSec, deletionOk,
  firstEventAfter, leaks, splitCsv, median,
} from './lib.mjs';

describe('epochAt', () => {
  it('counts encoder starts, so a relaunched supervisor whose generations restart at 0 is a new epoch', () => {
    // Arrange: process 1 has generations 0 and 1, process 2 starts over at generation 0
    const starts = [1000, 5000, 9000];

    // Act + Assert
    expect(epochAt(starts, 500)).toBe(-1);
    expect(epochAt(starts, 1000)).toBe(0);
    expect(epochAt(starts, 6000)).toBe(1);
    expect(epochAt(starts, 12_000)).toBe(2);
  });
});

describe('effectiveSeams', () => {
  it('drops seams a later flush cut away, like push_seam', () => {
    // Arrange: A plays, B was queued at 50, a control flushes back to 32 and starts C there
    const a = { name: 'A', offset: 10 };
    const b = { name: 'B', offset: 50 };
    const c = { name: 'C', offset: 32 };

    // Act
    const table = effectiveSeams([a, b, c]);

    // Assert
    expect(table.map((s) => s.name)).toEqual(['A', 'C']);
  });

  it('replaces a retry that started at the same offset without delivering anything', () => {
    const table = effectiveSeams([{ name: 'A', offset: 10 }, { name: 'A-403', offset: 40 }, { name: 'A-ok', offset: 40 }]);
    expect(table.map((s) => s.name)).toEqual(['A', 'A-ok']);
  });
});

describe('viewAt', () => {
  it('maps a timeline offset back to the video position the viewer saw', () => {
    // Arrange
    const table = [{ offset: 10, startPos: 0, source: 'slate' }, { offset: 20, startPos: 100, source: 'url' }];

    // Act
    const v = viewAt(table, 25.5);

    // Assert
    expect(v.pos).toBeCloseTo(105.5, 6);
  });

  it('holds the slate at its start position', () => {
    expect(viewAt([{ offset: 10, startPos: 42, source: 'slate' }], 15).pos).toBe(42);
  });

  it('returns null before the first seam', () => {
    expect(viewAt([{ offset: 10, startPos: 0, source: 'file' }], 5)).toBeNull();
  });
});

describe('readerReach', () => {
  it('finds the first reader packet whose dts reaches the mapped offset, not the first after a wall time', () => {
    // Arrange: the feeder started long before its first frame reached the reader
    const rows = [{ wallMs: 1000, dts: 5 }, { wallMs: 1500, dts: 9.9 }, { wallMs: 2000, dts: 10.1 }];

    // Act + Assert
    expect(readerReach(rows, 10).wallMs).toBe(2000);
    expect(readerReach(rows, 50)).toBeNull();
  });
});

describe('riseAt', () => {
  it('is the peak lag after t minus the median lag of the minute before', () => {
    // Arrange: flat 100 ms lag, then a 700 ms bump right at t = 70
    const points = Array.from({ length: 800 }, (_, i) => ({ t: i / 10, lag: i === 701 ? 700 : 100 }));

    // Act + Assert
    expect(riseAt(points, 70)).toBe(600);
  });

  it('returns null without a baseline', () => {
    expect(riseAt([{ t: 0, lag: 1 }], 0)).toBeNull();
  });
});

describe('lagSpikes', () => {
  it('reports off-seam rises and merges adjacent seconds, ignoring rises at a seam', () => {
    // Arrange: bumps at 120-121 s (off seam) and 200 s (seam at 199)
    const points = Array.from({ length: 3000 }, (_, i) => {
      const t = i / 10;
      const bump = (t >= 120 && t < 122) || (t >= 200 && t < 201);
      return { t, lag: bump ? 600 : 100 };
    });

    // Act
    const spikes = lagSpikes(points, [199]);

    // Assert
    expect(spikes).toHaveLength(1);
    expect(spikes[0]).toMatchObject({ t: 120, lastT: 121, riseMs: 500 });
  });
});

describe('slateEdge', () => {
  const slate = ['S1', 'S2'];
  const frames = (from, to, edge, slateFirst) => {
    const out = [];
    for (let t = from; t < to; t += 1 / 30) {
      const onSlate = slateFirst ? t < edge : t >= edge;
      out.push({ t, y: onSlate ? slate[out.length % 2] : `V${out.length}` });
    }
    return out;
  };

  it('finds the first video frame after the slate', () => {
    const f = frames(9, 11.5, 10.05, true);
    expect(slateEdge(f, 10, false)).toBeCloseTo(f.find((x) => x.t >= 10.05).t, 6);
  });

  it('finds the first slate frame after a cut to the slate', () => {
    const f = frames(9, 11.5, 9.98, false);
    expect(slateEdge(f, 10, true)).toBeCloseTo(f.find((x) => x.t >= 9.98).t, 6);
  });

  it('gives up when the side it learns from is not a still', () => {
    const f = Array.from({ length: 60 }, (_, i) => ({ t: 9 + i / 30, y: `V${i}` }));
    expect(slateEdge(f, 10, false)).toBeNull();
  });
});

describe('resumeErrorSec', () => {
  it('is the absolute gap regardless of which side is larger', () => {
    expect(resumeErrorSec(40, 42)).toBeCloseTo(2, 6);
    expect(resumeErrorSec(42, 40)).toBeCloseTo(2, 6);
  });
});

describe('deletionOk', () => {
  it('passes when P_view has reached the end of the deleted file', () => {
    expect(deletionOk({ id: 'a', pView: '120', fileEndTimeline: '118' })).toBe(true);
  });

  it('fails when a file is deleted before P_view caught up to its end', () => {
    expect(deletionOk({ id: 'a', pView: '100', fileEndTimeline: '118' })).toBe(false);
  });

  it('passes a file that never aired, unless it is the one on air (a blank end means either)', () => {
    expect(deletionOk({ id: 'a', pView: '50', fileEndTimeline: '' }, 'b')).toBe(true);
    expect(deletionOk({ id: 'a', pView: '50', fileEndTimeline: '' }, 'a')).toBe(false);
  });
});

describe('firstEventAfter', () => {
  it('matches events.jsonl kinds at or after the wall time', () => {
    const events = [{ kind: 'retry', wallMs: 5 }, { kind: 'fault', wallMs: 10 }, { kind: 'retry', wallMs: 12 }];
    expect(firstEventAfter(events, 10, ['retry', 'skip'])).toEqual({ kind: 'retry', wallMs: 12 });
  });
});

describe('leaks', () => {
  it('flags supervisor children and an owned token server, not the session recorder or the external token server', () => {
    // Arrange
    const ps = [
      { Name: 'ffmpeg.exe', CommandLine: 'ffmpeg -i rtmp://127.0.0.1:1935/live/spike -c copy -copyts rec.mkv' },
      { Name: 'ffmpeg.exe', CommandLine: 'ffmpeg -f mpegts -i pipe:0 -f flv rtmp://x' },
      { Name: 'yt-dlp.exe', CommandLine: 'yt-dlp -j x' },
      { Name: 'node.exe', CommandLine: 'node C:/bgutil/server/build/main.js' },
      { Name: 'node.exe', CommandLine: 'node C:/bgutil/server/build/main.js -H 127.0.0.1 -p 4416' },
    ];

    // Act
    const found = leaks(ps).map((p) => p.CommandLine);

    // Assert
    expect(found).toEqual([ps[1].CommandLine, ps[2].CommandLine, ps[4].CommandLine]);
  });
});

describe('splitCsv', () => {
  it('keeps commas inside the last column intact', () => {
    expect(splitCsv('1000,next,{"a":1,"b":2}', ['wallMs', 'cmd', 'payload']).payload).toBe('{"a":1,"b":2}');
  });
});

describe('median', () => {
  it('averages the two middle values of an even-length list', () => {
    expect(median([1, 2, 3, 4])).toBe(2.5);
  });
});
