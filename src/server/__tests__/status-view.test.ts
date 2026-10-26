import { describe, it, expect } from 'vitest';
// @ts-ignore — plain browser JS module without a declaration file
import { streamLine, twitchLine, isBenignReloadError, sourceLabel, wizardVerdict, formatTime, WIZARD_OFF_AIR_GRACE_MS, WIZARD_TIMEOUT_MS, encoderLabel, autostartLabel, dependencyRows, changedFields, savePatch, encoderOptionsHtml, youtubeNote } from '../../admin/status-view.js';

const twitch = (over: Record<string, unknown> = {}) => ({
  enabled: true,
  channelLive: true,
  encoderAlive: true,
  consecutiveMismatches: 0,
  mismatchThreshold: 3,
  lastCheckAt: 1,
  lastRestartAt: null,
  restartCount: 0,
  readback: { lastCheckAt: 1, lastDiff: 12, state: 'ok' },
  ...over,
});

describe('streamLine', () => {
  it.each([
    ['live', 'Live', 'ok'],
    ['recovering', 'Recovering', 'warn'],
    ['starting', 'Starting', 'warn'],
    ['off-air', 'Off air', 'err'],
    ['stopped', 'Stopped', 'muted'],
  ])('shows %s as %s in %s colour', (state, word, level) => {
    const line = streamLine({ state, reason: 'why' });
    expect(line.word).toBe(word);
    expect(line.level).toBe(level);
  });

  it('hides the reason while live', () => {
    expect(streamLine({ state: 'live', reason: 'stale' }).reason).toBeNull();
  });

  it('shows the reason when not live', () => {
    expect(streamLine({ state: 'off-air', reason: 'The encoder is not running.' }).reason).toBe('The encoder is not running.');
  });

  it('treats an unknown or missing state as off air with a reason', () => {
    expect(streamLine({ state: 'weird', reason: null })).toMatchObject({ word: 'Off air', level: 'err' });
    expect(streamLine(undefined).reason).toBeTruthy();
  });
});

describe('twitchLine', () => {
  it('says the check is off when liveness is disabled', () => {
    expect(twitchLine(twitch({ enabled: false })).text).toBe('Twitch check off');
  });

  it('puts the read-back next to the live check on one line', () => {
    expect(twitchLine(twitch())).toEqual({ text: 'Twitch: live · read-back: picture moving', level: 'ok' });
  });

  it('marks a frozen read-back as an error even when the channel is live', () => {
    expect(twitchLine(twitch({ readback: { lastCheckAt: 1, lastDiff: 0, state: 'frozen' } })).level).toBe('err');
  });

  it('shows the mismatch count while the channel is offline', () => {
    const line = twitchLine(twitch({ channelLive: false, consecutiveMismatches: 2, readback: { lastCheckAt: null, lastDiff: null, state: 'off' } }));
    expect(line).toEqual({ text: 'Twitch: offline (2/3) · read-back off', level: 'err' });
  });

  it('says the read-back is waiting before its first check', () => {
    expect(twitchLine(twitch({ readback: { lastCheckAt: null, lastDiff: null, state: 'ok' } })).text).toContain('waiting for the first check');
  });
});

describe('isBenignReloadError', () => {
  it('treats no error and a refused connection as benign', () => {
    expect(isBenignReloadError({ reloadError: null, reloadErrorCode: null })).toBe(true);
    expect(isBenignReloadError({ reloadError: 'fetch failed', reloadErrorCode: 'ECONNREFUSED' })).toBe(true);
  });

  it('reports any other failed fetch, such as a timeout or a reset', () => {
    expect(isBenignReloadError({ reloadError: 'fetch failed', reloadErrorCode: 'ECONNRESET' })).toBe(false);
    expect(isBenignReloadError({ reloadError: 'The operation was aborted due to timeout', reloadErrorCode: null })).toBe(false);
  });

  it('reports an error the supervisor answered with', () => {
    expect(isBenignReloadError({ reloadError: 'HTTP 400', reloadErrorCode: null })).toBe(false);
  });
});

describe('sourceLabel', () => {
  it('names the known sources and passes others through', () => {
    expect(sourceLabel('slate')).toBe('Standby slate');
    expect(sourceLabel('other')).toBe('other');
    expect(sourceLabel(undefined)).toBe('-');
  });
});

describe('wizardVerdict', () => {
  it('ends on live', () => {
    expect(wizardVerdict({ state: 'live' }, 0)).toBe('live');
  });

  it('waits through starting until the timeout', () => {
    expect(wizardVerdict({ state: 'starting' }, WIZARD_OFF_AIR_GRACE_MS + 1)).toBe('wait');
    expect(wizardVerdict({ state: 'starting' }, WIZARD_TIMEOUT_MS)).toBe('reason');
  });

  it('gives off air a grace period before showing the reason', () => {
    expect(wizardVerdict({ state: 'off-air' }, 1000)).toBe('wait');
    expect(wizardVerdict({ state: 'off-air' }, WIZARD_OFF_AIR_GRACE_MS)).toBe('reason');
  });
});

describe('formatTime', () => {
  it('formats seconds as h:mm:ss', () => {
    expect(formatTime(3725.9)).toBe('1:02:05');
    expect(formatTime(null)).toBe('0:00:00');
  });
});

const report = (encoder: Record<string, unknown> = {}, dependencies: { name: string; ok: boolean; path: string }[] = []) => ({
  at: '2026-10-25T10:00:00.000Z',
  encoder: { chosen: 'h264_nvenc', tried: ['h264_nvenc'], software: false, works: true, ...encoder },
  cache: { freeMb: 100000, recommendedMb: 4096, warning: null },
  dependencies,
});

describe('encoderLabel', () => {
  it('names a working hardware encoder as ok', () => {
    expect(encoderLabel(report())).toEqual({ text: 'NVIDIA (NVENC)', level: 'ok' });
  });

  it('warns when only the software encoder works', () => {
    const label = encoderLabel(report({ chosen: 'libx264', tried: ['h264_nvenc', 'h264_amf', 'h264_qsv', 'libx264'], software: true }));
    expect(label.level).toBe('warn');
    expect(label.text).toContain('Software');
  });

  it('shows an error, not a software pick, when no encoder works', () => {
    const label = encoderLabel(report({ chosen: 'libx264', software: true, works: false }));
    expect(label).toEqual({ text: 'No encoder works on this PC. Check the graphics driver.', level: 'err' });
  });
});

describe('autostartLabel', () => {
  it('says Off when there is no autostart file', () => {
    expect(autostartLabel({ enabled: false, target: null, thisInstall: false })).toBe('Off');
  });

  it('says This install when the file starts this install', () => {
    expect(autostartLabel({ enabled: true, target: 'C:\SL\START.bat', thisInstall: true })).toBe('This install');
  });

  it('names the other install the file starts', () => {
    expect(autostartLabel({ enabled: true, target: 'D:\Old\START.bat', thisInstall: false })).toBe('Another install: D:\Old\START.bat');
  });

  it('flags a StreamLoop.vbs that StreamLoop did not write', () => {
    expect(autostartLabel({ enabled: true, target: null, thisInstall: false })).toBe('set by another program (StreamLoop.vbs in the Startup folder)');
  });
});

describe('dependencyRows', () => {
  it('lists failures first and keeps the order otherwise', () => {
    const rows = dependencyRows(report({}, [
      { name: 'Supervisor', ok: true, path: 'a' },
      { name: 'ffmpeg', ok: false, path: 'b' },
      { name: 'yt-dlp', ok: true, path: 'c' },
      { name: 'Deno', ok: false, path: 'd' },
    ]));
    expect(rows.map((r: { name: string }) => r.name)).toEqual(['ffmpeg', 'Deno', 'Supervisor', 'yt-dlp']);
  });

  it('gives each row a line that reads as a sentence', () => {
    const rows = dependencyRows(report({}, [
      { name: 'Token server modules', ok: false, path: 'C:\\SL\\pot\\node_modules' },
      { name: 'ffmpeg', ok: false, path: 'C:\\SL\\ffmpeg\\ffmpeg.exe' },
      { name: 'Deno', ok: true, path: 'C:\\SL\\yt-dlp\\deno.exe' },
    ]));
    expect(rows.map((r: { text: string }) => r.text)).toEqual([
      'Token server packages are missing: C:\\SL\\pot\\node_modules',
      'ffmpeg is missing: C:\\SL\\ffmpeg\\ffmpeg.exe',
      'Deno',
    ]);
  });
});

describe('savePatch', () => {
  const loaded = { supervisor: { encoder: 'h264_nvenc', cacheBudgetMb: 300 } };

  it('makes a picked encoder the preference', () => {
    expect(savePatch(loaded, { supervisor: { encoder: 'libx264', cacheBudgetMb: 300 } }))
      .toEqual({ supervisor: { encoder: 'libx264', preferredEncoder: 'libx264' } });
  });

  it('sends no preference when the encoder was not changed', () => {
    expect(savePatch(loaded, { supervisor: { encoder: 'h264_nvenc', cacheBudgetMb: 500 } })).toEqual({ supervisor: { cacheBudgetMb: 500 } });
  });
});

describe('changedFields', () => {
  const loaded = {
    playlists: [{ id: 'PL1' }],
    twitchClientSecret: '********',
    supervisor: { encoder: 'libx264', cacheBudgetMb: 300, overlay: true },
  };

  it('leaves out a fallback encoder the user did not pick', () => {
    const edited = { supervisor: { encoder: 'libx264', cacheBudgetMb: 500, overlay: true } };
    expect(changedFields(loaded, edited)).toEqual({ supervisor: { cacheBudgetMb: 500 } });
  });

  it('sends the encoder when the user picked another one', () => {
    expect(changedFields(loaded, { supervisor: { encoder: 'h264_nvenc' } })).toEqual({ supervisor: { encoder: 'h264_nvenc' } });
  });

  it('returns nothing when nothing changed, including the masked secret', () => {
    expect(changedFields(loaded, { playlists: [{ id: 'PL1' }], twitchClientSecret: '********', supervisor: { overlay: true } })).toEqual({});
  });

  it('sends a changed array whole', () => {
    expect(changedFields(loaded, { playlists: [{ id: 'PL2' }, { id: 'PL1' }] })).toEqual({ playlists: [{ id: 'PL2' }, { id: 'PL1' }] });
  });

  it('sends a field the loaded config did not have', () => {
    expect(changedFields({}, { supervisor: { cacheBudgetMb: 50 } })).toEqual({ supervisor: { cacheBudgetMb: 50 } });
  });
});

describe('encoderOptionsHtml', () => {
  it('builds one option per encoder, in order, with the config value and the shown name', () => {
    expect(encoderOptionsHtml()).toBe(
      '<option value="h264_nvenc">NVIDIA (NVENC)</option>'
      + '<option value="h264_amf">AMD (AMF)</option>'
      + '<option value="h264_qsv">Intel (Quick Sync)</option>'
      + '<option value="libx264">Software (x264, uses the CPU)</option>',
    );
  });
});

describe('youtubeNote', () => {
  it('gives the next try in local time while the breaker is open', () => {
    const at = new Date(2026, 9, 26, 10, 41, 30).toISOString();
    expect(youtubeNote({ breaker: 'open', retryAt: at })).toBe('YouTube is blocking requests. Next try at 10:41.');
  });

  it('pads single-digit hours and minutes', () => {
    const at = new Date(2026, 9, 26, 7, 5).toISOString();
    expect(youtubeNote({ breaker: 'open', retryAt: at })).toBe('YouTube is blocking requests. Next try at 07:05.');
  });

  it('leaves out the time when the supervisor gives none', () => {
    expect(youtubeNote({ breaker: 'open', retryAt: null })).toBe('YouTube is blocking requests.');
  });

  it('says nothing while the breaker is closed or the status has no youtube part', () => {
    expect(youtubeNote({ breaker: 'closed', retryAt: null })).toBeNull();
    expect(youtubeNote(undefined)).toBeNull();
  });

  it('names the cookie problem, which is not YouTube blocking', () => {
    expect(youtubeNote({ breaker: 'closed', cookieProblem: 'yt-dlp could not read the firefox cookies: locked' })).toBe('yt-dlp could not read the firefox cookies: locked');
  });
});

describe('streamLine with a YouTube note', () => {
  it('adds the note to the reason, even while live on the slate', () => {
    expect(streamLine({ state: 'live', reason: null }, 'YouTube is blocking requests.').reason).toBe('YouTube is blocking requests.');
    expect(streamLine({ state: 'recovering', reason: 'Recovering the current video.' }, 'YouTube is blocking requests.').reason)
      .toBe('Recovering the current video. YouTube is blocking requests.');
  });

  it('shows no note while the stream is stopped', () => {
    expect(streamLine({ state: 'stopped', reason: 'Stopped.' }, 'YouTube is blocking requests.').reason).toBe('Stopped.');
  });
});
