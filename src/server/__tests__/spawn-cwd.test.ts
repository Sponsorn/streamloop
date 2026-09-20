import { describe, it, expect } from 'vitest';
import { readdirSync, readFileSync } from 'fs';
import { resolve, dirname, sep } from 'path';
import { fileURLToPath } from 'url';
import { SPAWN_CWD } from '../spawn-cwd.js';

const serverDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');

describe('child process working directory', () => {
  it('is outside the app directory, which the updater has to rename', () => {
    // Arrange
    const appDir = resolve(serverDir, '..', '..');

    // Act + Assert
    expect(resolve(SPAWN_CWD).toLowerCase().startsWith((appDir + sep).toLowerCase())).toBe(false);
    expect(resolve(SPAWN_CWD).toLowerCase()).not.toBe(appDir.toLowerCase());
  });

  it('is set on every process the server starts', () => {
    // Arrange: a child inherits cwd=app\ unless told otherwise, and any child that
    // outlives the server (browser, yt-dlp) then blocks the update's rename of app\.
    const sources = readdirSync(serverDir)
      .filter((name) => name.endsWith('.ts'))
      .map((name) => ({ name, text: readFileSync(resolve(serverDir, name), 'utf-8') }))
      .filter(({ text }) => text.includes("from 'child_process'"));

    // Act
    const missing = sources.flatMap(({ name, text }) => {
      const calls = text.match(/(?<![.\w])(spawn|execFile|exec)\(/g)?.length ?? 0;
      const cwds = text.match(/\bcwd\b\s*[:,}]/g)?.length ?? 0;
      return cwds < calls ? [`${name}: ${calls} process calls, ${cwds} with cwd`] : [];
    });

    // Assert
    expect(sources.length).toBeGreaterThan(0);
    expect(missing).toEqual([]);
  });
});
