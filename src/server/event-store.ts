import { appendFile } from 'fs/promises';
import { mkdirSync, readFileSync, existsSync, readdirSync, statSync, unlinkSync } from 'fs';
import { join } from 'path';
import type { EventLogEntry } from './types.js';
import { logger } from './logger.js';

export interface EventStoreOptions {
  dir: string;
  retentionDays?: number;
  filenamePrefix?: string;
}

/** Persists the curated dashboard event log to daily-rotated JSONL files. */
export class EventStore {
  private readonly dir: string;
  private readonly retentionDays: number;
  private readonly prefix: string;
  /** Serializes appends so rapid successive events keep their order on disk. */
  private writeChain: Promise<void> = Promise.resolve();

  constructor(opts: EventStoreOptions) {
    this.dir = opts.dir;
    this.retentionDays = opts.retentionDays ?? 7;
    this.prefix = opts.filenamePrefix ?? 'events-';
    mkdirSync(this.dir, { recursive: true });
    this.cleanupOldFiles();
  }

  private dateString(d: Date): string {
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  }

  private fileFor(date: string): string {
    return join(this.dir, `${this.prefix}${date}.jsonl`);
  }

  /** Append one event. Fire-and-forget; failures are logged, never thrown.
   *  Writes are chained so concurrent calls are written in call order
   *  (independent appendFile calls can otherwise interleave/reorder). */
  append(entry: EventLogEntry): void {
    const line = JSON.stringify(entry) + '\n';
    const path = this.fileFor(this.dateString(new Date()));
    this.writeChain = this.writeChain
      .then(() => appendFile(path, line, 'utf-8'))
      .catch((err) => logger.warn({ err }, 'Failed to persist event'));
  }

  /** Read up to `limit` most recent events (today + yesterday), oldest-first. */
  loadRecent(limit: number): EventLogEntry[] {
    const now = new Date();
    const yesterday = new Date(now.getTime() - 24 * 60 * 60 * 1000);
    const entries: EventLogEntry[] = [];
    for (const date of [this.dateString(yesterday), this.dateString(now)]) {
      const path = this.fileFor(date);
      if (!existsSync(path)) continue;
      try {
        for (const raw of readFileSync(path, 'utf-8').split('\n')) {
          if (!raw) continue;
          try { entries.push(JSON.parse(raw) as EventLogEntry); } catch { /* skip corrupt line */ }
        }
      } catch (err) {
        logger.warn({ err, path }, 'Failed to read event file');
      }
    }
    return entries.slice(-limit);
  }

  private cleanupOldFiles(): void {
    const cutoff = Date.now() - this.retentionDays * 24 * 60 * 60 * 1000;
    let files: string[];
    try {
      files = readdirSync(this.dir);
    } catch {
      return;
    }
    for (const file of files) {
      if (!file.startsWith(this.prefix) || !file.endsWith('.jsonl')) continue;
      const path = join(this.dir, file);
      try {
        if (statSync(path).mtimeMs < cutoff) unlinkSync(path);
      } catch { /* ignore individual file errors */ }
    }
  }
}
