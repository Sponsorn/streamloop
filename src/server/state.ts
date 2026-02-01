import { readFileSync, writeFileSync, renameSync } from 'fs';
import { resolve } from 'path';
import type { PersistedState } from './types.js';
import { logger } from './logger.js';

const DEBOUNCE_MS = 2000;

export class StateManager {
  private filePath: string;
  private current: PersistedState;
  private debounceTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(filePath: string) {
    this.filePath = resolve(filePath);
    this.current = this.load();
  }

  private load(): PersistedState {
    try {
      const raw = readFileSync(this.filePath, 'utf-8');
      const parsed = JSON.parse(raw) as PersistedState;
      // Backward compat: old state files may lack playlistIndex
      if (parsed.playlistIndex === undefined) {
        parsed.playlistIndex = 0;
      }
      logger.info({ state: parsed }, 'Loaded persisted state');
      return parsed;
    } catch {
      logger.info('No existing state file, starting fresh');
      return { playlistIndex: 0, videoIndex: 0, videoId: '', currentTime: 0, updatedAt: new Date().toISOString() };
    }
  }

  get(): PersistedState {
    return { ...this.current };
  }

  update(partial: Partial<Omit<PersistedState, 'updatedAt'>>) {
    Object.assign(this.current, partial, { updatedAt: new Date().toISOString() });
    this.debouncedWrite();
  }

  /** Write immediately (used on shutdown). */
  flush() {
    if (this.debounceTimer) {
      clearTimeout(this.debounceTimer);
      this.debounceTimer = null;
    }
    this.writeToDisk();
  }

  private debouncedWrite() {
    if (this.debounceTimer) clearTimeout(this.debounceTimer);
    this.debounceTimer = setTimeout(() => {
      this.writeToDisk();
      this.debounceTimer = null;
    }, DEBOUNCE_MS);
  }

  private writeToDisk() {
    const tmpPath = this.filePath + '.tmp';
    try {
      writeFileSync(tmpPath, JSON.stringify(this.current, null, 2), 'utf-8');
      renameSync(tmpPath, this.filePath);
      logger.debug({ state: this.current }, 'State saved');
    } catch (err) {
      logger.error({ err }, 'Failed to write state file');
    }
  }
}
