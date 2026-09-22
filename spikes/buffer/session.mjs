// One whole run with one command: MediaMTX if needed, supervisor, spike 1's watcher, verdict.
//   node session.mjs <name> [run.mjs options]   e.g.  node session.mjs soak --hours 1 --budget-mb 300
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
  if (!existsSync('../tools/mediamtx.exe')) { console.error('Nothing listens on :1935 and ../tools/mediamtx.exe is missing.'); process.exit(2); }
  mediamtx = spawn('../tools/mediamtx.exe', [], { cwd: '../tools', stdio: 'ignore' });
  await sleep(3000);
  if (!(await rtmpUp())) { console.error('MediaMTX did not open :1935.'); process.exit(2); }
}

rmSync('logs', { recursive: true, force: true });
mkdirSync('results', { recursive: true });

const supervisor = spawn(node, ['run.mjs', ...runArgs], { stdio: 'inherit' });
// The reader can only attach once the stream is published.
await sleep(8000);
// Spike 1's checker unchanged: it resolves its own lib.mjs and writes logs/ in this cwd.
const watch = spawn(node, ['../continuity/check.mjs', 'watch'], { stdio: 'ignore' });

const code = await new Promise((resolve) => supervisor.on('close', resolve));
// Give the reader time to see the stream end and flush its last rows.
await sleep(10000);
watch.kill();

// No recorder: real videos have no flash/beep, so spike 1's avsync numbers come out NaN here.
const verdict = spawnSync(node, ['../continuity/check.mjs', 'verdict'], { encoding: 'utf8' });
const report = `${new Date().toISOString()}  ${name}: run.mjs ${runArgs.join(' ')}  (supervisor exit ${code})\n${verdict.stdout}${verdict.stderr}`;
rmSync(`results/${name}`, { recursive: true, force: true });
renameSync('logs', `results/${name}`);
writeFileSync(`results/${name}/verdict.txt`, report);
console.log(report);
console.log(`Saved results/${name}/`);
mediamtx?.kill();
process.exit(code === 0 ? 0 : 1);
