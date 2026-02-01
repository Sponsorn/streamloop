import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { writeFileSync, unlinkSync, mkdirSync, readFileSync } from 'fs';
import { join } from 'path';
import { StateManager } from '../state.js';

const tmpDir = join(import.meta.dirname, '..', '..', '..', '.test-tmp');
const statePath = join(tmpDir, 'state.json');

beforeEach(() => {
  mkdirSync(tmpDir, { recursive: true });
  try { unlinkSync(statePath); } catch {}
  try { unlinkSync(statePath + '.tmp'); } catch {}
});

afterEach(() => {
  try { unlinkSync(statePath); } catch {}
  try { unlinkSync(statePath + '.tmp'); } catch {}
});

describe('StateManager', () => {
  it('starts with defaults when no file exists', () => {
    const sm = new StateManager(statePath);
    const s = sm.get();
    expect(s.playlistIndex).toBe(0);
    expect(s.videoIndex).toBe(0);
    expect(s.videoId).toBe('');
    expect(s.currentTime).toBe(0);
  });

  it('loads existing state from disk', () => {
    const saved = {
      playlistIndex: 2,
      videoIndex: 5,
      videoId: 'abc123',
      currentTime: 42.5,
      updatedAt: '2024-01-01T00:00:00Z',
    };
    writeFileSync(statePath, JSON.stringify(saved));
    const sm = new StateManager(statePath);
    const s = sm.get();
    expect(s.playlistIndex).toBe(2);
    expect(s.videoIndex).toBe(5);
    expect(s.videoId).toBe('abc123');
    expect(s.currentTime).toBe(42.5);
  });

  it('defaults playlistIndex to 0 for old state files without it', () => {
    const saved = {
      videoIndex: 3,
      videoId: 'old123',
      currentTime: 10,
      updatedAt: '2024-01-01T00:00:00Z',
    };
    writeFileSync(statePath, JSON.stringify(saved));
    const sm = new StateManager(statePath);
    const s = sm.get();
    expect(s.playlistIndex).toBe(0);
    expect(s.videoIndex).toBe(3);
  });

  it('updates and flushes to disk', () => {
    const sm = new StateManager(statePath);
    sm.update({ videoIndex: 10, videoId: 'xyz' });
    sm.flush();

    const raw = JSON.parse(readFileSync(statePath, 'utf-8'));
    expect(raw.videoIndex).toBe(10);
    expect(raw.videoId).toBe('xyz');
    expect(raw.updatedAt).toBeTruthy();
  });

  it('persists playlistIndex to disk', () => {
    const sm = new StateManager(statePath);
    sm.update({ playlistIndex: 2, videoIndex: 0 });
    sm.flush();

    const raw = JSON.parse(readFileSync(statePath, 'utf-8'));
    expect(raw.playlistIndex).toBe(2);
  });

  it('returns a copy from get(), not a reference', () => {
    const sm = new StateManager(statePath);
    const a = sm.get();
    a.videoIndex = 999;
    const b = sm.get();
    expect(b.videoIndex).toBe(0);
  });
});
