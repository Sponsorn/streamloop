import type { AppConfig } from './types.js';

/**
 * Build the mpv command-line args from config. Pure and side-effect free so it
 * can be unit-tested independently of the server bootstrap in index.ts.
 */
export function buildMpvArgs(config: AppConfig, ytdlpPath: string): string[] {
  const mpvArgs = [
    '--no-border',
    '--no-osc',
    '--osd-level=0',
    `--geometry=${config.mpvGeometry}`,
    '--hwdec=auto',
    `--ytdl-format=${config.mpvYtdlFormat}`,
    '--loop-playlist=inf',
    '--ytdl-raw-options=yes-playlist=,js-runtimes=node',
    `--script-opts=ytdl_hook-ytdl_path=${ytdlpPath}`,
  ];
  if (config.ytdlCookiesFromBrowser) {
    mpvArgs.push(`--ytdl-raw-options-append=cookies-from-browser=${config.ytdlCookiesFromBrowser}`);
  }
  // Pin yt-dlp's YouTube player_client(s). YouTube's per-client format/403
  // behaviour shifts over time, so this is a comma-separated fallback list
  // (e.g. `tv,web_safari`) that yt-dlp tries in order, merging formats. The
  // comma survives here because `--ytdl-raw-options-append` adds one literal
  // key=value entry — it is NOT re-split on commas the way the base
  // `--ytdl-raw-options=a,b` form is (verified end-to-end: mpv passes
  // `youtube:player_client=tv,web_safari` through to yt-dlp intact).
  if (config.ytdlPlayerClient) {
    mpvArgs.push(`--ytdl-raw-options-append=extractor-args=youtube:player_client=${config.ytdlPlayerClient}`);
  }
  mpvArgs.push(...config.mpvExtraArgs);
  return mpvArgs;
}
