import { existsSync, mkdirSync, rmSync, cpSync, renameSync, readFileSync, writeFileSync, createWriteStream, createReadStream } from 'fs';
import { createHash } from 'crypto';
import { execFileSync } from 'child_process';
import { join, resolve, dirname } from 'path';
import { fileURLToPath } from 'url';
import { pipeline } from 'stream/promises';
import { createRequire } from 'module';
import { logger } from './logger.js';

const __dirname = dirname(fileURLToPath(import.meta.url));

const GITHUB_REPO = 'Sponsorn/streamloop';
const GITHUB_API_URL = `https://api.github.com/repos/${GITHUB_REPO}/releases?per_page=50`;
const ALLOWED_DOWNLOAD_HOSTS = new Set(['github.com', 'objects.githubusercontent.com']);

function isAllowedDownloadUrl(url: string): boolean {
  try {
    const parsed = new URL(url);
    return parsed.protocol === 'https:' && ALLOWED_DOWNLOAD_HOSTS.has(parsed.hostname);
  } catch {
    return false;
  }
}

export type UpdateStatus = 'idle' | 'checking' | 'downloading' | 'extracting' | 'ready' | 'error';

export interface UpdateInfo {
  currentVersion: string;
  latestVersion: string | null;
  updateAvailable: boolean;
  status: UpdateStatus;
  error: string | null;
  isDevMode: boolean;
  nodeVersion: string;
  /** Target Node version when the available update only replaces Node. */
  nodeUpdate: string | null;
  nodeUpdateBlocked: NodeUpdateBlocked | null;
}

export interface SelectedAssets {
  zipUrl: string | null;
  checksumUrl: string | null;
  /** Stage the ZIP's node/ for START.bat to swap in. */
  stageNode: boolean;
}

/** Prefers the slim "-update.zip", or the full ZIP when the release bundles a different Node.
 *  The checksum is matched to the chosen ZIP by name, as "<file>.zip.sha256" or "<file>.sha256". */
export function selectReleaseAssets(
  assets: Array<{ name: string; browser_download_url: string }>,
  releaseNode: string | null = null,
  runningNode: string = process.versions.node,
): SelectedAssets {
  const slim = assets.find((a) => a.name.endsWith('-update.zip'));
  const full = assets.find((a) => a.name.endsWith('.zip') && !a.name.endsWith('-update.zip'));
  const wantNode = releaseNode !== null && releaseNode !== runningNode;
  const zipAsset = wantNode ? full ?? slim : slim ?? full;
  if (!zipAsset) return { zipUrl: null, checksumUrl: null, stageNode: false };

  const zipName = zipAsset.name;
  const zipBase = zipName.replace(/\.zip$/, '');
  const checksumAsset = assets.find(
    (a) => a.name === `${zipName}.sha256` || a.name === `${zipBase}.sha256`,
  );
  return {
    zipUrl: zipAsset.browser_download_url,
    checksumUrl: checksumAsset?.browser_download_url ?? null,
    stageNode: wantNode && zipAsset === full,
  };
}

/** The Node version a release names in its node-version.txt asset, or null. */
export function parseNodeVersion(text: string): string | null {
  const m = /^v?(\d+\.\d+\.\d+)$/.exec(text.trim());
  return m ? m[1] : null;
}

/** Move what START.bat swaps out of an extracted release into _update_tmp:
 *  app/, START.bat, optionally yt-dlp/ and node/. mpv/ is never staged. */
export function stageUpdate(extractDir: string, tmpDir: string, includeNode: boolean, includeYtdlp = true): void {
  // The ZIP holds streamloop/app/; older layouts may extract app/ directly.
  let root = join(extractDir, 'streamloop');
  if (!existsSync(join(root, 'app'))) root = extractDir;
  if (!existsSync(join(root, 'app'))) {
    throw new Error('Could not find app/ directory in update archive');
  }

  // config.json and state.json are copied by START.bat after the server exits,
  // so late config changes and the final flushed state are preserved.

  // START.bat never replaces itself in place; it stages this copy as START.bat.new.
  const newStartBat = join(extractDir, 'streamloop', 'START.bat');
  if (existsSync(newStartBat)) {
    cpSync(newStartBat, join(tmpDir, 'START.bat'));
  }

  // The live app/ can't be renamed while the server runs (EBUSY on Windows),
  // so START.bat swaps the staged copy in after the process exits.
  renameSync(join(root, 'app'), join(tmpDir, 'app'));

  // yt-dlp/ (yt-dlp.exe + deno.exe) rides along on every update: YouTube's
  // challenge shape rotates faster than we cut releases.
  const newYtdlpDir = join(extractDir, 'streamloop', 'yt-dlp');
  if (includeYtdlp && existsSync(newYtdlpDir)) {
    renameSync(newYtdlpDir, join(tmpDir, 'yt-dlp'));
  }

  const newNodeDir = join(extractDir, 'streamloop', 'node');
  if (includeNode && existsSync(join(newNodeDir, 'node.exe'))) {
    renameSync(newNodeDir, join(tmpDir, 'node'));
  }
}

export function compareVersions(a: string, b: string): number {
  const pa = a.replace(/^v/, '').split('.').map(Number);
  const pb = b.replace(/^v/, '').split('.').map(Number);
  const len = Math.max(pa.length, pb.length);
  for (let i = 0; i < len; i++) {
    const na = pa[i] ?? 0;
    const nb = pb[i] ?? 0;
    if (na > nb) return 1;
    if (na < nb) return -1;
  }
  return 0;
}

export interface GithubRelease {
  tag_name: string;
  draft: boolean;
  prerelease: boolean;
  assets: Array<{ name: string; browser_download_url: string }>;
}

const VERSION_TAG = /^v?(\d+)\.(\d+)\.(\d+)$/;

/** Newest stable release with the running major version. 3.x installs beside 2.x
 *  in its own folder, so a 2.x install must never take a 3.x release as an update. */
export function pickRelease(releases: GithubRelease[], currentVersion: string): GithubRelease | null {
  const major = currentVersion.replace(/^v/, '').split('.')[0];
  let best: GithubRelease | null = null;
  for (const r of releases) {
    const m = VERSION_TAG.exec(r.tag_name);
    if (r.draft || r.prerelease || !m || m[1] !== major) continue;
    if (!best || compareVersions(r.tag_name, best.tag_name) > 0) best = r;
  }
  return best;
}

export interface UpdatePlan extends SelectedAssets {
  updateAvailable: boolean;
  /** Target Node version when the update only replaces Node. */
  nodeOnly: string | null;
  nodeBlocked: NodeUpdateBlocked | null;
}

export interface NodeUpdateBlocked {
  version: string;
  reason: 'no checksum';
}

export interface PlanOptions {
  runningNode?: string;
  /** Node version of the last staged Node swap; still differing from runningNode means it rolled back. */
  lastNodeAttempt?: string | null;
  /** A user-requested check, which offers a failed Node version again. */
  manual?: boolean;
}

/** A newer release is a normal update; the current one is a Node-only update when its full ZIP bundles
 *  another Node and that swap has not failed before (or the check is manual). No checksum, no node/. */
export function planUpdate(
  release: GithubRelease,
  currentVersion: string,
  releaseNode: string | null,
  { runningNode = process.versions.node, lastNodeAttempt = null, manual = false }: PlanOptions = {},
): UpdatePlan {
  let assets = selectReleaseAssets(release.assets, releaseNode, runningNode);
  let nodeBlocked: NodeUpdateBlocked | null = null;
  if (assets.stageNode && !(assets.checksumUrl && isAllowedDownloadUrl(assets.checksumUrl))) {
    nodeBlocked = { version: releaseNode as string, reason: 'no checksum' };
    assets = selectReleaseAssets(release.assets, null, runningNode);
  }
  const cmp = compareVersions(release.tag_name, currentVersion);
  if (cmp > 0) return { ...assets, updateAvailable: true, nodeOnly: null, nodeBlocked };
  if (cmp === 0 && assets.stageNode && (manual || lastNodeAttempt !== releaseNode)) {
    return { ...assets, updateAvailable: true, nodeOnly: releaseNode, nodeBlocked };
  }
  return { ...assets, updateAvailable: false, nodeOnly: null, nodeBlocked };
}

export class Updater {
  private currentVersion: string;
  private latestVersion: string | null = null;
  private updateAvailable = false;
  private status: UpdateStatus = 'idle';
  private error: string | null = null;
  private autoCheckTimer: ReturnType<typeof setInterval> | null = null;
  private initialCheckTimer: ReturnType<typeof setTimeout> | null = null;
  private lastCheckTime = 0;
  private isDevMode: boolean;
  private releaseAssetUrl: string | null = null;
  private releaseChecksumUrl: string | null = null;
  private stageNode = false;
  private nodeUpdate: string | null = null;
  private nodeUpdateBlocked: NodeUpdateBlocked | null = null;
  private release: GithubRelease | null = null;
  private releaseNode: string | null = null;
  /** Survives the app/ swap, unlike anything inside app/. */
  private readonly attemptFile = resolve(__dirname, '..', '..', '..', 'node-update-attempt.txt');

  constructor() {
    // Read version from package.json
    const require = createRequire(import.meta.url);
    const pkg = require('../../package.json');
    this.currentVersion = pkg.version;

    // Dev mode: no sibling node/ directory means we're in dev
    const appDir = resolve(__dirname, '..', '..');
    const nodeDir = resolve(appDir, '..', 'node');
    this.isDevMode = !existsSync(nodeDir);
  }

  getStatus(): UpdateInfo {
    return {
      currentVersion: this.currentVersion,
      latestVersion: this.latestVersion,
      updateAvailable: this.updateAvailable,
      status: this.status,
      error: this.error,
      isDevMode: this.isDevMode,
      nodeVersion: process.versions.node,
      nodeUpdate: this.nodeUpdate,
      nodeUpdateBlocked: this.nodeUpdateBlocked,
    };
  }

  /** manual: the dashboard's "Check for Updates", which may re-offer a Node update that rolled back. */
  async checkForUpdate(manual = false): Promise<UpdateInfo> {
    // 60-second cooldown
    const now = Date.now();
    if (now - this.lastCheckTime < 60_000 && this.latestVersion !== null) {
      if (manual && this.release && this.status === 'idle') this.applyPlan(this.release, true);
      return this.getStatus();
    }

    this.status = 'checking';
    this.error = null;

    try {
      const res = await fetch(GITHUB_API_URL, {
        headers: {
          'Accept': 'application/vnd.github.v3+json',
          'User-Agent': `streamloop/${this.currentVersion}`,
        },
      });

      if (res.status === 404) {
        // No releases yet
        this.latestVersion = this.currentVersion;
        this.updateAvailable = false;
        this.nodeUpdate = null;
        this.nodeUpdateBlocked = null;
        this.status = 'idle';
        this.lastCheckTime = now;
        return this.getStatus();
      }

      if (!res.ok) {
        throw new Error(`GitHub API returned ${res.status}: ${res.statusText}`);
      }

      const release = pickRelease(await res.json() as GithubRelease[], this.currentVersion);
      if (!release) {
        this.latestVersion = this.currentVersion;
        this.updateAvailable = false;
        this.nodeUpdate = null;
        this.nodeUpdateBlocked = null;
        this.status = 'idle';
        this.lastCheckTime = now;
        return this.getStatus();
      }
      this.latestVersion = release.tag_name.replace(/^v/, '');
      // Dev mode runs the system Node, which the updater does not manage.
      this.releaseNode = this.isDevMode || compareVersions(this.latestVersion, this.currentVersion) < 0
        ? null
        : await this.fetchReleaseNodeVersion(release);
      this.release = release;
      this.applyPlan(release, manual);

      this.status = 'idle';
      this.lastCheckTime = now;
      logger.info({ currentVersion: this.currentVersion, latestVersion: this.latestVersion, updateAvailable: this.updateAvailable }, 'Update check complete');
    } catch (err) {
      this.status = 'error';
      this.error = err instanceof Error ? err.message : String(err);
      logger.error({ err }, 'Update check failed');
    }

    return this.getStatus();
  }

  private applyPlan(release: GithubRelease, manual: boolean): void {
    const plan = planUpdate(release, this.currentVersion, this.releaseNode, {
      lastNodeAttempt: this.readNodeAttempt(),
      manual,
    });
    if (plan.zipUrl && !isAllowedDownloadUrl(plan.zipUrl)) {
      throw new Error(`Untrusted download URL domain: ${plan.zipUrl}`);
    }
    this.updateAvailable = plan.updateAvailable;
    this.nodeUpdate = plan.nodeOnly;
    this.nodeUpdateBlocked = plan.nodeBlocked;
    if (plan.nodeBlocked) {
      logger.warn({ node: plan.nodeBlocked.version }, 'Node update unavailable: the release has no checksum for its full ZIP');
    }
    this.releaseAssetUrl = plan.updateAvailable ? plan.zipUrl : null;
    this.releaseChecksumUrl =
      plan.checksumUrl && isAllowedDownloadUrl(plan.checksumUrl) ? plan.checksumUrl : null;
    this.stageNode = plan.updateAvailable && plan.stageNode;
    if (this.stageNode) {
      logger.info({ from: process.versions.node, to: this.releaseNode, nodeOnly: plan.nodeOnly !== null }, 'Update will replace Node');
    }
  }

  private readNodeAttempt(): string | null {
    try {
      return parseNodeVersion(readFileSync(this.attemptFile, 'utf-8'));
    } catch {
      return null;
    }
  }

  /** Reads the release's node-version.txt asset. Any failure means "unknown",
   *  which keeps the slim update and the installed Node. */
  private async fetchReleaseNodeVersion(release: GithubRelease): Promise<string | null> {
    const url = release.assets.find((a) => a.name === 'node-version.txt')?.browser_download_url;
    if (!url || !isAllowedDownloadUrl(url)) return null;
    try {
      const res = await fetch(url, { headers: { 'User-Agent': `streamloop/${this.currentVersion}` } });
      return res.ok ? parseNodeVersion(await res.text()) : null;
    } catch (err) {
      logger.warn({ err }, 'Could not read the release Node version');
      return null;
    }
  }

  async downloadAndApply(): Promise<void> {
    if (this.isDevMode) {
      throw new Error('Cannot apply updates in dev mode');
    }

    if (!this.updateAvailable || !this.releaseAssetUrl) {
      throw new Error('No update available to apply');
    }

    if (this.status === 'downloading' || this.status === 'extracting' || this.status === 'ready') {
      throw new Error('Update already in progress');
    }

    const appDir = resolve(__dirname, '..', '..');
    const rootDir = resolve(appDir, '..');
    const tmpDir = join(rootDir, '_update_tmp');
    const oldDir = join(rootDir, '_update_old');

    try {
      // Clean up any previous update artifacts
      if (existsSync(tmpDir)) rmSync(tmpDir, { recursive: true, force: true });
      if (existsSync(oldDir)) rmSync(oldDir, { recursive: true, force: true });
      mkdirSync(tmpDir, { recursive: true });

      // Download
      this.status = 'downloading';
      this.error = null;
      logger.info({ url: this.releaseAssetUrl }, 'Downloading update');

      const zipPath = join(tmpDir, 'update.zip');
      const res = await fetch(this.releaseAssetUrl, {
        headers: { 'User-Agent': `streamloop/${this.currentVersion}` },
      });
      if (!res.ok) throw new Error(`Download failed: ${res.status} ${res.statusText}`);
      if (!res.body) throw new Error('No response body');

      const fileStream = createWriteStream(zipPath);
      await pipeline(res.body as unknown as NodeJS.ReadableStream, fileStream);

      // Verify SHA-256 checksum if available
      if (this.releaseChecksumUrl) {
        logger.info('Verifying update checksum');
        const checksumRes = await fetch(this.releaseChecksumUrl, {
          headers: { 'User-Agent': `streamloop/${this.currentVersion}` },
        });
        if (!checksumRes.ok) throw new Error(`Checksum download failed: ${checksumRes.status}`);
        const checksumText = await checksumRes.text();
        const expectedHash = checksumText.trim().split(/\s+/)[0].toLowerCase();
        const hash = createHash('sha256');
        await pipeline(createReadStream(zipPath), hash);
        const actualHash = hash.digest('hex');
        if (actualHash !== expectedHash) {
          throw new Error(`Checksum mismatch: expected ${expectedHash}, got ${actualHash}`);
        }
        logger.info('Update checksum verified');
      } else {
        logger.warn('No checksum file in release — skipping integrity verification');
      }

      // Extract
      this.status = 'extracting';
      logger.info('Extracting update');
      const extractDir = join(tmpDir, 'extracted');
      mkdirSync(extractDir, { recursive: true });
      execFileSync('C:\\Windows\\System32\\tar.exe', ['-xf', zipPath, '-C', extractDir], { timeout: 600_000 });

      // A Node-only update restages the same app and launcher but keeps the
      // installed yt-dlp, which may be newer after a self-update.
      const stageNode = this.stageNode && this.releaseChecksumUrl !== null;
      stageUpdate(extractDir, tmpDir, stageNode, this.nodeUpdate === null);
      if (stageNode && this.releaseNode) {
        writeFileSync(this.attemptFile, `${this.releaseNode}\n`);
      }

      this.status = 'ready';
      logger.info({ from: this.currentVersion, to: this.latestVersion }, 'Update staged, restart required');
    } catch (err) {
      this.status = 'error';
      this.error = err instanceof Error ? err.message : String(err);
      logger.error({ err }, 'Update failed');
      // Clean up partial download/extraction
      try {
        if (existsSync(tmpDir)) rmSync(tmpDir, { recursive: true, force: true });
      } catch { /* best effort */ }
      throw err;
    }
  }

  startAutoCheck(intervalMs: number): void {
    this.stopAutoCheck();
    // Initial check after 30 seconds
    this.initialCheckTimer = setTimeout(() => {
      this.initialCheckTimer = null;
      this.checkForUpdate().catch(() => {});
    }, 30_000);
    this.autoCheckTimer = setInterval(() => {
      this.checkForUpdate().catch(() => {});
    }, intervalMs);
  }

  stopAutoCheck(): void {
    if (this.initialCheckTimer) {
      clearTimeout(this.initialCheckTimer);
      this.initialCheckTimer = null;
    }
    if (this.autoCheckTimer) {
      clearInterval(this.autoCheckTimer);
      this.autoCheckTimer = null;
    }
  }
}
