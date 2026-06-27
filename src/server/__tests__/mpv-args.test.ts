import { describe, it, expect } from 'vitest';
import { buildMpvArgs } from '../mpv-args.js';
import type { AppConfig } from '../types.js';

function cfg(overrides: Partial<AppConfig> = {}): AppConfig {
  return {
    mpvGeometry: '1920x1080+0+0',
    mpvYtdlFormat: 'bestvideo[height<=?1080]+bestaudio/best',
    mpvExtraArgs: [],
    ytdlCookiesFromBrowser: '',
    ytdlPlayerClient: '',
    ...overrides,
  } as AppConfig;
}

describe('buildMpvArgs', () => {
  it('includes the core ytdl args', () => {
    const args = buildMpvArgs(cfg(), 'C:/yt-dlp.exe');
    expect(args).toContain('--loop-playlist=inf');
    expect(args).toContain('--ytdl-format=bestvideo[height<=?1080]+bestaudio/best');
    expect(args).toContain('--script-opts=ytdl_hook-ytdl_path=C:/yt-dlp.exe');
  });

  it('omits the player-client extractor-arg when ytdlPlayerClient is empty', () => {
    const args = buildMpvArgs(cfg(), 'C:/yt-dlp.exe');
    expect(args.some((a) => a.includes('player_client'))).toBe(false);
  });

  it('appends the player-client extractor-arg when ytdlPlayerClient is set', () => {
    const args = buildMpvArgs(cfg({ ytdlPlayerClient: 'web_safari' }), 'C:/yt-dlp.exe');
    expect(args).toContain('--ytdl-raw-options-append=extractor-args=youtube:player_client=web_safari');
  });

  it('still appends cookies-from-browser when set', () => {
    const args = buildMpvArgs(cfg({ ytdlCookiesFromBrowser: 'firefox' }), 'C:/yt-dlp.exe');
    expect(args).toContain('--ytdl-raw-options-append=cookies-from-browser=firefox');
  });

  it('appends mpvExtraArgs last', () => {
    const args = buildMpvArgs(cfg({ mpvExtraArgs: ['--mute=yes'] }), 'C:/yt-dlp.exe');
    expect(args[args.length - 1]).toBe('--mute=yes');
  });
});
