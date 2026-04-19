// --- mpv IPC types ---

export interface MpvHeartbeat {
  timePos: number;
  duration: number;
  paused: boolean;
  idle: boolean;
  playlistPos: number;
  playlistCount: number;
  mediaTitle: string;
  filename: string;
  hasVideo: boolean;
  vfps: number;
}

export interface MpvPlaylistEntry {
  index: number;
  id: string;
  title: string;
  duration: number;
  current?: boolean;
}

// --- Recovery ---

export enum RecoveryStep {
  None = 'none',
  RetryCurrent = 'retryCurrent',
  RestartMpv = 'restartMpv',
  CriticalAlert = 'criticalAlert',
}

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
  streamDrop: boolean;
  streamRestart: boolean;
  twitchMismatch: boolean;
  twitchRestart: boolean;
}

export interface DiscordTemplates {
  error: string;
  skip: string;
  recovery: string;
  critical: string;
  resume: string;
  obsDisconnect: string;
  obsReconnect: string;
  streamDrop: string;
  streamRestart: string;
  twitchMismatch: string;
  twitchRestart: string;
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
  qualityRecoveryEnabled: boolean;
  minQuality: string;
  qualityRecoveryDelayMs: number;
  sourceRefreshIntervalMs: number;
  twitchClientId: string;
  twitchClientSecret: string;
  twitchChannel: string;
  twitchLivenessEnabled: boolean;
  twitchPollIntervalMs: number;
  mpvGeometry: string;
  mpvYtdlFormat: string;
  mpvExtraArgs: string[];
  ytdlCookiesFromBrowser: string;
}
