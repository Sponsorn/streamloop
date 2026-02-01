import OBSWebSocket from 'obs-websocket-js';
import type { AppConfig } from './types.js';
import { logger } from './logger.js';

export class OBSClient {
  private obs = new OBSWebSocket();
  private config: AppConfig;
  private connected = false;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelay = 10000;
  private onConnectCallback: (() => void) | null = null;
  private onDisconnectCallback: (() => void) | null = null;

  constructor(config: AppConfig) {
    this.config = config;

    this.obs.on('ConnectionClosed', () => {
      if (this.connected) {
        logger.warn('OBS WebSocket disconnected');
        this.connected = false;
        this.onDisconnectCallback?.();
        this.scheduleReconnect();
      }
    });

    this.obs.on('ConnectionError', (err) => {
      logger.error({ err }, 'OBS WebSocket connection error');
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
      this.reconnectDelay = 10000;
      logger.info('Connected to OBS WebSocket');
      this.onConnectCallback?.();
    } catch (err) {
      logger.error({ err }, 'Failed to connect to OBS WebSocket');
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
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, 60000);
  }

  onConnect(cb: () => void) { this.onConnectCallback = cb; }
  onDisconnect(cb: () => void) { this.onDisconnectCallback = cb; }
  isConnected(): boolean { return this.connected; }

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

  async disconnect(): Promise<void> {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.connected) {
      await this.obs.disconnect();
      this.connected = false;
    }
  }
}
