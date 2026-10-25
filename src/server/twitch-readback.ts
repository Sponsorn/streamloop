import { execFile } from 'child_process';
import { SPAWN_CWD } from './spawn-cwd.js';
import { streamState } from './api.js';
import type { SupervisorClient } from './supervisor-client.js';
import { logger } from './logger.js';

/** Twitch's public web player client id: the playback token query takes no user credentials. */
const TWITCH_WEB_CLIENT_ID = 'kimne78kx3ncx6brgo4mv6wki5h1ko';
const GQL_URL = 'https://gql.twitch.tv/gql';
const TOKEN_QUERY =
  'query PlaybackAccessToken($login: String!) { streamPlaybackAccessToken(channelName: $login, ' +
  'params: {platform: "web", playerBackend: "mediaplayer", playerType: "site"}) { value signature } }';
const REQUEST_TIMEOUT_MS = 10_000;
const GRAB_TIMEOUT_MS = 20_000;

export const DEFAULT_INTERVAL_MS = 300_000;
export const DEFAULT_GAP_MS = 5_000;
/** Mean gray-level difference per pixel. One changed clock digit measures about 3.4 (render test). */
export const DEFAULT_THRESHOLD = 1.0;
const BAD_READS_TO_ACT = 2;

/** In source-frame pixels. */
export interface Region { x: number; y: number; w: number; h: number }
export interface Rendition { url: string; width: number; height: number }
export type ReadbackState = 'ok' | 'frozen' | 'unavailable' | 'off';
export interface ReadbackStatus { lastCheckAt: number | null; lastDiff: number | null; state: ReadbackState }
/** Runs ffmpeg and resolves with its stdout, or rejects. */
export type GrabRun = (exe: string, args: string[]) => Promise<Buffer>;

/** The overlay clock (`hh:mm:ss`) in a 1920x1080 frame, with a few pixels of slack: overlay.rs
 *  draws the progress line at x=24, y=1027, 22 px, and the clock is its first 8 characters. */
const CLOCK_1080: Region = { x: 16, y: 1019, w: 104, h: 32 };

export function timerRegion(width: number, height: number): Region {
  const sx = width / 1920;
  const sy = height / 1080;
  return {
    x: Math.round(CLOCK_1080.x * sx),
    y: Math.round(CLOCK_1080.y * sy),
    w: Math.round(CLOCK_1080.w * sx),
    h: Math.round(CLOCK_1080.h * sy),
  };
}

function pickRendition(master: string): Rendition | null {
  const lines = master.split(/\r?\n/);
  let best: (Rendition & { chunked: boolean }) | null = null;
  for (let i = 0; i < lines.length - 1; i++) {
    if (!lines[i].startsWith('#EXT-X-STREAM-INF:')) continue;
    const url = lines[i + 1].trim();
    if (!url || url.startsWith('#') || lines[i].includes('VIDEO="audio_only"')) continue;
    const res = /RESOLUTION=(\d+)x(\d+)/.exec(lines[i]);
    const [width, height] = res ? [Number(res[1]), Number(res[2])] : [1920, 1080];
    const chunked = lines[i].includes('VIDEO="chunked"');
    if (!best || (chunked && !best.chunked) || (chunked === best.chunked && width * height > best.width * best.height)) {
      best = { url, width, height, chunked };
    }
  }
  return best && { url: best.url, width: best.width, height: best.height };
}

/** The source rendition of the channel's live HLS, so the overlay text stays legible. Any failure is null. */
export async function liveRendition(channel: string, fetchImpl: typeof fetch = fetch): Promise<Rendition | null> {
  try {
    const login = channel.toLowerCase();
    const tokenRes = await fetchImpl(GQL_URL, {
      method: 'POST',
      headers: { 'Client-ID': TWITCH_WEB_CLIENT_ID, 'content-type': 'application/json' },
      body: JSON.stringify({ operationName: 'PlaybackAccessToken', query: TOKEN_QUERY, variables: { login } }),
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
    if (!tokenRes.ok) return null;
    const body = await tokenRes.json() as { data?: { streamPlaybackAccessToken?: { value?: string; signature?: string } | null } };
    const token = body.data?.streamPlaybackAccessToken;
    if (!token?.value || !token.signature) return null;
    const qs = new URLSearchParams({
      sig: token.signature, token: token.value, allow_source: 'true', allow_audio_only: 'false',
      p: String(Math.floor(Math.random() * 1e7)),
    });
    const masterRes = await fetchImpl(`https://usher.ttvnw.net/api/channel/hls/${encodeURIComponent(login)}.m3u8?${qs}`, {
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
    if (!masterRes.ok) return null;
    return pickRendition(await masterRes.text());
  } catch {
    return null;
  }
}

const runFfmpeg: GrabRun = (exe, args) =>
  new Promise((resolve, reject) => {
    execFile(exe, args, { cwd: SPAWN_CWD, encoding: 'buffer', timeout: GRAB_TIMEOUT_MS, windowsHide: true, maxBuffer: 16 * 1024 * 1024 },
      (err, stdout) => (err ? reject(err) : resolve(stdout)));
  });

/** One gray frame, cropped to `region`, or the whole frame at 32x18 without one. Any failure is null. */
export async function grabGrayRegion(ffmpegPath: string, hlsUrl: string, region: Region | null, run: GrabRun = runFfmpeg): Promise<Uint8Array | null> {
  const filter = region ? `crop=${region.w}:${region.h}:${region.x}:${region.y},format=gray` : 'scale=32:18,format=gray';
  const size = region ? region.w * region.h : 32 * 18;
  try {
    const out = await run(ffmpegPath, ['-hide_banner', '-loglevel', 'error', '-i', hlsUrl, '-frames:v', '1', '-vf', filter, '-f', 'rawvideo', '-']);
    return out.length === size ? new Uint8Array(out) : null;
  } catch {
    return null;
  }
}

/** A stitched ad (preroll or break) shows a still placeholder; streamlink's twitch plugin tests the same two marks. */
export function isAdPlaylist(media: string): boolean {
  if (/^#EXT-X-DATERANGE:.*CLASS="twitch-stitched-ad"/m.test(media)) return true;
  return [...media.matchAll(/^#EXTINF:[^,]*,(.*)$/gm)].some((m) => { const t = m[1].trim(); return t !== '' && t !== 'live'; });
}

async function fetchText(url: string, fetchImpl: typeof fetch): Promise<string | null> {
  try {
    const res = await fetchImpl(url, { signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS) });
    return res.ok ? await res.text() : null;
  } catch {
    return null;
  }
}

/** Frames of different sizes count as different. */
export function meanAbsDiff(a: Uint8Array, b: Uint8Array): number {
  if (a.length !== b.length || a.length === 0) return Infinity;
  let sum = 0;
  for (let i = 0; i < a.length; i++) sum += Math.abs(a[i] - b[i]);
  return sum / a.length;
}

export interface TwitchReadbackOptions {
  channel: string;
  ffmpegPath: string;
  client: SupervisorClient;
  onFrozen: () => void;
  onUnavailable: (why: string) => void;
  /** Two still whole-frame reads: reported once per episode, never acted on (a still upload looks the same). */
  onStill: () => void;
  /** Helix: only a channel Twitch calls live can be frozen. */
  isChannelLive: () => Promise<boolean | null>;
  /** `supervisor.overlay`: without it there is no clock, so the whole frame is compared. */
  overlay: boolean;
  intervalMs?: number;
  gapMs?: number;
  threshold?: number;
  fetchImpl?: typeof fetch;
  run?: GrabRun;
}

/** Pulls two frames of the channel's own HLS and checks the overlay clock moved between them.
 *  A clock that stands still means Twitch is receiving a stale stream. */
export class TwitchReadback {
  private readonly opts: TwitchReadbackOptions;
  private timer: ReturnType<typeof setInterval> | null = null;
  private busy = false;
  private badReads = 0;
  private inOutage = false;
  private stillNoted = false;
  private stopped = false;
  private current: ReadbackStatus = { lastCheckAt: null, lastDiff: null, state: 'ok' };

  constructor(opts: TwitchReadbackOptions) {
    this.opts = opts;
  }

  start(): void {
    this.stop();
    this.stopped = false;
    this.timer = setInterval(() => { void this.check(); }, this.opts.intervalMs ?? DEFAULT_INTERVAL_MS);
  }

  stop(): void {
    if (this.timer) { clearInterval(this.timer); this.timer = null; }
    // A check in flight finishes without acting.
    this.stopped = true;
  }

  status(): ReadbackStatus {
    return { ...this.current };
  }

  async check(): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      await this.read();
    } catch (err) {
      logger.error({ err }, 'Twitch read-back failed');
    } finally {
      this.busy = false;
    }
  }

  private async read(): Promise<void> {
    const sup = await this.opts.client.status();
    // Slate, holds, recoveries and missing keys show no clock, or no stream at all.
    if (streamState(sup, 'running', false).state !== 'live' || sup?.playing?.source === 'slate') {
      this.badReads = 0;
      return;
    }
    const rendition = await liveRendition(this.opts.channel, this.opts.fetchImpl);
    if (!rendition) return this.unavailable("the channel's video playlist could not be fetched");
    const media = await fetchText(rendition.url, this.opts.fetchImpl ?? fetch);
    if (media === null) return this.unavailable("the channel's video playlist could not be fetched");
    if (isAdPlaylist(media)) {
      logger.debug('Twitch read-back skipped: an ad is playing');
      this.badReads = 0;
      return;
    }

    // Without a duration the overlay shows the title alone, which never moves.
    const clock = this.opts.overlay && (sup?.playing?.duration ?? 0) > 0;
    const region = clock ? timerRegion(rendition.width, rendition.height) : null;
    const a = await grabGrayRegion(this.opts.ffmpegPath, rendition.url, region, this.opts.run);
    if (a) await new Promise((r) => setTimeout(r, this.opts.gapMs ?? DEFAULT_GAP_MS));
    const b = a && await grabGrayRegion(this.opts.ffmpegPath, rendition.url, region, this.opts.run);
    if (this.stopped) return;
    if (!a || !b) return this.unavailable('a frame could not be read from Twitch');

    this.inOutage = false;
    const diff = meanAbsDiff(a, b);
    const bad = diff < (this.opts.threshold ?? DEFAULT_THRESHOLD);
    this.current = { lastCheckAt: Date.now(), lastDiff: diff, state: bad ? 'frozen' : 'ok' };
    logger.debug({ diff, region: region ?? 'whole frame' }, 'Twitch read-back');
    if (!bad) {
      this.badReads = 0;
      this.stillNoted = false;
      return;
    }
    this.badReads++;
    if (this.badReads < BAD_READS_TO_ACT) return;
    if (!region) {
      if (!this.stillNoted) { this.stillNoted = true; this.opts.onStill(); }
      return;
    }
    if ((await this.opts.isChannelLive()) === true && !this.stopped) {
      this.badReads = 0;
      this.opts.onFrozen();
    }
  }

  private unavailable(why: string): void {
    this.badReads = 0;
    this.current = { ...this.current, lastCheckAt: Date.now(), state: 'unavailable' };
    if (this.inOutage) return;
    this.inOutage = true;
    logger.warn({ why }, 'Twitch read-back unavailable');
    this.opts.onUnavailable(why);
  }
}
