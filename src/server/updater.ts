import { existsSync, mkdirSync, rmSync, cpSync, renameSync, createWriteStream, createReadStream, readFileSync } from 'fs';
import { createHash } from 'crypto';
import { execFileSync } from 'child_process';
import { join, resolve, dirname } from 'path';
import { fileURLToPath } from 'url';
import { pipeline } from 'stream/promises';
import { createRequire } from 'module';
import { z } from 'zod';
import { logger } from './logger.js';
import { diffParts, foldersFor, PART_KEY_FILES, PARTS, type Part } from './components.js';
import { SPAWN_CWD } from './spawn-cwd.js';
import { clearLeftovers, failureMessage, type StopResult } from './full-update.js';
import { writeUpdateState, type UpdateState } from './update-state.js';

const __dirname = dirname(fileURLToPath(import.meta.url));

const GITHUB_REPO = 'Sponsorn/streamloop';
const GITHUB_API_URL = `https://api.github.com/repos/${GITHUB_REPO}/releases?per_page=50`;
const FETCH_TIMEOUT_MS = 10_000;
const ALLOWED_DOWNLOAD_HOSTS = new Set(['github.com', 'objects.githubusercontent.com']);

/** GitHub over https; with a test release list (STREAMLOOP_TEST_RELEASES), only http://127.0.0.1:<port>/ instead. */
function isAllowedDownloadUrl(url: string, testReleases = false): boolean {
  try {
    const parsed = new URL(url);
    if (testReleases) return parsed.protocol === 'http:' && parsed.hostname === '127.0.0.1' && parsed.port !== '';
    return parsed.protocol === 'https:' && ALLOWED_DOWNLOAD_HOSTS.has(parsed.hostname);
  } catch {
    return false;
  }
}

export type UpdateKind = 'app' | 'full';

export type InstallParts = Record<Part, string | null>;

export interface ReleaseComponents {
  version: string;
  node: string;
  parts: Partial<Record<Part, string>>;
}

const componentsSchema = z.object({
  version: z.string(),
  node: z.string(),
  parts: z.object(Object.fromEntries(PARTS.map((p) => [p, z.string().optional()]))),
});

/** A release's components.json, or null when it is not valid JSON of the expected shape. */
export function parseComponents(text: string): ReleaseComponents | null {
  try {
    const parsed = componentsSchema.safeParse(JSON.parse(text));
    if (!parsed.success) return null;
    const raw = parsed.data.parts as Record<string, string | undefined>;
    const parts: Partial<Record<Part, string>> = {};
    for (const p of PARTS) {
      const hash = raw[p];
      if (hash !== undefined) parts[p] = hash;
    }
    return { version: parsed.data.version, node: parsed.data.node, parts };
  } catch {
    return null;
  }
}

/** App-only unless both sides are known and a part's hash differs; the differing parts name the full update. */
export function updateKind(
  install: InstallParts | null,
  release: ReleaseComponents | null,
): { kind: UpdateKind; parts: Part[] } {
  if (!install || !release) return { kind: 'app', parts: [] };
  const parts = diffParts(install, release.parts);
  return parts.length > 0 ? { kind: 'full', parts } : { kind: 'app', parts: [] };
}

export type UpdateStatus = 'idle' | 'checking' | 'downloading' | 'extracting' | 'ready' | 'error';

export interface UpdateInfo {
  currentVersion: string;
  latestVersion: string | null;
  updateAvailable: boolean;
  status: UpdateStatus;
  error: string | null;
  isDevMode: boolean;
  /** Set when the release bundles another Node than the running one. Updates never deliver node/. */
  nodeNotice: string | null;
  /** Null until a check found something to install. */
  kind: UpdateKind | null;
  /** Parts a full update replaces. */
  parts: Part[];
  /** The offer is the running version with its parts reinstalled. */
  sameVersion: boolean;
}

/** Pick the best ZIP asset and its matching checksum from a release's assets.
 *  Prefers the slim "-update.zip" (much smaller) over the full bundle, falling
 *  back to any ".zip". The
 *  checksum is matched to the *chosen* ZIP by name so a release carrying both
 *  bundles can't cross the wires. Accepts either "<file>.zip.sha256" or
 *  "<file>.sha256", since both naming conventions have shipped. */
export function selectReleaseAssets(
  assets: Array<{ name: string; browser_download_url: string }>,
  kind: UpdateKind = 'app',
): { zipUrl: string | null; checksumUrl: string | null } {
  const zipAsset = kind === 'full'
    ? assets.find((a) => a.name.endsWith('.zip') && !a.name.endsWith('-update.zip'))
    : assets.find((a) => a.name.endsWith('-update.zip')) ??
      assets.find((a) => a.name.endsWith('.zip'));
  if (!zipAsset) return { zipUrl: null, checksumUrl: null };

  const zipName = zipAsset.name;
  const zipBase = zipName.replace(/\.zip$/, '');
  const checksumAsset = assets.find(
    (a) => a.name === `${zipName}.sha256` || a.name === `${zipBase}.sha256`,
  );
  return {
    zipUrl: zipAsset.browser_download_url,
    checksumUrl: checksumAsset?.browser_download_url ?? null,
  };
}

/** The Node version a release names in its node-version.txt asset, or null. */
export function parseNodeVersion(text: string): string | null {
  const m = /^v?(\d+\.\d+\.\d+)$/.exec(text.trim());
  return m ? m[1] : null;
}

/** Stage app/ and START.bat into _update_tmp, plus the listed part folders of a full update.
 *  START.bat swaps whatever lands there. An app-only update leaves the part folders out,
 *  since the supervisor holds yt-dlp/ and the token server holds node/. */
export function stageUpdate(extractDir: string, tmpDir: string, folders: string[] = []): void {
  // The ZIP holds streamloop/app/; older layouts may extract app/ directly.
  let root = join(extractDir, 'streamloop');
  if (!existsSync(join(root, 'app'))) root = extractDir;
  if (!existsSync(join(root, 'app'))) {
    throw new Error('Could not find app/ directory in update archive');
  }

  for (const folder of folders) {
    const keyFile = PART_KEY_FILES[folder];
    if (keyFile && !existsSync(join(root, keyFile))) {
      throw new Error(`The update archive has no ${keyFile}`);
    }
  }

  // config.json and logs\ are copied by START.bat after the server exits,
  // so late config changes are preserved.

  // START.bat never replaces itself in place; it stages this copy as START.bat.new.
  const newStartBat = join(root, 'START.bat');
  if (existsSync(newStartBat)) {
    cpSync(newStartBat, join(tmpDir, 'START.bat'));
  }

  // The live app/ can't be renamed while the server runs (EBUSY on Windows),
  // so START.bat swaps the staged copy in after the process exits.
  renameSync(join(root, 'app'), join(tmpDir, 'app'));
  for (const folder of folders) renameSync(join(root, folder), join(tmpDir, folder));
}

/** Numeric X.Y.Z compare. A suffixed version such as "3.0.0-dev" sorts below its release. */
export function compareVersions(a: string, b: string): number {
  const [ca, sa] = a.replace(/^v/, '').split('-', 2);
  const [cb, sb] = b.replace(/^v/, '').split('-', 2);
  const pa = ca.split('.').map(Number);
  const pb = cb.split('.').map(Number);
  const len = Math.max(pa.length, pb.length);
  for (let i = 0; i < len; i++) {
    const na = pa[i] ?? 0;
    const nb = pb[i] ?? 0;
    if (na > nb) return 1;
    if (na < nb) return -1;
  }
  if (sa === undefined && sb !== undefined) return 1;
  if (sa !== undefined && sb === undefined) return -1;
  return 0;
}

export interface GithubRelease {
  tag_name: string;
  draft: boolean;
  prerelease: boolean;
  assets: Array<{ name: string; browser_download_url: string }>;
}

const VERSION_TAG = /^v?(\d+)\.(\d+)\.(\d+)$/;

/** Newest release with the running major version; 3.x installs beside 2.x, so neither takes the other's.
 *  3.x ships as prereleases while on trial, so only 3.x accepts them. */
export function pickRelease(releases: GithubRelease[], currentVersion: string): GithubRelease | null {
  const major = currentVersion.replace(/^v/, '').split('.')[0];
  const allowPrerelease = Number(major) >= 3;
  let best: GithubRelease | null = null;
  for (const r of releases) {
    const m = VERSION_TAG.exec(r.tag_name);
    if (r.draft || (r.prerelease && !allowPrerelease) || !m || m[1] !== major) continue;
    // A release still uploading its assets must not beat a complete older one.
    const hasUpdateZip = r.assets.some((a) => a.name.endsWith('-update.zip'));
    const hasFullZip = r.assets.some((a) => a.name.endsWith('.zip') && !a.name.endsWith('-update.zip'));
    if (!hasUpdateZip || !hasFullZip) continue;
    if (!best || compareVersions(r.tag_name, best.tag_name) > 0) best = r;
  }
  return best;
}

export interface FullUpdateHooks {
  /** A test encode with the configured encoder on the installed ffmpeg; the health check compares against it. */
  baseline: () => Promise<UpdateState['baseline']>;
  /** Stops Node's own timers and the stream, and waits until nothing runs from the folders being replaced. */
  stop: (folders: string[], version: string) => Promise<StopResult>;
  /** Starts Node's own services again after a stop that failed. */
  resume: () => void;
  /** Posts the failure copy of an update this Node called off. */
  reportFailure: (message: string) => void;
}

const IN_PROGRESS: readonly UpdateStatus[] = ['downloading', 'extracting', 'ready'];

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
  private nodeNotice: string | null = null;
  private kind: UpdateKind | null = null;
  private parts: Part[] = [];
  private sameVersion = false;
  private installParts: () => Promise<InstallParts | null>;
  private rootDir: string;
  private full: FullUpdateHooks | null;
  private testReleases: string | null;

  constructor(opts: {
    isDevMode?: boolean;
    installParts?: () => Promise<InstallParts | null>;
    /** The install root that holds app\, _update_tmp\ and data\; the parent of app\ unless a test names one. */
    rootDir?: string;
    full?: FullUpdateHooks;
    /** Test runs only (STREAMLOOP_TEST_RELEASES): the release list is read from this file, never from GitHub. */
    testReleases?: string | null;
  } = {}) {
    this.installParts = opts.installParts ?? (async () => null);
    this.full = opts.full ?? null;
    this.testReleases = opts.testReleases ?? null;
    // Read version from package.json
    const require = createRequire(import.meta.url);
    const pkg = require('../../package.json');
    this.currentVersion = pkg.version;

    // Dev mode: no sibling node/ directory means we're in dev
    const appDir = resolve(__dirname, '..', '..');
    const nodeDir = resolve(appDir, '..', 'node');
    this.isDevMode = opts.isDevMode ?? !existsSync(nodeDir);
    this.rootDir = opts.rootDir ?? resolve(appDir, '..');
  }

  getStatus(): UpdateInfo {
    return {
      currentVersion: this.currentVersion,
      latestVersion: this.latestVersion,
      updateAvailable: this.updateAvailable,
      status: this.status,
      error: this.error,
      isDevMode: this.isDevMode,
      nodeNotice: this.nodeNotice,
      kind: this.kind,
      parts: this.parts,
      sameVersion: this.sameVersion,
    };
  }

  async checkForUpdate(): Promise<UpdateInfo> {
    // A check resets the offer that an update being applied still reads.
    if (IN_PROGRESS.includes(this.status)) return this.getStatus();
    // 60-second cooldown
    const now = Date.now();
    if (now - this.lastCheckTime < 60_000 && this.latestVersion !== null) {
      return this.getStatus();
    }

    this.status = 'checking';
    this.error = null;
    this.nodeNotice = null;
    this.kind = null;
    this.parts = [];
    this.sameVersion = false;

    try {
      let releases: GithubRelease[];
      if (this.testReleases) {
        releases = JSON.parse(readFileSync(this.testReleases, 'utf8')) as GithubRelease[];
      } else {
        const res = await fetch(GITHUB_API_URL, {
          headers: {
            'Accept': 'application/vnd.github.v3+json',
            'User-Agent': `streamloop/${this.currentVersion}`,
          },
          signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
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

        releases = await res.json() as GithubRelease[];
      }
      const release = pickRelease(releases, this.currentVersion);
      this.latestVersion = release ? release.tag_name.replace(/^v/, '') : this.currentVersion;
      this.updateAvailable = release !== null && compareVersions(this.latestVersion, this.currentVersion) > 0;

      // Nothing newer: the running version's own release may still carry parts this install lacks.
      if (!this.updateAvailable && !this.isDevMode) {
        const current = releases.find((r) => !r.draft && r.tag_name.replace(/^v/, '') === this.currentVersion);
        const full = current ? selectReleaseAssets(current.assets, 'full') : null;
        // Only a release that can be installed is offered; one still uploading is not.
        const installable = full !== null && full.zipUrl !== null && this.allowed(full.zipUrl)
          && full.checksumUrl !== null && this.allowed(full.checksumUrl);
        const decision = current && installable ? await this.decideKind(current) : null;
        if (current && decision && decision.kind === 'full') {
          this.applyFullOffer(current, decision.parts);
          this.sameVersion = true;
          this.status = 'idle';
          this.lastCheckTime = now;
          return this.getStatus();
        }
      }
      if (!release) {
        this.status = 'idle';
        this.lastCheckTime = now;
        return this.getStatus();
      }

      // Pick the ZIP (slim -update.zip preferred) + its matching checksum,
      // then validate URLs against allowed domains.
      if (this.updateAvailable) {
        // Dev mode has no install to compare and never applies updates.
        const decision = this.isDevMode ? null : await this.decideKind(release);
        if (decision && decision.kind === 'full') {
          this.applyFullOffer(release, decision.parts);
        } else {
          this.selectAssets(release, 'app');
          // Dev mode runs the system Node, which no release bundles. A release with
          // components.json names its changed parts instead of a Node notice.
          const releaseNode = this.isDevMode || decision?.hasComponents ? null : await this.fetchReleaseNodeVersion(release);
          if (releaseNode && releaseNode !== process.versions.node) {
            this.nodeNotice = `This release bundles Node ${releaseNode}. Install the full ZIP to get it.`;
          }
        }
      }

      this.status = 'idle';
      this.lastCheckTime = now;
      logger.info({ currentVersion: this.currentVersion, latestVersion: this.latestVersion, updateAvailable: this.updateAvailable }, 'Update check complete');
    } catch (err) {
      this.status = 'error';
      this.error = err instanceof Error && err.name === 'TimeoutError'
        ? 'GitHub did not answer within 10 seconds.'
        : err instanceof Error ? err.message : String(err);
      logger.error({ err }, 'Update check failed');
    }

    return this.getStatus();
  }

  private allowed(url: string): boolean {
    return isAllowedDownloadUrl(url, Boolean(this.testReleases));
  }

  private selectAssets(release: GithubRelease, kind: UpdateKind): void {
    const { zipUrl, checksumUrl } = selectReleaseAssets(release.assets, kind);
    if (zipUrl && !this.allowed(zipUrl)) {
      throw new Error(`Untrusted download URL domain: ${zipUrl}`);
    }
    this.releaseAssetUrl = zipUrl;
    this.releaseChecksumUrl = checksumUrl && this.allowed(checksumUrl) ? checksumUrl : null;
    this.kind = kind;
  }

  private applyFullOffer(release: GithubRelease, parts: Part[]): void {
    this.selectAssets(release, 'full');
    this.parts = parts;
    this.updateAvailable = true;
    this.latestVersion = release.tag_name.replace(/^v/, '');
  }

  private async decideKind(
    release: GithubRelease,
  ): Promise<{ kind: UpdateKind; parts: Part[]; hasComponents: boolean }> {
    let install: InstallParts | null = null;
    try {
      install = await this.installParts();
    } catch (err) {
      logger.warn({ err }, 'Could not read the installed parts');
    }
    const components = await this.fetchComponents(release);
    return { ...updateKind(install, components), hasComponents: components !== null };
  }

  private async fetchComponents(release: GithubRelease): Promise<ReleaseComponents | null> {
    const url = release.assets.find((a) => a.name === 'components.json')?.browser_download_url;
    if (!url || !this.allowed(url)) return null;
    try {
      const res = await fetch(url, {
        headers: { 'User-Agent': `streamloop/${this.currentVersion}` },
        signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
      });
      return res.ok ? parseComponents(await res.text()) : null;
    } catch (err) {
      logger.warn({ err }, 'Could not read the release components');
      return null;
    }
  }

  private async fetchReleaseNodeVersion(release: GithubRelease): Promise<string | null> {
    const url = release.assets.find((a) => a.name === 'node-version.txt')?.browser_download_url;
    if (!url || !this.allowed(url)) return null;
    try {
      const res = await fetch(url, {
        headers: { 'User-Agent': `streamloop/${this.currentVersion}` },
        signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
      });
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

    if (IN_PROGRESS.includes(this.status)) {
      throw new Error('Update already in progress');
    }

    const checksumUrl = this.releaseChecksumUrl;
    if (!checksumUrl) {
      this.status = 'error';
      this.error = 'This release has no checksum file, so it was not installed.';
      throw new Error(this.error);
    }

    // Read once: a check during the download must not change what is being installed.
    const zipUrl = this.releaseAssetUrl;
    const full = this.kind === 'full' ? this.full : null;
    if (this.kind === 'full' && !full) throw new Error('A full update cannot run here');
    const parts = [...this.parts];
    const folders = full ? foldersFor(parts) : [];
    const to = this.latestVersion ?? this.currentVersion;

    const rootDir = this.rootDir;
    const tmpDir = join(rootDir, '_update_tmp');
    const oldDir = join(rootDir, '_update_old');

    this.status = 'downloading';
    this.error = null;
    let state: UpdateState | null = null;
    const fail = (err: unknown): void => {
      this.status = 'error';
      this.error = err instanceof Error ? err.message : String(err);
      try {
        if (existsSync(tmpDir)) rmSync(tmpDir, { recursive: true, force: true });
      } catch { /* best effort */ }
      if (!state) return;
      const failed: UpdateState = { ...state, result: 'failed', reason: this.error };
      try {
        writeUpdateState(rootDir, failed);
        full?.reportFailure(failureMessage(failed));
      } catch (writeErr) {
        logger.error({ err: writeErr }, 'Could not record the failed update');
      }
    };

    try {
      if (full) {
        const at = new Date().toISOString();
        state = { from: this.currentVersion, to, parts, folders, step: 'download', result: 'running', reason: null, startedAt: at, stepStartedAt: at, baseline: null };
        writeUpdateState(rootDir, state);
      }

      // Clean up any previous update artifacts
      if (existsSync(tmpDir)) rmSync(tmpDir, { recursive: true, force: true });
      if (existsSync(oldDir)) rmSync(oldDir, { recursive: true, force: true });
      if (full) clearLeftovers(rootDir);
      mkdirSync(tmpDir, { recursive: true });

      // While the stream is still on air, so the check after the swap has something to compare with.
      if (full && state) {
        state = { ...state, baseline: await full.baseline() };
        writeUpdateState(rootDir, state);
      }

      // Download
      logger.info({ url: zipUrl, kind: this.kind }, 'Downloading update');

      const zipPath = join(tmpDir, 'update.zip');
      const res = await fetch(zipUrl, {
        headers: { 'User-Agent': `streamloop/${this.currentVersion}` },
      });
      if (!res.ok) throw new Error(`Download failed: ${res.status} ${res.statusText}`);
      if (!res.body) throw new Error('No response body');

      const fileStream = createWriteStream(zipPath);
      await pipeline(res.body as unknown as NodeJS.ReadableStream, fileStream);

      logger.info('Verifying update checksum');
      const checksumRes = await fetch(checksumUrl, {
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

      // Extract
      this.status = 'extracting';
      logger.info('Extracting update');
      const extractDir = join(tmpDir, 'extracted');
      mkdirSync(extractDir, { recursive: true });
      execFileSync('C:\\Windows\\System32\\tar.exe', ['-xf', zipPath, '-C', extractDir], { cwd: SPAWN_CWD, windowsHide: true, timeout: 600_000 });

      stageUpdate(extractDir, tmpDir, folders);

      this.status = 'ready';
      logger.info({ from: this.currentVersion, to, folders }, 'Update staged, restart required');
    } catch (err) {
      logger.error({ err }, 'Update failed');
      fail(err);
      throw err;
    }
    if (!full || !state) return;

    // The stream goes off air from here; until the swap nothing is replaced, so a failure keeps the old version running.
    let stopping = false;
    try {
      state = { ...state, step: 'stop', stepStartedAt: new Date().toISOString() };
      writeUpdateState(rootDir, state);
      stopping = true;
      const stopped = await full.stop(folders, to);
      if (!stopped.ok) throw new Error(stopped.reason);
      state = { ...state, step: 'swap', stepStartedAt: new Date().toISOString() };
      writeUpdateState(rootDir, state);
      logger.info({ to, folders }, 'Stream stopped for the swap');
    } catch (err) {
      logger.error({ err }, 'The full update was called off before the swap');
      fail(err);
      if (stopping) full.resume();
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
