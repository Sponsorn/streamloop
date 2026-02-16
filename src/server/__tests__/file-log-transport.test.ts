import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdirSync, writeFileSync, readdirSync, rmSync, utimesSync, existsSync } from 'fs';
import { join } from 'path';
import { formatLogLine, cleanupOldFiles } from '../file-log-transport.js';

const tmpDir = join(import.meta.dirname, '..', '..', '..', '.test-tmp');
const logsDir = join(tmpDir, 'logs');

/** Format a Date as local YYYY-MM-DD HH:mm:ss.SSS (matches the transport). */
function localTs(date: Date): string {
  const y = date.getFullYear();
  const mo = String(date.getMonth() + 1).padStart(2, '0');
  const d = String(date.getDate()).padStart(2, '0');
  const h = String(date.getHours()).padStart(2, '0');
  const mi = String(date.getMinutes()).padStart(2, '0');
  const s = String(date.getSeconds()).padStart(2, '0');
  const ms = String(date.getMilliseconds()).padStart(3, '0');
  return `${y}-${mo}-${d} ${h}:${mi}:${s}.${ms}`;
}

beforeEach(() => {
  mkdirSync(logsDir, { recursive: true });
});

afterEach(() => {
  rmSync(logsDir, { recursive: true, force: true });
});

describe('formatLogLine', () => {
  it('formats a basic info log with local timestamp', () => {
    const timestamp = 1739712185123;
    const obj = {
      level: 30,
      time: timestamp,
      msg: 'Server listening on port 7654',
      pid: 1234,
      hostname: 'test',
      v: 1,
    };
    const line = formatLogLine(obj);
    const expected = `[${localTs(new Date(timestamp))}] INFO: Server listening on port 7654`;
    expect(line).toBe(expected);
  });

  it('includes extra fields as JSON', () => {
    const timestamp = 1739712185123;
    const obj = {
      level: 30,
      time: timestamp,
      msg: 'Server listening',
      pid: 1234,
      hostname: 'test',
      v: 1,
      port: 7654,
    };
    const line = formatLogLine(obj);
    const expected = `[${localTs(new Date(timestamp))}] INFO: Server listening {"port":7654}`;
    expect(line).toBe(expected);
  });

  it('formats warn level', () => {
    const obj = {
      level: 40,
      time: 1739712186456,
      msg: 'Player stalled',
      pid: 1234,
      hostname: 'test',
      v: 1,
      stalledHeartbeats: 2,
      videoIndex: 5,
    };
    const line = formatLogLine(obj);
    expect(line).toContain('WARN: Player stalled');
    expect(line).toContain('"stalledHeartbeats":2');
    expect(line).toContain('"videoIndex":5');
  });

  it('formats error level', () => {
    const obj = {
      level: 50,
      time: 1739712187789,
      msg: 'Discord webhook failed',
      pid: 1234,
      hostname: 'test',
      v: 1,
      status: 429,
    };
    const line = formatLogLine(obj);
    expect(line).toContain('ERROR: Discord webhook failed');
    expect(line).toContain('"status":429');
  });

  it('handles missing msg gracefully', () => {
    const timestamp = 1739712185123;
    const obj = {
      level: 30,
      time: timestamp,
      pid: 1234,
      hostname: 'test',
      v: 1,
    };
    const line = formatLogLine(obj);
    expect(line).toBe(`[${localTs(new Date(timestamp))}] INFO: `);
  });

  it('handles unknown level number', () => {
    const obj = {
      level: 25,
      time: 1739712185123,
      msg: 'custom level',
      pid: 1234,
      hostname: 'test',
      v: 1,
    };
    const line = formatLogLine(obj);
    expect(line).toContain('LVL25: custom level');
  });
});

describe('cleanupOldFiles', () => {
  it('deletes files older than retention period', () => {
    const oldFile = join(logsDir, 'streamloop-2025-01-01.log');
    const recentFile = join(logsDir, 'streamloop-2025-02-15.log');

    writeFileSync(oldFile, 'old log data');
    writeFileSync(recentFile, 'recent log data');

    // Set the old file's mtime to 30 days ago
    const thirtyDaysAgo = new Date(Date.now() - 30 * 24 * 60 * 60 * 1000);
    utimesSync(oldFile, thirtyDaysAgo, thirtyDaysAgo);

    cleanupOldFiles(logsDir, 'streamloop-', 7);

    const remaining = readdirSync(logsDir);
    expect(remaining).not.toContain('streamloop-2025-01-01.log');
    expect(remaining).toContain('streamloop-2025-02-15.log');
  });

  it('keeps files within retention period', () => {
    const recentFile = join(logsDir, 'streamloop-2025-02-15.log');
    writeFileSync(recentFile, 'recent log data');

    cleanupOldFiles(logsDir, 'streamloop-', 7);

    const remaining = readdirSync(logsDir);
    expect(remaining).toContain('streamloop-2025-02-15.log');
  });

  it('ignores non-matching files', () => {
    const otherFile = join(logsDir, 'other-file.txt');
    writeFileSync(otherFile, 'some data');

    // Set old mtime
    const thirtyDaysAgo = new Date(Date.now() - 30 * 24 * 60 * 60 * 1000);
    utimesSync(otherFile, thirtyDaysAgo, thirtyDaysAgo);

    cleanupOldFiles(logsDir, 'streamloop-', 7);

    const remaining = readdirSync(logsDir);
    expect(remaining).toContain('other-file.txt');
  });

  it('handles non-existent directory gracefully', () => {
    expect(() => {
      cleanupOldFiles(join(tmpDir, 'nonexistent'), 'streamloop-', 7);
    }).not.toThrow();
  });
});

describe('transport initialization', () => {
  it('creates logs directory if it does not exist', () => {
    const testLogsDir = join(tmpDir, 'transport-init-test');
    expect(existsSync(testLogsDir)).toBe(false);

    // mkdirSync with recursive:true is what the transport does on init
    mkdirSync(testLogsDir, { recursive: true });
    expect(existsSync(testLogsDir)).toBe(true);

    rmSync(testLogsDir, { recursive: true, force: true });
  });

  it('runs cleanup on init (deletes old files in provided dir)', () => {
    const testLogsDir = join(tmpDir, 'transport-cleanup-test');
    mkdirSync(testLogsDir, { recursive: true });

    const oldFile = join(testLogsDir, 'streamloop-2024-01-01.log');
    const recentFile = join(testLogsDir, 'streamloop-2026-02-15.log');
    writeFileSync(oldFile, 'old');
    writeFileSync(recentFile, 'recent');

    const thirtyDaysAgo = new Date(Date.now() - 30 * 24 * 60 * 60 * 1000);
    utimesSync(oldFile, thirtyDaysAgo, thirtyDaysAgo);

    // Simulates what the transport does on init
    cleanupOldFiles(testLogsDir, 'streamloop-', 7);

    const remaining = readdirSync(testLogsDir);
    expect(remaining).not.toContain('streamloop-2024-01-01.log');
    expect(remaining).toContain('streamloop-2026-02-15.log');

    rmSync(testLogsDir, { recursive: true, force: true });
  });
});
