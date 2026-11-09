//! Lands a staged exe (spec steps 1 to 4: stop the supervisor, swap `streamloop.exe` by renames,
//! start the supervisor from the new exe), and on the next start confirms the update or rolls it back.
//! A landing or a rollback never changes whether the stream is held.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use streamloop_common::api::SupervisorStatus;
use streamloop_common::paths::Layout;
use streamloop_common::time::iso;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::clock::Clock;
use crate::discord::{Level, Notifier};
use crate::events::{Alert, DiscordKind};
use crate::launcher::{LaunchOutcome, Launcher};
use crate::runner::{RunErrorKind, RunSpec, Runner};
use crate::supervisor_client::SupervisorApi;
use crate::timeline::EventLog;
use crate::updater::Updater;

/// How long the new version has to come up before it is rolled back.
pub const CONFIRM_WITHIN: Duration = Duration::from_secs(180);
const SUPERVISOR_PORT: u16 = 4417;
/// The token server port of `streamloop.exe check`.
const CHECK_TOKEN_PORT: u16 = 4418;
const SHUTDOWN_WAIT: Duration = Duration::from_secs(20);
/// After the tree kill, so the new supervisor does not find the port still taken.
const AFTER_KILL_WAIT: Duration = Duration::from_secs(5);
const GONE_POLL: Duration = Duration::from_millis(500);
const CONFIRM_POLL: Duration = Duration::from_secs(2);
/// The guard's test encode and the `launch` helper's port wait both fit well inside this.
const LAUNCH_WAIT: Duration = Duration::from_secs(60);
/// Master's bound on one run of the check, well above its own worst case.
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const PERCENT_POLL: Duration = Duration::from_secs(1);
const RENAME_RETRIES: u32 = 3;
const RENAME_PAUSE: Duration = Duration::from_millis(200);
const CHECK_PREFIX: &str = "streamloop-supervisor check:";
/// Master's text for an update that a restart cut off.
const RESTARTED: &str = "StreamLoop was restarted during the update.";
/// START.bat's crash-branch rollback, in the words it prints.
const STOPPED_UNCONFIRMED: &str = "The updated StreamLoop stopped before it was confirmed.";

pub type KillTree = Arc<dyn Fn(u32) + Send + Sync>;
pub type PidOnPort = Arc<dyn Fn(u16) -> Option<u32> + Send + Sync>;

/// `_update\landed`, and `data\update-rolled-back` after a rollback, where `to` names the version that failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandedRecord {
    pub from: String,
    pub to: String,
    /// Whether the encoder was live when the landing began.
    pub on_air: bool,
    /// Stopped from the dashboard or waiting for Start before the landing: nothing is launched, and
    /// the check confirms the update instead of a supervisor answer.
    #[serde(default)]
    pub held: bool,
    /// A rollback that could not put the old exe back, reported once; later starts keep this exe.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rollback_failed: bool,
}

impl LandedRecord {
    /// `None` when the file is missing or holds no record.
    pub fn read(path: &Path) -> Option<LandedRecord> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        std::fs::write(path, serde_json::to_string(self).map_err(io::Error::other)?)
    }
}

/// After `RolledBack` the caller exits 75.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    Confirmed,
    RolledBack,
}

type Outcome = Option<Result<(), String>>;

/// One landing at a time. Build it with `LandingGate::default()`.
#[derive(Default)]
pub struct LandingGate {
    /// The landing that runs, or the one that went through. A failed one is cleared, so Update Now can try again.
    current: Mutex<Option<watch::Receiver<Outcome>>>,
    in_progress: AtomicBool,
}

pub struct LandingDeps {
    pub layout: Layout,
    pub updater: Updater,
    pub launcher: Launcher,
    pub client: Arc<dyn SupervisorApi>,
    pub notifier: Arc<dyn Notifier>,
    pub timeline: Arc<EventLog>,
    pub clock: Arc<dyn Clock>,
    /// Runs `streamloop.exe check` for a held landing.
    pub runner: Arc<dyn Runner>,
    pub kill_tree: KillTree,
    pub pid_on_port: PidOnPort,
    pub gate: LandingGate,
}

impl LandingDeps {
    /// True from the landing's first step until the process exits, or until the landing fails.
    pub fn landing_in_progress(&self) -> bool {
        self.gate.in_progress.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateStep {
    Download,
    Stop,
    Swap,
    Check,
    Start,
    Done,
}

impl UpdateStep {
    /// The dashboard's step label (`update-text.js`), in lower case as its failure text uses it.
    fn label(self) -> &'static str {
        match self {
            UpdateStep::Download => "downloading",
            UpdateStep::Stop => "stopping the stream",
            UpdateStep::Swap => "replacing files",
            UpdateStep::Check => "checking the new version",
            UpdateStep::Start => "starting the stream",
            UpdateStep::Done => "back on air",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateResult {
    Running,
    Ok,
    Failed,
    RolledBack,
}

/// `data\update-state.json` in master's shape and key order, which the dashboard reads unchanged.
/// An exe update replaces no parts, so `parts` and `folders` stay empty and `baseline` null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub from: String,
    pub to: String,
    pub parts: Vec<String>,
    pub folders: Vec<String>,
    pub step: UpdateStep,
    pub result: UpdateResult,
    pub reason: Option<String>,
    pub started_at: String,
    pub step_started_at: String,
    pub baseline: Option<Value>,
    /// Written while Update Now downloads the exe, and kept after, as master does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_percent: Option<u8>,
}

/// `None` when the file is missing or is not an update state.
pub fn read_update_state(path: &Path) -> Option<UpdateState> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// Through a `.tmp` and a rename, so the dashboard never reads half a file. Blocking, as master's is,
/// so a landing dropped half way can still record how it ended.
fn write_update_state(path: &Path, state: &UpdateState) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state).map_err(io::Error::other)?)?;
    let mut retry = 0;
    loop {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if retry < RENAME_RETRIES && held_open(&e) => {
                retry += 1;
                std::thread::sleep(RENAME_PAUSE);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Master's `failureText`: "The update to X failed at replacing files: reason. Still on Y."
fn failure_text(s: &UpdateState) -> String {
    let reason = s.reason.as_deref().filter(|r| !r.is_empty()).unwrap_or("no reason was recorded").trim_end_matches('.');
    format!("The update to {} failed at {}: {reason}. Still on {}.", s.to, s.step.label(), s.from)
}

/// An antivirus scan or an indexer can hold a file for a moment: three more tries, 200 ms apart.
async fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut retry = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if retry < RENAME_RETRIES && held_open(&e) => {
                retry += 1;
                tokio::time::sleep(RENAME_PAUSE).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Node's EPERM and EBUSY on Windows: access denied, a sharing violation, a lock violation.
fn held_open(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

fn now_iso(clock: &dyn Clock) -> String {
    iso(clock.now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64))
}

/// The landing's progress file. A write that fails is logged and the update goes on without it.
struct Progress<'a> {
    path: &'a Path,
    clock: &'a dyn Clock,
    state: UpdateState,
}

impl<'a> Progress<'a> {
    fn start(layout: &'a Layout, clock: &'a dyn Clock, from: &str, to: &str, step: UpdateStep) -> Progress<'a> {
        let at = now_iso(clock);
        let state = UpdateState {
            from: from.into(),
            to: to.into(),
            parts: Vec::new(),
            folders: Vec::new(),
            step,
            result: UpdateResult::Running,
            reason: None,
            started_at: at.clone(),
            step_started_at: at,
            baseline: None,
            download_percent: None,
        };
        let p = Progress { path: &layout.update_state, clock, state };
        p.write();
        p
    }

    /// The landing's own progress, when one for this update still runs.
    fn resume(layout: &'a Layout, clock: &'a dyn Clock, to: &str) -> Option<Progress<'a>> {
        let state = read_update_state(&layout.update_state)?;
        (state.result == UpdateResult::Running && state.to == to).then_some(Progress { path: &layout.update_state, clock, state })
    }

    fn write(&self) {
        if let Err(err) = write_update_state(self.path, &self.state) {
            tracing::warn!(%err, "Could not record the update's progress");
        }
    }

    fn step(&mut self, step: UpdateStep) {
        self.state.step = step;
        self.state.step_started_at = now_iso(self.clock);
        self.write();
    }

    fn percent(&mut self, percent: Option<u8>) {
        if percent.is_some() && percent != self.state.download_percent {
            self.state.download_percent = percent;
            self.write();
        }
    }

    fn finish(&mut self, step: UpdateStep, result: UpdateResult, reason: Option<String>) {
        if step != self.state.step {
            self.state.step = step;
            self.state.step_started_at = now_iso(self.clock);
        }
        self.state.result = result;
        self.state.reason = reason;
        self.write();
    }
}

/// Info under the `recovery` toggle, whose template reads "Recovery action: {step}".
fn recovery(text: &str) -> Alert {
    Alert { kind: DiscordKind::Recovery, vars: vec![("step".into(), Value::String(text.into()))], level: Level::Info }
}

fn encoder_alive(status: Option<&SupervisorStatus>) -> bool {
    status.and_then(|s| s.encoder.as_ref()).and_then(|e| e.alive) == Some(true)
}

/// Launches the supervisor through the launcher's guarded poll, also during the landing hold, and
/// waits for that launch to end. `start` covers a confirmation that runs before the launcher does.
async fn launch_and_wait(launcher: &Launcher) {
    launcher.start();
    match launcher.launch_now_and_wait(LAUNCH_WAIT).await {
        Some(LaunchOutcome::Exited(Some(0))) => {}
        Some(outcome) => tracing::warn!(?outcome, "The supervisor launch did not hand off"),
        None => tracing::warn!("The supervisor launch did not end within 60 s"),
    }
}

/// Clears the gate when a landing fails or is dropped half way. One that went through keeps it,
/// so nothing lands again before the exit.
struct Running<'a> {
    gate: &'a LandingGate,
    went_through: bool,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if !self.went_through {
            self.gate.in_progress.store(false, Ordering::SeqCst);
            *self.gate.current.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

/// Steps 1 to 4 of the spec's landing, staging first when nothing is staged (Update Now). A second
/// call while one runs waits for it and returns its result. On `Ok` the caller exits 75.
pub async fn land(deps: &LandingDeps) -> Result<(), String> {
    let joined = {
        let mut current = deps.gate.current.lock().unwrap_or_else(|e| e.into_inner());
        match current.as_ref() {
            Some(rx) => Err(rx.clone()),
            None => {
                let (tx, rx) = watch::channel(None);
                *current = Some(rx);
                Ok(tx)
            }
        }
    };
    let tx = match joined {
        Ok(tx) => tx,
        Err(mut rx) => {
            return match rx.wait_for(Option::is_some).await {
                Ok(outcome) => outcome.clone().unwrap_or(Ok(())),
                Err(_) => Err("The update that was already running was cut off.".into()),
            };
        }
    };
    let mut running = Running { gate: &deps.gate, went_through: false };
    let mut landing = Landing { deps, progress: None, held: false, suspended: false, landed_written: false, swapped: false, ended: false };
    let result = landing.run().await;
    landing.ended = true;
    running.went_through = result.is_ok();
    let _ = tx.send(Some(result.clone()));
    result
}

/// One landing, with what it has changed so far, so one dropped half way puts things back.
struct Landing<'a> {
    deps: &'a LandingDeps,
    progress: Option<Progress<'a>>,
    held: bool,
    suspended: bool,
    landed_written: bool,
    swapped: bool,
    ended: bool,
}

impl Drop for Landing<'_> {
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        // After the swap the next start confirms the update, so its progress stays `running` for that check.
        if !self.swapped {
            let layout = &self.deps.layout;
            if self.landed_written {
                if !layout.exe.exists() && layout.exe_old.exists() {
                    let _ = std::fs::rename(&layout.exe_old, &layout.exe);
                }
                let _ = std::fs::remove_file(&layout.landed);
            }
            if let Some(p) = self.progress.as_mut() {
                let step = p.state.step;
                p.finish(step, UpdateResult::Failed, Some(RESTARTED.into()));
            }
            if self.suspended {
                self.deps.updater.landing_failed(RESTARTED);
            }
        }
        if self.suspended {
            self.deps.launcher.resume();
        }
    }
}

impl<'a> Landing<'a> {
    async fn run(&mut self) -> Result<(), String> {
        let deps = self.deps;
        // A check or a stage in flight could replace `_update\streamloop.exe` under the swap.
        deps.updater.wait_for_check().await;
        if deps.updater.staged_version().is_none() {
            self.stage_first().await?;
        }
        self.held = deps.launcher.is_stopped_by_user() || deps.launcher.is_waiting_for_start();

        deps.gate.in_progress.store(true, Ordering::SeqCst);
        // `ready` refuses new checks and stages; one that began before it is waited for.
        deps.updater.landing_started();
        // No relaunch and no outage alert until the process exits.
        deps.launcher.suspend();
        self.suspended = true;
        deps.updater.wait_for_stage().await;
        let Some(to) = deps.updater.staged_version() else {
            return Err(self.fail_before_stop("No update is staged.".into()));
        };
        let from = deps.updater.status().current_version;

        let installing = format!("Installing StreamLoop {to}. The stream restarts.");
        deps.timeline.add(&installing);
        deps.notifier.notify_event(recovery(&installing)).await;
        match self.progress.as_mut() {
            Some(p) => {
                p.state.to = to.clone();
                p.step(UpdateStep::Stop);
            }
            None => self.progress = Some(Progress::start(&deps.layout, deps.clock.as_ref(), &from, &to, UpdateStep::Stop)),
        }
        tracing::info!(%from, %to, held = self.held, "Landing the update");

        let probe = deps.client.status().await.or_else(|| deps.launcher.last_status());
        let on_air = encoder_alive(probe.as_ref());
        stop_supervisor(deps).await;

        self.step(UpdateStep::Swap);
        let record = LandedRecord { from, to, on_air, held: self.held, rollback_failed: false };
        if let Err(reason) = self.swap_in(&record).await {
            return Err(self.fail_at_swap(reason).await);
        }
        self.swapped = true;

        // A held stream stays held, and the next start confirms the update with the check.
        if !self.held {
            self.step(UpdateStep::Start);
            launch_and_wait(&deps.launcher).await;
        }
        deps.notifier.flush().await;
        deps.timeline.flush().await;
        Ok(())
    }

    fn step(&mut self, step: UpdateStep) {
        if let Some(p) = self.progress.as_mut() {
            p.step(step);
        }
    }

    /// Update Now with nothing staged: stages the exe with the `download` step in the progress file,
    /// which leaves the stream alone if it fails.
    async fn stage_first(&mut self) -> Result<(), String> {
        let deps = self.deps;
        let info = deps.updater.status();
        if let (true, Some(to)) = (info.update_available, info.latest_version.as_deref()) {
            self.progress = Some(Progress::start(&deps.layout, deps.clock.as_ref(), &info.current_version, to, UpdateStep::Download));
        }
        let stage = deps.updater.stage();
        tokio::pin!(stage);
        let mut tick = tokio::time::interval(PERCENT_POLL);
        let result = loop {
            tokio::select! {
                result = &mut stage => break result,
                _ = tick.tick() => {
                    if let Some(p) = self.progress.as_mut() {
                        p.percent(deps.updater.status().download_percent);
                    }
                }
            }
        };
        if let (Err(reason), Some(p)) = (&result, self.progress.as_mut()) {
            p.finish(UpdateStep::Download, UpdateResult::Failed, Some(reason.clone()));
        }
        result
    }

    /// The record first, then the two renames: Windows lets a running exe be renamed, not overwritten.
    /// When the second rename fails the first is undone and the record goes, so nothing is left to confirm.
    async fn swap_in(&mut self, record: &LandedRecord) -> Result<(), String> {
        let layout = &self.deps.layout;
        record.write(&layout.landed).map_err(|e| format!("_update\\landed could not be written ({e})."))?;
        self.landed_written = true;
        let mut failed = None;
        if let Err(e) = rename_with_retry(&layout.exe, &layout.exe_old).await {
            failed = Some(format!("streamloop.exe could not be renamed to streamloop.exe.old ({e})."));
        } else if let Err(e) = rename_with_retry(&layout.staged_exe, &layout.exe).await {
            let mut reason = format!("The new streamloop.exe could not be moved into place ({e}).");
            if let Err(back) = rename_with_retry(&layout.exe_old, &layout.exe).await {
                // START.bat's preflight renames .old back when streamloop.exe is missing.
                reason.push_str(&format!(" streamloop.exe.old could not be renamed back either ({back})."));
            }
            failed = Some(reason);
        }
        let Some(reason) = failed else { return Ok(()) };
        if let Err(e) = std::fs::remove_file(&layout.landed) {
            tracing::error!(error = %e, "Could not remove _update\\landed after a landing that failed");
        }
        self.landed_written = false;
        Err(reason)
    }

    /// Nothing was stopped yet: the status says why, and the launcher carries on.
    fn fail_before_stop(&mut self, reason: String) -> String {
        tracing::error!(%reason, "The update could not be landed");
        if let Some(p) = self.progress.as_mut() {
            let step = p.state.step;
            p.finish(step, UpdateResult::Failed, Some(reason.clone()));
        }
        self.deps.updater.landing_failed(&reason);
        self.deps.launcher.resume();
        self.suspended = false;
        reason
    }

    async fn fail_at_swap(&mut self, reason: String) -> String {
        let deps = self.deps;
        tracing::error!(%reason, "The update could not be landed; the old exe stays");
        deps.updater.landing_failed(&reason);
        if let Some(p) = self.progress.as_mut() {
            p.finish(UpdateStep::Swap, UpdateResult::Failed, Some(reason.clone()));
            let text = failure_text(&p.state);
            deps.timeline.add(&text);
            deps.notifier.send(&text, Level::Warn, None).await;
        }
        deps.launcher.resume();
        self.suspended = false;
        if !self.held {
            deps.launcher.start();
            deps.launcher.launch_now();
        }
        reason
    }
}

/// Asks the supervisor to shut down and gives it 20 s to stop answering and let go of port 4417,
/// then ends its tree.
async fn stop_supervisor(deps: &LandingDeps) {
    let first_pid = (deps.pid_on_port)(SUPERVISOR_PORT);
    deps.client.shutdown().await;
    if wait_until_gone(deps, SHUTDOWN_WAIT).await {
        return;
    }
    let Some(pid) = (deps.pid_on_port)(SUPERVISOR_PORT).or(first_pid) else {
        tracing::warn!("The supervisor still answers after 20 s and no process holds port 4417");
        return;
    };
    tracing::warn!(pid, "The supervisor did not stop within 20 s; ending its process tree");
    (deps.kill_tree)(pid);
    if !wait_until_gone(deps, AFTER_KILL_WAIT).await {
        tracing::warn!(pid, "The supervisor still answers after its process tree was ended");
    }
}

async fn wait_until_gone(deps: &LandingDeps, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if deps.client.status().await.is_none() && (deps.pid_on_port)(SUPERVISOR_PORT).is_none() {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        tokio::time::sleep(GONE_POLL.min(deadline - now)).await;
    }
}

/// For a start with no `_update\landed` to confirm: a progress still `running` belongs to a landing
/// or a check that was cut off, and is ended here so neither the dashboard nor the 409 gate waits on it.
pub fn settle_unfinished_update(layout: &Layout, clock: &dyn Clock, running_version: &str) {
    if layout.landed.exists() {
        return;
    }
    let Some(state) = read_update_state(&layout.update_state).filter(|s| s.result == UpdateResult::Running) else {
        return;
    };
    let (result, reason) = if state.to == running_version {
        (UpdateResult::Ok, None)
    } else if matches!(state.step, UpdateStep::Download | UpdateStep::Stop | UpdateStep::Swap) {
        (UpdateResult::Failed, Some(RESTARTED.to_string()))
    } else {
        (UpdateResult::RolledBack, Some(STOPPED_UNCONFIRMED.to_string()))
    };
    tracing::warn!(to = %state.to, ?result, "Recording how an earlier update ended");
    let step = state.step;
    let mut progress = Progress { path: &layout.update_state, clock, state };
    progress.finish(step, result, reason);
}

/// Confirms when, within `within`, the new version answers with a live encoder (any encoder after an
/// off-air landing); a held landing, or a Stop during the window, is confirmed by `streamloop.exe check`.
/// Otherwise rolls back, and the caller exits 75. A record this exe did not land, or whose rollback failed, gives `Confirmed`.
pub async fn confirm_or_roll_back(deps: &LandingDeps, record: LandedRecord, within: Duration) -> Confirmation {
    let running = deps.updater.status().current_version;
    if running != record.to {
        // The landing stopped before its swap, so the old exe still runs and there is nothing to put back.
        tracing::warn!(to = %record.to, %running, "Dropping the record of a landing that did not swap");
        if let Err(e) = std::fs::remove_file(&deps.layout.landed) {
            tracing::error!(error = %e, "Could not remove _update\\landed");
        }
        settle_unfinished_update(&deps.layout, deps.clock.as_ref(), &running);
        return Confirmation::Confirmed;
    }
    if record.rollback_failed {
        tracing::warn!(to = %record.to, "Keeping this exe: its rollback could not put the old one back");
        return Confirmation::Confirmed;
    }
    let mut progress = Progress::resume(&deps.layout, deps.clock.as_ref(), &record.to);
    if let Some(p) = progress.as_mut() {
        p.step(UpdateStep::Check);
    }
    tracing::info!(from = %record.from, to = %record.to, on_air = record.on_air, held = record.held, "Checking the landed update");
    let deadline = Instant::now() + within;
    loop {
        if record.held || deps.launcher.is_stopped_by_user() {
            return match run_check(deps).await {
                Ok(()) => {
                    confirm(deps, &record, progress, false).await;
                    Confirmation::Confirmed
                }
                Err(reason) => roll_back(deps, &record, progress, reason).await,
            };
        }
        let status = deps.client.status().await;
        let version = status.as_ref().and_then(|s| s.version.as_deref());
        let alive = encoder_alive(status.as_ref());
        if version == Some(record.to.as_str()) && (alive || !record.on_air) {
            confirm(deps, &record, progress, alive).await;
            return Confirmation::Confirmed;
        }
        let now = Instant::now();
        if now >= deadline {
            let reason = if version == Some(record.to.as_str()) {
                format!("The stream did not come back on air within {} s.", within.as_secs())
            } else {
                format!("The new supervisor did not answer within {} s.", within.as_secs())
            };
            return roll_back(deps, &record, progress, reason).await;
        }
        tokio::time::sleep(CONFIRM_POLL.min(deadline - now)).await;
    }
}

/// `streamloop.exe check --root ROOT` from the new exe. A run that outlives its bound leaves its
/// token server on 4418, so that is ended too.
async fn run_check(deps: &LandingDeps) -> Result<(), String> {
    let exe = deps.layout.exe.to_string_lossy();
    let root = deps.layout.root.to_string_lossy();
    let failed = |why: String| format!("The new supervisor failed its check: {why}");
    match deps.runner.run(RunSpec::new(&exe, &["check", "--root", &root], CHECK_TIMEOUT)).await {
        Ok(out) if out.code == Some(0) => Ok(()),
        Ok(out) => {
            let stderr = out.stderr_text();
            let said = stderr.trim().lines().last().map(|l| l.trim().trim_start_matches(CHECK_PREFIX).trim()).filter(|l| !l.is_empty());
            Err(failed(match (said, out.code) {
                (Some(said), _) => said.to_string(),
                (None, Some(code)) => format!("it exited with code {code}"),
                (None, None) => "it ended without an exit code".to_string(),
            }))
        }
        Err(e) if e.kind == RunErrorKind::Timeout => {
            if let Some(pid) = (deps.pid_on_port)(CHECK_TOKEN_PORT) {
                (deps.kill_tree)(pid);
            }
            Err(failed(format!("it did not finish within {} s", CHECK_TIMEOUT.as_secs())))
        }
        Err(e) => Err(failed(e.message)),
    }
}

async fn confirm(deps: &LandingDeps, record: &LandedRecord, progress: Option<Progress<'_>>, on_air_now: bool) {
    let layout = &deps.layout;
    for path in [&layout.landed, &layout.exe_old] {
        if let Err(e) = std::fs::remove_file(path) {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), error = %e, "Could not remove a file the update left");
            }
        }
    }
    if let Err(e) = std::fs::remove_dir_all(&layout.update_dir) {
        if e.kind() != io::ErrorKind::NotFound {
            tracing::warn!(error = %e, "Could not remove _update");
        }
    }
    let text = format!("Updated to StreamLoop {}.", record.to);
    tracing::info!(version = %record.to, "The update came up");
    deps.timeline.add(&text);
    deps.notifier.notify_event(recovery(&text)).await;
    if let Some(mut p) = progress {
        let step = if on_air_now { UpdateStep::Done } else { UpdateStep::Start };
        p.finish(step, UpdateResult::Ok, None);
    }
}

async fn roll_back(deps: &LandingDeps, record: &LandedRecord, progress: Option<Progress<'_>>, reason: String) -> Confirmation {
    let layout = &deps.layout;
    tracing::error!(to = %record.to, from = %record.from, %reason, "The update did not come up; rolling back");
    deps.launcher.suspend();
    if let Err(e) = swap_back(layout).await {
        keep_after_failed_rollback(deps, record, progress, &e).await;
        return Confirmation::Confirmed;
    }
    keep_rolled_back(layout, record).await;
    stop_supervisor(deps).await;
    // A held stream stays held; anything else goes back on air on the old exe.
    if !record.held && !deps.launcher.is_stopped_by_user() {
        launch_and_wait(&deps.launcher).await;
    }
    let text = format!("The update to {} did not come up, so StreamLoop went back to {}.", record.to, record.from);
    deps.timeline.add(&text);
    deps.notifier.notify_critical(&text).await;
    if let Some(mut p) = progress {
        p.finish(UpdateStep::Check, UpdateResult::RolledBack, Some(reason));
    }
    deps.notifier.flush().await;
    deps.timeline.flush().await;
    Confirmation::RolledBack
}

/// The old exe could not be put back: this exe stays, the critical goes out once, and the record
/// is marked so a later start neither tries again nor exits 75 for it.
async fn keep_after_failed_rollback(deps: &LandingDeps, record: &LandedRecord, progress: Option<Progress<'_>>, error: &str) {
    tracing::error!(%error, "The previous exe could not be put back; keeping this one");
    let text = format!("The update to {} did not come up, and the previous version could not be put back: {error}", record.to);
    deps.timeline.add(&text);
    deps.notifier.notify_critical(&text).await;
    let marked = LandedRecord { rollback_failed: true, ..record.clone() };
    if let Err(e) = marked.write(&deps.layout.landed) {
        tracing::error!(error = %e, "Could not mark _update\\landed");
    }
    if let Some(mut p) = progress {
        p.finish(UpdateStep::Check, UpdateResult::Failed, Some(format!("The previous version could not be put back: {error}")));
    }
    deps.launcher.resume();
    deps.notifier.flush().await;
    deps.timeline.flush().await;
}

/// `streamloop.exe` to `.bad`, then `.old` to `streamloop.exe`; the first is undone when the second fails.
async fn swap_back(layout: &Layout) -> Result<(), String> {
    rename_with_retry(&layout.exe, &layout.exe_bad).await.map_err(|e| format!("streamloop.exe could not be renamed to streamloop.exe.bad ({e})."))?;
    if let Err(e) = rename_with_retry(&layout.exe_old, &layout.exe).await {
        let mut reason = format!("streamloop.exe.old could not be renamed to streamloop.exe ({e}).");
        if let Err(undo) = rename_with_retry(&layout.exe_bad, &layout.exe).await {
            reason.push_str(&format!(" streamloop.exe.bad could not be renamed back either ({undo})."));
        }
        return Err(reason);
    }
    Ok(())
}

/// Moves `_update\landed` to `data\update-rolled-back`, which keeps the version from landing by itself again.
async fn keep_rolled_back(layout: &Layout, record: &LandedRecord) {
    if let Err(e) = std::fs::create_dir_all(&layout.data) {
        tracing::warn!(error = %e, "Could not create data");
    }
    if let Err(e) = rename_with_retry(&layout.landed, &layout.rolled_back).await {
        tracing::warn!(error = %e, "Could not move _update\\landed; writing data\\update-rolled-back instead");
        if let Err(e) = record.write(&layout.rolled_back).and_then(|()| std::fs::remove_file(&layout.landed)) {
            tracing::error!(error = %e, "Could not record the rollback");
        }
    }
    // Empty once the staged exe and the record are gone.
    let _ = std::fs::remove_dir(&layout.update_dir);
}
