//! The exe updater: picks a release, stages its exe in `_update\` once the SHA-256 and `--version`
//! match, and decides when it lands: at Twitch's 48 h break or off air.
//!
//! `status()` keeps the 11 fields of master's Node updater; `nodeNotice` says when the update installs.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use streamloop_common::api::SupervisorStatus;
use streamloop_common::paths::Layout;
use tokio::sync::OwnedMutexGuard;

use crate::clock::Clock;
use crate::http::{Http, HttpErrorKind, HttpRequest, ReqwestHttp};
use crate::runner::{RunErrorKind, RunSpec, Runner};

pub const GITHUB_RELEASES_URL: &str = "https://api.github.com/repos/Sponsorn/streamloop/releases?per_page=50";
/// Test runs only: a JSON file in GitHub's release-list shape that replaces GitHub.
pub const TEST_RELEASES_VAR: &str = "STREAMLOOP_TEST_RELEASES";
/// Test runs only: the encoder uptime in seconds at which a staged update lands.
pub const TEST_BREAK_VAR: &str = "STREAMLOOP_TEST_BREAK_AT_S";

/// 47 h 45 min: the update's restart comes just before Twitch would cut the broadcast at 48 h.
pub const BREAK_AT_S: u64 = 171_900;
pub const OFF_AIR_AFTER: Duration = Duration::from_secs(300);
const GITHUB_TIMEOUT: Duration = Duration::from_secs(10);
const CHECK_COOLDOWN: Duration = Duration::from_secs(60);
const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const PERCENT_EVERY: Duration = Duration::from_secs(1);
const ALLOWED_DOWNLOAD_HOSTS: [&str; 2] = ["github.com", "objects.githubusercontent.com"];
/// GitHub sends release downloads on to release-assets.githubusercontent.com, and API answers may move within the API.
const REDIRECT_HOSTS: [&str; 4] = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com", "api.github.com"];
/// The off-air text promises a landing within a minute, so it shows from this long before `off_air_after`.
const OFF_AIR_NOTICE_LEAD: Duration = Duration::from_secs(60);
const STAGE_CUT_OFF: &str = "it stopped before it finished";
const GITHUB_SILENT: &str = "GitHub did not answer within 10 seconds.";
const UPDATE_NOW: &str = "Update Now installs it and restarts the stream.";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GithubAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GithubRelease {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub html_url: String,
    pub assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExeAssets {
    pub exe_url: String,
    pub checksum_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakPolicy {
    /// Encoder uptime at which a staged update lands while on air.
    pub break_at_s: u64,
    /// How long no live encoder counts as off air.
    pub off_air_after: Duration,
}

impl Default for BreakPolicy {
    fn default() -> Self {
        Self { break_at_s: BREAK_AT_S, off_air_after: OFF_AIR_AFTER }
    }
}

impl BreakPolicy {
    /// Reads `STREAMLOOP_TEST_BREAK_AT_S` once; the box never sets it.
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// `var` looks up an environment variable; a value that is not a whole number of seconds is ignored.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let mut policy = Self::default();
        if let Some(s) = var(TEST_BREAK_VAR).and_then(|v| v.trim().parse().ok()) {
            policy.break_at_s = s;
        }
        policy
    }
}

/// The test release list, when `STREAMLOOP_TEST_RELEASES` names one.
pub fn test_releases_from_env() -> Option<PathBuf> {
    std::env::var_os(TEST_RELEASES_VAR).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// What the line under the update banner says about the landing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeState {
    /// Nothing to say about the landing: no update, not staged yet, or dev mode.
    Quiet,
    /// Staged, and it lands at the break; `notice_text` needs the hours left.
    OnAir,
    /// Staged, and the stream is off air.
    OffAir,
    /// Only Update Now installs it: `autoUpdateCheck` is off, or this version rolled back once.
    UpdateNowOnly,
    /// The last stage failed, for this reason.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateStatus {
    Idle,
    Checking,
    Downloading,
    Ready,
    Error,
}

/// Master's `/update/status` fields in master's key order. R1 never offers parts: `kind` is `"app"`
/// whenever an update is offered, since that is the dashboard's "Version X is available" banner.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub status: UpdateStatus,
    pub error: Option<String>,
    pub is_dev_mode: bool,
    pub node_notice: Option<String>,
    pub kind: Option<String>,
    pub parts: Vec<String>,
    pub same_version: bool,
    pub download_percent: Option<u8>,
}

/// The download percentage to show, at most once per second and once more at the end.
#[derive(Debug, Default)]
pub struct DownloadCounter {
    received: u64,
    total: Option<u64>,
    last_at: Option<SystemTime>,
    last_percent: Option<u8>,
}

impl DownloadCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// After a chunk: the percentage to report, or `None` within a second of the last one or without a total.
    pub fn on_progress(&mut self, received: u64, total: Option<u64>, now: SystemTime) -> Option<u8> {
        self.received = received;
        self.total = total.filter(|t| *t > 0);
        self.percent()?;
        let due = match self.last_at {
            None => true,
            Some(last) => now.duration_since(last).is_ok_and(|d| d >= PERCENT_EVERY),
        };
        if !due {
            return None;
        }
        self.last_at = Some(now);
        self.last_percent = self.percent();
        self.last_percent
    }

    /// At the end: the final percentage, unless it was the last one reported.
    pub fn finish(&mut self) -> Option<u8> {
        let percent = self.percent()?;
        (self.last_percent != Some(percent)).then(|| {
            self.last_percent = Some(percent);
            percent
        })
    }

    fn percent(&self) -> Option<u8> {
        let total = self.total?;
        Some((self.received.saturating_mul(100) / total).min(100) as u8)
    }
}

pub struct UpdaterOptions {
    /// The running version, as `streamloop.exe --version` prints it.
    pub current: String,
    /// The root did not come from the exe's folder: checks only, never stages.
    pub dev_mode: bool,
    pub http: Arc<dyn Http>,
    pub runner: Arc<dyn Runner>,
    pub clock: Arc<dyn Clock>,
    pub layout: Layout,
    pub releases_url: String,
    /// `STREAMLOOP_TEST_RELEASES`: the release list comes from this file, and assets only from 127.0.0.1.
    pub test_releases: Option<PathBuf>,
    /// `node\node.exe --version`, compared with the release's `node-version.txt`.
    pub bundled_node: Option<String>,
    pub policy: BreakPolicy,
}

#[derive(Debug, Clone)]
struct Offer {
    version: String,
    exe_url: String,
    checksum_url: String,
}

/// The stream as the last `observe` saw it.
#[derive(Debug, Clone)]
struct View {
    live: bool,
    /// The encoder uptime at the last poll with a live encoder; a short outage keeps it.
    uptime_s: Option<u64>,
    off_air_for: Duration,
    stopped: bool,
    auto_check: bool,
    rolled_back: Option<String>,
}

#[derive(Debug)]
struct State {
    latest_version: Option<String>,
    update_available: bool,
    status: UpdateStatus,
    error: Option<String>,
    node_notice: Option<String>,
    download_percent: Option<u8>,
    offer: Option<Offer>,
    /// The version whose exe passed both checks; it counts only while the offer names it.
    staged: Option<String>,
    stage_failure: Option<String>,
    last_check: Option<SystemTime>,
    view: Option<View>,
    off_air_since: Option<SystemTime>,
}

struct Inner {
    current: String,
    dev_mode: bool,
    http: Arc<dyn Http>,
    runner: Arc<dyn Runner>,
    clock: Arc<dyn Clock>,
    layout: Layout,
    releases_url: String,
    test_releases: Option<PathBuf>,
    bundled_node: Option<String>,
    policy: BreakPolicy,
    state: Mutex<State>,
    /// Held for the whole of a stage, so two never run at once.
    stage_lock: Arc<tokio::sync::Mutex<()>>,
}

/// A handle to the one updater; clones share it.
#[derive(Clone)]
pub struct Updater {
    inner: Arc<Inner>,
}

struct Found {
    latest: String,
    offer: Option<Offer>,
    node_notice: Option<String>,
}

impl Updater {
    pub fn new(opts: UpdaterOptions) -> Self {
        if let Some(file) = &opts.test_releases {
            tracing::warn!(file = %file.display(), "STREAMLOOP_TEST_RELEASES is set: updates come from this file and 127.0.0.1, never from GitHub");
        }
        let state = State {
            latest_version: None,
            update_available: false,
            status: UpdateStatus::Idle,
            error: None,
            node_notice: None,
            download_percent: None,
            offer: None,
            staged: None,
            stage_failure: None,
            last_check: None,
            view: None,
            off_air_since: None,
        };
        Self {
            inner: Arc::new(Inner {
                current: opts.current,
                dev_mode: opts.dev_mode,
                http: opts.http,
                runner: opts.runner,
                clock: opts.clock,
                layout: opts.layout,
                releases_url: opts.releases_url,
                test_releases: opts.test_releases,
                bundled_node: opts.bundled_node.as_deref().and_then(parse_node_version),
                policy: opts.policy,
                state: Mutex::new(state),
                stage_lock: Arc::new(tokio::sync::Mutex::new(())),
            }),
        }
    }

    pub fn status(&self) -> UpdateInfo {
        let st = self.inner.lock();
        self.inner.info(&st)
    }

    /// Looks for a newer release, and starts staging it in the background when one is found.
    /// A check beside a check or a stage, or within 60 s of the last one, returns the status as it is.
    pub async fn check(&self) -> UpdateInfo {
        let now = self.inner.clock.now();
        {
            let mut st = self.inner.lock();
            if matches!(st.status, UpdateStatus::Checking | UpdateStatus::Downloading | UpdateStatus::Ready) {
                return self.inner.info(&st);
            }
            let cooling = st.last_check.is_some_and(|t| now.duration_since(t).map_or(true, |d| d < CHECK_COOLDOWN));
            if cooling && st.latest_version.is_some() {
                return self.inner.info(&st);
            }
            st.status = UpdateStatus::Checking;
            st.error = None;
            st.stage_failure = None;
            // An offer with nothing staged goes, so a failed check leaves nothing half-known to stage.
            // A staged exe stays offered, and can land, until a check that answers says otherwise.
            if self.inner.staged_locked(&st).is_none() {
                st.update_available = false;
                st.offer = None;
                st.node_notice = None;
            }
        }

        let result = self.inner.find_update().await;

        let to_stage = {
            let mut st = self.inner.lock();
            // A landing that started meanwhile owns the status.
            let checking = st.status == UpdateStatus::Checking;
            match result {
                Ok(found) => {
                    tracing::info!(current = %self.inner.current, latest = %found.latest, available = found.offer.is_some(), "Update check complete");
                    st.latest_version = Some(found.latest);
                    st.update_available = found.offer.is_some();
                    st.offer = found.offer;
                    st.node_notice = found.node_notice;
                    st.last_check = Some(now);
                    if checking {
                        st.status = UpdateStatus::Idle;
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Update check failed");
                    if checking {
                        st.status = UpdateStatus::Error;
                        st.error = Some(e);
                    }
                }
            }
            checking && !self.inner.dev_mode && st.offer.is_some() && self.inner.staged_locked(&st).is_none()
        };
        if to_stage {
            self.stage_in_background();
        }
        self.status()
    }

    /// Downloads and checks the offered exe, unless it is staged already. Beside a running stage it
    /// waits for that one first.
    pub async fn stage(&self) -> Result<(), String> {
        if self.inner.dev_mode {
            return Err("Cannot apply updates in dev mode".into());
        }
        if self.inner.lock().status == UpdateStatus::Checking {
            return Err("Update already in progress".into());
        }
        let guard = self.inner.stage_lock.clone().lock_owned().await;
        self.inner.clone().stage_holding(guard).await
    }

    /// Waits until the stage a `check` started has ended.
    pub async fn wait_for_stage(&self) {
        drop(self.inner.stage_lock.lock().await);
    }

    /// The version of the verified exe in `_update\`, while the last check still offers it.
    pub fn staged_version(&self) -> Option<String> {
        let st = self.inner.lock();
        self.inner.staged_locked(&st)
    }

    /// Records what the stream is doing, for the notice and the landing decision, and says whether
    /// a staged update should land now. Called on every supervisor poll.
    pub fn observe(&self, sup: Option<&SupervisorStatus>, stopped: bool, auto_check: bool) -> bool {
        let now = self.inner.clock.now();
        let rolled_back = rolled_back_version(&self.inner.layout.rolled_back);
        let live = encoder_live(sup);
        let mut st = self.inner.lock();
        if live {
            st.off_air_since = None;
        } else if st.off_air_since.is_none() {
            st.off_air_since = Some(now);
        }
        let off_air_for = st.off_air_since.and_then(|t| now.duration_since(t).ok()).unwrap_or_default();
        let uptime_s = if live { encoder_uptime(sup) } else { st.view.as_ref().and_then(|v| v.uptime_s) };
        st.view = Some(View { live, uptime_s, off_air_for, stopped, auto_check, rolled_back: rolled_back.clone() });
        let staged = self.inner.staged_locked(&st);
        drop(st);
        should_land(&self.inner.policy, auto_check, staged.as_deref(), rolled_back.as_deref(), sup, stopped, off_air_for)
    }

    /// Step 1 of a landing: the status reads `ready` until the process exits or the landing fails.
    pub fn landing_started(&self) {
        let mut st = self.inner.lock();
        st.status = UpdateStatus::Ready;
        st.error = None;
    }

    /// A landing that did not go through: the status reads `error` with the reason. Forgetting the
    /// staged exe keeps the next poll from landing it again before a new stage.
    pub fn landing_failed(&self, reason: &str) {
        let mut st = self.inner.lock();
        st.status = UpdateStatus::Error;
        st.error = Some(reason.to_string());
        st.staged = None;
    }

    fn stage_in_background(&self) {
        let inner = self.inner.clone();
        match inner.stage_lock.clone().try_lock_owned() {
            // Taken before this returns, so `wait_for_stage` right after a check waits for this stage.
            Ok(guard) => {
                tokio::spawn(async move {
                    let _ = inner.stage_holding(guard).await;
                });
            }
            Err(_) => {
                tokio::spawn(async move {
                    let guard = inner.stage_lock.clone().lock_owned().await;
                    let _ = inner.stage_holding(guard).await;
                });
            }
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn info(&self, st: &State) -> UpdateInfo {
        let (state, hours) = self.notice_state(st);
        UpdateInfo {
            current_version: self.current.clone(),
            latest_version: st.latest_version.clone(),
            update_available: st.update_available,
            status: st.status,
            error: st.error.clone(),
            is_dev_mode: self.dev_mode,
            node_notice: notice_text(state, hours, st.node_notice.clone()),
            kind: st.update_available.then(|| "app".to_string()),
            parts: Vec::new(),
            same_version: false,
            download_percent: st.download_percent,
        }
    }

    fn notice_state(&self, st: &State) -> (NoticeState, Option<u32>) {
        if self.dev_mode || !st.update_available {
            return (NoticeState::Quiet, None);
        }
        if let Some(reason) = &st.stage_failure {
            return (NoticeState::Failed(reason.clone()), None);
        }
        let Some(view) = &st.view else { return (NoticeState::Quiet, None) };
        if !view.auto_check {
            return (NoticeState::UpdateNowOnly, None);
        }
        let Some(staged) = self.staged_locked(st) else { return (NoticeState::Quiet, None) };
        if view.rolled_back.as_deref() == Some(staged.as_str()) {
            return (NoticeState::UpdateNowOnly, None);
        }
        let landing_soon = self.policy.off_air_after.saturating_sub(OFF_AIR_NOTICE_LEAD);
        if view.stopped || (!view.live && view.off_air_for >= landing_soon) {
            return (NoticeState::OffAir, None);
        }
        if !view.live && view.uptime_s.is_none() {
            return (NoticeState::Quiet, None);
        }
        (NoticeState::OnAir, view.uptime_s.map(|u| hours_until(self.policy.break_at_s, u)))
    }

    fn staged_locked(&self, st: &State) -> Option<String> {
        let staged = st.staged.as_ref()?;
        let offered = st.offer.as_ref()?;
        (*staged == offered.version && self.layout.staged_exe.is_file()).then(|| staged.clone())
    }

    fn allowed(&self, url: &str) -> bool {
        reqwest::Url::parse(url).is_ok_and(|u| url_allowed(&u, &ALLOWED_DOWNLOAD_HOSTS, self.test_releases.is_some()))
    }

    fn part_path(&self) -> PathBuf {
        self.layout.update_dir.join("streamloop.exe.part")
    }

    fn user_agent(&self) -> String {
        format!("streamloop/{}", self.current)
    }

    async fn find_update(&self) -> Result<Found, String> {
        let Some(releases) = self.fetch_releases().await? else {
            // GitHub answers 404 for a repository with no releases yet.
            return Ok(Found { latest: self.current.clone(), offer: None, node_notice: None });
        };
        let picked = pick_release(&releases, &self.current);
        let latest = picked.map_or_else(|| self.current.clone(), |r| strip_v(&r.tag_name).to_string());
        let release = match picked {
            Some(r) if compare_versions(&latest, &self.current) == Ordering::Greater => r,
            _ => return Ok(Found { latest, offer: None, node_notice: None }),
        };
        let assets = select_exe_assets(release).expect("pick_release takes only releases with an exe and its checksum");
        for url in [&assets.exe_url, &assets.checksum_url] {
            if !self.allowed(url) {
                return Err(format!("Untrusted download URL domain: {url}"));
            }
        }
        // Dev mode runs no bundled Node.
        let node_notice = if self.dev_mode { None } else { self.release_node_notice(release).await };
        let offer = Offer { version: latest.clone(), exe_url: assets.exe_url, checksum_url: assets.checksum_url };
        Ok(Found { latest, offer: Some(offer), node_notice })
    }

    /// The release list, or `None` for GitHub's 404.
    async fn fetch_releases(&self) -> Result<Option<Vec<GithubRelease>>, String> {
        if let Some(file) = &self.test_releases {
            let text = tokio::fs::read_to_string(file).await.map_err(|e| format!("Could not read {}: {e}", file.display()))?;
            return serde_json::from_str(text.trim_start_matches('\u{feff}')).map(Some).map_err(|e| format!("{} is not a release list: {e}", file.display()));
        }
        let mut req = HttpRequest::get(&self.releases_url);
        req.headers = vec![("Accept".into(), "application/vnd.github.v3+json".into()), ("User-Agent".into(), self.user_agent())];
        req.timeout = Some(GITHUB_TIMEOUT);
        let res = match tokio::time::timeout(GITHUB_TIMEOUT, self.http.send(req)).await {
            Err(_) => return Err(GITHUB_SILENT.into()),
            Ok(Err(e)) if e.kind == HttpErrorKind::Timeout => return Err(GITHUB_SILENT.into()),
            Ok(Err(e)) => return Err(e.message),
            Ok(Ok(res)) => res,
        };
        if res.status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&res.status) {
            let reason = reqwest::StatusCode::from_u16(res.status).ok().and_then(|s| s.canonical_reason()).unwrap_or("");
            return Err(format!("GitHub API returned {}: {reason}", res.status));
        }
        serde_json::from_slice(&res.body).map(Some).map_err(|e| format!("GitHub sent a release list that could not be read: {e}"))
    }

    /// A GET for a small asset, with GitHub's timeout.
    async fn fetch_text(&self, url: &str) -> Result<String, String> {
        let mut req = HttpRequest::get(url);
        req.headers = vec![("User-Agent".into(), self.user_agent())];
        req.timeout = Some(GITHUB_TIMEOUT);
        let res = match tokio::time::timeout(GITHUB_TIMEOUT, self.http.send(req)).await {
            Err(_) => return Err(format!("{url} did not answer within 10 seconds")),
            Ok(Err(e)) => return Err(e.message),
            Ok(Ok(res)) => res,
        };
        if !(200..300).contains(&res.status) {
            return Err(format!("HTTP {}", res.status));
        }
        Ok(res.text())
    }

    async fn release_node_notice(&self, release: &GithubRelease) -> Option<String> {
        let bundled = self.bundled_node.as_deref()?;
        let url = &release.assets.iter().find(|a| a.name == "node-version.txt")?.browser_download_url;
        if !self.allowed(url) {
            return None;
        }
        let text = match self.fetch_text(url).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(error = %e, "Could not read the release Node version");
                return None;
            }
        };
        let release_node = parse_node_version(&text)?;
        (release_node != bundled).then(|| format!("This release bundles Node {release_node}. Install the full ZIP to get it."))
    }

    async fn stage_holding(self: Arc<Self>, _guard: OwnedMutexGuard<()>) -> Result<(), String> {
        let offer = {
            let mut st = self.lock();
            if st.status == UpdateStatus::Ready {
                return Err("Update already in progress".into());
            }
            let Some(offer) = st.offer.clone() else {
                return Err("No update available to apply".into());
            };
            if self.staged_locked(&st).is_some() {
                return Ok(());
            }
            st.status = UpdateStatus::Downloading;
            st.download_percent = None;
            st.stage_failure = None;
            st.staged = None;
            offer
        };
        let mut cut_off = CutOffStage { inner: &self, armed: true };
        tracing::info!(version = %offer.version, url = %offer.exe_url, "Downloading the update");
        let result = self.download_and_verify(&offer).await;
        cut_off.armed = false;
        if result.is_err() {
            remove_file(&self.part_path());
            remove_file(&self.layout.staged_exe);
        }
        let mut st = self.lock();
        st.download_percent = None;
        if st.status == UpdateStatus::Downloading {
            st.status = UpdateStatus::Idle;
        }
        match &result {
            Ok(()) => {
                tracing::info!(version = %offer.version, "Update staged");
                st.staged = Some(offer.version);
            }
            Err(reason) => {
                tracing::error!(version = %offer.version, %reason, "The update could not be staged");
                st.stage_failure = Some(reason.clone());
            }
        }
        result
    }

    /// Downloads to `streamloop.exe.part` and renames it only once both checks pass, so a crash
    /// mid-download never leaves a partial exe where a landing would take it.
    async fn download_and_verify(&self, offer: &Offer) -> Result<(), String> {
        let part = self.part_path();
        std::fs::create_dir_all(&self.layout.update_dir).map_err(|e| format!("{} could not be created ({e})", self.layout.update_dir.display()))?;
        for earlier in [&part, &self.layout.staged_exe] {
            if earlier.exists() {
                std::fs::remove_file(earlier).map_err(|e| format!("an earlier {} could not be removed ({e})", earlier.display()))?;
            }
        }
        let dest = &part;

        let counter = Mutex::new(DownloadCounter::new());
        let report = |received: u64, total: Option<u64>| {
            let percent = counter.lock().unwrap_or_else(|e| e.into_inner()).on_progress(received, total, self.clock.now());
            if let Some(p) = percent {
                self.lock().download_percent = Some(p);
            }
        };
        self.http.download_with_progress(&offer.exe_url, dest, &report).await.map_err(|e| e.message)?;
        if let Some(p) = counter.lock().unwrap_or_else(|e| e.into_inner()).finish() {
            self.lock().download_percent = Some(p);
        }

        let checksum = self.fetch_text(&offer.checksum_url).await.map_err(|e| format!("the checksum file could not be read ({e})"))?;
        let expected = checksum.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        let actual = sha256_file(dest).await.map_err(|e| format!("the downloaded exe could not be read ({e})"))?;
        if actual != expected {
            return Err("the checksum does not match".into());
        }

        let program = dest.to_string_lossy();
        let out = self.runner.run(RunSpec::new(&program, &["--version"], VERSION_CHECK_TIMEOUT)).await.map_err(|e| match e.kind {
            RunErrorKind::Timeout => "the downloaded exe did not answer --version within 10 seconds".to_string(),
            _ => format!("the downloaded exe did not run ({})", e.message),
        })?;
        match out.code {
            Some(0) => {}
            Some(c) => return Err(format!("the downloaded exe exited with code {c} on --version")),
            None => return Err("the downloaded exe ended without an exit code on --version".into()),
        }
        let printed = out.stdout_text().trim().to_string();
        if printed != offer.version {
            return Err(format!("the downloaded exe reports version {printed}, not {}", offer.version));
        }
        std::fs::rename(&part, &self.layout.staged_exe).map_err(|e| format!("{} could not be renamed ({e})", part.display()))
    }
}

/// Puts the status back when a stage ends early, by a panic or by a caller that dropped it,
/// so `downloading` never sticks.
struct CutOffStage<'a> {
    inner: &'a Inner,
    armed: bool,
}

impl Drop for CutOffStage<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        remove_file(&self.inner.part_path());
        let mut st = self.inner.lock();
        st.download_percent = None;
        if st.status == UpdateStatus::Downloading {
            st.status = UpdateStatus::Idle;
        }
        st.stage_failure.get_or_insert_with(|| STAGE_CUT_OFF.to_string());
    }
}

/// `https` on one of `hosts`, or under the test hook only `http://127.0.0.1:<port>`.
fn url_allowed(url: &reqwest::Url, hosts: &[&str], test_hook: bool) -> bool {
    if test_hook {
        return url.scheme() == "http" && url.host_str() == Some("127.0.0.1") && url.port().is_some();
    }
    url.scheme() == "https" && url.host_str().is_some_and(|h| hosts.contains(&h))
}

pub fn redirect_allowed(url: &str, test_hook: bool) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| url_allowed(&u, &REDIRECT_HOSTS, test_hook))
}

/// The updater's HTTP client: it follows redirects only to GitHub's hosts, or to 127.0.0.1 under
/// the test hook (`STREAMLOOP_TEST_RELEASES`).
pub fn updater_http(test_hook: bool) -> ReqwestHttp {
    ReqwestHttp::following_only(move |u| url_allowed(u, &REDIRECT_HOSTS, test_hook))
}

fn remove_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %path.display(), error = %e, "Could not remove a file of the staged update");
        }
    }
}

async fn sha256_file(path: &Path) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The `to` of `data\update-rolled-back`: the version that did not come up.
fn rolled_back_version(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    value.get("to")?.as_str().map(|s| strip_v(s).to_string())
}

fn encoder_live(sup: Option<&SupervisorStatus>) -> bool {
    sup.and_then(|s| s.encoder.as_ref()).and_then(|e| e.alive) == Some(true)
}

fn encoder_uptime(sup: Option<&SupervisorStatus>) -> Option<u64> {
    sup.and_then(|s| s.encoder.as_ref()).and_then(|e| e.uptime_s).flatten()
}

/// Whole hours to the break, rounded, and at least 1.
fn hours_until(break_at_s: u64, uptime_s: u64) -> u32 {
    let left = break_at_s.saturating_sub(uptime_s);
    ((left + 1800) / 3600).clamp(1, u64::from(u32::MAX)) as u32
}

fn strip_v(s: &str) -> &str {
    s.strip_prefix('v').unwrap_or(s)
}

/// JavaScript's `Number()` for a version segment: blank is 0, anything not a number is `None` (NaN).
fn js_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return Some(0.0);
    }
    t.parse::<f64>().ok().filter(|n| !n.is_nan())
}

/// Numeric X.Y.Z compare. A suffixed version such as `3.0.0-dev` sorts below its release, a
/// missing segment counts as 0, and a segment that is not a number compares equal to anything.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    fn split(v: &str) -> (Vec<Option<f64>>, bool) {
        let mut halves = strip_v(v).splitn(2, '-');
        let core = halves.next().unwrap_or("");
        (core.split('.').map(js_number).collect(), halves.next().is_some())
    }
    let (pa, suffix_a) = split(a);
    let (pb, suffix_b) = split(b);
    for i in 0..pa.len().max(pb.len()) {
        let na = pa.get(i).copied().unwrap_or(Some(0.0));
        let nb = pb.get(i).copied().unwrap_or(Some(0.0));
        if let (Some(x), Some(y)) = (na, nb) {
            if x > y {
                return Ordering::Greater;
            }
            if x < y {
                return Ordering::Less;
            }
        }
    }
    match (suffix_a, suffix_b) {
        (false, true) => Ordering::Greater,
        (true, false) => Ordering::Less,
        _ => Ordering::Equal,
    }
}

/// The three numbers of a `vX.Y.Z` or `X.Y.Z` tag.
fn tag_parts(tag: &str) -> Option<[&str; 3]> {
    let mut parts = strip_v(tag).split('.');
    let numbers = [parts.next()?, parts.next()?, parts.next()?];
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    (parts.next().is_none() && numbers.iter().all(|p| digits(p))).then_some(numbers)
}

fn release_version(r: &GithubRelease) -> Option<String> {
    tag_parts(&r.tag_name).map(|p| p.join("."))
}

fn asset_url<'a>(r: &'a GithubRelease, name: &str) -> Option<&'a str> {
    r.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.as_str())
}

/// The Node version a release names in its `node-version.txt`.
pub fn parse_node_version(text: &str) -> Option<String> {
    tag_parts(text.trim()).map(|p| p.join("."))
}

/// `streamloop-vX.Y.Z.exe` and its checksum: `streamloop-vX.Y.Z.exe.sha256`, else
/// `streamloop-vX.Y.Z.sha256`. The base name is also the full ZIP's checksum, so it comes second.
pub fn select_exe_assets(r: &GithubRelease) -> Option<ExeAssets> {
    let v = release_version(r)?;
    let exe = format!("streamloop-v{v}.exe");
    let exe_url = asset_url(r, &exe)?;
    let checksum_url = asset_url(r, &format!("{exe}.sha256")).or_else(|| asset_url(r, &format!("streamloop-v{v}.sha256")))?;
    Some(ExeAssets { exe_url: exe_url.into(), checksum_url: checksum_url.into() })
}

/// The newest release with the running major version that carries the full ZIP, the exe and its
/// checksum. 3.x installs beside 2.x, so neither takes the other's; from 3 up prereleases count.
pub fn pick_release<'a>(releases: &'a [GithubRelease], current: &str) -> Option<&'a GithubRelease> {
    let major = strip_v(current).split('.').next().unwrap_or("");
    let allow_prerelease = js_number(major).is_some_and(|n| n >= 3.0);
    let mut best: Option<&GithubRelease> = None;
    for r in releases {
        let Some(version) = release_version(r) else { continue };
        let Some([release_major, _, _]) = tag_parts(&r.tag_name) else { continue };
        if r.draft || (r.prerelease && !allow_prerelease) || release_major != major {
            continue;
        }
        // A release still uploading its assets must not beat a complete older one.
        if asset_url(r, &format!("streamloop-v{version}.zip")).is_none() || select_exe_assets(r).is_none() {
            continue;
        }
        if best.is_none_or(|b| compare_versions(&r.tag_name, &b.tag_name) == Ordering::Greater) {
            best = Some(r);
        }
    }
    best
}

/// With `autoUpdateCheck` on: stopped from the dashboard, the encoder at the break, or no live
/// encoder for `off_air_after`. A version that rolled back once waits for Update Now.
pub fn should_land(
    policy: &BreakPolicy,
    auto: bool,
    staged: Option<&str>,
    rolled_back: Option<&str>,
    sup: Option<&SupervisorStatus>,
    stopped: bool,
    off_air_for: Duration,
) -> bool {
    let Some(staged) = staged else { return false };
    if !auto || rolled_back == Some(staged) {
        return false;
    }
    if stopped {
        return true;
    }
    if encoder_live(sup) {
        return encoder_uptime(sup).is_some_and(|u| u >= policy.break_at_s);
    }
    off_air_for >= policy.off_air_after
}

/// The line under the update banner, with the Node notice after it.
pub fn notice_text(state: NoticeState, hours: Option<u32>, node_notice: Option<String>) -> Option<String> {
    let own = match state {
        NoticeState::Quiet => None,
        NoticeState::OnAir => Some(match hours {
            Some(n) => {
                let hours = if n == 1 { "1 hour".to_string() } else { format!("{n} hours") };
                format!("It installs by itself in about {hours}, just before Twitch's 48-hour cut. Update Now installs it at once and restarts the stream.")
            }
            None => UPDATE_NOW.to_string(),
        }),
        NoticeState::OffAir => Some("It installs within a minute, since the stream is off air.".to_string()),
        NoticeState::UpdateNowOnly => Some(UPDATE_NOW.to_string()),
        NoticeState::Failed(reason) => Some(format!("The download failed: {}. It is tried again at the next check.", reason.trim_end_matches('.'))),
    };
    match (own, node_notice) {
        (Some(own), Some(node)) => Some(format!("{own} {node}")),
        (own, node) => own.or(node),
    }
}
