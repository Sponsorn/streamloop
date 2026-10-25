/**
 * prepare-release.js
 *
 * Builds the 3.x release ZIPs in the install layout of supervisor/src/paths.rs
 * (listed in build/release-layout.js):
 *
 *   node build/prepare-release.js          full ZIP for installs: node\, app\, supervisor\,
 *                                          ffmpeg\, yt-dlp\ (with deno and the PO-token
 *                                          plugin), pot\ (the token server), data\, START.bat
 *   node build/prepare-release.js --slim   update ZIP (streamloop-vX.Y.Z-update.zip): app\ and
 *                                          START.bat only, the parts the updater swaps
 *
 * Both write NODE_VERSION to app\node-version.txt and dist\node-version.txt (a release
 * asset), a .sha256 per ZIP, and run verify-release.mjs on the ZIP.
 *
 * The full build needs cargo (it builds the supervisor) and spikes/buffer/media/slate.mp4.
 * ffmpeg, the token server source and the yt-dlp plugin are pinned by SHA-256; a mismatch
 * fails the build.
 */

import { execSync, execFileSync } from 'child_process';
import { createHash } from 'crypto';
import { mkdirSync, cpSync, existsSync, rmSync, writeFileSync, readFileSync, createReadStream, createWriteStream } from 'fs';
import { join, resolve } from 'path';
import { pipeline } from 'stream/promises';

const NODE_VERSION = '22.23.4';
const NODE_ARCH = 'win-x64';
const NODE_URL = `https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-${NODE_ARCH}.zip`;
const YTDLP_URL = 'https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download/yt-dlp.exe';
const DENO_URL = 'https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip';

const FFMPEG = {
  url: 'https://github.com/GyanD/codexffmpeg/releases/download/8.0/ffmpeg-8.0-essentials_build.zip',
  sha256: '647e467caf82b9fa200a562769b5ff4d736aaf725804ed2c64ea9752106fa569',
  exe: 'ffmpeg-8.0-essentials_build/bin/ffmpeg.exe',
};
const POT_TAG = '2.0.0';
const POT_SERVER = {
  url: `https://github.com/Brainicism/bgutil-ytdlp-pot-provider/archive/refs/tags/${POT_TAG}.tar.gz`,
  sha256: '47038f7e0556a3d689044460f1b6d78381212e1bd04723a27358e1a42279f866',
  dir: `bgutil-ytdlp-pot-provider-${POT_TAG}/server`,
};
const POT_PLUGIN = {
  url: `https://github.com/Brainicism/bgutil-ytdlp-pot-provider/releases/download/${POT_TAG}/bgutil-ytdlp-pot-provider.zip`,
  sha256: 'bce874dfa25896c2798e0f4f8147b7b22e785479eb1e459ab232bf2506c95016',
};

const TAR = 'C:\\Windows\\System32\\tar.exe';
const ROOT = resolve(import.meta.dirname, '..');
const DIST = join(ROOT, 'dist');
const RELEASE = join(DIST, 'streamloop');
const WORK = join(DIST, 'work');

async function main() {
  const SLIM = process.argv.includes('--slim');
  console.log(`=== StreamLoop Release Builder ${SLIM ? '(update ZIP)' : '(full)'} ===\n`);

  const pkg = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf-8'));
  const suffix = SLIM ? '-update' : '';
  const zipFilename = `streamloop-v${pkg.version}${suffix}.zip`;
  const checksumFilename = `streamloop-v${pkg.version}${suffix}.sha256`;

  // Only this mode's outputs go, so the full and update builds can run back to back.
  mkdirSync(DIST, { recursive: true });
  for (const p of [RELEASE, WORK, join(DIST, zipFilename), join(DIST, checksumFilename), join(DIST, 'node-version.txt')]) {
    rmSync(p, { recursive: true, force: true });
  }
  mkdirSync(WORK, { recursive: true });

  // The full build runs npm under the bundled Node, first on PATH, so canvas installs for its ABI.
  let npm = { cmd: 'npm', env: process.env };
  if (!SLIM) {
    console.log(`Node.js v${NODE_VERSION}...`);
    const nodeZip = join(WORK, 'node.zip');
    await downloadFile(NODE_URL, nodeZip);
    execFileSync(TAR, ['-xf', nodeZip, '-C', WORK], { stdio: 'inherit', windowsHide: true });
    cpSync(join(WORK, `node-v${NODE_VERSION}-${NODE_ARCH}`), join(RELEASE, 'node'), { recursive: true });
    const nodeDir = join(RELEASE, 'node');
    npm = { cmd: `"${join(nodeDir, 'npm.cmd')}"`, env: { ...process.env, PATH: `${nodeDir};${process.env.PATH}` } };
  }

  console.log('app\\...');
  const appDir = join(RELEASE, 'app');
  cpSync(join(ROOT, 'src'), join(appDir, 'src'), { recursive: true, filter: (src) => !src.includes('__tests__') });
  for (const file of ['package.json', 'package-lock.json', 'tsconfig.json']) {
    if (existsSync(join(ROOT, file))) cpSync(join(ROOT, file), join(appDir, file));
  }
  // never the real config.json; START.bat carries the installed one across updates
  cpSync(join(ROOT, 'config.example.json'), join(appDir, 'config.json'));
  writeFileSync(join(appDir, 'node-version.txt'), `${NODE_VERSION}\n`);
  writeFileSync(join(DIST, 'node-version.txt'), `${NODE_VERSION}\n`);
  execSync(`${npm.cmd} install --omit=dev`, { cwd: appDir, env: npm.env, stdio: 'inherit', windowsHide: true });
  // tsx is a devDependency, but START.bat runs the server through it
  execSync(`${npm.cmd} install tsx@4`, { cwd: appDir, env: npm.env, stdio: 'inherit', windowsHide: true });

  if (!SLIM) {
    console.log('supervisor\\...');
    execSync('cargo build --release', { cwd: join(ROOT, 'supervisor'), stdio: 'inherit', windowsHide: true });
    mkdirSync(join(RELEASE, 'supervisor'));
    cpSync(join(ROOT, 'supervisor', 'target', 'release', 'streamloop-supervisor.exe'), join(RELEASE, 'supervisor', 'streamloop-supervisor.exe'));
    cpSync(join(ROOT, 'spikes', 'buffer', 'media', 'slate.mp4'), join(RELEASE, 'supervisor', 'slate.mp4'));

    console.log('ffmpeg\\...');
    const ffmpegZip = join(WORK, 'ffmpeg.zip');
    await downloadPinned(FFMPEG, ffmpegZip);
    execFileSync(TAR, ['-xf', ffmpegZip, '-C', WORK, FFMPEG.exe], { stdio: 'inherit', windowsHide: true });
    mkdirSync(join(RELEASE, 'ffmpeg'));
    cpSync(join(WORK, FFMPEG.exe), join(RELEASE, 'ffmpeg', 'ffmpeg.exe'));

    // yt-dlp nightly tracks YouTube's challenge changes faster than stable; deno solves
    // the n-param challenge; the plugin fetches PO tokens from pot\.
    console.log('yt-dlp\\...');
    const ytdlpDir = join(RELEASE, 'yt-dlp');
    mkdirSync(join(ytdlpDir, 'plugins'), { recursive: true });
    await downloadFile(YTDLP_URL, join(ytdlpDir, 'yt-dlp.exe'));
    const denoZip = join(WORK, 'deno.zip');
    await downloadFile(DENO_URL, denoZip);
    execFileSync(TAR, ['-xf', denoZip, '-C', ytdlpDir], { stdio: 'inherit', windowsHide: true });
    await downloadPinned(POT_PLUGIN, join(ytdlpDir, 'plugins', 'bgutil-ytdlp-pot-provider.zip'));

    // upstream has no build script; its Dockerfile and README build with tsc
    console.log('pot\\...');
    const potTar = join(WORK, 'pot.tar.gz');
    await downloadPinned(POT_SERVER, potTar);
    execFileSync(TAR, ['-xzf', potTar, '-C', WORK, POT_SERVER.dir], { stdio: 'inherit', windowsHide: true });
    const potSrc = join(WORK, POT_SERVER.dir);
    execSync(`${npm.cmd} ci --no-audit --no-fund`, { cwd: potSrc, env: npm.env, stdio: 'inherit', windowsHide: true });
    execFileSync(join(RELEASE, 'node', 'node.exe'), [join('node_modules', 'typescript', 'bin', 'tsc')], { cwd: potSrc, stdio: 'inherit', windowsHide: true });
    execSync(`${npm.cmd} prune --omit=dev --no-audit --no-fund`, { cwd: potSrc, env: npm.env, stdio: 'inherit', windowsHide: true });
    for (const part of ['build', 'node_modules', 'package.json']) {
      cpSync(join(potSrc, part), join(RELEASE, 'pot', part), { recursive: true });
    }

    mkdirSync(join(RELEASE, 'data'));
    cpSync(join(ROOT, 'scripts', 'README.txt'), join(RELEASE, 'README.txt'));
  }

  cpSync(join(ROOT, 'scripts', 'START.bat'), join(RELEASE, 'START.bat'));

  // antivirus may still hold freshly written files, so the ZIP is retried
  console.log('Creating ZIP...');
  const zipPath = join(DIST, zipFilename);
  for (let attempt = 1; attempt <= 3; attempt++) {
    try {
      await new Promise((r) => setTimeout(r, attempt === 1 ? 5000 : 10000));
      execFileSync(TAR, ['-acf', zipPath, '-C', DIST, 'streamloop'], { stdio: 'inherit', windowsHide: true });
      if (existsSync(zipPath)) break;
    } catch (err) {
      if (attempt === 3) throw err;
      console.warn(`  ZIP attempt ${attempt} failed, retrying...`);
      rmSync(zipPath, { force: true });
    }
  }

  const checksumPath = join(DIST, checksumFilename);
  writeFileSync(checksumPath, `${await sha256File(zipPath)}  ${zipFilename}\n`);
  rmSync(WORK, { recursive: true, force: true });

  execFileSync(process.execPath, [join(ROOT, 'build', 'verify-release.mjs'), zipPath], { stdio: 'inherit', windowsHide: true });

  console.log('\nRelease built:');
  console.log(`  ZIP: ${zipPath}`);
  console.log(`  SHA-256: ${checksumPath}`);
  console.log(`  Node version asset: ${join(DIST, 'node-version.txt')}`);
}

async function downloadFile(url, dest) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`Failed to download ${url}: ${res.status} ${res.statusText}`);
  await pipeline(res.body, createWriteStream(dest));
}

async function downloadPinned({ url, sha256 }, dest) {
  await downloadFile(url, dest);
  const actual = await sha256File(dest);
  if (actual !== sha256) {
    rmSync(dest, { force: true });
    throw new Error(`Checksum mismatch for ${url}\n  expected ${sha256}\n  actual   ${actual}`);
  }
}

async function sha256File(path) {
  const hash = createHash('sha256');
  await pipeline(createReadStream(path), hash);
  return hash.digest('hex');
}

main().catch((err) => {
  console.error('Build failed:', err);
  process.exit(1);
});
