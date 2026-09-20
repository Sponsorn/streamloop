import { describe, it, expect } from 'vitest';
import {
  FPS, BASE_OFFSET, CLIPS, SHORT_CLIPS, PTS_WRAP_SECONDS, feederArgs, encoderArgs, offsetAfter,
  parseProgress, createAnalyzer, pairOffsets, lagStats, verdict,
} from './lib.mjs';

describe('feeder arguments', () => {
  it('carries the timeline offset with microsecond precision and writes MPEG-TS to stdout', () => {
    // Arrange + Act
    const args = feederArgs('media/a.mp4', 1234.5);

    // Assert
    expect(args[args.indexOf('-output_ts_offset') + 1]).toBe('1234.500000');
    expect(args.slice(-3)).toEqual(['-f', 'mpegts', 'pipe:1']);
    expect(args).toContain('+initial_discontinuity');
  });
});

describe('encoder arguments', () => {
  it('reads stdin in real time and pushes FLV to a single target', () => {
    // Arrange + Act
    const args = encoderArgs('rtmp://127.0.0.1:1935/live/spike');

    // Assert
    expect(args.indexOf('-re')).toBeLessThan(args.indexOf('-i'));
    expect(args.slice(-3)).toEqual(['-f', 'flv', 'rtmp://127.0.0.1:1935/live/spike']);
    expect(args).toContain('libx264');
  });

  it('uses the tee muxer with explicit stream maps when two targets are given', () => {
    // Arrange + Act
    const args = encoderArgs('[f=flv]rtmp://a|[f=flv:onfail=ignore]rtmp://b', { tee: true });

    // Assert
    expect(args.slice(-2)).toEqual(['tee', '[f=flv]rtmp://a|[f=flv:onfail=ignore]rtmp://b']);
    expect(args).toContain('+global_header');
    expect(args.filter((a) => a === '-map')).toHaveLength(2);
  });

  it('corrects audio drift at every seam instead of waiting for 100 ms to build up', () => {
    // Each seam adds up to one 24 ms audio frame of overlap. With ffmpeg's default
    // min_hard_comp of 0.1 the A/V offset saw-tooths from 10 to 90 ms across seams.
    const args = encoderArgs('x');

    expect(args[args.indexOf('-af') + 1]).toBe('aresample=async=1:min_hard_comp=0.01');
  });

  it('switches the video encoder to NVENC on request', () => {
    expect(encoderArgs('x', { codec: 'h264_nvenc' })).toContain('h264_nvenc');
  });
});

describe('short clip set', () => {
  it('keeps each clip\'s format and only shortens it, 13 seconds per cycle', () => {
    // Arrange + Act
    const total = SHORT_CLIPS.reduce((sum, clip) => sum + clip.seconds, 0);

    // Assert
    expect(total).toBe(13);
    SHORT_CLIPS.forEach((short, i) => {
      expect(short.name).toBe(`${CLIPS[i].name}-short`);
      expect([short.size, short.rate, short.sampleRate, short.channels])
        .toEqual([CLIPS[i].size, CLIPS[i].rate, CLIPS[i].sampleRate, CLIPS[i].channels]);
    });
  });
});

describe('timeline offset', () => {
  it('advances by exactly the number of output frames', () => {
    expect(offsetAfter(BASE_OFFSET, 150)).toBe(BASE_OFFSET + 5);
    expect(offsetAfter(0, 70 * FPS)).toBe(70);
  });

  it('knows where the 33-bit MPEG-TS timestamp wraps', () => {
    expect(PTS_WRAP_SECONDS).toBeCloseTo(95443.7, 1);
  });
});

describe('progress parsing', () => {
  it('returns the last value of each key and ignores lines that are not key=value', () => {
    // Arrange
    const text = 'frame=10\nspeed=1.01x\n[mpegts] some warning\nframe=150\nprogress=end\n';

    // Act + Assert
    expect(parseProgress(text)).toEqual({ frame: '150', speed: '1.01x', progress: 'end' });
  });
});

describe('timestamp analyzer', () => {
  it('stays quiet for normal frame steps on two interleaved streams', () => {
    // Arrange
    const a = createAnalyzer();

    // Act
    for (let i = 0; i < 90; i++) {
      a.push({ stream: '0', dts: 10 + i / 30 });
      a.push({ stream: '1', dts: 10 + i * 0.0213 });
    }

    // Assert
    expect(a.events).toEqual([]);
  });

  it('flags a timestamp that goes backwards and a jump over 100 ms, per stream', () => {
    // Arrange
    const a = createAnalyzer();

    // Act
    a.push({ stream: '0', dts: 10 });
    a.push({ stream: '1', dts: 50 });
    a.push({ stream: '0', dts: 9.5 });
    a.push({ stream: '0', dts: 9.9 });

    // Assert
    expect(a.events.map((e) => [e.type, e.stream])).toEqual([['backwards', '0'], ['jump', '0']]);
  });
});

describe('flash and beep pairing', () => {
  it('reports how late the beeps are in milliseconds and drops beeps without a flash nearby', () => {
    // Arrange
    const flashes = [1, 2, 3, 4];
    const beeps = [1.04, 2.04, 3.04, 4.04, 9.7];

    // Act
    const result = pairOffsets(flashes, beeps);

    // Assert
    expect(result.pairs).toBe(4);
    expect(result.medianMs).toBeCloseTo(40, 5);
    expect(result.maxAbsMs).toBeCloseTo(40, 5);
  });
});

describe('delivery lag', () => {
  const stream = (seconds, starveAtSecond, starveMs) => {
    const rows = [];
    let wall = 1_000_000;
    for (let i = 0; i <= seconds * 10; i++) {
      if (i === starveAtSecond * 10) wall += starveMs;
      rows.push({ wallMs: wall, mediaUs: 10_000_000 + i * 100_000 });
      wall += 100;
    }
    return rows;
  };

  it('sees no rise and a speed of 1 for a steady stream', () => {
    // Arrange + Act
    const stats = lagStats(stream(300, -1, 0));

    // Assert
    expect(stats.maxRiseMs).toBeLessThan(1);
    expect(stats.overallSpeed).toBeCloseTo(1, 3);
  });

  it('sees a starved seam that the encoder later catches up from, ignoring the first 60 seconds', () => {
    // Arrange: after a starve the encoder catches up, so lag returns to normal.
    const recovered = stream(300, 200, 700).map((row, i) => (i > 2300 ? { ...row, wallMs: row.wallMs - 700 } : row));

    // Act + Assert
    expect(lagStats(stream(300, 30, 900)).maxRiseMs).toBeLessThan(1);
    expect(lagStats(recovered).maxRiseMs).toBeGreaterThan(600);
  });
});

describe('verdict', () => {
  const good = {
    backwards: 0, readerDisconnects: 0, encoderExitedEarly: false, maxRiseMs: 180,
    overallSpeed: 1.0004, avFirstMs: 12, avLastMs: 31, avFirstMaxMs: 24, avLastMaxMs: 40,
  };

  it('passes a clean run', () => {
    expect(verdict(good).pass).toBe(true);
  });

  it('fails on a backwards timestamp, a slow seam, growing A/V offset or an early encoder exit', () => {
    expect(verdict({ ...good, backwards: 1 }).pass).toBe(false);
    expect(verdict({ ...good, maxRiseMs: 640 }).seamLag).toBe(false);
    expect(verdict({ ...good, avLastMs: 90 }).avStable).toBe(false);
    expect(verdict({ ...good, encoderExitedEarly: true }).pass).toBe(false);
  });

  it('fails when the worst offset in a segment is over 100 ms even though the median is fine', () => {
    expect(verdict({ ...good, avLastMaxMs: 114 }).avInSync).toBe(false);
  });

  it('fails when the A/V offset could not be measured at all', () => {
    expect(verdict({ ...good, avFirstMs: NaN, avLastMs: NaN, avFirstMaxMs: NaN, avLastMaxMs: NaN }).pass).toBe(false);
  });
});
