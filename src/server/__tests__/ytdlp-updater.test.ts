import { describe, it, expect, vi } from 'vitest';
import { updateYtdlp } from '../ytdlp-updater.js';

describe('updateYtdlp', () => {
  it('skips when the binary does not exist (dev mode)', async () => {
    const execFn = vi.fn();
    const result = await updateYtdlp('/missing/yt-dlp.exe', {
      execFn,
      exists: () => false,
    });

    expect(result.ok).toBe(false);
    expect(result.error).toMatch(/not found/i);
    expect(execFn).not.toHaveBeenCalled();
  });

  it('runs -U then --version and returns the trimmed version on success', async () => {
    const execFn = vi
      .fn()
      .mockResolvedValueOnce({ stdout: 'Updated yt-dlp to stable@2026.06.01', stderr: '' })
      .mockResolvedValueOnce({ stdout: '2026.06.01\n', stderr: '' });

    const result = await updateYtdlp('/bin/yt-dlp.exe', {
      execFn,
      exists: () => true,
    });

    expect(result.ok).toBe(true);
    expect(result.version).toBe('2026.06.01');
    expect(execFn).toHaveBeenNthCalledWith(1, '/bin/yt-dlp.exe', ['-U'], expect.anything());
    expect(execFn).toHaveBeenNthCalledWith(2, '/bin/yt-dlp.exe', ['--version'], expect.anything());
  });

  it('returns ok:false with the error message when the update fails', async () => {
    const execFn = vi.fn().mockRejectedValueOnce(new Error('network unreachable'));

    const result = await updateYtdlp('/bin/yt-dlp.exe', {
      execFn,
      exists: () => true,
    });

    expect(result.ok).toBe(false);
    expect(result.error).toMatch(/network unreachable/);
  });
});
