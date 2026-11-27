import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { describe, expect, test } from 'vitest';
import { autostartSandbox, testKeyDeleteArgs, testKeyName } from './lib.mjs';

const PREFIX = String.raw`Software\StreamLoopTest-`;

describe('the autostart test key', () => {
  test('is named under Software\\StreamLoopTest-', () => {
    // Arrange, act
    const key = testKeyName();
    // Assert
    expect(key.startsWith(PREFIX)).toBe(true);
    expect(key).toMatch(/^Software\\StreamLoopTest-[0-9a-f]{12}$/);
  });

  test('is deleted only through a command that names HKCU\\Software\\StreamLoopTest-', () => {
    // Arrange
    const key = testKeyName();
    // Act
    const args = testKeyDeleteArgs(key);
    // Assert
    expect(args).toEqual(['delete', `HKCU\\${key}`, '/f']);
    expect(args[1].startsWith(String.raw`HKCU\Software\StreamLoopTest-`)).toBe(true);
  });

  test('is never deleted for any other name', () => {
    // Arrange
    const others = [
      'SoftwareStreamLoopTest-0123456789ab',
      String.raw`Software\Microsoft\Windows\CurrentVersion\Run`,
      String.raw`Software\StreamLoopTest-`,
      String.raw`Software\StreamLoopTest-xyz`,
      String.raw`Software\StreamLoopTest-0123456789ab\..\Run`,
      String.raw`HKCU\Software\StreamLoopTest-0123456789ab`,
      '',
      undefined,
    ];
    // Act, assert
    for (const key of others) expect(testKeyDeleteArgs(key)).toBeNull();
  });

  test('is removed from the registry with its Startup folder when the sandbox cleans up', () => {
    // Arrange
    const box = autostartSandbox('vitest');
    const add = spawnSync('reg.exe', ['add', `HKCU\\${box.key}\\Run`, '/v', 'StreamLoop', '/d', 'x', '/f'], { windowsHide: true });
    const before = spawnSync('reg.exe', ['query', `HKCU\\${box.key}`], { windowsHide: true }).status;
    // Act
    box.cleanup();
    const after = spawnSync('reg.exe', ['query', `HKCU\\${box.key}`], { windowsHide: true }).status;
    // Assert
    expect(add.status).toBe(0);
    expect(before).toBe(0);
    expect(after).not.toBe(0);
    expect(existsSync(box.startupDir)).toBe(false);
    expect(process.env.STREAMLOOP_TEST_AUTOSTART_KEY).toBe(box.key);
  });
});
