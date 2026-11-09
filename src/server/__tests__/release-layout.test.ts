import { describe, it, expect } from 'vitest';
import { readFileSync } from 'fs';
import { resolve, dirname } from 'path';
import { fileURLToPath, pathToFileURL } from 'url';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
// build/ is plain JS outside the TypeScript project, so it is loaded by URL
const { FULL_LAYOUT, UPDATE_LAYOUT, missingPaths } = await import(pathToFileURL(resolve(repo, 'build', 'release-layout.js')).href);

describe('release layout', () => {
  it('ships every path the supervisor resolves in the full ZIP', () => {
    // Arrange
    const pathsRs = readFileSync(resolve(repo, 'crates', 'common', 'src', 'paths.rs'), 'utf-8');
    const supervisorPaths = [...pathsRs.matchAll(/\bat\("([^"]+)"\)/g)].map((m) => m[1]);

    // Act
    // Node creates app/config.json from the shipped example on first start
    // The 3.0 release ships none of the R1 paths; Task 21 replaces this guard with the R1 layout.
    const r1Only = [
      'streamloop.exe', 'streamloop.exe.old', 'streamloop.exe.bad', '_update', '_update/streamloop.exe', '_update/landed',
      'app', 'app/logs', 'data/stopped-by-user', 'data/update-rolled-back', 'data/setup-check.json',
      'data/waiting-for-start', 'data/update-state.json',
    ];
    const absent = supervisorPaths.filter((p) => p !== 'app/config.json' && !r1Only.includes(p) && !FULL_LAYOUT.includes(p));

    // Assert
    expect(supervisorPaths.length).toBeGreaterThan(5);
    expect(absent).toEqual([]);
  });

  it('never ships a config.json that would overwrite the installed one', () => {
    // Assert
    expect(FULL_LAYOUT).not.toContain('app/config.json');
    expect(UPDATE_LAYOUT).not.toContain('app/config.json');
    expect(FULL_LAYOUT).toContain('app/config.example.json');
  });

  it('ships the tsx CLI that START.bat runs the server with', () => {
    // Assert
    expect(FULL_LAYOUT).toContain('app/node_modules/tsx/dist/cli.mjs');
    expect(UPDATE_LAYOUT).toContain('app/node_modules/tsx/dist/cli.mjs');
  });

  it('keeps the update ZIP to app and the launcher', () => {
    // Act
    const tools = UPDATE_LAYOUT.filter((p: string) => /^(node|ffmpeg|pot|supervisor|yt-dlp)\//.test(p));

    // Assert
    expect(tools).toEqual([]);
    expect(UPDATE_LAYOUT).toEqual(expect.arrayContaining(['START.bat', 'app/package.json', 'app/node-version.txt']));
  });

  it('reports a path missing from the ZIP listing and accepts files and folders', () => {
    // Arrange
    const entries = ['streamloop/', 'streamloop/data/', 'streamloop/pot/node_modules/canvas/index.js', 'streamloop/START.bat'];

    // Act
    const missing = missingPaths(entries, ['data', 'pot/node_modules', 'START.bat', 'node/node.exe']);

    // Assert
    expect(missing).toEqual(['node/node.exe']);
  });
});
