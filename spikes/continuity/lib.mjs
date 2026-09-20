export const FPS = 30;
// The timeline starts above zero: at offset 0 ffmpeg shifts timestamps to avoid
// negatives, so the first clip would be treated differently from every later one.
export const BASE_OFFSET = 10;
export const LOCAL_URL = 'rtmp://127.0.0.1:1935/live/spike';
// MPEG-TS timestamps are 33 bits at 90 kHz, so the pipe's timeline wraps after 26.5 h.
export const PTS_WRAP_SECONDS = 2 ** 33 / 90000;

export const CLIPS = [
  { name: 'a-720p30-mono44k', size: '1280x720', rate: 30, color: '0x101040', sampleRate: 44100, channels: 1, seconds: 70 },
  { name: 'b-1080p60-stereo48k', size: '1920x1080', rate: 60, color: '0x104010', sampleRate: 48000, channels: 2, seconds: 45 },
  { name: 'c-480p25-4x3-mono32k', size: '640x480', rate: 25, color: '0x401010', sampleRate: 32000, channels: 1, seconds: 30 },
];

// Same formats, 13 s per cycle: a 2 h run sees about as many seams as a day of CLIPS.
export const SHORT_CLIPS = CLIPS.map((clip, i) => ({ ...clip, name: `${clip.name}-short`, seconds: [6, 4, 3][i] }));

/** A flash and a 1 kHz beep in the first 100 ms of every second, so A/V sync is measurable. */
export function inputArgs(clip, outFile) {
  return [
    '-v', 'error', '-y',
    '-f', 'lavfi', '-i', `color=c=${clip.color}:s=${clip.size}:r=${clip.rate},drawbox=c=white:t=fill:enable='lt(mod(t,1),0.1)'`,
    '-f', 'lavfi', '-i', `aevalsrc='0.5*sin(2*PI*1000*t)*lt(mod(t,1),0.1)':s=${clip.sampleRate}`,
    '-t', String(clip.seconds), '-ac', String(clip.channels),
    '-c:v', 'libx264', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-b:a', '128k',
    outFile,
  ];
}

export function feederArgs(file, offsetSeconds) {
  return [
    '-v', 'error', '-nostats', '-progress', 'pipe:2', '-i', file,
    '-vf', `scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,fps=${FPS},format=yuv420p`,
    '-af', 'aresample=48000,aformat=sample_fmts=s16:channel_layouts=stereo',
    // -bf 0 keeps dts equal to pts, so no clip starts with a negative dts.
    '-c:v', 'mpeg2video', '-q:v', '2', '-g', '15', '-bf', '0', '-c:a', 'mp2', '-b:a', '384k',
    '-muxdelay', '0', '-muxpreload', '0', '-mpegts_flags', '+initial_discontinuity',
    '-output_ts_offset', offsetSeconds.toFixed(6), '-f', 'mpegts', 'pipe:1',
  ];
}

export function encoderArgs(target, { codec = 'libx264', tee = false } = {}) {
  const video = codec === 'h264_nvenc'
    ? ['-c:v', 'h264_nvenc', '-preset', 'p4', '-rc', 'cbr', '-bf', '0']
    : ['-c:v', 'libx264', '-preset', 'veryfast', '-sc_threshold', '0', '-keyint_min', '60'];
  return [
    '-v', 'warning', '-nostats', '-stats_period', '0.1', '-progress', 'pipe:2',
    // Pacing lives here: the encoder reads in real time and feeders block on the pipe.
    '-re', '-f', 'mpegts', '-i', 'pipe:0',
    ...video,
    '-b:v', '4500k', '-maxrate', '4500k', '-bufsize', '9000k', '-g', '60', '-pix_fmt', 'yuv420p',
    '-af', 'aresample=async=1', '-c:a', 'aac', '-b:a', '160k', '-ar', '48000',
    ...(tee
      ? ['-map', '0:v', '-map', '0:a', '-flags', '+global_header', '-f', 'tee', target]
      : ['-f', 'flv', target]),
  ];
}

export function offsetAfter(offset, frames) {
  return offset + frames / FPS;
}

export function parseProgress(text) {
  const out = {};
  for (const line of String(text).split(/\r?\n/)) {
    const m = /^(\w+)=(.*)$/.exec(line.trim());
    if (m) out[m[1]] = m[2].trim();
  }
  return out;
}

export function createAnalyzer({ jumpSec = 0.1 } = {}) {
  const last = {};
  const events = [];
  return {
    events,
    push({ stream, dts }) {
      if (stream in last) {
        const step = dts - last[stream];
        if (step < 0) events.push({ type: 'backwards', stream, at: dts, value: step });
        else if (step > jumpSec) events.push({ type: 'jump', stream, at: dts, value: step });
      }
      last[stream] = dts;
    },
  };
}

function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** Positive means the beep comes after the flash. Beeps with no flash within 0.5 s are dropped. */
export function pairOffsets(flashes, beeps) {
  const offsets = [];
  for (const beep of beeps) {
    let best = null;
    for (const flash of flashes) {
      if (best === null || Math.abs(beep - flash) < Math.abs(beep - best)) best = flash;
    }
    if (best !== null && Math.abs(beep - best) <= 0.5) offsets.push((beep - best) * 1000);
  }
  if (offsets.length === 0) return { pairs: 0, medianMs: NaN, maxAbsMs: NaN };
  return { pairs: offsets.length, medianMs: median(offsets), maxAbsMs: Math.max(...offsets.map(Math.abs)) };
}

/** rows: { wallMs, mediaUs } from the encoder's progress output. */
export function lagStats(rows) {
  const first = rows[0];
  const lastRow = rows[rows.length - 1];
  // The first minute holds ffmpeg's start-up burst, which is not a seam.
  const lags = rows
    .filter((r) => r.wallMs - first.wallMs >= 60_000)
    .map((r) => (r.wallMs - first.wallMs) - (r.mediaUs - first.mediaUs) / 1000);
  // Not Math.max(...lags): a multi-hour run has enough rows to overflow the call stack.
  const maxLag = lags.reduce((max, lag) => (lag > max ? lag : max), -Infinity);
  const maxRiseMs = lags.length ? maxLag - median(lags) : NaN;
  const overallSpeed = ((lastRow.mediaUs - first.mediaUs) / 1000) / (lastRow.wallMs - first.wallMs);
  return { maxRiseMs, overallSpeed };
}

export function verdict(m) {
  const result = {
    noBackwards: m.backwards === 0,
    readerStayed: m.readerDisconnects === 0,
    encoderStayed: !m.encoderExitedEarly,
    seamLag: m.maxRiseMs < 500,
    speed: m.overallSpeed >= 0.99 && m.overallSpeed <= 1.01,
    avInSync: Math.abs(m.avFirstMs) < 100 && Math.abs(m.avLastMs) < 100,
    avStable: Math.abs(m.avLastMs - m.avFirstMs) <= 50,
  };
  return { ...result, pass: Object.values(result).every(Boolean) };
}
