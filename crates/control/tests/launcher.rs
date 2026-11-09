use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use streamloop_common::api::SupervisorStatus;
use streamloop_common::encoder::Encoder::{self, Amf, Nvenc};
use streamloop_common::paths::Layout;
use streamloop_control::clock::{Clock, TestClock};
use streamloop_control::launcher::*;
use streamloop_control::runner::{RunError, RunOutput, RunSpec, Runner};
use streamloop_control::setup_check::{LaunchGuard, LaunchGuardOptions, FFMPEG_MISSING, NO_ENCODER, OBS_RUNNING};
use streamloop_control::supervisor_client::fake::{Call, FakeSupervisor};
use tokio::sync::oneshot;

// ---------------------------------------------------------------- harness

struct FakeSpawner {
    calls: Mutex<Vec<(PathBuf, Vec<String>)>>,
    exits: Mutex<Vec<Option<oneshot::Sender<Option<i32>>>>>,
}

struct FakeHandle(oneshot::Receiver<Option<i32>>);

#[async_trait]
impl SpawnHandle for FakeHandle {
    async fn exit_code(self: Box<Self>) -> Option<i32> {
        match self.0.await {
            Ok(code) => code,
            // A test that never ends the child leaves it running.
            Err(_) => std::future::pending().await,
        }
    }
}

impl Spawner for FakeSpawner {
    fn spawn(&self, exe: &Path, args: &[String]) -> std::io::Result<Box<dyn SpawnHandle>> {
        let (tx, rx) = oneshot::channel();
        self.calls.lock().unwrap().push((exe.to_path_buf(), args.to_vec()));
        self.exits.lock().unwrap().push(Some(tx));
        Ok(Box::new(FakeHandle(rx)))
    }
}

impl FakeSpawner {
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn args(&self, i: usize) -> Vec<String> {
        self.calls.lock().unwrap()[i].1.clone()
    }

    fn exit(&self, i: usize, code: i32) {
        let _ = self.exits.lock().unwrap()[i].take().unwrap().send(Some(code));
    }
}

/// A probe runner: `works(codec, call number)` answers each test encode, after `delay`.
struct Probes {
    works: Box<dyn Fn(&str, usize) -> bool + Send + Sync>,
    delay: Duration,
    calls: AtomicUsize,
}

#[async_trait]
impl Runner for Probes {
    async fn run(&self, spec: RunSpec) -> Result<RunOutput, RunError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let at = spec.args.iter().position(|a| a == "-c:v").unwrap();
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let ok = (self.works)(&spec.args[at + 1], n);
        Ok(RunOutput { code: Some(if ok { 0 } else { 1 }), ..RunOutput::default() })
    }
}

type Events = Arc<Mutex<Vec<(String, Map<String, Value>)>>>;

struct Guarded {
    runner: Arc<Probes>,
    obs: Arc<AtomicBool>,
    obs_calls: Arc<AtomicUsize>,
    ffmpeg: Arc<AtomicBool>,
    encoders: Arc<Mutex<(Encoder, Option<Encoder>)>>,
    saved: Arc<Mutex<Vec<(Encoder, Encoder)>>>,
    events: Events,
}

struct Rig {
    launcher: Launcher,
    client: Arc<FakeSupervisor>,
    spawner: Arc<FakeSpawner>,
    clock: Arc<TestClock>,
    events: Events,
    layout: Layout,
    guard: Option<Guarded>,
    _dir: tempfile::TempDir,
}

#[derive(Default)]
struct Setup {
    answering: bool,
    debug_faults: bool,
    hold_for_start: bool,
    own_version: Option<&'static str>,
    no_exe: bool,
    /// Creates a file or folder under the root before the launcher is built.
    stopped_marker: bool,
    guard: Option<GuardSetup>,
    remote_version: Option<&'static str>,
}

struct GuardSetup {
    works: Box<dyn Fn(&str, usize) -> bool + Send + Sync>,
    delay: Duration,
    obs: bool,
    ffmpeg: bool,
    encoders: (Encoder, Option<Encoder>),
}

fn guard_setup() -> GuardSetup {
    GuardSetup { works: Box::new(|c, _| c == "h264_nvenc"), delay: Duration::ZERO, obs: false, ffmpeg: true, encoders: (Nvenc, None) }
}

fn playing(version: Option<&str>) -> SupervisorStatus {
    SupervisorStatus { state: Some("playing".into()), version: version.map(str::to_string), ..SupervisorStatus::default() }
}

fn rig(s: Setup) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path());
    if !s.no_exe {
        std::fs::write(&layout.exe, "").unwrap();
    }
    if s.stopped_marker {
        std::fs::create_dir_all(&layout.data).unwrap();
        std::fs::write(&layout.stopped_marker, "").unwrap();
    }
    let clock = Arc::new(TestClock::new(SystemTime::now()));
    let client = Arc::new(FakeSupervisor::new());
    if s.answering {
        client.set_status(Some(playing(s.remote_version)));
    }
    let spawner = Arc::new(FakeSpawner { calls: Mutex::new(Vec::new()), exits: Mutex::new(Vec::new()) });
    let events: Events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let guarded = s.guard.map(|g| {
        let runner = Arc::new(Probes { works: g.works, delay: g.delay, calls: AtomicUsize::new(0) });
        Guarded {
            runner,
            obs: Arc::new(AtomicBool::new(g.obs)),
            obs_calls: Arc::new(AtomicUsize::new(0)),
            ffmpeg: Arc::new(AtomicBool::new(g.ffmpeg)),
            encoders: Arc::new(Mutex::new(g.encoders)),
            saved: Arc::new(Mutex::new(Vec::new())),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    });
    let guard = guarded.as_ref().map(|g| {
        let (obs, obs_calls, ffmpeg, encoders, saved, events) = (g.obs.clone(), g.obs_calls.clone(), g.ffmpeg.clone(), g.encoders.clone(), g.saved.clone(), g.events.clone());
        let (get, set) = (encoders.clone(), encoders);
        LaunchGuard::new(LaunchGuardOptions {
            layout: layout.clone(),
            runner: g.runner.clone(),
            obs_running: Box::new(move || {
                obs_calls.fetch_add(1, Ordering::SeqCst);
                obs.load(Ordering::SeqCst)
            }),
            exists: Box::new(move |_| ffmpeg.load(Ordering::SeqCst)),
            clock: clock.clone(),
            get_encoders: Box::new(move || *get.lock().unwrap()),
            save_encoders: Box::new(move |enc, pref| {
                saved.lock().unwrap().push((enc, pref));
                *set.lock().unwrap() = (enc, Some(pref));
                Ok(())
            }),
            on_event: Box::new(move |kind, detail| events.lock().unwrap().push((kind.to_string(), detail))),
        })
    });
    let launcher = Launcher::new(LauncherOptions {
        layout: layout.clone(),
        client: client.clone(),
        spawner: spawner.clone(),
        clock: clock.clone(),
        debug_faults: s.debug_faults,
        guard,
        own_version: s.own_version.map(str::to_string),
        on_event: Box::new(move |kind, detail| sink.lock().unwrap().push((kind.to_string(), detail))),
        hold_for_start: s.hold_for_start,
    });
    Rig { launcher, client, spawner, clock, events, layout, guard: guarded, _dir: dir }
}

impl Rig {
    /// Moves tokio's paused time and the test clock together, in steps the 2 s poll lines up with.
    async fn advance(&self, ms: u64) {
        assert_eq!(ms % 100, 0);
        let step = if ms % 500 == 0 { 500 } else { 100 };
        // Tasks started since the last call run first, so a new poll loop begins at the time it was started.
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        for _ in 0..ms / step {
            self.clock.advance(Duration::from_millis(step));
            tokio::time::advance(Duration::from_millis(step)).await;
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
        }
    }

    fn answering(&self, version: Option<&str>) {
        self.client.set_status(Some(playing(version)));
    }

    fn silent(&self) {
        self.client.set_status(None);
    }

    fn kinds(&self) -> Vec<String> {
        self.events.lock().unwrap().iter().map(|(k, _)| k.clone()).collect()
    }

    fn count(&self, kind: &str) -> usize {
        self.kinds().iter().filter(|k| *k == kind).count()
    }

    fn shutdowns(&self) -> usize {
        self.client.calls().iter().filter(|c| matches!(c, Call::Shutdown)).count()
    }

    fn guard(&self) -> &Guarded {
        self.guard.as_ref().unwrap()
    }

    fn guard_kinds(&self) -> Vec<String> {
        self.guard().events.lock().unwrap().iter().map(|(k, _)| k.clone()).collect()
    }

    fn data_logs(&self) -> PathBuf {
        let d = self.layout.data.join("logs");
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

fn guarded(g: GuardSetup, answering: bool) -> Rig {
    rig(Setup { guard: Some(g), answering, ..Setup::default() })
}

// ---------------------------------------------------------------- SupervisorLauncher

async fn shows_the_refusal(log_base: &str) {
    // Arrange
    let r = rig(Setup::default());
    let date = streamloop_common::time::local_date(r.clock.now());
    let lines = [
        json!({"ts": "t", "level": "critical", "kind": "config-refused", "reason": "old"}),
        json!({"ts": "t", "level": "info", "kind": "start"}),
        json!({"ts": "t", "level": "critical", "kind": "config-refused", "reason": "config.json: ytdlCookiesFromBrowser is longer than 200 characters"}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(r.data_logs().join(format!("{log_base}-{date}.jsonl")), text).unwrap();
    r.launcher.start();
    r.advance(4_000).await;
    // Act
    r.spawner.exit(0, 3);
    r.advance(2_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.launcher.state(), LauncherState::Down);
    assert_eq!(
        r.launcher.block_reason().as_deref(),
        Some("The supervisor refused config.json: config.json: ytdlCookiesFromBrowser is longer than 200 characters")
    );
}

#[tokio::test(start_paused = true)]
async fn shows_why_the_supervisor_refused_config_json_when_it_exits_3_supervisor_events_log() {
    shows_the_refusal("supervisor-events").await;
}

#[tokio::test(start_paused = true)]
async fn shows_why_the_supervisor_refused_config_json_when_it_exits_3_events_log() {
    shows_the_refusal("events").await;
}

#[tokio::test(start_paused = true)]
async fn attaches_without_spawning_when_the_supervisor_already_answers() {
    // Arrange
    let r = rig(Setup { answering: true, ..Setup::default() });
    // Act
    r.launcher.start();
    r.advance(20_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 0);
    assert_eq!(r.launcher.state(), LauncherState::Running);
}

#[tokio::test(start_paused = true)]
async fn spawns_the_root_exe_with_launch() {
    // Arrange
    let r = rig(Setup::default());
    // Act
    r.launcher.start();
    r.advance(2_000).await; // polls at 0 and 2 s
    let after_two = r.spawner.count();
    r.advance(2_000).await; // third miss at 4 s
    r.launcher.stop();
    // Assert
    assert_eq!(after_two, 0);
    assert_eq!(r.spawner.count(), 1);
    let (exe, args) = r.spawner.calls.lock().unwrap()[0].clone();
    assert_eq!(exe, r.layout.exe);
    assert_eq!(args, vec!["launch".to_string(), "--root".into(), r.layout.root.to_string_lossy().into_owned()]);
    assert_eq!(r.launcher.state(), LauncherState::Starting);
    assert!(r.kinds().contains(&"supervisor-launched".to_string()));
}

#[tokio::test(start_paused = true)]
async fn passes_debug_faults_only_when_asked_to() {
    // Arrange
    let plain = rig(Setup::default());
    let debug = rig(Setup { debug_faults: true, ..Setup::default() });
    // Act
    plain.launcher.start();
    debug.launcher.start();
    plain.advance(4_000).await;
    plain.launcher.stop();
    debug.launcher.stop();
    // Assert
    assert!(!plain.spawner.args(0).contains(&"--debug-faults".to_string()));
    assert_eq!(debug.spawner.args(0).last().map(String::as_str), Some("--debug-faults"));
}

#[tokio::test(start_paused = true)]
async fn does_not_spawn_again_while_starting_and_inside_the_backoff() {
    // Arrange
    let r = rig(Setup::default());
    // Act
    r.launcher.start();
    r.advance(4_000 + 9_000).await;
    let within_first = r.spawner.count();
    r.advance(2_000).await; // 10 s backoff over
    let after_first = r.spawner.count();
    r.advance(28_000).await; // 30 s backoff not over
    let within_second = r.spawner.count();
    r.advance(4_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!((within_first, after_first, within_second), (1, 2, 2));
    assert_eq!(r.spawner.count(), 3);
}

#[tokio::test(start_paused = true)]
async fn never_spawns_when_stopped_by_the_user_until_launch_now() {
    // Arrange
    let r = rig(Setup::default());
    r.launcher.mark_stopped_by_user();
    // Act
    r.launcher.start();
    r.advance(600_000).await;
    let while_stopped = r.spawner.count();
    let state_while_stopped = r.launcher.state();
    r.launcher.launch_now();
    r.advance(0).await;
    tokio::task::yield_now().await;
    r.launcher.stop();
    // Assert
    assert_eq!(while_stopped, 0);
    assert_eq!(state_while_stopped, LauncherState::StoppedByUser);
    assert_eq!(r.spawner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn stop_only_stops_watching_and_never_shuts_the_supervisor_down() {
    // Arrange
    let r = rig(Setup { answering: true, ..Setup::default() });
    r.launcher.start();
    r.advance(0).await;
    // Act
    r.launcher.stop();
    r.advance(10_000).await;
    // Assert
    assert_eq!(r.shutdowns(), 0);
    assert_eq!(r.client.calls().iter().filter(|c| matches!(c, Call::Status)).count(), 1);
}

#[tokio::test(start_paused = true)]
async fn reports_missing_once_when_the_exe_is_absent_at_a_launch_attempt() {
    // Arrange
    let r = rig(Setup { no_exe: true, ..Setup::default() });
    // Act
    r.launcher.start();
    r.advance(60_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 0);
    assert_eq!(r.launcher.state(), LauncherState::Missing);
    assert_eq!(r.count("supervisor-missing"), 1);
}

#[tokio::test(start_paused = true)]
async fn emits_down_once_per_outage_and_back_when_it_answers_again() {
    // Arrange
    let r = rig(Setup { answering: true, ..Setup::default() });
    r.launcher.start();
    r.advance(0).await;
    // Act
    r.silent();
    r.advance(30_000).await;
    r.answering(None);
    r.advance(2_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.count("supervisor-down"), 1);
    assert_eq!(r.count("supervisor-back"), 1);
    assert_eq!(r.launcher.state(), LauncherState::Running);
}

#[tokio::test(start_paused = true)]
async fn stays_starting_when_the_intermediate_exits_0_after_handing_off() {
    // Arrange
    let r = rig(Setup::default());
    r.launcher.start();
    r.advance(4_000).await;
    // Act
    r.spawner.exit(0, 0);
    r.advance(0).await;
    // Assert
    assert_eq!(r.launcher.state(), LauncherState::Starting);
    r.launcher.stop();
}

#[tokio::test(start_paused = true)]
async fn treats_an_intermediate_that_relays_exit_2_while_another_supervisor_answers_as_attach() {
    // Arrange
    let r = rig(Setup::default());
    r.launcher.start();
    r.advance(4_000).await;
    // Act: the port was taken by a supervisor that was still starting
    r.spawner.exit(0, 2);
    r.answering(None);
    r.advance(2_000).await;
    r.silent();
    r.advance(6_000).await; // three misses, no backoff owed
    // Assert
    assert_eq!(r.spawner.count(), 2);
    r.launcher.stop();
}

#[tokio::test(start_paused = true)]
async fn resets_the_backoff_after_60_s_of_running() {
    // Arrange: four spawns (4 s, 14 s, 44 s, 104 s) put the next wait at 5 min
    let r = rig(Setup::default());
    r.launcher.start();
    r.advance(106_000).await;
    assert_eq!(r.spawner.count(), 4);
    // Act
    r.answering(None);
    r.advance(62_000).await;
    r.silent();
    r.advance(6_000).await;
    let after_outage = r.spawner.count();
    r.launcher.stop();
    // Assert: the first spawn of a new outage has no backoff
    assert_eq!(after_outage, 5);
}

// ---------------------------------------------------------------- SupervisorLauncher fixes

#[tokio::test(start_paused = true)]
async fn spawns_exactly_once_when_launch_now_lands_while_a_poll_is_in_flight() {
    // Arrange: stopped by the user, misses piled up, the next status call hangs until released
    let r = rig(Setup::default());
    r.launcher.mark_stopped_by_user();
    r.launcher.start();
    r.advance(10_000).await;
    r.client.hang();
    r.advance(2_000).await; // that poll is now in flight
    // Act
    r.launcher.launch_now();
    r.client.release();
    r.advance(4_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn launch_now_spawns_at_once_without_the_backoff_of_earlier_crashes() {
    // Arrange: two spawns put the next wait at 30 s, then the user stops it
    let r = rig(Setup::default());
    r.launcher.start();
    r.advance(15_000).await;
    assert_eq!(r.spawner.count(), 2);
    r.launcher.mark_stopped_by_user();
    // Act
    r.launcher.launch_now();
    r.advance(0).await;
    tokio::task::yield_now().await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_user_stop_is_not_an_outage_no_down_no_back_no_spawn() {
    // Arrange
    let r = rig(Setup { answering: true, ..Setup::default() });
    r.launcher.start();
    r.advance(0).await;
    // Act
    r.launcher.mark_stopped_by_user();
    r.silent();
    r.advance(10_000).await;
    let state_after_stop = r.launcher.state();
    r.answering(None);
    r.launcher.launch_now();
    r.advance(2_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(state_after_stop, LauncherState::StoppedByUser);
    assert_eq!(r.spawner.count(), 0);
    assert!(!r.kinds().contains(&"supervisor-down".to_string()));
    assert!(!r.kinds().contains(&"supervisor-back".to_string()));
}

#[tokio::test(start_paused = true)]
async fn a_start_soon_after_stop_is_not_an_outage_either() {
    // Arrange: Stop, then Start before three misses
    let r = rig(Setup { answering: true, ..Setup::default() });
    r.launcher.start();
    r.advance(0).await;
    r.launcher.mark_stopped_by_user();
    r.silent();
    r.advance(2_000).await;
    // Act
    r.launcher.launch_now();
    r.advance(6_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 1);
    assert!(!r.kinds().contains(&"supervisor-down".to_string()));
}

#[tokio::test(start_paused = true)]
async fn stop_then_launch_now_while_the_old_one_is_still_answering_spawns_once_with_no_down_event() {
    // Arrange: running, then Stop and Start back to back (the dashboard's Restart stream)
    let r = rig(Setup { answering: true, ..Setup::default() });
    r.launcher.start();
    r.advance(0).await;
    // Act: the old one keeps answering for 6 s while it shuts down, then goes away
    r.launcher.mark_stopped_by_user();
    r.launcher.launch_now();
    r.advance(6_000).await;
    let while_draining = r.spawner.count();
    r.silent();
    r.advance(10_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(while_draining, 0);
    assert_eq!(r.spawner.count(), 1);
    assert!(!r.kinds().contains(&"supervisor-down".to_string()));
    assert!(!r.launcher.is_stopped_by_user());
}

#[tokio::test(start_paused = true)]
async fn the_old_one_never_going_away_within_the_bound_gives_a_plain_reason_and_no_down_event() {
    // Arrange
    let r = rig(Setup { answering: true, ..Setup::default() });
    r.launcher.start();
    r.advance(0).await;
    r.launcher.mark_stopped_by_user();
    r.launcher.launch_now();
    // Act
    r.advance(5_000).await;
    let early = r.launcher.wait_reason();
    r.advance(25_000).await;
    let late = r.launcher.wait_reason();
    r.silent();
    r.advance(10_000).await;
    r.launcher.stop();
    // Assert: a reason the whole time, a different one past 20 s, and a quiet launch once it exits
    assert!(early.unwrap().to_lowercase().contains("waiting for the stopped supervisor"));
    assert!(late.unwrap().to_lowercase().contains("still running after 20 s"));
    assert!(!r.kinds().contains(&"supervisor-down".to_string()));
    assert_eq!(r.spawner.count(), 1);
    assert_eq!(r.launcher.wait_reason(), None);
}

#[tokio::test(start_paused = true)]
async fn ignores_an_older_childs_exit_once_a_newer_spawn_is_starting() {
    // Arrange: spawns at 4 s and 14 s
    let r = rig(Setup::default());
    r.launcher.start();
    r.advance(15_000).await;
    // Act
    r.spawner.exit(0, 1);
    r.advance(0).await;
    // Assert
    assert_eq!(r.launcher.state(), LauncherState::Starting);
    r.launcher.stop();
}

#[tokio::test(start_paused = true)]
async fn reports_down_when_the_current_launch_exits_1_the_supervisor_crashed_during_the_bind_wait() {
    // Arrange: the first spawn at 4 s
    let r = rig(Setup::default());
    r.launcher.start();
    r.advance(5_000).await;
    // Act
    r.spawner.exit(0, 1);
    r.advance(0).await;
    // Assert
    assert_eq!(r.launcher.state(), LauncherState::Down);
    r.launcher.stop();
}

// ---------------------------------------------------------------- stopped-by-user marker

#[tokio::test(start_paused = true)]
async fn a_launcher_constructed_with_the_marker_present_starts_stopped_and_never_spawns() {
    // Arrange
    let r = rig(Setup { stopped_marker: true, ..Setup::default() });
    // Act
    r.launcher.start();
    r.advance(600_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 0);
    assert_eq!(r.launcher.state(), LauncherState::StoppedByUser);
    assert!(r.launcher.is_stopped_by_user());
}

#[tokio::test(start_paused = true)]
async fn stop_writes_the_marker_and_start_removes_it() {
    // Arrange
    let r = rig(Setup::default());
    // Act
    r.launcher.mark_stopped_by_user();
    let after_stop = r.layout.stopped_marker.exists();
    r.launcher.launch_now();
    // Assert
    assert!(after_stop);
    assert!(!r.layout.stopped_marker.exists());
}

#[tokio::test(start_paused = true)]
async fn start_still_starts_when_the_marker_cannot_be_removed() {
    // Arrange: a folder at the marker path makes the delete fail, like a file held by antivirus
    let r = rig(Setup::default());
    std::fs::create_dir_all(r.layout.stopped_marker.join("held")).unwrap();
    // Act
    r.launcher.launch_now();
    // Assert
    assert!(!r.launcher.is_stopped_by_user());
}

// ---------------------------------------------------------------- hold for start

#[tokio::test(start_paused = true)]
async fn holds_the_launch_while_waiting_for_start_and_reports_ready() {
    // Arrange
    let r = rig(Setup { hold_for_start: true, ..Setup::default() });
    // Act
    r.launcher.start();
    r.advance(600_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 0);
    assert_eq!(r.launcher.state(), LauncherState::Ready);
}

#[tokio::test(start_paused = true)]
async fn attaches_to_a_supervisor_that_answers_while_held() {
    // Arrange
    let r = rig(Setup { hold_for_start: true, answering: true, ..Setup::default() });
    // Act
    r.launcher.start();
    r.advance(4_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.launcher.state(), LauncherState::Running);
    assert_eq!(r.spawner.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn launches_after_start() {
    // Arrange
    let r = rig(Setup { hold_for_start: true, ..Setup::default() });
    r.launcher.start();
    r.advance(10_000).await;
    // Act
    r.launcher.launch_now();
    r.advance(2_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn release_launches_unless_the_stream_was_stopped_by_the_user() {
    // Arrange
    let free = rig(Setup { hold_for_start: true, ..Setup::default() });
    free.launcher.start();
    free.advance(10_000).await;
    let stopped = rig(Setup { hold_for_start: true, stopped_marker: true, ..Setup::default() });
    stopped.launcher.start();
    // Act
    free.launcher.release();
    stopped.launcher.release();
    free.advance(3_000).await;
    stopped.advance(3_000).await;
    free.launcher.stop();
    stopped.launcher.stop();
    // Assert
    assert_eq!(free.spawner.count(), 1);
    assert_eq!(stopped.spawner.count(), 0);
    assert_eq!(stopped.launcher.state(), LauncherState::StoppedByUser);
}

#[tokio::test(start_paused = true)]
async fn keeps_the_waiting_marker_until_start() {
    // Arrange
    let r = rig(Setup { hold_for_start: true, ..Setup::default() });
    r.launcher.start();
    r.advance(10_000).await;
    let while_held = r.layout.waiting_for_start.exists();
    // Act
    r.launcher.launch_now();
    r.launcher.stop();
    // Assert
    assert!(while_held);
    assert!(!r.layout.waiting_for_start.exists());
}

#[tokio::test(start_paused = true)]
async fn removes_the_waiting_marker_on_attach() {
    // Arrange
    let r = rig(Setup { hold_for_start: true, answering: true, ..Setup::default() });
    let before = r.layout.waiting_for_start.exists();
    // Act
    r.launcher.start();
    r.advance(2_000).await;
    r.launcher.stop();
    // Assert
    assert!(before);
    assert!(!r.layout.waiting_for_start.exists());
}

#[tokio::test(start_paused = true)]
async fn relaunches_a_supervisor_that_dies_after_it_was_attached_while_held() {
    // Arrange
    let r = rig(Setup { hold_for_start: true, answering: true, ..Setup::default() });
    r.launcher.start();
    r.advance(4_000).await;
    // Act
    r.silent();
    r.advance(10_000).await;
    r.launcher.stop();
    // Assert
    assert!(r.kinds().contains(&"supervisor-down".to_string()));
    assert_eq!(r.spawner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn removes_a_waiting_marker_left_by_an_earlier_window_when_this_start_does_not_hold() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path());
    std::fs::create_dir_all(&layout.data).unwrap();
    std::fs::write(&layout.waiting_for_start, "").unwrap();
    // Act
    let _launcher = Launcher::new(LauncherOptions {
        layout: layout.clone(),
        client: Arc::new(FakeSupervisor::new()),
        spawner: Arc::new(FakeSpawner { calls: Mutex::new(Vec::new()), exits: Mutex::new(Vec::new()) }),
        clock: Arc::new(TestClock::new(SystemTime::now())),
        debug_faults: false,
        guard: None,
        own_version: None,
        on_event: Box::new(|_, _| {}),
        hold_for_start: false,
    });
    // Assert
    assert!(!layout.waiting_for_start.exists());
}

#[tokio::test(start_paused = true)]
async fn release_launches_on_the_next_poll() {
    // Arrange
    let r = rig(Setup { hold_for_start: true, ..Setup::default() });
    r.launcher.start();
    r.advance(100).await;
    // Act
    r.launcher.release();
    r.advance(100).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn release_removes_the_waiting_marker() {
    // Arrange
    let r = rig(Setup { hold_for_start: true, ..Setup::default() });
    // Act
    r.launcher.release();
    // Assert
    assert!(!r.layout.waiting_for_start.exists());
}

#[tokio::test(start_paused = true)]
async fn writes_no_marker_when_not_held() {
    // Arrange + Act
    let r = rig(Setup::default());
    // Assert
    assert!(!r.layout.waiting_for_start.exists());
}

#[test]
fn should_hold_follows_the_start_kind() {
    // Arrange: (start, autoStartStream, waiting marker, holds)
    let table = [
        (StartKind::First, true, false, false),
        (StartKind::First, false, false, true),
        (StartKind::First, false, true, true),
        (StartKind::Rollback, true, false, false),
        (StartKind::Rollback, false, false, true),
        (StartKind::Restart, false, false, false),
        (StartKind::Restart, true, true, true),
        (StartKind::Restart, false, true, true),
        (StartKind::Update, true, false, true),
        (StartKind::Update, false, false, true),
    ];
    for (start, auto, marker, expected) in table {
        // Act
        let held = should_hold(start, auto, marker);
        // Assert
        assert_eq!(held, expected, "{start:?} with autoStartStream {auto} and marker {marker}");
    }
}

// ---------------------------------------------------------------- launch guard

#[tokio::test(start_paused = true)]
async fn holds_the_launch_while_obs_runs_with_the_reason_and_one_event_and_launches_once_obs_closes() {
    // Arrange
    let r = guarded(GuardSetup { obs: true, ..guard_setup() }, false);
    // Act
    r.launcher.start();
    r.advance(20_000).await;
    let blocked = r.launcher.block_reason();
    let spawns_while_blocked = r.spawner.count();
    r.guard().obs.store(false, Ordering::SeqCst);
    r.advance(2_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(spawns_while_blocked, 0);
    assert_eq!(blocked.as_deref(), Some(OBS_RUNNING));
    assert_eq!(r.guard_kinds().iter().filter(|k| *k == "supervisor-blocked-by-obs").count(), 1);
    assert_eq!(r.spawner.count(), 1);
    assert_eq!(r.launcher.block_reason(), None);
}

#[tokio::test(start_paused = true)]
async fn falls_back_to_amf_when_nvenc_fails_twice_keeps_the_preference_emits_once_then_spawns_once() {
    // Arrange
    let r = guarded(GuardSetup { works: Box::new(|c, _| c == "h264_amf" || c == "libx264"), ..guard_setup() }, false);
    // Act
    r.launcher.start();
    r.advance(8_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(*r.guard().saved.lock().unwrap(), vec![(Amf, Nvenc)]);
    let events = r.guard().events.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "encoder-fallback");
    assert_eq!(Value::Object(events[0].1.clone()), json!({"from": "h264_nvenc", "to": "h264_amf"}));
    assert_eq!(r.spawner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn keeps_nvenc_through_a_blip_that_passes_on_the_retry_no_fallback_no_save() {
    // Arrange
    let r = guarded(GuardSetup { works: Box::new(|c, call| c == "h264_nvenc" && call > 1), ..guard_setup() }, false);
    // Act
    r.launcher.start();
    r.advance(8_000).await;
    r.launcher.stop();
    // Assert
    assert!(r.guard().saved.lock().unwrap().is_empty());
    assert!(r.guard_kinds().is_empty());
    assert_eq!(r.spawner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn restores_the_preferred_encoder_once_it_works_again_with_one_event() {
    // Arrange: an earlier fallback left amf in use
    let r = guarded(GuardSetup { encoders: (Amf, Some(Nvenc)), ..guard_setup() }, false);
    // Act: launch, then the supervisor answers, dies and is launched again
    r.launcher.start();
    r.advance(5_000).await;
    r.spawner.exit(0, 0);
    r.answering(None);
    r.advance(2_000).await;
    r.silent();
    r.advance(20_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(*r.guard().saved.lock().unwrap(), vec![(Nvenc, Nvenc)]);
    assert_eq!(*r.guard().encoders.lock().unwrap(), (Nvenc, Some(Nvenc)));
    let events = r.guard().events.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "encoder-restored");
    assert_eq!(Value::Object(events[0].1.clone()), json!({"from": "h264_amf", "to": "h264_nvenc"}));
}

#[tokio::test(start_paused = true)]
async fn neither_saves_nor_emits_when_the_configured_encoder_works() {
    // Arrange
    let r = guarded(guard_setup(), false);
    // Act
    r.launcher.start();
    r.advance(5_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 1);
    assert!(r.guard().saved.lock().unwrap().is_empty());
    assert!(r.guard_kinds().is_empty());
}

#[tokio::test(start_paused = true)]
async fn never_spawns_when_no_encoder_works_one_alert_a_plain_reason_and_one_round_of_probes_a_minute() {
    // Arrange
    let r = guarded(GuardSetup { works: Box::new(|_, _| false), ..guard_setup() }, false);
    // Act
    r.launcher.start();
    r.advance(30_000).await;
    let probes_in_30s = r.guard().runner.calls.load(Ordering::SeqCst);
    r.advance(40_000).await;
    let probes_in_70s = r.guard().runner.calls.load(Ordering::SeqCst);
    r.launcher.stop();
    // Assert: 5 probes per round (nvenc twice, then the other three)
    assert_eq!((probes_in_30s, probes_in_70s), (5, 10));
    assert_eq!(r.spawner.count(), 0);
    assert_eq!(r.launcher.block_reason().as_deref(), Some(NO_ENCODER));
    assert_eq!(r.guard_kinds().iter().filter(|k| *k == "no-encoder").count(), 1);
}

#[tokio::test(start_paused = true)]
async fn holds_with_its_own_reason_when_ffmpeg_is_missing_without_probing() {
    // Arrange
    let r = guarded(GuardSetup { ffmpeg: false, ..guard_setup() }, false);
    // Act
    r.launcher.start();
    r.advance(10_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 0);
    assert_eq!(r.guard().runner.calls.load(Ordering::SeqCst), 0);
    assert_eq!(r.launcher.block_reason().as_deref(), Some(FFMPEG_MISSING));
    assert_eq!(r.guard_kinds(), vec!["ffmpeg-missing"]);
}

#[tokio::test(start_paused = true)]
async fn the_guard_does_not_run_on_attach() {
    // Arrange
    let r = guarded(guard_setup(), true);
    // Act
    r.launcher.start();
    r.advance(20_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.guard().obs_calls.load(Ordering::SeqCst), 0);
    assert_eq!(r.guard().runner.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn the_guard_does_not_run_again_while_a_spawn_waits_out_its_backoff() {
    // Arrange: the first spawn at 4 s owes a 10 s backoff
    let r = guarded(guard_setup(), false);
    // Act
    r.launcher.start();
    r.advance(12_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 1);
    assert_eq!(r.guard().obs_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn does_not_spawn_when_stop_lands_during_a_slow_guard() {
    // Arrange: the guard starts at 4 s and takes 3 s
    let r = guarded(GuardSetup { delay: Duration::from_secs(3), ..guard_setup() }, false);
    r.launcher.start();
    r.advance(5_000).await;
    // Act
    r.launcher.mark_stopped_by_user();
    r.advance(20_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 0);
    assert_eq!(r.launcher.state(), LauncherState::StoppedByUser);
}

#[tokio::test(start_paused = true)]
async fn holds_a_start_while_obs_runs_and_spawns_on_the_first_poll_after_obs_closes() {
    // Arrange
    let r = guarded(GuardSetup { obs: true, ..guard_setup() }, false);
    r.launcher.mark_stopped_by_user();
    r.launcher.start();
    r.advance(10_000).await;
    // Act
    r.launcher.launch_now();
    r.advance(4_000).await;
    let held = r.launcher.block_reason();
    let spawns_while_held = r.spawner.count();
    r.guard().obs.store(false, Ordering::SeqCst);
    r.advance(2_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(held.as_deref(), Some(OBS_RUNNING));
    assert_eq!(spawns_while_held, 0);
    assert_eq!(r.spawner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_start_during_drain_that_the_guard_holds_launches_on_the_first_poll_after_the_hold_clears() {
    // Arrange: running, then Stop and Start back to back, with OBS open
    let r = guarded(GuardSetup { obs: true, ..guard_setup() }, true);
    r.launcher.start();
    r.advance(0).await;
    r.launcher.mark_stopped_by_user();
    r.launcher.launch_now();
    // Act: the old one exits, the guard holds, then OBS closes
    r.silent();
    r.advance(4_000).await;
    let wait = r.launcher.wait_reason();
    let block = r.launcher.block_reason();
    let spawns_while_held = r.spawner.count();
    r.guard().obs.store(false, Ordering::SeqCst);
    r.advance(2_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!((wait, block.as_deref()), (None, Some(OBS_RUNNING)));
    assert_eq!(spawns_while_held, 0);
    assert_eq!(r.spawner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn measures_the_port_taken_window_from_the_real_spawn_after_a_slow_guard() {
    // Arrange: miss 3 at 4 s, the guard takes 3 s, so the spawn is at 7 s
    let r = guarded(GuardSetup { delay: Duration::from_secs(3), ..guard_setup() }, false);
    r.launcher.start();
    r.advance(10_000).await;
    // Act: exit 2 at 10 s is 3 s after the spawn but 6 s after the guard began; another supervisor answers
    r.spawner.exit(0, 2);
    r.answering(None);
    r.advance(2_000).await;
    r.silent();
    r.advance(28_000).await;
    r.launcher.stop();
    // Assert: as an attach, spawn 2 (21 s) owes 10 s, not 30 s, so spawn 3 lands by 40 s
    assert_eq!(r.spawner.count(), 3);
}

// ---------------------------------------------------------------- version rule

const OWN: &str = "3.1.0";

fn versioned(remote: Option<&'static str>) -> Rig {
    rig(Setup { answering: true, own_version: Some(OWN), remote_version: remote, ..Setup::default() })
}

#[tokio::test(start_paused = true)]
async fn restarts_a_supervisor_of_another_version() {
    // Arrange
    let r = versioned(Some("3.0.9"));
    r.launcher.start();
    // Act: it keeps answering while it shuts down, then goes away
    r.advance(6_000).await;
    let while_draining = (r.shutdowns(), r.spawner.count());
    r.silent();
    r.advance(10_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(while_draining, (1, 0));
    assert_eq!(r.shutdowns(), 1);
    assert_eq!(r.spawner.count(), 1);
    assert!(!r.kinds().contains(&"supervisor-down".to_string()));
    let events = r.events.lock().unwrap().clone();
    let (_, d) = events.iter().find(|(k, _)| k == "supervisor-version-restart").expect("a timeline event for the restart");
    assert_eq!(Value::Object(d.clone()), json!({"running": "3.0.9", "expected": OWN}));
}

#[tokio::test(start_paused = true)]
async fn names_both_versions_in_the_timeline_text() {
    // Arrange
    let r = versioned(Some("3.0.9"));
    r.launcher.start();
    r.advance(2_000).await;
    let (kind, detail) = r.events.lock().unwrap().iter().find(|(k, _)| k == "supervisor-version-restart").cloned().unwrap();
    let event = streamloop_control::events::launcher_event(&kind, detail, r.clock.now());
    // Act
    let text = streamloop_control::events::describe(&event).message;
    r.launcher.stop();
    // Assert
    assert_eq!(text, "Restarting the supervisor: it runs 3.0.9, this is 3.1.0.");
}

#[tokio::test(start_paused = true)]
async fn restarts_a_supervisor_that_reports_no_version() {
    // Arrange
    let r = versioned(None);
    r.launcher.start();
    // Act
    r.advance(4_000).await;
    r.silent();
    r.advance(6_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.shutdowns(), 1);
    assert_eq!(r.spawner.count(), 1);
    assert!(!r.kinds().contains(&"supervisor-down".to_string()));
}

#[tokio::test(start_paused = true)]
async fn leaves_another_version_alone_when_stopped_by_user() {
    // Arrange
    let r = versioned(Some("3.0.9"));
    r.launcher.mark_stopped_by_user();
    // Act
    r.launcher.start();
    r.advance(20_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.shutdowns(), 0);
    assert_eq!(r.spawner.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn leaves_another_version_alone_while_suspended() {
    // Arrange
    let r = versioned(Some("3.0.9"));
    r.launcher.suspend();
    // Act
    r.launcher.start();
    r.advance(20_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.shutdowns(), 0);
}

#[tokio::test(start_paused = true)]
async fn never_restarts_a_freshly_launched_supervisor_of_the_same_version() {
    // Arrange: a supervisor between its port bind and its first status answers `starting`
    let r = rig(Setup { own_version: Some(OWN), ..Setup::default() });
    r.client.set_status(Some(SupervisorStatus { state: Some("starting".into()), version: Some(OWN.into()), ..SupervisorStatus::default() }));
    // Act
    r.launcher.start();
    r.advance(30_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.shutdowns(), 0);
    assert_eq!(r.spawner.count(), 0);
    assert_eq!(r.launcher.state(), LauncherState::Running);
}

#[tokio::test(start_paused = true)]
async fn does_not_replace_a_supervisor_again_within_five_minutes() {
    // Arrange: the replacement also reports the old version
    let r = versioned(Some("3.0.9"));
    r.launcher.start();
    r.advance(2_000).await;
    r.silent();
    r.advance(6_000).await;
    r.answering(Some("3.0.9"));
    // Act
    r.advance(120_000).await;
    let within = r.shutdowns();
    r.advance(200_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(within, 1);
    assert_eq!(r.shutdowns(), 2);
}

#[tokio::test(start_paused = true)]
async fn keeps_the_old_supervisor_when_its_shutdown_fails() {
    // Arrange
    let r = versioned(Some("3.0.9"));
    r.client.set_shutdown_result(false);
    // Act
    r.launcher.start();
    r.advance(20_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.shutdowns(), 1);
    assert_eq!(r.spawner.count(), 0);
    assert_eq!(r.count("supervisor-version-restart"), 0);
    assert_eq!(r.launcher.state(), LauncherState::Running);
}

// ---------------------------------------------------------------- suspend

#[tokio::test(start_paused = true)]
async fn holds_no_relaunch_while_held() {
    // Arrange
    let r = rig(Setup::default());
    r.launcher.suspend();
    // Act
    r.launcher.start();
    r.advance(600_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn reports_no_outage_while_held() {
    // Arrange
    let r = rig(Setup { answering: true, ..Setup::default() });
    r.launcher.start();
    r.advance(2_000).await;
    r.launcher.suspend();
    // Act
    r.silent();
    r.advance(60_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.count("supervisor-down"), 0);
    assert_eq!(r.spawner.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn does_not_spawn_when_a_landing_suspends_during_a_slow_guard() {
    // Arrange: the guard starts at 4 s and takes 3 s
    let r = guarded(GuardSetup { delay: Duration::from_secs(3), ..guard_setup() }, false);
    r.launcher.start();
    r.advance(5_000).await;
    // Act
    r.launcher.suspend();
    r.advance(20_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_start_while_suspended_launches_once_and_keeps_the_hold() {
    // Arrange
    let r = rig(Setup { own_version: Some(OWN), ..Setup::default() });
    r.launcher.suspend();
    r.launcher.start();
    r.advance(30_000).await;
    // Act: the launched supervisor is another version and then dies
    r.launcher.launch_now();
    r.advance(2_000).await;
    let launched = r.spawner.count();
    r.answering(Some("3.1.1"));
    r.advance(10_000).await;
    r.silent();
    r.advance(600_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(launched, 1);
    assert_eq!(r.spawner.count(), 1);
    assert_eq!(r.shutdowns(), 0);
    assert_eq!(r.count("supervisor-down"), 0);
}

#[tokio::test(start_paused = true)]
async fn relaunches_after_resume() {
    // Arrange
    let r = rig(Setup::default());
    r.launcher.suspend();
    r.launcher.start();
    r.advance(30_000).await;
    // Act
    r.launcher.resume();
    r.advance(6_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(r.spawner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn last_status_is_what_the_latest_poll_saw() {
    // Arrange
    let r = rig(Setup { answering: true, ..Setup::default() });
    r.launcher.start();
    r.advance(0).await;
    let answered = r.launcher.last_status();
    // Act
    r.silent();
    r.advance(2_000).await;
    r.launcher.stop();
    // Assert
    assert_eq!(answered.and_then(|s| s.state).as_deref(), Some("playing"));
    assert_eq!(r.launcher.last_status(), None);
}
