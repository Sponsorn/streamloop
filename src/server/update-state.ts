import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'fs';
import { join } from 'path';
import { z } from 'zod';
import type { Part } from './components.js';
import type { Encoder } from './setup-check.js';

export type UpdateStep = 'download' | 'stop' | 'swap' | 'check' | 'start' | 'done';

export interface UpdateState {
  from: string;
  to: string;
  parts: Part[];
  folders: string[];
  step: UpdateStep;
  result: 'running' | 'ok' | 'failed' | 'rolled-back';
  reason: string | null;
  startedAt: string;
  stepStartedAt: string;
  baseline: { encoder: Encoder; works: boolean } | null;
}

/** File in data\ that exists while the new Node runs its health check. */
export const CHECKING_MARKER = 'update-checking';

const STATE_FILE = 'update-state.json';
const SWAP_LOG = 'update-swap.log';

const stateSchema = z.object({
  from: z.string(),
  to: z.string(),
  parts: z.array(z.enum(['node', 'supervisor', 'ffmpeg', 'pot', 'ytdlp-plugin'])),
  folders: z.array(z.string()),
  step: z.enum(['download', 'stop', 'swap', 'check', 'start', 'done']),
  result: z.enum(['running', 'ok', 'failed', 'rolled-back']),
  reason: z.string().nullable(),
  startedAt: z.string(),
  stepStartedAt: z.string(),
  baseline: z
    .object({
      encoder: z.enum(['h264_nvenc', 'h264_amf', 'h264_qsv', 'libx264']),
      works: z.boolean(),
    })
    .nullable(),
});

const dataDir = (root: string) => join(root, 'data');

export function readUpdateState(root: string): UpdateState | null {
  try {
    const parsed = stateSchema.safeParse(JSON.parse(readFileSync(join(dataDir(root), STATE_FILE), 'utf8')));
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

export function writeUpdateState(root: string, s: UpdateState): void {
  const dir = dataDir(root);
  mkdirSync(dir, { recursive: true });
  const tmp = join(dir, `${STATE_FILE}.tmp`);
  writeFileSync(tmp, JSON.stringify(s, null, 2));
  renameSync(tmp, join(dir, STATE_FILE));
}

export function setStep(root: string, step: UpdateStep, patch: Partial<UpdateState> = {}): UpdateState | null {
  const current = readUpdateState(root);
  if (!current) return null;
  const next: UpdateState = { ...current, ...patch, step, stepStartedAt: new Date().toISOString() };
  writeUpdateState(root, next);
  return next;
}

export function clearUpdateState(root: string): void {
  rmSync(join(dataDir(root), STATE_FILE), { force: true });
}

export interface SwapLine {
  folder: string;
  status: 'retry' | 'killed' | 'ok' | 'failed' | 'restored';
  detail: string;
}

const STATUSES: readonly string[] = ['retry', 'killed', 'ok', 'failed', 'restored'];

/** Reads START.bat's swap log: one `<folder> <status> <detail...>` line per event. */
export function readSwapLog(root: string): SwapLine[] {
  const file = join(dataDir(root), SWAP_LOG);
  if (!existsSync(file)) return [];
  const out: SwapLine[] = [];
  for (const raw of readFileSync(file, 'utf8').split(/\r?\n/)) {
    const m = /^(\S+)\s+(\S+)(?:\s+(.*))?$/.exec(raw.trim());
    if (!m || !STATUSES.includes(m[2])) continue;
    out.push({ folder: m[1], status: m[2] as SwapLine['status'], detail: m[3] ?? '' });
  }
  return out;
}
