// The coexistence proof (criterion 9): a Node client that only ever talks to the supervisor's
// HTTP API (and, for the playlist switch, to config.json, as Node will). Run by session.mjs with
// cwd set to supervisor/, so 'logs/...' lands there.
//   node drive.mjs controls [--resume-from k] [--gap-s 60]
//   node drive.mjs faults --config <config.json>
//   node drive.mjs switch --config <config.json>   (the playlist switch alone)
// The switch needs a run without --start: --start applies to every list, and the second has one video.
import { appendFileSync, writeFileSync, existsSync, readFileSync } from 'node:fs';
import { apiGet, apiPost, PLAYLIST, SECOND_PLAYLIST } from './lib.mjs';

const [plan, ...rest] = process.argv.slice(2);
const opt = (name, fallback) => (rest.includes(`--${name}`) ? rest[rest.indexOf(`--${name}`) + 1] : fallback);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const status = () => apiGet('/status').catch(() => null);

function logRow(cmd, payload) {
  appendFileSync('logs/drive.csv', `${[Date.now(), cmd, JSON.stringify(payload)].join(',')}\n`);
}

async function waitForRealVideo() {
  for (;;) {
    const s = await status();
    if (s?.playing?.source && s.playing.source !== 'slate' && s.playing.duration > 0) return s;
    await sleep(2000);
  }
}

/** Polls /status until it names the target on a real video (not the slate), up to 30 s. */
async function waitForTarget(targetIndex, fromMs) {
  for (;;) {
    const s = await status();
    if (s?.playing?.index === targetIndex && s.playing.source !== 'slate') return { ms: Date.now() - fromMs, playing: s.playing };
    if (Date.now() - fromMs > 30_000) return { ms: null, playing: s?.playing };
    await sleep(250);
  }
}

async function sendControl(cmd, index) {
  const before = await status();
  const reply = await apiPost('/control', index == null ? { cmd } : { cmd, index }).catch((e) => ({ error: String(e) }));
  const replyMs = Date.now();
  const seen = reply.targetIndex == null ? { ms: null } : await waitForTarget(reply.targetIndex, replyMs);
  logRow(cmd, { replyMs, before: before?.playing, reply, statusMatchMs: seen.ms, after: seen.playing });
  return reply;
}

async function sendFault(kind, count) {
  const before = await status();
  const reply = await apiPost('/debug/fault', count == null ? { kind } : { kind, count }).catch((e) => ({ error: String(e) }));
  logRow(kind, { replyMs: Date.now(), count: count ?? null, before: before?.playing, reply });
  return reply;
}

/** Edits playlists[0].id in the temp config, POSTs /reload and waits (up to 120 s) until
 *  /status plays video 0 of the new list off the slate. */
async function switchPlaylist(id) {
  const file = opt('config', null);
  const config = JSON.parse(readFileSync(file, 'utf8'));
  const before = await status();
  config.playlists[0].id = id;
  writeFileSync(file, JSON.stringify(config, null, 2));
  const reply = await apiPost('/reload').catch((e) => ({ error: String(e) }));
  const replyMs = Date.now();
  let seen = null;
  while (Date.now() - replyMs < 120_000) {
    const s = await status();
    // the old list plays on until the new one is fetched; then the switch jumps to its video 0
    if (s?.playing?.index === 0 && s.playing.source !== 'slate' && s.playing.id !== before?.playing?.id) { seen = s; break; }
    await sleep(250);
  }
  logRow('playlist-switch', { replyMs, playlistId: id, before: before?.playing, reply, statusMatchMs: seen ? Date.now() - replyMs : null, after: seen?.playing ?? null });
}

/** A live playlist switch through config.json + POST /reload, then back. */
async function switchAndBack() {
  await switchPlaylist(SECOND_PLAYLIST);
  await sleep(60_000);
  await switchPlaylist(PLAYLIST);
  await sleep(30_000);
}

if (plan === 'switch') {
  await waitForRealVideo();
  await sleep(60_000);
  await switchAndBack();
} else if (plan === 'controls') {
  const resumeFrom = Number(opt('resume-from', '0'));
  const gapMs = Number(opt('gap-s', '60')) * 1000;
  const sequence = Array.from({ length: 3 }, () => ['next', 'prev', 'restart', 'jump']).flat();
  if (resumeFrom === 0) {
    // one gap on the first video before the first command, so its URL-to-file handover (criterion 2)
    // happens instead of being cut away by a 'next' a second after the cold start
    await waitForRealVideo();
    await sleep(gapMs);
  } else {
    // a relaunched client keeps the spacing from the last command the killed one sent
    const last = readFileSync('logs/drive.csv', 'utf8').trim().split(/\r?\n/).at(-1);
    await sleep(Math.max(0, Number(last.split(',')[0]) + gapMs - Date.now()));
  }
  for (let i = resumeFrom; i < sequence.length; i += 1) {
    const cmd = sequence[i];
    if (cmd === 'jump') {
      const s = await status();
      const count = s?.playlistCount ?? 1;
      let target = s?.playing?.index ?? 0;
      if (count > 1) while (target === s?.playing?.index) target = Math.floor(Math.random() * count);
      await sendControl('jump', target);
    } else {
      await sendControl(cmd);
    }
    const sent = Number(readFileSync('logs/drive.csv', 'utf8').trim().split(/\r?\n/).at(-1).split(',')[0]);
    if (i < sequence.length - 1) await sleep(Math.max(0, sent + gapMs - Date.now()));
  }
} else if (plan === 'faults') {
  // Needs --url-ttl-s 300 --url-refresh-margin-s 60 and throttled downloads so the first video
  // stays on its URL and self-refreshes; 'restart' gives each fault a fresh retry budget.
  await waitForRealVideo();
  await sleep(270_000); // one proactive refresh happens on its own in here (ttl 300 s, margin 60 s)

  await sendFault('break-url', 1); // the retry's re-resolve comes back broken...
  await sendFault('kill-feeder'); // ...after the feeder dies: exit retry, then a 403 retry
  await sleep(75_000);

  await sendControl('restart');
  await sleep(45_000);
  await sendFault('stall-feeder');
  await sleep(75_000);

  await sendControl('restart');
  await sleep(45_000);
  await sendFault('break-url', 'always'); // every resolve broken: retry, retry, skip, next video
  await sleep(90_000);

  await sendFault('kill-encoder');
  await sleep(90_000);

  await sendFault('kill-token-server');
  await sleep(45_000);
  // the throttled download in flight predates the kill; a new target starts a new download, whose
  // [pot:bgutil:http] line shows yt-dlp using the token server the supervisor now owns
  await sendControl('next');
  await sleep(60_000);

  // Watchdog: a frozen picture is healed through the stall ladder (restart first: a fresh retry
  // budget), a muted video is retried once.
  await sendControl('restart');
  await sleep(45_000);
  await sendFault('freeze-video');
  await sleep(90_000);
  await sendControl('restart');
  await sleep(45_000);
  await sendFault('mute-audio');
  await sleep(100_000);

  await switchAndBack();

  // MediaMTX down: the session owns MediaMTX, so hand it a signal file instead of touching it here.
  const before = await status();
  writeFileSync('logs/mediamtx-control.json', JSON.stringify({ durationMs: 60_000 }));
  logRow('mediamtx-down-requested', { before: before?.playing });
  while (existsSync('logs/mediamtx-control.json')) await sleep(500); // session consumes and deletes it
  await sleep(60_000 + 30_000);
  logRow('mediamtx-back', { after: (await status())?.playing });

  // YouTube breaker without asking YouTube: the next resolve reports a bot check. The breaker
  // opens, nothing calls yt-dlp for a minute, then one probe resolve closes it.
  await sendFault('bot-check');
  for (let i = 0; i < 18; i += 1) {
    await sleep(10_000);
    const s = await status();
    logRow('breaker-status', { youtube: s?.youtube, playing: s?.playing });
  }
} else {
  console.error('usage: node drive.mjs controls [--resume-from k] [--gap-s 60] | faults --config f | switch --config f');
  process.exit(2);
}
