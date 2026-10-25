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

  it('titles the window StreamLoop 3', () => {
    expect(lines).toContain('title StreamLoop 3');
  });

  it.each([
    ['supervisor\\streamloop-supervisor.exe'],
    ['ffmpeg\\ffmpeg.exe'],
    ['pot\\build\\main.js'],
  ])('fails preflight with a plain error and a pause when %s is missing', (part) => {
    // Arrange
    const check = lines.indexOf(`if not exist "%ROOT%${part}" (`);

    // Assert
    expect(check).toBeGreaterThan(-1);
    expect(lines[check + 1]).toMatch(/^\s+echo \[ERROR\] .*Reinstall StreamLoop from the full ZIP\.$/);
    expect(lines[check + 2].trim()).toBe('pause');
    expect(lines[check + 3].trim()).toBe('exit /b 1');
  });

  it('heals before preflight and again on every start under :loop', () => {
    // Arrange
    const calls = lines.map((l, i) => (l === 'call :heal' ? i : -1)).filter((i) => i >= 0);
    const preflight = lines.indexOf('if not exist "%NODE%" (');
    const loop = lines.indexOf(':loop');
    const start = lines.findIndex((l) => l.startsWith('"%NODE%" node_modules'));

    // Assert
    expect(calls).toHaveLength(2);
    expect(calls[0]).toBeLessThan(preflight);
    expect(calls[1]).toBeGreaterThan(loop);
    expect(calls[1]).toBeLessThan(start);
  });

  it('restores app from _update_old or promotes a staged app in the heal routine', () => {
    const heal = lines.slice(lines.indexOf(':heal')).join('\n');
    expect(heal).toContain('if not exist "%APP%\\src\\server\\index.ts" (');
    expect(heal).toContain('rename "%ROOT%_update_old" app');
    expect(heal).toContain('move "%ROOT%_update_tmp\\app" "%APP%"');
  });

  it('heals app only, never node', () => {
    const heal = lines.slice(lines.indexOf(':heal')).join('\n');
    expect(heal).not.toMatch(/_update_old_node|_update_tmp\\node/);
  });

  it('keeps the heal routine after the last goto loop, so it only runs via call', () => {
    expect(lines.indexOf(':heal')).toBeGreaterThan(lines.lastIndexOf('goto loop'));
  });

  it('swaps app on exit 75 and carries config.json and logs over', () => {
    const text = lines.join('\n');
    expect(text).toContain('rename "%ROOT%app" _update_old');
    expect(text).toContain('move "%ROOT%_update_tmp\\app" "%ROOT%app"');
    expect(text).toContain('copy /y "%ROOT%_update_old\\config.json" "%ROOT%app\\config.json"');
    expect(text).toContain('xcopy "%ROOT%_update_old\\logs" "%ROOT%app\\logs\\"');
  });

  it('never swaps yt-dlp or node, which the supervisor and token server hold', () => {
    const text = lines.join('\n');
    expect(text).not.toMatch(/_update_tmp\\(yt-dlp|node)|_update_old_(ytdlp|node)/);
    expect(text).not.toMatch(/rename "%ROOT%(yt-dlp|node)"/);
  });

  it('keeps the launcher trampoline for a changed START.bat', () => {
    const text = lines.join('\n');
    expect(text).toContain('fc /b "%ROOT%_update_tmp\\START.bat" "%ROOT%START.bat"');
    expect(text).toContain('copy /y "%ROOT%_update_tmp\\START.bat" "%ROOT%START.bat.new"');
    expect(text).toContain('start "" cmd /c "!TRAMP!"');
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
