import { mkdtempSync, writeFileSync, rmSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { describe, expect, test } from 'vitest';
import { variantsAfter, grepTree } from './install-kit.mjs';

describe('grepTree', () => {
  test('skips a file that goes between the listing and the read, as a finished download part does', () => {
    // Arrange
    const dir = mkdtempSync(join(tmpdir(), 'sl-grep-'));
    writeFileSync(join(dir, 'config.json'), 'live_TESTKEY');
    writeFileSync(join(dir, 'video.mp4.ytdl'), 'part');
    const read = (p) => {
      if (p.endsWith('.ytdl')) throw Object.assign(new Error(`ENOENT: no such file or directory, open '${p}'`), { code: 'ENOENT' });
      return readFileSync(p);
    };

    // Act
    const g = grepTree(dir, 'live_TESTKEY', { read });
    rmSync(dir, { recursive: true, force: true });

    // Assert
    expect(g).toEqual({ files: 1, hits: ['config.json'] });
  });
});

describe('variantsAfter', () => {
  test('numbers the test builds above the release under test', () => {
    // Arrange
    const release = '3.1.1';

    // Act
    const v = variantsAfter(release);

    // Assert
    expect([v.n1, v.n2, v.n3, v.n4].map((b) => b.version)).toEqual(['3.1.2', '3.1.3', '3.1.4', '3.1.5']);
    expect(v['n1-supervisor-fails']).toEqual({ version: '3.1.2', feature: 'test-supervisor-fails' });
    expect(v['n1-control-fails']).toEqual({ version: '3.1.2', feature: 'test-control-fails' });
  });

  test('counts a dev build from its release number', () => {
    // Act
    const v = variantsAfter('3.1.0-dev');

    // Assert
    expect(v.n1.version).toBe('3.1.1');
  });
});
