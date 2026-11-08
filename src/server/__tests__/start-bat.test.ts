import { describe, it, expect, beforeAll, afterAll, afterEach } from 'vitest';
import { spawn, execFileSync, type ChildProcess } from 'child_process';
import {
  copyFileSync, existsSync, linkSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync,
} from 'fs';
import { tmpdir } from 'os';
import { dirname, join, resolve } from 'path';
import { fileURLToPath } from 'url';
import { PART_KEY_FILES, SWAP_FOLDERS } from '../components.js';

// Runs the real START.bat in temp roots with a space and a non-ASCII letter in the path. Only
// node.exe is real; the app stub logs each start to starts.txt and exits as scenario.json says.

const launcher = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'scripts', 'START.bat');

const SCENARIO_STUB = `
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'fs';
import { dirname, join } from 'path';
const root = dirname(process.cwd());
const scenario = JSON.parse(readFileSync(join(root, 'scenario.json'), 'utf8'));
const runsFile = join(root, 'runs.txt');
const run = (existsSync(runsFile) ? readFileSync(runsFile, 'utf8').split('\\n').filter(Boolean).length : 0) + 1;
appendFileSync(runsFile, run + '\\n');
const tag = readFileSync(join(process.cwd(), 'version.txt'), 'utf8').trim();
appendFileSync(join(root, 'starts.txt'), (process.env.STREAMLOOP_START ?? '') + ' ' + tag + '\\n');
for (const [rel, content] of Object.entries(scenario.markers?.[run] ?? {})) {
  mkdirSync(dirname(join(root, rel)), { recursive: true });
  writeFileSync(join(root, rel), content);
}
const code = scenario.exits[run - 1] ?? 0;
if (code === 'kill-parent') {
  process.kill(process.ppid);
  process.exit(0);
}
process.exit(code);
`;

// An app from before full updates: it knows nothing of STREAMLOOP_START or update state.
const PLAIN_STUB = `
import { appendFileSync } from 'fs';
import { dirname, join } from 'path';
appendFileSync(join(dirname(process.cwd()), 'starts.txt'), 'plain old\\n');
process.exit(0);
`;

type Exit = number | 'kill-parent';
interface Scenario { exits: Exit[]; markers?: Record<number, Record<string, string>> }

let cache: string;
let nodeExe: string;
const roots: string[] = [];
const holders: ChildProcess[] = [];

function put(file: string, content: string): void {
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, content);
}

function linkNode(dest: string): void {
  mkdirSync(dirname(dest), { recursive: true });
  try {
    linkSync(nodeExe, dest);
  } catch {
    copyFileSync(nodeExe, dest);
  }
}

function keyFile(folder: string): string {
  return PART_KEY_FILES[folder].slice(folder.length + 1);
}

function writePart(dir: string, folder: string, tag: string): void {
  if (folder === 'node') linkNode(join(dir, 'node.exe'));
  else put(join(dir, keyFile(folder)), `${tag} ${folder}`);
  if (folder === 'yt-dlp') put(join(dir, 'yt-dlp.exe'), `${tag} yt-dlp.exe`);
  put(join(dir, 'version.txt'), tag);
}

function writeApp(dir: string, tag: string, stub = SCENARIO_STUB, config = true): void {
  put(join(dir, 'src', 'server', 'index.ts'), '// stub\n');
  put(join(dir, 'node_modules', 'tsx', 'dist', 'cli.mjs'), stub);
  put(join(dir, 'version.txt'), tag);
  if (config) put(join(dir, 'config.json'), JSON.stringify({ app: tag }));
}

function makeRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'sl start Ö-'));
  roots.push(root);
  copyFileSync(launcher, join(root, 'START.bat'));
  mkdirSync(join(root, 'data'));
  return root;
}

function install(root: string, tag = 'old', stub = SCENARIO_STUB): void {
  writeApp(join(root, 'app'), tag, stub);
  for (const f of SWAP_FOLDERS) writePart(join(root, f), f, tag);
}

/** Stages what the updater would: the changed part folders, app\ without config.json, and START.bat. */
function stage(root: string, folders: string[], opts: { app?: boolean; startBat?: string } = {}): void {
  const tmp = join(root, '_update_tmp');
  for (const f of folders) writePart(join(tmp, f), f, 'new');
  if (opts.app ?? true) writeApp(join(tmp, 'app'), 'new', SCENARIO_STUB, false);
  put(join(tmp, 'START.bat'), opts.startBat ?? readFileSync(launcher, 'utf8'));
}

function scenario(root: string, s: Scenario): void {
  writeFileSync(join(root, 'scenario.json'), JSON.stringify(s));
}

function lines(file: string): string[] {
  return existsSync(file) ? readFileSync(file, 'utf8').split(/\r?\n/).filter(Boolean) : [];
}

const starts = (root: string) => lines(join(root, 'starts.txt'));
const swapLog = (root: string) => lines(join(root, 'data', 'update-swap.log'));
const statuses = (root: string) => swapLog(root).map((l) => l.split(' ').slice(0, 2).join(' '));
const read = (root: string, rel: string) => readFileSync(join(root, rel), 'utf8');
const exists = (root: string, rel: string) => existsSync(join(root, rel));

function killTree(pid: number | undefined): void {
  if (!pid) return;
  try {
    execFileSync('taskkill', ['/T', '/F', '/PID', String(pid)], { stdio: 'ignore', windowsHide: true });
  } catch {
    // already gone
  }
}

/** Ends every process started from the root or naming it on its command line (new windows, stubs). */
function killUnder(root: string): void {
  const command = "Get-CimInstance Win32_Process | Where-Object { ($_.ExecutablePath -and $_.ExecutablePath.StartsWith($env:T_ROOT, [StringComparison]::OrdinalIgnoreCase)) -or ($_.CommandLine -and $_.CommandLine.IndexOf($env:T_ROOT, [StringComparison]::OrdinalIgnoreCase) -ge 0) } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }";
  try {
    execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', command], {
      env: { ...process.env, T_ROOT: root },
      stdio: 'ignore',
      windowsHide: true,
    });
  } catch {
    // best effort
  }
}

function runStartBat(root: string, args: string[] = [], timeoutMs = 50_000): Promise<{ code: number | null; out: string }> {
  return new Promise((done, fail) => {
    const child = spawn('cmd.exe', ['/d', '/c', join(root, 'START.bat'), ...args], {
      cwd: root,
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    });
    let out = '';
    child.stdout.on('data', (d: Buffer) => { out += d.toString('latin1'); });
    child.stderr.on('data', (d: Buffer) => { out += d.toString('latin1'); });
    const timer = setTimeout(() => {
      killTree(child.pid);
      fail(new Error(`START.bat did not exit within ${timeoutMs} ms:\n${out}`));
    }, timeoutMs);
    child.on('exit', (code) => {
      clearTimeout(timer);
      done({ code, out });
    });
  });
}

/** Starts a Node that holds dir as its working directory until it is ended. */
function startHolder(exe: string, dir: string): Promise<ChildProcess> {
  const child = spawn(exe, ['-e', "process.stdout.write('ready');setInterval(()=>{},1e9)"], {
    cwd: dir,
    stdio: ['ignore', 'pipe', 'ignore'],
    windowsHide: true,
  });
  holders.push(child);
  return new Promise((done, fail) => {
    child.stdout!.once('data', () => done(child));
    child.once('error', fail);
  });
}

async function waitFor(check: () => boolean, timeoutMs: number): Promise<void> {
  const end = Date.now() + timeoutMs;
  while (!check()) {
    if (Date.now() > end) throw new Error('timed out waiting');
    await new Promise((r) => setTimeout(r, 200));
  }
}

describe.skipIf(process.platform !== 'win32')('START.bat, run for real', () => {
  beforeAll(() => {
    cache = mkdtempSync(join(tmpdir(), 'sl start node-'));
    nodeExe = join(cache, 'node.exe');
    copyFileSync(process.execPath, nodeExe);
  });

  afterEach(() => {
    for (const h of holders.splice(0)) killTree(h.pid);
    for (const root of roots.splice(0)) {
      killUnder(root);
      rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
    }
  });

  afterAll(() => {
    rmSync(cache, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  });

  it('swaps every staged part and app, keeps the old ones, and starts the new Node with STREAMLOOP_START=update', async () => {
    // Arrange
    const root = makeRoot();
    install(root);
    stage(root, SWAP_FOLDERS);
    scenario(root, { exits: [75, 0] });

    // Act
    const { code, out } = await runStartBat(root);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['first old', 'update new']);
    for (const f of SWAP_FOLDERS) {
      expect(read(root, `${f}/version.txt`)).toBe('new');
      expect(read(root, `_update_old_${f}/version.txt`)).toBe('old');
    }
    expect(read(root, '_update_old/version.txt')).toBe('old');
    expect(JSON.parse(read(root, 'app/config.json'))).toEqual({ app: 'old' });
    expect(exists(root, '_update_tmp')).toBe(false);
    expect(exists(root, 'data/update-swapping')).toBe(false);
    expect(exists(root, 'data/update-checking')).toBe(true);
    expect(swapLog(root)).toEqual([...SWAP_FOLDERS.map((f) => `${f} ok swapped`), 'app ok swapped']);
  }, 60_000);

  it('keeps the installed yt-dlp.exe in the new yt-dlp folder', async () => {
    // Arrange
    const root = makeRoot();
    install(root);
    stage(root, ['yt-dlp']);
    scenario(root, { exits: [75, 0] });

    // Act
    const { code, out } = await runStartBat(root);

    // Assert
    expect(code, out).toBe(0);
    expect(read(root, 'yt-dlp/yt-dlp.exe')).toBe('old yt-dlp.exe');
    expect(read(root, `yt-dlp/${keyFile('yt-dlp')}`)).toBe('new yt-dlp');
    expect(starts(root)).toEqual(['first old', 'update new']);
  }, 60_000);

  it('ends a process holding a part folder and swaps on the second try', async () => {
    // Arrange
    const root = makeRoot();
    install(root);
    stage(root, ['ffmpeg']);
    scenario(root, { exits: [75, 0] });
    linkNode(join(root, 'ffmpeg', 'holder.exe'));
    const holder = await startHolder(join(root, 'ffmpeg', 'holder.exe'), join(root, 'ffmpeg'));

    // Act
    const { code, out } = await runStartBat(root);

    // Assert
    expect(code, out).toBe(0);
    await waitFor(() => holder.exitCode !== null || holder.signalCode !== null, 5_000);
    expect(read(root, 'ffmpeg/version.txt')).toBe('new');
    expect(statuses(root)).toEqual(['ffmpeg retry', 'ffmpeg killed', 'ffmpeg ok', 'app ok']);
    expect(swapLog(root)).toContain(`ffmpeg killed ffmpeg\\holder.exe pid ${holder.pid}`);
    expect(starts(root)).toEqual(['first old', 'update new']);
  }, 60_000);

  it('rolls back every part and starts the old version when a folder stays locked', async () => {
    // Arrange: a Node from outside the root is not ended, so ffmpeg\ stays held through all three tries.
    const root = makeRoot();
    install(root);
    stage(root, SWAP_FOLDERS);
    scenario(root, { exits: [75, 0] });
    await startHolder(process.execPath, join(root, 'ffmpeg'));

    // Act
    const { code, out } = await runStartBat(root);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['first old', 'rollback old']);
    expect(statuses(root)).toEqual([
      'node ok', 'supervisor ok', 'ffmpeg retry', 'ffmpeg retry', 'ffmpeg failed', 'node restored', 'supervisor restored',
    ]);
    for (const f of SWAP_FOLDERS) {
      expect(read(root, `${f}/version.txt`)).toBe('old');
      expect(exists(root, `_update_old_${f}`)).toBe(false);
      expect(exists(root, `_update_bad_${f}`)).toBe(false);
    }
    expect(read(root, 'app/version.txt')).toBe('old');
    expect(exists(root, '_update_tmp')).toBe(false);
    expect(exists(root, 'data/update-swapping')).toBe(false);
  }, 60_000);

  it('passes update to a START.bat that replaced itself through the trampoline', async () => {
    // Arrange
    const root = makeRoot();
    install(root);
    const changed = readFileSync(launcher, 'utf8') + 'rem a changed launcher\r\n';
    stage(root, ['supervisor'], { startBat: changed });
    scenario(root, { exits: [75, 'kill-parent'] });

    // Act
    const { code, out } = await runStartBat(root);
    await waitFor(() => starts(root).length >= 2 && !exists(root, '_apply_launcher.bat'), 30_000);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['first old', 'update new']);
    expect(read(root, 'START.bat')).toBe(changed);
    expect(exists(root, 'START.bat.new')).toBe(false);
  }, 60_000);

  it('passes restart to a START.bat that replaced itself in an app-only update', async () => {
    // Arrange
    const root = makeRoot();
    install(root);
    const changed = readFileSync(launcher, 'utf8') + 'rem a changed launcher\r\n';
    stage(root, [], { startBat: changed });
    scenario(root, { exits: [75, 'kill-parent'] });

    // Act
    const { code, out } = await runStartBat(root);
    await waitFor(() => starts(root).length >= 2 && !exists(root, '_apply_launcher.bat'), 30_000);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['first old', 'restart new']);
    expect(read(root, 'START.bat')).toBe(changed);
    expect(exists(root, '_update_old')).toBe(false);
  }, 60_000);

  it('restores parts and app on exit 76 and keeps the newer config.json', async () => {
    // Arrange
    const root = makeRoot();
    install(root);
    stage(root, SWAP_FOLDERS);
    scenario(root, {
      exits: [75, 76, 0],
      markers: {
        2: {
          'app/config.json': JSON.stringify({ app: 'newer' }),
          'app/logs/new.log': 'from the new app',
          'data/update-checking': '',
        },
      },
    });

    // Act
    const { code, out } = await runStartBat(root);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['first old', 'update new', 'rollback old']);
    for (const f of SWAP_FOLDERS) {
      expect(read(root, `${f}/version.txt`)).toBe('old');
      expect(exists(root, `_update_old_${f}`)).toBe(false);
      expect(exists(root, `_update_bad_${f}`)).toBe(false);
    }
    expect(read(root, 'app/version.txt')).toBe('old');
    expect(JSON.parse(read(root, 'app/config.json'))).toEqual({ app: 'newer' });
    expect(read(root, 'app/logs/new.log')).toBe('from the new app');
    expect(exists(root, '_update_old')).toBe(false);
    expect(exists(root, '_update_bad_app')).toBe(false);
    expect(exists(root, 'data/update-checking')).toBe(false);
    expect(swapLog(root).slice(-6)).toEqual([
      ...SWAP_FOLDERS.map((f) => `${f} restored the previous version`),
      'app restored the previous version',
    ]);
  }, 60_000);

  it('rolls back on the third crash while update-checking exists', async () => {
    // Arrange
    const root = makeRoot();
    install(root);
    stage(root, ['supervisor']);
    scenario(root, { exits: [75, 1, 1, 1, 0], markers: { 2: { 'data/update-checking': '' } } });

    // Act
    const { code, out } = await runStartBat(root, [], 80_000);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['first old', 'update new', 'update new', 'update new', 'rollback old']);
    expect(read(root, 'supervisor/version.txt')).toBe('old');
    expect(read(root, 'app/version.txt')).toBe('old');
    expect(exists(root, 'data/update-checking')).toBe(false);
  }, 90_000);

  it('rolls back on the third crash of a new Node that dies before its check starts', async () => {
    // Arrange: no run writes data\update-checking, as with an app that fails on import.
    const root = makeRoot();
    install(root);
    stage(root, ['supervisor']);
    scenario(root, { exits: [75, 1, 1, 1, 0] });

    // Act
    const { code, out } = await runStartBat(root, [], 80_000);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['first old', 'update new', 'update new', 'update new', 'rollback old']);
    expect(read(root, 'supervisor/version.txt')).toBe('old');
    expect(exists(root, 'data/update-checking')).toBe(false);
  }, 90_000);

  it('leaves no update state behind after an app-only update', async () => {
    // Arrange
    const root = makeRoot();
    install(root);
    stage(root, []);
    scenario(root, { exits: [75, 0] });

    // Act
    const { code, out } = await runStartBat(root);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['first old', 'restart new']);
    expect(exists(root, 'data/update-checking')).toBe(false);
    expect(exists(root, '_update_old')).toBe(false);
    expect(swapLog(root)).toEqual([]);
  }, 60_000);

  it('restores a missing node folder from _update_old_node before starting', async () => {
    // Arrange: the window was closed after node\ was renamed and before the new one moved in.
    const root = makeRoot();
    install(root);
    renameSync(join(root, 'node'), join(root, '_update_old_node'));
    scenario(root, { exits: [0] });

    // Act
    const { code, out } = await runStartBat(root);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['first old']);
    expect(read(root, 'node/version.txt')).toBe('old');
    expect(exists(root, '_update_old_node')).toBe(false);
  }, 60_000);

  it('puts the previous version back when the window closed in the middle of a swap', async () => {
    // Arrange: supervisor\ already swapped, ffmpeg\ and app\ still staged.
    const root = makeRoot();
    install(root);
    renameSync(join(root, 'supervisor'), join(root, '_update_old_supervisor'));
    writePart(join(root, 'supervisor'), 'supervisor', 'new');
    stage(root, ['ffmpeg']);
    writeFileSync(join(root, 'data', 'update-swapping'), '');
    scenario(root, { exits: [0] });

    // Act
    const { code, out } = await runStartBat(root);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['first old']);
    expect(read(root, 'supervisor/version.txt')).toBe('old');
    expect(read(root, 'ffmpeg/version.txt')).toBe('old');
    expect(exists(root, '_update_old_supervisor')).toBe(false);
    expect(exists(root, '_update_tmp')).toBe(false);
    expect(exists(root, 'data/update-swapping')).toBe(false);
  }, 60_000);

  it('rolls back to an old app that knows nothing of update state', async () => {
    // Arrange: the state right after a part swap, as the trampoline would start it.
    const root = makeRoot();
    writeApp(join(root, 'app'), 'new');
    writeApp(join(root, '_update_old'), 'old', PLAIN_STUB);
    for (const f of SWAP_FOLDERS) {
      writePart(join(root, f), f, 'new');
      writePart(join(root, `_update_old_${f}`), f, 'old');
    }
    scenario(root, { exits: [76], markers: { 1: { 'data/update-checking': '' } } });

    // Act
    const { code, out } = await runStartBat(root, ['update']);

    // Assert
    expect(code, out).toBe(0);
    expect(starts(root)).toEqual(['update new', 'plain old']);
    for (const f of SWAP_FOLDERS) expect(read(root, `${f}/version.txt`)).toBe('old');
    expect(read(root, 'app/version.txt')).toBe('old');
    expect(exists(root, 'data/update-checking')).toBe(false);
  }, 60_000);
});
