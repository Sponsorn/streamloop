// Admin Dashboard Client Logic

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => document.querySelectorAll(sel);

let apiToken = '';
let currentView = 'loading'; // 'wizard' | 'dashboard'
let pollTimer = null;
let pollFailures = 0;
let lastHeartbeatAt = 0;
let heartbeatIntervalMs = 5000;
let heartbeatCountdownTimer = null;
let playerConnected = false;

// --- Initialization ---

async function init() {
  try {
    const status = await api('/api/status');
    if (status.firstRun) {
      showWizard();
    } else {
      showDashboard();
    }
  } catch (err) {
    console.error('Failed to init:', err);
    showWizard();
  }
}

function showWizard() {
  currentView = 'wizard';
  $('#view-wizard').classList.remove('hidden');
  $('#view-dashboard').classList.add('hidden');
  stopPolling();
  wizInitWizard();
}

function showDashboard() {
  currentView = 'dashboard';
  $('#view-wizard').classList.add('hidden');
  $('#view-dashboard').classList.remove('hidden');
  startPolling();
  loadSettings();
  loadAutostart();
}

// --- API helper ---

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

// --- Playlist row helpers ---

function addSetPlaylist(id, name) {
  addPlaylistRow('#set-playlists-list', id || '', name || '');
}

function addPlaylistRow(containerSel, id, name) {
  const container = $(containerSel);
  const row = document.createElement('div');
  row.className = 'playlist-row';
  row.innerHTML =
    `<input type="text" class="pl-id" placeholder="PLxxxxxxxxxxxxxxxx" value="${escapeHtml(id)}">` +
    `<input type="text" class="pl-name" placeholder="Name (optional)" value="${escapeHtml(name)}">` +
    `<button type="button" class="btn-remove" title="Remove">&times;</button>`;
  row.querySelector('.btn-remove').addEventListener('click', () => {
    row.remove();
  });
  container.appendChild(row);
}

function extractPlaylistId(raw) {
  try {
    const url = new URL(raw);
    if (url.hostname.includes('youtube.com') || url.hostname.includes('youtu.be')) {
      const list = url.searchParams.get('list');
      if (list) return list;
      return null; // YouTube URL without a playlist
    }
  } catch { /* not a URL, treat as bare ID */ }
  return raw;
}

function collectPlaylists(containerSel) {
  const rows = $$(containerSel + ' .playlist-row');
  const playlists = [];
  let hasError = false;
  for (const row of rows) {
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
  if (hasError) return null;
  return playlists;
}

// --- Wizard (multi-step onboarding) ---

const WIZ_TOTAL_STEPS = 6;
let wizStep = 1;
const wizState = {
  obsWebsocketPassword: '',
  obsBrowserSourceName: 'Playlist Player',
  playlists: [],
  discordWebhookUrl: '',
  autostart: false,
};

function wizInitWizard() {
  // Load defaults from server config if available
  api('/api/config').then(cfg => {
    if (cfg.obsWebsocketPassword && cfg.obsWebsocketPassword !== '********') {
      wizState.obsWebsocketPassword = cfg.obsWebsocketPassword;
    }
    if (cfg.obsBrowserSourceName) {
      wizState.obsBrowserSourceName = cfg.obsBrowserSourceName;
    }
    if (cfg.discord && cfg.discord.webhookUrl && cfg.discord.webhookUrl !== '********') {
      wizState.discordWebhookUrl = cfg.discord.webhookUrl;
    }
    const playlists = cfg.playlists || [];
    const hasReal = playlists.some(p => !p.id.includes('xxxxx'));
    if (hasReal) {
      wizState.playlists = playlists;
    }
  }).catch(() => {});
  // Set player URL
  $('#wiz-player-url').textContent = window.location.origin;
  wizGoTo(1);
}

function wizGoTo(step) {
  // Collect data from current step before leaving
  if (wizStep !== step) wizCollectStepData();
  wizStep = step;
  // Show/hide step panels
  $$('.wiz-step').forEach(el => {
    el.classList.toggle('hidden', Number(el.dataset.step) !== step);
  });
  // Update progress dots and lines
  $$('.wiz-progress-dot').forEach(dot => {
    const s = Number(dot.dataset.step);
    dot.classList.remove('active', 'completed');
    if (s === step) dot.classList.add('active');
    else if (s < step) dot.classList.add('completed');
  });
  $$('.wiz-progress-line').forEach(line => {
    const after = Number(line.dataset.after);
    line.classList.toggle('completed', after < step);
  });
  // Update nav buttons
  const backBtn = $('#wiz-back');
  const nextBtn = $('#wiz-next');
  backBtn.classList.toggle('invisible', step === 1);
  if (step === WIZ_TOTAL_STEPS) {
    nextBtn.textContent = 'Finish';
  } else if (step === 1) {
    nextBtn.textContent = 'Get Started';
  } else {
    nextBtn.textContent = 'Continue';
  }
  // Init step-specific UI
  wizInitStep(step);
}

function wizNext() {
  if (wizStep === WIZ_TOTAL_STEPS) {
    runVerification();
    return;
  }
  if (!wizValidateStep()) return;
  wizGoTo(wizStep + 1);
}

function wizBack() {
  if (wizStep > 1) wizGoTo(wizStep - 1);
}

function wizCollectStepData() {
  switch (wizStep) {
    case 2:
      wizState.obsWebsocketPassword = $('#wiz-obs-pass').value;
      break;
    case 3:
      wizState.obsBrowserSourceName = $('#wiz-source').value.trim();
      break;
    case 4: {
      const pl = collectPlaylists('#wiz-playlists-list');
      if (pl) wizState.playlists = pl;
      break;
    }
    case 5:
      wizState.discordWebhookUrl = $('#wiz-discord').value.trim();
      wizState.autostart = $('#wiz-autostart').checked;
      break;
  }
}

function wizInitStep(step) {
  switch (step) {
    case 2:
      $('#wiz-obs-pass').value = wizState.obsWebsocketPassword;
      $('#wiz-obs-result').textContent = '';
      $('#wiz-obs-result').className = 'wiz-test-result';
      break;
    case 3:
      $('#wiz-player-url').textContent = window.location.origin;
      $('#wiz-source').value = wizState.obsBrowserSourceName;
      $('#wiz-player-result').textContent = '';
      $('#wiz-player-result').className = 'wiz-test-result';
      break;
    case 4: {
      const container = $('#wiz-playlists-list');
      container.innerHTML = '';
      if (wizState.playlists.length > 0) {
        wizState.playlists.forEach(p => wizAddPlaylist(p.id, p.name || ''));
      } else {
        wizAddPlaylist('', '');
      }
      break;
    }
    case 5:
      $('#wiz-discord').value = wizState.discordWebhookUrl;
      $('#wiz-autostart').checked = wizState.autostart;
      break;
    case 6:
      // Reset verification icons
      ['config', 'obs', 'player'].forEach(key => {
        const icon = $(`#wiz-chk-${key}`);
        icon.textContent = '\u25CF';
        icon.className = 'wiz-verify-icon';
      });
      $('#wiz-verify-status').textContent = 'Click Finish to save your configuration and verify connections.';
      $('#wiz-next').textContent = 'Finish';
      $('#wiz-next').disabled = false;
      break;
  }
}

function wizValidateStep() {
  switch (wizStep) {
    case 3: {
      const source = $('#wiz-source').value.trim();
      if (!source) {
        $('#wiz-source').classList.add('invalid');
        showToast('Enter the name of the Browser Source you created in OBS.');
        return false;
      }
      $('#wiz-source').classList.remove('invalid');
      return true;
    }
    case 4: {
      const pl = collectPlaylists('#wiz-playlists-list');
      if (pl === null) return false; // validation error already shown
      wizState.playlists = pl;
      if (wizState.playlists.length === 0) {
        showToast('Add at least one playlist ID.');
        return false;
      }
      return true;
    }
    default:
      return true;
  }
}

function wizAddPlaylist(id, name) {
  addPlaylistRow('#wiz-playlists-list', id || '', name || '');
}

async function wizTestObs() {
  const resultEl = $('#wiz-obs-result');
  const btn = $('#wiz-test-obs');
  resultEl.textContent = 'Testing...';
  resultEl.className = 'wiz-test-result pending';
  btn.disabled = true;

  // Save just the OBS password to config (keeps placeholder playlist so isFirstRun stays true)
  const password = $('#wiz-obs-pass').value;
  try {
    await api('/api/config', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ obsWebsocketPassword: password }),
    });
  } catch {
    // Non-critical, continue with test
  }

  // Wait a moment for server to reconnect, then poll status
  await sleep(2000);
  try {
    const status = await api('/api/status');
    if (status.obsConnected) {
      resultEl.textContent = 'Connected to OBS!';
      resultEl.className = 'wiz-test-result success';
    } else {
      resultEl.textContent = 'Could not connect. Check OBS is open and WebSocket is enabled.';
      resultEl.className = 'wiz-test-result failure';
    }
  } catch {
    resultEl.textContent = 'Could not reach server.';
    resultEl.className = 'wiz-test-result failure';
  }
  btn.disabled = false;
}

async function wizTestPlayer() {
  const resultEl = $('#wiz-player-result');
  const btn = $('#wiz-test-player');
  resultEl.textContent = 'Checking...';
  resultEl.className = 'wiz-test-result pending';
  btn.disabled = true;

  // Save the source name so server knows what to look for
  const sourceName = $('#wiz-source').value.trim();
  if (sourceName) {
    try {
      await api('/api/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ obsBrowserSourceName: sourceName }),
      });
    } catch {
      // Non-critical
    }
  }

  await sleep(2000);
  try {
    const status = await api('/api/status');
    if (status.playerConnected) {
      resultEl.textContent = 'Player is connected!';
      resultEl.className = 'wiz-test-result success';
    } else {
      resultEl.textContent = 'Player not detected. Make sure the Browser Source is active and the URL is correct.';
      resultEl.className = 'wiz-test-result failure';
    }
  } catch {
    resultEl.textContent = 'Could not reach server.';
    resultEl.className = 'wiz-test-result failure';
  }
  btn.disabled = false;
}

function wizCopyUrl() {
  const url = $('#wiz-player-url').textContent;
  navigator.clipboard.writeText(url).then(() => {
    showToast('Copied to clipboard!', 'success');
  }).catch(() => {
    // Fallback: select the text
    const range = document.createRange();
    range.selectNodeContents($('#wiz-player-url'));
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
    showToast('Press Ctrl+C to copy', 'success');
  });
}

async function runVerification() {
  const nextBtn = $('#wiz-next');
  const statusEl = $('#wiz-verify-status');
  nextBtn.disabled = true;
  nextBtn.textContent = 'Saving...';

  // Collect final data
  wizCollectStepData();

  // Step 1: Save config
  setVerifyIcon('config', 'spin');
  statusEl.textContent = 'Saving configuration...';

  const body = {
    playlists: wizState.playlists,
    obsBrowserSourceName: wizState.obsBrowserSourceName,
    discord: { webhookUrl: wizState.discordWebhookUrl },
  };
  if (wizState.obsWebsocketPassword) {
    body.obsWebsocketPassword = wizState.obsWebsocketPassword;
  }

  try {
    await api('/api/config', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    setVerifyIcon('config', 'pass');
  } catch (err) {
    setVerifyIcon('config', 'fail');
    statusEl.textContent = 'Failed to save: ' + err.message;
    nextBtn.disabled = false;
    nextBtn.textContent = 'Retry';
    return;
  }

  // Set autostart if requested
  if (wizState.autostart) {
    try {
      await api('/api/autostart', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled: true }),
      });
    } catch {
      // Non-critical
    }
  }

  // Step 2: Check connections (poll a few times)
  setVerifyIcon('obs', 'spin');
  setVerifyIcon('player', 'spin');
  statusEl.textContent = 'Checking connections...';
  nextBtn.textContent = 'Verifying...';

  let obsOk = false;
  let playerOk = false;
  for (let i = 0; i < 5; i++) {
    await sleep(2000);
    try {
      const status = await api('/api/status');
      if (status.obsConnected) { obsOk = true; setVerifyIcon('obs', 'pass'); }
      if (status.playerConnected) { playerOk = true; setVerifyIcon('player', 'pass'); }
      if (obsOk && playerOk) break;
    } catch {
      // Ignore transient errors
    }
  }

  if (!obsOk) setVerifyIcon('obs', 'fail');
  if (!playerOk) setVerifyIcon('player', 'fail');

  if (obsOk && playerOk) {
    statusEl.textContent = 'Everything looks good!';
  } else {
    const issues = [];
    if (!obsOk) issues.push('OBS WebSocket');
    if (!playerOk) issues.push('Player');
    statusEl.textContent = issues.join(' and ') + ' not connected. You can fix this later in Settings.';
  }

  nextBtn.disabled = false;
  nextBtn.textContent = 'Go to Dashboard';
  nextBtn.onclick = () => showDashboard();
}

function setVerifyIcon(key, state) {
  const icon = $(`#wiz-chk-${key}`);
  icon.className = 'wiz-verify-icon ' + state;
  if (state === 'pass') icon.textContent = '\u2714';
  else if (state === 'fail') icon.textContent = '\u2718';
  else if (state === 'spin') icon.textContent = '\u25CF';
  else icon.textContent = '\u25CF';
}

function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

// --- Dashboard polling ---

function startPolling() {
  pollOnce();
  pollTimer = setInterval(pollOnce, 2000);
}

function stopPolling() {
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
  if (heartbeatCountdownTimer) { clearInterval(heartbeatCountdownTimer); heartbeatCountdownTimer = null; }
}

async function pollOnce() {
  try {
    const [status, state, events, updateStatus] = await Promise.all([
      api('/api/status'),
      api('/api/state'),
      api('/api/events'),
      api('/api/update/status').catch(() => null),
    ]);
    pollFailures = 0;
    $('#connection-lost').classList.add('hidden');
    renderStatus(status);
    renderNowPlaying(state, status.totalVideos, status);
    renderEvents(events);
    if (updateStatus) renderUpdateBanner(updateStatus);
  } catch (err) {
    console.error('Poll error:', err);
    pollFailures++;
    if (pollFailures >= 2) {
      $('#connection-lost').classList.remove('hidden');
      const pill = $('#header-pill');
      pill.textContent = 'Disconnected';
      pill.className = 'status-pill pill-err';
    }
  }
}

// --- Rendering ---

const RECOVERY_LABELS = {
  none: 'Idle',
  retryCurrent: 'Retrying Video',
  refreshSource: 'Refreshing Source',
  toggleVisibility: 'Toggling Source',
  criticalAlert: 'Critical Alert',
};

function renderStatus(s) {
  // Status cards
  setCard('player-status', s.playerConnected ? 'Connected' : 'Disconnected', s.playerConnected ? 'ok' : 'err');
  const obsLabel = !s.obsConnected ? 'Disconnected' : s.obsStreaming ? 'Streaming' : 'Not Live';
  const obsLevel = !s.obsConnected ? 'err' : s.obsStreaming ? 'ok' : 'warn';
  setCard('obs-status', obsLabel, obsLevel);
  const recoveryLabel = RECOVERY_LABELS[s.recoveryStep] || s.recoveryStep;
  setCard('recovery-step', recoveryLabel, s.recoveryStep === 'none' ? 'ok' : 'warn');
  setCard('errors', String(s.consecutiveErrors), s.consecutiveErrors === 0 ? 'ok' : 'warn');

  // Header pill
  const pill = $('#header-pill');
  if (s.playerConnected && s.recoveryStep === 'none') {
    pill.textContent = 'Healthy';
    pill.className = 'status-pill pill-ok';
  } else if (s.recoveryStep !== 'none') {
    pill.textContent = 'Recovering';
    pill.className = 'status-pill pill-warn';
  } else {
    pill.textContent = 'Offline';
    pill.className = 'status-pill pill-err';
  }

  // Heartbeat countdown
  lastHeartbeatAt = s.lastHeartbeatAt;
  heartbeatIntervalMs = s.heartbeatIntervalMs || 5000;
  playerConnected = s.playerConnected;
  updateHeartbeatPill();
  if (!heartbeatCountdownTimer) {
    heartbeatCountdownTimer = setInterval(updateHeartbeatPill, 1000);
  }

  // Uptime
  $('#uptime').textContent = 'Uptime: ' + formatDuration(s.uptimeMs);

  // Playlist info in Now Playing
  if (s.totalPlaylists > 1) {
    $('#np-playlist').textContent = `${s.playlistIndex + 1} of ${s.totalPlaylists}`;
  } else {
    $('#np-playlist').textContent = '1 of 1';
  }
}

function updateHeartbeatPill() {
  const pill = $('#heartbeat-pill');
  if (!playerConnected) {
    pill.style.display = 'none';
    return;
  }
  pill.style.display = '';
  const elapsed = Date.now() - lastHeartbeatAt;
  const remaining = Math.max(0, heartbeatIntervalMs - elapsed);
  const seconds = Math.ceil(remaining / 1000);
  if (remaining > 0) {
    pill.textContent = `Heartbeat: ${seconds}s`;
    pill.className = 'status-pill pill-ok';
  } else {
    const overdue = Math.round(elapsed / 1000);
    pill.textContent = `Heartbeat: ${overdue}s ago`;
    pill.className = elapsed > heartbeatIntervalMs * 3 ? 'status-pill pill-err' : 'status-pill pill-warn';
  }
}

function setCard(id, text, cls) {
  const el = $(`#${id}`);
  el.textContent = text;
  el.className = 'card-value ' + cls;
}

const QUALITY_LABELS = {
  small: '240p',
  medium: '360p',
  large: '480p',
  hd720: '720p',
  hd1080: '1080p',
  hd1440: '1440p',
  hd2160: '4K',
  highres: '4K+',
};

function renderNowPlaying(s, totalVideos, status) {
  $('#np-title').textContent = s.videoTitle || '-';
  $('#np-index').textContent = totalVideos ? `${s.videoIndex + 1} / ${totalVideos}` : `${s.videoIndex + 1}`;
  const vidEl = $('#np-videoid');
  if (s.videoId) {
    vidEl.innerHTML = `<a href="https://www.youtube.com/watch?v=${escapeHtml(s.videoId)}" target="_blank" rel="noopener">${escapeHtml(s.videoId)}</a>`;
  } else {
    vidEl.textContent = '-';
  }
  const timeStr = formatTime(s.currentTime);
  const durationStr = s.videoDuration ? formatTime(s.videoDuration) : '--:--:--';
  $('#np-time').textContent = `${timeStr} / ${durationStr}`;
  const quality = status.playbackQuality;
  const qualityEl = $('#np-quality');
  qualityEl.textContent = quality ? (QUALITY_LABELS[quality] || quality) : '-';
  const QUALITY_RANKS = { small: 0, medium: 1, large: 2, hd720: 3, hd1080: 4, hd1440: 5, hd2160: 6, highres: 7 };
  if (quality && (QUALITY_RANKS[quality] ?? 99) < 3) {
    qualityEl.classList.add('warn');
  } else {
    qualityEl.classList.remove('warn');
  }
  $('#np-updated').textContent = s.updatedAt ? new Date(s.updatedAt).toLocaleTimeString() : '-';
}

function renderEvents(events) {
  const container = $('#event-log');
  if (!events.length) {
    container.innerHTML = '<div class="event-empty">No events yet</div>';
    return;
  }
  // Show newest first
  const reversed = [...events].reverse();
  container.innerHTML = reversed.map(e =>
    `<div class="event-entry"><span class="event-time">${new Date(e.timestamp).toLocaleTimeString()}</span>${escapeHtml(e.message)}</div>`
  ).join('');
}

// --- Settings tab ---

let settingsLoaded = false;

async function loadSettings() {
  if (settingsLoaded) return;
  try {
    const cfg = await api('/api/config');
    const container = $('#set-playlists-list');
    container.innerHTML = '';
    (cfg.playlists || []).forEach(p => addSetPlaylist(p.id, p.name || ''));
    $('#set-source').value = cfg.obsBrowserSourceName;
    $('#set-obs-pass').value = cfg.obsWebsocketPassword;
    $('#obs-restart-toggle').checked = cfg.obsAutoRestart || false;
    $('#obs-stream-toggle').checked = cfg.obsAutoStream || false;
    $('#set-obs-path').value = cfg.obsPath || '';
    settingsLoaded = true;
  } catch (err) {
    console.error('Failed to load settings:', err);
  }
}

async function handleSettingsSave() {
  const playlists = collectPlaylists('#set-playlists-list');
  if (playlists === null) return; // validation error already shown
  if (playlists.length === 0) {
    showToast('Add at least one playlist.');
    return;
  }
  const body = {
    playlists,
    obsBrowserSourceName: $('#set-source').value.trim(),
    obsWebsocketPassword: $('#set-obs-pass').value,
    obsAutoRestart: $('#obs-restart-toggle').checked,
    obsAutoStream: $('#obs-stream-toggle').checked,
    obsPath: $('#set-obs-path').value.trim(),
  };

  try {
    $('#set-btn').disabled = true;
    $('#set-btn').textContent = 'Saving...';
    await api('/api/config', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    $('#set-btn').textContent = 'Saved!';
    settingsLoaded = false;
    setTimeout(() => { $('#set-btn').textContent = 'Save Settings'; $('#set-btn').disabled = false; }, 1500);
  } catch (err) {
    showToast('Failed to save: ' + err.message);
    $('#set-btn').textContent = 'Save Settings';
    $('#set-btn').disabled = false;
  }
}

// --- OBS path helpers ---

async function detectObsPath() {
  const resultEl = $('#obs-path-result');
  resultEl.textContent = 'Searching...';
  resultEl.className = 'obs-path-result';
  try {
    const res = await api('/api/obs-path/detect', { method: 'POST' });
    if (res.found) {
      $('#set-obs-path').value = res.path;
      resultEl.textContent = 'Found: ' + res.path;
      resultEl.className = 'obs-path-result valid';
    } else {
      resultEl.textContent = 'OBS not found in default locations. Enter the path manually.';
      resultEl.className = 'obs-path-result invalid';
    }
  } catch {
    resultEl.textContent = 'Detection failed.';
    resultEl.className = 'obs-path-result invalid';
  }
}

async function validateObsPath() {
  const path = $('#set-obs-path').value.trim();
  const resultEl = $('#obs-path-result');
  if (!path) {
    resultEl.textContent = 'No path entered. Auto-detect will be used.';
    resultEl.className = 'obs-path-result';
    return;
  }
  resultEl.textContent = 'Checking...';
  resultEl.className = 'obs-path-result';
  try {
    const res = await api('/api/obs-path/validate', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path }),
    });
    if (res.valid) {
      resultEl.textContent = 'Path is valid!';
      resultEl.className = 'obs-path-result valid';
      $('#set-obs-path').classList.remove('invalid');
    } else {
      resultEl.textContent = res.error;
      resultEl.className = 'obs-path-result invalid';
      $('#set-obs-path').classList.add('invalid');
    }
  } catch {
    resultEl.textContent = 'Validation failed.';
    resultEl.className = 'obs-path-result invalid';
  }
}

// --- Autostart ---

async function loadAutostart() {
  try {
    const { enabled } = await api('/api/autostart');
    $('#autostart-toggle').checked = enabled;
  } catch {
    // ignore
  }
}

async function handleAutostartToggle() {
  const enabled = $('#autostart-toggle').checked;
  try {
    await api('/api/autostart', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled }),
    });
  } catch (err) {
    showToast('Failed to set autostart: ' + err.message);
    $('#autostart-toggle').checked = !enabled;
  }
}

// --- Webhooks tab ---

let webhookDefaults = null;
let webhookSettingsLoaded = false;

const EVENT_LABELS = {
  error: 'Playback Error',
  skip: 'Video Skip',
  recovery: 'Recovery Action',
  critical: 'Critical Alert',
  resume: 'Playback Resumed',
  obsDisconnect: 'OBS Disconnected',
  obsReconnect: 'OBS Reconnected',
  streamDrop: 'Stream Dropped',
  streamRestart: 'Stream Restarted',
};

const EVENT_LEVELS = {
  error: 'warn',
  skip: 'warn',
  recovery: 'warn',
  critical: 'error',
  resume: 'info',
  obsDisconnect: 'warn',
  obsReconnect: 'info',
  streamDrop: 'warn',
  streamRestart: 'info',
};

const PREVIEW_SAMPLE_VARS = {
  error: { videoIndex: 3, videoId: 'dQw4w9WgXcQ', errorCode: 150, attempt: 2 },
  skip: { videoIndex: 3, videoId: 'dQw4w9WgXcQ', reason: 'Error 150 (unavailable/not embeddable)' },
  recovery: { step: 'refreshSource' },
  critical: { message: 'All recovery steps exhausted. Player may be unresponsive.' },
  resume: { videoIndex: 4, videoId: 'abc123def' },
  obsDisconnect: {},
  obsReconnect: {},
  streamDrop: { attempt: 1, maxAttempts: 5 },
  streamRestart: { attempts: 2 },
};

async function loadWebhookSettings() {
  if (webhookSettingsLoaded) return;
  try {
    const [cfg, defaults] = await Promise.all([
      api('/api/config'),
      api('/api/discord/defaults'),
    ]);
    webhookDefaults = defaults;
    const discord = cfg.discord || {};

    // Identity
    $('#wh-url').value = discord.webhookUrl === '********' ? '' : (discord.webhookUrl || '');
    if (discord.webhookUrl === '********') {
      $('#wh-url').placeholder = 'Webhook configured (leave blank to keep)';
    }
    $('#wh-bot-name').value = discord.botName || '';
    $('#wh-avatar-url').value = discord.avatarUrl || '';
    $('#wh-role-ping').value = discord.rolePing || '';

    // Events
    const events = discord.events || {};
    for (const key of Object.keys(EVENT_LABELS)) {
      const el = $(`#wh-evt-${key}`);
      if (el) el.checked = events[key] !== false;
    }

    // Templates
    const templates = discord.templates || {};
    buildTemplateEditors(defaults.templates, defaults.variables, templates);

    // Preview
    updatePreview();
    webhookSettingsLoaded = true;
  } catch (err) {
    console.error('Failed to load webhook settings:', err);
  }
}

function buildTemplateEditors(defaults, variables, current) {
  const container = $('#wh-templates-container');
  container.innerHTML = '';
  for (const [key, label] of Object.entries(EVENT_LABELS)) {
    const group = document.createElement('div');
    group.className = 'wh-template-group';

    const header = document.createElement('div');
    header.className = 'wh-template-header';
    header.innerHTML = `<span class="wh-template-label">${escapeHtml(label)}</span>`;
    const resetBtn = document.createElement('button');
    resetBtn.type = 'button';
    resetBtn.className = 'btn-reset';
    resetBtn.textContent = 'Reset';
    resetBtn.addEventListener('click', () => resetTemplate(key));
    header.appendChild(resetBtn);
    group.appendChild(header);

    // Variable chips
    const vars = variables[key] || [];
    if (vars.length > 0) {
      const chipsRow = document.createElement('div');
      chipsRow.className = 'wh-chips';
      for (const v of vars) {
        const chip = document.createElement('span');
        chip.className = 'wh-chip';
        chip.textContent = `{${v}}`;
        chip.addEventListener('click', () => insertVariable(key, v));
        chipsRow.appendChild(chip);
      }
      group.appendChild(chipsRow);
    }

    // Textarea
    const textarea = document.createElement('textarea');
    textarea.className = 'wh-template-input';
    textarea.id = `wh-tpl-${key}`;
    textarea.value = current[key] || defaults[key] || '';
    textarea.rows = 2;
    textarea.addEventListener('input', () => updatePreview());
    group.appendChild(textarea);

    container.appendChild(group);
  }
}

function insertVariable(eventType, varName) {
  const textarea = $(`#wh-tpl-${eventType}`);
  if (!textarea) return;
  const start = textarea.selectionStart;
  const end = textarea.selectionEnd;
  const text = textarea.value;
  const insert = `{${varName}}`;
  textarea.value = text.substring(0, start) + insert + text.substring(end);
  textarea.selectionStart = textarea.selectionEnd = start + insert.length;
  textarea.focus();
  updatePreview();
}

function resetTemplate(eventType) {
  if (!webhookDefaults) return;
  const textarea = $(`#wh-tpl-${eventType}`);
  if (textarea) {
    textarea.value = webhookDefaults.templates[eventType] || '';
    updatePreview();
  }
}

function updatePreview() {
  const selectEl = $('#wh-preview-select');
  if (!selectEl) return;
  const eventType = selectEl.value;
  const level = EVENT_LEVELS[eventType] || 'info';
  const emoji = { info: '\u2139\uFE0F', warn: '\u26A0\uFE0F', error: '\uD83D\uDEA8' };
  const colors = { info: '#3498db', warn: '#f1c40f', error: '#e74c3c' };

  // Get template
  const textarea = $(`#wh-tpl-${eventType}`);
  const template = textarea ? textarea.value : '';
  const vars = PREVIEW_SAMPLE_VARS[eventType] || {};

  // Render template
  const rendered = template.replace(/\{(\w+)\}/g, (match, key) => {
    return key in vars ? String(vars[key]) : match;
  });

  // Bot name
  const botName = $('#wh-bot-name') ? $('#wh-bot-name').value.trim() : '';
  const titleName = botName || 'StreamLoop';

  // Update preview elements
  $('#wh-preview-bar').style.background = colors[level];
  $('#wh-preview-title').textContent = `${emoji[level]} ${titleName}`;
  $('#wh-preview-desc').textContent = rendered;

  // Fields for error/critical
  const fieldsContainer = $('#wh-preview-fields');
  fieldsContainer.innerHTML = '';
  if (eventType === 'error') {
    const fieldData = [
      { name: 'Error Code', value: String(vars.errorCode || '') },
      { name: 'Video', value: `#${vars.videoIndex} (${vars.videoId})` },
      { name: 'Attempt', value: String(vars.attempt || '') },
    ];
    for (const f of fieldData) {
      const fieldEl = document.createElement('div');
      fieldEl.innerHTML = `<div class="wh-preview-field-name">${escapeHtml(f.name)}</div><div class="wh-preview-field-value">${escapeHtml(f.value)}</div>`;
      fieldsContainer.appendChild(fieldEl);
    }
  } else if (eventType === 'critical') {
    const fieldEl = document.createElement('div');
    fieldEl.style.gridColumn = '1 / -1';
    fieldEl.innerHTML = `<div class="wh-preview-field-name">Status</div><div class="wh-preview-field-value">${escapeHtml(String(vars.message || ''))}</div>`;
    fieldsContainer.appendChild(fieldEl);
  }

  // Footer
  $('#wh-preview-footer').textContent = `Dashboard: http://localhost:7654/admin | Uptime: 2h 15m | v1.0.0`;
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

  for (const key of Object.keys(EVENT_LABELS)) {
    const el = $(`#wh-evt-${key}`);
    if (el) discord.events[key] = el.checked;
    const tpl = $(`#wh-tpl-${key}`);
    if (tpl) discord.templates[key] = tpl.value;
  }

  try {
    btn.disabled = true;
    btn.textContent = 'Saving...';
    await api('/api/config', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ discord }),
    });
    btn.textContent = 'Saved!';
    webhookSettingsLoaded = false;
    setTimeout(() => { btn.textContent = 'Save Webhook Settings'; btn.disabled = false; }, 1500);
  } catch (err) {
    showToast('Failed to save: ' + err.message);
    btn.textContent = 'Save Webhook Settings';
    btn.disabled = false;
  }
}

async function testDiscordWebhook() {
  const resultEl = $('#wh-test-result');
  resultEl.style.display = 'block';
  resultEl.textContent = 'Sending test message...';
  resultEl.className = 'obs-path-result';
  try {
    await api('/api/discord/test', { method: 'POST' });
    resultEl.textContent = 'Test message sent! Check your Discord channel.';
    resultEl.className = 'obs-path-result valid';
  } catch (err) {
    resultEl.textContent = err.message || 'Failed to send test message.';
    resultEl.className = 'obs-path-result invalid';
  }
}

// --- Playback tab ---

let playbackSettingsLoaded = false;

async function loadPlaybackSettings() {
  if (playbackSettingsLoaded) return;
  try {
    const cfg = await api('/api/config');
    $('#pb-recovery-delay').value = String(cfg.recoveryDelayMs || 5000);
    $('#pb-max-errors').value = String(cfg.maxConsecutiveErrors || 3);
    $('#pb-quality-toggle').checked = cfg.qualityRecoveryEnabled !== false;
    $('#pb-quality-min').value = cfg.minQuality || 'hd720';
    $('#pb-quality-delay').value = String(cfg.qualityRecoveryDelayMs || 120000);
    $('#pb-refresh-interval').value = String(cfg.sourceRefreshIntervalMs || 0);
    playbackSettingsLoaded = true;
  } catch (err) {
    console.error('Failed to load playback settings:', err);
  }
}

async function handlePlaybackSave() {
  const btn = $('#pb-save-btn');
  const body = {
    recoveryDelayMs: Number($('#pb-recovery-delay').value),
    maxConsecutiveErrors: Number($('#pb-max-errors').value),
    qualityRecoveryEnabled: $('#pb-quality-toggle').checked,
    minQuality: $('#pb-quality-min').value,
    qualityRecoveryDelayMs: Number($('#pb-quality-delay').value),
    sourceRefreshIntervalMs: Number($('#pb-refresh-interval').value),
  };
  try {
    btn.disabled = true;
    btn.textContent = 'Saving...';
    await api('/api/config', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    btn.textContent = 'Saved!';
    playbackSettingsLoaded = false;
    setTimeout(() => { btn.textContent = 'Save Playback Settings'; btn.disabled = false; }, 1500);
  } catch (err) {
    showToast('Failed to save: ' + err.message);
    btn.textContent = 'Save Playback Settings';
    btn.disabled = false;
  }
}

// --- Tab switching ---

function switchTab(tabName) {
  $$('.tab').forEach(t => t.classList.toggle('active', t.dataset.tab === tabName));
  $$('.tab-panel').forEach(p => p.classList.toggle('hidden', p.id !== 'panel-' + tabName));
  if (tabName === 'settings') loadSettings();
  if (tabName === 'webhooks') loadWebhookSettings();
  if (tabName === 'playback') loadPlaybackSettings();
}

// --- Helpers ---

function formatDuration(ms) {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s % 60}s`;
  return `${s}s`;
}

function formatTime(seconds) {
  if (!seconds) return '0:00:00';
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  return `${h}:${m.toString().padStart(2, '0')}:${s.toString().padStart(2, '0')}`;
}

function escapeHtml(str) {
  const d = document.createElement('div');
  d.textContent = str;
  return d.innerHTML;
}

function showToast(message, type = 'error') {
  const container = $('#toast-container');
  const toast = document.createElement('div');
  toast.className = `toast toast-${type}`;
  toast.textContent = message;
  container.appendChild(toast);
  setTimeout(() => {
    toast.style.opacity = '0';
    setTimeout(() => toast.remove(), 200);
  }, 4000);
}

// --- Update banner ---

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
    text.textContent = 'Update installed, restarting...';
    btn.disabled = true;
    btn.textContent = 'Restarting...';
  } else if (status.status === 'error') {
    text.textContent = `Update failed: ${status.error}`;
    btn.disabled = false;
    btn.textContent = 'Retry';
  } else {
    text.textContent = `Version ${status.latestVersion} is available (current: ${status.currentVersion})`;
    btn.disabled = false;
    btn.textContent = status.isDevMode ? 'Dev Mode' : 'Update Now';
    if (status.isDevMode) btn.disabled = true;
  }
}

async function handleUpdate() {
  const btn = $('#update-btn');
  btn.disabled = true;
  btn.textContent = 'Starting...';

  try {
    await api('/api/update/apply', { method: 'POST' });
    btn.textContent = 'Restarting...';
    // Poll until server comes back
    setTimeout(waitForRestart, 3000);
  } catch (err) {
    showToast('Update failed: ' + err.message);
    btn.disabled = false;
    btn.textContent = 'Retry';
  }
}

async function waitForRestart() {
  try {
    await api('/api/status');
    // Server is back, reload page
    window.location.reload();
  } catch {
    // Server still restarting, try again
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
    const status = await api('/api/update/check', { method: 'POST' });
    const settingsBtn = $('#settings-update-btn');
    if (status.updateAvailable) {
      result.textContent = `v${status.latestVersion} available!`;
      result.style.color = 'var(--accent)';
      if (settingsBtn && !status.isDevMode) settingsBtn.classList.remove('hidden');
      renderUpdateBanner(status);
    } else {
      result.textContent = `Up to date (v${status.currentVersion})`;
      result.style.color = 'var(--text-muted)';
      if (settingsBtn) settingsBtn.classList.add('hidden');
    }
  } catch (err) {
    result.textContent = 'Check failed: ' + err.message;
    result.style.color = 'var(--red)';
  }
  btn.textContent = 'Check for Updates';
  btn.disabled = false;
}

// --- Boot ---

document.addEventListener('DOMContentLoaded', async () => {
  // Fetch API token before any API calls (CSRF protection)
  await fetchApiToken();

  // Settings form
  $('#settings-form').addEventListener('submit', (e) => e.preventDefault());
  $('#set-btn').addEventListener('click', handleSettingsSave);

  // Tabs
  $$('.tab').forEach(t => t.addEventListener('click', () => switchTab(t.dataset.tab)));

  // Playback tab
  $('#pb-save-btn').addEventListener('click', handlePlaybackSave);

  // Autostart
  $('#autostart-toggle').addEventListener('change', handleAutostartToggle);

  // Check for updates button
  $('#check-update-btn').addEventListener('click', handleCheckUpdate);

  // Webhook preview updates
  const previewSelect = $('#wh-preview-select');
  if (previewSelect) {
    previewSelect.addEventListener('change', () => updatePreview());
  }
  const botNameInput = $('#wh-bot-name');
  if (botNameInput) {
    botNameInput.addEventListener('input', () => updatePreview());
  }

  init();
});
