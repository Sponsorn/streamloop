//! Lands a staged exe (spec steps 1 to 4: stop the supervisor, swap `streamloop.exe` by renames,
//! start the supervisor from the new exe), and on the next start confirms the update or rolls it back.

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
use crate::launcher::Launcher;
use crate::supervisor_client::SupervisorApi;
use crate::timeline::EventLog;
use crate::updater::Updater;

/// How long the new version has to come up before it is rolled back.
pub const CONFIRM_WITHIN: Duration = Duration::from_secs(180);
const SUPERVISOR_PORT: u16 = 4417;
const SHUTDOWN_WAIT: Duration = Duration::from_secs(20);
/// After the tree kill, so the new supervisor does not find the port still taken.
const AFTER_KILL_WAIT: Duration = Duration::from_secs(5);
const GONE_POLL: Duration = Duration::from_millis(500);
const CONFIRM_POLL: Duration = Duration::from_secs(2);
const RENAME_RETRIES: u32 = 3;
const RENAME_PAUSE: Duration = Duration::from_millis(200);

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
}

impl LandedRecord {
    /// `None` when the file is missing or holds no record.
    pub fn read(path: &Path) -> Option<LandedRecord> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
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
}

/// `None` when the file is missing or is not an update state.
pub fn read_update_state(path: &Path) -> Option<UpdateState> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// Through a `.tmp` and a rename, so the dashboard never reads half a file.
async fn write_update_state(path: &Path, state: &UpdateState) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state).map_err(io::Error::other)?)?;
    rename_with_retry(&tmp, path).await
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
    async fn start(deps: &'a LandingDeps, from: &str, to: &str) -> Progress<'a> {
        let at = now_iso(deps.clock.as_ref());
        let state = UpdateState {
            from: from.into(),
            to: to.into(),
            parts: Vec::new(),
            folders: Vec::new(),
            step: UpdateStep::Stop,
            result: UpdateResult::Running,
            reason: None,
            started_at: at.clone(),
            step_started_at: at,
            baseline: None,
        };
        let p = Progress { path: &deps.layout.update_state, clock: deps.clock.as_ref(), state };
        p.write().await;
        p
    }

    /// The landing's own progress, when one for this update still runs.
    fn resume(deps: &'a LandingDeps, record: &LandedRecord) -> Option<Progress<'a>> {
        let state = read_update_state(&deps.layout.update_state)?;
        (state.result == UpdateResult::Running && state.to == record.to).then(|| Progress { path: &deps.layout.update_state, clock: deps.clock.as_ref(), state })
    }

    async fn write(&self) {
        if let Err(err) = write_update_state(self.path, &self.state).await {
            tracing::warn!(%err, "Could not record the update's progress");
        }
    }

    async fn step(&mut self, step: UpdateStep) {
        self.state.step = step;
        self.state.step_started_at = now_iso(self.clock);
        self.write().await;
    }

    async fn finish(&mut self, step: UpdateStep, result: UpdateResult, reason: Option<String>) {
        if step != self.state.step {
            self.state.step = step;
            self.state.step_started_at = now_iso(self.clock);
        }
        self.state.result = result;
        self.state.reason = reason;
        self.write().await;
    }
}

/// Info under the `recovery` toggle, whose template reads "Recovery action: {step}".
fn recovery(text: &str) -> Alert {
    Alert { kind: DiscordKind::Recovery, vars: vec![("step".into(), Value::String(text.into()))], level: Level::Info }
}

fn encoder_alive(status: Option<&SupervisorStatus>) -> bool {
    status.and_then(|s| s.encoder.as_ref()).and_then(|e| e.alive) == Some(true)
}

/// Starts the supervisor through the launcher's guarded poll, also while the landing hold is on.
/// `start` covers a confirmation that runs before the launcher was started; a running one is left as it is.
fn relaunch(launcher: &Launcher) {
    launcher.start();
    launcher.launch_now();
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
    let result = land_once(deps).await;
    running.went_through = result.is_ok();
    let _ = tx.send(Some(result.clone()));
    result
}

async fn land_once(deps: &LandingDeps) -> Result<(), String> {
    if deps.updater.staged_version().is_none() {
        deps.updater.stage().await?;
    }
    let Some(to) = deps.updater.staged_version() else {
        return Err("No update is staged.".into());
    };
    let from = deps.updater.status().current_version;

    deps.gate.in_progress.store(true, Ordering::SeqCst);
    deps.updater.landing_started();
    // No relaunch and no outage alert until the process exits.
    deps.launcher.suspend();
    let installing = format!("Installing StreamLoop {to}. The stream restarts.");
    deps.timeline.add(&installing);
    deps.notifier.notify_event(recovery(&installing)).await;
    let mut progress = Progress::start(deps, &from, &to).await;
    tracing::info!(%from, %to, "Landing the update");

    let on_air = encoder_alive(deps.client.status().await.as_ref());
    stop_supervisor(deps).await;

    progress.step(UpdateStep::Swap).await;
    if let Err(reason) = swap_in(&deps.layout, &LandedRecord { from, to, on_air }).await {
        tracing::error!(%reason, "The update could not be landed; the old exe stays");
        progress.finish(UpdateStep::Swap, UpdateResult::Failed, Some(reason.clone())).await;
        deps.updater.landing_failed(&reason);
        let text = failure_text(&progress.state);
        deps.timeline.add(&text);
        deps.notifier.send(&text, Level::Warn, None).await;
        deps.launcher.resume();
        if !deps.launcher.is_stopped_by_user() {
            relaunch(&deps.launcher);
        }
        return Err(reason);
    }

    // A Stop from the dashboard holds; anything else was on air or is meant to be.
    progress.step(UpdateStep::Start).await;
    if !deps.launcher.is_stopped_by_user() {
        relaunch(&deps.launcher);
    }
    deps.notifier.flush().await;
    deps.timeline.flush().await;
    Ok(())
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

/// The record first, then the two renames: Windows lets a running exe be renamed, not overwritten.
/// When the second rename fails the first is undone and the record goes, so nothing is left to confirm.
async fn swap_in(layout: &Layout, record: &LandedRecord) -> Result<(), String> {
    let text = serde_json::to_string(record).map_err(|e| e.to_string())?;
    std::fs::write(&layout.landed, text).map_err(|e| format!("_update\\landed could not be written ({e})."))?;
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
    Err(reason)
}

/// Master's text for an update that a restart cut off before anything was swapped.
const RESTARTED: &str = "StreamLoop was restarted during the update.";
/// START.bat's crash-branch rollback, in the words it prints.
const STOPPED_UNCONFIRMED: &str = "The updated StreamLoop stopped before it was confirmed.";

/// For a start with no `_update\landed` to confirm: a progress still `running` belongs to a landing
/// or a check that was cut off, and is ended here so neither the dashboard nor the 409 gate waits on it.
pub async fn settle_unfinished_update(layout: &Layout, clock: &dyn Clock, running_version: &str) {
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
    progress.finish(step, result, reason).await;
}

/// Confirms when, within `within`, the supervisor answers with the new version and its encoder is
/// alive or the landing was off air; a stream stopped from the dashboard needs no answer. Otherwise
/// rolls back, and the caller exits 75. A record this exe did not land is dropped as `Confirmed`.
pub async fn confirm_or_roll_back(deps: &LandingDeps, record: LandedRecord, within: Duration) -> Confirmation {
    let running = deps.updater.status().current_version;
    if running != record.to {
        // The landing stopped before its swap, so the old exe still runs and there is nothing to put back.
        tracing::warn!(to = %record.to, %running, "Dropping the record of a landing that did not swap");
        if let Err(e) = std::fs::remove_file(&deps.layout.landed) {
            tracing::error!(error = %e, "Could not remove _update\\landed");
        }
        settle_unfinished_update(&deps.layout, deps.clock.as_ref(), &running).await;
        return Confirmation::Confirmed;
    }
    let mut progress = Progress::resume(deps, &record);
    if let Some(p) = progress.as_mut() {
        p.step(UpdateStep::Check).await;
    }
    tracing::info!(from = %record.from, to = %record.to, on_air = record.on_air, "Checking the landed update");
    let deadline = Instant::now() + within;
    loop {
        let status = deps.client.status().await;
        let version = status.as_ref().and_then(|s| s.version.as_deref());
        let alive = encoder_alive(status.as_ref());
        let came_up = version == Some(record.to.as_str()) && (alive || !record.on_air);
        let stopped = deps.launcher.is_stopped_by_user() && version.is_none_or(|v| v == record.to);
        if came_up || stopped {
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
            roll_back(deps, &record, progress, reason).await;
            return Confirmation::RolledBack;
        }
        tokio::time::sleep(CONFIRM_POLL.min(deadline - now)).await;
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
        p.finish(step, UpdateResult::Ok, None).await;
    }
}

async fn roll_back(deps: &LandingDeps, record: &LandedRecord, progress: Option<Progress<'_>>, reason: String) {
    let layout = &deps.layout;
    tracing::error!(to = %record.to, from = %record.from, %reason, "The update did not come up; rolling back");
    deps.launcher.suspend();
    if let Err(e) = swap_back(layout).await {
        // The exit 75 that follows starts this exe again, and that start checks the update once more.
        tracing::error!(error = %e, "The previous exe could not be put back");
        let text = format!("The update to {} did not come up, and the previous version could not be put back: {e}", record.to);
        deps.timeline.add(&text);
        deps.notifier.notify_critical(&text).await;
        deps.launcher.resume();
        deps.notifier.flush().await;
        deps.timeline.flush().await;
        return;
    }
    keep_rolled_back(layout, record).await;
    stop_supervisor(deps).await;
    if !deps.launcher.is_stopped_by_user() {
        relaunch(&deps.launcher);
    }
    let text = format!("The update to {} did not come up, so StreamLoop went back to {}.", record.to, record.from);
    deps.timeline.add(&text);
    deps.notifier.notify_critical(&text).await;
    if let Some(mut p) = progress {
        p.finish(UpdateStep::Check, UpdateResult::RolledBack, Some(reason)).await;
    }
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
        let written = serde_json::to_string(record).map_err(io::Error::other).and_then(|text| std::fs::write(&layout.rolled_back, text));
        if let Err(e) = written.and_then(|()| std::fs::remove_file(&layout.landed)) {
            tracing::error!(error = %e, "Could not record the rollback");
        }
    }
    // Empty once the staged exe and the record are gone.
    let _ = std::fs::remove_dir(&layout.update_dir);
}

/// The pid listening on 127.0.0.1:`port` or on every address, from the TCP table.
#[cfg(windows)]
pub fn pid_on_port(port: u16) -> Option<u32> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER};
    const AF_INET: u32 = 2;
    const NO_ERROR: u32 = 0;
    const ROW_WORDS: usize = std::mem::size_of::<MIB_TCPROW_OWNER_PID>() / 4;
    let loopback = u32::from_ne_bytes([127, 0, 0, 1]);
    let mut size = 0u32;
    // The first call reports the size; the table can grow before the next, which then reports it again.
    for _ in 0..4 {
        let mut buf = vec![0u32; (size as usize).div_ceil(4).max(1)];
        // SAFETY: `buf` holds at least `size` bytes, aligned for the table's u32 fields; the call writes
        // no more than `size` bytes and sets `size` to what it needs when that is too small.
        let rc = unsafe { GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, AF_INET, TCP_TABLE_OWNER_PID_LISTENER, 0) };
        if rc != NO_ERROR {
            continue;
        }
        let count = (buf[0] as usize).min((buf.len() - 1) / ROW_WORDS);
        // SAFETY: on success the buffer holds the entry count followed by that many rows; `count` is
        // capped to the rows that fit in `buf`.
        let rows = unsafe { std::slice::from_raw_parts(buf.as_ptr().add(1).cast::<MIB_TCPROW_OWNER_PID>(), count) };
        return rows
            .iter()
            .find(|r| u16::from_be((r.dwLocalPort & 0xffff) as u16) == port && (r.dwLocalAddr == loopback || r.dwLocalAddr == 0))
            .map(|r| r.dwOwningPid);
    }
    None
}

#[cfg(not(windows))]
pub fn pid_on_port(_port: u16) -> Option<u32> {
    None
}

/// Ends a process and every process under it, by parent pid from one snapshot.
#[cfg(windows)]
pub fn kill_tree(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    for p in tree(pid) {
        // SAFETY: the handle is checked before use and closed after it.
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, p);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn kill_tree(_pid: u32) {}

/// `root` and its descendants. A parent pid can be reused, which is fine for a live supervisor's own children.
#[cfg(windows)]
fn tree(root: u32) -> Vec<u32> {
    let pairs = processes();
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i];
        let children: Vec<u32> = pairs.iter().filter(|(c, p)| *p == parent && *c != parent && !out.contains(c)).map(|(c, _)| *c).collect();
        out.extend(children);
        i += 1;
    }
    out
}

/// (pid, parent pid) of every process, from one snapshot; empty when the snapshot fails.
#[cfg(windows)]
fn processes() -> Vec<(u32, u32)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    let mut pairs = Vec::new();
    // SAFETY: the snapshot handle is checked and closed, and the entry is zeroed with its size set as the API requires.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return pairs;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut entry) != 0;
        while more {
            pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
            more = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
    }
    pairs
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn finds_the_pid_listening_on_a_local_port() {
        // Arrange
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Act
        let pid = pid_on_port(port);
        // Assert
        assert_eq!(pid, Some(std::process::id()));
    }

    #[test]
    fn finds_no_pid_on_a_port_nothing_listens_on() {
        // Arrange
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        // Act
        let pid = pid_on_port(port);
        // Assert
        assert_eq!(pid, None);
    }

    #[test]
    fn kill_tree_ends_a_process_and_its_children() {
        // Arrange: cmd.exe running ping, which is cmd's child
        let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let ping = system.join("PING.EXE").to_string_lossy().into_owned();
        let mut cmd = streamloop_common::procs::command(system.join("cmd.exe"));
        cmd.args(["/c", ping.as_str(), "-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let mut child = cmd.spawn().unwrap();
        let mut started = Vec::new();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            let now = tree(child.id());
            if now.len() > 1 && now == started {
                break;
            }
            started = now;
        }
        assert!(started.len() > 1, "cmd started no child");
        // Act
        kill_tree(child.id());
        // Assert
        assert!(!child.wait().unwrap().success());
        let gone = (0..40).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            let all = processes();
            started.iter().all(|pid| !all.iter().any(|(p, _)| p == pid))
        });
        assert!(gone, "{started:?} still run");
    }
}
