import { streamLine, twitchLine, isBenignReloadError, sourceLabel, wizardVerdict, formatTime } from './status-view.js';

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => document.querySelectorAll(sel);

let apiToken = '';
let pollTimer = null;
let pollFailures = 0;
const VIDEO_LIST_PER_PAGE = 25;

// Initialization

async function init() {
  try {
    const status = await api('/api/status');
    if (status.firstRun) showWizard();
    else showDashboard();
  } catch (err) {
    console.error('Failed to init:', err);
    showWizard();
  }
}

function showWizard() {
  $('#view-wizard').classList.remove('hidden');
  $('#view-dashboard').classList.add('hidden');
  stopPolling();
  wizInitWizard();
}

function showDashboard() {
  $('#view-wizard').classList.add('hidden');
  $('#view-dashboard').classList.remove('hidden');
  startPolling();
  loadSettings();
  loadAutostart();
  loadPlaylistSelector();
  loadVideoList(1);
  loadLogList();
}

// API helper

async function fetchApiToken() {
  try {
    const res = await fetch('/api/token');
    const data = await res.json();
    apiToken = data.token;
  } catch (err) {
    console.error('Failed to fetch API token:', err);
  }
}

async function api(url, opts) {
  if (!opts) opts = {};
  if (opts.body && typeof opts.body === 'string') {
    opts.headers = { 'Content-Type': 'application/json', ...opts.headers };
  }
  if (apiToken) {
    opts.headers = { ...opts.headers, 'X-API-Token': apiToken };
  }
  const res = await fetch(url, opts);
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.error || `HTTP ${res.status}`);
  }
  return res.json();
}

function post(url, body) {
  return api(url, body === undefined ? { method: 'POST' } : { method: 'POST', body: JSON.stringify(body) });
}

/** Saves a config patch; returns the response. Shows a reload error unless it only means the supervisor is down. */
async function saveConfig(patch) {
  const r = await post('/api/config', patch);
  if (!isBenignReloadError(r)) showToast('Saved, but the supervisor could not reload: ' + r.reloadError);
  return r;
}

// Playlist row helpers

function addPlaylistRow(containerSel, id, name) {
  const row = document.createElement('div');
  row.className = 'playlist-row';
  row.innerHTML =
    `<input type="text" class="pl-id" placeholder="PLxxxxxxxxxxxxxxxx" aria-label="Playlist ID or URL" value="${escapeHtml(id || '')}">` +
    `<input type="text" class="pl-name" placeholder="Name (optional)" aria-label="Playlist name" value="${escapeHtml(name || '')}">` +
    `<button type="button" class="btn-remove" title="Remove" aria-label="Remove playlist">&times;</button>`;
  row.querySelector('.btn-remove').addEventListener('click', () => row.remove());
  $(containerSel).appendChild(row);
}

function extractPlaylistId(raw) {
  try {
    const url = new URL(raw);
    if (url.hostname.includes('youtube.com') || url.hostname.includes('youtu.be')) {
      return url.searchParams.get('list'); // null for a YouTube URL without a playlist
    }
  } catch { /* not a URL, treat as bare ID */ }
  return raw;
}

function collectPlaylists(containerSel) {
  const playlists = [];
  let hasError = false;
  for (const row of $$(containerSel + ' .playlist-row')) {
    const input = row.querySelector('.pl-id');
    const raw = input.value.trim();
    if (!raw) continue;
    const id = extractPlaylistId(raw);
    if (!id) {
      input.classList.add('invalid');
      showToast('That looks like a single video URL. Paste a playlist URL or a playlist ID (starts with PL).');
      hasError = true;
      continue;
    }
    input.classList.remove('invalid');
    const name = row.querySelector('.pl-name').value.trim();
    playlists.push(name ? { id, name } : { id });
  }
  return hasError ? null : playlists;
}

// Wizard

const WIZ_TOTAL_STEPS = 5;
let wizStep = 1;
let wizFinished = false;
const wizState = {
  playlists: [],
  ingestUrl: '',
  discordWebhookUrl: '',
  autostart: false,
};

function wizInitWizard() {
  api('/api/config').then(cfg => {
    if (cfg.discord && cfg.discord.webhookUrl && cfg.discord.webhookUrl !== '********') {
      wizState.discordWebhookUrl = cfg.discord.webhookUrl;
    }
    wizState.ingestUrl = (cfg.supervisor && cfg.supervisor.ingestUrl) || '';
    const playlists = cfg.playlists || [];
    if (playlists.some(p => !p.id.includes('xxxxx'))) wizState.playlists = playlists;
  }).catch(() => {});
  wizGoTo(1);
}

function wizGoTo(step) {
  if (wizStep !== step) wizCollectStepData();
  wizStep = step;
  $$('.wiz-step').forEach(el => el.classList.toggle('hidden', Number(el.dataset.step) !== step));
  $$('.wiz-progress-dot').forEach(dot => {
    const s = Number(dot.dataset.step);
    dot.classList.remove('active', 'completed');
    if (s === step) dot.classList.add('active');
    else if (s < step) dot.classList.add('completed');
  });
  $$('.wiz-progress-line').forEach(line => line.classList.toggle('completed', Number(line.dataset.after) < step));
  $('#wiz-back').classList.toggle('invisible', step === 1);
  $('#wiz-next').textContent = step === WIZ_TOTAL_STEPS ? 'Finish' : step === 1 ? 'Get Started' : 'Continue';
  wizInitStep(step);
}

function wizNext() {
  if (wizFinished) { showDashboard(); return; }
  if (wizStep === WIZ_TOTAL_STEPS) { runVerification(); return; }
  if (!wizValidateStep()) return;
  wizGoTo(wizStep + 1);
}

function wizBack() {
  if (wizStep > 1) wizGoTo(wizStep - 1);
}

function wizCollectStepData() {
  switch (wizStep) {
    case 2: {
      const pl = collectPlaylists('#wiz-playlists-list');
      if (pl) wizState.playlists = pl;
      break;
    }
    case 3:
      // The stream key stays in its input; it never goes into wizState.
      wizState.ingestUrl = $('#wiz-ingest').value.trim();
      break;
    case 4:
      wizState.discordWebhookUrl = $('#wiz-discord').value.trim();
      wizState.autostart = $('#wiz-autostart').checked;
      break;
  }
}

function wizInitStep(step) {
  switch (step) {
    case 2: {
      $('#wiz-playlists-list').innerHTML = '';
      if (wizState.playlists.length > 0) wizState.playlists.forEach(p => addPlaylistRow('#wiz-playlists-list', p.id, p.name));
      else addPlaylistRow('#wiz-playlists-list', '', '');
      break;
    }
    case 3:
      $('#wiz-ingest').value = wizState.ingestUrl;
      break;
    case 4:
      $('#wiz-discord').value = wizState.discordWebhookUrl;
      $('#wiz-autostart').checked = wizState.autostart;
      break;
    case 5:
      ['config', 'key', 'live'].forEach(key => setVerifyIcon(key, ''));
      $('#wiz-verify-status').textContent = '';
      $('#wiz-next').disabled = false;
      break;
  }
}

function wizValidateStep() {
  if (wizStep === 2) {
    const pl = collectPlaylists('#wiz-playlists-list');
    if (pl === null) return false;
    wizState.playlists = pl;
    if (pl.length === 0) {
      showToast('Add at least one playlist ID.');
      return false;
    }
  }
  if (wizStep === 3 && !$('#wiz-stream-key').value.trim()) {
    showToast('Enter your stream key. Twitch needs it to accept the stream.');
    return false;
  }
  return true;
}

async function runVerification() {
  const nextBtn = $('#wiz-next');
  const statusEl = $('#wiz-verify-status');
  nextBtn.disabled = true;
  nextBtn.textContent = 'Saving...';
  $('#wiz-back').classList.add('invisible');
  wizCollectStepData();

  setVerifyIcon('config', 'spin');
  statusEl.textContent = 'Saving settings...';
  const body = {
    playlists: wizState.playlists,
    discord: { webhookUrl: wizState.discordWebhookUrl },
  };
  if (wizState.ingestUrl) body.supervisor = { ingestUrl: wizState.ingestUrl };
  try {
    // The supervisor is not up yet on a first save, so a refused reload is expected here.
    await saveConfig(body);
    setVerifyIcon('config', 'pass');
  } catch (err) {
    setVerifyIcon('config', 'fail');
    statusEl.textContent = 'Could not save: ' + err.message;
    $('#wiz-back').classList.remove('invisible');
    nextBtn.disabled = false;
    nextBtn.textContent = 'Retry';
    return;
  }

  setVerifyIcon('key', 'spin');
  statusEl.textContent = 'Saving the stream key...';
  const keyInput = $('#wiz-stream-key');
  let keyOk = false;
  try {
    await post('/api/stream-key', { key: keyInput.value });
    keyInput.value = '';
    keyOk = true;
    setVerifyIcon('key', 'pass');
  } catch (err) {
    setVerifyIcon('key', 'fail');
    statusEl.textContent = 'Could not save the stream key: ' + err.message;
  }

  if (wizState.autostart) {
    try {
      await post('/api/autostart', { enabled: true });
    } catch (err) {
      showToast('Could not turn on autostart: ' + err.message);
    }
  }

  setVerifyIcon('live', 'spin');
  nextBtn.textContent = 'Checking...';
  const started = Date.now();
  let verdict = 'wait';
  let stream = null;
  while (verdict === 'wait') {
    await sleep(2000);
    try {
      stream = (await api('/api/status')).stream;
      if (keyOk) statusEl.textContent = streamLine(stream).word + (stream.reason ? ': ' + stream.reason : '');
    } catch {
      // The server can drop a poll while it takes the new config in.
    }
    verdict = wizardVerdict(stream, Date.now() - started);
  }

  if (verdict === 'live') {
    setVerifyIcon('live', 'pass');
    statusEl.textContent = 'The stream is live.';
  } else {
    setVerifyIcon('live', 'fail');
    const why = (stream && stream.reason) || 'The server did not answer.';
    statusEl.textContent = (keyOk ? '' : statusEl.textContent + ' ') + why + ' You can fix this later from the dashboard.';
  }
  wizFinished = true;
  nextBtn.disabled = false;
  nextBtn.textContent = 'Go to Dashboard';
}

function setVerifyIcon(key, state) {
  const icon = $(`#wiz-chk-${key}`);
  icon.className = 'wiz-verify-icon ' + state;
  icon.textContent = state === 'pass' ? '✔' : state === 'fail' ? '✘' : '●';
}

function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

// Monitor: stream line

function renderStream(s) {
  const line = streamLine(s.stream);
  const word = $('#stream-word');
  word.textContent = line.word;
  word.className = 'stream-word ' + line.level;
  const reason = $('#stream-reason');
  reason.textContent = line.reason || '';
  reason.classList.toggle('hidden', !line.reason);

  const state = s.stream && s.stream.state;
  $('#btn-stream-start').classList.toggle('hidden', state !== 'stopped' && state !== 'off-air');
  $('#btn-stream-stop').classList.toggle('hidden', state === 'stopped');

  const tw = twitchLine(s.twitch);
  const twEl = $('#twitch-line');
  twEl.textContent = tw.text;
  twEl.className = 'stream-detail ' + tw.level;

  const enc = s.supervisor && s.supervisor.encoder;
  let encText = 'Encoder: not running';
  if (enc && enc.alive) {
    encText = 'Encoder: running';
    if (typeof enc.speed === 'number') encText += `, ${enc.speed.toFixed(2)}x speed`;
    if (enc.restarts) encText += `, ${enc.restarts} restart${enc.restarts === 1 ? '' : 's'}`;
  }
  $('#encoder-line').textContent = encText;

  $('#uptime').textContent = 'Uptime: ' + formatDuration(s.uptimeMs);
}

function renderNowPlaying(s) {
  const p = (s.supervisor && s.supervisor.playing) || {};
  const count = s.supervisor && s.supervisor.playlistCount;
  $('#np-title').textContent = p.title || '-';
  $('#np-index').textContent = typeof p.index === 'number' ? (count ? `${p.index + 1} of ${count}` : String(p.index + 1)) : '-';
  const vidEl = $('#np-videoid');
  vidEl.textContent = '';
  if (p.id) {
    const a = document.createElement('a');
    a.href = 'https://www.youtube.com/watch?v=' + encodeURIComponent(p.id);
    a.target = '_blank';
    a.rel = 'noopener';
    a.textContent = p.id;
    vidEl.appendChild(a);
  } else {
    vidEl.textContent = '-';
  }
  $('#np-time').textContent = typeof p.position === 'number'
    ? `${formatTime(p.position)} / ${p.duration ? formatTime(p.duration) : '--:--:--'}`
    : '-';
  $('#np-source').textContent = sourceLabel(p.source);
  updateVideoListHighlight(typeof p.index === 'number' ? p.index : -1);
}

// Monitor: stream and player controls

async function runAction(btn, fn) {
  btn.disabled = true;
  try {
    await fn();
  } catch (err) {
    showToast(err.message);
  } finally {
    btn.disabled = false;
  }
  pollOnce();
}

async function restartStream() {
  await post('/api/stream/stop');
  await post('/api/stream/start');
}

function initMonitorControls() {
  $('#btn-prev').addEventListener('click', (e) => runAction(e.currentTarget, () => post('/api/player/prev')));
  $('#btn-next').addEventListener('click', (e) => runAction(e.currentTarget, () => post('/api/player/next')));
  $('#btn-restart').addEventListener('click', (e) => runAction(e.currentTarget, () => post('/api/player/restart')));
  $('#btn-stream-start').addEventListener('click', (e) => runAction(e.currentTarget, () => post('/api/stream/start')));
  $('#btn-stream-stop').addEventListener('click', (e) => {
    if (!confirm('Stop the stream? The channel goes offline until you start it again.')) return;
    runAction(e.currentTarget, () => post('/api/stream/stop'));
  });

  $('#playlist-select').addEventListener('change', (e) => {
    const playlistIndex = Number(e.target.value);
    post('/api/playlist/switch', { playlistIndex })
      .then((r) => {
        if (!isBenignReloadError(r)) showToast('Switched, but the supervisor could not reload: ' + r.reloadError);
        settingsLoaded = false;
        loadPlaylistSelector();
        loadVideoList(1);
      })
      .catch((err) => showToast(err.message));
  });

  $('#video-list-body').addEventListener('click', (e) => {
    const btn = e.target.closest('.btn-play');
    if (btn) runAction(btn, () => post('/api/player/jump', { index: Number(btn.dataset.index) }));
  });

  $('#video-list-pagination').addEventListener('click', (e) => {
    const page = e.target.dataset && e.target.dataset.page;
    if (page) loadVideoList(Number(page));
  });

  $('#log-refresh').addEventListener('click', loadLogList);
  $('#log-select').addEventListener('change', (e) => viewLog(e.target.value));
}

// Monitor: playlist and video list

async function loadPlaylistSelector() {
  const select = $('#playlist-select');
  try {
    const cfg = await api('/api/config');
    select.innerHTML = '';
    cfg.playlists.forEach((p, i) => {
      const opt = document.createElement('option');
      opt.value = String(i);
      opt.textContent = p.name || p.id;
      select.appendChild(opt);
    });
    // The first playlist is the one playing; a switch moves the chosen one to the front.
    select.value = '0';
  } catch (err) {
    console.error('Failed to load playlists:', err);
  }
}

let lastHighlightedIndex = -1;

async function loadVideoList(page) {
  const status = $('#video-list-status');
  status.textContent = 'Loading playlist...';
  status.classList.remove('hidden');
  try {
    const data = await api('/api/playlist/videos?page=' + page + '&perPage=' + VIDEO_LIST_PER_PAGE);
    renderVideoList(data);
  } catch (err) {
    $('#video-list-body').innerHTML = '';
    status.textContent = 'Could not load the playlist: ' + err.message;
  }
}

function renderVideoList(data) {
  const tbody = $('#video-list-body');
  const status = $('#video-list-status');
  tbody.innerHTML = '';
  if (!data.videos.length) {
    status.textContent = 'This playlist has no videos.';
  } else {
    status.classList.add('hidden');
  }
  for (const v of data.videos) {
    const tr = document.createElement('tr');
    if (v.index === data.currentIndex) tr.className = 'current';
    tr.innerHTML = '<td>' + (v.index + 1) + '</td>' +
      '<td>' + escapeHtml(v.title || 'Untitled') + '</td>' +
      '<td>' + (v.duration ? formatTime(v.duration).replace(/^0:/, '') : '-') + '</td>' +
      '<td><button type="button" class="btn btn-secondary btn-play" data-index="' + v.index + '" aria-label="Play video ' + (v.index + 1) + '">&#9654;</button></td>';
    tbody.appendChild(tr);
  }
  lastHighlightedIndex = data.currentIndex ?? -1;

  const totalPages = Math.ceil(data.total / data.perPage);
  const pag = $('#video-list-pagination');
  pag.innerHTML = '';
  if (totalPages <= 1) return;
  for (let i = 1; i <= totalPages; i++) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'page-btn' + (i === data.page ? ' current' : '');
    b.textContent = String(i);
    b.dataset.page = String(i);
    if (i === data.page) b.setAttribute('aria-current', 'page');
    pag.appendChild(b);
  }
}

function updateVideoListHighlight(currentIndex) {
  if (currentIndex === lastHighlightedIndex) return;
  lastHighlightedIndex = currentIndex;
  $$('#video-list-body tr').forEach(tr => {
    const btn = tr.querySelector('.btn-play');
    tr.className = btn && Number(btn.dataset.index) === currentIndex ? 'current' : '';
  });
}

// Monitor: polling

function startPolling() {
  pollOnce();
  pollTimer = setInterval(pollOnce, 2000);
}

function stopPolling() {
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
}

async function pollOnce() {
  try {
    const [status, events, updateStatus] = await Promise.all([
      api('/api/status'),
      api('/api/events'),
      api('/api/update/status').catch(() => null),
    ]);
    pollFailures = 0;
    $('#connection-lost').classList.add('hidden');
    renderStream(status);
    renderNowPlaying(status);
    renderEvents(events);
    if (updateStatus) renderUpdateBanner(updateStatus);
  } catch (err) {
    console.error('Poll error:', err);
    pollFailures++;
    if (pollFailures >= 2) $('#connection-lost').classList.remove('hidden');
  }
}

function renderEvents(events) {
  const container = $('#event-log');
  container.innerHTML = '';
  if (!events.length) {
    container.innerHTML = '<div class="event-empty">No events yet</div>';
    return;
  }
  for (const e of [...events].reverse()) {
    const row = document.createElement('div');
    row.className = 'event-entry';
    const time = document.createElement('span');
    time.className = 'event-time';
    time.textContent = new Date(e.timestamp).toLocaleTimeString();
    row.append(time, document.createTextNode(e.message));
    container.appendChild(row);
  }
}

// Monitor: logs

async function loadLogList() {
  const select = $('#log-select');
  const view = $('#log-view');
  try {
    const files = await api('/api/logs');
    if (!files.length) {
      select.innerHTML = '<option value="">(no log files yet)</option>';
      view.textContent = 'No log files yet.';
      return;
    }
    const prev = select.value;
    select.innerHTML = '';
    for (const f of files) {
      const opt = document.createElement('option');
      opt.value = f.source + '/' + f.name;
      opt.textContent = `${f.source}: ${f.name} (${formatBytes(f.size)}, ${new Date(f.mtime).toLocaleString()})`;
      select.appendChild(opt);
    }
    select.value = files.some(f => f.source + '/' + f.name === prev) ? prev : select.options[0].value;
    await viewLog(select.value);
  } catch (err) {
    select.innerHTML = '<option value="">(could not load)</option>';
    view.textContent = 'Could not list the log files: ' + err.message;
  }
}

async function viewLog(value) {
  const view = $('#log-view');
  if (!value) {
    view.textContent = 'Select a log file to view.';
    return;
  }
  const [source, name] = value.split('/');
  view.textContent = 'Loading...';
  try {
    const res = await fetch(`/api/logs/${encodeURIComponent(source)}/${encodeURIComponent(name)}`);
    if (!res.ok) {
      const body = await res.json().catch(() => ({}));
      throw new Error(body.error || `HTTP ${res.status}`);
    }
    view.textContent = (await res.text()) || '(empty)';
    // Errors are usually at the end.
    view.scrollTop = view.scrollHeight;
  } catch (err) {
    view.textContent = 'Could not load the log: ' + err.message;
  }
}

function formatBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

// Settings tab

let settingsLoaded = false;

async function loadSettings() {
  if (settingsLoaded) return;
  try {
    const cfg = await api('/api/config');
    $('#set-playlists-list').innerHTML = '';
    (cfg.playlists || []).forEach(p => addPlaylistRow('#set-playlists-list', p.id, p.name));
    $('#set-ingest').value = cfg.supervisor.ingestUrl || '';
    renderStreamKeyState(cfg.supervisor.streamKeySet === true);
    $('#twitch-enabled-toggle').checked = cfg.twitchLivenessEnabled || false;
    $('#set-twitch-client-id').value = cfg.twitchClientId || '';
    $('#set-twitch-client-secret').value = cfg.twitchClientSecret || '';
    $('#set-twitch-channel').value = cfg.twitchChannel || '';
    $('#auto-open-toggle').checked = cfg.autoOpenAdmin !== false;
    settingsLoaded = true;
  } catch (err) {
    showToast('Could not load settings: ' + err.message);
  }
}

function renderStreamKeyState(isSet) {
  $('#set-stream-key-state').textContent = isSet ? 'A stream key is set.' : 'No stream key is set. The stream cannot start without one.';
  $('#set-stream-key-btn').textContent = isSet ? 'Replace' : 'Set';
  $('#set-stream-key').placeholder = isSet ? 'Paste a new key to replace it' : 'Paste your Twitch stream key';
}

async function handleStreamKeySet() {
  const input = $('#set-stream-key');
  const btn = $('#set-stream-key-btn');
  if (!input.value.trim()) {
    showToast('Paste the stream key first.');
    return;
  }
  btn.disabled = true;
  try {
    await post('/api/stream-key', { key: input.value });
    input.value = '';
    renderStreamKeyState(true);
    showToast('Stream key saved.', 'success');
  } catch (err) {
    showToast('Could not save the stream key: ' + err.message);
  } finally {
    btn.disabled = false;
  }
}

async function handleSettingsSave() {
  const playlists = collectPlaylists('#set-playlists-list');
  if (playlists === null) return;
  if (playlists.length === 0) {
    showToast('Add at least one playlist.');
    return;
  }
  const btn = $('#set-btn');
  const body = {
    playlists,
    supervisor: withoutBlanks({ ingestUrl: $('#set-ingest').value.trim() }),
    twitchLivenessEnabled: $('#twitch-enabled-toggle').checked,
    twitchClientId: $('#set-twitch-client-id').value.trim(),
    twitchClientSecret: $('#set-twitch-client-secret').value,
    twitchChannel: $('#set-twitch-channel').value.trim(),
  };
  try {
    btn.disabled = true;
    btn.textContent = 'Saving...';
    const r = await saveConfig(body);
    btn.textContent = 'Saved';
    if (r.restartRequired) $('#set-restart-note').classList.remove('hidden');
    settingsLoaded = false;
    loadSettings();
    loadPlaylistSelector();
    loadVideoList(1);
  } catch (err) {
    showToast('Could not save: ' + err.message);
  }
  setTimeout(() => { btn.textContent = 'Save Settings'; btn.disabled = false; }, 1500);
}

function copyRedirectUrl(btn) {
  navigator.clipboard.writeText('https://localhost').then(() => {
    const original = btn.textContent;
    btn.textContent = 'Copied';
    btn.classList.add('btn-copied');
    setTimeout(() => {
      btn.textContent = original;
      btn.classList.remove('btn-copied');
    }, 2000);
  }).catch(() => showToast('Copy failed. Select the text and press Ctrl+C.'));
}

// Autostart

async function loadAutostart() {
  try {
    const { enabled } = await api('/api/autostart');
    $('#autostart-toggle').checked = enabled;
  } catch {
    // Leaves the toggle off; changing it reports any error.
  }
}

async function handleAutostartToggle() {
  const enabled = $('#autostart-toggle').checked;
  try {
    await post('/api/autostart', { enabled });
  } catch (err) {
    showToast('Could not change autostart: ' + err.message);
    $('#autostart-toggle').checked = !enabled;
  }
}

async function handleAutoOpenToggle() {
  const input = $('#auto-open-toggle');
  try {
    await saveConfig({ autoOpenAdmin: input.checked });
  } catch (err) {
    showToast('Could not change the setting: ' + err.message);
    input.checked = !input.checked;
  }
}

// Playback tab

let playbackSettingsLoaded = false;

const PLAYBACK_FIELDS = [
  { id: '#pb-read-ahead', key: 'readAheadSeconds', min: 2, label: 'Read-ahead' },
  { id: '#pb-cache', key: 'cacheBudgetMb', min: 50, label: 'Download cache', integer: true },
  { id: '#pb-freeze', key: 'freezeSeconds', min: 5, label: 'Frozen picture' },
  { id: '#pb-silence', key: 'silenceSeconds', min: 5, label: 'Silence' },
];

async function loadPlaybackSettings() {
  if (playbackSettingsLoaded) return;
  try {
    const { supervisor, ytdlCookiesFromBrowser } = await api('/api/config');
    $('#pb-encoder').value = supervisor.encoder;
    $('#pb-ytdl-cookies').value = ytdlCookiesFromBrowser || '';
    $('#pb-overlay').checked = supervisor.overlay !== false;
    $('#pb-player-client').value = supervisor.playerClient || '';
    for (const f of PLAYBACK_FIELDS) $(f.id).value = String(supervisor[f.key]);
    playbackSettingsLoaded = true;
  } catch (err) {
    showToast('Could not load playback settings: ' + err.message);
  }
}

async function handlePlaybackSave() {
  const btn = $('#pb-save-btn');
  const supervisor = withoutBlanks({
    encoder: $('#pb-encoder').value,
    overlay: $('#pb-overlay').checked,
    playerClient: $('#pb-player-client').value.trim(),
  });
  for (const f of PLAYBACK_FIELDS) {
    const input = $(f.id);
    const n = Number(input.value);
    if (input.value === '' || !Number.isFinite(n) || n < f.min || (f.integer && !Number.isInteger(n))) {
      input.classList.add('invalid');
      showToast(`${f.label} must be ${f.integer ? 'a whole number, ' : ''}at least ${f.min}.`);
      return;
    }
    input.classList.remove('invalid');
    supervisor[f.key] = n;
  }
  try {
    btn.disabled = true;
    btn.textContent = 'Saving...';
    // Blank cookies is a real value (none), so it is sent as is.
    const r = await saveConfig({ supervisor, ytdlCookiesFromBrowser: $('#pb-ytdl-cookies').value.trim() });
    btn.textContent = 'Saved';
    if (r.restartRequired) $('#pb-restart-note').classList.remove('hidden');
    playbackSettingsLoaded = false;
    loadPlaybackSettings();
  } catch (err) {
    showToast('Could not save: ' + err.message);
  }
  setTimeout(() => { btn.textContent = 'Save Playback Settings'; btn.disabled = false; }, 1500);
}

// Webhooks tab

let webhookDefaults = null;
let webhookSettingsLoaded = false;

// Labels for the known events; an event the server adds later shows under its key.
const EVENT_INFO = {
  error: ['Video Errors', 'A video failed and is being retried', 'warn'],
  skip: ['Video Skips', 'A video was skipped after repeated failures', 'warn'],
  recovery: ['Recovery', 'The supervisor is answering again after being down', 'info'],
  critical: ['Critical Alerts', 'The stream cannot recover by itself and needs you', 'error'],
  resume: ['Playback Resumed', 'The stream resumed a video after a restart', 'info'],
  streamDrop: ['Encoder Stopped', 'The encoder stopped and is restarting', 'warn'],
  streamRestart: ['Encoder Restarted', 'The encoder is running again after stopping', 'info'],
  outputFrozen: ['Frozen or Silent Output', 'The picture froze or the audio went silent', 'warn'],
  twitchMismatch: ['Twitch Mismatch', 'The encoder runs but the channel is offline or frozen on Twitch', 'warn'],
  twitchRestart: ['Twitch Restarted', 'The stream restarted after a Twitch mismatch', 'info'],
};

const SAMPLE_VARS = {
  videoIndex: 3, videoId: 'dQw4w9WgXcQ', errorCode: 'early-eof', attempt: 2, reason: 'unavailable: Video unavailable',
  step: 'Supervisor answering again', message: 'The encoder has stopped 3 times in a row.', maxAttempts: 3, attempts: 1,
  kind: 'freeze', channel: 'mychannel',
};

function eventInfo(key) {
  return EVENT_INFO[key] || [key, '', 'info'];
}

async function loadWebhookSettings() {
  if (webhookSettingsLoaded) return;
  try {
    const [cfg, defaults] = await Promise.all([api('/api/config'), api('/api/discord/defaults')]);
    webhookDefaults = defaults;
    const discord = cfg.discord || {};
    const keys = Object.keys(defaults.templates);

    $('#wh-url').value = discord.webhookUrl === '********' ? '' : (discord.webhookUrl || '');
    $('#wh-url').placeholder = discord.webhookUrl === '********' ? 'Webhook set (leave blank to keep it)' : 'https://discord.com/api/webhooks/...';
    $('#wh-bot-name').value = discord.botName || '';
    $('#wh-avatar-url').value = discord.avatarUrl || '';
    $('#wh-role-ping').value = discord.rolePing || '';

    buildEventToggles(keys, discord.events || {});
    buildTemplateEditors(keys, defaults, discord.templates || {});
    const select = $('#wh-preview-select');
    const prev = select.value;
    select.innerHTML = '';
    for (const key of keys) {
      const opt = document.createElement('option');
      opt.value = key;
      opt.textContent = eventInfo(key)[0];
      select.appendChild(opt);
    }
    if (keys.includes(prev)) select.value = prev;
    updatePreview();
    webhookSettingsLoaded = true;
  } catch (err) {
    $('#wh-events-container').textContent = 'Could not load webhook settings: ' + err.message;
  }
}

function buildEventToggles(keys, events) {
  const container = $('#wh-events-container');
  container.innerHTML = '';
  for (const key of keys) {
    const [label, desc] = eventInfo(key);
    const row = document.createElement('div');
    row.className = 'wh-toggle';
    row.innerHTML =
      `<div><div class="toggle-label">${escapeHtml(label)}</div><div class="toggle-desc">${escapeHtml(desc)}</div></div>` +
      `<label class="toggle"><input type="checkbox" id="wh-evt-${escapeHtml(key)}" aria-label="${escapeHtml(label)}"><span class="toggle-slider"></span></label>`;
    row.querySelector('input').checked = events[key] !== false;
    container.appendChild(row);
  }
}

function buildTemplateEditors(keys, defaults, current) {
  const container = $('#wh-templates-container');
  container.innerHTML = '';
  for (const key of keys) {
    const group = document.createElement('div');
    group.className = 'wh-template-group';

    const header = document.createElement('div');
    header.className = 'wh-template-header';
    const label = document.createElement('span');
    label.className = 'wh-template-label';
    label.textContent = eventInfo(key)[0];
    const resetBtn = document.createElement('button');
    resetBtn.type = 'button';
    resetBtn.className = 'btn-reset';
    resetBtn.textContent = 'Reset';
    resetBtn.addEventListener('click', () => resetTemplate(key));
    header.append(label, resetBtn);
    group.appendChild(header);

    const vars = defaults.variables[key] || [];
    if (vars.length > 0) {
      const chipsRow = document.createElement('div');
      chipsRow.className = 'wh-chips';
      for (const v of vars) {
        const chip = document.createElement('button');
        chip.type = 'button';
        chip.className = 'wh-chip';
        chip.textContent = `{${v}}`;
        chip.addEventListener('click', () => insertVariable(key, v));
        chipsRow.appendChild(chip);
      }
      group.appendChild(chipsRow);
    }

    const textarea = document.createElement('textarea');
    textarea.className = 'wh-template-input';
    textarea.id = `wh-tpl-${key}`;
    textarea.setAttribute('aria-label', eventInfo(key)[0] + ' template');
    textarea.value = current[key] || defaults.templates[key] || '';
    textarea.rows = 2;
    textarea.addEventListener('input', updatePreview);
    group.appendChild(textarea);

    container.appendChild(group);
  }
}

function insertVariable(eventType, varName) {
  const textarea = document.getElementById(`wh-tpl-${eventType}`);
  if (!textarea) return;
  const start = textarea.selectionStart;
  const insert = `{${varName}}`;
  textarea.value = textarea.value.substring(0, start) + insert + textarea.value.substring(textarea.selectionEnd);
  textarea.selectionStart = textarea.selectionEnd = start + insert.length;
  textarea.focus();
  updatePreview();
}

function resetTemplate(eventType) {
  const textarea = document.getElementById(`wh-tpl-${eventType}`);
  if (!webhookDefaults || !textarea) return;
  textarea.value = webhookDefaults.templates[eventType] || '';
  updatePreview();
}

function updatePreview() {
  const eventType = $('#wh-preview-select').value;
  if (!eventType) return;
  const level = eventInfo(eventType)[2];
  const emoji = { info: 'ℹ️', warn: '⚠️', error: '🚨' };
  const colors = { info: '#3498db', warn: '#f1c40f', error: '#e74c3c' };
  const textarea = document.getElementById(`wh-tpl-${eventType}`);
  const template = textarea ? textarea.value : '';
  const rendered = template.replace(/\{(\w+)\}/g, (match, key) => (key in SAMPLE_VARS ? String(SAMPLE_VARS[key]) : match));

  // Discord's embed title is always StreamLoop; the bot name sets the sender, and notifyEvent sends no fields.
  $('#wh-preview-bar').style.background = colors[level];
  $('#wh-preview-title').textContent = `${emoji[level]} StreamLoop`;
  $('#wh-preview-desc').textContent = rendered;
}

async function handleWebhookSave() {
  const btn = $('#wh-save-btn');
  const discord = {
    webhookUrl: $('#wh-url').value.trim() || '********',
    botName: $('#wh-bot-name').value.trim(),
    avatarUrl: $('#wh-avatar-url').value.trim(),
    rolePing: $('#wh-role-ping').value.trim(),
    events: {},
    templates: {},
  };
  for (const key of Object.keys((webhookDefaults && webhookDefaults.templates) || {})) {
    const el = document.getElementById(`wh-evt-${key}`);
    if (el) discord.events[key] = el.checked;
    const tpl = document.getElementById(`wh-tpl-${key}`);
    if (tpl) discord.templates[key] = tpl.value;
  }
  try {
    btn.disabled = true;
    btn.textContent = 'Saving...';
    await saveConfig({ discord });
    btn.textContent = 'Saved';
    webhookSettingsLoaded = false;
  } catch (err) {
    showToast('Could not save: ' + err.message);
  }
  setTimeout(() => { btn.textContent = 'Save Webhook Settings'; btn.disabled = false; }, 1500);
}

async function testDiscordWebhook() {
  const resultEl = $('#wh-test-result');
  resultEl.classList.remove('hidden', 'valid', 'invalid');
  resultEl.textContent = 'Sending a test message...';
  try {
    await post('/api/discord/test');
    resultEl.textContent = 'Test message sent. Check your Discord channel.';
    resultEl.classList.add('valid');
  } catch (err) {
    resultEl.textContent = err.message || 'Could not send the test message.';
    resultEl.classList.add('invalid');
  }
}

// Tab switching

function switchTab(tabName) {
  $$('.tab').forEach(t => t.classList.toggle('active', t.dataset.tab === tabName));
  $$('.tab-panel').forEach(p => p.classList.toggle('hidden', p.id !== 'panel-' + tabName));
  if (tabName === 'settings') loadSettings();
  if (tabName === 'webhooks') loadWebhookSettings();
  if (tabName === 'playback') loadPlaybackSettings();
}

// Helpers

function formatDuration(ms) {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s % 60}s`;
  return `${s}s`;
}

/** Drops blank strings, so the server keeps the saved value instead of storing ''. */
function withoutBlanks(obj) {
  return Object.fromEntries(Object.entries(obj).filter(([, v]) => v !== ''));
}

function escapeHtml(str) {
  const d = document.createElement('div');
  d.textContent = str;
  return d.innerHTML.replace(/"/g, '&quot;');
}

function showToast(message, type = 'error') {
  const toast = document.createElement('div');
  toast.className = `toast toast-${type}`;
  toast.textContent = message;
  $('#toast-container').appendChild(toast);
  setTimeout(() => {
    toast.style.opacity = '0';
    setTimeout(() => toast.remove(), 200);
  }, 4000);
}

// Updates

function renderUpdateBanner(status) {
  const banner = $('#update-banner');
  const text = $('#update-banner-text');
  const btn = $('#update-btn');
  if (!status.updateAvailable) {
    banner.classList.add('hidden');
    return;
  }
  banner.classList.remove('hidden');
  if (status.status === 'downloading') {
    text.textContent = `Downloading v${status.latestVersion}...`;
    btn.disabled = true;
    btn.textContent = 'Downloading...';
  } else if (status.status === 'extracting') {
    text.textContent = `Installing v${status.latestVersion}...`;
    btn.disabled = true;
    btn.textContent = 'Installing...';
  } else if (status.status === 'ready') {
    text.textContent = 'Update installed. Restarting...';
    btn.disabled = true;
    btn.textContent = 'Restarting...';
  } else if (status.status === 'error') {
    text.textContent = `Update failed: ${status.error}`;
    btn.disabled = false;
    btn.textContent = 'Retry';
  } else {
    text.textContent = `Version ${status.latestVersion} is available (current: ${status.currentVersion})`;
    btn.textContent = status.isDevMode ? 'Dev Mode' : 'Update Now';
    btn.disabled = !!status.isDevMode;
  }
}

async function handleUpdate() {
  const btn = $('#update-btn');
  btn.disabled = true;
  btn.textContent = 'Starting...';
  try {
    await post('/api/update/apply');
    pollUpdateProgress();
  } catch (err) {
    showToast('Update failed: ' + err.message);
    btn.disabled = false;
    btn.textContent = 'Retry';
  }
}

async function pollUpdateProgress() {
  try {
    const status = await api('/api/update/status');
    renderUpdateBanner(status);
    if (status.status === 'ready') {
      setTimeout(waitForRestart, 3000);
      return;
    }
    if (status.status === 'error') return;
    setTimeout(pollUpdateProgress, 1500);
  } catch {
    // The server went down to restart.
    setTimeout(waitForRestart, 2000);
  }
}

let restartAttempts = 0;
async function waitForRestart() {
  restartAttempts++;
  if (restartAttempts > 30) {
    showToast('The server did not come back after the update. Check the StreamLoop window for errors.');
    const btn = $('#update-btn');
    btn.disabled = false;
    btn.textContent = 'Retry';
    restartAttempts = 0;
    return;
  }
  try {
    await api('/api/status');
    restartAttempts = 0;
    window.location.reload();
  } catch {
    setTimeout(waitForRestart, 2000);
  }
}

async function handleCheckUpdate() {
  const btn = $('#check-update-btn');
  const result = $('#check-update-result');
  btn.disabled = true;
  btn.textContent = 'Checking...';
  result.textContent = '';
  try {
    const status = await post('/api/update/check');
    const settingsBtn = $('#settings-update-btn');
    if (status.updateAvailable) {
      result.textContent = `v${status.latestVersion} is available`;
      settingsBtn.classList.toggle('hidden', !!status.isDevMode);
      renderUpdateBanner(status);
    } else {
      result.textContent = `Up to date (v${status.currentVersion})`;
      settingsBtn.classList.add('hidden');
    }
  } catch (err) {
    result.textContent = 'Check failed: ' + err.message;
  }
  btn.textContent = 'Check for Updates';
  btn.disabled = false;
}

async function handleYtdlpUpdate() {
  const btn = $('#update-ytdlp-btn');
  const result = $('#update-ytdlp-result');
  btn.disabled = true;
  btn.textContent = 'Updating...';
  result.textContent = '';
  try {
    const res = await post('/api/yt-dlp/update');
    result.textContent = `Updated to yt-dlp ${res.version}`;
  } catch (err) {
    result.textContent = 'Update failed: ' + err.message;
  }
  btn.textContent = 'Update yt-dlp';
  btn.disabled = false;
}

function handleShutdown(e) {
  const btn = e.currentTarget;
  if (!confirm('Shut down the dashboard server? The stream keeps running, but this page stops working until StreamLoop starts again.')) return;
  post('/api/shutdown').catch((err) => showToast(err.message));
  btn.textContent = 'Shutting down...';
  btn.disabled = true;
}

// Boot

async function boot() {
  // Every POST needs the token.
  await fetchApiToken();

  $('#wiz-back').addEventListener('click', wizBack);
  $('#wiz-next').addEventListener('click', wizNext);
  $('#wiz-add-playlist').addEventListener('click', () => addPlaylistRow('#wiz-playlists-list', '', ''));

  $$('.tab').forEach(t => t.addEventListener('click', () => switchTab(t.dataset.tab)));
  initMonitorControls();

  $('#pb-save-btn').addEventListener('click', handlePlaybackSave);
  $$('.restart-stream-btn').forEach(b => b.addEventListener('click', (e) => runAction(e.currentTarget, async () => {
    await restartStream();
    $$('.restart-note').forEach(n => n.classList.add('hidden'));
  })));

  $('#wh-test-btn').addEventListener('click', testDiscordWebhook);
  $('#wh-save-btn').addEventListener('click', handleWebhookSave);
  $('#wh-preview-select').addEventListener('change', updatePreview);

  $('#set-add-playlist').addEventListener('click', () => addPlaylistRow('#set-playlists-list', '', ''));
  $('#set-stream-key-btn').addEventListener('click', handleStreamKeySet);
  $('#set-btn').addEventListener('click', handleSettingsSave);
  $('#copy-redirect-btn').addEventListener('click', (e) => copyRedirectUrl(e.currentTarget));
  $('#autostart-toggle').addEventListener('change', handleAutostartToggle);
  $('#auto-open-toggle').addEventListener('change', handleAutoOpenToggle);
  $('#check-update-btn').addEventListener('click', handleCheckUpdate);
  $('#settings-update-btn').addEventListener('click', handleUpdate);
  $('#update-btn').addEventListener('click', handleUpdate);
  $('#update-ytdlp-btn').addEventListener('click', handleYtdlpUpdate);
  $('#shutdown-btn').addEventListener('click', handleShutdown);

  init();
}

// A module runs after parsing, so the DOM is ready.
boot();
