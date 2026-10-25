import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { existsSync, mkdirSync, readdirSync, rmSync, writeFileSync } from 'fs';
import { join } from 'path';
import {
  compareVersions, selectReleaseAssets, pickRelease, parseNodeVersion, stageUpdate, Updater, type GithubRelease,
} from '../updater.js';

function asset(name: string) {
  return { name, browser_download_url: `https://github.com/x/y/releases/download/v1/${name}` };
}

describe('compareVersions', () => {
  it('returns 0 for equal versions', () => {
    expect(compareVersions('1.0.0', '1.0.0')).toBe(0);
  });

  it('returns 1 when first is greater (major)', () => {
    expect(compareVersions('2.0.0', '1.0.0')).toBe(1);
  });

  it('returns -1 when first is lesser (major)', () => {
    expect(compareVersions('1.0.0', '2.0.0')).toBe(-1);
  });

  it('compares minor versions', () => {
    expect(compareVersions('1.2.0', '1.1.0')).toBe(1);
    expect(compareVersions('1.1.0', '1.2.0')).toBe(-1);
  });

  it('compares patch versions', () => {
    expect(compareVersions('1.0.2', '1.0.1')).toBe(1);
    expect(compareVersions('1.0.1', '1.0.2')).toBe(-1);
  });

  it('handles v prefix', () => {
    expect(compareVersions('v1.2.0', '1.2.0')).toBe(0);
    expect(compareVersions('v2.0.0', 'v1.0.0')).toBe(1);
  });

  it('handles different segment counts', () => {
    expect(compareVersions('1.0', '1.0.0')).toBe(0);
    expect(compareVersions('1.0.1', '1.0')).toBe(1);
    expect(compareVersions('1.0', '1.0.1')).toBe(-1);
  });

  it('handles larger numbers', () => {
    expect(compareVersions('1.10.0', '1.9.0')).toBe(1);
    expect(compareVersions('1.2.10', '1.2.9')).toBe(1);
  });
});

describe('selectReleaseAssets', () => {
  it('prefers the slim -update.zip over the full bundle', () => {
    const { zipUrl } = selectReleaseAssets([
      asset('streamloop-v2.3.0.zip'),
      asset('streamloop-v2.3.0-update.zip'),
    ]);
    expect(zipUrl).toContain('streamloop-v2.3.0-update.zip');
  });

  it('falls back to the full zip when no -update.zip is present', () => {
    const { zipUrl } = selectReleaseAssets([asset('streamloop-v2.3.0.zip')]);
    expect(zipUrl).toContain('streamloop-v2.3.0.zip');
  });

  it('returns nulls when there is no zip asset', () => {
    expect(selectReleaseAssets([asset('notes.txt')])).toEqual({ zipUrl: null, checksumUrl: null });
  });

  it('matches the checksum named <file>.zip.sha256', () => {
    const { checksumUrl } = selectReleaseAssets([
      asset('streamloop-v2.3.0.zip'),
      asset('streamloop-v2.3.0.zip.sha256'),
    ]);
    expect(checksumUrl).toContain('streamloop-v2.3.0.zip.sha256');
  });

  it('matches the checksum named <file>.sha256 (base form)', () => {
    const { checksumUrl } = selectReleaseAssets([
      asset('streamloop-v2.3.0.zip'),
      asset('streamloop-v2.3.0.sha256'),
    ]);
    expect(checksumUrl).toContain('streamloop-v2.3.0.sha256');
  });

  it('picks the slim checksum (not the full one) when both bundles ship together', () => {
    const { zipUrl, checksumUrl } = selectReleaseAssets([
      asset('streamloop-v2.3.0.zip'),
      asset('streamloop-v2.3.0.sha256'),
      asset('streamloop-v2.3.0-update.zip'),
      asset('streamloop-v2.3.0-update.sha256'),
    ]);
    expect(zipUrl).toContain('streamloop-v2.3.0-update.zip');
    expect(checksumUrl).toContain('streamloop-v2.3.0-update.sha256');
  });

  it('returns a null checksum when none matches the chosen zip', () => {
    const { checksumUrl } = selectReleaseAssets([asset('streamloop-v2.3.0-update.zip')]);
    expect(checksumUrl).toBeNull();
  });
});

function release(tag: string, opts: { draft?: boolean; prerelease?: boolean } = {}): GithubRelease {
  return { tag_name: tag, draft: opts.draft ?? false, prerelease: opts.prerelease ?? false, assets: [] };
}

describe('pickRelease', () => {
  it('picks the newest stable release with the current major', () => {
    // Arrange
    const releases = [release('v3.0.0'), release('v2.3.8'), release('v2.3.7')];
    // Act
    const picked = pickRelease(releases, '2.3.6');
    // Assert
    expect(picked?.tag_name).toBe('v2.3.8');
  });

  it('finds a 2.x hotfix listed after a newer 3.x release', () => {
    const releases = [release('v3.1.0'), release('v3.0.0'), release('v2.3.9')];
    expect(pickRelease(releases, '2.3.6')?.tag_name).toBe('v2.3.9');
  });

  it('ignores drafts and prereleases', () => {
    const releases = [release('v2.4.0', { prerelease: true }), release('v2.3.9', { draft: true }), release('v2.3.7')];
    expect(pickRelease(releases, '2.3.6')?.tag_name).toBe('v2.3.7');
  });

  it('ignores tags that are not X.Y.Z', () => {
    const releases = [release('spike-overlay'), release('v3'), release('2.4.0-beta'), release('v2.3.7')];
    expect(pickRelease(releases, '2.3.6')?.tag_name).toBe('v2.3.7');
  });

  it('returns null when nothing matches', () => {
    expect(pickRelease([release('v3.0.0')], '2.3.6')).toBeNull();
    expect(pickRelease([], '2.3.6')).toBeNull();
  });

  it('accepts tags with and without a leading v', () => {
    const releases = [release('2.3.8'), release('v2.3.7')];
    expect(pickRelease(releases, '2.3.6')?.tag_name).toBe('2.3.8');
  });
});

describe('3.x updates', () => {
  it('treats a -dev build as older than its release', () => {
    expect(compareVersions('v3.0.0', '3.0.0-dev')).toBe(1);
    expect(compareVersions('3.0.0-dev', '3.0.0')).toBe(-1);
    expect(compareVersions('3.0.0-dev', '3.0.0-dev')).toBe(0);
    expect(compareVersions('3.0.1-dev', '3.0.0')).toBe(1);
  });

  it('offers a 3.0.0 install the v3.0.1 prerelease', () => {
    const releases = [release('v3.0.1', { prerelease: true }), release('v3.0.0', { prerelease: true })];
    expect(pickRelease(releases, '3.0.0')?.tag_name).toBe('v3.0.1');
  });

  it('offers the dev build the v3.0.0 prerelease', () => {
    const picked = pickRelease([release('v3.0.0', { prerelease: true })], '3.0.0-dev');
    expect(picked?.tag_name).toBe('v3.0.0');
  });

  it('never offers a 3.0.0 install a 2.x or 4.x release', () => {
    const releases = [release('v4.0.0'), release('v4.0.0', { prerelease: true }), release('v2.3.8')];
    expect(pickRelease(releases, '3.0.0')).toBeNull();
  });

  it('never offers a 2.3.8 install v3.0.1, prerelease or not', () => {
    expect(pickRelease([release('v3.0.1', { prerelease: true })], '2.3.8')).toBeNull();
    expect(pickRelease([release('v3.0.1')], '2.3.8')).toBeNull();
  });

  it('downloads the slim -update.zip of a 3.x release', () => {
    const { zipUrl, checksumUrl } = selectReleaseAssets([
      asset('streamloop-v3.0.1.zip'),
      asset('streamloop-v3.0.1.zip.sha256'),
      asset('streamloop-v3.0.1-update.zip'),
      asset('streamloop-v3.0.1-update.zip.sha256'),
      asset('node-version.txt'),
    ]);
    expect(zipUrl).toContain('streamloop-v3.0.1-update.zip');
    expect(checksumUrl).toContain('streamloop-v3.0.1-update.zip.sha256');
  });
});

describe('parseNodeVersion', () => {
  it('accepts a v prefix and surrounding whitespace', () => {
    expect(parseNodeVersion('  v22.23.4\r\n')).toBe('22.23.4');
    expect(parseNodeVersion('22.23.4')).toBe('22.23.4');
  });

  it('returns null for anything else', () => {
    expect(parseNodeVersion('')).toBeNull();
    expect(parseNodeVersion('node 22')).toBeNull();
    expect(parseNodeVersion('22.23')).toBeNull();
  });
});

describe('stageUpdate', () => {
  const root = join(import.meta.dirname, '..', '..', '..', '.test-tmp', 'updater');
  const extractDir = join(root, 'extracted');
  const tmpDir = join(root, 'tmp');

  beforeEach(() => {
    rmSync(root, { recursive: true, force: true });
    for (const dir of ['app/src', 'yt-dlp', 'node', 'mpv']) {
      mkdirSync(join(extractDir, 'streamloop', dir), { recursive: true });
    }
    writeFileSync(join(extractDir, 'streamloop', 'START.bat'), '@echo off');
    writeFileSync(join(extractDir, 'streamloop', 'node', 'node.exe'), '');
    writeFileSync(join(extractDir, 'streamloop', 'yt-dlp', 'yt-dlp.exe'), '');
    mkdirSync(tmpDir, { recursive: true });
  });

  afterEach(() => {
    rmSync(root, { recursive: true, force: true });
  });

  it('stages only app/ and START.bat, ignoring yt-dlp/, node/ and mpv/', () => {
    // Act
    stageUpdate(extractDir, tmpDir);
    // Assert
    expect(readdirSync(tmpDir).sort()).toEqual(['START.bat', 'app']);
    expect(existsSync(join(tmpDir, 'app', 'src'))).toBe(true);
  });

  it('throws when the archive has no app/', () => {
    rmSync(join(extractDir, 'streamloop', 'app'), { recursive: true });
    expect(() => stageUpdate(extractDir, tmpDir)).toThrow(/app\//);
  });
});

describe('Updater node notice', () => {
  const zip = 'streamloop-v3.0.1-update.zip';
  const assets = [asset(zip), asset(`${zip}.sha256`), asset('node-version.txt')];

  function stubFetch(nodeVersionText: string | null) {
    vi.stubGlobal('fetch', vi.fn(async (url: string) => {
      if (url.includes('api.github.com')) {
        return new Response(JSON.stringify([{ tag_name: 'v3.0.1', draft: false, prerelease: true, assets }]));
      }
      if (url.endsWith('node-version.txt') && nodeVersionText !== null) return new Response(nodeVersionText);
      return new Response('', { status: 404 });
    }));
  }

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('sets the notice when the release bundles another Node', async () => {
    // Arrange
    stubFetch('v1.2.3\n');
    // Act
    const info = await new Updater().checkForUpdate();
    // Assert
    expect(info.updateAvailable).toBe(true);
    expect(info.nodeNotice).toBe('This release bundles Node 1.2.3. Install the full ZIP to get it.');
  });

  it('leaves the notice empty when the Node versions match', async () => {
    stubFetch(process.versions.node);
    expect((await new Updater().checkForUpdate()).nodeNotice).toBeNull();
  });

  it('leaves the notice empty when node-version.txt is missing or malformed', async () => {
    stubFetch(null);
    expect((await new Updater().checkForUpdate()).nodeNotice).toBeNull();
    stubFetch('latest');
    expect((await new Updater().checkForUpdate()).nodeNotice).toBeNull();
  });
});
