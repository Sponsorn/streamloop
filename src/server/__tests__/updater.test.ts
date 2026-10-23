import { describe, it, expect } from 'vitest';
import { compareVersions, selectReleaseAssets, pickRelease, type GithubRelease } from '../updater.js';

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
