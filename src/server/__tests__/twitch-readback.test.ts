import { describe, it, expect, vi } from 'vitest';
import { execFileSync } from 'child_process';
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'fs';
import { join } from 'path';
import {
  DEFAULT_THRESHOLD, TwitchReadback, grabGrayRegion, liveRendition, meanAbsDiff, timerRegion, type GrabRun,
} from '../twitch-readback.js';
import type { SupervisorClient, SupervisorStatus } from '../supervisor-client.js';

const LIVE: SupervisorStatus = {
  state: 'playing',
  encoder: { alive: true },
  recovery: { step: 'none' },
  hold: null,
  playing: { index: 2, source: 'file', duration: 600 },
};

const MASTER = [
  '#EXTM3U',
  '#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID="720p30",NAME="720p",AUTOSELECT=YES,DEFAULT=YES',
  '#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,CODECS="avc1.4D401F,mp4a.40.2",VIDEO="720p30",FRAME-RATE=30.000',
  'https://video.example/720p30/index.m3u8',
  '#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID="chunked",NAME="1080p30 (source)",AUTOSELECT=YES,DEFAULT=YES',
  '#EXT-X-STREAM-INF:BANDWIDTH=6000000,RESOLUTION=1920x1080,CODECS="avc1.64002A,mp4a.40.2",VIDEO="chunked",FRAME-RATE=30.000',
  'https://video.example/chunked/index.m3u8',
  '#EXT-X-STREAM-INF:BANDWIDTH=160000,CODECS="mp4a.40.2",VIDEO="audio_only"',
  'https://video.example/audio_only/index.m3u8',
].join('\n');

function twitchFetch(master: string | null): typeof fetch {
  return vi.fn(async (url: string | URL | Request) => {
    const u = String(url);
    if (u.startsWith('https://gql.twitch.tv/')) {
      return new Response(JSON.stringify({ data: { streamPlaybackAccessToken: { value: '{"t":1}', signature: 'sig' } } }));
    }
    if (u.startsWith('https://usher.ttvnw.net/') && master !== null) return new Response(master);
    return new Response('not found', { status: 404 });
  }) as unknown as typeof fetch;
}

function frame(fill: number, n = 32 * 18): Buffer {
  return Buffer.alloc(n, fill);
}

function setup(opts: {
  status?: SupervisorStatus | null;
  frames?: (Buffer | null)[];
  master?: string | null;
  live?: boolean | null;
  overlay?: boolean;
} = {}) {
  const status = vi.fn(async () => (opts.status === undefined ? LIVE : opts.status));
  const client = { status, control: vi.fn() } as unknown as SupervisorClient;
  const frames = [...(opts.frames ?? [])];
  const run: GrabRun = vi.fn(async () => {
    const f = frames.shift();
    if (!f) throw new Error('ffmpeg failed');
    return f;
  });
  const onFrozen = vi.fn();
  const onUnavailable = vi.fn();
  const rb = new TwitchReadback({
    channel: 'testchannel',
    ffmpegPath: 'ffmpeg.exe',
    client,
    onFrozen,
    onUnavailable,
    overlay: opts.overlay ?? true,
    isChannelLive: async () => (opts.live === undefined ? true : opts.live),
    gapMs: 0,
    fetchImpl: twitchFetch(opts.master === undefined ? MASTER : opts.master),
    run,
  });
  return { rb, run, onFrozen, onUnavailable, status };
}

const REGION_1080 = timerRegion(1920, 1080);
const still = (): Buffer => frame(40, REGION_1080.w * REGION_1080.h);
const moved = (): Buffer => frame(90, REGION_1080.w * REGION_1080.h);

describe('meanAbsDiff', () => {
  it('is the average absolute difference per pixel', () => {
    // Arrange
    const a = new Uint8Array([0, 10, 200, 255]);
    const b = new Uint8Array([4, 10, 190, 255]);

    // Act + Assert
    expect(meanAbsDiff(a, b)).toBe(3.5);
    expect(meanAbsDiff(a, a)).toBe(0);
  });

  it('treats frames of different sizes as different', () => {
    expect(meanAbsDiff(new Uint8Array(4), new Uint8Array(6))).toBe(Infinity);
  });
});

describe('timerRegion', () => {
  it('covers the overlay clock in a 1920x1080 rendition', () => {
    expect(timerRegion(1920, 1080)).toEqual({ x: 16, y: 1019, w: 104, h: 32 });
  });

  it('scales to a 1280x720 rendition', () => {
    expect(timerRegion(1280, 720)).toEqual({ x: 11, y: 679, w: 69, h: 21 });
  });
});

describe('liveRendition', () => {
  it('picks the source rendition from the master playlist', async () => {
    // Arrange
    const fetchImpl = twitchFetch(MASTER);

    // Act
    const r = await liveRendition('TestChannel', fetchImpl);

    // Assert
    expect(r).toEqual({ url: 'https://video.example/chunked/index.m3u8', width: 1920, height: 1080 });
    const usher = String((fetchImpl as unknown as ReturnType<typeof vi.fn>).mock.calls[1][0]);
    expect(usher).toContain('/api/channel/hls/testchannel.m3u8?');
  });

  it('falls back to the highest resolution when no rendition is marked chunked', async () => {
    const master = MASTER.replace(/chunked/g, '1080p30');
    expect((await liveRendition('c', twitchFetch(master)))?.url).toBe('https://video.example/1080p30/index.m3u8');
  });

  it('assumes 1920x1080 when the playlist gives no resolution', async () => {
    const master = '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,VIDEO="chunked"\nhttps://video.example/s.m3u8\n';
    expect(await liveRendition('c', twitchFetch(master))).toEqual({ url: 'https://video.example/s.m3u8', width: 1920, height: 1080 });
  });

  it('returns null when the channel is offline', async () => {
    expect(await liveRendition('c', twitchFetch(null))).toBeNull();
  });

  it('returns null when the request throws', async () => {
    const fetchImpl = vi.fn(async () => { throw new Error('network down'); }) as unknown as typeof fetch;
    expect(await liveRendition('c', fetchImpl)).toBeNull();
  });
});

describe('grabGrayRegion', () => {
  it('crops one gray frame in the temp folder with a timeout', async () => {
    // Arrange
    const run = vi.fn<GrabRun>(async () => Buffer.alloc(4 * 2, 7));

    // Act
    const out = await grabGrayRegion('ffmpeg.exe', 'https://x/s.m3u8', { x: 1, y: 2, w: 4, h: 2 }, run);

    // Assert
    expect(out?.length).toBe(8);
    const args = run.mock.calls[0][1];
    expect(args).toEqual(expect.arrayContaining(['-frames:v', '1', '-vf', 'crop=4:2:1:2,format=gray', '-f', 'rawvideo']));
  });

  it('scales the whole frame to 32x18 without a region', async () => {
    const run = vi.fn<GrabRun>(async () => Buffer.alloc(32 * 18));
    expect((await grabGrayRegion('f', 'u', null, run))?.length).toBe(576);
    expect(run.mock.calls[0][1]).toContain('scale=32:18,format=gray');
  });

  it('returns null when ffmpeg fails or returns a short frame', async () => {
    expect(await grabGrayRegion('f', 'u', null, async () => { throw new Error('exit 1'); })).toBeNull();
    expect(await grabGrayRegion('f', 'u', null, async () => Buffer.alloc(10))).toBeNull();
  });
});

describe('TwitchReadback', () => {
  it('two bad reads in a row with Twitch live call onFrozen once', async () => {
    // Arrange
    const { rb, onFrozen } = setup({ frames: [still(), still(), still(), still()] });

    // Act
    await rb.check();
    await rb.check();

    // Assert
    expect(onFrozen).toHaveBeenCalledOnce();
    expect(rb.status().state).toBe('frozen');
    expect(rb.status().lastDiff).toBe(0);
  });

  it('one bad read then a good one does not act', async () => {
    const { rb, onFrozen } = setup({ frames: [still(), still(), still(), moved(), still(), still()] });

    await rb.check();
    await rb.check();
    await rb.check();

    expect(onFrozen).not.toHaveBeenCalled();
  });

  it('does not act on two bad reads while Helix says the channel is offline', async () => {
    const { rb, onFrozen } = setup({ frames: [still(), still(), still(), still()], live: false });

    await rb.check();
    await rb.check();

    expect(onFrozen).not.toHaveBeenCalled();
  });

  it('reports ok after a good read', async () => {
    const { rb } = setup({ frames: [still(), moved()] });
    await rb.check();
    expect(rb.status()).toMatchObject({ state: 'ok', lastDiff: 50 });
    expect(rb.status().lastCheckAt).toEqual(expect.any(Number));
  });

  it.each([
    ['the slate', { ...LIVE, playing: { source: 'slate' } }],
    ['a hold', { ...LIVE, hold: { reason: 'all-failed' } }],
    ['needs-stream-key', { ...LIVE, state: 'needs-stream-key' }],
    ['waiting-for-playlist', { ...LIVE, state: 'waiting-for-playlist' }],
    ['a recovery', { ...LIVE, recovery: { step: 'retry' } }],
    ['no supervisor', null],
  ])('grabs nothing on %s', async (_name, status) => {
    const { rb, run } = setup({ status: status as SupervisorStatus | null, frames: [still(), still()] });
    await rb.check();
    expect(run).not.toHaveBeenCalled();
  });

  it('crops the clock when the overlay is on, scaled to the rendition', async () => {
    const master = MASTER.replace('VIDEO="chunked"', 'VIDEO="other"');
    const { rb, run } = setup({ master: master.replace('1920x1080', '640x360'), frames: [] });
    await rb.check();
    // the highest listed rendition is now 1280x720
    expect((run as ReturnType<typeof vi.fn>).mock.calls[0][1]).toContain('crop=69:21:11:679,format=gray');
  });

  it('compares the whole frame when the overlay is off', async () => {
    const { rb, run } = setup({ overlay: false, frames: [] });
    await rb.check();
    expect((run as ReturnType<typeof vi.fn>).mock.calls[0][1]).toContain('scale=32:18,format=gray');
  });

  it('compares the whole frame when the video has no duration, so no clock shows', async () => {
    const { rb, run } = setup({ status: { ...LIVE, playing: { source: 'url', duration: null } }, frames: [] });
    await rb.check();
    expect((run as ReturnType<typeof vi.fn>).mock.calls[0][1]).toContain('scale=32:18,format=gray');
  });

  it('a missing playlist calls onUnavailable once per outage and never onFrozen', async () => {
    // Arrange
    const { rb, onUnavailable, onFrozen, run } = setup({ master: null });

    // Act
    await rb.check();
    await rb.check();
    await rb.check();

    // Assert
    expect(onUnavailable).toHaveBeenCalledOnce();
    expect(onUnavailable).toHaveBeenCalledWith(expect.any(String));
    expect(onFrozen).not.toHaveBeenCalled();
    expect(run).not.toHaveBeenCalled();
    expect(rb.status().state).toBe('unavailable');
  });

  it('a failed frame grab calls onUnavailable once, and again after a recovery', async () => {
    const { rb, onUnavailable, onFrozen } = setup({ frames: [still(), null, still(), null, still(), moved(), still(), null] });

    await rb.check();
    await rb.check();
    await rb.check();
    await rb.check();

    expect(onUnavailable).toHaveBeenCalledTimes(2);
    expect(onFrozen).not.toHaveBeenCalled();
  });
});

// ffmpeg for the render test: FFMPEG, then ROOT\ffmpeg\ffmpeg.exe, then PATH.
function findFfmpeg(): string | null {
  const root = process.env.STREAMLOOP_ROOT;
  const candidates = [process.env.FFMPEG, root ? join(root, 'ffmpeg', 'ffmpeg.exe') : undefined].filter((c): c is string => !!c);
  for (const c of candidates) if (existsSync(c)) return c;
  try {
    execFileSync('ffmpeg', ['-version'], { stdio: 'ignore', timeout: 10_000 });
    return 'ffmpeg';
  } catch {
    return null;
  }
}

const FFMPEG = findFfmpeg();
const FONT = 'C:/Windows/Fonts/malgun.ttf';

describe('the overlay clock as the movement signal (renders with ffmpeg)', () => {
  // The supervisor's filter has no fixture outside Rust, so this mirrors overlay.rs's script() for a
  // video with a duration: title, NOT LIVE label, progress line with the clock, and the bar.
  const dir = join(process.cwd(), '.test-tmp', 'twitch-readback');
  const esc = (p: string): string => p.replace(/\\/g, '/').replace(/:/g, '\\:');
  const text = (file: string, size: number, y: number, expand: boolean): string =>
    `drawtext=fontfile='${esc(FONT)}':textfile='${esc(file)}'${expand ? '' : ':expansion=none'}:fontsize=${size}:fontcolor=white:x=24:y=${y}:box=1:boxcolor=black@0.55:boxborderw=10`;

  function render(startPos: number): string {
    const duration = 1200;
    writeFileSync(join(dir, 'title.txt'), 'A still video for the read-back test');
    writeFileSync(join(dir, 'label.txt'), 'NOT LIVE');
    const progress = join(dir, `progress-${startPos}.txt`);
    writeFileSync(progress, `%{pts:gmtime:${startPos}:%H\\:%M\\:%S} / 00:20:00  %{eif:min(100,100*(${startPos}+t)/${duration}):d}\\%`);
    const graph = [
      // a still picture: one testsrc2 frame, looped
      'testsrc2=s=1920x1080:r=30,trim=end_frame=1,loop=loop=-1:size=1,setpts=N/30/TB[norm]',
      `[norm]${text(join(dir, 'title.txt'), 28, 981, false)},` +
        `drawtext=fontfile='${esc(FONT)}':textfile='${esc(join(dir, 'label.txt'))}':expansion=none:fontsize=22:fontcolor=white:x=main_w-text_w-24:y=24:box=1:boxcolor=red@0.8:boxborderw=10,` +
        `${text(progress, 22, 1027, true)},drawbox=x=0:y=1074:w=1920:h=6:color=white@0.25:t=fill[base]`,
      'color=c=red:s=1920x6[bar]',
      `[base][bar]overlay=x='max(-1920,min(0,1920*((${startPos}+t)/${duration}-1)))':y=1074:shortest=1[outv]`,
    ].join(';');
    const out = join(dir, `frame-${startPos}.png`);
    execFileSync(FFMPEG!, ['-hide_banner', '-loglevel', 'error', '-y', '-filter_complex', graph, '-map', '[outv]', '-frames:v', '1', out], { timeout: 30_000 });
    return out;
  }

  it.skipIf(!FFMPEG || !existsSync(FONT))('a still picture with a moving clock is a good read, an identical frame is not', async () => {
    // Arrange
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(dir, { recursive: true });
    const at10 = render(10);
    const at15 = render(15);
    const region = timerRegion(1920, 1080);

    // Act
    const a = await grabGrayRegion(FFMPEG!, at10, region);
    const b = await grabGrayRegion(FFMPEG!, at15, region);
    const whole1 = await grabGrayRegion(FFMPEG!, at10, null);
    const whole2 = await grabGrayRegion(FFMPEG!, at10, null);

    // Assert
    const threshold = DEFAULT_THRESHOLD;
    const clockDiff = meanAbsDiff(a!, b!);
    const stillDiff = meanAbsDiff(whole1!, whole2!);
    console.log(`clock region diff 10 s -> 15 s: ${clockDiff.toFixed(3)}, identical whole frame: ${stillDiff.toFixed(3)}, threshold ${threshold}`);
    expect(clockDiff).toBeGreaterThan(2 * threshold);
    expect(stillDiff).toBeLessThan(threshold);
  }, 60_000);
});
