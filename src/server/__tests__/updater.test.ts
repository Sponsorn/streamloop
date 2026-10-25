import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdirSync, writeFileSync, existsSync, rmSync } from 'fs';
import { join } from 'path';
import {
  compareVersions, selectReleaseAssets, pickRelease, parseNodeVersion, stageUpdate, planUpdate, type GithubRelease,
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
    expect(selectReleaseAssets([asset('notes.txt')])).toEqual({ zipUrl: null, checksumUrl: null, stageNode: false });
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

describe('selectReleaseAssets with a Node version', () => {
  const bothBundles = [
    asset('streamloop-v2.3.8.zip'),
    asset('streamloop-v2.3.8.sha256'),
    asset('streamloop-v2.3.8-update.zip'),
    asset('streamloop-v2.3.8-update.sha256'),
  ];

  it('chooses the full ZIP and stages node/ when the release bundles a different Node', () => {
    // Act
    const picked = selectReleaseAssets(bothBundles, '22.23.4', '22.12.0');

    // Assert
    expect(picked.zipUrl).toMatch(/streamloop-v2\.3\.8\.zip$/);
    expect(picked.checksumUrl).toMatch(/streamloop-v2\.3\.8\.sha256$/);
    expect(picked.stageNode).toBe(true);
  });

  it('keeps the slim ZIP and ignores node/ when the release bundles the running Node', () => {
    const picked = selectReleaseAssets(bothBundles, '22.23.4', '22.23.4');
    expect(picked.zipUrl).toContain('-update.zip');
    expect(picked.checksumUrl).toContain('-update.sha256');
    expect(picked.stageNode).toBe(false);
  });

  it('keeps the slim ZIP when the release does not name its Node', () => {
    const picked = selectReleaseAssets(bothBundles, null, '22.12.0');
    expect(picked.zipUrl).toContain('-update.zip');
    expect(picked.stageNode).toBe(false);
  });

  it('falls back to the slim ZIP without node/ when the release has no full ZIP', () => {
    const picked = selectReleaseAssets([asset('streamloop-v2.3.8-update.zip')], '22.23.4', '22.12.0');
    expect(picked.zipUrl).toContain('-update.zip');
    expect(picked.stageNode).toBe(false);
  });
});

describe('parseNodeVersion', () => {
  it('reads a bare or v-prefixed version with surrounding whitespace', () => {
    expect(parseNodeVersion('22.23.4\n')).toBe('22.23.4');
    expect(parseNodeVersion(' v22.23.4 ')).toBe('22.23.4');
  });

  it('rejects anything that is not X.Y.Z', () => {
    expect(parseNodeVersion('<html>Not Found</html>')).toBeNull();
    expect(parseNodeVersion('')).toBeNull();
  });
});

describe('stageUpdate', () => {
  const root = join(import.meta.dirname, '..', '..', '..', '.test-tmp', 'updater');
  const extractDir = join(root, 'extracted');
  const tmpDir = join(root, 'tmp');

  beforeEach(() => {
    rmSync(root, { recursive: true, force: true });
    for (const dir of ['app/src', 'node', 'mpv', 'yt-dlp']) {
      mkdirSync(join(extractDir, 'streamloop', dir), { recursive: true });
    }
    writeFileSync(join(extractDir, 'streamloop', 'node', 'node.exe'), '');
    writeFileSync(join(extractDir, 'streamloop', 'mpv', 'mpv.exe'), '');
    writeFileSync(join(extractDir, 'streamloop', 'START.bat'), '');
    mkdirSync(tmpDir, { recursive: true });
  });

  afterEach(() => {
    rmSync(root, { recursive: true, force: true });
  });

  it('stages node/ next to app/ when asked to', () => {
    // Act
    stageUpdate(extractDir, tmpDir, true);

    // Assert
    expect(existsSync(join(tmpDir, 'app', 'src'))).toBe(true);
    expect(existsSync(join(tmpDir, 'node', 'node.exe'))).toBe(true);
    expect(existsSync(join(tmpDir, 'yt-dlp'))).toBe(true);
    expect(existsSync(join(tmpDir, 'START.bat'))).toBe(true);
    expect(existsSync(join(tmpDir, 'mpv'))).toBe(false);
  });

  it('leaves node/ out when the running Node already matches', () => {
    stageUpdate(extractDir, tmpDir, false);
    expect(existsSync(join(tmpDir, 'app', 'src'))).toBe(true);
    expect(existsSync(join(tmpDir, 'node'))).toBe(false);
    expect(existsSync(join(tmpDir, 'mpv'))).toBe(false);
  });

  it('leaves yt-dlp/ out of a Node-only update', () => {
    stageUpdate(extractDir, tmpDir, true, false);
    expect(existsSync(join(tmpDir, 'node', 'node.exe'))).toBe(true);
    expect(existsSync(join(tmpDir, 'yt-dlp'))).toBe(false);
  });

  it('throws when the archive has no app/', () => {
    rmSync(join(extractDir, 'streamloop', 'app'), { recursive: true });
    expect(() => stageUpdate(extractDir, tmpDir, true)).toThrow(/app\//);
  });
});

describe('planUpdate', () => {
  const v238 = {
    tag_name: 'v2.3.8', draft: false, prerelease: false,
    assets: [asset('streamloop-v2.3.8.zip'), asset('streamloop-v2.3.8.sha256'), asset('streamloop-v2.3.8-update.zip')],
  };

  it('offers nothing when the current release bundles the running Node', () => {
    // Act
    const plan = planUpdate(v238, '2.3.8', '22.23.4', { runningNode: '22.23.4' });

    // Assert
    expect(plan.updateAvailable).toBe(false);
    expect(plan.nodeOnly).toBeNull();
  });

  it('offers a Node-only update from the full ZIP when the current release bundles another Node', () => {
    const plan = planUpdate(v238, '2.3.8', '22.23.4', { runningNode: '22.12.0' });
    expect(plan.updateAvailable).toBe(true);
    expect(plan.nodeOnly).toBe('22.23.4');
    expect(plan.zipUrl).toMatch(/streamloop-v2\.3\.8\.zip$/);
    expect(plan.stageNode).toBe(true);
  });

  it('does not re-offer a Node version whose swap already failed on an automatic check', () => {
    const plan = planUpdate(v238, '2.3.8', '22.23.4', { runningNode: '22.12.0', lastNodeAttempt: '22.23.4' });
    expect(plan.updateAvailable).toBe(false);
    expect(plan.nodeOnly).toBeNull();
  });

  it('offers a failed Node version again on a manual check', () => {
    const plan = planUpdate(v238, '2.3.8', '22.23.4', { runningNode: '22.12.0', lastNodeAttempt: '22.23.4', manual: true });
    expect(plan.nodeOnly).toBe('22.23.4');
  });

  it('offers nothing when the current release does not name its Node', () => {
    expect(planUpdate(v238, '2.3.8', null, { runningNode: '22.12.0' }).updateAvailable).toBe(false);
  });

  it('treats a newer release as a normal update, not a Node-only one', () => {
    const plan = planUpdate({ ...v238, tag_name: 'v2.3.9' }, '2.3.8', '22.23.4', {
      runningNode: '22.12.0', lastNodeAttempt: '22.23.4',
    });
    expect(plan.updateAvailable).toBe(true);
    expect(plan.nodeOnly).toBeNull();
    expect(plan.stageNode).toBe(true);
  });
});
