const FRAME_CHECK_INTERVAL_MS = 10_000;
const FRAME_CONFIRM_DELAY_MS = 2_000;

export interface FrameMonitorOptions {
  /** Returns a base64 image string, or null if capture is unavailable. */
  captureFrame: () => Promise<string | null>;
  /** Gate: when false, the tick is a no-op that resets the counter. */
  shouldCapture: () => boolean;
  /** Fired once when a freeze is confirmed. */
  onFreeze: () => void;
  /** Optional: fired when the static window is reached and confirmation begins. */
  onSuspect?: () => void;
  /** Optional: fired when the confirmation frame differs (false alarm). */
  onFalseAlarm?: () => void;
  /** Live config read each tick: how long static = frozen. */
  getWindowMs: () => number;
  /** Override poll interval (defaults to FRAME_CHECK_INTERVAL_MS). */
  intervalMs?: number;
  /** Override confirmation delay (defaults to FRAME_CONFIRM_DELAY_MS). */
  confirmDelayMs?: number;
}

/** FNV-1a 32-bit hash. Cheap, non-cryptographic, good enough for frame equality. */
function fnv1a(str: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

export class FrameMonitor {
  private readonly opts: FrameMonitorOptions;
  private readonly intervalMs: number;
  private readonly confirmDelayMs: number;
  private timer: ReturnType<typeof setInterval> | null = null;
  private confirmTimer: ReturnType<typeof setTimeout> | null = null;
  private prevHash: number | null = null;
  private staticCount = 0;
  private fired = false;
  private confirming = false;
  private stopped = false;

  constructor(opts: FrameMonitorOptions) {
    this.opts = opts;
    this.intervalMs = opts.intervalMs ?? FRAME_CHECK_INTERVAL_MS;
    this.confirmDelayMs = opts.confirmDelayMs ?? FRAME_CONFIRM_DELAY_MS;
  }

  start(): void {
    this.stop();
    this.stopped = false;
    this.timer = setInterval(() => { void this.tick(); }, this.intervalMs);
  }

  stop(): void {
    if (this.timer) { clearInterval(this.timer); this.timer = null; }
    if (this.confirmTimer) { clearTimeout(this.confirmTimer); this.confirmTimer = null; }
    this.confirming = false;
    this.stopped = true;
  }

  private resetCount(): void {
    this.staticCount = 0;
  }

  private async tick(): Promise<void> {
    if (this.confirming) return;
    if (!this.opts.shouldCapture()) {
      this.prevHash = null;
      this.staticCount = 0;
      this.fired = false;
      return;
    }
    const frame = await this.opts.captureFrame();
    if (frame == null) { this.resetCount(); return; }
    const hash = fnv1a(frame);
    if (hash !== this.prevHash) {
      this.prevHash = hash;
      this.staticCount = 0; // baseline frame: zero elapsed static time yet
      this.fired = false; // picture moved -> re-arm
      return;
    }
    // Each identical-to-previous frame == one interval of confirmed-static time.
    this.staticCount++;
    if (!this.fired && this.staticCount * this.intervalMs >= this.opts.getWindowMs()) {
      this.startConfirm();
    }
  }

  private startConfirm(): void {
    this.confirming = true;
    this.opts.onSuspect?.();
    this.confirmTimer = setTimeout(async () => {
      this.confirmTimer = null;
      try {
        const frame = await this.opts.captureFrame();
        if (this.stopped) return; // monitor was stopped while the capture was in flight
        if (frame == null) return; // can't confirm; re-evaluate next tick
        const hash = fnv1a(frame);
        if (hash === this.prevHash) {
          this.fired = true;
          this.opts.onFreeze();
        } else {
          this.prevHash = hash;
          this.staticCount = 0;
          this.opts.onFalseAlarm?.();
        }
      } finally {
        this.confirming = false;
      }
    }, this.confirmDelayMs);
  }
}
