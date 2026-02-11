/**
 * prepare-release.js
 *
 * Assembles a distributable ZIP for Windows:
 *   1. Downloads portable Node.js (win-x64)
 *   2. Copies application source files
 *   3. Installs production dependencies
 *   4. Packages everything into a ZIP
 *
 * Usage: node build/prepare-release.js
 */

import { execSync } from 'child_process';
import { mkdirSync, cpSync, existsSync, rmSync, writeFileSync, readFileSync, createWriteStream } from 'fs';
import { join, resolve } from 'path';
import { pipeline } from 'stream/promises';

const NODE_VERSION = '22.12.0';
const NODE_ARCH = 'win-x64';
const NODE_URL = `https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-${NODE_ARCH}.zip`;

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

  // Step 2: Copy application files
  console.log('Copying application files...');
  const appDir = join(RELEASE, 'app');

  // Copy source directories
  cpSync(join(ROOT, 'src'), join(appDir, 'src'), { recursive: true });

  // Copy package files (never copy config.json — use the example template instead)
  for (const file of ['package.json', 'package-lock.json', 'tsconfig.json']) {
    const src = join(ROOT, file);
    if (existsSync(src)) {
      cpSync(src, join(appDir, file));
    }
  }
  cpSync(join(ROOT, 'config.example.json'), join(appDir, 'config.json'));

  // Step 3: Install production dependencies
  console.log('Installing production dependencies...');
  const nodeExe = join(RELEASE, 'node', 'node.exe');
  const npmCmd = join(RELEASE, 'node', 'npm.cmd');
  execSync(`"${npmCmd}" install --omit=dev`, { cwd: appDir, stdio: 'inherit' });

  // tsx is a devDependency but needed at runtime for the portable bundle
  execSync(`"${npmCmd}" install tsx@4`, { cwd: appDir, stdio: 'inherit' });

  // Step 4: Copy scripts
  console.log('Copying scripts...');
  cpSync(join(ROOT, 'scripts', 'START.bat'), join(RELEASE, 'START.bat'));
  cpSync(join(ROOT, 'scripts', 'README.txt'), join(RELEASE, 'README.txt'));

  // Step 5: Create ZIP
  console.log('Creating ZIP archive...');
  const pkg = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf-8'));
  const zipPath = join(DIST, `streamloop-v${pkg.version}.zip`);
  execSync(`powershell -Command "Compress-Archive -Path '${RELEASE}' -DestinationPath '${zipPath}' -Force"`, { stdio: 'inherit' });

  console.log(`\nRelease built successfully!`);
  console.log(`  ZIP: ${zipPath}`);
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
