import { mkdtempSync, writeFileSync, rmSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { describe, expect, test } from 'vitest';
import { variantsAfter, grepTree, checksumOf, guardVerdict } from './install-kit.mjs';

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

  test('numbers the builds that a released 3.1.x lands as the next minor version', () => {
    // Act
    const v = variantsAfter('3.1.2');

    // Assert
    expect([v['next-minor'], v['next-minor-1']]).toEqual([{ version: '3.2.0' }, { version: '3.2.1' }]);
  });
});

describe('checksumOf', () => {
  test('takes the hash from a release\'s .sha256, as the build writes it', () => {
    // Arrange
    const text = 'F8D0511E8049BB272B0055CDF243D5C21B3DD94CD8EEF77D8984FEDBF2395F0C  streamloop-v3.1.2.zip\r\n';

    // Act
    const hash = checksumOf(text);

    // Assert
    expect(hash).toBe('f8d0511e8049bb272b0055cdf243d5c21b3dd94cd8eef77d8984fedbf2395f0c');
  });

  test('answers null for a page that holds no hash', () => {
    // Act, assert
    for (const text of ['Not Found', '', 'f8d0511e  streamloop-v3.1.2.zip', undefined]) expect(checksumOf(text)).toBeNull();
  });
});

describe('guardVerdict', () => {
  test('reads 2.x\'s stream guard from the line it prints, after any warning', () => {
    // Arrange
    const streaming = '(node:1234) ExperimentalWarning: something\n{"streaming":true}\n';
    const not = '{"streaming":false}\r\n';

    // Act, assert
    expect(guardVerdict(streaming)).toBe(true);
    expect(guardVerdict(not)).toBe(false);
  });

  test('answers null when the guard printed no verdict', () => {
    // Act, assert
    for (const text of ['', 'Error: Cannot find module', '{"streaming":"yes"}', undefined]) expect(guardVerdict(text)).toBeNull();
  });
});
