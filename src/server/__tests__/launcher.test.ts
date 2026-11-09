import { describe, it, expect } from 'vitest';
import { readFileSync } from 'fs';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';
import { PART_KEY_FILES, SWAP_FOLDERS } from '../components.js';

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

  it('heals every part folder as well as app', () => {
    // Arrange
    const heal = lines.slice(lines.indexOf(':heal'), lines.indexOf(':heal_part')).join('\n');
    const healPart = lines.slice(lines.indexOf(':heal_part')).join('\n');

    // Assert
    expect(heal).toContain('for %%F in (%PARTS%) do call :heal_part %%F');
    expect(healPart).toContain('if exist "%ROOT%%~1\\!KEY_%~1!" goto :eof');
    expect(healPart).toContain('if exist "%ROOT%_update_old_%~1\\!KEY_%~1!" (');
    expect(healPart).toContain('rename "%ROOT%_update_old_%~1" %~1');
    expect(healPart).toContain('move "%ROOT%_update_tmp\\%~1" "%ROOT%%~1"');
    expect(healPart).toContain('rename "%ROOT%_update_bad_%~1" %~1');
  });

  it('puts the previous version back when a swap or a rollback was cut off before it finished', () => {
    // Arrange
    const heal = lines.slice(lines.indexOf(':heal'), lines.indexOf(':heal_part'));
    const marker = heal.indexOf('if exist "%SWAPPING%" (');
    const branch = heal.slice(marker, heal.indexOf(')', marker) + 1).map((l) => l.trim());

    // Assert
    expect(lines).toContain('set "SWAPPING=%ROOT%data\\update-swapping"');
    expect(marker).toBeGreaterThan(-1);
    expect(branch).toEqual(expect.arrayContaining([
      'call :restore_parts',
      'call :restore_app',
      'if exist "%CHECKING%" del /f /q "%CHECKING%"',
    ]));
  });

  it('marks a rollback in progress until it is done, so heal can finish one that was cut off', () => {
    // Arrange
    const section = lines.slice(lines.indexOf(':rollback'), lines.indexOf(':heal')).map((l) => l.trim());
    const marked = section.indexOf('type nul > "%SWAPPING%"');

    // Assert
    expect(marked).toBeGreaterThan(-1);
    expect(marked).toBeLessThan(section.indexOf('call :restore_parts'));
    expect(section.indexOf('if "!RESTORE_FAILED!"=="0" del /f /q "%SWAPPING%" 2>nul')).toBeGreaterThan(section.indexOf('call :restore_app'));
  });

  it('calls a swap off without restoring anything when an earlier leftover cannot be removed', () => {
    // Arrange
    const swapParts = lines.slice(lines.indexOf(':swap_parts'), lines.indexOf(':swap_one')).map((l) => l.trim());
    const refused = swapParts.indexOf('if "%REFUSED%"=="1" (');
    const refusal = swapParts.slice(refused, swapParts.indexOf('goto :eof', refused) + 1);

    // Assert
    expect(swapParts.indexOf('call :clear_leftovers')).toBeLessThan(refused);
    expect(refused).toBeLessThan(swapParts.indexOf('type nul > "%SWAPPING%"'));
    expect(refusal).toContain('set "PARTS_RESULT=failed"');
    expect(refusal).not.toContain('call :restore_parts');
  });

  it('keeps the current part when its previous copy cannot be put back whole', () => {
    // Arrange
    const restoreOne = lines.slice(lines.indexOf(':restore_one'), lines.indexOf(':restore_app')).map((l) => l.trim());
    const restored = restoreOne.indexOf('call :log %~1 restored the previous version');

    // Assert: _update_bad_ is only removed once the restored folder holds its key file.
    expect(restoreOne.filter((l) => l.includes('rmdir /s /q "%ROOT%_update_bad_%~1"'))).toHaveLength(2);
    expect(restoreOne[restored + 1]).toBe('if exist "%ROOT%_update_bad_%~1" rmdir /s /q "%ROOT%_update_bad_%~1" 2>nul');
    expect(restoreOne).toContain('if not exist "%ROOT%%~1" if exist "%ROOT%_update_bad_%~1" rename "%ROOT%_update_bad_%~1" %~1 2>nul');
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

  it('carries no state.json from the old app, since 3.0 keeps state in data', () => {
    expect(lines.join('\n')).not.toContain('state.json');
  });

  it('drops the staged app when every rename fails, so a later exit 75 cannot swap it in', () => {
    const abort = lines.findIndex((l) => l.includes('Failed to rename old app directory'));
    expect(lines[abort + 1].trim()).toBe('if exist "%ROOT%_update_tmp" rmdir /s /q "%ROOT%_update_tmp" 2>nul');
  });

  it('lists the same swap folders and key files as components.js', () => {
    // Arrange
    const parts = lines.find((l) => l.startsWith('set "PARTS='))?.match(/^set "PARTS=(.*)"$/)?.[1].split(' ');
    const keys = Object.fromEntries(
      lines
        .map((l) => /^set "KEY_([\w-]+)=(.*)"$/.exec(l))
        .filter((m): m is RegExpExecArray => m !== null)
        .map((m) => [m[1], `${m[1]}/${m[2].replaceAll('\\', '/')}`]),
    );

    // Assert
    expect(parts).toEqual(SWAP_FOLDERS);
    expect(keys).toEqual(PART_KEY_FILES);
  });

  it('swaps each staged part folder into _update_old_<folder>', () => {
    // Arrange
    const swapParts = lines.slice(lines.indexOf(':swap_parts'), lines.indexOf(':swap_one')).join('\n');
    const swapOne = lines.slice(lines.indexOf(':swap_one'), lines.indexOf(':rename_tries')).join('\n');
    const tries = lines.slice(lines.indexOf(':rename_tries'), lines.indexOf(':end_processes')).join('\n');
    const block = lines.slice(lines.indexOf('if %ERRORLEVEL% equ 75 ('), lines.indexOf('if %ERRORLEVEL% equ 0 (')).join('\n');

    // Assert: binaries first in PARTS order, app after them.
    expect(swapParts).toContain('for %%F in (%PARTS%) do if exist "%ROOT%_update_tmp\\%%F" set "STAGED=!STAGED! %%F"');
    expect(block.indexOf('call :swap_parts')).toBeGreaterThan(-1);
    expect(block.indexOf('call :swap_parts')).toBeLessThan(block.indexOf('rename "%ROOT%app" _update_old'));
    expect(swapOne).toContain('call :rename_tries "%~1" "_update_old_%~1" %~1');
    expect(swapOne).toContain('move "%ROOT%_update_tmp\\%~1" "%ROOT%%~1"');
    expect(swapOne).toContain('"%ROOT%node\\node.exe" --version >nul 2>&1');
    expect(swapOne).toContain('copy /y "%ROOT%_update_old_yt-dlp\\yt-dlp.exe" "%ROOT%yt-dlp\\yt-dlp.exe"');
    expect(tries).toContain('for /L %%i in (1,1,3) do (');
    expect(tries).toContain('call :end_processes %~1');
    expect(tries).toContain('ping -n 4 127.0.0.1 >nul');
  });

  it('ends what runs from a folder before its first rename, held or not', () => {
    // Arrange
    const swapParts = lines.slice(lines.indexOf(':swap_parts'), lines.indexOf(':swap_one')).map((l) => l.trim());
    const restoreParts = lines.slice(lines.indexOf(':restore_parts'), lines.indexOf(':restore_one')).map((l) => l.trim());
    const restoreApp = lines.slice(lines.indexOf(':restore_app'), lines.indexOf(':app_failed')).map((l) => l.trim());

    // Assert
    expect(swapParts.indexOf('call :end_processes %STAGED%')).toBeGreaterThan(-1);
    expect(swapParts.indexOf('call :end_processes %STAGED%')).toBeLessThan(swapParts.findIndex((l) => l.includes('call :swap_one')));
    expect(restoreParts.indexOf('if defined AWAY call :end_processes %AWAY%')).toBeLessThan(restoreParts.findIndex((l) => l.includes('call :restore_one')));
    expect(restoreApp.indexOf('call :end_processes app')).toBeLessThan(restoreApp.indexOf('call :rename_tries "app" "_update_bad_app" app'));
  });

  it('keeps the old app in _update_old after a part swap, for a rollback', () => {
    // Assert
    expect(lines.map((l) => l.trim())).toContain(
      'if not "!PARTS_RESULT!"=="swapped" if exist "%ROOT%_update_old" rmdir /s /q "%ROOT%_update_old" 2>nul',
    );
  });

  it('rolls back on exit 76', () => {
    // Arrange
    const exit76 = lines.indexOf('if %ERRORLEVEL% equ 76 goto rollback');
    const section = lines
      .slice(lines.indexOf(':rollback'), lines.indexOf(':heal'))
      .map((l) => l.trim())
      .filter((l) => l && !l.startsWith('::'));

    // Assert
    expect(exit76).toBeGreaterThan(lines.indexOf('if %ERRORLEVEL% equ 0 ('));
    expect(section).toEqual(expect.arrayContaining([
      'call :restore_parts',
      'call :restore_app',
      'if exist "%CHECKING%" del /f /q "%CHECKING%"',
    ]));
    expect(section.slice(-2)).toEqual(['set "STREAMLOOP_START=rollback"', 'goto loop']);
  });

  it('carries the newer config.json and logs into the app it restores', () => {
    // Arrange
    const restore = lines.slice(lines.indexOf(':restore_app'), lines.indexOf(':app_failed')).join('\n');

    // Assert
    expect(restore).toContain('if exist "%ROOT%_update_old\\src\\server\\index.ts" (');
    expect(restore).toContain('copy /y "%ROOT%_update_bad_app\\config.json" "%APP%\\config.json"');
    expect(restore).toContain('xcopy "%ROOT%_update_bad_app\\logs" "%APP%\\logs\\" /E /I /Y');
  });

  it('rolls back on the third crash during an update check', () => {
    // Arrange
    const crash = lines.slice(lines.indexOf('if %ERRORLEVEL% equ 76 goto rollback'), lines.indexOf(':rollback')).map((l) => l.trim());
    const counted = crash.indexOf('set /a CHECK_CRASHES+=1');

    // Assert
    expect(crash).toContain('if exist "%CHECKING%" (');
    expect(counted).toBeGreaterThan(-1);
    expect(crash[counted + 1]).toBe('if !CHECK_CRASHES! geq 3 (');
    expect(crash.slice(counted + 1, counted + 6)).toContain('goto rollback');
    expect(crash).toContain('set "NEXT_START=update"');
    expect(lines).toContain('set "CHECKING=%ROOT%data\\update-checking"');
  });

  it('marks the health check as due after a part swap, so a crash before Node writes the marker counts', () => {
    // Arrange
    const block = lines.slice(lines.indexOf('if %ERRORLEVEL% equ 75 ('), lines.indexOf('if %ERRORLEVEL% equ 0 (')).map((l) => l.trim());
    const swapped = block.indexOf('if "!PARTS_RESULT!"=="swapped" (');
    const marked = block.indexOf('type nul > "%CHECKING%"');

    // Assert
    expect(swapped).toBeGreaterThan(-1);
    expect(block.slice(swapped, marked)).toContain('set "NEXT_START=update"');
    expect(marked - swapped).toBeLessThan(5);
    expect(marked).toBeLessThan(block.indexOf('if exist "%ROOT%START.bat.new" ('));
  });

  it('writes update-checking before it removes update-swapping, so new parts are never left with neither marker', () => {
    // Arrange
    const block = lines.slice(lines.indexOf('if %ERRORLEVEL% equ 75 ('), lines.indexOf('if %ERRORLEVEL% equ 0 (')).map((l) => l.trim());
    const marked = block.indexOf('type nul > "%CHECKING%"');
    const unswapped = block.flatMap((l, i) => (l.includes('del /f /q "%SWAPPING%"') ? [i] : []));
    const unmarkedForRollback = block.indexOf('if "!NEXT_START!"=="rollback" if exist "%CHECKING%" del /f /q "%CHECKING%"');

    // Assert
    expect(unswapped.length).toBeGreaterThan(0);
    for (const i of unswapped) expect(marked).toBeLessThan(i);
    // A rollback drops update-checking first, so the old version is never taken for unchecked new parts.
    expect(unmarkedForRollback).toBeGreaterThan(-1);
    expect(unmarkedForRollback).toBeLessThan(unswapped[unswapped.length - 1]);
  });

  it('drops update-checking before update-swapping when a part swap is put back', () => {
    // Arrange
    const swapParts = lines.slice(lines.indexOf(':swap_parts'), lines.indexOf(':clear_leftovers')).map((l) => l.trim());
    const restored = swapParts.indexOf('call :restore_parts');
    const after = swapParts.slice(restored);

    // Assert
    expect(restored).toBeGreaterThan(-1);
    expect(after.indexOf('if exist "%CHECKING%" del /f /q "%CHECKING%"')).toBeGreaterThan(-1);
    expect(after.indexOf('if exist "%CHECKING%" del /f /q "%CHECKING%"'))
      .toBeLessThan(after.indexOf('if "!RESTORE_FAILED!"=="0" if exist "%SWAPPING%" del /f /q "%SWAPPING%"'));
  });

  const psLines = lines.filter((l) => /powershell/i.test(l) && !/^\s*(::|rem\b)/i.test(l));

  it('passes folder paths to PowerShell through the environment, never in the command text', () => {
    // Arrange
    const ps = psLines;

    // Assert
    expect(ps.length).toBeGreaterThan(0);
    for (const l of ps) {
      expect(l).not.toMatch(/%ROOT%|!ROOT!|%~dp0|%APP%|%~1|%~2|%\*|%%F|%SWAP_LOG%/);
      expect(l).toContain('$env:SL_DIRS');
      expect(l).toContain('$env:SL_ROOT');
    }
  });

  it('writes the swap log statuses the dashboard reads', () => {
    // Arrange
    const statuses = new Set(
      lines.flatMap((l) => [...l.matchAll(/call :log \S+ (\S+)/g)].map((m) => m[1])),
    );

    // Assert
    expect([...statuses].sort()).toEqual(['failed', 'ok', 'restored', 'retry']);
    expect(psLines.join('\n')).toContain("' killed '");
  });

  it('keeps the launcher trampoline for a changed START.bat', () => {
    const text = lines.join('\n');
    expect(text).toContain('fc /b "%ROOT%_update_tmp\\START.bat" "%ROOT%START.bat"');
    expect(text).toContain('copy /y "%ROOT%_update_tmp\\START.bat" "%ROOT%START.bat.new"');
    expect(text).toContain('start "" cmd /c "!TRAMP!"');
  });

  it('writes no root path into the trampoline, which finds the root from its own location', () => {
    // Arrange
    const generated = lines.filter((l) => /^\s+>>? "!TRAMP!" echo /.test(l));

    // Assert
    expect(generated.length).toBeGreaterThan(0);
    expect(generated.filter((l) => /%ROOT%|!ROOT!/.test(l))).toEqual([]);
    expect(generated.join('\n')).toContain('move /y "%%~dp0START.bat.new" "%%~dp0START.bat"');
    expect(generated.join('\n')).toContain('start "StreamLoop 3" "%%~dp0START.bat"');
  });

  it('waits with ping, not timeout, which returns at once when stdin is redirected', () => {
    // Arrange
    const text = lines.join('\n');

    // Assert
    expect(text).not.toMatch(/timeout \/t/i);
    expect(text).toContain('ping -n 11 127.0.0.1 >nul');
  });

  it('starts as first, or as the kind of start its first argument names', () => {
    // Arrange
    const loop = lines.indexOf(':loop');
    const first = lines.indexOf('set "STREAMLOOP_START=first"');

    // Assert
    expect(first).toBeGreaterThan(-1);
    expect(first).toBeLessThan(loop);
    expect(lines[first + 1]).toBe('for %%A in (restart update rollback) do if /i "%~1"=="%%A" set "STREAMLOOP_START=%%A"');
  });

  it('names the next start before each goto loop: restart, update after a part swap, rollback', () => {
    // Arrange
    const gotos = lines.map((l, i) => (l.trim() === 'goto loop' ? i : -1)).filter((i) => i >= 0);
    const values = new Set(
      lines.flatMap((l) => [...l.matchAll(/set "(?:STREAMLOOP_START|NEXT_START)=([^"]+)"/g)].map((m) => m[1])),
    );

    // Assert
    expect(gotos.length).toBeGreaterThan(0);
    for (const g of gotos) expect(lines[g - 1].trim()).toMatch(/^set "STREAMLOOP_START=(rollback|!NEXT_START!|%NEXT_START%)"$/);
    expect([...values].filter((v) => !/^(%%A|!NEXT_START!|%NEXT_START%)$/.test(v)).sort()).toEqual(
      ['first', 'restart', 'rollback', 'update'],
    );
  });

  it('keeps STREAMLOOP_START across the launcher trampoline', () => {
    // Arrange
    const generated = lines.filter((l) => /^\s+>>? "!TRAMP!" echo /.test(l)).join('\n');

    // Assert
    expect(generated).toContain('start "StreamLoop 3" "%%~dp0START.bat" !NEXT_START!');
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
