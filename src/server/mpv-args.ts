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
  // Pin yt-dlp's YouTube player_client. YouTube 403s the default `tv` (TVHTML5)
  // client's stream URLs at the CDN without a GVS PO token; `web_safari` (HLS)
  // is served normally. A single client only — mpv's ytdl-raw-options are
  // comma-separated, so a comma-joined client list would mis-parse here.
  if (config.ytdlPlayerClient) {
    mpvArgs.push(`--ytdl-raw-options-append=extractor-args=youtube:player_client=${config.ytdlPlayerClient}`);
  }
  mpvArgs.push(...config.mpvExtraArgs);
  return mpvArgs;
}
