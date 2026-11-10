use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use streamloop_common::api::{ControlCmd, EncoderStatus, ReloadRequest, SupervisorStatus};
use streamloop_common::paths::Layout;
use streamloop_control::clock::TestClock;
use streamloop_control::discord::{EmbedField, Level, Notifier};
use streamloop_control::events::Alert;
use streamloop_control::http::{FakeHttp, Http, HttpError, HttpRequest, HttpResponse, Progress};
use streamloop_control::landing::{
    confirm_or_roll_back, land, settle_unfinished_update, Confirmation, LandedRecord, LandingDeps, LandingGate, CONFIRM_WITHIN, FINISH_BEFORE_EXIT,
};
use streamloop_control::launcher::{Launcher, LauncherOptions, SpawnHandle, Spawner};
use streamloop_control::runner::{FakeRunner, RunOutput};
use streamloop_control::supervisor_client::{ControlResult, ReloadResult, SupervisorApi};
use streamloop_control::timeline::{EventLog, EventStore};
use streamloop_control::updater::{BreakPolicy, UpdateStatus, Updater, UpdaterOptions, GITHUB_RELEASES_URL};
use tokio::time::Instant;

const OLD: &str = "3.1.0";
const NEW: &str = "3.1.1";
const NEWER: &str = "3.1.2";
const OLD_EXE: &[u8] = b"the running streamloop.exe";
const NEW_EXE: &[u8] = b"the new streamloop.exe";
const NEWER_EXE: &[u8] = b"the newer streamloop.exe";
const PID: u32 = 4242;
const INSTALLING: &str = "Installing StreamLoop 3.1.1. The stream restarts.";
const INSTALLING_HELD: &str = "Installing StreamLoop 3.1.1. The stream stays stopped.";
const UPDATED: &str = "Updated to StreamLoop 3.1.1.";
const WENT_BACK: &str = "The update to 3.1.1 did not come up, so StreamLoop went back to 3.1.0.";
const RESTARTED: &str = "StreamLoop was restarted during the update.";

// Harness

fn status(version: &str, encoder_alive: bool) -> SupervisorStatus {
    SupervisorStatus {
        state: Some("playing".into()),
        version: Some(version.into()),
        encoder: Some(EncoderStatus { alive: Some(encoder_alive), uptime_s: Some(encoder_alive.then_some(100)), ..EncoderStatus::default() }),
        ..SupervisorStatus::default()
    }
}

/// The supervisor on 4417: answers what the test sets, and stops answering on `/shutdown` unless told to ignore it.
#[derive(Default)]
struct Sup {
    status: Mutex<Option<SupervisorStatus>>,
    ignores_shutdown: AtomicBool,
    shutdowns: AtomicUsize,
    /// Runs inside the shutdown call, so after the stop and before the swap.
    on_shutdown: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Sup {
    fn answer(&self, s: Option<SupervisorStatus>) {
        *self.status.lock().unwrap() = s;
    }

    fn answering(&self) -> bool {
        self.status.lock().unwrap().is_some()
    }
}

#[async_trait]
impl SupervisorApi for Sup {
    async fn status(&self) -> Option<SupervisorStatus> {
        self.status.lock().unwrap().clone()
    }

    async fn control(&self, _: ControlCmd, _: Option<u32>, _: Option<String>, _: Option<String>) -> ControlResult {
        ControlResult::default()
    }

    async fn reload(&self, _: ReloadRequest) -> ReloadResult {
        ReloadResult::default()
    }

    async fn shutdown(&self) -> bool {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        let hook = self.on_shutdown.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        if !self.ignores_shutdown.load(Ordering::SeqCst) {
            self.answer(None);
        }
        true
    }
}

/// The exe, its arguments and the exe's bytes at the launch.
type Spawn = (PathBuf, Vec<String>, Vec<u8>);

/// Records each launch; the supervisor it starts answers with that exe's version, on air.
struct Spawns {
    sup: Arc<Sup>,
    calls: Mutex<Vec<Spawn>>,
}

/// The `launch` helper, which exits 0 once the supervisor holds its port.
struct HandedOff;

const HAND_OFF: Duration = Duration::from_secs(2);

#[async_trait]
impl SpawnHandle for HandedOff {
    async fn exit_code(self: Box<Self>) -> Option<i32> {
        tokio::time::sleep(HAND_OFF).await;
        Some(0)
    }
}

impl Spawner for Spawns {
    fn spawn(&self, exe: &Path, args: &[String]) -> std::io::Result<Box<dyn SpawnHandle>> {
        let bytes = std::fs::read(exe)?;
        let version = match bytes.as_slice() {
            b if b == NEW_EXE => NEW,
            b if b == NEWER_EXE => NEWER,
            _ => OLD,
        };
        self.sup.answer(Some(status(version, true)));
        self.calls.lock().unwrap().push((exe.to_path_buf(), args.to_vec(), bytes));
        Ok(Box::new(HandedOff))
    }
}

impl Spawns {
    fn all(&self) -> Vec<Spawn> {
        self.calls.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct Posts(Mutex<Vec<String>>);

impl Posts {
    fn push(&self, s: String) {
        self.0.lock().unwrap().push(s);
    }

    fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    fn criticals(&self) -> Vec<String> {
        self.all().into_iter().filter(|p| p.starts_with("critical: ")).collect()
    }
}

#[async_trait]
impl Notifier for Posts {
    async fn notify_critical(&self, message: &str) {
        self.push(format!("critical: {message}"));
    }

    async fn notify_event(&self, alert: Alert) {
        let vars: Vec<String> = alert.vars.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default())).collect();
        self.push(format!("{}/{}: {}", alert.kind.as_str(), alert.level.as_str(), vars.join(",")));
    }

    async fn notify_twitch_mismatch(&self, _: &str) {
        unreachable!("the landing never reports a Twitch mismatch")
    }

    async fn notify_twitch_restart(&self, _: &str) {
        unreachable!("the landing never reports a Twitch restart")
    }

    async fn send(&self, content: &str, level: Level, _: Option<Vec<EmbedField>>) {
        self.push(format!("send/{}: {content}", level.as_str()));
    }

    async fn flush(&self) {
        self.push("flush".into());
    }
}

/// FakeHttp with a delay on requests, and on downloads after they report half their bytes.
#[derive(Default)]
struct SlowHttp {
    inner: FakeHttp,
    send_delay: Mutex<Duration>,
    download_delay: Mutex<Duration>,
}

#[async_trait]
impl Http for SlowHttp {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        let delay = *self.send_delay.lock().unwrap();
        tokio::time::sleep(delay).await;
        self.inner.send(req).await
    }

    async fn download(&self, url: &str, dest: &Path) -> Result<u64, HttpError> {
        self.download_with_progress(url, dest, &|_, _| {}).await
    }

    async fn download_with_progress(&self, url: &str, dest: &Path, progress: &Progress<'_>) -> Result<u64, HttpError> {
        let delay = *self.download_delay.lock().unwrap();
        progress(1, Some(2));
        tokio::time::sleep(delay).await;
        self.inner.download_with_progress(url, dest, progress).await
    }
}

type Kills = Arc<Mutex<Vec<(u32, Instant)>>>;

struct Rig {
    deps: LandingDeps,
    sup: Arc<Sup>,
    spawns: Arc<Spawns>,
    kills: Kills,
    posts: Arc<Posts>,
    http: Arc<SlowHttp>,
    runner: Arc<FakeRunner>,
    clock: Arc<TestClock>,
    layout: Layout,
    _dir: tempfile::TempDir,
}

/// An install in a temp root with a space and an Ö, running `version` from `exe_bytes`.
fn rig(version: &str, exe_bytes: &[u8], runner: Arc<FakeRunner>, hold_for_start: bool) -> Rig {
    let dir = tempfile::Builder::new().prefix("sl3 Ö land").tempdir().unwrap();
    let layout = Layout::new(dir.path());
    std::fs::create_dir_all(&layout.app_dir).unwrap();
    std::fs::write(&layout.exe, exe_bytes).unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_792_929_600)));
    let http = Arc::new(SlowHttp::default());
    offer(&http, NEW, NEW_EXE);
    let sup = Arc::new(Sup::default());
    let spawns = Arc::new(Spawns { sup: sup.clone(), calls: Mutex::new(Vec::new()) });
    let posts = Arc::new(Posts::default());
    let kills: Kills = Arc::new(Mutex::new(Vec::new()));
    let updater = Updater::new(UpdaterOptions {
        current: version.into(),
        dev_mode: false,
        http: http.clone(),
        runner: runner.clone(),
        clock: clock.clone(),
        layout: layout.clone(),
        releases_url: GITHUB_RELEASES_URL.into(),
        test_releases: None,
        bundled_node: None,
        policy: BreakPolicy::default(),
    });
    let launcher = Launcher::new(LauncherOptions {
        layout: layout.clone(),
        client: sup.clone(),
        spawner: spawns.clone(),
        clock: clock.clone(),
        debug_faults: false,
        guard: None,
        own_version: Some(version.into()),
        on_event: Box::new(|_, _| {}),
        hold_for_start,
    });
    let (port_sup, kill_sup, kill_log) = (sup.clone(), sup.clone(), kills.clone());
    let deps = LandingDeps {
        layout: layout.clone(),
        updater,
        launcher,
        client: sup.clone(),
        notifier: posts.clone(),
        timeline: Arc::new(EventLog::new(EventStore::new(&layout.app_logs, clock.clone()))),
        clock: clock.clone(),
        runner: runner.clone(),
        kill_tree: Arc::new(move |pid| {
            kill_log.lock().unwrap().push((pid, Instant::now()));
            kill_sup.answer(None);
        }),
        pid_on_port: Arc::new(move |port| (port == 4417 && port_sup.answering()).then_some(PID)),
        first_run: Arc::new(|| false),
        gate: LandingGate::default(),
    };
    Rig { deps, sup, spawns, kills, posts, http, runner, clock, layout, _dir: dir }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

const GH: &str = "https://github.com/Sponsorn/streamloop/releases/download";

/// GitHub offers `version`, whose exe is `bytes`. A later offer replaces an earlier one.
fn offer(http: &SlowHttp, version: &str, bytes: &[u8]) {
    let names = [format!("streamloop-v{version}.zip"), format!("streamloop-v{version}.exe"), format!("streamloop-v{version}.exe.sha256")];
    let assets: Vec<Value> = names.iter().map(|n| json!({ "name": n, "browser_download_url": format!("{GH}/v{version}/{n}") })).collect();
    let list = json!([{ "tag_name": format!("v{version}"), "draft": false, "prerelease": true, "assets": assets }]).to_string();
    http.inner.respond(GITHUB_RELEASES_URL, 200, &list);
    http.inner.respond(&format!("{GH}/v{version}/streamloop-v{version}.exe"), 200, std::str::from_utf8(bytes).unwrap());
    http.inner.respond(&format!("{GH}/v{version}/streamloop-v{version}.exe.sha256"), 200, &format!("{}  streamloop-v{version}.exe\n", sha256_hex(bytes)));
}

/// The old control process: OLD on air, the launcher watching, and NEW staged in `_update\`.
async fn landing_rig() -> Rig {
    landing_rig_with(false, true).await
}

/// `on_air` false: nothing has run since the start, so a start hold is still on.
async fn landing_rig_with(hold_for_start: bool, on_air: bool) -> Rig {
    let runner = Arc::new(FakeRunner::new());
    runner.push(Ok(RunOutput::ok(&format!("{NEW}\r\n"))));
    let r = rig(OLD, OLD_EXE, runner, hold_for_start);
    r.deps.updater.check().await;
    r.deps.updater.wait_for_stage().await;
    assert_eq!(r.deps.updater.staged_version().as_deref(), Some(NEW), "the exe was staged");
    if on_air {
        r.sup.answer(Some(status(OLD, true)));
    }
    r.deps.launcher.start();
    // The launcher's first poll sees the stream.
    tokio::time::sleep(Duration::from_millis(100)).await;
    r
}

/// The old control process with NEW offered but not staged: its background stage got a wrong `--version`.
async fn unstaged_rig(second_version_answer: &str) -> Rig {
    let runner = Arc::new(FakeRunner::new());
    runner.push(Ok(RunOutput::ok("3.0.9\r\n")));
    runner.push(Ok(RunOutput::ok(&format!("{second_version_answer}\r\n"))));
    let r = rig(OLD, OLD_EXE, runner, false);
    r.deps.updater.check().await;
    r.deps.updater.wait_for_stage().await;
    assert_eq!(r.deps.updater.staged_version(), None, "the first stage failed");
    r.sup.answer(Some(status(OLD, true)));
    r.deps.launcher.start();
    tokio::time::sleep(Duration::from_millis(100)).await;
    r
}

const STATE_AT_START: &str = r#"{
  "from": "3.1.0",
  "to": "3.1.1",
  "parts": [],
  "folders": [],
  "step": "start",
  "result": "running",
  "reason": null,
  "startedAt": "2026-10-25T12:00:00.000Z",
  "stepStartedAt": "2026-10-25T12:00:21.000Z",
  "baseline": null
}"#;

fn record(on_air: bool, held: bool) -> LandedRecord {
    LandedRecord { from: OLD.into(), to: NEW.into(), on_air, held, rollback_failed: false }
}

/// The new control process right after a landing: NEW in place, OLD kept as `.old`, the record in `_update\`.
fn confirming_rig(record: LandedRecord, hold_for_start: bool) -> Rig {
    let r = rig(NEW, NEW_EXE, Arc::new(FakeRunner::new()), hold_for_start);
    std::fs::write(&r.layout.exe_old, OLD_EXE).unwrap();
    std::fs::create_dir_all(&r.layout.update_dir).unwrap();
    std::fs::write(&r.layout.landed, serde_json::to_string(&record).unwrap()).unwrap();
    std::fs::create_dir_all(&r.layout.data).unwrap();
    std::fs::write(&r.layout.update_state, STATE_AT_START).unwrap();
    r
}

fn check_failing(line: &str) -> RunOutput {
    RunOutput { code: Some(1), stdout: Vec::new(), stderr: format!("streamloop-supervisor check: {line}\r\n").into_bytes() }
}

impl Rig {
    /// Gives the launcher time to act on its own, after a landing that did not wait for a launch.
    async fn settle(&self) {
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    fn read(&self, path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap_or_default()
    }

    fn timeline(&self) -> Vec<String> {
        self.deps.timeline.entries().into_iter().map(|e| e.message).collect()
    }

    fn update_state(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.layout.update_state).unwrap()).unwrap()
    }

    fn rolled_back(&self) -> Option<LandedRecord> {
        LandedRecord::read(&self.layout.rolled_back)
    }

    fn fail_the_second_rename(&self) {
        let staged = self.layout.staged_exe.clone();
        *self.sup.on_shutdown.lock().unwrap() = Some(Box::new(move || std::fs::remove_file(&staged).unwrap()));
    }
}

// land

#[tokio::test(start_paused = true)]
async fn swaps_the_exe_and_keeps_the_old_one() {
    // Arrange
    let r = landing_rig().await;
    // Act
    let result = land(&r.deps).await;
    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(r.read(&r.layout.exe), NEW_EXE);
    assert_eq!(r.read(&r.layout.exe_old), OLD_EXE);
    assert!(!r.layout.staged_exe.exists());
    assert_eq!(LandedRecord::read(&r.layout.landed), Some(record(true, false)));
}

#[tokio::test(start_paused = true)]
async fn launches_from_the_new_exe_after_the_swap() {
    // Arrange
    let r = landing_rig().await;
    let started = Instant::now();
    // Act
    land(&r.deps).await.unwrap();
    // Assert: the launch helper has exited before the caller exits 75
    assert!(started.elapsed() >= HAND_OFF, "returned before the launch ended");
    let spawns = r.spawns.all();
    assert_eq!(spawns.len(), 1);
    let (exe, args, bytes) = &spawns[0];
    assert_eq!(exe, &r.layout.exe);
    assert_eq!(bytes.as_slice(), NEW_EXE);
    assert_eq!(args, &["launch".to_string(), "--root".to_string(), r.layout.root.to_string_lossy().into_owned()]);
}

#[tokio::test(start_paused = true)]
async fn does_not_launch_when_stopped_from_the_dashboard() {
    // Arrange: Stop, and the supervisor has drained
    let r = landing_rig().await;
    r.deps.launcher.mark_stopped_by_user();
    r.sup.answer(None);
    tokio::time::sleep(Duration::from_secs(3)).await;
    // Act
    let result = land(&r.deps).await;
    tokio::time::sleep(Duration::from_secs(60)).await;
    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(r.read(&r.layout.exe), NEW_EXE);
    assert!(r.spawns.all().is_empty());
    assert!(r.layout.stopped_marker.exists());
    let landed = LandedRecord::read(&r.layout.landed).unwrap();
    assert_eq!((landed.on_air, landed.held), (false, true));
    assert!(r.timeline().contains(&INSTALLING_HELD.to_string()), "{:?}", r.timeline());
    assert!(r.posts.all().contains(&format!("recovery/info: step={INSTALLING_HELD}")), "{:?}", r.posts.all());
}

#[tokio::test(start_paused = true)]
async fn a_landing_while_waiting_for_start_does_not_relaunch_and_stays_held() {
    // Arrange: autoStartStream off and Start not pressed, so nothing runs
    let r = landing_rig_with(true, false).await;
    // Act
    let result = land(&r.deps).await;
    tokio::time::sleep(Duration::from_secs(60)).await;
    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(r.read(&r.layout.exe), NEW_EXE);
    assert!(r.spawns.all().is_empty());
    assert!(r.layout.waiting_for_start.exists());
    assert!(r.deps.launcher.is_waiting_for_start());
    assert_eq!(LandedRecord::read(&r.layout.landed).map(|l| l.held), Some(true));
    assert!(r.timeline().contains(&INSTALLING_HELD.to_string()), "{:?}", r.timeline());
    assert!(!r.timeline().contains(&INSTALLING.to_string()));
}

#[tokio::test(start_paused = true)]
async fn an_on_air_landing_relaunches_even_with_auto_start_off() {
    // Arrange: held at start, and a supervisor that came up since ended the hold
    let r = landing_rig_with(true, true).await;
    assert!(!r.deps.launcher.is_waiting_for_start(), "the running supervisor ended the start hold");
    // Act
    land(&r.deps).await.unwrap();
    // Assert
    let spawns = r.spawns.all();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].2.as_slice(), NEW_EXE);
    assert_eq!(LandedRecord::read(&r.layout.landed).map(|l| l.held), Some(false));
}

#[tokio::test(start_paused = true)]
async fn on_air_comes_from_the_launchers_last_status_when_the_probe_fails() {
    // Arrange: the launcher saw the stream on air; the landing's own probe gets no answer
    let r = landing_rig().await;
    r.sup.answer(None);
    // Act
    land(&r.deps).await.unwrap();
    // Assert
    assert_eq!(LandedRecord::read(&r.layout.landed).map(|l| l.on_air), Some(true));
}

#[tokio::test(start_paused = true)]
async fn a_failed_second_rename_restores_the_old_exe() {
    // Arrange: the staged exe is gone by the time it is moved into place
    let r = landing_rig().await;
    r.fail_the_second_rename();
    // Act
    let result = land(&r.deps).await;
    r.settle().await;
    // Assert
    assert!(result.is_err());
    assert_eq!(r.read(&r.layout.exe), OLD_EXE);
    assert!(!r.layout.exe_old.exists());
    assert!(!r.layout.landed.exists());
    assert_eq!(r.deps.updater.status().status, UpdateStatus::Error);
    let spawns = r.spawns.all();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].2.as_slice(), OLD_EXE);
}

#[cfg(windows)]
#[tokio::test(start_paused = true)]
async fn a_locked_staged_exe_leaves_the_old_one_in_place() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    // Arrange: antivirus holds the staged exe open without share-delete for the whole landing
    let r = landing_rig().await;
    let lock = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&r.layout.staged_exe).unwrap();
    // Act
    let result = land(&r.deps).await;
    r.settle().await;
    drop(lock);
    // Assert
    let reason = result.unwrap_err();
    assert!(reason.starts_with("The new streamloop.exe could not be moved into place"), "{reason}");
    assert_eq!(r.read(&r.layout.exe), OLD_EXE);
    assert_eq!(r.read(&r.layout.staged_exe), NEW_EXE);
    assert!(!r.layout.exe_old.exists());
    assert!(!r.layout.landed.exists());
    assert_eq!(r.deps.updater.status().status, UpdateStatus::Error);
    let spawns = r.spawns.all();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].2.as_slice(), OLD_EXE);
}

#[tokio::test(start_paused = true)]
async fn ends_a_supervisor_that_ignores_shutdown_after_20_s() {
    // Arrange
    let r = landing_rig().await;
    r.sup.ignores_shutdown.store(true, Ordering::SeqCst);
    let started = Instant::now();
    // Act
    let result = land(&r.deps).await;
    // Assert
    let kills = r.kills.lock().unwrap().clone();
    assert_eq!(kills.len(), 1);
    assert_eq!(kills[0].0, PID);
    let at = kills[0].1.duration_since(started);
    assert!(at >= Duration::from_secs(20) && at < Duration::from_secs(21), "killed at {at:?}");
    assert_eq!(result, Ok(()));
    assert_eq!(r.read(&r.layout.exe), NEW_EXE);
}

#[tokio::test(start_paused = true)]
async fn two_landings_at_once_swap_once() {
    // Arrange
    let r = landing_rig().await;
    // Act
    let (first, second) = tokio::join!(land(&r.deps), land(&r.deps));
    // Assert
    assert_eq!((first, second), (Ok(()), Ok(())));
    assert_eq!(r.sup.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(r.read(&r.layout.exe), NEW_EXE);
    assert_eq!(r.read(&r.layout.exe_old), OLD_EXE);
    assert_eq!(r.spawns.all().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_landing_after_one_went_through_lands_nothing_more() {
    // Arrange
    let r = landing_rig().await;
    land(&r.deps).await.unwrap();
    // Act
    let again = land(&r.deps).await;
    // Assert
    assert_eq!(again, Ok(()));
    assert_eq!(r.sup.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(r.read(&r.layout.exe_old), OLD_EXE);
    assert_eq!(r.spawns.all().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_landing_waits_for_a_check_in_flight_and_lands_what_it_staged() {
    // Arrange: NEW is staged, and a slow check is finding NEWER when the landing starts
    let r = landing_rig().await;
    offer(&r.http, NEWER, NEWER_EXE);
    r.runner.push(Ok(RunOutput::ok(&format!("{NEWER}\r\n"))));
    r.clock.advance(Duration::from_secs(61));
    *r.http.send_delay.lock().unwrap() = Duration::from_secs(5);
    let updater = r.deps.updater.clone();
    let check = tokio::spawn(async move { updater.check().await });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    *r.http.send_delay.lock().unwrap() = Duration::ZERO;
    let started = Instant::now();
    // Act
    let result = land(&r.deps).await;
    check.await.unwrap();
    // Assert
    assert_eq!(result, Ok(()));
    assert!(started.elapsed() >= Duration::from_secs(5), "the landing did not wait for the check");
    assert_eq!(r.read(&r.layout.exe), NEWER_EXE);
    assert_eq!(LandedRecord::read(&r.layout.landed).map(|l| l.to), Some(NEWER.to_string()));
}

#[tokio::test(start_paused = true)]
async fn installing_goes_to_the_timeline_and_to_discord_under_recovery_before_the_exit() {
    // Arrange
    let r = landing_rig().await;
    // Act
    land(&r.deps).await.unwrap();
    // Assert
    assert!(r.timeline().contains(&INSTALLING.to_string()));
    let posts = r.posts.all();
    let info = posts.iter().position(|p| p == &format!("recovery/info: step={INSTALLING}")).expect("the info was sent");
    assert!(posts[info..].contains(&"flush".to_string()), "flushed after the info: {posts:?}");
}

#[tokio::test(start_paused = true)]
async fn writes_the_update_progress_in_masters_shape() {
    // Arrange
    let r = landing_rig().await;
    // Act
    land(&r.deps).await.unwrap();
    // Assert
    let text = std::fs::read_to_string(&r.layout.update_state).unwrap();
    let keys = ["\"from\"", "\"to\"", "\"parts\"", "\"folders\"", "\"step\"", "\"result\"", "\"reason\"", "\"startedAt\"", "\"stepStartedAt\"", "\"baseline\""];
    let at: Vec<usize> = keys.iter().map(|k| text.find(k).unwrap_or_else(|| panic!("{k} missing in {text}"))).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "keys out of master's order: {text}");
    let s = r.update_state();
    assert_eq!((s["from"].as_str(), s["to"].as_str()), (Some(OLD), Some(NEW)));
    assert_eq!((s["parts"].clone(), s["folders"].clone(), s["baseline"].clone()), (json!([]), json!([]), Value::Null));
    assert_eq!((s["step"].as_str(), s["result"].as_str()), (Some("start"), Some("running")));
}

#[tokio::test(start_paused = true)]
async fn update_now_records_the_download_step_and_its_percentage() {
    // Arrange: nothing staged, and the download takes 3 s after its first half
    let r = unstaged_rig(NEW).await;
    *r.http.download_delay.lock().unwrap() = Duration::from_secs(3);
    // Act
    let (result, during) = tokio::join!(land(&r.deps), async {
        tokio::time::sleep(Duration::from_secs(2)).await;
        r.update_state()
    });
    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!((during["step"].as_str(), during["result"].as_str()), (Some("download"), Some("running")));
    assert_eq!(during["downloadPercent"].as_u64(), Some(50));
    assert_eq!(r.read(&r.layout.exe), NEW_EXE);
}

#[tokio::test(start_paused = true)]
async fn a_failed_stage_on_update_now_is_recorded_at_the_download_step() {
    // Arrange: the second stage gets a wrong --version too
    let r = unstaged_rig("3.0.9").await;
    // Act
    let reason = land(&r.deps).await.unwrap_err();
    // Assert
    let s = r.update_state();
    assert_eq!((s["step"].as_str(), s["result"].as_str(), s["reason"].as_str()), (Some("download"), Some("failed"), Some(reason.as_str())));
    assert_eq!(r.sup.shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(r.read(&r.layout.exe), OLD_EXE);
    assert!(!r.deps.landing_in_progress());
}

#[tokio::test(start_paused = true)]
async fn a_landing_after_a_failed_stage_stages_the_exe_first() {
    // Arrange
    let r = unstaged_rig(NEW).await;
    // Act
    let result = land(&r.deps).await;
    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(r.read(&r.layout.exe), NEW_EXE);
}

#[tokio::test(start_paused = true)]
async fn a_failed_swap_is_recorded_for_the_dashboard_and_discord() {
    // Arrange
    let r = landing_rig().await;
    r.fail_the_second_rename();
    // Act
    let reason = land(&r.deps).await.unwrap_err();
    // Assert
    let s = r.update_state();
    assert_eq!((s["step"].as_str(), s["result"].as_str(), s["reason"].as_str()), (Some("swap"), Some("failed"), Some(reason.as_str())));
    assert_eq!(r.deps.updater.status().error.as_deref(), Some(reason.as_str()));
    let failure = format!("The update to 3.1.1 failed at replacing files: {}. Still on 3.1.0.", reason.trim_end_matches('.'));
    assert!(r.timeline().contains(&failure), "{:?}", r.timeline());
    assert!(r.posts.all().contains(&format!("send/warn: {failure}")));
}

#[tokio::test(start_paused = true)]
async fn a_landing_is_in_progress_from_its_first_step_until_it_fails() {
    // Arrange: the supervisor takes its 20 s, and the swap then fails
    let r = landing_rig().await;
    let before = r.deps.landing_in_progress();
    r.sup.ignores_shutdown.store(true, Ordering::SeqCst);
    r.fail_the_second_rename();
    // Act
    let (result, during) = tokio::join!(land(&r.deps), async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        r.deps.landing_in_progress()
    });
    // Assert
    assert!(result.is_err());
    assert_eq!((before, during, r.deps.landing_in_progress()), (false, true, false));
}

#[tokio::test(start_paused = true)]
async fn a_landing_that_went_through_stays_in_progress_until_the_exit() {
    // Arrange
    let r = landing_rig().await;
    // Act
    land(&r.deps).await.unwrap();
    // Assert
    assert!(r.deps.landing_in_progress());
    assert_eq!(r.deps.updater.status().status, UpdateStatus::Ready);
}

#[tokio::test(start_paused = true)]
async fn a_dropped_landing_resumes_the_launcher_and_ends_its_progress() {
    // Arrange: the landing is waiting for a supervisor that ignores /shutdown
    let r = landing_rig().await;
    r.sup.ignores_shutdown.store(true, Ordering::SeqCst);
    // Act
    let cut = tokio::time::timeout(Duration::from_secs(5), land(&r.deps)).await;
    r.sup.answer(None);
    tokio::time::sleep(Duration::from_secs(10)).await;
    // Assert
    assert!(cut.is_err(), "the landing was cut off");
    assert!(!r.deps.landing_in_progress());
    assert_ne!(r.deps.updater.status().status, UpdateStatus::Ready);
    let s = r.update_state();
    assert_eq!((s["result"].as_str(), s["reason"].as_str()), (Some("failed"), Some(RESTARTED)));
    assert_eq!(r.read(&r.layout.exe), OLD_EXE);
    assert!(!r.layout.landed.exists());
    assert_eq!(r.spawns.all().first().map(|s| s.2.clone()), Some(OLD_EXE.to_vec()), "the resumed launcher relaunched the old supervisor");
}

// confirm_or_roll_back

#[tokio::test(start_paused = true)]
async fn confirms_when_the_new_version_streams() {
    // Arrange: the new supervisor comes up on air 30 s into the window
    let r = confirming_rig(record(true, false), false);
    let sup = r.sup.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        sup.answer(Some(status(NEW, true)));
    });
    let started = Instant::now();
    // Act
    let outcome = confirm_or_roll_back(&r.deps, record(true, false), CONFIRM_WITHIN).await;
    // Assert
    assert_eq!(outcome, Confirmation::Confirmed);
    assert!(started.elapsed() < Duration::from_secs(180));
    assert!(!r.layout.landed.exists());
    assert!(!r.layout.exe_old.exists());
    assert!(!r.layout.update_dir.exists());
    assert_eq!(r.read(&r.layout.exe), NEW_EXE);
    assert!(r.timeline().contains(&UPDATED.to_string()));
    let s = r.update_state();
    assert_eq!((s["step"].as_str(), s["result"].as_str()), (Some("done"), Some("ok")));
}

#[tokio::test(start_paused = true)]
async fn confirms_an_off_air_landing_without_an_encoder() {
    // Arrange
    let r = confirming_rig(record(false, false), false);
    r.sup.answer(Some(status(NEW, false)));
    // Act
    let outcome = confirm_or_roll_back(&r.deps, record(false, false), CONFIRM_WITHIN).await;
    // Assert
    assert_eq!(outcome, Confirmation::Confirmed);
    assert!(!r.layout.landed.exists());
    assert!(!r.layout.exe_old.exists());
    assert!(r.timeline().contains(&UPDATED.to_string()));
    let s = r.update_state();
    assert_eq!((s["step"].as_str(), s["result"].as_str()), (Some("start"), Some("ok")));
}

#[tokio::test(start_paused = true)]
async fn confirms_a_held_landing_when_the_new_exe_passes_its_check() {
    // Arrange: stopped from the dashboard, so nothing was launched and nothing answers
    let r = confirming_rig(record(false, true), false);
    r.deps.launcher.mark_stopped_by_user();
    r.runner.push(Ok(RunOutput::ok("ok\r\n")));
    // Act
    let outcome = confirm_or_roll_back(&r.deps, record(false, true), CONFIRM_WITHIN).await;
    // Assert
    assert_eq!(outcome, Confirmation::Confirmed);
    let calls = r.runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].program, r.layout.exe.to_string_lossy());
    assert_eq!(calls[0].args, ["check".to_string(), "--root".to_string(), r.layout.root.to_string_lossy().into_owned()]);
    assert_eq!(r.read(&r.layout.exe), NEW_EXE);
    assert!(!r.layout.exe_old.exists());
    assert!(r.spawns.all().is_empty());
    assert!(r.layout.stopped_marker.exists());
    let s = r.update_state();
    assert_eq!((s["step"].as_str(), s["result"].as_str()), (Some("start"), Some("ok")));
}

#[tokio::test(start_paused = true)]
async fn rolls_back_a_held_landing_whose_new_exe_fails_its_check() {
    // Arrange: waiting for Start, so nothing was launched and nothing answers
    let r = confirming_rig(record(false, true), true);
    r.runner.push(Ok(check_failing("ffmpeg\\ffmpeg.exe is missing")));
    // Act
    let outcome = confirm_or_roll_back(&r.deps, record(false, true), CONFIRM_WITHIN).await;
    // Assert
    assert_eq!(outcome, Confirmation::RolledBack);
    assert_eq!(r.read(&r.layout.exe), OLD_EXE);
    assert_eq!(r.read(&r.layout.exe_bad), NEW_EXE);
    assert_eq!(r.rolled_back(), Some(record(false, true)));
    assert!(r.posts.all().contains(&format!("critical: {WENT_BACK}")));
    assert!(r.spawns.all().is_empty(), "a held stream stays held");
    assert!(r.layout.waiting_for_start.exists());
    let s = r.update_state();
    assert_eq!((s["step"].as_str(), s["result"].as_str()), (Some("check"), Some("rolled-back")));
    assert_eq!(s["reason"].as_str(), Some("The new supervisor failed its check: ffmpeg\\ffmpeg.exe is missing"));
}

#[tokio::test(start_paused = true)]
async fn rolls_back_when_the_new_supervisor_never_answers() {
    // Arrange
    let r = confirming_rig(record(true, false), false);
    let started = Instant::now();
    // Act
    let outcome = confirm_or_roll_back(&r.deps, record(true, false), CONFIRM_WITHIN).await;
    // Assert: the old supervisor's launch has ended before the caller exits 75
    assert_eq!(outcome, Confirmation::RolledBack);
    assert!(started.elapsed() >= Duration::from_secs(180) + HAND_OFF, "returned before the launch ended");
    assert_eq!(r.read(&r.layout.exe_bad), NEW_EXE);
    assert_eq!(r.read(&r.layout.exe), OLD_EXE);
    assert!(!r.layout.exe_old.exists());
    assert!(!r.layout.landed.exists());
    assert_eq!(r.rolled_back(), Some(record(true, false)));
    assert_eq!(r.rolled_back().map(|l| l.to), Some(NEW.to_string()));
    assert!(r.posts.all().contains(&format!("critical: {WENT_BACK}")));
    assert!(r.timeline().contains(&WENT_BACK.to_string()));
    let spawns = r.spawns.all();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].2.as_slice(), OLD_EXE);
    let s = r.update_state();
    assert_eq!((s["step"].as_str(), s["result"].as_str()), (Some("check"), Some("rolled-back")));
}

#[tokio::test(start_paused = true)]
async fn rolls_back_when_the_encoder_never_comes_up_after_an_on_air_landing() {
    // Arrange: the new supervisor answers, but its encoder never runs
    let r = confirming_rig(record(true, false), false);
    r.sup.answer(Some(status(NEW, false)));
    // Act
    let outcome = confirm_or_roll_back(&r.deps, record(true, false), CONFIRM_WITHIN).await;
    // Assert
    assert_eq!(outcome, Confirmation::RolledBack);
    assert_eq!(r.read(&r.layout.exe), OLD_EXE);
    assert_eq!(r.read(&r.layout.exe_bad), NEW_EXE);
    assert_eq!(r.rolled_back(), Some(record(true, false)));
    assert!(r.posts.all().contains(&format!("critical: {WENT_BACK}")));
    assert_eq!(r.sup.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(r.spawns.all().last().map(|s| s.2.clone()), Some(OLD_EXE.to_vec()));
}

#[tokio::test(start_paused = true)]
async fn a_failed_swap_back_is_reported_once_and_keeps_the_new_exe() {
    // Arrange: streamloop.exe.old is gone, so the old exe cannot be put back
    let r = confirming_rig(record(true, false), false);
    std::fs::remove_file(&r.layout.exe_old).unwrap();
    // Act: the first start tries to roll back, a later start finds the same record
    let first = confirm_or_roll_back(&r.deps, record(true, false), CONFIRM_WITHIN).await;
    let again = LandedRecord::read(&r.layout.landed).expect("the record stays");
    let started = Instant::now();
    let second = confirm_or_roll_back(&r.deps, again.clone(), CONFIRM_WITHIN).await;
    // Assert
    assert_eq!((first, second), (Confirmation::Confirmed, Confirmation::Confirmed));
    assert!(again.rollback_failed);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(r.read(&r.layout.exe), NEW_EXE);
    assert!(!r.layout.exe_bad.exists());
    let criticals = r.posts.criticals();
    assert_eq!(criticals.len(), 1, "{criticals:?}");
    assert!(criticals[0].starts_with("critical: The update to 3.1.1 did not come up, and the previous version could not be put back"), "{criticals:?}");
    assert_eq!(r.sup.shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(r.update_state()["result"].as_str(), Some("failed"));
}

#[tokio::test(start_paused = true)]
async fn drops_a_record_this_exe_did_not_land() {
    // Arrange: the old control process died between writing the record and the swap, so OLD runs again
    let r = rig(OLD, OLD_EXE, Arc::new(FakeRunner::new()), false);
    std::fs::create_dir_all(&r.layout.update_dir).unwrap();
    std::fs::write(&r.layout.staged_exe, NEW_EXE).unwrap();
    std::fs::write(&r.layout.landed, serde_json::to_string(&record(true, false)).unwrap()).unwrap();
    std::fs::create_dir_all(&r.layout.data).unwrap();
    std::fs::write(&r.layout.update_state, STATE_AT_START.replace("\"start\"", "\"swap\"")).unwrap();
    let started = Instant::now();
    // Act
    let outcome = confirm_or_roll_back(&r.deps, record(true, false), CONFIRM_WITHIN).await;
    // Assert
    assert_eq!(outcome, Confirmation::Confirmed);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!r.layout.landed.exists());
    assert_eq!(r.read(&r.layout.exe), OLD_EXE);
    assert!(!r.layout.exe_bad.exists());
    assert_eq!(r.sup.shutdowns.load(Ordering::SeqCst), 0);
    assert!(!r.timeline().contains(&UPDATED.to_string()));
    let s = r.update_state();
    assert_eq!((s["result"].as_str(), s["reason"].as_str()), (Some("failed"), Some(RESTARTED)));
    let failure = format!("The update to 3.1.1 failed at replacing files: {}. Still on 3.1.0.", RESTARTED.trim_end_matches('.'));
    assert!(r.timeline().contains(&failure), "{:?}", r.timeline());
    assert_eq!(r.posts.criticals(), [format!("critical: {failure}")]);
}

// settle_unfinished_update

#[tokio::test(start_paused = true)]
async fn settle_marks_a_check_that_start_bat_cut_off_as_rolled_back() {
    // Arrange: START.bat put OLD back and moved the record, leaving the progress at the check
    let r = confirming_rig(record(true, false), false);
    std::fs::remove_file(&r.layout.landed).unwrap();
    std::fs::write(&r.layout.update_state, STATE_AT_START.replace("\"start\"", "\"check\"")).unwrap();
    // Act
    let text = settle_unfinished_update(&r.layout, r.deps.clock.as_ref(), OLD);
    // Assert
    let s = r.update_state();
    assert_eq!((s["step"].as_str(), s["result"].as_str()), (Some("check"), Some("rolled-back")));
    assert_eq!(s["reason"].as_str(), Some("The updated StreamLoop stopped before it was confirmed."));
    assert_eq!(
        text.as_deref(),
        Some("The update to 3.1.1 failed at checking the new version: The updated StreamLoop stopped before it was confirmed. Still on 3.1.0.")
    );
}

#[tokio::test(start_paused = true)]
async fn settle_marks_a_landing_cut_off_before_the_swap_as_failed() {
    // Arrange
    let r = confirming_rig(record(true, false), false);
    std::fs::remove_file(&r.layout.landed).unwrap();
    std::fs::write(&r.layout.update_state, STATE_AT_START.replace("\"start\"", "\"stop\"")).unwrap();
    // Act
    let text = settle_unfinished_update(&r.layout, r.deps.clock.as_ref(), OLD);
    // Assert
    let s = r.update_state();
    assert_eq!((s["step"].as_str(), s["result"].as_str()), (Some("stop"), Some("failed")));
    assert_eq!(s["reason"].as_str(), Some(RESTARTED));
    assert_eq!(text.as_deref(), Some("The update to 3.1.1 failed at stopping the stream: StreamLoop was restarted during the update. Still on 3.1.0."));
}

#[tokio::test(start_paused = true)]
async fn settle_leaves_an_update_that_has_its_record_or_has_ended_alone() {
    // Arrange: one still has its record to confirm; the other ended long ago
    let with_record = confirming_rig(record(true, false), false);
    let ended = confirming_rig(record(true, false), false);
    std::fs::remove_file(&ended.layout.landed).unwrap();
    let done = STATE_AT_START.replace("\"running\"", "\"ok\"");
    std::fs::write(&ended.layout.update_state, &done).unwrap();
    // Act
    let texts = (
        settle_unfinished_update(&with_record.layout, with_record.deps.clock.as_ref(), OLD),
        settle_unfinished_update(&ended.layout, ended.deps.clock.as_ref(), OLD),
    );
    // Assert
    assert_eq!(std::fs::read_to_string(&with_record.layout.update_state).unwrap(), STATE_AT_START);
    assert_eq!(std::fs::read_to_string(&ended.layout.update_state).unwrap(), done);
    assert_eq!(texts, (None, None));
}

#[tokio::test(start_paused = true)]
async fn settle_reports_nothing_for_an_update_whose_version_runs() {
    // Arrange: the record went, but the new version is what runs
    let r = confirming_rig(record(true, false), false);
    std::fs::remove_file(&r.layout.landed).unwrap();
    // Act
    let text = settle_unfinished_update(&r.layout, r.deps.clock.as_ref(), NEW);
    // Assert
    assert_eq!(text, None);
    assert_eq!(r.update_state()["result"].as_str(), Some("ok"));
}

// finish_before_exit

#[tokio::test(start_paused = true)]
async fn an_exit_during_a_landing_waits_until_the_new_supervisor_is_launched() {
    // Arrange: the supervisor takes its 20 s to stop, and the exit comes 5 s into them
    let r = landing_rig().await;
    r.sup.ignores_shutdown.store(true, Ordering::SeqCst);
    // Act
    let (result, at_exit) = tokio::join!(land(&r.deps), async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        r.deps.finish_before_exit(FINISH_BEFORE_EXIT).await;
        (r.spawns.all(), r.read(&r.layout.exe))
    });
    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(at_exit.0.len(), 1, "the exit came before the launch");
    assert_eq!(at_exit.0[0].2.as_slice(), NEW_EXE);
    assert_eq!(at_exit.1, NEW_EXE);
}

#[tokio::test(start_paused = true)]
async fn an_exit_before_the_landing_stops_the_supervisor_makes_it_give_up() {
    // Arrange: Update Now downloads for 3 s, and the exit comes 1 s in
    let r = unstaged_rig(NEW).await;
    *r.http.download_delay.lock().unwrap() = Duration::from_secs(3);
    let started = Instant::now();
    // Act
    let (result, exit_took) = tokio::join!(land(&r.deps), async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        r.deps.finish_before_exit(FINISH_BEFORE_EXIT).await;
        started.elapsed()
    });
    // Assert
    assert_eq!(result, Err("StreamLoop was closed before the update could start.".to_string()));
    assert!(exit_took < Duration::from_secs(2), "the exit waited for a landing that had not begun: {exit_took:?}");
    assert_eq!(r.sup.shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(r.read(&r.layout.exe), OLD_EXE);
    assert!(!r.layout.landed.exists());
    assert!(!r.deps.landing_in_progress());
}

#[tokio::test(start_paused = true)]
async fn an_exit_after_a_landing_went_through_does_not_wait() {
    // Arrange
    let r = landing_rig().await;
    land(&r.deps).await.unwrap();
    let started = Instant::now();
    // Act
    r.deps.finish_before_exit(FINISH_BEFORE_EXIT).await;
    // Assert
    assert_eq!(started.elapsed(), Duration::ZERO);
}
