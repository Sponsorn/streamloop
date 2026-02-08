import { existsSync, mkdirSync, rmSync, cpSync, renameSync, createWriteStream } from 'fs';
import { execSync } from 'child_process';
import { join, resolve, dirname } from 'path';
import { fileURLToPath } from 'url';
import { pipeline } from 'stream/promises';
import { createRequire } from 'module';
import { logger } from './logger.js';

const __dirname = dirname(fileURLToPath(import.meta.url));

const GITHUB_REPO = 'Sponsorn/kandy-freeze-monitor';
const GITHUB_API_URL = `https://api.github.com/repos/${GITHUB_REPO}/releases/latest`;

export type UpdateStatus = 'idle' | 'checking' | 'downloading' | 'extracting' | 'ready' | 'error';

export interface UpdateInfo {
  currentVersion: string;
  latestVersion: string | null;
  updateAvailable: boolean;
  status: UpdateStatus;
  error: string | null;
  isDevMode: boolean;
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

export class Updater {
  private currentVersion: string;
  private latestVersion: string | null = null;
  private updateAvailable = false;
  private status: UpdateStatus = 'idle';
  private error: string | null = null;
  private autoCheckTimer: ReturnType<typeof setInterval> | null = null;
  private lastCheckTime = 0;
  private isDevMode: boolean;
  private releaseAssetUrl: string | null = null;

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
    };
  }

  async checkForUpdate(): Promise<UpdateInfo> {
    // 60-second cooldown
    const now = Date.now();
    if (now - this.lastCheckTime < 60_000 && this.latestVersion !== null) {
      return this.getStatus();
    }

    this.status = 'checking';
    this.error = null;

    try {
      const res = await fetch(GITHUB_API_URL, {
        headers: {
          'Accept': 'application/vnd.github.v3+json',
          'User-Agent': `freeze-monitor/${this.currentVersion}`,
        },
      });

      if (res.status === 404) {
        // No releases yet
        this.latestVersion = this.currentVersion;
        this.updateAvailable = false;
        this.status = 'idle';
        this.lastCheckTime = now;
        return this.getStatus();
      }

      if (!res.ok) {
        throw new Error(`GitHub API returned ${res.status}: ${res.statusText}`);
      }

      const release = await res.json() as { tag_name: string; assets: Array<{ name: string; browser_download_url: string }> };
      this.latestVersion = release.tag_name.replace(/^v/, '');
      this.updateAvailable = compareVersions(this.latestVersion, this.currentVersion) > 0;

      // Find the ZIP asset
      if (this.updateAvailable) {
        const zipAsset = release.assets.find(a => a.name.endsWith('.zip'));
        this.releaseAssetUrl = zipAsset?.browser_download_url ?? null;
      }

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

  async downloadAndApply(): Promise<void> {
    if (this.isDevMode) {
      throw new Error('Cannot apply updates in dev mode');
    }

    if (!this.updateAvailable || !this.releaseAssetUrl) {
      throw new Error('No update available to apply');
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
        headers: { 'User-Agent': `freeze-monitor/${this.currentVersion}` },
      });
      if (!res.ok) throw new Error(`Download failed: ${res.status} ${res.statusText}`);
      if (!res.body) throw new Error('No response body');

      const fileStream = createWriteStream(zipPath);
      await pipeline(res.body as unknown as NodeJS.ReadableStream, fileStream);

      // Extract
      this.status = 'extracting';
      logger.info('Extracting update');
      const extractDir = join(tmpDir, 'extracted');
      mkdirSync(extractDir, { recursive: true });
      execSync(`powershell -Command "Expand-Archive -Path '${zipPath}' -DestinationPath '${extractDir}' -Force"`, { timeout: 120_000 });

      // Find the app directory inside the extracted zip
      // The ZIP contains freeze-monitor/app/ — we need the app folder
      let newAppDir = join(extractDir, 'freeze-monitor', 'app');
      if (!existsSync(newAppDir)) {
        // Maybe the zip extracts directly
        newAppDir = join(extractDir, 'app');
      }
      if (!existsSync(newAppDir)) {
        throw new Error('Could not find app/ directory in update archive');
      }

      // Copy config.json and state.json from current app to new app
      for (const file of ['config.json', 'state.json']) {
        const src = join(appDir, file);
        if (existsSync(src)) {
          cpSync(src, join(newAppDir, file));
        }
      }

      // Also copy the new START.bat and node/ if present in the release
      const newStartBat = join(extractDir, 'freeze-monitor', 'START.bat');
      if (existsSync(newStartBat)) {
        cpSync(newStartBat, join(rootDir, 'START.bat'));
      }

      // Swap: app/ → _update_old/, new app → app/
      this.status = 'ready';
      renameSync(appDir, oldDir);
      renameSync(newAppDir, appDir);

      logger.info({ from: this.currentVersion, to: this.latestVersion }, 'Update applied, restart required');
    } catch (err) {
      this.status = 'error';
      this.error = err instanceof Error ? err.message : String(err);
      logger.error({ err }, 'Update failed');

      // Try to restore if the old dir exists but app/ doesn't
      if (existsSync(oldDir) && !existsSync(appDir)) {
        try {
          renameSync(oldDir, appDir);
          logger.info('Restored original app/ after failed update');
        } catch (restoreErr) {
          logger.error({ err: restoreErr }, 'Failed to restore app/ after failed update');
        }
      }

      throw err;
    }
  }

  startAutoCheck(intervalMs: number): void {
    this.stopAutoCheck();
    // Initial check after 30 seconds
    setTimeout(() => {
      this.checkForUpdate().catch(() => {});
    }, 30_000);
    this.autoCheckTimer = setInterval(() => {
      this.checkForUpdate().catch(() => {});
    }, intervalMs);
  }

  stopAutoCheck(): void {
    if (this.autoCheckTimer) {
      clearInterval(this.autoCheckTimer);
      this.autoCheckTimer = null;
    }
  }
}
