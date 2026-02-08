// --- Player → Server messages ---

export interface PlayerReadyMessage {
  type: 'ready';
}

export interface PlayerHeartbeatMessage {
  type: 'heartbeat';
  videoIndex: number;
  videoId: string;
  playerState: number; // YT.PlayerState
  currentTime: number;
}

export interface PlayerErrorMessage {
  type: 'error';
  errorCode: number;
  videoIndex: number;
  videoId: string;
}

export interface PlayerStateChangeMessage {
  type: 'stateChange';
  playerState: number;
  videoIndex: number;
  videoId: string;
}

export interface PlayerPlaylistLoadedMessage {
  type: 'playlistLoaded';
  totalVideos: number;
}

export type PlayerMessage =
  | PlayerReadyMessage
  | PlayerHeartbeatMessage
  | PlayerErrorMessage
  | PlayerStateChangeMessage
  | PlayerPlaylistLoadedMessage;

// --- Server → Player messages ---

export interface ServerLoadPlaylistMessage {
  type: 'loadPlaylist';
  playlistId: string;
  index: number;
}

export interface ServerRetryCurrentMessage {
  type: 'retryCurrent';
}

export interface ServerSkipMessage {
  type: 'skip';
  index: number;
}

export type ServerMessage =
  | ServerLoadPlaylistMessage
  | ServerRetryCurrentMessage
  | ServerSkipMessage;

// --- Persisted state ---

export interface PersistedState {
  playlistIndex: number;
  videoIndex: number;
  videoId: string;
  currentTime: number;
  updatedAt: string;
}

// --- Recovery ---

export enum RecoveryStep {
  None = 'none',
  RetryCurrent = 'retryCurrent',
  RefreshSource = 'refreshSource',
  ToggleVisibility = 'toggleVisibility',
  CriticalAlert = 'criticalAlert',
}

// --- Config ---

export interface PlaylistEntry {
  id: string;
  name?: string;
}

export interface AppConfig {
  port: number;
  obsWebsocketUrl: string;
  obsWebsocketPassword: string;
  obsBrowserSourceName: string;
  playlists: PlaylistEntry[];
  discordWebhookUrl: string;
  heartbeatIntervalMs: number;
  heartbeatTimeoutMs: number;
  maxConsecutiveErrors: number;
  stateFilePath: string;
  recoveryDelayMs: number;
  autoUpdateCheck: boolean;
  updateCheckIntervalMs: number;
}
