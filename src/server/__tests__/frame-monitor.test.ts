import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { FrameMonitor } from '../frame-monitor.js';

const INTERVAL = 10_000;
const CONFIRM = 2_000;

function setup(opts: {
  frames: () => string | null;       // what captureFrame returns each call
  shouldCapture?: () => boolean;
  windowMs?: number;
}) {
  const onFreeze = vi.fn();
  const onSuspect = vi.fn();
  const onFalseAlarm = vi.fn();
  const captureFrame = vi.fn(async () => opts.frames());
  const monitor = new FrameMonitor({
    captureFrame,
    shouldCapture: opts.shouldCapture ?? (() => true),
    onFreeze,
    onSuspect,
    onFalseAlarm,
    getWindowMs: () => opts.windowMs ?? 30_000,
  });
  return { monitor, onFreeze, onSuspect, onFalseAlarm, captureFrame };
}

describe('FrameMonitor', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('fires once after a sustained static window confirmed by a second screenshot', async () => {
    const { monitor, onFreeze } = setup({ frames: () => 'AAAA', windowMs: 30_000 });
    monitor.start();
    // staticCount = number of frames identical to the previous one, so frozen
    // duration = staticCount * interval. A 30s window / 10s interval needs
    // staticCount 3, i.e. a baseline capture plus 3 identical ones (4 ticks).
    await vi.advanceTimersByTimeAsync(INTERVAL * 3); // staticCount 2 (20s) — not enough
    expect(onFreeze).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(INTERVAL);     // staticCount 3 (30s) — confirm scheduled
    expect(onFreeze).not.toHaveBeenCalled();         // confirmation still pending
    await vi.advanceTimersByTimeAsync(CONFIRM);
    expect(onFreeze).toHaveBeenCalledTimes(1);
    monitor.stop();
  });

  it('does not fire if the confirmation frame differs, and reports a false alarm', async () => {
    let val = 'AAAA';
    const { monitor, onFreeze, onSuspect, onFalseAlarm } = setup({ frames: () => val, windowMs: 30_000 });
    monitor.start();
    await vi.advanceTimersByTimeAsync(INTERVAL * 4); // window reached, confirm scheduled
    expect(onSuspect).toHaveBeenCalledTimes(1);
    val = 'BBBB';                                    // picture moved during confirm delay
    await vi.advanceTimersByTimeAsync(CONFIRM);
    expect(onFreeze).not.toHaveBeenCalled();
    expect(onFalseAlarm).toHaveBeenCalledTimes(1);
    monitor.stop();
  });

  it('does not fire if the confirmation frame is null', async () => {
    let val: string | null = 'AAAA';
    const { monitor, onFreeze } = setup({ frames: () => val, windowMs: 30_000 });
    monitor.start();
    await vi.advanceTimersByTimeAsync(INTERVAL * 4);
    val = null;
    await vi.advanceTimersByTimeAsync(CONFIRM);
    expect(onFreeze).not.toHaveBeenCalled();
    monitor.stop();
  });

  it('resets the counter when a frame changes mid-window', async () => {
    let val = 'AAAA';
    const { monitor, onFreeze } = setup({ frames: () => val, windowMs: 30_000 });
    monitor.start();
    await vi.advanceTimersByTimeAsync(INTERVAL * 3); // staticCount 2
    val = 'CCCC';                                    // change -> reset baseline
    await vi.advanceTimersByTimeAsync(INTERVAL * 3); // staticCount back to 2, not enough
    await vi.advanceTimersByTimeAsync(CONFIRM);
    expect(onFreeze).not.toHaveBeenCalled();
    monitor.stop();
  });

  it('treats a null frame mid-window as a reset and never fires', async () => {
    let val: string | null = 'AAAA';
    const { monitor, onFreeze } = setup({ frames: () => val, windowMs: 30_000 });
    monitor.start();
    await vi.advanceTimersByTimeAsync(INTERVAL * 2);
    val = null;
    await vi.advanceTimersByTimeAsync(INTERVAL * 4 + CONFIRM);
    expect(onFreeze).not.toHaveBeenCalled();
    monitor.stop();
  });

  it('does nothing while shouldCapture() is false', async () => {
    const { monitor, onFreeze, captureFrame } = setup({
      frames: () => 'AAAA',
      shouldCapture: () => false,
      windowMs: 30_000,
    });
    monitor.start();
    await vi.advanceTimersByTimeAsync(INTERVAL * 5 + CONFIRM);
    expect(captureFrame).not.toHaveBeenCalled();
    expect(onFreeze).not.toHaveBeenCalled();
    monitor.stop();
  });

  it('re-arms after firing once the picture changes again', async () => {
    let val = 'AAAA';
    const { monitor, onFreeze } = setup({ frames: () => val, windowMs: 30_000 });
    monitor.start();
    await vi.advanceTimersByTimeAsync(INTERVAL * 4 + CONFIRM);
    expect(onFreeze).toHaveBeenCalledTimes(1);
    val = 'DDDD';                                    // picture moves -> cooldown clears, new baseline
    await vi.advanceTimersByTimeAsync(INTERVAL);
    val = 'DDDD';
    await vi.advanceTimersByTimeAsync(INTERVAL * 4 + CONFIRM); // freeze again
    expect(onFreeze).toHaveBeenCalledTimes(2);
    monitor.stop();
  });
});
