// One whole run with one command: MediaMTX if needed, supervisor, watcher, recorder, verdict.
//   node session.mjs <name> [run.mjs options]      e.g.  node session.mjs stress --hours 2 --short
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, renameSync, writeFileSync, existsSync } from 'node:fs';
import net from 'node:net';

const [name, ...runArgs] = process.argv.slice(2);
if (!name || name.startsWith('--')) {
  console.error('usage: node session.mjs <name> [run.mjs options]');
  process.exit(2);
}
const node = process.execPath;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const rtmpUp = () => new Promise((resolve) => {
  const socket = net.connect(1935, '127.0.0.1');
  socket.on('connect', () => { socket.destroy(); resolve(true); });
  socket.on('error', () => resolve(false));
});

let mediamtx = null;
if (!(await rtmpUp())) {
  const toolsDir = ['tools', '../tools'].find((dir) => existsSync(`${dir}/mediamtx.exe`));
  if (!toolsDir) { console.error('Nothing listens on :1935 and mediamtx.exe is in neither tools/ nor ../tools/.'); process.exit(2); }
  mediamtx = spawn(`${toolsDir}/mediamtx.exe`, [], { cwd: toolsDir, stdio: 'ignore' });
  await sleep(3000);
  if (!(await rtmpUp())) { console.error('MediaMTX did not open :1935.'); process.exit(2); }
}

rmSync('logs', { recursive: true, force: true });
rmSync('rec', { recursive: true, force: true });
mkdirSync('results', { recursive: true });

const supervisor = spawn(node, ['run.mjs', ...runArgs], { stdio: 'inherit' });
// The readers can only attach once the stream is published.
await sleep(8000);
const watch = spawn(node, ['check.mjs', 'watch'], { stdio: 'ignore' });
const record = spawn(node, ['check.mjs', 'record'], { stdio: 'ignore' });

const code = await new Promise((resolve) => supervisor.on('close', resolve));
// Give the readers time to see the stream end and flush their last rows and segment.
await sleep(10000);
watch.kill();
record.kill();

const verdict = spawnSync(node, ['check.mjs', 'verdict'], { encoding: 'utf8' });
const report = `${new Date().toISOString()}  ${name}: run.mjs ${runArgs.join(' ')}  (supervisor exit ${code})\n${verdict.stdout}${verdict.stderr}`;
writeFileSync(`results/${name}-verdict.txt`, report);
rmSync(`results/${name}-logs`, { recursive: true, force: true });
renameSync('logs', `results/${name}-logs`);
console.log(report);
console.log(`Saved results/${name}-verdict.txt and results/${name}-logs/. Recordings stay in rec/ until the next session.`);
mediamtx?.kill();
process.exit(code === 0 ? 0 : 1);
