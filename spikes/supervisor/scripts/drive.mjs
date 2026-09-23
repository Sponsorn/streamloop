// The coexistence proof (criterion 9): a Node client that only ever talks to the supervisor's
// HTTP API. Run by session.mjs with cwd set to spikes/supervisor, so 'logs/...' lands there.
//   node drive.mjs controls [--resume-from k]
//   node drive.mjs faults
import { appendFileSync, writeFileSync, existsSync } from 'node:fs';
import { apiGet, apiPost } from './lib.mjs';

const [plan, ...rest] = process.argv.slice(2);
const opt = (name, fallback) => (rest.includes(`--${name}`) ? rest[rest.indexOf(`--${name}`) + 1] : fallback);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function logRow(cmd, payload) {
  appendFileSync('logs/drive.csv', `${[Date.now(), cmd, JSON.stringify(payload)].join(',')}\n`);
}

async function waitForRealVideo() {
  for (;;) {
    const status = await apiGet('/status').catch(() => null);
    if (status?.playing?.source && status.playing.source !== 'slate' && status.playing.duration > 0) return status;
    await sleep(2000);
  }
}

async function sendControl(cmd, index) {
  const before = await apiGet('/status').catch(() => null);
  const reply = await apiPost('/control', index == null ? { cmd } : { cmd, index }).catch((e) => ({ error: String(e) }));
  await sleep(1000);
  const after = await apiGet('/status').catch(() => null);
  logRow(cmd, { before: before?.playing, reply, after: after?.playing });
  return reply;
}

async function sendFault(kind, count) {
  const before = await apiGet('/status').catch(() => null);
  const reply = await apiPost('/debug/fault', { kind, count: count ?? 1 }).catch((e) => ({ error: String(e) }));
  logRow(kind, { before: before?.playing, reply });
  return reply;
}

if (plan === 'controls') {
  const resumeFrom = Number(opt('resume-from', '0'));
  const status0 = resumeFrom === 0 ? await waitForRealVideo() : await apiGet('/status');
  const sequence = Array.from({ length: 3 }, () => ['next', 'prev', 'restart', 'jump']).flat();
  for (let i = resumeFrom; i < sequence.length; i += 1) {
    const cmd = sequence[i];
    if (cmd === 'jump') {
      const status = await apiGet('/status').catch(() => status0);
      const count = status?.playlistCount ?? 1;
      let target = status?.playing?.index ?? 0;
      if (count > 1) while (target === status?.playing?.index) target = Math.floor(Math.random() * count);
      await sendControl('jump', target);
    } else {
      await sendControl(cmd);
    }
    if (i < sequence.length - 1) await sleep(60_000);
  }
} else if (plan === 'faults') {
  // 1: let the proactive URL refresh (~120s in, url-ttl-s 150/margin 30) happen on its own.
  await sleep(130_000);
  logRow('observed-refresh-window', { after: (await apiGet('/status').catch(() => null))?.playing });
  await sleep(90_000);

  await sendFault('break-url', 1);
  await sendFault('kill-feeder', 1);
  await sleep(90_000);

  await sendFault('stall-feeder', 1);
  await sleep(90_000);

  await sendFault('break-url', 'always');
  await sleep(90_000);

  await sendFault('kill-encoder', 1);
  await sleep(90_000);

  await sendFault('kill-token-server', 1);
  await sleep(90_000);

  // MediaMTX down: the session owns MediaMTX, so hand it a signal file instead of touching it here.
  const before = await apiGet('/status').catch(() => null);
  writeFileSync('logs/mediamtx-control.json', JSON.stringify({ durationMs: 60_000 }));
  logRow('mediamtx-down-requested', { before: before?.playing });
  while (existsSync('logs/mediamtx-control.json')) await sleep(500); // session consumes and deletes it
  await sleep(60_000 + 2000);
  const after = await apiGet('/status').catch(() => null);
  logRow('mediamtx-back', { after: after?.playing });
} else {
  console.error('usage: node drive.mjs controls [--resume-from k] | faults');
  process.exit(2);
}
