// Pure decisions for the dashboard, kept out of admin.js so vitest can import them.

const STREAM_WORDS = {
  live: { word: 'Live', level: 'ok' },
  recovering: { word: 'Recovering', level: 'warn' },
  starting: { word: 'Starting', level: 'warn' },
  'off-air': { word: 'Off air', level: 'err' },
  stopped: { word: 'Stopped', level: 'muted' },
};

/** The stream line: word, colour level and the reason to show (null when Live). */
export function streamLine(stream) {
  const known = stream && STREAM_WORDS[stream.state];
  const { word, level } = known || STREAM_WORDS['off-air'];
  const live = known && stream.state === 'live';
  const reason = live ? null : (stream && stream.reason) || (known ? null : 'The server sent no stream state.');
  return { word, level, reason };
}

/** The Twitch live check and the read-back, as one line. */
export function twitchLine(twitch) {
  if (!twitch || !twitch.enabled) return { text: 'Twitch check off', level: 'muted' };
  let text;
  let level;
  if (twitch.channelLive === null) {
    text = 'Twitch: checking';
    level = 'warn';
  } else if (twitch.channelLive) {
    text = 'Twitch: live';
    level = 'ok';
  } else {
    text = 'Twitch: offline';
    level = twitch.consecutiveMismatches > 0 ? 'err' : 'warn';
  }
  if (twitch.consecutiveMismatches > 0) text += ` (${twitch.consecutiveMismatches}/${twitch.mismatchThreshold})`;

  const rb = twitch.readback || { state: 'off', lastCheckAt: null };
  if (rb.state === 'off') return { text: text + ' · read-back off', level };
  if (rb.state === 'frozen') return { text: text + ' · read-back: picture frozen', level: 'err' };
  if (rb.state === 'unavailable') return { text: text + ' · read-back: no picture from Twitch', level: level === 'ok' ? 'warn' : level };
  return { text: text + (rb.lastCheckAt ? ' · read-back: picture moving' : ' · read-back: waiting for the first check'), level };
}

/** A refused reload is harmless: no supervisor is running, and it reads the saved config at start. */
export function isBenignReloadError({ reloadError, reloadErrorCode }) {
  return !reloadError || reloadErrorCode === 'ECONNREFUSED';
}

const SOURCE_LABELS = { file: 'Cached file', url: 'YouTube', slate: 'Standby slate' };

export function sourceLabel(source) {
  return SOURCE_LABELS[source] || source || '-';
}

/** How long the wizard waits on off-air before it shows the reason, and before it gives up on anything. */
export const WIZARD_OFF_AIR_GRACE_MS = 15000;
export const WIZARD_TIMEOUT_MS = 90000;

/**
 * The wizard's verify step: 'live' ends it, 'wait' polls again, 'reason' ends it showing stream.reason.
 * Off air right after the first save is normal (the supervisor is still launching), so it gets a grace period.
 */
export function wizardVerdict(stream, elapsedMs) {
  const state = stream && stream.state;
  if (state === 'live') return 'live';
  if (elapsedMs >= WIZARD_TIMEOUT_MS) return 'reason';
  if (state === 'starting' || state === 'recovering') return 'wait';
  return elapsedMs >= WIZARD_OFF_AIR_GRACE_MS ? 'reason' : 'wait';
}

/** Seconds as h:mm:ss. */
export function formatTime(seconds) {
  if (!seconds || seconds < 0) return '0:00:00';
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  return `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

/** Same names as the Playback encoder dropdown. */
export const ENCODER_NAMES = {
  h264_nvenc: 'NVIDIA (NVENC)',
  h264_amf: 'AMD (AMF)',
  h264_qsv: 'Intel (Quick Sync)',
  libx264: 'Software (x264, uses the CPU)',
};

/** The setup check's encoder line. works false means nothing works, so libx264 there is not a pick. */
export function encoderLabel(report) {
  const { chosen, software, works } = report.encoder;
  if (!works) return { text: 'No encoder works on this PC. Check the graphics driver.', level: 'err' };
  const name = ENCODER_NAMES[chosen] || chosen;
  if (software) return { text: `${name}. No graphics card encoder works on this PC.`, level: 'warn' };
  return { text: name, level: 'ok' };
}

/** Where the Startup folder's StreamLoop.vbs points, from GET /api/autostart. */
export function autostartLabel(status) {
  if (!status.enabled) return 'Off';
  if (status.thisInstall) return 'This install';
  if (status.target) return `Another install: ${status.target}`;
  return 'Unknown: StreamLoop.vbs in the Startup folder was not written by StreamLoop';
}

export function dependencyRows(report) {
  const deps = report.dependencies || [];
  return [...deps.filter((d) => !d.ok), ...deps.filter((d) => d.ok)];
}

const isPlainObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);

/** The part of edited that differs from original, as a POST /api/config patch: objects key by key, arrays whole.
 *  A save then never resends a value the server set on its own, such as a fallback encoder. */
export function changedFields(original, edited) {
  const out = {};
  for (const [key, value] of Object.entries(edited)) {
    const before = original ? original[key] : undefined;
    if (isPlainObject(value)) {
      const inner = changedFields(isPlainObject(before) ? before : {}, value);
      if (Object.keys(inner).length > 0) out[key] = inner;
    } else if (JSON.stringify(value) !== JSON.stringify(before)) {
      out[key] = value;
    }
  }
  return out;
}
