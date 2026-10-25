import { execFile } from 'child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'fs';
import { statfs } from 'fs/promises';
import { join } from 'path';
import { logger } from './logger.js';
import { SPAWN_CWD } from './spawn-cwd.js';
import type { AppConfig } from './types.js';

export type Encoder = AppConfig['supervisor']['encoder'];

/** Resolves true when the program exits 0 within timeoutMs. */
export type RunFn = (file: string, args: string[], timeoutMs: number) => Promise<boolean>;

export interface SetupReport {
  at: string;
  encoder: { chosen: Encoder; tried: Encoder[]; software: boolean };
  cache: { freeMb: number; recommendedMb: number; warning: string | null };
  dependencies: { name: string; ok: boolean; path: string }[];
}

/** video_set in supervisor/src/media.rs; setup-check.test.ts fails when the two differ. */
export const ENCODER_ARGS: Record<Encoder, readonly string[]> = {
  h264_nvenc: ['-preset', 'p4', '-rc', 'cbr', '-bf', '0', '-profile:v', 'high'],
  h264_amf: ['-usage', 'transcoding', '-quality', 'balanced', '-rc', 'cbr', '-bf', '0', '-profile:v', 'high'],
  h264_qsv: ['-preset', 'medium', '-look_ahead', '0', '-bf', '0', '-profile:v', 'high'],
  libx264: ['-preset', 'veryfast', '-sc_threshold', '0', '-keyint_min', '60'],
};

const ORDER: Encoder[] = ['h264_nvenc', 'h264_amf', 'h264_qsv', 'libx264'];
const PROBE_TIMEOUT_MS = 20_000;
const MiB = 1024 * 1024;

export const OBS_RUNNING = 'OBS is running, and 2.x may be streaming on the same key. Close OBS to start 3.0.';
export const NO_ENCODER = 'No encoder works on this PC. Check the graphics driver.';

const runHidden: RunFn = (file, args, timeoutMs) =>
  new Promise((resolve) => {
    execFile(file, args, { cwd: SPAWN_CWD, windowsHide: true, timeout: timeoutMs }, (err) => resolve(!err));
  });

export async function probeEncoder(ffmpeg: string, enc: Encoder, run: RunFn = runHidden): Promise<boolean> {
  const args = [
    '-hide_banner', '-v', 'error',
    '-f', 'lavfi', '-i', 'testsrc2=size=1920x1080:rate=30', '-t', '2',
    '-c:v', enc, ...ENCODER_ARGS[enc],
    '-f', 'null', '-',
  ];
  return run(ffmpeg, args, PROBE_TIMEOUT_MS);
}

/** `works` is false when even libx264 failed; `encoder` is then the last one tried. */
export async function pickEncoder(
  ffmpeg: string,
  preferred: Encoder | null,
  run: RunFn = runHidden,
): Promise<{ encoder: Encoder; tried: Encoder[]; software: boolean; works: boolean }> {
  const order = preferred ? [preferred, ...ORDER.filter((e) => e !== preferred)] : ORDER;
  const tried: Encoder[] = [];
  for (const enc of order) {
    tried.push(enc);
    if (await probeEncoder(ffmpeg, enc, run)) return { encoder: enc, tried, software: enc === 'libx264', works: true };
  }
  const last = tried[tried.length - 1];
  return { encoder: last, tried, software: last === 'libx264', works: false };
}

export function recommendCacheMb(freeMb: number): { mb: number; warning: string | null } {
  if (freeMb < 2048) return { mb: 50, warning: 'Less than 2 GB free on this drive. The download cache is set to its minimum.' };
  const mb = Math.min(Math.max(Math.floor(freeMb * 0.1), 500), 4096, freeMb - 2048);
  return { mb: Math.max(mb, 50), warning: null };
}

/** What the supervisor needs at run time, in the layout of supervisor/src/paths.rs. */
export function checkDependencies(root: string, exists: (p: string) => boolean = existsSync): { name: string; ok: boolean; path: string }[] {
  const files: [string, string][] = [
    ['Supervisor', join(root, 'supervisor', 'streamloop-supervisor.exe')],
    ['Standby slate', join(root, 'supervisor', 'slate.mp4')],
    ['ffmpeg', join(root, 'ffmpeg', 'ffmpeg.exe')],
    ['yt-dlp', join(root, 'yt-dlp', 'yt-dlp.exe')],
    ['Deno', join(root, 'yt-dlp', 'deno.exe')],
    ['PO-token plugin', join(root, 'yt-dlp', 'plugins', 'bgutil-ytdlp-pot-provider.zip')],
    ['Token server', join(root, 'pot', 'build', 'main.js')],
    ['Token server modules', join(root, 'pot', 'node_modules')],
    // supervisor/src/overlay.rs names this exact file, not one under %WINDIR%.
    ['Overlay font', 'C:\\Windows\\Fonts\\malgun.ttf'],
  ];
  return files.map(([name, path]) => ({ name, ok: exists(path), path }));
}

export interface SetupCheckDeps {
  run?: RunFn;
  exists?: (p: string) => boolean;
  statfs?: (path: string) => Promise<{ bavail: number; bsize: number }>;
}

const reportPath = (root: string): string => join(root, 'data', 'setup-check.json');

export async function runSetupCheck(root: string, deps: SetupCheckDeps = {}): Promise<SetupReport> {
  const exists = deps.exists ?? existsSync;
  const pick = await pickEncoder(join(root, 'ffmpeg', 'ffmpeg.exe'), null, deps.run);
  const data = join(root, 'data');
  const fs = await (deps.statfs ?? statfs)(exists(data) ? data : root);
  const freeMb = Math.floor((fs.bavail * fs.bsize) / MiB);
  const cache = recommendCacheMb(freeMb);
  const report: SetupReport = {
    at: new Date().toISOString(),
    encoder: { chosen: pick.encoder, tried: pick.tried, software: pick.software },
    cache: { freeMb, recommendedMb: cache.mb, warning: cache.warning },
    dependencies: checkDependencies(root, exists),
  };
  mkdirSync(data, { recursive: true });
  writeFileSync(reportPath(root), JSON.stringify(report, null, 2));
  return report;
}

export function readSetupReport(root: string): SetupReport | null {
  try {
    return JSON.parse(readFileSync(reportPath(root), 'utf-8')) as SetupReport;
  } catch {
    return null;
  }
}

/** Fails open: when tasklist itself fails, OBS is taken as not running. */
export function obs64Running(): Promise<boolean> {
  return new Promise((resolve) => {
    execFile('tasklist', ['/FI', 'IMAGENAME eq obs64.exe', '/NH'], { cwd: SPAWN_CWD, windowsHide: true, timeout: 10_000 }, (err, out) =>
      resolve(!err && /obs64\.exe/i.test(out)));
  });
}

export interface LaunchGuardOptions {
  root: string;
  getEncoder: () => Encoder;
  saveEncoder: (enc: Encoder) => void;
  onEvent: (kind: 'supervisor-blocked-by-obs' | 'encoder-fallback' | 'no-encoder', detail?: Record<string, unknown>) => void;
  obsRunning?: () => Promise<boolean>;
  run?: RunFn;
}

/** The launcher's beforeLaunch: null lets the spawn go ahead, a string holds it with that reason. */
export function launchGuard(opts: LaunchGuardOptions): () => Promise<string | null> {
  const obsRunning = opts.obsRunning ?? obs64Running;
  let obsReported = false;
  let noEncoderReported = false;
  return async () => {
    if (await obsRunning()) {
      if (!obsReported) opts.onEvent('supervisor-blocked-by-obs');
      obsReported = true;
      return OBS_RUNNING;
    }
    obsReported = false;
    const from = opts.getEncoder();
    // ponytail: a box with no working encoder re-probes on every poll; throttle it if that ever shows up.
    const pick = await pickEncoder(join(opts.root, 'ffmpeg', 'ffmpeg.exe'), from, opts.run);
    if (!pick.works) {
      if (!noEncoderReported) opts.onEvent('no-encoder');
      noEncoderReported = true;
      return NO_ENCODER;
    }
    noEncoderReported = false;
    if (pick.encoder !== from) {
      try {
        opts.saveEncoder(pick.encoder);
      } catch (err) {
        logger.warn({ err }, 'Saving the fallback encoder failed');
      }
      opts.onEvent('encoder-fallback', { from, to: pick.encoder });
    }
    return null;
  };
}
