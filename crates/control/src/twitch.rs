//! The Twitch live check. While the encoder runs it asks Helix whether the channel is live; three mismatches in a
//! row restart the encoder. The read-back (`readback.rs`) shares the restart clock: at most one restart per 30 min
//! between both, and a problem inside that window raises one critical alert.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Serialize;
use streamloop_common::api::ControlCmd;
use streamloop_common::config::Config;
use tokio::task::JoinHandle;

use crate::clock::Clock;
use crate::discord::Notifier;
use crate::http::{Http, HttpRequest};
use crate::readback::{encode_uri_component, form_encode, FfmpegGrab, FrameGrab, ReadbackHooks, ReadbackOptions, ReadbackStatus, TwitchReadback};
use crate::runner::ProcessRunner;
use crate::supervisor_client::SupervisorApi;

const TWITCH_TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const TWITCH_STREAMS_URL: &str = "https://api.twitch.tv/helix/streams";
/// Refresh 5 min before expiry.
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);
const MISMATCH_THRESHOLD: u32 = 3;
const INITIAL_CHECK_DELAY: Duration = Duration::from_secs(15);
/// Between encoder restarts from Twitch checks; a problem inside it only alerts.
const RESTART_LIMIT: Duration = Duration::from_secs(30 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
struct RestartState {
    last_attempt_at: Option<SystemTime>,
    /// When the window's one critical went out.
    alerted_at: Option<SystemTime>,
}

/// The 30 min restart limit. Cloning shares it, so every checker a config reload builds keeps the same limit.
#[derive(Clone, Default)]
pub struct RestartClock(Arc<Mutex<RestartState>>);

impl RestartClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn last_attempt_at(&self) -> Option<SystemTime> {
        self.0.lock().unwrap().last_attempt_at
    }

    pub fn alerted_at(&self) -> Option<SystemTime> {
        self.0.lock().unwrap().alerted_at
    }
}

pub type AddEvent = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
pub struct TwitchOptions {
    /// The install's ffmpeg under ROOT; without it (and without `grab`) the read-back is off.
    pub ffmpeg_path: Option<String>,
    pub add_event: Option<AddEvent>,
    /// Shared by every checker a config reload builds, so a reload cannot lift the 30 min limit.
    pub restart_clock: Option<RestartClock>,
    /// Test runs only (`STREAMLOOP_TEST_HLS_URL`): the read-back reads this HLS and the channel counts as live,
    /// so no request reaches Twitch and no Helix credentials are needed.
    pub test_hls_url: Option<String>,
    /// Replaces the ffmpeg frame grab; tests supply frames here.
    pub grab: Option<Arc<dyn FrameGrab>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TwitchStatus {
    pub enabled: bool,
    pub channel_live: Option<bool>,
    pub encoder_alive: Option<bool>,
    pub consecutive_mismatches: u32,
    pub mismatch_threshold: u32,
    pub last_check_at: Option<u64>,
    pub last_restart_at: Option<u64>,
    pub restart_count: u64,
    pub readback: ReadbackStatus,
}

#[derive(Default)]
struct State {
    access_token: Option<String>,
    token_expires_at: Option<SystemTime>,
    consecutive_mismatches: u32,
    channel_live: Option<bool>,
    encoder_alive: Option<bool>,
    last_check_at: Option<u64>,
    last_restart_at: Option<u64>,
    restart_count: u64,
}

struct Inner {
    channel: String,
    client_id: String,
    client_secret: String,
    liveness_enabled: bool,
    poll_interval: Duration,
    client: Arc<dyn SupervisorApi>,
    notifier: Arc<dyn Notifier>,
    http: Arc<dyn Http>,
    clock: Arc<dyn Clock>,
    add_event: AddEvent,
    restart_clock: RestartClock,
    test_hls_url: Option<String>,
    readback: Option<Arc<TwitchReadback>>,
    state: Mutex<State>,
    restart_in_progress: AtomicBool,
}

/// Clears the restart flag however the restart ends.
struct Restarting<'a>(&'a AtomicBool);

impl Drop for Restarting<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The read-back's way back into its checker, without keeping the checker alive.
struct Hooks(Weak<Inner>);

#[async_trait]
impl ReadbackHooks for Hooks {
    async fn is_channel_live(&self) -> Option<bool> {
        self.0.upgrade()?.is_channel_live().await
    }

    async fn on_frozen(&self) {
        if let Some(inner) = self.0.upgrade() {
            inner.handle_frozen().await;
        }
    }

    fn on_unavailable(&self, why: &str) {
        if let Some(inner) = self.0.upgrade() {
            inner.handle_unavailable(why);
        }
    }

    fn on_still(&self) {
        if let Some(inner) = self.0.upgrade() {
            inner.handle_still();
        }
    }
}

pub struct TwitchChecker {
    inner: Arc<Inner>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl TwitchChecker {
    pub fn new(
        config: &Config,
        client: Arc<dyn SupervisorApi>,
        notifier: Arc<dyn Notifier>,
        http: Arc<dyn Http>,
        clock: Arc<dyn Clock>,
        opts: TwitchOptions,
    ) -> Self {
        let test_hls_url = opts.test_hls_url.filter(|u| !u.is_empty());
        let overlay = config.supervisor.overlay;
        let ffmpeg_path = opts.ffmpeg_path.filter(|p| !p.is_empty());
        let inner = Arc::new_cyclic(|weak: &Weak<Inner>| {
            let mut inner = Inner {
                channel: config.twitch_channel.clone(),
                client_id: config.twitch_client_id.clone(),
                client_secret: config.twitch_client_secret.clone(),
                liveness_enabled: config.twitch_liveness_enabled,
                poll_interval: Duration::from_millis(config.twitch_poll_interval_ms),
                client: client.clone(),
                notifier,
                http: http.clone(),
                clock: clock.clone(),
                add_event: opts.add_event.unwrap_or_else(|| Arc::new(|_| {})),
                restart_clock: opts.restart_clock.unwrap_or_default(),
                test_hls_url: test_hls_url.clone(),
                readback: None,
                state: Mutex::new(State::default()),
                restart_in_progress: AtomicBool::new(false),
            };
            let grab = opts.grab.or_else(|| ffmpeg_path.map(|p| Arc::new(FfmpegGrab::new(Arc::new(ProcessRunner), p)) as Arc<dyn FrameGrab>));
            if let (Some(grab), true) = (grab, inner.enabled()) {
                inner.readback = Some(TwitchReadback::new(ReadbackOptions {
                    channel: config.twitch_channel.clone(),
                    grab,
                    http,
                    client,
                    clock,
                    hooks: Arc::new(Hooks(weak.clone())),
                    overlay,
                    interval: None,
                    gap: None,
                    threshold: None,
                    hls_url: test_hls_url,
                }));
            }
            inner
        });
        Self { inner, task: Mutex::new(None) }
    }

    pub fn enabled(&self) -> bool {
        self.inner.enabled()
    }

    pub fn start(&self) {
        let inner = &self.inner;
        if !inner.liveness_enabled {
            tracing::info!("Twitch liveness checker disabled");
            return;
        }
        if !inner.enabled() {
            tracing::warn!("Twitch liveness checker enabled but missing credentials (need Client ID, Client Secret, and Channel)");
            return;
        }
        tracing::info!(channel = %inner.channel, interval_ms = inner.poll_interval.as_millis() as u64, "Starting Twitch liveness checker");
        let mut task = self.task.lock().unwrap();
        if let Some(old) = task.take() {
            old.abort();
        }
        let inner = self.inner.clone();
        *task = Some(tokio::spawn(async move {
            tokio::time::sleep(INITIAL_CHECK_DELAY).await;
            inner.check().await;
            let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + inner.poll_interval, inner.poll_interval);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                inner.check().await;
            }
        }));
        drop(task);
        if let Some(readback) = &self.inner.readback {
            readback.start();
        }
    }

    pub fn stop(&self) {
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        if let Some(readback) = &self.inner.readback {
            readback.stop();
        }
    }

    pub fn status(&self) -> TwitchStatus {
        let inner = &self.inner;
        let st = inner.state.lock().unwrap();
        TwitchStatus {
            enabled: inner.enabled(),
            channel_live: st.channel_live,
            encoder_alive: st.encoder_alive,
            consecutive_mismatches: st.consecutive_mismatches,
            mismatch_threshold: MISMATCH_THRESHOLD,
            last_check_at: st.last_check_at,
            last_restart_at: st.last_restart_at,
            restart_count: st.restart_count,
            readback: inner.readback.as_ref().map_or_else(ReadbackStatus::off, |r| r.status()),
        }
    }

    pub async fn check(&self) {
        self.inner.check().await;
    }

    /// The read-back saw the overlay clock stand still twice while Twitch calls the channel live.
    pub async fn handle_frozen(&self) {
        self.inner.handle_frozen().await;
    }

    /// The whole picture stood still with no overlay clock to tell a stale stream from still content.
    pub fn handle_still(&self) {
        self.inner.handle_still();
    }

    /// The read-back itself could not run: recorded only, never acted on.
    pub fn handle_unavailable(&self, why: &str) {
        self.inner.handle_unavailable(why);
    }
}

impl Drop for TwitchChecker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Keeps the credentials out of any text that is logged.
fn scrub(text: &str, secrets: &[&str]) -> String {
    secrets.iter().filter(|s| !s.is_empty()).fold(text.to_string(), |t, s| t.replace(s, "[redacted]"))
}

impl Inner {
    fn enabled(&self) -> bool {
        self.liveness_enabled
            && !self.channel.is_empty()
            && (self.test_hls_url.is_some() || (!self.client_id.is_empty() && !self.client_secret.is_empty()))
    }

    fn now_ms(&self) -> u64 {
        self.clock.now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
    }

    async fn check(&self) {
        if self.restart_in_progress.load(Ordering::SeqCst) {
            return;
        }
        self.state.lock().unwrap().last_check_at = Some(self.now_ms());

        let (status, live) = tokio::join!(self.client.status(), self.is_channel_live());
        let encoder_alive = status.and_then(|s| s.encoder).and_then(|e| e.alive) == Some(true);
        let mismatches = {
            let mut st = self.state.lock().unwrap();
            st.encoder_alive = Some(encoder_alive);
            st.channel_live = live;
            tracing::debug!(encoder_alive, channel_live = ?live, consecutive_mismatches = st.consecutive_mismatches, "Twitch liveness check");
            // An API error leaves the counter alone.
            let Some(live) = live else { return };
            if encoder_alive && !live {
                st.consecutive_mismatches += 1;
                tracing::warn!(consecutive_mismatches = st.consecutive_mismatches, threshold = MISMATCH_THRESHOLD, "Twitch liveness mismatch: encoder running but channel not live");
                st.consecutive_mismatches
            } else {
                st.consecutive_mismatches = 0;
                0
            }
        };
        if mismatches >= MISMATCH_THRESHOLD {
            self.restart_stream().await;
        }
    }

    async fn handle_frozen(&self) {
        (self.add_event)("Twitch is showing a frozen picture");
        self.restart_encoder("Twitch shows a frozen picture", false).await;
    }

    fn handle_still(&self) {
        (self.add_event)("The picture on Twitch is not changing (no overlay clock to check, so no restart)");
    }

    fn handle_unavailable(&self, why: &str) {
        (self.add_event)(&format!("Could not check the picture on Twitch: {why}"));
    }

    async fn restart_stream(&self) {
        self.state.lock().unwrap().consecutive_mismatches = 0;
        self.restart_encoder("Twitch shows the channel offline", true).await;
    }

    /// `reason` goes to the supervisor's log and into any alert.
    async fn restart_encoder(&self, reason: &str, offline: bool) {
        if self.restart_in_progress.load(Ordering::SeqCst) {
            return;
        }
        let now = self.clock.now();
        let capped = {
            let mut clock = self.restart_clock.0.lock().unwrap();
            match clock.last_attempt_at {
                Some(last) if now.duration_since(last).unwrap_or(Duration::ZERO) < RESTART_LIMIT => {
                    // One critical per window; later hits in it go to the timeline only.
                    let alert = !clock.alerted_at.is_some_and(|alerted| alerted >= last);
                    if alert {
                        clock.alerted_at = Some(now);
                    }
                    Some(alert)
                }
                _ => {
                    self.restart_in_progress.store(true, Ordering::SeqCst);
                    clock.last_attempt_at = Some(now);
                    None
                }
            }
        };
        if let Some(alert) = capped {
            let message = format!("{reason}, and the encoder was already restarted in the last 30 minutes. Check the stream.");
            if alert {
                tracing::warn!(reason, "Twitch liveness: encoder restarted less than 30 min ago, alerting only");
                self.notifier.notify_critical(&message).await;
            } else {
                tracing::warn!(reason, "Twitch liveness: still failing inside the 30 min window, already alerted");
                (self.add_event)(&message);
            }
            return;
        }
        let _restarting = Restarting(&self.restart_in_progress);

        tracing::warn!(reason, "Twitch liveness: restarting the encoder");
        if offline {
            self.notifier.notify_twitch_mismatch(&self.channel).await;
        }

        let result = self.client.control(ControlCmd::RestartEncoder, None, Some(reason.to_string()), None).await;
        if !result.ok {
            let error = result.error.as_deref();
            let why = match error {
                Some(e) if e.starts_with("needs-stream-key") => "no stream key is set, so no encoder is running",
                Some(e) => e,
                None => "unknown error",
            };
            tracing::error!(error = why, "Twitch liveness: encoder restart refused");
            self.notifier.notify_critical(&format!("Could not restart the encoder ({reason}): {why}.")).await;
            return;
        }
        {
            let mut st = self.state.lock().unwrap();
            st.last_restart_at = Some(self.now_ms());
            st.restart_count += 1;
        }
        tracing::info!(already_restarting = result.already_restarting, "Twitch liveness: encoder restart requested");
        if offline {
            self.notifier.notify_twitch_restart(&self.channel).await;
        }
    }

    /// `None` when Helix cannot say.
    async fn is_channel_live(&self) -> Option<bool> {
        if self.test_hls_url.is_some() {
            return Some(true);
        }
        let token = self.ensure_token().await?;
        let url = format!("{TWITCH_STREAMS_URL}?user_login={}", encode_uri_component(&self.channel.to_lowercase()));
        let res = self.streams(&url, &token).await?;
        if res.status == 401 {
            // Token expired or revoked: invalidate and retry once.
            tracing::warn!("Twitch API returned 401, refreshing token");
            {
                let mut st = self.state.lock().unwrap();
                st.access_token = None;
                st.token_expires_at = None;
            }
            let token = self.ensure_token().await?;
            let retry = self.streams(&url, &token).await?;
            if !(200..300).contains(&retry.status) {
                tracing::error!(status = retry.status, "Twitch API retry failed");
                return None;
            }
            return live_from(&retry.body);
        }
        if !(200..300).contains(&res.status) {
            tracing::error!(status = res.status, "Twitch API error");
            return None;
        }
        live_from(&res.body)
    }

    async fn streams(&self, url: &str, token: &str) -> Option<crate::http::HttpResponse> {
        let mut req = HttpRequest::get(url);
        req.headers = vec![("Client-ID".into(), self.client_id.clone()), ("Authorization".into(), format!("Bearer {token}"))];
        req.timeout = Some(REQUEST_TIMEOUT);
        match self.http.send(req).await {
            Ok(res) => Some(res),
            Err(e) => {
                tracing::error!(error = %self.scrub(&e.message), "Twitch API request failed");
                None
            }
        }
    }

    fn scrub(&self, text: &str) -> String {
        let token = self.state.lock().unwrap().access_token.clone().unwrap_or_default();
        scrub(text, &[&self.client_secret, &token])
    }

    /// The current token, fetching a new one when it is missing or within 5 min of expiring.
    async fn ensure_token(&self) -> Option<String> {
        {
            let st = self.state.lock().unwrap();
            if let (Some(token), Some(expires)) = (&st.access_token, st.token_expires_at) {
                if self.clock.now() + TOKEN_REFRESH_MARGIN < expires {
                    return Some(token.clone());
                }
            }
        }
        let body = form_encode(&[("client_id", &self.client_id), ("client_secret", &self.client_secret), ("grant_type", "client_credentials")]);
        let mut req = HttpRequest::get(TWITCH_TOKEN_URL);
        req.method = "POST".into();
        req.headers = vec![("content-type".into(), "application/x-www-form-urlencoded;charset=UTF-8".into())];
        req.body = Some(body.into_bytes());
        req.timeout = Some(REQUEST_TIMEOUT);
        let res = match self.http.send(req).await {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(error = %self.scrub(&e.message), "Twitch token request failed");
                self.state.lock().unwrap().access_token = None;
                return None;
            }
        };
        let parsed = if (200..300).contains(&res.status) { res.json() } else { None };
        if !(200..300).contains(&res.status) {
            tracing::error!(status = res.status, "Failed to obtain Twitch access token");
        }
        let token = parsed.as_ref().and_then(|j| j.get("access_token")).and_then(|t| t.as_str()).filter(|t| !t.is_empty());
        let Some(token) = token else {
            if parsed.is_none() && (200..300).contains(&res.status) {
                tracing::error!("Twitch token request failed");
            }
            self.state.lock().unwrap().access_token = None;
            return None;
        };
        // A reply without `expires_in` counts as already expired, so the next check asks again.
        let expires_in = parsed.as_ref().and_then(|j| j.get("expires_in")).and_then(|v| v.as_f64()).unwrap_or(0.0).max(0.0);
        let mut st = self.state.lock().unwrap();
        st.access_token = Some(token.to_string());
        st.token_expires_at = Some(self.clock.now() + Duration::from_secs_f64(expires_in));
        tracing::info!("Obtained Twitch access token");
        Some(token.to_string())
    }
}

/// Helix answers `{"data": [...]}`; a channel is live when the list has an entry.
fn live_from(body: &[u8]) -> Option<bool> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    Some(!json.get("data")?.as_array()?.is_empty())
}
