import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdirSync, rmSync, writeFileSync } from 'fs';
import { join, resolve } from 'path';
import {
  readUpdateState,
  writeUpdateState,
  setStep,
  clearUpdateState,
  readSwapLog,
  type UpdateState,
} from '../update-state.js';

const ROOT = resolve('.test-tmp', 'update-state');

function sample(): UpdateState {
  return {
    from: '3.0.5',
    to: '3.0.6',
    parts: ['node', 'ffmpeg'],
    folders: ['node', 'ffmpeg', 'yt-dlp'],
    step: 'download',
    result: 'running',
    reason: null,
    startedAt: '2026-11-08T10:00:00.000Z',
    stepStartedAt: '2026-11-08T10:00:00.000Z',
    baseline: { encoder: 'h264_nvenc', works: true },
  };
}

beforeEach(() => {
  rmSync(ROOT, { recursive: true, force: true });
  mkdirSync(ROOT, { recursive: true });
});
afterEach(() => rmSync(ROOT, { recursive: true, force: true }));

describe('update state', () => {
  it('round-trips a state', () => {
    // Arrange
    const s = sample();

    // Act
    writeUpdateState(ROOT, s);

    // Assert
    expect(readUpdateState(ROOT)).toEqual(s);
  });

  it('returns null for a missing or corrupt file', () => {
    // Arrange
    expect(readUpdateState(ROOT)).toBeNull();
    mkdirSync(join(ROOT, 'data'), { recursive: true });

    // Act
    writeFileSync(join(ROOT, 'data', 'update-state.json'), '{ not json');
    const corrupt = readUpdateState(ROOT);
    writeFileSync(join(ROOT, 'data', 'update-state.json'), JSON.stringify({ from: '1' }));
    const wrongShape = readUpdateState(ROOT);

    // Assert
    expect(corrupt).toBeNull();
    expect(wrongShape).toBeNull();
  });

  it('setStep moves the step and its start time', () => {
    // Arrange
    writeUpdateState(ROOT, sample());

    // Act
    const next = setStep(ROOT, 'swap', { reason: 'x' });

    // Assert
    expect(next?.step).toBe('swap');
    expect(next?.reason).toBe('x');
    expect(next?.stepStartedAt).not.toBe('2026-11-08T10:00:00.000Z');
    expect(readUpdateState(ROOT)).toEqual(next);
    clearUpdateState(ROOT);
    expect(readUpdateState(ROOT)).toBeNull();
    expect(setStep(ROOT, 'done')).toBeNull();
  });

  it('readSwapLog parses each status and skips unknown lines', () => {
    // Arrange
    mkdirSync(join(ROOT, 'data'), { recursive: true });
    expect(readSwapLog(ROOT)).toEqual([]);
    writeFileSync(
      join(ROOT, 'data', 'update-swap.log'),
      'node retry rename blocked\r\n\r\nffmpeg killed pid 12\r\nsupervisor ok\r\npot failed no access\r\napp restored from _update_old\r\nnode weird thing\r\ngarbage\r\n',
    );

    // Act
    const lines = readSwapLog(ROOT);

    // Assert
    expect(lines).toEqual([
      { folder: 'node', status: 'retry', detail: 'rename blocked' },
      { folder: 'ffmpeg', status: 'killed', detail: 'pid 12' },
      { folder: 'supervisor', status: 'ok', detail: '' },
      { folder: 'pot', status: 'failed', detail: 'no access' },
      { folder: 'app', status: 'restored', detail: 'from _update_old' },
    ]);
  });
});
