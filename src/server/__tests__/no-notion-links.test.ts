import { describe, it, expect } from 'vitest';
import { execFileSync } from 'child_process';
import { existsSync, readFileSync } from 'fs';
import { resolve, dirname, extname } from 'path';
import { fileURLToPath } from 'url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
const skippedExtensions = new Set([
  '.png', '.jpg', '.jpeg', '.gif', '.ico', '.webp', '.zip', '.exe', '.dll', '.mp4', '.mpg', '.lock',
]);
const skippedNames = new Set(['package-lock.json']);
const notionUrl = /notion\.(so|site|com)\//i;

describe('tracked files', () => {
  it('contain no Notion URL, since visitors would be asked to request access', () => {
    // Arrange
    const tracked = execFileSync('git', ['ls-files'], { cwd: repoRoot, encoding: 'utf8' })
      .split('\n')
      .filter(Boolean)
      .filter((path) => !skippedExtensions.has(extname(path).toLowerCase()))
      .filter((path) => !skippedNames.has(path.split('/').pop() ?? ''))
      .map((path) => resolve(repoRoot, path))
      .filter((path) => existsSync(path));

    // Act
    const hits = tracked.flatMap((file) =>
      readFileSync(file, 'utf8')
        .split(/\r?\n/)
        .flatMap((line, index) => (notionUrl.test(line) ? [`${file}:${index + 1}`] : [])),
    );

    // Assert
    expect(hits, `Notion URLs found:\n${hits.join('\n')}`).toEqual([]);
  });
});
