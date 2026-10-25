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

  it('swaps a staged node/ in with a rename, and checks the new node.exe runs', () => {
    // Arrange
    const text = lines.join('\n');

    // Assert
    expect(text).toContain('if exist "%ROOT%_update_tmp\\node\\node.exe" (');
    expect(text).toContain('rename "%ROOT%node" _update_old_node');
    expect(text).toContain('move "%ROOT%_update_tmp\\node" "%ROOT%node"');
    expect(text).toContain('"%ROOT%node\\node.exe" --version');
  });

  it('rolls node/ back from _update_old_node when the new node.exe fails', () => {
    const text = lines.join('\n');
    expect(text).toMatch(/--version[^\n]*\n\s+if errorlevel 1 \(/);
    expect(text).toContain('rename "%ROOT%_update_old_node" node');
  });

  it('retries putting the old node/ back when the rollback rename fails', () => {
    const text = lines.join('\n');
    expect(text).toMatch(/for \/L %%m in \(1,1,5\) do \(\s+if "!BACK_OK!"=="0" \(\s+rename "%ROOT%_update_old_node" node/);
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

  it('restores node/ from _update_old_node in the heal routine', () => {
    const heal = lines.slice(lines.indexOf(':heal')).join('\n');
    expect(heal).toContain('if exist "%ROOT%node\\node.exe" goto :eof');
    expect(heal).toContain('if exist "%ROOT%_update_old_node\\node.exe" (');
    expect(heal).toContain('rename "%ROOT%_update_old_node" node');
  });

  it('promotes a lone staged node/ only after its node.exe runs', () => {
    const heal = lines.slice(lines.indexOf(':heal')).join('\n');
    expect(heal).toMatch(
      /"%ROOT%_update_tmp\\node\\node\.exe" --version[^\n]*\n\s+if not errorlevel 1 \([\s\S]*move "%ROOT%_update_tmp\\node" "%ROOT%node"/,
    );
  });

  it('keeps the heal routine after the last goto loop, so it only runs via call', () => {
    expect(lines.indexOf(':heal')).toBeGreaterThan(lines.lastIndexOf('goto loop'));
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
