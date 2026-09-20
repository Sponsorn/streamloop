import { describe, it, expect } from 'vitest';
import { readFileSync } from 'fs';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';

const launcherPath = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'scripts', 'START.bat');
const lines = readFileSync(launcherPath, 'utf-8').split(/\r?\n/);

// cmd parses a whole parenthesized block up front; a `::` line inside one aborts the
// script and closes the window on any server exit. START.bat indents block contents.
describe('START.bat', () => {
  it('has no :: comments inside parenthesized blocks', () => {
    // Arrange
    const offenders = lines
      .map((text, i) => ({ line: i + 1, text }))
      .filter(({ text }) => /^\s+::/.test(text));

    // Act + Assert
    expect(offenders).toEqual([]);
  });

  it('keeps parentheses out of rem comments, which would open or close a block', () => {
    // Arrange
    const offenders = lines
      .map((text, i) => ({ line: i + 1, text }))
      .filter(({ text }) => /^\s+rem\b.*[()]/i.test(text));

    // Act + Assert
    expect(offenders).toEqual([]);
  });

  it('uses CRLF line endings, which cmd needs for labels and goto', () => {
    // Arrange
    const raw = readFileSync(launcherPath, 'utf-8');

    // Act
    const bareLf = raw.split('\n').length - raw.split('\r\n').length;

    // Assert
    expect(bareLf).toBe(0);
  });
});
