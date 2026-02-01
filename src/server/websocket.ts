import { WebSocketServer, WebSocket } from 'ws';
import type { Server } from 'http';
import type { PlayerMessage, ServerMessage } from './types.js';
import { logger } from './logger.js';

export class PlayerWebSocket {
  private wss: WebSocketServer;
  private client: WebSocket | null = null;
  private onMessageCallback: ((msg: PlayerMessage) => void) | null = null;
  private onConnectCallback: (() => void) | null = null;
  private onDisconnectCallback: (() => void) | null = null;

  constructor(server: Server) {
    this.wss = new WebSocketServer({ server, path: '/ws' });

    this.wss.on('connection', (ws) => {
      logger.info('Player WebSocket connected');
      this.client = ws;
      this.onConnectCallback?.();

      ws.on('message', (data) => {
        try {
          const msg = JSON.parse(data.toString()) as PlayerMessage;
          this.onMessageCallback?.(msg);
        } catch (err) {
          logger.error({ err }, 'Invalid WebSocket message from player');
        }
      });

      ws.on('close', () => {
        logger.warn('Player WebSocket disconnected');
        if (this.client === ws) this.client = null;
        this.onDisconnectCallback?.();
      });

      ws.on('error', (err) => {
        logger.error({ err }, 'Player WebSocket error');
      });
    });
  }

  onMessage(cb: (msg: PlayerMessage) => void) {
    this.onMessageCallback = cb;
  }

  onConnect(cb: () => void) {
    this.onConnectCallback = cb;
  }

  onDisconnect(cb: () => void) {
    this.onDisconnectCallback = cb;
  }

  send(msg: ServerMessage) {
    if (this.client?.readyState === WebSocket.OPEN) {
      this.client.send(JSON.stringify(msg));
    } else {
      logger.warn({ msg }, 'Cannot send to player: not connected');
    }
  }

  isConnected(): boolean {
    return this.client?.readyState === WebSocket.OPEN;
  }

  close() {
    this.wss.close();
  }
}
