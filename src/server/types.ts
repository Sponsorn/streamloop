// --- Player → Server messages ---

export interface PlayerReadyMessage {
  type: 'ready';
}

export interface PlayerHeartbeatMessage {
  type: 'heartbeat';
  videoIndex: number;
  videoId: string;
  videoTitle: string;
  playerState: number; // YT.PlayerState
  currentTime: number;
  videoDuration: number;
  nextVideoId: string;
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
  videoTitle: string;
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
  loop: boolean;
  startTime?: number;
}

export interface ServerRetryCurrentMessage {
  type: 'retryCurrent';
}

export interface ServerResumeMessage {
  type: 'resume';
}

export interface ServerSkipMessage {
  type: 'skip';
  index: number;
}

export type ServerMessage =
  | ServerLoadPlaylistMessage
  | ServerRetryCurrentMessage
  | ServerResumeMessage
  | ServerSkipMessage;

// --- Persisted state ---

export interface PersistedState {
  playlistIndex: number;
  videoIndex: number;
  videoId: string;
  videoTitle: string;
  currentTime: number;
  videoDuration: number;
  nextVideoId: string;
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

export interface DiscordEventToggles {
  error: boolean;
  skip: boolean;
  recovery: boolean;
  critical: boolean;
  resume: boolean;
  obsDisconnect: boolean;
  obsReconnect: boolean;
}

export interface DiscordTemplates {
  error: string;
  skip: string;
  recovery: string;
  critical: string;
  resume: string;
  obsDisconnect: string;
  obsReconnect: string;
}

export interface DiscordConfig {
  webhookUrl: string;
  botName: string;
  avatarUrl: string;
  rolePing: string;
  events: DiscordEventToggles;
  templates: DiscordTemplates;
}

export interface AppConfig {
  port: number;
  obsWebsocketUrl: string;
  obsWebsocketPassword: string;
  obsBrowserSourceName: string;
  playlists: PlaylistEntry[];
  discord: DiscordConfig;
  heartbeatIntervalMs: number;
  heartbeatTimeoutMs: number;
  maxConsecutiveErrors: number;
  stateFilePath: string;
  recoveryDelayMs: number;
  obsAutoRestart: boolean;
  obsAutoStream: boolean;
  obsPath: string;
  autoUpdateCheck: boolean;
  updateCheckIntervalMs: number;
}
