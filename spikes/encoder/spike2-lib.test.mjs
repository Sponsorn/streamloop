import { describe, expect, it } from 'vitest';
import {
  chooseEncoder, judge, parseSample, pipelineChecks, stats, streamCheck, vendorMatches, wsGrowthMb,
} from './spike2-lib.mjs';

describe('counter samples', () => {
  const line = JSON.stringify({ s: [
    { k: 'tot', i: '_Total', v: 25 },
    { k: 'pid', i: 'ffmpeg', v: 100 },
    { k: 'pid', i: 'ffmpeg#1', v: 200 },
    { k: 'pid', i: 'ffmpeg#2', v: 300 },
    { k: 'cpu', i: 'ffmpeg', v: 160 },
    { k: 'cpu', i: 'ffmpeg#1', v: 32 },
    { k: 'cpu', i: 'ffmpeg#2', v: 16 },
    { k: 'ws', i: 'ffmpeg', v: 300 * 2 ** 20 },
    { k: 'ws', i: 'ffmpeg#1', v: 999 * 2 ** 20 },
    { k: 'gpu', i: 'pid_200_luid_0x0_0x1_phys_0_eng_6_engtype_VideoEncode', v: 3 },
    { k: 'gpu', i: 'pid_200_luid_0x0_0x1_phys_0_eng_7_engtype_VideoEncode', v: 4 },
    { k: 'gpu', i: 'pid_200_luid_0x0_0x1_phys_0_eng_0_engtype_3D', v: 9 },
    { k: 'gpu', i: 'pid_77_luid_0x0_0x2_phys_0_eng_10_engtype_Video Codec 0', v: 40 },
  ] });

  it('splits ffmpeg CPU into encoder and feeders by pid, as a share of the whole machine', () => {
    // Arrange: pid 200 is the encoder, 300 the recorder, 100 a feeder; 16 cores.

    // Act
    const row = parseSample(line, { encoderPid: 200, recorderPid: 300, cores: 16 });

    // Assert
    expect(row.cpuTotal).toBe(25);
    expect(row.encoderCpu).toBe(2);
    expect(row.feederCpu).toBe(10);
    expect(row.encoderWsMb).toBe(999);
  });

  it('sums every encode engine instance, AMD\'s "Video Codec" included, and nothing else', () => {
    const row = parseSample(line, { encoderPid: 200, recorderPid: 300, cores: 16 });

    expect(row.encodeEngine).toBe(47);
    expect(row.encoderEngines).toEqual({ VideoEncode: 4, '3D': 9 });
  });

  it('counts every ffmpeg as a feeder before run.mjs has reported the encoder pid', () => {
    const row = parseSample(line, { encoderPid: undefined, recorderPid: undefined, cores: 16 });

    expect(row.encoderCpu).toBe(0);
    expect(row.feederCpu).toBe(13);
    expect(row.encoderWsMb).toBeNull();
  });
});

describe('statistics', () => {
  it('reports average and nearest-rank 95th percentile, ignoring missing values', () => {
    const s = stats([...Array.from({ length: 20 }, (_, i) => i + 1), NaN]);

    expect(s.avg).toBe(10.5);
    expect(s.p95).toBe(19);
    expect(s.n).toBe(20);
  });

  it('measures working-set growth from the second minute to the last', () => {
    // Arrange: 300 MB during start-up, 310 in the second minute, 340 at the end of 10 minutes.
    const rows = Array.from({ length: 121 }, (_, i) => ({ t: i * 5000, encoderWsMb: i < 12 ? 300 : i > 108 ? 340 : 310 }));

    expect(wsGrowthMb(rows)).toBe(30);
  });
});

describe('pipeline checks', () => {
  const clean = { encoderExitedEarly: false, readerDisconnects: 0, backwards: 0, maxSeamRiseMs: 120, overallSpeed: 1.0001 };
  const light = { cpu: { avg: 20, p95: 30 }, engine: { avg: 10 }, wsGrowthMb: 5 };

  it('passes a clean stream under every bar', () => {
    const c = pipelineChecks(clean, light);

    expect(c.hardPass).toBe(true);
    expect(c.barsPass).toBe(true);
  });

  it('keeps the stream checks apart from a missed CPU bar', () => {
    const c = pipelineChecks(clean, { ...light, cpu: { avg: 45, p95: 70 } });

    expect(c.hardPass).toBe(true);
    expect(c.barsPass).toBe(false);
    expect(c.bars.cpuAvgUnder40).toBe(false);
  });

  it('fails on a seam lag rise of 500 ms or speed outside 0.99 to 1.01', () => {
    expect(pipelineChecks({ ...clean, maxSeamRiseMs: 500 }, light).hard.seamLag).toBe(false);
    expect(pipelineChecks({ ...clean, overallSpeed: 0.98 }, light).hard.speed).toBe(false);
  });
});

describe('stream check', () => {
  const stream = { profile: 'High', width: 1920, height: 1080, avg_frame_rate: '30/1', pix_fmt: 'yuv420p' };
  // 10 s at 30 fps, a keyframe every 60 frames, 18750 bytes a frame = 4500 kbit/s.
  const packets = Array.from({ length: 301 }, (_, i) => ({ pts_time: (i / 30).toFixed(6), size: '18750', flags: i % 60 === 0 ? 'K__' : '___' }));

  it('passes a fixed 2 s GOP at 4500 kbit/s', () => {
    const r = streamCheck(stream, packets);

    expect(r.keyframeIntervals).toEqual([2, 2, 2, 2, 2]);
    expect(r.avgKbps).toBeCloseTo(4500, 0);
    expect(r.pass).toBe(true);
  });

  it('fails on one scene-cut keyframe and on Main profile', () => {
    const cut = packets.map((p, i) => (i === 75 ? { ...p, flags: 'K__' } : p));

    const r = streamCheck({ ...stream, profile: 'Main' }, cut);

    expect(r.checks.gop).toBe(false);
    expect(r.checks.high).toBe(false);
  });
});

describe('verdict', () => {
  const detection = [
    { codec: 'h264_nvenc', ok: true, error: '' },
    { codec: 'h264_amf', ok: true, error: '' },
    { codec: 'h264_qsv', ok: false, error: 'Error creating a MFX session: -9.' },
  ];
  const gpus = ['NVIDIA GeForce RTX 4080', 'AMD Radeon(TM) Graphics'];
  const pipeline = {
    m: { minutes: 30, maxSeamRiseMs: 100, overallSpeed: 1, backwards: 0, readerDisconnects: 0, encoderExitedEarly: false },
    usage: { cpu: { avg: 20, p95: 30 }, engine: { avg: 10 }, wsGrowthMb: 5 },
    checks: { hard: {}, hardPass: true, barsPass: true },
  };
  const stream = { pass: true, checks: {}, profile: 'High', size: '1920x1080', fps: 30, pixFmt: 'yuv420p', keyframeIntervals: [2], avgKbps: 4480 };
  const headroom = [{ codec: 'h264_nvenc', fps: 400 }, { codec: 'h264_amf', fps: 200 }];

  it('chooses the first working encoder unless one is forced', () => {
    expect(chooseEncoder(detection)).toBe('h264_nvenc');
    expect(chooseEncoder(detection, 'h264_amf')).toBe('h264_amf');
    expect(chooseEncoder(detection.map((d) => ({ ...d, ok: false })))).toBeNull();
  });

  it('matches an encoder to a GPU vendor in the inventory', () => {
    expect(vendorMatches('h264_amf', gpus)).toBe(true);
    expect(vendorMatches('h264_qsv', gpus)).toBe(false);
  });

  it('passes when 1 to 4 pass and Twitch did not run', () => {
    const v = judge({ gpus, detection, chosen: 'h264_nvenc', headroom, pipeline, stream, twitch: null });

    expect(v.criteria).toMatchObject({ 1: 'PASS', 2: 'PASS', 3: 'PASS', 4: 'PASS', 5: 'NOT RUN' });
    expect(v.text).toMatch(/Verdict: PASS\n/);
    expect(v.text).toContain('MFX session');
  });

  it('ends the spike when no hardware encoder works', () => {
    const v = judge({ gpus, detection: detection.map((d) => ({ ...d, ok: false })), chosen: null, headroom: [], pipeline: null, stream: null });

    expect(v.criteria[1]).toBe('FAIL');
    expect(v.text).toContain('hardware requirement fails');
  });

  it('fails headroom under 90 fps', () => {
    const v = judge({ gpus, detection, chosen: 'h264_nvenc', headroom: [{ codec: 'h264_nvenc', fps: 85 }], pipeline, stream });

    expect(v.criteria[2]).toBe('FAIL');
  });

  it('calls a stream-format miss with another working vendor a fix round', () => {
    const v = judge({ gpus, detection, chosen: 'h264_nvenc', headroom, pipeline, stream: { ...stream, pass: false, checks: { gop: false } } });

    expect(v.criteria[4]).toBe('FAIL');
    expect(v.text).toContain('h264_amf also works: a fix round');
  });

  it('reports missed resource bars on a clean stream as numbers, not a fail', () => {
    const v = judge({ gpus, detection, chosen: 'h264_nvenc', headroom, pipeline: { ...pipeline, checks: { hard: {}, hardPass: true, barsPass: false } }, stream });

    expect(v.criteria[3]).toBe('BARS MISSED');
    expect(v.text).toContain('PASS with resource bars missed');
  });

  it('leaves criterion 5 to Twitch Inspector when the Twitch leg ran', () => {
    const v = judge({ gpus, detection, chosen: 'h264_amf', headroom, pipeline, stream, twitch: 'RAN: check Twitch Inspector' });

    expect(v.text).toContain('criterion 5 needs Twitch Inspector');
  });
});
