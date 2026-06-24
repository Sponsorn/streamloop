import { execFile as execFileCb } from 'child_process';
import { promisify } from 'util';
import { existsSync } from 'fs';

const execFile = promisify(execFileCb);

export interface YtdlpUpdateResult {
  ok: boolean;
  version?: string;
  error?: string;
}

type ExecFn = (
  file: string,
  args: string[],
  opts: { timeout: number },
) => Promise<{ stdout: string; stderr: string }>;

interface UpdateOptions {
  execFn?: ExecFn;
  exists?: (path: string) => boolean;
}

/**
 * Run the bundled yt-dlp's self-update (`-U`) and report the resulting version.
 *
 * Self-update only works on the standalone Windows binary shipped in the release.
 * In dev (`npm start`) the `yt-dlp/` directory may not exist — in that case this
 * is a no-op that returns `ok: false` rather than throwing, so callers can fire
 * it on startup without guarding for the dev layout.
 */
export async function updateYtdlp(
  ytdlpPath: string,
  opts: UpdateOptions = {},
): Promise<YtdlpUpdateResult> {
  const exec = (opts.execFn ?? (execFile as unknown as ExecFn));
  const exists = opts.exists ?? existsSync;

  if (!exists(ytdlpPath)) {
    return { ok: false, error: 'yt-dlp binary not found (dev mode or missing install)' };
  }

  try {
    await exec(ytdlpPath, ['-U'], { timeout: 120_000 });
    const { stdout } = await exec(ytdlpPath, ['--version'], { timeout: 10_000 });
    return { ok: true, version: stdout.trim() };
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  }
}
