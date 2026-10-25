import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { mkdirSync, writeFileSync, rmSync, existsSync, utimesSync } from 'fs';
import { join } from 'path';
import { EventStore } from '../event-store.js';

const tmpDir = join(import.meta.dirname, '..', '..', '..', '.test-tmp', 'events');

function todayString(): string {
  const n = new Date();
  return `${n.getFullYear()}-${String(n.getMonth() + 1).padStart(2, '0')}-${String(n.getDate()).padStart(2, '0')}`;
}

describe('EventStore', () => {
  beforeEach(() => {
    rmSync(tmpDir, { recursive: true, force: true });
    mkdirSync(tmpDir, { recursive: true });
  });
  afterEach(() => {
    rmSync(tmpDir, { recursive: true, force: true });
  });

  it('appends entries that loadRecent reads back oldest-first', async () => {
    const store = new EventStore({ dir: tmpDir });
    store.append({ timestamp: '2026-06-09T00:00:01Z', message: 'first' });
    store.append({ timestamp: '2026-06-09T00:00:02Z', message: 'second' });
    // append is fire-and-forget, so poll until both writes land
    await vi.waitFor(() => {
      expect(store.loadRecent(10).map((e) => e.message)).toEqual(['first', 'second']);
    }, { timeout: 2000 });
  });

  it('writes to a date-named .jsonl file', async () => {
    const store = new EventStore({ dir: tmpDir });
    store.append({ timestamp: 't', message: 'x' });
    await vi.waitFor(() => {
      expect(existsSync(join(tmpDir, `events-${todayString()}.jsonl`))).toBe(true);
    }, { timeout: 2000 });
  });

  it('loadRecent returns at most `limit`, the most recent ones', async () => {
    const store = new EventStore({ dir: tmpDir });
    for (let i = 1; i <= 5; i++) store.append({ timestamp: `t${i}`, message: `m${i}` });
    await vi.waitFor(() => {
      expect(store.loadRecent(2).map((e) => e.message)).toEqual(['m4', 'm5']);
    }, { timeout: 2000 });
  });

  it('cleans up files older than retentionDays on construction', () => {
    const oldFile = join(tmpDir, 'events-2000-01-01.jsonl');
    writeFileSync(oldFile, '{"timestamp":"t","message":"old"}\n');
    const old = Date.now() - 30 * 24 * 60 * 60 * 1000;
    utimesSync(oldFile, old / 1000, old / 1000);
    new EventStore({ dir: tmpDir, retentionDays: 7 });
    expect(existsSync(oldFile)).toBe(false);
  });

  it('loadRecent returns an empty array when no files exist', () => {
    const store = new EventStore({ dir: tmpDir });
    expect(store.loadRecent(10)).toEqual([]);
  });
});
