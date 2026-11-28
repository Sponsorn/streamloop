//! Starts the supervisor as a sibling of the control process and relaunches it when it stops answering.
//! It never stops the supervisor except to replace one of another version; the dashboard's "Stop stream" does the rest.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde_json::{Map, Value};
use streamloop_common::api::SupervisorStatus;
use streamloop_common::events::events_file;
use streamloop_common::paths::Layout;
use streamloop_common::time::local_date;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::clock::Clock;
use crate::setup_check::LaunchGuard;
use crate::supervisor_client::SupervisorApi;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MISSES_BEFORE_LAUNCH: u32 = 3;
const BACKOFF: [Duration; 4] = [Duration::from_secs(10), Duration::from_secs(30), Duration::from_secs(60), Duration::from_secs(300)];
const BACKOFF_RESET: Duration = Duration::from_secs(60);
const PORT_TAKEN_EXIT: i32 = 2;
const CONFIG_REFUSED_EXIT: i32 = 3;
const PORT_TAKEN_WINDOW: Duration = Duration::from_secs(5);
const DRAIN_LIMIT: Duration = Duration::from_secs(20);
/// A supervisor that still has the wrong version after a replacement is left alone this long, so a bad install cannot restart the stream in a loop.
const VERSION_RESTART_PAUSE: Duration = Duration::from_secs(300);
/// How long a stop of the supervisor waits for a `launch` helper to exit. The helper waits up to 3 s for the
/// supervisor's port and 0.3 s more for a refused config.json; the rest is for a slow box starting it.
pub const LAUNCH_EXIT_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherState {
    Missing,
    Starting,
    Running,
    StoppedByUser,
    Down,
    /// Held at this start until Start; nothing is running.
    Ready,
}

impl LauncherState {
    pub fn as_str(self) -> &'static str {
        match self {
            LauncherState::Missing => "missing",
            LauncherState::Starting => "starting",
            LauncherState::Running => "running",
            LauncherState::StoppedByUser => "stopped-by-user",
            LauncherState::Down => "down",
            LauncherState::Ready => "ready",
        }
    }
}

/// How a launch ended, for a caller that waits for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// The `launch` helper exited: 0 once the supervisor holds its port, 2 when another one does.
    Exited(Option<i32>),
    /// The launch guard held it, for this reason.
    Held(String),
    /// `streamloop.exe` is missing or could not be started.
    NotStarted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartKind {
    First,
    Restart,
    /// The first start after a landing; `held` comes from `_update\landed`.
    Update { held: bool },
    /// The first start after START.bat put the previous exe back; `held` comes from `data\update-rolled-back`
    /// when it can be read.
    Rollback { held: Option<bool> },
}

/// Whether this start waits for Start. Recovery is not a start: a restart holds only if the last start still waits.
/// An update or its rollback never changes whether the stream is held.
pub fn should_hold(start: StartKind, auto_start_stream: bool, waiting_marker: bool) -> bool {
    match start {
        StartKind::First => !auto_start_stream,
        StartKind::Restart => waiting_marker,
        StartKind::Update { held } => held,
        StartKind::Rollback { held } => held.unwrap_or(!auto_start_stream),
    }
}

pub trait Spawner: Send + Sync {
    fn spawn(&self, exe: &Path, args: &[String]) -> std::io::Result<Box<dyn SpawnHandle>>;
}

#[async_trait]
pub trait SpawnHandle: Send {
    /// `None` when the process ended without an exit code.
    async fn exit_code(self: Box<Self>) -> Option<i32>;
}

/// Starts a real process: no window, no stdio, not killed when the handle goes.
pub struct ProcessSpawner;

struct ProcessHandle(tokio::process::Child);

#[async_trait]
impl SpawnHandle for ProcessHandle {
    async fn exit_code(mut self: Box<Self>) -> Option<i32> {
        self.0.wait().await.ok().and_then(|s| s.code())
    }
}

impl Spawner for ProcessSpawner {
    fn spawn(&self, exe: &Path, args: &[String]) -> std::io::Result<Box<dyn SpawnHandle>> {
        let mut cmd = streamloop_common::procs::command(exe);
        cmd.args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let child = tokio::process::Command::from(cmd).spawn()?;
        Ok(Box::new(ProcessHandle(child)))
    }
}

pub type OnEvent = Box<dyn Fn(&str, Map<String, Value>) + Send + Sync>;

pub struct LauncherOptions {
    pub layout: Layout,
    pub client: Arc<dyn SupervisorApi>,
    pub spawner: Arc<dyn Spawner>,
    pub clock: Arc<dyn Clock>,
    /// Dev and test runs only: adds `--debug-faults`, which opens `POST /debug/fault`.
    pub debug_faults: bool,
    /// Runs right before a spawn, never on attach. A reason holds the launch until a later poll.
    pub guard: Option<LaunchGuard>,
    /// The version of this exe; a supervisor that reports another one (or none) is replaced.
    pub own_version: Option<String>,
    pub on_event: OnEvent,
    /// Launch nothing until Start or `release`; a supervisor that already answers is still attached.
    pub hold_for_start: bool,
}

struct State {
    running: bool,
    polling: bool,
    phase: LauncherState,
    stopped_by_user: bool,
    /// Stop was sent and the old supervisor has not been seen gone yet.
    draining: bool,
    /// When a Start (or a version replacement) came in while draining; the launch waits for the old one to exit.
    start_wait_since: Option<SystemTime>,
    misses: u32,
    in_outage: bool,
    missing_reported: bool,
    running_since: SystemTime,
    spawns: u32,
    spawn_seq: u64,
    last_spawn_at: Option<SystemTime>,
    last_exit: Option<LastExit>,
    held: Option<String>,
    waiting_for_start: bool,
    /// Why the last supervisor refused config.json (exit 3), until one answers.
    refused: Option<String>,
    suspended: bool,
    /// A Start while suspended: one launch goes through, and the hold stays on.
    launch_once: bool,
    /// The app's Quit has begun: nothing launches for the rest of the process, Start included.
    quitting: bool,
    last_version_restart: Option<SystemTime>,
    last_status: Option<SupervisorStatus>,
}

impl State {
    /// Quitting, or suspended for a landing with no Start waiting to go through.
    fn held_back(&self) -> bool {
        self.quitting || (self.suspended && !self.launch_once)
    }
}

struct LastExit {
    code: Option<i32>,
    at: SystemTime,
    spawned_at: SystemTime,
}

struct Inner {
    layout: Layout,
    client: Arc<dyn SupervisorApi>,
    spawner: Arc<dyn Spawner>,
    clock: Arc<dyn Clock>,
    debug_faults: bool,
    own_version: Option<String>,
    on_event: OnEvent,
    guard: tokio::sync::Mutex<Option<LaunchGuard>>,
    /// Counts launch outcomes, with the last one.
    outcomes: watch::Sender<(u64, Option<LaunchOutcome>)>,
    /// `launch` helpers spawned that have not exited yet.
    helpers: watch::Sender<u32>,
    state: Mutex<State>,
    task: Mutex<Option<JoinHandle<()>>>,
}

/// Cloning shares the launcher.
#[derive(Clone)]
pub struct Launcher {
    inner: Arc<Inner>,
}

/// Clears the poll flag however the poll ends.
struct PollFlag<'a>(&'a Inner);

impl Drop for PollFlag<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().polling = false;
    }
}

enum Next {
    Done,
    Launch,
    ReplaceVersion(Option<String>),
}

fn since(now: SystemTime, then: SystemTime) -> Duration {
    now.duration_since(then).unwrap_or(Duration::ZERO)
}

fn detail(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// The reason of the last config-refused event in the day's supervisor events file, if any.
pub fn last_config_refusal(logs_dir: &Path, now: SystemTime) -> Option<String> {
    let text = std::fs::read_to_string(events_file(logs_dir, &local_date(now))).ok()?;
    for line in text.lines().rev() {
        let Ok(Value::Object(e)) = serde_json::from_str::<Value>(line) else { continue };
        if e.get("kind").and_then(Value::as_str) == Some("config-refused") {
            return e.get("reason").and_then(Value::as_str).map(str::to_string);
        }
    }
    None
}

impl Launcher {
    pub fn new(opts: LauncherOptions) -> Launcher {
        // A Stop survives a restart of the control process: the marker keeps the supervisor down until Start.
        let stopped_by_user = opts.layout.stopped_marker.exists();
        let now = opts.clock.now();
        let inner = Arc::new(Inner {
            layout: opts.layout,
            client: opts.client,
            spawner: opts.spawner,
            clock: opts.clock,
            debug_faults: opts.debug_faults,
            own_version: opts.own_version,
            on_event: opts.on_event,
            guard: tokio::sync::Mutex::new(opts.guard),
            outcomes: watch::Sender::new((0, None)),
            helpers: watch::Sender::new(0),
            state: Mutex::new(State {
                running: false,
                polling: false,
                phase: LauncherState::Starting,
                stopped_by_user,
                draining: false,
                start_wait_since: None,
                misses: 0,
                in_outage: false,
                missing_reported: false,
                running_since: now,
                spawns: 0,
                spawn_seq: 0,
                last_spawn_at: None,
                last_exit: None,
                held: None,
                waiting_for_start: opts.hold_for_start,
                refused: None,
                suspended: false,
                launch_once: false,
                quitting: false,
                last_version_restart: None,
                last_status: None,
            }),
            task: Mutex::new(None),
        });
        if opts.hold_for_start {
            // Only a restart in the same window would not know about the hold if this fails.
            let marker = &inner.layout.waiting_for_start;
            if let Some(dir) = marker.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(marker, "");
        } else {
            inner.end_hold();
        }
        Launcher { inner }
    }

    pub fn start(&self) {
        let mut task = self.inner.task.lock().unwrap();
        if task.is_some() {
            return;
        }
        self.inner.state.lock().unwrap().running = true;
        let inner = self.inner.clone();
        *task = Some(tokio::spawn(async move {
            let mut ticks = tokio::time::interval(POLL_INTERVAL);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                inner.poll().await;
            }
        }));
    }

    /// Stops watching. The supervisor and the stream keep running.
    pub fn stop(&self) {
        self.inner.state.lock().unwrap().running = false;
        if let Some(task) = self.inner.task.lock().unwrap().take() {
            task.abort();
        }
    }

    pub fn state(&self) -> LauncherState {
        let st = self.inner.state.lock().unwrap();
        if st.phase != LauncherState::Running && st.stopped_by_user {
            return LauncherState::StoppedByUser;
        }
        if st.phase != LauncherState::Running && st.waiting_for_start {
            return LauncherState::Ready;
        }
        st.phase
    }

    /// Set by Stop stream until the next Start, even while the old supervisor still answers.
    pub fn is_stopped_by_user(&self) -> bool {
        self.inner.state.lock().unwrap().stopped_by_user
    }

    pub fn mark_stopped_by_user(&self) {
        {
            let mut st = self.inner.state.lock().unwrap();
            st.stopped_by_user = true;
            st.draining = true;
        }
        let marker = &self.inner.layout.stopped_marker;
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // The Stop still holds until the process restarts; only its persistence is lost.
        let _ = std::fs::write(marker, "");
    }

    /// Why a Start has not launched yet, while it waits for the stopped supervisor to exit.
    pub fn wait_reason(&self) -> Option<String> {
        let since_wait = self.inner.state.lock().unwrap().start_wait_since?;
        Some(if since(self.inner.clock.now(), since_wait) >= DRAIN_LIMIT {
            "The stopped supervisor is still running after 20 s. The new one starts when it exits.".to_string()
        } else {
            "Waiting for the stopped supervisor to exit before starting it again.".to_string()
        })
    }

    /// Why the launch guard is holding the launch, until a spawn or an answering supervisor clears it.
    pub fn block_reason(&self) -> Option<String> {
        let st = self.inner.state.lock().unwrap();
        st.held.clone().or_else(|| st.refused.clone())
    }

    /// An operator Start: no backoff, and the spawn still goes through the one guarded poll.
    /// While suspended it launches once and the hold stays on: a landing starts the new exe this way. While
    /// quitting it does nothing, so `data\stopped-by-user` stays as it is.
    pub fn launch_now(&self) {
        if self.inner.state.lock().unwrap().quitting {
            return;
        }
        self.inner.end_hold();
        let _ = std::fs::remove_file(&self.inner.layout.stopped_marker);
        let now = self.inner.clock.now();
        {
            let mut st = self.inner.state.lock().unwrap();
            st.stopped_by_user = false;
            st.spawns = 0;
            st.launch_once = true;
            if st.draining {
                // The old supervisor still holds the port; launch once it stops answering, with no outage.
                st.start_wait_since = Some(now);
                st.phase = LauncherState::Starting;
            } else {
                st.misses = MISSES_BEFORE_LAUNCH;
                // a supervisor still answering after Stop must not turn the next miss into an outage
                if st.phase == LauncherState::Running {
                    st.phase = LauncherState::Starting;
                }
            }
        }
        self.poll_soon();
    }

    /// Held at start until Start or `release`, with no supervisor seen since.
    pub fn is_waiting_for_start(&self) -> bool {
        self.inner.state.lock().unwrap().waiting_for_start
    }

    /// `launch_now`, then waits up to `limit` for that launch to end; `None` when it did not.
    pub async fn launch_now_and_wait(&self, limit: Duration) -> Option<LaunchOutcome> {
        let mut outcomes = self.inner.outcomes.subscribe();
        let before = outcomes.borrow_and_update().0;
        self.launch_now();
        let ended = match tokio::time::timeout(limit, outcomes.wait_for(|(seq, _)| *seq > before)).await {
            Ok(Ok(ended)) => ended.1.clone(),
            _ => None,
        };
        ended
    }

    /// Ends the start hold; the next poll launches unless the stream was stopped by the user.
    pub fn release(&self) {
        self.inner.end_hold();
        self.inner.state.lock().unwrap().misses = MISSES_BEFORE_LAUNCH;
        self.poll_soon();
    }

    /// No relaunch and no outage alert until `resume`: an update is landing and the supervisor is meant to be gone.
    pub fn suspend(&self) {
        let mut st = self.inner.state.lock().unwrap();
        st.suspended = true;
        // A Start from before the hold must not launch during it.
        st.launch_once = false;
    }

    pub fn resume(&self) {
        let mut st = self.inner.state.lock().unwrap();
        st.suspended = false;
        st.misses = 0;
    }

    /// The app's Quit: no launch, no version replacement and no outage alert until the process exits. Unlike
    /// `suspend`, neither `launch_now` nor `resume` lifts it.
    pub fn hold_for_quit(&self) {
        let mut st = self.inner.state.lock().unwrap();
        st.quitting = true;
        st.launch_once = false;
    }

    /// Waits up to `limit` until every `launch` helper this launcher spawned has exited; false when one still
    /// runs. A helper exits once its supervisor answers, or after its own 3 s wait for that.
    pub async fn wait_for_launch_helpers(&self, limit: Duration) -> bool {
        let mut helpers = self.inner.helpers.subscribe();
        let ended = matches!(tokio::time::timeout(limit, helpers.wait_for(|running| *running == 0)).await, Ok(Ok(_)));
        ended
    }

    pub fn last_status(&self) -> Option<SupervisorStatus> {
        self.inner.state.lock().unwrap().last_status.clone()
    }

    fn poll_soon(&self) {
        if self.inner.state.lock().unwrap().running {
            let inner = self.inner.clone();
            tokio::spawn(async move { inner.poll().await });
        }
    }
}

impl Inner {
    fn emit(&self, kind: &str, d: Map<String, Value>) {
        (self.on_event)(kind, d);
    }

    fn report(&self, outcome: LaunchOutcome) {
        self.outcomes.send_modify(|(seq, last)| {
            *seq += 1;
            *last = Some(outcome);
        });
    }

    fn end_hold(&self) {
        self.state.lock().unwrap().waiting_for_start = false;
        // A restart before the marker goes would wait for Start again; nothing else depends on it.
        let _ = std::fs::remove_file(&self.layout.waiting_for_start);
    }

    async fn poll(self: &Arc<Self>) {
        {
            let mut st = self.state.lock().unwrap();
            if st.polling {
                return;
            }
            st.polling = true;
        }
        let _flag = PollFlag(self);
        let answer = self.client.status().await;
        let running = self.state.lock().unwrap().running;
        if running {
            self.on_poll(answer).await;
        }
    }

    async fn on_poll(self: &Arc<Self>, answer: Option<SupervisorStatus>) {
        let next = self.decide(answer);
        match next {
            Next::Done => {}
            Next::Launch => self.launch().await,
            Next::ReplaceVersion(running) => self.replace_version(running).await,
        }
    }

    /// The bookkeeping of one poll; the async follow-up it asks for runs with no lock held.
    fn decide(&self, answer: Option<SupervisorStatus>) -> Next {
        let answered = answer.is_some();
        let t = self.clock.now();
        let mut events: Vec<(&str, Map<String, Value>)> = Vec::new();
        let next = {
            let mut st = self.state.lock().unwrap();
            st.last_status = answer.clone();
            if !answered {
                st.draining = false;
            }
            if st.start_wait_since.is_some() {
                if answered {
                    return Next::Done;
                }
                st.start_wait_since = None;
                // If the guard holds this launch, the next poll retries it instead of counting misses again.
                st.misses = MISSES_BEFORE_LAUNCH;
                Next::Launch
            } else if let Some(status) = answer {
                // A supervisor that is up is not waiting for Start; if it dies later, it is relaunched like any outage.
                if st.waiting_for_start {
                    drop(st);
                    self.end_hold();
                    st = self.state.lock().unwrap();
                }
                st.misses = 0;
                st.held = None;
                st.refused = None;
                if let Some(exit) = st.last_exit.take() {
                    if exit.code == Some(PORT_TAKEN_EXIT) && since(exit.at, exit.spawned_at) < PORT_TAKEN_WINDOW {
                        // Another supervisor held the port: we attached, the spawn did not fail.
                        st.spawns = st.spawns.saturating_sub(1);
                        st.last_spawn_at = None;
                    }
                }
                if st.phase != LauncherState::Running {
                    st.phase = LauncherState::Running;
                    st.running_since = t;
                    if st.in_outage {
                        events.push(("supervisor-back", Map::new()));
                    }
                    st.in_outage = false;
                }
                if since(t, st.running_since) >= BACKOFF_RESET {
                    st.spawns = 0;
                }
                let paused = st.last_version_restart.is_some_and(|at| since(t, at) < VERSION_RESTART_PAUSE);
                match &self.own_version {
                    Some(own) if !st.stopped_by_user && !st.suspended && !st.quitting && !paused && status.version.as_ref() != Some(own) => {
                        Next::ReplaceVersion(status.version)
                    }
                    _ => Next::Done,
                }
            } else {
                st.misses += 1;
                if st.misses < MISSES_BEFORE_LAUNCH {
                    return Next::Done;
                }
                if st.stopped_by_user {
                    st.phase = LauncherState::StoppedByUser;
                    return Next::Done;
                }
                if st.waiting_for_start || st.held_back() {
                    return Next::Done;
                }
                if st.phase == LauncherState::Running {
                    st.phase = LauncherState::Down;
                    if !st.in_outage {
                        events.push(("supervisor-down", Map::new()));
                    }
                    st.in_outage = true;
                }
                Next::Launch
            }
        };
        for (kind, d) in events {
            self.emit(kind, d);
        }
        next
    }

    /// Asks the running supervisor of another version to shut down; the launch that follows is not an outage.
    async fn replace_version(&self, running: Option<String>) {
        let shut = self.client.shutdown().await;
        let now = self.clock.now();
        {
            let mut st = self.state.lock().unwrap();
            // Whether or not it went, the next try waits out the pause.
            st.last_version_restart = Some(now);
            if !shut || st.stopped_by_user || st.suspended || st.quitting {
                return;
            }
            st.draining = true;
            st.start_wait_since = Some(now);
            st.spawns = 0;
            st.phase = LauncherState::Starting;
        }
        let d = detail(&[
            ("running", running.map_or(Value::Null, Value::from)),
            ("expected", self.own_version.clone().map_or(Value::Null, Value::from)),
        ]);
        self.emit("supervisor-version-restart", d);
    }

    async fn launch(self: &Arc<Self>) {
        let t = self.clock.now();
        {
            let st = self.state.lock().unwrap();
            if st.held_back() {
                return;
            }
            if st.spawns > 0 {
                let wait = BACKOFF[((st.spawns - 1) as usize).min(BACKOFF.len() - 1)];
                if st.last_spawn_at.is_some_and(|at| since(t, at) < wait) {
                    return;
                }
            }
        }
        let exe = self.layout.exe.clone();
        if !exe.exists() {
            let first = {
                let mut st = self.state.lock().unwrap();
                st.phase = LauncherState::Missing;
                !std::mem::replace(&mut st.missing_reported, true)
            };
            if first {
                self.emit("supervisor-missing", detail(&[("root", self.layout.root.to_string_lossy().into_owned().into())]));
            }
            self.report(LaunchOutcome::NotStarted);
            return;
        }
        self.state.lock().unwrap().missing_reported = false;

        {
            // The poll stays guarded while this runs, so no second launch can start beside it.
            let mut guard = self.guard.lock().await;
            if let Some(guard) = guard.as_mut() {
                let held = guard.check().await;
                let mut st = self.state.lock().unwrap();
                if !st.running || st.stopped_by_user || st.held_back() {
                    return;
                }
                st.held = held.clone();
                if let Some(reason) = held {
                    drop(st);
                    self.report(LaunchOutcome::Held(reason));
                    return;
                }
            }
        }
        // The guard can take seconds; the port-taken window and the backoff count from the real spawn.
        let spawned_at = self.clock.now();

        // `launch` starts the supervisor detached and exits, so no exit or tree kill of this process reaches it.
        // Its exit code is the supervisor's port-taken 2 or refused-config 3, relayed; 1 when the supervisor
        // crashed during the bind wait or could not spawn; otherwise 0.
        let mut args = vec!["launch".to_string(), "--root".to_string(), self.layout.root.to_string_lossy().into_owned()];
        if self.debug_faults {
            args.push("--debug-faults".into());
        }
        let (spawned, seq) = {
            let mut st = self.state.lock().unwrap();
            // Under the lock `hold_for_quit` takes, so no spawn begins once a quit has begun.
            if st.quitting {
                return;
            }
            let spawned = self.spawner.spawn(&exe, &args);
            if spawned.is_ok() {
                // Under the same lock, so a quit's hold either stops this spawn or sees the helper counted.
                self.helpers.send_modify(|running| *running += 1);
            }
            st.spawn_seq += 1;
            st.spawns += 1;
            st.launch_once = false;
            st.last_spawn_at = Some(spawned_at);
            st.phase = LauncherState::Starting;
            (spawned, st.spawn_seq)
        };
        match spawned {
            Ok(handle) => {
                let inner = self.clone();
                tokio::spawn(async move {
                    let code = handle.exit_code().await;
                    inner.exited(seq, code, spawned_at);
                    inner.helpers.send_modify(|running| *running = running.saturating_sub(1));
                });
                self.emit("supervisor-launched", detail(&[("exe", exe.to_string_lossy().into_owned().into())]));
            }
            Err(err) => {
                tracing::warn!(%err, "The supervisor could not be spawned");
                {
                    let mut st = self.state.lock().unwrap();
                    if st.spawn_seq == seq && st.phase == LauncherState::Starting {
                        st.phase = LauncherState::Down;
                    }
                }
                self.report(LaunchOutcome::NotStarted);
            }
        }
    }

    fn exited(&self, seq: u64, code: Option<i32>, spawned_at: SystemTime) {
        let now = self.clock.now();
        let mut st = self.state.lock().unwrap();
        if seq != st.spawn_seq {
            return;
        }
        st.last_exit = Some(LastExit { code, at: now, spawned_at });
        if code == Some(CONFIG_REFUSED_EXIT) {
            let why = last_config_refusal(&self.layout.data.join("logs"), now);
            st.refused = Some(match why {
                Some(why) => format!("The supervisor refused config.json: {why}"),
                None => "The supervisor refused config.json.".to_string(),
            });
        }
        // 0 is the intermediate handing off to a running supervisor
        if code != Some(0) && st.phase == LauncherState::Starting {
            st.phase = LauncherState::Down;
        }
        drop(st);
        self.report(LaunchOutcome::Exited(code));
    }
}
