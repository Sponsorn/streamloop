import { describe, it, expect } from 'vitest';
// @ts-ignore — plain browser JS module without a declaration file
import { streamLine, twitchLine, isBenignReloadError, sourceLabel, wizardVerdict, formatTime, WIZARD_OFF_AIR_GRACE_MS, WIZARD_TIMEOUT_MS, encoderLabel, autostartLabel, dependencyRows, changedFields, savePatch, encoderOptionsHtml, youtubeNote, playingListIndex, updateOfferText, updateProgressText, formatElapsed, updateRunning, updateBannerView, appDownloadText } from '../status-view.js';

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
      { name: 'Token server', ok: false, path: 'C:\\SL\\supervisor\\streamloop-pot.exe' },
      { name: 'ffmpeg', ok: false, path: 'C:\\SL\\ffmpeg\\ffmpeg.exe' },
      { name: 'Deno', ok: true, path: 'C:\\SL\\yt-dlp\\deno.exe' },
    ]));
    expect(rows.map((r: { text: string }) => r.text)).toEqual([
      'Token server is missing: C:\\SL\\supervisor\\streamloop-pot.exe',
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

describe('playingListIndex', () => {
  const lists = [{ id: 'PLa' }, { id: 'PLb' }];

  it('finds the list on air in the configured order', () => {
    expect(playingListIndex(lists, 'PLb')).toBe(1);
  });

  it('takes the first list when the supervisor names none', () => {
    expect(playingListIndex(lists, null)).toBe(0);
  });

  it('takes the first list when the one on air has left the config', () => {
    expect(playingListIndex(lists, 'PLgone')).toBe(0);
  });
});

describe('formatElapsed', () => {
  it('formats elapsed times', () => {
    // Act + Assert
    expect(formatElapsed(0)).toBe('0 s');
    expect(formatElapsed(45_000)).toBe('45 s');
    expect(formatElapsed(80_000)).toBe('1 min 20 s');
    expect(formatElapsed(12 * 60_000 + 5_000)).toBe('12 min');
    expect(formatElapsed(-5)).toBe('0 s');
  });
});

describe('updateOfferText', () => {
  const base = { currentVersion: '3.0.5', latestVersion: '3.0.6', kind: 'app', parts: [], sameVersion: false };

  it('keeps the app-only text', () => {
    expect(updateOfferText(base)).toBe('Version 3.0.6 is available (current: 3.0.5)');
  });

  it('names the parts of a full update', () => {
    // Act
    const text = updateOfferText({ ...base, kind: 'full', parts: ['node', 'ffmpeg', 'ytdlp-plugin'] });

    // Assert
    expect(text).toBe('Full update: the stream goes off air while these parts are replaced: Node, ffmpeg, yt-dlp plugin.');
  });

  it('offers a same-version parts update', () => {
    expect(updateOfferText({ ...base, latestVersion: '3.0.5', kind: 'full', parts: ['pot'], sameVersion: true })).toBe('Update parts to 3.0.5');
  });
});

describe('updateProgressText', () => {
  const state = (over: Record<string, unknown> = {}) => ({
    from: '3.0.5', to: '3.0.6', parts: ['node'], folders: ['node'], step: 'swap', result: 'running', reason: null,
    startedAt: '2026-11-08T10:00:00.000Z', stepStartedAt: '2026-11-08T10:00:00.000Z', baseline: null, ...over,
  });
  const at = (s: number) => Date.parse('2026-11-08T10:00:00.000Z') + s * 1000;

  it('shows the step and its elapsed time', () => {
    expect(updateProgressText(state(), [], at(80))).toBe('Replacing files, 1 min 20 s');
    expect(updateProgressText(state({ step: 'stop' }), [], at(5))).toBe('Stopping the stream, 5 s');
    expect(updateProgressText(state({ step: 'download' }), [], at(0))).toBe('Downloading, 0 s');
    expect(updateProgressText(state({ step: 'check' }), [], at(3))).toBe('Checking the new version, 3 s');
    expect(updateProgressText(state({ step: 'start' }), [], at(3))).toBe('Starting the stream, 3 s');
  });

  it('shows the download percentage during the download step only', () => {
    expect(updateProgressText(state({ step: 'download', downloadPercent: 42 }), [], at(70))).toBe('Downloading, 42%, 1 min 10 s');
    expect(updateProgressText(state({ step: 'download', downloadPercent: null }), [], at(70))).toBe('Downloading, 1 min 10 s');
    expect(updateProgressText(state({ step: 'stop', downloadPercent: 100 }), [], at(5))).toBe('Stopping the stream, 5 s');
  });

  it('mentions a swap retry', () => {
    const log = [{ folder: 'node', status: 'retry', detail: 'in use' }];
    expect(updateProgressText(state(), log, at(10))).toBe('Replacing files, 10 s (trying node again)');
  });

  it('shows the ready copy when the update went through, and back on air once the stream is up', () => {
    expect(updateProgressText(state({ step: 'start', result: 'ok' }), [], at(1))).toBe('Updated to 3.0.6. Ready to stream.');
    expect(updateProgressText(state({ step: 'done', result: 'ok' }), [], at(1))).toBe('Back on air on 3.0.6.');
  });

  it('shows the failure with step and reason', () => {
    // Act
    const failed = updateProgressText(state({ step: 'swap', result: 'failed', reason: 'ffmpeg is still running.' }), [], at(1));
    const back = updateProgressText(state({ step: 'check', result: 'rolled-back', reason: 'No encoder works.' }), [], at(1));

    // Assert
    expect(failed).toBe('The update to 3.0.6 failed at replacing files: ffmpeg is still running. Still on 3.0.5.');
    expect(back).toBe('The update to 3.0.6 failed at checking the new version: No encoder works. Still on 3.0.5.');
  });
});

describe('appDownloadText', () => {
  it('adds the percentage of an app-only download when the server knows it', () => {
    expect(appDownloadText({ latestVersion: '3.0.7', downloadPercent: 42 })).toBe('Downloading v3.0.7, 42%...');
    expect(appDownloadText({ latestVersion: '3.0.7', downloadPercent: null })).toBe('Downloading v3.0.7...');
  });
});

describe('updateRunning', () => {
  it('is true only while the result is running', () => {
    expect(updateRunning({ result: 'running' })).toBe(true);
    expect(updateRunning({ result: 'ok' })).toBe(false);
    expect(updateRunning(null)).toBe(false);
  });
});

describe('updateBannerView', () => {
  const T0 = Date.parse('2026-11-08T10:00:00.000Z');
  const progress = (over: Record<string, unknown> = {}) => ({
    from: '3.0.5', to: '3.0.6', parts: ['node'], folders: ['node'], step: 'swap', result: 'running', reason: 'boom',
    startedAt: new Date(T0).toISOString(), stepStartedAt: new Date(T0).toISOString(), baseline: null, ...over,
  });
  const status = (over: Record<string, unknown> = {}) => ({ currentVersion: '3.0.6', updateAvailable: false, status: 'idle', progress: null, swapLog: [], ...over });

  it('hides the button while an update runs', () => {
    // Act
    const view = updateBannerView(status({ updateAvailable: true, progress: progress() }), T0 + 5000);

    // Assert
    expect(view).toEqual({ visible: true, text: 'Replacing files, 5 s', showButton: false, keepText: true });
  });

  it('keeps the failure text and still shows the button when an update is available', () => {
    // Act
    const view = updateBannerView(status({ updateAvailable: true, progress: progress({ result: 'failed' }) }), T0 + 3_600_000);

    // Assert
    expect(view.visible).toBe(true);
    expect(view.text).toBe('The update to 3.0.6 failed at replacing files: boom. Still on 3.0.5.');
    expect(view.showButton).toBe(true);
    expect(view.keepText).toBe(true);
  });

  it('shows the failure without a button when no update is available', () => {
    expect(updateBannerView(status({ progress: progress({ result: 'rolled-back' }) }), T0).showButton).toBe(false);
  });

  it('shows ok for 10 minutes and then falls back to the offer', () => {
    // Arrange
    const ok = progress({ result: 'ok', step: 'done' });

    // Act + Assert
    expect(updateBannerView(status({ progress: ok }), T0 + 9 * 60_000)).toMatchObject({ visible: true, text: 'Back on air on 3.0.6.', showButton: false });
    expect(updateBannerView(status({ progress: ok }), T0 + 11 * 60_000)).toMatchObject({ visible: false, showButton: true, keepText: false });
    expect(updateBannerView(status({ updateAvailable: true, progress: ok }), T0 + 11 * 60_000)).toMatchObject({ visible: true, showButton: true });
  });

  it('drops the ready banner once the stream is live, and keeps back on air', () => {
    // Arrange
    const ready = progress({ result: 'ok', step: 'start' });
    const back = progress({ result: 'ok', step: 'done' });

    // Act + Assert
    expect(updateBannerView(status({ progress: ready }), T0 + 5000, 'stopped')).toMatchObject({ visible: true, text: 'Updated to 3.0.6. Ready to stream.' });
    expect(updateBannerView(status({ progress: ready }), T0 + 5000, 'live')).toMatchObject({ visible: false, keepText: false });
    expect(updateBannerView(status({ progress: back }), T0 + 5000, 'live')).toMatchObject({ visible: true, text: 'Back on air on 3.0.6.' });
  });

  it('hides a finished update that neither came from nor went to the running version', () => {
    // Arrange: a failed full update from 3.0.5 to 3.0.6, then an app-only update to 3.0.8.
    const failed = progress({ result: 'failed' });

    // Act
    const later = updateBannerView(status({ currentVersion: '3.0.8', progress: failed }), T0 + 5000);
    const offer = updateBannerView(status({ currentVersion: '3.0.8', updateAvailable: true, progress: failed }), T0 + 5000);
    const stillOld = updateBannerView(status({ currentVersion: '3.0.5', progress: failed }), T0 + 5000);

    // Assert
    expect(later).toMatchObject({ visible: false, keepText: false });
    expect(offer).toMatchObject({ visible: true, showButton: true, keepText: false });
    expect(stillOld).toMatchObject({ visible: true, keepText: true, text: 'The update to 3.0.6 failed at replacing files: boom. Still on 3.0.5.' });
  });
});
