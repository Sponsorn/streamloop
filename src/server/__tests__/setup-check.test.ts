import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mkdirSync, readFileSync, rmSync } from 'fs';
import { join } from 'path';
import {
  ENCODER_ARGS,
  PROBE_TAIL,
  checkDependencies,
  pickEncoder,
  probeEncoder,
  recommendCacheMb,
  runSetupCheck,
  type Encoder,
  type RunFn,
} from '../setup-check.js';

const FFMPEG = 'X:\\root\\ffmpeg\\ffmpeg.exe';

/** A run stub where only the listed encoders pass their test encode. */
function runWhere(working: Encoder[]): RunFn & ReturnType<typeof vi.fn> {
  return vi.fn(async (_file: string, args: string[]) => working.includes(args[args.indexOf('-c:v') + 1] as Encoder));
}

describe('probeEncoder', () => {
  it('runs a 2 s 1080p30 test encode with the supervisor arguments and a 20 s timeout', async () => {
    // Arrange
    const run = runWhere(['h264_nvenc']);

    // Act
    const ok = await probeEncoder(FFMPEG, 'h264_nvenc', run);

    // Assert
    expect(ok).toBe(true);
    const [file, args, timeoutMs] = run.mock.calls[0] as [string, string[], number];
    expect(file).toBe(FFMPEG);
    expect(args.join(' ')).toContain('-f lavfi -i testsrc2=size=1920x1080:rate=30 -t 2 -c:v h264_nvenc -preset p4 -rc cbr');
    expect(args.join(' ')).toContain('-profile:v high -b:v 4500k -maxrate 4500k -bufsize 9000k -g 60 -pix_fmt yuv420p -f null -');
    expect(timeoutMs).toBe(20_000);
  });
});

describe('pickEncoder', () => {
  it('falls back from a failing nvenc to amf', async () => {
    const r = await pickEncoder(FFMPEG, null, runWhere(['h264_amf', 'libx264']));
    expect(r).toMatchObject({ encoder: 'h264_amf', tried: ['h264_nvenc', 'h264_amf'], software: false, works: true });
  });

  it('ends on libx264 when no hardware encoder works', async () => {
    const r = await pickEncoder(FFMPEG, null, runWhere(['libx264']));
    expect(r).toMatchObject({ encoder: 'libx264', tried: ['h264_nvenc', 'h264_amf', 'h264_qsv', 'libx264'], software: true, works: true });
  });

  it('tries the preferred encoder first', async () => {
    const run = runWhere(['h264_qsv', 'h264_nvenc']);
    const r = await pickEncoder(FFMPEG, 'h264_qsv', run);
    expect(r).toMatchObject({ encoder: 'h264_qsv', tried: ['h264_qsv'], software: false });
    expect(run).toHaveBeenCalledTimes(1);
  });

  it('retries the preferred encoder once after a pause and keeps it when the second try passes', async () => {
    // Arrange
    vi.useFakeTimers();
    let calls = 0;
    const run: RunFn = vi.fn(async () => ++calls > 1);

    // Act
    const picking = pickEncoder(FFMPEG, 'h264_nvenc', run, 2_000);
    await vi.advanceTimersByTimeAsync(1_999);
    const beforePause = calls;
    await vi.advanceTimersByTimeAsync(1);
    const r = await picking;
    vi.useRealTimers();

    // Assert
    expect(beforePause).toBe(1);
    expect(r).toMatchObject({ encoder: 'h264_nvenc', tried: ['h264_nvenc'], works: true });
  });

  it('probes at most four times and reports when nothing works', async () => {
    const run = runWhere([]);
    const r = await pickEncoder(FFMPEG, 'h264_amf', run);
    expect(run).toHaveBeenCalledTimes(4);
    expect(r.works).toBe(false);
    expect(r.tried).toEqual(['h264_amf', 'h264_nvenc', 'h264_qsv', 'libx264']);
  });
});

describe('recommendCacheMb', () => {
  it.each([
    [100 * 1024, 4096],
    [10 * 1024, 1024],
    [3 * 1024, 500],
  ])('recommends %i MB free → %i MB', (free, mb) => {
    expect(recommendCacheMb(free)).toEqual({ mb, warning: null });
  });

  it('sets the minimum with a warning below 2 GB free', () => {
    expect(recommendCacheMb(1536)).toEqual({ mb: 50, warning: 'Less than 2 GB free on this drive. The download cache is set to its minimum.' });
  });

  it('makes room for the two longest videos, so the next one downloads while the current one plays', () => {
    // Arrange: the box's 8.5 h and 7.5 h videos, at 0.5 MB a second
    const longest = [30_474, 26_953, 600];

    // Act
    const r = recommendCacheMb(400 * 1024, longest);

    // Assert: (30474 + 26953) * 0.5 = 28714 MB, well past the old 4096 MB ceiling
    expect(r).toEqual({ mb: 28_714, warning: null });
  });

  it('keeps at least 500 MB when the playlist has only short videos', () => {
    expect(recommendCacheMb(400 * 1024, [120, 90])).toEqual({ mb: 500, warning: null });
  });

  it('caps at half the free space and says long videos will play from YouTube while they download', () => {
    // Arrange: 40 GB free, so at most 20 GB can go to the cache
    const r = recommendCacheMb(40 * 1024, [30_474, 26_953]);

    // Assert
    expect(r.mb).toBe(20 * 1024);
    expect(r.warning).toBe('The two longest videos need about 28 GB, but only 20 GB can be spared on this drive. Long videos play from YouTube while they download.');
  });
});

describe('checkDependencies', () => {
  it('lists exactly the one missing file as not ok', () => {
    // Arrange
    const missing = join('R:', 'pot', 'build', 'main.js');

    // Act
    const deps = checkDependencies('R:', (p) => p !== missing);

    // Assert
    expect(deps.filter((d) => !d.ok)).toEqual([{ name: 'Token server', ok: false, path: missing }]);
    expect(deps.map((d) => d.path)).toContain('C:\\Windows\\Fonts\\malgun.ttf');
    expect(deps).toHaveLength(9);
  });
});

describe('ENCODER_ARGS', () => {
  it('probes with the common tail of encoder_args in supervisor/src/media.rs', () => {
    // Arrange
    const rs = readFileSync(join(process.cwd(), 'supervisor', 'src', 'media.rs'), 'utf-8');
    const start = rs.indexOf('a.extend([', rs.indexOf('pub fn encoder_args'));
    const tail = rs.slice(start, rs.indexOf('"-vf"', start));

    // Act
    const parsed = [...tail.matchAll(/"([^"]*)"/g)].map((m) => m[1]);

    // Assert
    expect(parsed).toEqual(PROBE_TAIL);
  });

  it('matches video_set in supervisor/src/media.rs', () => {
    // Arrange
    const rs = readFileSync(join(process.cwd(), 'supervisor', 'src', 'media.rs'), 'utf-8');
    const body = rs.slice(rs.indexOf('fn video_set'), rs.indexOf('_ => return None'));

    // Act
    const parsed = Object.fromEntries(
      [...body.matchAll(/"(\w+)" => &\[([^\]]*)\]/g)].map((m) => [m[1], [...m[2].matchAll(/"([^"]*)"/g)].map((a) => a[1])]),
    );

    // Assert
    expect(parsed).toEqual(ENCODER_ARGS);
  });
});

describe('runSetupCheck', () => {
  const root = join(process.cwd(), '.test-tmp', 'setup-check');

  beforeEach(() => {
    rmSync(root, { recursive: true, force: true });
    mkdirSync(join(root, 'data'), { recursive: true });
  });
  afterEach(() => rmSync(root, { recursive: true, force: true }));

  it('writes the report to data\\setup-check.json, sizing the cache from the data drive', async () => {
    // Arrange
    const statfs = vi.fn(async () => ({ bavail: 10 * 1024 * 256, bsize: 4096 })); // 10 GB
    const run = runWhere(['h264_nvenc']);

    // Act
    const report = await runSetupCheck(root, { run, exists: () => true, statfs });

    // Assert
    expect(statfs).toHaveBeenCalledWith(join(root, 'data'));
    expect(report.encoder).toEqual({ chosen: 'h264_nvenc', tried: ['h264_nvenc'], software: false, works: true });
    expect(report.cache).toEqual({ freeMb: 10240, recommendedMb: 1024, warning: null });
    expect(report.dependencies.every((d) => d.ok)).toBe(true);
    expect(JSON.parse(readFileSync(join(root, 'data', 'setup-check.json'), 'utf-8'))).toEqual(report);
  });

  it('reports works: false only when every encoder failed', async () => {
    const statfs = async () => ({ bavail: 1, bsize: 1 });
    const report = await runSetupCheck(root, { run: runWhere([]), exists: () => true, statfs });
    expect(report.encoder).toEqual({ chosen: 'libx264', tried: ['h264_nvenc', 'h264_amf', 'h264_qsv', 'libx264'], software: true, works: false });
  });
});
