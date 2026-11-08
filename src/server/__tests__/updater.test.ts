import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'fs';
import { execFileSync } from 'child_process';
import { createHash } from 'crypto';
import { join } from 'path';
import { fileURLToPath } from 'url';
import {
  compareVersions, selectReleaseAssets, pickRelease, parseNodeVersion, stageUpdate, Updater, type GithubRelease, parseComponents, updateKind,
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

function release(
  tag: string,
  opts: { draft?: boolean; prerelease?: boolean; assets?: GithubRelease['assets'] } = {},
): GithubRelease {
  const v = tag.replace(/^v/, '');
  return {
    tag_name: tag,
    draft: opts.draft ?? false,
    prerelease: opts.prerelease ?? false,
    assets: opts.assets ?? [asset(`streamloop-v${v}.zip`), asset(`streamloop-v${v}-update.zip`)],
  };
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

describe('pickRelease with incomplete releases', () => {
  it('skips a newer release whose update or full ZIP is not uploaded yet', () => {
    // Arrange
    const releases = [
      release('v3.0.3', { prerelease: true, assets: [asset('streamloop-v3.0.3.zip')] }),
      release('v3.0.2', { prerelease: true, assets: [asset('streamloop-v3.0.2-update.zip')] }),
      release('v3.0.1', { prerelease: true }),
    ];
    // Act
    const picked = pickRelease(releases, '3.0.0');
    // Assert
    expect(picked?.tag_name).toBe('v3.0.1');
  });
});

describe('stageUpdate with a flat ZIP', () => {
  const root = join(import.meta.dirname, '..', '..', '..', '.test-tmp', 'updater-flat');
  const extractDir = join(root, 'extracted');
  const tmpDir = join(root, 'tmp');

  afterEach(() => {
    rmSync(root, { recursive: true, force: true });
  });

  it('stages START.bat from an archive that extracts app/ directly', () => {
    // Arrange
    rmSync(root, { recursive: true, force: true });
    mkdirSync(join(extractDir, 'app', 'src'), { recursive: true });
    writeFileSync(join(extractDir, 'START.bat'), '@echo off');
    mkdirSync(tmpDir, { recursive: true });
    // Act
    stageUpdate(extractDir, tmpDir);
    // Assert
    expect(readdirSync(tmpDir).sort()).toEqual(['START.bat', 'app']);
  });
});

// One patch above the installed version, so the release below is always an update.
const [major, minor, patch] = (JSON.parse(readFileSync(fileURLToPath(new URL('../../../package.json', import.meta.url)), 'utf8')) as { version: string }).version.split('.').map(Number);
const NEXT = `${major}.${minor}.${patch + 1}`;

describe('Updater node notice', () => {
  const zip = `streamloop-v${NEXT}-update.zip`;
  const assets = [asset(zip), asset(`${zip}.sha256`), asset(`streamloop-v${NEXT}.zip`), asset('node-version.txt')];

  function stubFetch(nodeVersionText: string | null) {
    vi.stubGlobal('fetch', vi.fn(async (url: string) => {
      if (url.includes('api.github.com')) {
        return new Response(JSON.stringify([{ tag_name: `v${NEXT}`, draft: false, prerelease: true, assets }]));
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
    const info = await new Updater({ isDevMode: false }).checkForUpdate();
    // Assert
    expect(info.updateAvailable).toBe(true);
    expect(info.nodeNotice).toBe('This release bundles Node 1.2.3. Install the full ZIP to get it.');
  });

  it('leaves the notice empty when the Node versions match', async () => {
    stubFetch(process.versions.node);
    expect((await new Updater({ isDevMode: false }).checkForUpdate()).nodeNotice).toBeNull();
  });

  it('leaves the notice empty when node-version.txt is missing or malformed', async () => {
    stubFetch(null);
    expect((await new Updater({ isDevMode: false }).checkForUpdate()).nodeNotice).toBeNull();
    stubFetch('latest');
    expect((await new Updater({ isDevMode: false }).checkForUpdate()).nodeNotice).toBeNull();
  });

  it('leaves the notice empty in dev mode, which runs the system Node', async () => {
    stubFetch('v1.2.3');
    expect((await new Updater({ isDevMode: true }).checkForUpdate()).nodeNotice).toBeNull();
  });
});

describe('Updater fetch timeouts', () => {
  const zip = `streamloop-v${NEXT}-update.zip`;
  const assets = [asset(zip), asset(`${zip}.sha256`), asset(`streamloop-v${NEXT}.zip`), asset('node-version.txt')];

  // Answers only when aborted, like a stalled CDN.
  function hang(init?: RequestInit): Promise<Response> {
    return new Promise((_, reject) => {
      const signal = init?.signal;
      if (!signal) return;
      if (signal.aborted) reject(signal.reason);
      signal.addEventListener('abort', () => reject(signal.reason));
    });
  }

  beforeEach(() => {
    // AbortSignal.timeout runs on Node's internal timers, which fake timers do not reach.
    vi.spyOn(AbortSignal, 'timeout').mockImplementation(() =>
      AbortSignal.abort(new DOMException('The operation was aborted due to timeout', 'TimeoutError')));
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('gives up on a stalled node-version.txt and shows no notice', async () => {
    // Arrange
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      if (url.includes('api.github.com')) {
        return new Response(JSON.stringify([{ tag_name: `v${NEXT}`, draft: false, prerelease: true, assets }]));
      }
      return hang(init);
    }));
    // Act
    const info = await new Updater({ isDevMode: false }).checkForUpdate();
    // Assert
    expect(AbortSignal.timeout).toHaveBeenCalledWith(10_000);
    expect(info.status).toBe('idle');
    expect(info.updateAvailable).toBe(true);
    expect(info.nodeNotice).toBeNull();
  });

  it('shows a plain error when the GitHub API stalls', async () => {
    vi.stubGlobal('fetch', vi.fn((_url: string, init?: RequestInit) => hang(init)));
    const info = await new Updater({ isDevMode: false }).checkForUpdate();
    expect(AbortSignal.timeout).toHaveBeenCalledWith(10_000);
    expect(info.status).toBe('error');
    expect(info.error).toBe('GitHub did not answer within 10 seconds.');
  });
});

describe('Updater checksum', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('refuses to install a release with no checksum for its ZIP', async () => {
    // Arrange
    const download = vi.fn();
    vi.stubGlobal('fetch', vi.fn(async (url: string) => {
      if (url.includes('api.github.com')) {
        return new Response(JSON.stringify([{ tag_name: `v${NEXT}`, draft: false, prerelease: true, assets: [
          asset(`streamloop-v${NEXT}-update.zip`), asset(`streamloop-v${NEXT}.zip`), asset(`streamloop-v${NEXT}.zip.sha256`),
        ] }]));
      }
      download(url);
      return new Response('', { status: 404 });
    }));
    const updater = new Updater({ isDevMode: false });
    await updater.checkForUpdate();
    // Act
    const apply = updater.downloadAndApply();
    // Assert
    await expect(apply).rejects.toThrow('This release has no checksum file, so it was not installed.');
    expect(download).not.toHaveBeenCalled();
    expect(updater.getStatus().status).toBe('error');
  });
});

describe('Updater part comparison', () => {
  const HASH = (c: string) => c.repeat(64);
  const sameParts = { node: HASH('1'), supervisor: HASH('2'), ffmpeg: HASH('3'), pot: HASH('4'), 'ytdlp-plugin': HASH('5') };
  const components = (parts: Record<string, string> = sameParts) =>
    JSON.stringify({ version: NEXT, node: '22.23.4', parts });
  const current = `${major}.${minor}.${patch}`;

  function releaseAssets(version: string, withComponents: boolean) {
    const base = [asset(`streamloop-v${version}-update.zip`), asset(`streamloop-v${version}-update.zip.sha256`),
      asset(`streamloop-v${version}.zip`), asset(`streamloop-v${version}.zip.sha256`)];
    return withComponents ? [...base, asset('components.json')] : base;
  }

  function stubFetch(releases: Array<{ tag: string; assets: ReturnType<typeof asset>[] }>, componentsText: string | null) {
    vi.stubGlobal('fetch', vi.fn(async (url: string) => {
      if (url.includes('api.github.com')) {
        return new Response(JSON.stringify(releases.map((r) => ({ tag_name: r.tag, draft: false, prerelease: true, assets: r.assets }))));
      }
      if (url.endsWith('components.json') && componentsText !== null) return new Response(componentsText);
      return new Response('', { status: 404 });
    }));
  }

  const installOf = (over: Record<string, string | null> = {}) => async () => ({ ...sameParts, ...over }) as never;

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('takes the update ZIP when every part matches the release', async () => {
    // Arrange
    stubFetch([{ tag: `v${NEXT}`, assets: releaseAssets(NEXT, true) }], components());
    // Act
    const info = await new Updater({ isDevMode: false, installParts: installOf() }).checkForUpdate();
    // Assert
    expect(info.updateAvailable).toBe(true);
    expect(info.kind).toBe('app');
    expect(info.parts).toEqual([]);
    expect(info.sameVersion).toBe(false);
  });

  it('takes the full ZIP and names the parts when the supervisor differs', async () => {
    // Arrange
    stubFetch([{ tag: `v${NEXT}`, assets: releaseAssets(NEXT, true) }], components());
    // Act
    const info = await new Updater({ isDevMode: false, installParts: installOf({ supervisor: HASH('9') }) }).checkForUpdate();
    // Assert
    expect(info.kind).toBe('full');
    expect(info.parts).toEqual(['supervisor']);
    expect(info.nodeNotice).toBeNull();
  });

  it('falls back to the update ZIP when the release has no components.json', async () => {
    // Arrange
    stubFetch([{ tag: `v${NEXT}`, assets: releaseAssets(NEXT, false) }], null);
    // Act
    const info = await new Updater({ isDevMode: false, installParts: installOf({ supervisor: HASH('9') }) }).checkForUpdate();
    // Assert
    expect(info.updateAvailable).toBe(true);
    expect(info.kind).toBe('app');
  });

  it('falls back to the update ZIP when components.json is malformed', async () => {
    // Arrange
    stubFetch([{ tag: `v${NEXT}`, assets: releaseAssets(NEXT, true) }], '{"parts": [');
    // Act
    const info = await new Updater({ isDevMode: false, installParts: installOf({ supervisor: HASH('9') }) }).checkForUpdate();
    // Assert
    expect(info.updateAvailable).toBe(true);
    expect(info.kind).toBe('app');
    expect(info.parts).toEqual([]);
  });

  it('offers a same-version full update when the installed parts differ from the current release', async () => {
    // Arrange
    stubFetch([{ tag: `v${current}`, assets: releaseAssets(current, true) }], components());
    // Act
    const info = await new Updater({ isDevMode: false, installParts: installOf({ ffmpeg: HASH('9') }) }).checkForUpdate();
    // Assert
    expect(info.updateAvailable).toBe(true);
    expect(info.sameVersion).toBe(true);
    expect(info.kind).toBe('full');
    expect(info.parts).toEqual(['ffmpeg']);
    expect(info.latestVersion).toBe(current);
  });

  it('makes no same-version offer when the current release lacks the full ZIP or its checksum', async () => {
    // Arrange
    const noZip = [asset(`streamloop-v${current}-update.zip`), asset(`streamloop-v${current}-update.zip.sha256`), asset('components.json')];
    const noChecksum = [...noZip, asset(`streamloop-v${current}.zip`)];
    for (const assets of [noZip, noChecksum]) {
      stubFetch([{ tag: `v${current}`, assets }], components());
      // Act
      const info = await new Updater({ isDevMode: false, installParts: installOf({ ffmpeg: HASH('9') }) }).checkForUpdate();
      // Assert
      expect(info.updateAvailable).toBe(false);
      expect(info.sameVersion).toBe(false);
      expect(info.kind).toBeNull();
    }
  });

  it('keeps a same-version offer and reads components.json once per check', async () => {
    // Arrange
    let componentsRequests = 0;
    const assets = releaseAssets(current, true);
    vi.stubGlobal('fetch', vi.fn(async (url: string) => {
      if (url.includes('api.github.com')) {
        return new Response(JSON.stringify([{ tag_name: `v${current}`, draft: false, prerelease: true, assets }]));
      }
      if (url.endsWith('components.json')) {
        componentsRequests += 1;
        return componentsRequests === 1 ? new Response(components()) : new Response('', { status: 500 });
      }
      return new Response('', { status: 404 });
    }));
    // Act
    const info = await new Updater({ isDevMode: false, installParts: installOf({ ffmpeg: HASH('9') }) }).checkForUpdate();
    // Assert
    expect(componentsRequests).toBe(1);
    expect(info.updateAvailable).toBe(true);
    expect(info.sameVersion).toBe(true);
    expect(info.kind).toBe('full');
  });

  it("offers nothing when the current release's parts match", async () => {
    // Arrange
    stubFetch([{ tag: `v${current}`, assets: releaseAssets(current, true) }], components());
    // Act
    const info = await new Updater({ isDevMode: false, installParts: installOf() }).checkForUpdate();
    // Assert
    expect(info.updateAvailable).toBe(false);
    expect(info.sameVersion).toBe(false);
    expect(info.kind).toBeNull();
  });

  it('dev mode never offers a full update', async () => {
    // Arrange
    stubFetch([{ tag: `v${NEXT}`, assets: releaseAssets(NEXT, true) }], components());
    // Act
    const info = await new Updater({ isDevMode: true, installParts: installOf({ supervisor: HASH('9') }) }).checkForUpdate();
    // Assert
    expect(info.updateAvailable).toBe(true);
    expect(info.kind).toBe('app');
  });
});

describe('parseComponents and updateKind', () => {
  it('returns null for anything that is not a components file', () => {
    expect(parseComponents('')).toBeNull();
    expect(parseComponents('{}')).toBeNull();
    expect(parseComponents('{"version":"1.0.0","node":"22.0.0","parts":"x"}')).toBeNull();
  });

  it('keeps the part hashes of a valid file', () => {
    const text = JSON.stringify({ version: '3.0.6', node: '22.23.4', parts: { node: 'a', supervisor: 'b' } });
    expect(parseComponents(text)).toEqual({ version: '3.0.6', node: '22.23.4', parts: { node: 'a', supervisor: 'b' } });
  });

  it('stays app-only without an install or a release', () => {
    expect(updateKind(null, null)).toEqual({ kind: 'app', parts: [] });
  });
});

describe('selectReleaseAssets for a full update', () => {
  it('selectReleaseAssets with full picks the full ZIP and its own checksum', () => {
    // Arrange
    const assets = [
      asset('streamloop-v3.0.6.zip'), asset('streamloop-v3.0.6.zip.sha256'),
      asset('streamloop-v3.0.6-update.zip'), asset('streamloop-v3.0.6-update.zip.sha256'),
    ];
    // Act
    const { zipUrl, checksumUrl } = selectReleaseAssets(assets, 'full');
    // Assert
    expect(zipUrl).toMatch(/streamloop-v3\.0\.6\.zip$/);
    expect(checksumUrl).toMatch(/streamloop-v3\.0\.6\.zip\.sha256$/);
  });
});

describe('stageUpdate with part folders', () => {
  const root = join(import.meta.dirname, '..', '..', '..', '.test-tmp', 'updater-parts');
  const extractDir = join(root, 'extracted');
  const tmpDir = join(root, 'tmp');
  const top = join(extractDir, 'streamloop');

  beforeEach(() => {
    rmSync(root, { recursive: true, force: true });
    mkdirSync(join(top, 'app', 'src'), { recursive: true });
    mkdirSync(join(top, 'supervisor'), { recursive: true });
    mkdirSync(join(top, 'yt-dlp', 'plugins'), { recursive: true });
    writeFileSync(join(top, 'supervisor', 'streamloop-supervisor.exe'), '');
    writeFileSync(join(top, 'yt-dlp', 'plugins', 'bgutil-ytdlp-pot-provider.zip'), '');
    writeFileSync(join(top, 'START.bat'), '@echo off');
    mkdirSync(tmpDir, { recursive: true });
  });

  afterEach(() => {
    rmSync(root, { recursive: true, force: true });
  });

  it('stageUpdate moves the listed folders and app into the staging folder', () => {
    // Act
    stageUpdate(extractDir, tmpDir, ['supervisor', 'yt-dlp']);
    // Assert
    expect(readdirSync(tmpDir).sort()).toEqual(['START.bat', 'app', 'supervisor', 'yt-dlp']);
    expect(existsSync(join(tmpDir, 'supervisor', 'streamloop-supervisor.exe'))).toBe(true);
  });

  it("stageUpdate refuses an archive missing a part's key file", () => {
    // Arrange
    mkdirSync(join(top, 'ffmpeg'), { recursive: true });
    // Act
    const stage = () => stageUpdate(extractDir, tmpDir, ['supervisor', 'ffmpeg']);
    // Assert
    expect(stage).toThrow('The update archive has no ffmpeg/ffmpeg.exe');
    expect(readdirSync(tmpDir)).toEqual([]);
  });
});

describe.skipIf(process.platform !== 'win32')('Updater full update', () => {
  const HASH = (c: string) => c.repeat(64);
  const parts = { node: HASH('1'), supervisor: HASH('2'), ffmpeg: HASH('3'), pot: HASH('4'), 'ytdlp-plugin': HASH('5') };
  const root = join(import.meta.dirname, '..', '..', '..', '.test-tmp', 'updater-full');
  const zipName = `streamloop-v${NEXT}.zip`;
  let zip: Buffer;

  function buildZip(): Buffer {
    const src = join(root, 'src');
    const top = join(src, 'streamloop');
    for (const [rel, text] of [
      ['app/src/server/index.ts', ''], ['START.bat', '@echo off'],
      ['supervisor/streamloop-supervisor.exe', 'new'], ['supervisor/slate.mp4', ''],
      ['yt-dlp/yt-dlp.exe', ''], ['yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip', 'new'],
    ]) {
      mkdirSync(join(top, rel, '..'), { recursive: true });
      writeFileSync(join(top, rel), text);
    }
    const out = join(root, zipName);
    execFileSync('C:\\Windows\\System32\\tar.exe', ['-a', '-cf', out, '-C', src, 'streamloop']);
    return readFileSync(out);
  }

  function install(): void {
    for (const rel of ['supervisor/streamloop-supervisor.exe', 'ffmpeg/ffmpeg.exe', 'node/node.exe', 'pot/build/main.js', 'yt-dlp/plugins/bgutil-ytdlp-pot-provider.zip']) {
      mkdirSync(join(root, rel, '..'), { recursive: true });
      writeFileSync(join(root, rel), 'old');
    }
  }

  function stubRelease(): void {
    const sha = createHash('sha256').update(zip).digest('hex');
    vi.stubGlobal('fetch', vi.fn(async (url: string) => {
      if (url.includes('api.github.com')) {
        return new Response(JSON.stringify([{ tag_name: `v${NEXT}`, draft: false, prerelease: true, assets: [
          asset(`streamloop-v${NEXT}-update.zip`), asset(`streamloop-v${NEXT}-update.zip.sha256`),
          asset(zipName), asset(`${zipName}.sha256`), asset('components.json'),
        ] }]));
      }
      if (url.endsWith('components.json')) return new Response(JSON.stringify({ version: NEXT, node: '22.23.4', parts }));
      if (url.endsWith(`${zipName}.sha256`)) return new Response(`${sha}  ${zipName}\n`);
      if (url.endsWith(zipName)) return new Response(new Uint8Array(zip));
      return new Response('', { status: 404 });
    }));
  }

  type StopResult = { ok: true } | { ok: false; reason: string };

  function hooks(stop: () => Promise<StopResult> = async () => ({ ok: true })) {
    return {
      baseline: vi.fn(async () => ({ encoder: 'h264_nvenc' as const, works: true })),
      stop: vi.fn(stop),
      resume: vi.fn(),
      reportFailure: vi.fn(),
    };
  }

  async function updaterWith(h: ReturnType<typeof hooks>): Promise<Updater> {
    const updater = new Updater({
      isDevMode: false,
      installParts: async () => ({ ...parts, supervisor: HASH('9') }),
      rootDir: root,
      full: h,
    });
    const info = await updater.checkForUpdate();
    expect(info.kind).toBe('full');
    return updater;
  }

  const readState = () => JSON.parse(readFileSync(join(root, 'data', 'update-state.json'), 'utf8'));

  beforeEach(() => {
    rmSync(root, { recursive: true, force: true });
    mkdirSync(root, { recursive: true });
    zip = buildZip();
    rmSync(join(root, 'src'), { recursive: true, force: true });
    install();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    rmSync(root, { recursive: true, force: true });
  });

  it('records the baseline, stages the parts, stops the stream and moves on to the swap', async () => {
    // Arrange
    stubRelease();
    const h = hooks();
    const updater = await updaterWith(h);

    // Act
    await updater.downloadAndApply();

    // Assert
    expect(readdirSync(join(root, '_update_tmp')).filter((n) => !['extracted', 'update.zip'].includes(n)).sort())
      .toEqual(['START.bat', 'app', 'supervisor', 'yt-dlp']);
    expect(h.stop).toHaveBeenCalledWith(['supervisor', 'yt-dlp'], NEXT);
    expect(h.resume).not.toHaveBeenCalled();
    expect(h.reportFailure).not.toHaveBeenCalled();
    expect(readState()).toMatchObject({
      from: `${major}.${minor}.${patch}`, to: NEXT, parts: ['supervisor'], folders: ['supervisor', 'yt-dlp'],
      step: 'swap', result: 'running', reason: null, baseline: { encoder: 'h264_nvenc', works: true },
    });
    expect(updater.getStatus().status).toBe('ready');
  });

  it('removes copies left by an earlier update before it starts', async () => {
    // Arrange
    stubRelease();
    for (const dir of ['_update_old', '_update_old_ffmpeg', '_update_old_node', '_update_bad_pot']) {
      mkdirSync(join(root, dir, 'x'), { recursive: true });
    }
    const updater = await updaterWith(hooks());

    // Act
    await updater.downloadAndApply();

    // Assert
    for (const dir of ['_update_old', '_update_old_ffmpeg', '_update_old_node', '_update_bad_pot']) {
      expect(existsSync(join(root, dir))).toBe(false);
    }
  });

  it('keeps an earlier copy that is the only one of its part and calls the update off', async () => {
    // Arrange
    stubRelease();
    rmSync(join(root, 'ffmpeg'), { recursive: true });
    mkdirSync(join(root, '_update_old_ffmpeg'), { recursive: true });
    writeFileSync(join(root, '_update_old_ffmpeg', 'ffmpeg.exe'), 'old');
    const h = hooks();
    const updater = await updaterWith(h);

    // Act
    const apply = updater.downloadAndApply();

    // Assert
    await expect(apply).rejects.toThrow('_update_old_ffmpeg');
    expect(existsSync(join(root, '_update_old_ffmpeg', 'ffmpeg.exe'))).toBe(true);
    expect(h.stop).not.toHaveBeenCalled();
    expect(readState()).toMatchObject({ step: 'download', result: 'failed' });
    expect(h.reportFailure.mock.calls).toEqual([[
      `The update to ${NEXT} failed at download: ffmpeg\\ is not whole, so _update_old_ffmpeg from an earlier update was kept. Still on ${major}.${minor}.${patch}.`,
    ]]);
  });

  it('calls the update off and starts the services again when the stream cannot be stopped', async () => {
    // Arrange
    stubRelease();
    const h = hooks(async () => ({ ok: false, reason: 'ffmpeg\\ffmpeg.exe is still running.' }));
    const updater = await updaterWith(h);

    // Act
    const apply = updater.downloadAndApply();

    // Assert
    await expect(apply).rejects.toThrow('ffmpeg\\ffmpeg.exe is still running.');
    expect(readState()).toMatchObject({ step: 'stop', result: 'failed', reason: 'ffmpeg\\ffmpeg.exe is still running.' });
    expect(existsSync(join(root, '_update_tmp'))).toBe(false);
    expect(h.resume).toHaveBeenCalledTimes(1);
    expect(updater.getStatus()).toMatchObject({ status: 'error', error: 'ffmpeg\\ffmpeg.exe is still running.' });
    expect(h.reportFailure.mock.calls).toEqual([[
      `The update to ${NEXT} failed at stop: ffmpeg\\ffmpeg.exe is still running. Still on ${major}.${minor}.${patch}.`,
    ]]);
  });

  it('does not check for updates while one is being applied', async () => {
    // Arrange
    stubRelease();
    let finishStop: (v: StopResult) => void = () => {};
    const h = hooks(() => new Promise<StopResult>((r) => { finishStop = r; }));
    const updater = await updaterWith(h);
    const apply = updater.downloadAndApply();
    await vi.waitFor(() => expect(h.stop).toHaveBeenCalled());
    const fetchCalls = vi.mocked(fetch).mock.calls.length;
    // Past the 60 s cooldown, as the 6-hourly check would be.
    const later = Date.now() + 61_000;
    vi.spyOn(Date, 'now').mockReturnValue(later);

    // Act
    const info = await updater.checkForUpdate();
    vi.mocked(Date.now).mockRestore();

    // Assert
    expect(info).toMatchObject({ status: 'ready', kind: 'full', parts: ['supervisor'] });
    expect(vi.mocked(fetch).mock.calls.length).toBe(fetchCalls);
    finishStop({ ok: true });
    await apply;
  });
});
