import { describe, expect, test } from 'vitest';
import {
  parseProcessCensus, installCensus, parseWindowCensus, windowsOf, fullScreenApp, foregroundGate, runCommandParts, verdictOf, SKIPPED,
} from './install-kit.mjs';

const ROOT = 'C:\\Users\\anton\\AppData\\Local\\Temp\\sl3 install Ö\\streamloop';
const EXE = `${ROOT}\\streamloop.exe`;
const BAT = `C:\\WINDOWS\\system32\\cmd.exe /d /s /c ""${ROOT}\\START.bat""`;

/** One row of `Get-CimInstance Win32_Process | Select-Object ... | ConvertTo-Json -Compress`, as PowerShell writes it. */
const row = (pid, ppid, name, cmd, ms, path = name === 'streamloop.exe' ? EXE : `C:\\WINDOWS\\system32\\${name}`) => ({
  ProcessId: pid, ParentProcessId: ppid, Name: name, ExecutablePath: path, CommandLine: cmd, CreationDate: `/Date(${ms})/`,
});
const json = (rows) => JSON.stringify(rows).replaceAll('/Date(', '\\/Date(').replace(/\)\/"/g, ')\\/"');

describe('parseProcessCensus', () => {
  test('reads an array with each start time in epoch ms', () => {
    // Arrange
    const text = json([row(10, 1, 'cmd.exe', BAT, 1764282116123), row(11, 10, 'streamloop.exe', `"${EXE}"`, 1764282117000)]);

    // Act
    const procs = parseProcessCensus(text);

    // Assert
    expect(procs.map((p) => [p.ProcessId, p.created])).toEqual([[10, 1764282116123], [11, 1764282117000]]);
  });

  test('reads the single object PowerShell writes for one process, after a byte order mark', () => {
    // Arrange
    const text = `\uFEFF${JSON.stringify(row(11, 4, 'streamloop.exe', `"${EXE}" ui --tray`, 5))}\r\n`;

    // Act
    const procs = parseProcessCensus(text);

    // Assert
    expect(procs).toHaveLength(1);
    expect(procs[0].mode).toBe('ui');
  });

  test('answers no processes for an empty or broken answer', () => {
    // Act + Assert
    expect(parseProcessCensus('')).toEqual([]);
    expect(parseProcessCensus('   \r\n')).toEqual([]);
    expect(parseProcessCensus('Get-CimInstance : Access denied')).toEqual([]);
  });

  test('names the mode of each streamloop.exe: the control process under START.bat, the app started by Explorer', () => {
    // Arrange
    const text = json([
      row(1, 0, 'explorer.exe', 'C:\\WINDOWS\\Explorer.EXE', 1),
      row(20, 1, 'streamloop.exe', `"${EXE}"`, 100),
      row(21, 99, 'cmd.exe', BAT, 200),
      row(22, 21, 'streamloop.exe', `"${EXE}"`, 300),
      row(23, 99, 'streamloop.exe', `"${EXE}" detach ui --follow 3.1.3 --tray`, 400),
    ]);

    // Act
    const modes = parseProcessCensus(text).filter((p) => p.Name === 'streamloop.exe').map((p) => [p.ProcessId, p.mode]);

    // Assert
    expect(modes).toEqual([[20, 'ui'], [22, 'control'], [23, 'detach']]);
  });
});

describe('installCensus', () => {
  test('sorts the install processes into the app, START.bat, the control process, the supervisor and the intermediates', () => {
    // Arrange
    const procs = parseProcessCensus(json([
      row(30, 7, 'streamloop.exe', `"${EXE}"`, 100),
      row(31, 99, 'cmd.exe', BAT, 200),
      row(32, 31, 'streamloop.exe', `"${EXE}"`, 300),
      row(33, 98, 'streamloop.exe', `"${EXE}" supervisor --root "${ROOT}"`, 400),
      row(34, 32, 'streamloop.exe', `"${EXE}" launch --root "${ROOT}"`, 410),
      row(35, 30, 'streamloop.exe', `"${EXE}" detach start-bat --root "${ROOT}"`, 150),
      row(36, 33, 'ffmpeg.exe', `"${ROOT}\\ffmpeg\\ffmpeg.exe" -i pipe:0`, 500, `${ROOT}\\ffmpeg\\ffmpeg.exe`),
      row(37, 31, 'cmd.exe', 'cmd.exe /c echo', 600),
    ]));

    // Act
    const c = installCensus(procs);

    // Assert
    const pids = (list) => list.map((p) => p.ProcessId);
    expect(pids(c.apps)).toEqual([30]);
    expect(pids(c.startBats)).toEqual([31]);
    expect(pids(c.controls)).toEqual([32]);
    expect(pids(c.supervisors)).toEqual([33]);
    expect(pids(c.launches)).toEqual([34]);
    expect(pids(c.detaches)).toEqual([35]);
    expect(c.counts).toEqual({ apps: 1, startBats: 1, controls: 1, supervisors: 1 });
  });

  test('counts nothing in an empty census', () => {
    // Act
    const c = installCensus([]);

    // Assert
    expect(c.counts).toEqual({ apps: 0, startBats: 0, controls: 0, supervisors: 0 });
  });
});

/** What the window census script prints: the foreground window with its process name, and every visible top-level window. */
const win = (hwnd, pid, title, cls, [l, t, r, b], { cloaked = false, mon = [0, 0, 2560, 1440] } = {}) => ({
  Hwnd: hwnd, Pid: pid, Title: title, ClassName: cls, Left: l, Top: t, Right: r, Bottom: b, Cloaked: cloaked,
  MonLeft: mon[0], MonTop: mon[1], MonRight: mon[2], MonBottom: mon[3],
});

describe('parseWindowCensus', () => {
  test('reads the foreground window and the visible windows with their owning process', () => {
    // Arrange
    const text = JSON.stringify({
      foreground: win(34999090, 27556, 'Claude', 'Chrome_WidgetWin_1', [-8, -8, 2568, 1400]),
      foregroundName: 'claude',
      windows: [win(65882, 2332, '', 'Shell_TrayWnd', [0, 1392, 2560, 1440]), win(77, 30, 'StreamLoop', 'Zed::Window', [640, 320, 1920, 1120])],
    });

    // Act
    const c = parseWindowCensus(text);

    // Assert
    expect(c.foreground).toMatchObject({ hwnd: 34999090, pid: 27556, name: 'claude', rect: { left: -8, top: -8, right: 2568, bottom: 1400 }, monitor: { left: 0, top: 0, right: 2560, bottom: 1440 } });
    expect(c.windows.map((w) => [w.hwnd, w.pid, w.title, w.className])).toEqual([[65882, 2332, '', 'Shell_TrayWnd'], [77, 30, 'StreamLoop', 'Zed::Window']]);
  });

  test('reads a census with no foreground window and one window written as an object', () => {
    // Arrange
    const text = `\uFEFF${JSON.stringify({ foreground: null, foregroundName: null, windows: win(5, 6, 'x', 'y', [0, 0, 1, 1]) })}`;

    // Act
    const c = parseWindowCensus(text);

    // Assert
    expect(c.foreground).toBeNull();
    expect(c.windows).toHaveLength(1);
  });

  test('answers an empty census for output that is not JSON', () => {
    // Act
    const c = parseWindowCensus('Add-Type : cannot compile');

    // Assert
    expect(c).toEqual({ foreground: null, windows: [], error: 'Add-Type : cannot compile' });
  });
});

describe('windowsOf', () => {
  test('keeps the shown windows of the given processes and leaves out cloaked ones', () => {
    // Arrange
    const c = parseWindowCensus(JSON.stringify({
      foreground: null,
      windows: [win(1, 30, 'StreamLoop', 'Zed::Window', [0, 0, 9, 9]), win(2, 30, '', 'Cloaked', [0, 0, 9, 9], { cloaked: true }), win(3, 40, 'Other', 'X', [0, 0, 9, 9])],
    }));

    // Act
    const mine = windowsOf(c, [30, 31]);

    // Assert
    expect(mine.map((w) => w.hwnd)).toEqual([1]);
  });
});

describe('fullScreenApp', () => {
  const fg = (name, cls, rect, mon) => parseWindowCensus(JSON.stringify({ foreground: win(1, 2, 't', cls, rect, { mon }), foregroundName: name, windows: [] })).foreground;

  test('takes a window that covers its whole monitor for a full-screen app', () => {
    // Act + Assert: a borderless game on the primary display and one on the second
    expect(fullScreenApp(fg('Wow', 'GxWindowClass', [0, 0, 2560, 1440], [0, 0, 2560, 1440]))).toBe(true);
    expect(fullScreenApp(fg('game', 'UnityWndClass', [2560, 0, 5120, 1440], [2560, 0, 5120, 1440]))).toBe(true);
  });

  test('does not take a maximised window above the taskbar for one', () => {
    // Act + Assert
    expect(fullScreenApp(fg('claude', 'Chrome_WidgetWin_1', [-8, -8, 2568, 1400], [0, 0, 2560, 1440]))).toBe(false);
  });

  test('does not take the desktop, Explorer or the lock screen for one', () => {
    // Act + Assert
    expect(fullScreenApp(fg('explorer', 'Progman', [0, 0, 2560, 1440], [0, 0, 2560, 1440]))).toBe(false);
    expect(fullScreenApp(fg('explorer', 'WorkerW', [0, 0, 2560, 1440], [0, 0, 2560, 1440]))).toBe(false);
    expect(fullScreenApp(fg('LockApp', 'Windows.UI.Core.CoreWindow', [0, 0, 2560, 1440], [0, 0, 2560, 1440]))).toBe(false);
  });

  test('does not take a missing foreground window for one', () => {
    // Act + Assert
    expect(fullScreenApp(null)).toBe(false);
  });
});

describe('foregroundGate', () => {
  const game = { name: 'Wow', fullScreen: true };
  const desk = { name: 'claude', fullScreen: false };
  /** A clock that moves only when the gate sleeps, and a probe that answers from `seen` in turn, repeating the last. */
  function rig(seen) {
    let t = 0;
    const said = [];
    return {
      said,
      opts: { label: 'A1', probe: async () => (seen.length > 1 ? seen.shift() : seen[0]), wait: async (ms) => { t += ms; }, now: () => t, say: (m) => said.push(m) },
    };
  }

  test('goes on at once when no full-screen app is in front', async () => {
    // Arrange
    const { opts, said } = rig([desk]);

    // Act
    const g = await foregroundGate(opts);

    // Assert
    expect(g).toMatchObject({ state: 'clear', waitedMs: 0 });
    expect(said).toEqual([]);
  });

  test('waits in steps of 30 s while a full-screen app is in front, saying so, and goes on once it is gone', async () => {
    // Arrange
    const { opts, said } = rig([game, game, game, desk]);

    // Act
    const g = await foregroundGate(opts);

    // Assert
    expect(g).toMatchObject({ state: 'clear', waitedMs: 90_000 });
    expect(said).toHaveLength(3);
    expect(said[0]).toMatch(/A1.*waits.*full-screen.*Wow/);
  });

  test('skips the step after 30 min with a full-screen app still in front', async () => {
    // Arrange
    const { opts } = rig([game]);

    // Act
    const g = await foregroundGate(opts);

    // Assert
    expect(g.state).toBe(SKIPPED);
    expect(g.waitedMs).toBe(30 * 60_000);
  });
});

describe('runCommandParts', () => {
  test('splits the Run value into the quoted exe and its arguments', () => {
    // Act
    const parts = runCommandParts(`"${EXE}" ui --tray`);

    // Assert
    expect(parts).toEqual({ exe: EXE, args: ['ui', '--tray'] });
  });

  test('reads an unquoted exe and answers null for no value', () => {
    // Act + Assert
    expect(runCommandParts('C:\\sl3\\streamloop.exe ui --tray')).toEqual({ exe: 'C:\\sl3\\streamloop.exe', args: ['ui', '--tray'] });
    expect(runCommandParts(null)).toBeNull();
    expect(runCommandParts('  ')).toBeNull();
  });
});

describe('verdictOf', () => {
  test('passes a run whose checks all hold', () => {
    // Act
    const v = verdictOf({ a: true, b: true });

    // Assert
    expect(v).toEqual({ verdict: 'PASS', failedChecks: [], skippedChecks: [] });
  });

  test('tells a run with a skipped check apart from a pass', () => {
    // Act
    const v = verdictOf({ a: true, 'A2.windowInFront': SKIPPED });

    // Assert
    expect(v).toEqual({ verdict: SKIPPED, failedChecks: [], skippedChecks: ['A2.windowInFront'] });
  });

  test('fails a run with a failed check, a skipped one beside it, or a stop or a failed cleanup', () => {
    // Act + Assert
    expect(verdictOf({ a: false, b: SKIPPED })).toEqual({ verdict: 'FAIL', failedChecks: ['a'], skippedChecks: ['b'] });
    expect(verdictOf({ a: true }, { stopped: true }).verdict).toBe('FAIL');
    expect(verdictOf({ a: true }, { cleanupOk: false }).verdict).toBe('FAIL');
    expect(verdictOf(undefined).verdict).toBe('FAIL');
  });
});
