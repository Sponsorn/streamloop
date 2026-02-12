import { execFile, spawn } from 'child_process';
import { existsSync, rmSync } from 'fs';
import { basename, dirname, join } from 'path';
import OBSWebSocket from 'obs-websocket-js';
import type { AppConfig } from './types.js';
import { logger } from './logger.js';

const DEFAULT_OBS_PATHS = [
  'C:\\Program Files\\obs-studio\\bin\\64bit\\obs64.exe',
  'C:\\Program Files (x86)\\obs-studio\\bin\\64bit\\obs64.exe',
];

export class OBSClient {
  private obs = new OBSWebSocket();
  private config: AppConfig;
  private connected = false;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelay = 5000;
  private failedReconnects = 0;
  private obsLaunched = false;
  private onConnectCallback: (() => void) | null = null;
  private onDisconnectCallback: (() => void) | null = null;
  private onStreamDropCallback: ((attempt: number, maxAttempts: number) => void) | null = null;
  private onStreamRestartCallback: ((attempts: number) => void) | null = null;
  private onStreamRestartFailedCallback: (() => void) | null = null;
  private streamCheckTimer: ReturnType<typeof setInterval> | null = null;
  private isPlayerHealthy: (() => boolean) | null = null;

  // Stream restart backoff state
  private streamRestartTimer: ReturnType<typeof setTimeout> | null = null;
  private streamRestartDelay = 10_000; // Start at 10s
  private streamRestartAttempts = 0;
  private static readonly MAX_STREAM_RESTART_ATTEMPTS = 5;
  private static readonly STREAM_RESTART_DELAYS = [10_000, 30_000, 60_000, 60_000, 60_000];
  private lastStreamStartTime = 0;

  constructor(config: AppConfig) {
    this.config = config;

    this.obs.on('ConnectionClosed', () => {
      if (this.connected) {
        logger.warn('OBS WebSocket disconnected');
        this.connected = false;
        this.failedReconnects = 0;
        this.obsLaunched = false;
        this.clearStreamRestartTimer();
        this.onDisconnectCallback?.();
        this.scheduleReconnect();
      }
    });

    this.obs.on('ConnectionError', () => {
      // Handled by the connect() catch block — suppress duplicate logging
    });

    // Listen for stream state changes (encoder errors, network drops, etc.)
    this.obs.on('StreamStateChanged' as any, (event: { outputActive: boolean; outputState: string }) => {
      this.handleStreamStateChanged(event);
    });
  }

  async connect(): Promise<void> {
    if (!this.config.obsWebsocketUrl) {
      logger.info('OBS WebSocket URL not configured, skipping OBS integration');
      return;
    }

    try {
      await this.obs.connect(
        this.config.obsWebsocketUrl,
        this.config.obsWebsocketPassword || undefined,
      );
      this.connected = true;
      this.reconnectDelay = 5000;
      this.failedReconnects = 0;
      this.obsLaunched = false;
      logger.info('Connected to OBS WebSocket');
      this.onConnectCallback?.();
    } catch (err: any) {
      const msg = err?.message || String(err);
      if (msg.includes('ECONNREFUSED')) {
        logger.warn('OBS not reachable at %s - is OBS running?', this.config.obsWebsocketUrl);
      } else {
        logger.error({ err }, 'Failed to connect to OBS WebSocket');
      }
      this.failedReconnects++;
      this.tryLaunchObs();
      this.scheduleReconnect();
    }
  }

  private scheduleReconnect() {
    if (this.reconnectTimer) return;
    logger.info({ delayMs: this.reconnectDelay }, 'Scheduling OBS reconnect');
    this.reconnectTimer = setTimeout(async () => {
      this.reconnectTimer = null;
      await this.connect();
    }, this.reconnectDelay);
    this.reconnectDelay = Math.min(this.reconnectDelay * 1.5, 30000);
  }

  private tryLaunchObs() {
    if (!this.config.obsAutoRestart) return;
    if (this.obsLaunched) return;
    if (this.failedReconnects < 2) return; // Give OBS a chance to come back on its own

    const obsPath = this.resolveObsPath();
    if (!obsPath) {
      logger.warn('obsAutoRestart enabled but OBS executable not found. Set obsPath in config.');
      return;
    }

    // Check if OBS is still running before launching (use execFile to avoid shell injection)
    const exeName = basename(obsPath);
    execFile('tasklist', ['/FI', `IMAGENAME eq ${exeName}`, '/NH'], (err, stdout) => {
      if (err) {
        logger.error({ err }, 'Failed to check if OBS is running');
        return;
      }
      if (stdout.toLowerCase().includes(exeName.toLowerCase())) {
        logger.info('OBS process still running, waiting for it to exit');
        return;
      }
      // Clear OBS crash sentinel so it doesn't prompt for safe mode
      this.clearObsSentinel();

      logger.info({ obsPath }, 'Launching OBS');
      this.obsLaunched = true;
      try {
        const child = spawn(obsPath, ['--disable-shutdown-check'], {
          cwd: dirname(obsPath),
          detached: true,
          stdio: 'ignore',
          windowsHide: false,
        });
        child.unref();
        child.on('error', (launchErr) => {
          logger.error({ err: launchErr, obsPath }, 'Failed to launch OBS');
          this.obsLaunched = false;
        });
      } catch (launchErr) {
        logger.error({ err: launchErr, obsPath }, 'Failed to spawn OBS');
        this.obsLaunched = false;
      }
    });
  }

  private clearObsSentinel() {
    const appData = process.env.APPDATA;
    if (!appData) return;
    const sentinelPath = join(appData, 'obs-studio', '.sentinel');
    try {
      if (existsSync(sentinelPath)) {
        rmSync(sentinelPath, { recursive: true, force: true });
        logger.info('Cleared OBS crash sentinel');
      }
    } catch (err) {
      logger.warn({ err }, 'Failed to clear OBS crash sentinel');
    }
  }

  private resolveObsPath(): string | null {
    if (this.config.obsPath) {
      if (existsSync(this.config.obsPath)) return this.config.obsPath;
      logger.warn({ obsPath: this.config.obsPath }, 'Configured obsPath does not exist');
      return null;
    }
    for (const p of DEFAULT_OBS_PATHS) {
      if (existsSync(p)) return p;
    }
    return null;
  }

  onConnect(cb: () => void) { this.onConnectCallback = cb; }
  onDisconnect(cb: () => void) { this.onDisconnectCallback = cb; }
  onStreamDrop(cb: (attempt: number, maxAttempts: number) => void) { this.onStreamDropCallback = cb; }
  onStreamRestart(cb: (attempts: number) => void) { this.onStreamRestartCallback = cb; }
  onStreamRestartFailed(cb: () => void) { this.onStreamRestartFailedCallback = cb; }
  isConnected(): boolean { return this.connected; }

  private handleStreamStateChanged(event: { outputActive: boolean; outputState: string }) {
    logger.info({ outputState: event.outputState, outputActive: event.outputActive }, 'OBS stream state changed');

    if (event.outputState === 'OBS_WEBSOCKET_OUTPUT_STARTED') {
      this.lastStreamStartTime = Date.now();
      // Stream started successfully — reset backoff if this was a restart
      if (this.streamRestartAttempts > 0) {
        logger.info({ attempts: this.streamRestartAttempts }, 'Stream restarted successfully after drop');
        this.onStreamRestartCallback?.(this.streamRestartAttempts);
        this.dismissObsDialogs();
      }
      this.streamRestartAttempts = 0;
      this.clearStreamRestartTimer();
      return;
    }

    if (event.outputState === 'OBS_WEBSOCKET_OUTPUT_STOPPED') {
      if (!this.config.obsAutoStream) return;
      this.scheduleStreamRestart();
    }
  }

  private scheduleStreamRestart() {
    if (this.streamRestartTimer) return; // Already scheduled
    if (this.streamRestartAttempts >= OBSClient.MAX_STREAM_RESTART_ATTEMPTS) {
      logger.error({ attempts: this.streamRestartAttempts }, 'Max stream restart attempts reached, giving up');
      this.onStreamRestartFailedCallback?.();
      // Reset so recovery can kick in again if user manually restarts and it drops later
      this.streamRestartAttempts = 0;
      return;
    }

    const delay = OBSClient.STREAM_RESTART_DELAYS[
      Math.min(this.streamRestartAttempts, OBSClient.STREAM_RESTART_DELAYS.length - 1)
    ];
    this.streamRestartAttempts++;

    logger.warn(
      { attempt: this.streamRestartAttempts, delayMs: delay, max: OBSClient.MAX_STREAM_RESTART_ATTEMPTS },
      'Stream dropped, scheduling restart',
    );

    this.onStreamDropCallback?.(this.streamRestartAttempts, OBSClient.MAX_STREAM_RESTART_ATTEMPTS);

    this.streamRestartTimer = setTimeout(async () => {
      this.streamRestartTimer = null;
      if (!this.connected || !this.config.obsAutoStream) return;
      if (!this.isPlayerHealthy?.()) {
        logger.warn('Skipping stream restart: player not healthy');
        return;
      }

      try {
        const { outputActive } = await this.obs.call('GetStreamStatus');
        if (outputActive) {
          logger.info('Stream already active, no restart needed');
          this.streamRestartAttempts = 0;
          return;
        }
        logger.info({ attempt: this.streamRestartAttempts }, 'Attempting to restart stream');
        await this.obs.call('StartStream');
        // StreamStateChanged event will fire and handle success/failure
      } catch (err) {
        logger.error({ err, attempt: this.streamRestartAttempts }, 'Failed to restart stream');
        // Schedule another attempt if we haven't hit max
        this.scheduleStreamRestart();
      }
    }, delay);
  }

  private clearStreamRestartTimer() {
    if (this.streamRestartTimer) {
      clearTimeout(this.streamRestartTimer);
      this.streamRestartTimer = null;
    }
  }

  /**
   * Dismiss OBS error dialog popups by finding owned windows (dialogs)
   * belonging to the OBS process and sending WM_CLOSE.
   */
  private dismissObsDialogs() {
    // C# source for Win32 dialog closer — finds visible top-level windows
    // owned by another window (i.e. dialogs) in the target process
    const csharpSrc = [
      'using System;',
      'using System.Runtime.InteropServices;',
      'public class DialogCloser {',
      '  delegate bool EnumCb(IntPtr h, IntPtr l);',
      '  [DllImport("user32.dll")] static extern bool EnumWindows(EnumCb cb, IntPtr l);',
      '  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);',
      '  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);',
      '  [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr h, uint cmd);',
      '  [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, uint msg, IntPtr wp, IntPtr lp);',
      '  public static int Close(int pid) {',
      '    int n = 0;',
      '    EnumCb cb = (h, _) => {',
      '      uint p; GetWindowThreadProcessId(h, out p);',
      '      if ((int)p == pid && IsWindowVisible(h) && GetWindow(h, 4) != IntPtr.Zero) {',
      '        PostMessage(h, 0x0010, IntPtr.Zero, IntPtr.Zero);',
      '        n++;',
      '      }',
      '      return true;',
      '    };',
      '    EnumWindows(cb, IntPtr.Zero);',
      '    return n;',
      '  }',
      '}',
    ].join('\n');

    const psScript = [
      `Add-Type -TypeDefinition '${csharpSrc}'`,
      '$p = Get-Process obs64 -EA 0',
      'if (!$p) { $p = Get-Process obs32 -EA 0 }',
      'if ($p) { [DialogCloser]::Close($p.Id) }',
    ].join('\n');

    execFile('powershell', ['-NoProfile', '-NonInteractive', '-Command', psScript],
      { timeout: 15_000 },
      (err, stdout) => {
        if (err) {
          logger.warn({ err }, 'Failed to dismiss OBS dialogs');
        } else {
          const closed = parseInt(stdout.trim(), 10);
          if (closed > 0) {
            logger.info({ closed }, 'Dismissed OBS dialog(s)');
          }
        }
      },
    );
  }

  async isStreaming(): Promise<boolean> {
    if (!this.connected) return false;
    try {
      const { outputActive } = await this.obs.call('GetStreamStatus');
      return outputActive;
    } catch {
      return false;
    }
  }

  /** Refresh browser source by toggling its URL with a cache-busting param. */
  async refreshBrowserSource(): Promise<boolean> {
    if (!this.connected) return false;
    const name = this.config.obsBrowserSourceName;
    try {
      const { inputSettings } = await this.obs.call('GetInputSettings', { inputName: name });
      const url = (inputSettings as any).url as string;
      const separator = url.includes('?') ? '&' : '?';
      const bustUrl = url.replace(/[?&]_cb=\d+/, '') + separator + '_cb=' + Date.now();
      await this.obs.call('SetInputSettings', {
        inputName: name,
        inputSettings: { url: bustUrl },
      });
      logger.info('Refreshed browser source via URL cache-bust');
      return true;
    } catch (err) {
      logger.error({ err }, 'Failed to refresh browser source');
      return false;
    }
  }

  /** Toggle browser source visibility off then on. */
  async toggleBrowserSource(): Promise<boolean> {
    if (!this.connected) return false;
    const name = this.config.obsBrowserSourceName;
    try {
      // Find the scene item
      const { currentProgramSceneName } = await this.obs.call('GetCurrentProgramScene');
      const { sceneItems } = await this.obs.call('GetSceneItemList', {
        sceneName: currentProgramSceneName,
      });
      const item = sceneItems.find((i: any) => i.sourceName === name);
      if (!item) {
        logger.error({ name }, 'Browser source not found in current scene');
        return false;
      }
      const sceneItemId = item.sceneItemId as number;

      // Toggle off
      await this.obs.call('SetSceneItemEnabled', {
        sceneName: currentProgramSceneName,
        sceneItemId,
        sceneItemEnabled: false,
      });

      await new Promise((r) => setTimeout(r, 1000));

      // Toggle on
      await this.obs.call('SetSceneItemEnabled', {
        sceneName: currentProgramSceneName,
        sceneItemId,
        sceneItemEnabled: true,
      });

      logger.info('Toggled browser source visibility');
      return true;
    } catch (err) {
      logger.error({ err }, 'Failed to toggle browser source');
      return false;
    }
  }

  /** Validate OBS state and start streaming if everything looks good. */
  async startStreaming(): Promise<boolean> {
    if (!this.connected) {
      logger.warn('Cannot start stream: OBS not connected');
      return false;
    }

    try {
      // Check if already streaming
      const { outputActive } = await this.obs.call('GetStreamStatus');
      if (outputActive) {
        logger.info('OBS is already streaming, no action needed');
        return true;
      }

      // Verify the browser source exists in the current scene
      const { currentProgramSceneName } = await this.obs.call('GetCurrentProgramScene');
      const { sceneItems } = await this.obs.call('GetSceneItemList', {
        sceneName: currentProgramSceneName,
      });
      const source = sceneItems.find((i: any) => i.sourceName === this.config.obsBrowserSourceName);
      if (!source) {
        logger.warn({ source: this.config.obsBrowserSourceName, scene: currentProgramSceneName },
          'Cannot start stream: browser source not found in current scene');
        return false;
      }

      // Check the source is enabled
      const sceneItemId = source.sceneItemId as number;
      const { sceneItemEnabled } = await this.obs.call('GetSceneItemEnabled', {
        sceneName: currentProgramSceneName,
        sceneItemId,
      });
      if (!sceneItemEnabled) {
        logger.warn('Cannot start stream: browser source is disabled');
        return false;
      }

      // All checks passed — start streaming
      await this.obs.call('StartStream');
      logger.info('Auto-started OBS stream');
      return true;
    } catch (err) {
      logger.error({ err }, 'Failed to auto-start stream');
      return false;
    }
  }

  /** Start periodic stream health check. Restarts stream if it drops while everything else is healthy. */
  startStreamMonitor(isPlayerHealthy: () => boolean) {
    this.stopStreamMonitor();
    this.isPlayerHealthy = isPlayerHealthy;
    this.streamCheckTimer = setInterval(() => this.checkStreamHealth(), 30_000);
  }

  stopStreamMonitor() {
    if (this.streamCheckTimer) {
      clearInterval(this.streamCheckTimer);
      this.streamCheckTimer = null;
    }
  }

  private async checkStreamHealth() {
    if (!this.config.obsAutoStream) return;
    if (!this.connected) return;
    if (!this.isPlayerHealthy?.()) return;
    // Skip if event-driven restart is already in progress
    if (this.streamRestartTimer) return;

    try {
      const { outputActive } = await this.obs.call('GetStreamStatus');
      if (!outputActive) {
        logger.warn('Stream health check: not streaming, attempting to restart');
        await this.startStreaming();
      }
    } catch {
      // OBS call failed, skip this check
    }
  }

  async disconnect(): Promise<void> {
    this.stopStreamMonitor();
    this.clearStreamRestartTimer();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.connected) {
      this.connected = false; // Set before disconnect so ConnectionClosed handler doesn't fire callback
      await this.obs.disconnect();
    }
  }
}
