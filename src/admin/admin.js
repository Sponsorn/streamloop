// Admin Dashboard Client Logic

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => document.querySelectorAll(sel);

let currentView = 'loading'; // 'wizard' | 'dashboard'
let pollTimer = null;
let pollFailures = 0;

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
  loadWizardDefaults();
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

async function api(url, opts) {
  const res = await fetch(url, opts);
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.error || `HTTP ${res.status}`);
  }
  return res.json();
}

// --- Playlist row helpers ---

function addWizPlaylist(id, name) {
  addPlaylistRow('#wiz-playlists-list', id || '', name || '');
}

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

function collectPlaylists(containerSel) {
  const rows = $$(containerSel + ' .playlist-row');
  const playlists = [];
  for (const row of rows) {
    const id = row.querySelector('.pl-id').value.trim();
    const name = row.querySelector('.pl-name').value.trim();
    if (id) {
      playlists.push(name ? { id, name } : { id });
    }
  }
  return playlists;
}

// --- Wizard ---

async function loadWizardDefaults() {
  try {
    const cfg = await api('/api/config');
    const container = $('#wiz-playlists-list');
    container.innerHTML = '';
    const playlists = cfg.playlists || [];
    const hasReal = playlists.some(p => !p.id.includes('xxxxx'));
    if (hasReal) {
      playlists.forEach(p => addWizPlaylist(p.id, p.name || ''));
    } else {
      addWizPlaylist('', '');
    }
    $('#wiz-source').value = cfg.obsBrowserSourceName || '';
    $('#wiz-obs-pass').value = '';
    $('#wiz-discord').value = (cfg.discordWebhookUrl && cfg.discordWebhookUrl !== '********') ? cfg.discordWebhookUrl : '';
  } catch {
    addWizPlaylist('', '');
  }
}

async function handleWizardSubmit(e) {
  e.preventDefault();

  const playlists = collectPlaylists('#wiz-playlists-list');
  const obsBrowserSourceName = $('#wiz-source').value.trim();
  const obsWebsocketPassword = $('#wiz-obs-pass').value;
  const discordWebhookUrl = $('#wiz-discord').value.trim();

  // Validation
  let valid = true;
  if (playlists.length === 0) { valid = false; showToast('Add at least one playlist.'); }
  if (!obsBrowserSourceName) { $('#wiz-source').classList.add('invalid'); valid = false; }
  else { $('#wiz-source').classList.remove('invalid'); }

  if (!valid) return;

  const body = { playlists, obsBrowserSourceName, discordWebhookUrl };
  if (obsWebsocketPassword) body.obsWebsocketPassword = obsWebsocketPassword;

  try {
    $('#wiz-btn').disabled = true;
    $('#wiz-btn').textContent = 'Saving...';
    await api('/api/config', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    showDashboard();
  } catch (err) {
    showToast('Failed to save: ' + err.message);
  } finally {
    $('#wiz-btn').disabled = false;
    $('#wiz-btn').textContent = 'Save & Start';
  }
}

// --- Dashboard polling ---

function startPolling() {
  pollOnce();
  pollTimer = setInterval(pollOnce, 2000);
}

function stopPolling() {
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
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
    renderNowPlaying(state);
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
  setCard('obs-status', s.obsConnected ? 'Connected' : 'Disconnected', s.obsConnected ? 'ok' : 'err');
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

  // Uptime
  $('#uptime').textContent = 'Uptime: ' + formatDuration(s.uptimeMs);

  // Playlist info in Now Playing
  if (s.totalPlaylists > 1) {
    $('#np-playlist').textContent = `${s.playlistIndex + 1} of ${s.totalPlaylists}`;
  } else {
    $('#np-playlist').textContent = '1 of 1';
  }
}

function setCard(id, text, cls) {
  const el = $(`#${id}`);
  el.textContent = text;
  el.className = 'card-value ' + cls;
}

function renderNowPlaying(s) {
  $('#np-index').textContent = s.videoIndex;
  const vidEl = $('#np-videoid');
  if (s.videoId) {
    vidEl.innerHTML = `<a href="https://www.youtube.com/watch?v=${escapeHtml(s.videoId)}" target="_blank" rel="noopener">${escapeHtml(s.videoId)}</a>`;
  } else {
    vidEl.textContent = '-';
  }
  $('#np-time').textContent = formatTime(s.currentTime);
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
    $('#set-discord').value = cfg.discordWebhookUrl === '********' ? '' : cfg.discordWebhookUrl;
    if (cfg.discordWebhookUrl === '********') {
      $('#set-discord').placeholder = 'Discord webhook configured (leave blank to keep)';
    }
    settingsLoaded = true;
  } catch (err) {
    console.error('Failed to load settings:', err);
  }
}

async function handleSettingsSave(e) {
  e.preventDefault();
  const playlists = collectPlaylists('#set-playlists-list');
  if (playlists.length === 0) {
    showToast('Add at least one playlist.');
    return;
  }
  const body = {
    playlists,
    obsBrowserSourceName: $('#set-source').value.trim(),
    obsWebsocketPassword: $('#set-obs-pass').value,
    discordWebhookUrl: $('#set-discord').value.trim() || '********',
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

// --- Tab switching ---

function switchTab(tabName) {
  $$('.tab').forEach(t => t.classList.toggle('active', t.dataset.tab === tabName));
  $$('.tab-panel').forEach(p => p.classList.toggle('hidden', p.id !== 'panel-' + tabName));
  if (tabName === 'settings') loadSettings();
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
  if (!seconds) return '0:00';
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, '0')}`;
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

// --- Boot ---

document.addEventListener('DOMContentLoaded', () => {
  // Wizard form
  $('#wizard-form').addEventListener('submit', handleWizardSubmit);

  // Settings form
  $('#settings-form').addEventListener('submit', handleSettingsSave);

  // Tabs
  $$('.tab').forEach(t => t.addEventListener('click', () => switchTab(t.dataset.tab)));

  // Autostart
  $('#autostart-toggle').addEventListener('change', handleAutostartToggle);

  init();
});
