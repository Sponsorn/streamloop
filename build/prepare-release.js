/**
 * prepare-release.js
 *
 * Assembles a distributable ZIP for Windows:
 *   1. Downloads portable Node.js (win-x64)
 *   2. Downloads yt-dlp.exe
 *   3. Bundles mpv.exe (must be pre-placed in build/mpv/mpv.exe)
 *   4. Copies application source files
 *   5. Installs production dependencies
 *   6. Packages everything into a ZIP
 *
 * Usage: node build/prepare-release.js
 *
 * Note: mpv.exe must be manually placed in build/mpv/mpv.exe before building.
 * Download from: https://github.com/shinchiro/mpv-winbuild-cmake/releases
 * (mpv releases use .7z archives which are complex to extract in a build script)
 */

import { execSync } from 'child_process';
import { createHash } from 'crypto';
import { mkdirSync, cpSync, existsSync, rmSync, writeFileSync, readFileSync, createWriteStream } from 'fs';
import { join, resolve } from 'path';
import { pipeline } from 'stream/promises';

const NODE_VERSION = '22.12.0';
const NODE_ARCH = 'win-x64';
const NODE_URL = `https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-${NODE_ARCH}.zip`;
const YTDLP_URL = 'https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe';

const ROOT = resolve(import.meta.dirname, '..');
const DIST = join(ROOT, 'dist');
const RELEASE = join(DIST, 'streamloop');

async function main() {
  console.log('=== StreamLoop Release Builder ===\n');

  // Clean previous build
  if (existsSync(DIST)) {
    console.log('Cleaning previous dist...');
    rmSync(DIST, { recursive: true, force: true });
  }

  // Create release directory structure
  mkdirSync(join(RELEASE, 'app', 'src'), { recursive: true });
  mkdirSync(join(RELEASE, 'node'), { recursive: true });

  // Step 1: Download portable Node.js
  console.log(`Downloading Node.js v${NODE_VERSION} (${NODE_ARCH})...`);
  const nodeZip = join(DIST, 'node.zip');
  await downloadFile(NODE_URL, nodeZip);

  console.log('Extracting Node.js...');
  execSync(`powershell -Command "Expand-Archive -Path '${nodeZip}' -DestinationPath '${DIST}' -Force"`, { stdio: 'inherit' });

  // Move node files to release/node/
  const extractedNodeDir = join(DIST, `node-v${NODE_VERSION}-${NODE_ARCH}`);
  cpSync(extractedNodeDir, join(RELEASE, 'node'), { recursive: true });

  // Step 2: Download yt-dlp
  console.log('Downloading yt-dlp...');
  mkdirSync(join(RELEASE, 'yt-dlp'), { recursive: true });
  await downloadFile(YTDLP_URL, join(RELEASE, 'yt-dlp', 'yt-dlp.exe'));

  // Step 3: Bundle mpv
  console.log('Bundling mpv...');
  mkdirSync(join(RELEASE, 'mpv'), { recursive: true });
  const localMpv = join(ROOT, 'build', 'mpv', 'mpv.exe');
  if (existsSync(localMpv)) {
    cpSync(localMpv, join(RELEASE, 'mpv', 'mpv.exe'));
    console.log('  Copied mpv.exe from build/mpv/');
  } else {
    console.warn('  WARNING: mpv.exe not found at build/mpv/mpv.exe');
    console.warn('  Download from: https://github.com/shinchiro/mpv-winbuild-cmake/releases');
    console.warn('  Place mpv.exe in build/mpv/ and re-run this script');
  }

  // Write default mpv.conf
  writeFileSync(join(RELEASE, 'mpv', 'mpv.conf'), [
    'no-border',
    'no-osc',
    'osd-level=0',
    'hwdec=auto',
    'ytdl-format=bestvideo[height<=?1080]+bestaudio/best',
    'ytdl-raw-options=yes-playlist=',
    'loop-playlist=inf',
    'keep-open=yes',
  ].join('\n'));
  console.log('  Wrote default mpv.conf');

  // Step 4: Copy application files
  console.log('Copying application files...');
  const appDir = join(RELEASE, 'app');

  // Copy source directories (exclude test files)
  cpSync(join(ROOT, 'src'), join(appDir, 'src'), {
    recursive: true,
    filter: (src) => !src.includes('__tests__'),
  });

  // Copy package files (never copy config.json — use the example template instead)
  for (const file of ['package.json', 'package-lock.json', 'tsconfig.json']) {
    const src = join(ROOT, file);
    if (existsSync(src)) {
      cpSync(src, join(appDir, file));
    }
  }
  cpSync(join(ROOT, 'config.example.json'), join(appDir, 'config.json'));

  // Step 5: Install production dependencies
  console.log('Installing production dependencies...');
  const nodeExe = join(RELEASE, 'node', 'node.exe');
  const npmCmd = join(RELEASE, 'node', 'npm.cmd');
  execSync(`"${npmCmd}" install --omit=dev`, { cwd: appDir, stdio: 'inherit' });

  // tsx is a devDependency but needed at runtime for the portable bundle
  execSync(`"${npmCmd}" install tsx@4`, { cwd: appDir, stdio: 'inherit' });

  // Step 6: Copy scripts
  console.log('Copying scripts...');
  cpSync(join(ROOT, 'scripts', 'START.bat'), join(RELEASE, 'START.bat'));
  cpSync(join(ROOT, 'scripts', 'README.txt'), join(RELEASE, 'README.txt'));

  // Step 7: Create ZIP (retry up to 3 times — antivirus may lock newly created files)
  console.log('Creating ZIP archive...');
  const pkg = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf-8'));
  const zipPath = join(DIST, `streamloop-v${pkg.version}.zip`);
  for (let attempt = 1; attempt <= 3; attempt++) {
    try {
      // Wait for antivirus to finish scanning new files
      if (attempt > 1) console.log(`  Retry ${attempt}/3 after 10s delay...`);
      await new Promise(r => setTimeout(r, attempt === 1 ? 5000 : 10000));
      execSync(`powershell -Command "Compress-Archive -Path '${RELEASE}' -DestinationPath '${zipPath}' -Force"`, { stdio: 'inherit' });
      if (existsSync(zipPath)) break;
    } catch (err) {
      if (attempt === 3) throw err;
      console.warn(`  ZIP attempt ${attempt} failed, retrying...`);
      if (existsSync(zipPath)) rmSync(zipPath, { force: true });
    }
  }

  // Step 8: Generate SHA-256 checksum
  console.log('Generating SHA-256 checksum...');
  const zipData = readFileSync(zipPath);
  const hash = createHash('sha256').update(zipData).digest('hex');
  const zipFilename = `streamloop-v${pkg.version}.zip`;
  const checksumPath = join(DIST, `streamloop-v${pkg.version}.sha256`);
  writeFileSync(checksumPath, `${hash}  ${zipFilename}\n`);

  console.log(`\nRelease built successfully!`);
  console.log(`  ZIP: ${zipPath}`);
  console.log(`  SHA-256: ${checksumPath}`);
  console.log(`  Dir: ${RELEASE}`);
}

async function downloadFile(url, dest) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`Failed to download: ${res.status} ${res.statusText}`);
  const fileStream = createWriteStream(dest);
  await pipeline(res.body, fileStream);
}

main().catch((err) => {
  console.error('Build failed:', err);
  process.exit(1);
});
