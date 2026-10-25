import { describe, it, expect } from 'vitest';
// @ts-ignore — plain browser JS module without a declaration file
import { streamLine, twitchLine, isBenignReloadError, sourceLabel, wizardVerdict, formatTime, WIZARD_OFF_AIR_GRACE_MS, WIZARD_TIMEOUT_MS } from '../../admin/status-view.js';

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
