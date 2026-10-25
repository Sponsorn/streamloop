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
  streamDrop: boolean;
  streamRestart: boolean;
  outputFrozen: boolean;
  twitchMismatch: boolean;
  twitchRestart: boolean;
}

export interface DiscordTemplates {
  error: string;
  skip: string;
  recovery: string;
  critical: string;
  resume: string;
  streamDrop: string;
  streamRestart: string;
  outputFrozen: string;
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

export type SupervisorEncoder = 'h264_nvenc' | 'h264_amf' | 'h264_qsv' | 'libx264';

/** The `supervisor` block of config.json; the Rust supervisor reads the same keys. */
export interface SupervisorConfig {
  ingestUrl: string;
  /** DPAPI blob; the plain stream key is never stored. */
  streamKeyProtected: string;
  encoder: SupervisorEncoder;
  /** What the user chose; encoder may hold a fallback until this works again. */
  preferredEncoder?: SupervisorEncoder;
  cacheBudgetMb: number;
  readAheadSeconds: number;
  playerClient: string;
  overlay: boolean;
  freezeSeconds: number;
  silenceSeconds: number;
}

export interface AppConfig {
  port: number;
  playlists: PlaylistEntry[];
  discord: DiscordConfig;
  supervisor: SupervisorConfig;
  autoUpdateCheck: boolean;
  updateCheckIntervalMs: number;
  autoUpdateYtdlp: boolean;
  autoOpenAdmin: boolean;
  twitchClientId: string;
  twitchClientSecret: string;
  twitchChannel: string;
  twitchLivenessEnabled: boolean;
  twitchPollIntervalMs: number;
  ytdlCookiesFromBrowser: string;
}

// --- Event log ---

export interface EventLogEntry {
  timestamp: string;
  message: string;
}
