/**
 * prepare-release.js
 *
 * node build/prepare-release.js writes, in dist\:
 *
 *   streamloop-vX.Y.Z.zip           the install: streamloop.exe, app\ (config.example.json
 *                                   only), supervisor\slate.mp4 and supervisor\streamloop-pot.exe (the
 *                                   token server), ffmpeg\, yt-dlp\ (with deno and the PO-token plugin),
 *                                   data\, START.bat. No node\ and no pot\.
 *   streamloop-vX.Y.Z.exe           the same streamloop.exe, which the updater downloads
 *   streamloop-vX.Y.Z.sha256        <hash>  <file name> for the ZIP
 *   streamloop-vX.Y.Z.exe.sha256    the same for the exe
 *
 * It then runs verify-release.mjs on the ZIP. The build needs cargo for both Cargo projects (the root
 * workspace and tokenserver/, which links V8 and has its own static-CRT config) and
 * spikes/buffer/media/slate.mp4. ffmpeg and the yt-dlp plugin are pinned by SHA-256; a mismatch fails the build.
 */

import { execSync, execFileSync } from 'child_process';
import { createHash } from 'crypto';
import { mkdirSync, cpSync, existsSync, rmSync, writeFileSync, readFileSync, createReadStream, createWriteStream } from 'fs';
import { join, resolve } from 'path';
import { pipeline } from 'stream/promises';

const YTDLP_URL = 'https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download/yt-dlp.exe';
const DENO_URL = 'https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip';

const FFMPEG = {
  url: 'https://github.com/GyanD/codexffmpeg/releases/download/8.0/ffmpeg-8.0-essentials_build.zip',
  sha256: '647e467caf82b9fa200a562769b5ff4d736aaf725804ed2c64ea9752106fa569',
  exe: 'ffmpeg-8.0-essentials_build/bin/ffmpeg.exe',
};
const POT_TAG = '2.0.0';
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
  console.log('=== StreamLoop Release Builder ===\n');
  try {
    execSync('cargo --version', { stdio: 'ignore', windowsHide: true });
  } catch {
    throw new Error('Rust (cargo) is needed for the build');
  }

  const pkg = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf-8'));
  const base = `streamloop-v${pkg.version}`;
  const zipFilename = `${base}.zip`;
  const exeFilename = `${base}.exe`;
  const outputs = [zipFilename, `${base}.sha256`, exeFilename, `${exeFilename}.sha256`].map((f) => join(DIST, f));

  mkdirSync(DIST, { recursive: true });
  for (const p of [RELEASE, WORK, ...outputs]) rmSync(p, { recursive: true, force: true });
  mkdirSync(WORK, { recursive: true });

  console.log('streamloop.exe...');
  execSync('cargo build --release -p streamloop', { cwd: ROOT, stdio: 'inherit', windowsHide: true });
  const builtExe = join(ROOT, 'target', 'release', 'streamloop.exe');
  cpSync(builtExe, join(RELEASE, 'streamloop.exe'));
  cpSync(builtExe, join(DIST, exeFilename));

  // tokenserver/ is no workspace member: its .cargo/config.toml (static CRT) applies only from its own folder.
  console.log('streamloop-pot.exe...');
  execSync('cargo build --release', { cwd: join(ROOT, 'tokenserver'), stdio: 'inherit', windowsHide: true });
  const builtPot = join(ROOT, 'tokenserver', 'target', 'release', 'streamloop-pot.exe');

  // no config.json: the control process creates it from the example on first start
  console.log('app\\...');
  mkdirSync(join(RELEASE, 'app'));
  cpSync(join(ROOT, 'config.example.json'), join(RELEASE, 'app', 'config.example.json'));

  console.log('supervisor\\...');
  mkdirSync(join(RELEASE, 'supervisor'));
  cpSync(join(ROOT, 'spikes', 'buffer', 'media', 'slate.mp4'), join(RELEASE, 'supervisor', 'slate.mp4'));
  cpSync(builtPot, join(RELEASE, 'supervisor', 'streamloop-pot.exe'));

  console.log('ffmpeg\\...');
  const ffmpegZip = join(WORK, 'ffmpeg.zip');
  await downloadPinned(FFMPEG, ffmpegZip);
  execFileSync(TAR, ['-xf', ffmpegZip, '-C', WORK, FFMPEG.exe], { stdio: 'inherit', windowsHide: true });
  mkdirSync(join(RELEASE, 'ffmpeg'));
  cpSync(join(WORK, FFMPEG.exe), join(RELEASE, 'ffmpeg', 'ffmpeg.exe'));

  // yt-dlp nightly tracks YouTube's challenge changes faster than stable; deno solves
  // the n-param challenge; the plugin fetches PO tokens from the token server.
  console.log('yt-dlp\\...');
  const ytdlpDir = join(RELEASE, 'yt-dlp');
  mkdirSync(join(ytdlpDir, 'plugins'), { recursive: true });
  await downloadFile(YTDLP_URL, join(ytdlpDir, 'yt-dlp.exe'));
  const denoZip = join(WORK, 'deno.zip');
  await downloadFile(DENO_URL, denoZip);
  execFileSync(TAR, ['-xf', denoZip, '-C', ytdlpDir], { stdio: 'inherit', windowsHide: true });
  // unpinned "latest" downloads; the log records what shipped
  for (const exe of ['yt-dlp.exe', 'deno.exe']) console.log(`  ${exe} sha256 ${await sha256File(join(ytdlpDir, exe))}`);
  await downloadPinned(POT_PLUGIN, join(ytdlpDir, 'plugins', 'bgutil-ytdlp-pot-provider.zip'));

  mkdirSync(join(RELEASE, 'data'));
  cpSync(join(ROOT, 'scripts', 'README.txt'), join(RELEASE, 'README.txt'));
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

  writeFileSync(join(DIST, `${base}.sha256`), `${await sha256File(zipPath)}  ${zipFilename}\n`);
  writeFileSync(join(DIST, `${exeFilename}.sha256`), `${await sha256File(join(DIST, exeFilename))}  ${exeFilename}\n`);
  rmSync(WORK, { recursive: true, force: true });

  execFileSync(process.execPath, [join(ROOT, 'build', 'verify-release.mjs'), zipPath], { stdio: 'inherit', windowsHide: true });

  console.log('\nRelease built:');
  for (const p of outputs) console.log(`  ${p}`);
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
