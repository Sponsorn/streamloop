//! Control mode, `streamloop.exe control [--root DIR]`: the dashboard, alerts, checks and updater that replace the
//! Node backend. This is `src/server/index.ts`: the wiring, the start order, a config save, the shutdown and
//! the exit codes.

use std::net::Ipv4Addr;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use streamloop_common::config::{is_first_run, Config};
use streamloop_common::encoder::Encoder;
use streamloop_common::paths::{install_root, Layout, Root};
use streamloop_common::version::VERSION;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::api::{self, AppDeps, AppState, ExeUpdates, Protect, RunSetupCheck, Updates};
use crate::clock::{Clock, SystemClock};
use crate::discord::{CurrentNotifier, DiscordNotifier, Notifier};
use crate::events::{launcher_event, with_playlist_names, Alert, AlertGate, SupervisorEvent, SupervisorEvents};
use crate::http::{Http, ReqwestHttp};
use crate::landing::{self, Confirmation, KillTree, LandedRecord, LandingDeps, LandingGate, PidOnPort, CONFIRM_WITHIN, FINISH_BEFORE_EXIT};
use crate::launcher::{should_hold, Launcher, LauncherOptions, ProcessSpawner, Spawner, StartKind};
use crate::leftovers::remove_3_0_leftovers;
use crate::logging;
use crate::playlists::PlaylistCache;
use crate::runner::{ProcessRunner, Runner};
use crate::setup_check::{self, LaunchGuard, LaunchGuardOptions};
use crate::supervisor_client::{SupervisorApi, SupervisorClient};
use crate::timeline::{EventLog, EventStore};
use crate::twitch::{AddEvent, RestartClock, TwitchChecker, TwitchOptions};
use crate::updater::{self, BreakPolicy, UpdateInfo, Updater, UpdaterOptions, GITHUB_RELEASES_URL};
use crate::ytdlp_update::{update_ytdlp, PausedUpdates};

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
/// Another StreamLoop holds the port. START.bat stays down on it instead of treating it as a crash,
/// whose branch could roll back an update that is still being confirmed.
pub const EXIT_PORT_TAKEN: i32 = 2;
pub const EXIT_USAGE: i32 = 64;
/// START.bat starts the control process again: an update landed or rolled back, or the test hook asked.
pub const EXIT_RESTART: i32 = 75;

const USAGE: &str = "Usage: streamloop.exe control [--root DIR]";
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(30);
const EVENTS_EVERY: Duration = Duration::from_secs(2);
const OBSERVE_EVERY: Duration = Duration::from_secs(2);
/// Node forced its exit this long after a shutdown that had not closed the server.
const CLOSE_LIMIT: Duration = Duration::from_secs(5);

/// What the environment says about this start. `STREAMLOOP_START` comes from START.bat; the rest are test
/// switches for the dev machine, never set by `config.json` or a release.
#[derive(Clone, Default)]
pub struct EnvVars {
    /// `STREAMLOOP_START`: `first`, `restart` or `rollback`.
    pub start: Option<String>,
    /// `STREAMLOOP_DEBUG_FAULTS=1`: the supervisor starts with `--debug-faults`.
    pub debug_faults: bool,
    /// `STREAMLOOP_TEST_HOOKS=1`: opens `POST /api/test/exit-75`.
    pub test_hooks: bool,
    /// `STREAMLOOP_TEST_HLS_URL`: the read-back reads this HLS and never asks Twitch.
    pub test_hls_url: Option<String>,
    /// `STREAMLOOP_TEST_RELEASES`: the release list file that replaces GitHub.
    pub test_releases: Option<PathBuf>,
    /// `STREAMLOOP_TEST_BREAK_AT_S` moves the planned break.
    pub policy: BreakPolicy,
}

impl EnvVars {
    pub fn from_env() -> EnvVars {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        EnvVars {
            start: var("STREAMLOOP_START"),
            debug_faults: var("STREAMLOOP_DEBUG_FAULTS").as_deref() == Some("1"),
            test_hooks: var("STREAMLOOP_TEST_HOOKS").as_deref() == Some("1"),
            test_hls_url: var("STREAMLOOP_TEST_HLS_URL"),
            test_releases: updater::test_releases_from_env(),
            policy: BreakPolicy::from_env(),
        }
    }
}

/// Everything the control process talks to, so tests can put fakes behind a real socket.
pub struct Wiring {
    pub root: Root,
    pub vars: EnvVars,
    pub client: Arc<dyn SupervisorApi>,
    /// Discord and Twitch.
    pub http: Arc<dyn Http>,
    /// GitHub and the downloads, behind the updater's redirect allow-list.
    pub updater_http: Arc<dyn Http>,
    pub runner: Arc<dyn Runner>,
    pub spawner: Arc<dyn Spawner>,
    pub clock: Arc<dyn Clock>,
    pub startup_dir: PathBuf,
    pub obs_running: Arc<dyn Fn() -> bool + Send + Sync>,
    pub kill_tree: KillTree,
    pub pid_on_port: PidOnPort,
    pub protect: Protect,
    pub setup_check: RunSetupCheck,
    /// One message per Ctrl+C or console close, whenever it came; the first one after the start ends the process.
    pub stop_signal: mpsc::UnboundedReceiver<()>,
}

impl Wiring {
    /// The production wiring. Call it inside the runtime: it starts listening for Ctrl+C and the console
    /// closing at once, and keeps listening until the runtime ends.
    pub fn real(root: Root) -> Wiring {
        let vars = EnvVars::from_env();
        let http: Arc<dyn Http> = Arc::new(ReqwestHttp::new());
        let runner: Arc<dyn Runner> = Arc::new(ProcessRunner);
        Wiring {
            root,
            client: Arc::new(SupervisorClient::new(http.clone(), None, None)),
            updater_http: Arc::new(updater::updater_http(vars.test_releases.is_some())),
            http,
            setup_check: api::setup_check_with(runner.clone()),
            runner,
            spawner: Arc::new(ProcessSpawner),
            clock: Arc::new(SystemClock),
            startup_dir: api::startup_folder(),
            obs_running: Arc::new(setup_check::obs_running),
            kill_tree: Arc::new(streamloop_common::procs::kill_tree),
            pid_on_port: Arc::new(streamloop_common::procs::pid_on_port),
            protect: Arc::new(streamloop_common::secret::protect),
            stop_signal: console_stop(),
            vars,
        }
    }
}

/// How the process ends. The first request sets the exit code and any later one changes nothing, so two
/// Update Now clicks that both reach the restart exit once.
#[derive(Clone)]
pub struct ExitSwitch(watch::Sender<Option<i32>>);

impl Default for ExitSwitch {
    fn default() -> Self {
        Self::new()
    }
}

impl ExitSwitch {
    pub fn new() -> Self {
        Self(watch::Sender::new(None))
    }

    /// True when this call set the exit code.
    pub fn request(&self, code: i32) -> bool {
        self.0.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(code);
            true
        })
    }

    pub fn is_requested(&self) -> bool {
        self.0.borrow().is_some()
    }

    pub async fn wait(&self) -> i32 {
        let mut rx = self.0.subscribe();
        let code = *rx.wait_for(Option::is_some).await.expect("the switch outlives its waiters");
        code.expect("waited for a code")
    }
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Turns supervisor and launcher events into timeline lines and Discord alerts. One gate for the process
/// lifetime, so both sources share its streaks.
struct EventSink {
    gate: Mutex<AlertGate>,
    config: Arc<RwLock<Config>>,
    timeline: Arc<EventLog>,
    clock: Arc<dyn Clock>,
    alerts: mpsc::UnboundedSender<Alert>,
    paused_updates: Mutex<PausedUpdates>,
    update_ytdlp: Arc<dyn Fn() + Send + Sync>,
}

impl EventSink {
    fn launcher(&self, kind: &str, detail: Map<String, Value>) {
        self.handle(launcher_event(kind, detail, self.clock.now()));
    }

    /// One event on its own: a handler that fails is logged, and the events after it still go through.
    fn handle(&self, e: SupervisorEvent) {
        let kind = e.kind.clone();
        if std::panic::catch_unwind(AssertUnwindSafe(|| self.route(e))).is_err() {
            tracing::error!(kind = %kind, "Handling a supervisor event failed");
        }
    }

    fn route(&self, e: SupervisorEvent) {
        if read(&self.config).auto_update_ytdlp && lock(&self.paused_updates).due(&e.kind, self.clock.now()) {
            (self.update_ytdlp)();
        }
        let named = with_playlist_names(e, &read(&self.config).playlists);
        let d = lock(&self.gate).describe(&named);
        if !d.message.is_empty() {
            self.timeline.add(&d.message);
        }
        if let Some(alert) = d.discord {
            let _ = self.alerts.send(alert);
        }
    }
}

/// Sends the event alerts in order, each in its own task, so one that fails never stops the next.
fn send_alerts(mut alerts: mpsc::UnboundedReceiver<Alert>, notifier: Arc<dyn Notifier>) {
    tokio::spawn(async move {
        while let Some(alert) = alerts.recv().await {
            let notifier = notifier.clone();
            if tokio::spawn(async move { notifier.notify_event(alert).await }).await.is_err() {
                tracing::error!("Sending a Discord alert failed");
            }
        }
    });
}

/// What a config save needs to build a new Discord notifier.
struct DiscordParts {
    uptime_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    http: Arc<dyn Http>,
    clock: Arc<dyn Clock>,
}

impl DiscordParts {
    fn build(&self, c: &Config) -> Arc<dyn Notifier> {
        Arc::new(DiscordNotifier::new(c.discord.clone(), VERSION, self.uptime_ms.clone(), self.http.clone(), self.clock.clone()))
    }
}

/// What a config save needs to build new Twitch checks; the restart clock carries over to each.
struct TwitchParts {
    client: Arc<dyn SupervisorApi>,
    notifier: Arc<dyn Notifier>,
    http: Arc<dyn Http>,
    clock: Arc<dyn Clock>,
    ffmpeg: String,
    add_event: AddEvent,
    restart_clock: RestartClock,
    test_hls_url: Option<String>,
}

impl TwitchParts {
    fn build(&self, c: &Config) -> Arc<TwitchChecker> {
        let opts = TwitchOptions {
            ffmpeg_path: Some(self.ffmpeg.clone()),
            add_event: Some(self.add_event.clone()),
            restart_clock: Some(self.restart_clock.clone()),
            test_hls_url: self.test_hls_url.clone(),
            grab: None,
        };
        Arc::new(TwitchChecker::new(c, self.client.clone(), self.notifier.clone(), self.http.clone(), self.clock.clone(), opts))
    }
}

/// The routes' updater. During a confirmation an update counts as running (Update Now and the other changing
/// routes answer 409) and a check changes nothing, since the confirmation removes `_update\` and what it holds.
struct RouteUpdates {
    exe: ExeUpdates,
    confirming: Arc<AtomicBool>,
}

#[async_trait]
impl Updates for RouteUpdates {
    fn status(&self) -> UpdateInfo {
        self.exe.status()
    }

    async fn check(&self) -> UpdateInfo {
        if self.confirming.load(Ordering::SeqCst) {
            return self.exe.status();
        }
        self.exe.check().await
    }

    fn landing_in_progress(&self) -> bool {
        self.confirming.load(Ordering::SeqCst) || self.exe.landing_in_progress()
    }

    fn confirming(&self) -> bool {
        self.confirming.load(Ordering::SeqCst)
    }

    async fn land(&self) -> Result<(), String> {
        self.exe.land().await
    }
}

/// What runs for the life of the process, shared by the routes and the timers.
struct Control {
    layout: Layout,
    config: Arc<RwLock<Config>>,
    clock: Arc<dyn Clock>,
    launcher: Launcher,
    updater: Updater,
    landing: Arc<LandingDeps>,
    timeline: Arc<EventLog>,
    sink: Arc<EventSink>,
    notifier: CurrentNotifier,
    discord_parts: DiscordParts,
    twitch_parts: TwitchParts,
    twitch: RwLock<Arc<TwitchChecker>>,
    port: u16,
    /// The launcher, the events tail and the Twitch checks run. A first run starts them on its first save.
    started: AtomicBool,
    confirming: Arc<AtomicBool>,
    landing_running: AtomicBool,
    /// The events tail, the update checks and the landing watch.
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// One config save's rebuild at a time.
    reload: Mutex<()>,
    exit: ExitSwitch,
}

impl Control {
    fn config(&self) -> Config {
        read(&self.config).clone()
    }

    fn twitch(&self) -> Arc<TwitchChecker> {
        read(&self.twitch).clone()
    }

    fn keep(&self, task: JoinHandle<()>) {
        lock(&self.tasks).push(task);
    }

    fn request_exit(&self, code: i32) {
        if !self.exit.request(code) {
            return;
        }
        if code == EXIT_RESTART {
            tracing::info!(port = self.port, "Restart requested for update");
        } else {
            tracing::info!(port = self.port, "Shutting down...");
        }
    }

    /// The launcher, the events tail and the Twitch checks, unless this is a first run: the shipped example's
    /// placeholder playlist starts nothing until the wizard saves a real one.
    fn start_services(self: &Arc<Self>) {
        if self.exit.is_requested() || is_first_run(&self.config()) || self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        self.launcher.start();
        self.start_events_tail();
        self.twitch().start();
    }

    fn start_events_tail(&self) {
        let mut events = SupervisorEvents::new(&self.layout.data.join("logs"), self.clock.clone());
        events.start();
        let sink = self.sink.clone();
        self.keep(tokio::spawn(async move {
            let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + EVENTS_EVERY, EVENTS_EVERY);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                for e in events.poll() {
                    sink.handle(e);
                }
            }
        }));
    }

    /// After `POST /config`: a new Discord notifier and new Twitch checks that keep the 30 min restart clock.
    /// It builds from the config as it is now, so of two saves the later one is what runs.
    fn apply_config(self: &Arc<Self>) {
        let _one = lock(&self.reload);
        let c = self.config();
        self.notifier.set(self.discord_parts.build(&c));
        let next = self.twitch_parts.build(&c);
        let old = std::mem::replace(&mut *write(&self.twitch), next.clone());
        old.stop();
        if !self.started.load(Ordering::SeqCst) {
            self.start_services();
        } else if !self.exit.is_requested() {
            next.start();
        }
        tracing::info!("Components reloaded with new config");
    }

    /// Confirms or rolls back a landed update, then starts the update checks and the landing watch. Neither
    /// runs during the confirmation, which removes `_update\` when it confirms.
    fn confirm_then_watch_updates(self: &Arc<Self>, landed: Option<LandedRecord>) {
        let me = self.clone();
        tokio::spawn(async move {
            if let Some(record) = landed {
                if landing::confirm_or_roll_back(&me.landing, record, CONFIRM_WITHIN).await == Confirmation::RolledBack {
                    me.request_exit(EXIT_RESTART);
                    return;
                }
            }
            me.confirming.store(false, Ordering::SeqCst);
            if !me.exit.is_requested() {
                me.start_update_timers();
            }
        });
    }

    fn start_update_timers(self: &Arc<Self>) {
        let me = self.clone();
        self.keep(tokio::spawn(async move {
            tokio::time::sleep(FIRST_UPDATE_CHECK).await;
            loop {
                let c = me.config();
                if c.auto_update_check {
                    me.updater.check().await;
                }
                tokio::time::sleep(Duration::from_millis(c.update_check_interval_ms)).await;
            }
        }));
        let me = self.clone();
        self.keep(tokio::spawn(async move {
            let mut ticks = tokio::time::interval(OBSERVE_EVERY);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                me.observe();
            }
        }));
    }

    /// Hands the launcher's last poll to the updater, and lands a staged update when it says so.
    fn observe(self: &Arc<Self>) {
        if !self.started.load(Ordering::SeqCst) {
            return;
        }
        let sup = self.launcher.last_status();
        let auto = read(&self.config).auto_update_check;
        let land = self.updater.observe(sup.as_ref(), self.launcher.is_stopped_by_user(), auto);
        if !land || self.landing.landing_in_progress() || self.landing_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let result = landing::land(&me.landing).await;
            me.landing_running.store(false, Ordering::SeqCst);
            match result {
                Ok(()) => me.request_exit(EXIT_RESTART),
                Err(err) => tracing::error!(%err, "The update could not be landed"),
            }
        });
    }

    /// The control process's own timers only: the supervisor and the stream keep running.
    fn stop_own(&self) {
        for task in lock(&self.tasks).drain(..) {
            task.abort();
        }
        self.twitch().stop();
        self.launcher.stop();
    }
}

pub struct App {
    control: Arc<Control>,
    server: JoinHandle<()>,
    close: oneshot::Sender<()>,
    stop_signal: mpsc::UnboundedReceiver<()>,
}

impl App {
    /// Loads the config, binds the port, removes the 3.0 leftovers, starts the confirmation of a landed
    /// update and everything else that runs. `Err` is the exit code when it could not start.
    pub async fn start(w: Wiring) -> Result<App, i32> {
        let Wiring {
            root,
            vars,
            client,
            http,
            updater_http,
            runner,
            spawner,
            clock,
            startup_dir,
            obs_running,
            kill_tree,
            pid_on_port,
            protect,
            setup_check,
            stop_signal,
        } = w;
        let layout = Layout::new(&root.dir);
        let config = match streamloop_common::config::load(&layout.config) {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(%err, "Could not load config.json");
                return Err(EXIT_FAILED);
            }
        };
        tracing::info!(port = config.port, playlists = config.playlists.len(), "Starting StreamLoop");
        // Nothing starts before the port is ours, so a second StreamLoop never starts a second stream.
        let (listener, port) = bind(config.port).await?;
        tracing::info!(port, "Server listening");

        let leftovers = layout.clone();
        if let Err(err) = tokio::task::spawn_blocking(move || remove_3_0_leftovers(&leftovers)).await {
            tracing::error!(%err, "Removing the 3.0 leftovers stopped");
        }
        let landed = landed_record(&layout);

        if vars.debug_faults {
            tracing::warn!("STREAMLOOP_DEBUG_FAULTS=1: the supervisor starts with --debug-faults");
        }
        if vars.test_hooks {
            tracing::warn!("STREAMLOOP_TEST_HOOKS=1: POST /api/test/exit-75 is open");
        }
        if vars.test_hls_url.is_some() {
            tracing::warn!("STREAMLOOP_TEST_HLS_URL is set: the Twitch read-back reads a local test stream and Twitch is never asked");
        }

        let started_at = Instant::now();
        let uptime_ms: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(move || started_at.elapsed().as_millis() as u64);
        let shared = Arc::new(RwLock::new(config.clone()));
        let timeline = Arc::new(EventLog::new(EventStore::new(&layout.app_logs, clock.clone())));
        let playlists = Arc::new(PlaylistCache::new(runner.clone(), layout.ytdlp.clone(), config.ytdl_cookies_from_browser.clone(), clock.clone()));

        let discord_parts = DiscordParts { uptime_ms: uptime_ms.clone(), http: http.clone(), clock: clock.clone() };
        let notifier = CurrentNotifier::new(discord_parts.build(&config));
        let forwarding: Arc<dyn Notifier> = Arc::new(notifier.clone());
        let (alerts, alerts_rx) = mpsc::unbounded_channel();
        send_alerts(alerts_rx, forwarding.clone());
        let sink = Arc::new(EventSink {
            gate: Mutex::default(),
            config: shared.clone(),
            timeline: timeline.clone(),
            clock: clock.clone(),
            alerts,
            paused_updates: Mutex::default(),
            update_ytdlp: {
                let (runner, ytdlp, playlists) = (runner.clone(), layout.ytdlp.clone(), playlists.clone());
                Arc::new(move || update_ytdlp_in_background(runner.clone(), ytdlp.clone(), playlists.clone(), "YouTube lookups are paused"))
            },
        });
        // Before the routes answer, so the 409 gate never waits on a progress that was cut off. A rollback by
        // START.bat, or a landing cut off, is reported as this process reports its own rollback.
        if let Some(text) = landing::settle_unfinished_update(&layout, clock.as_ref(), VERSION) {
            timeline.add(&text);
            let notifier = forwarding.clone();
            tokio::spawn(async move { notifier.notify_critical(&text).await });
        }

        // An update or its rollback holds this start only when the stream was held before the landing.
        let kind = start_kind(vars.start.as_deref(), landed.as_ref(), &layout);
        let hold = should_hold(kind, config.auto_start_stream, layout.waiting_for_start.exists());
        if hold {
            tracing::info!(start_kind = ?kind, "The stream waits for Start");
        }
        let guard = launch_guard(&layout, runner.clone(), clock.clone(), obs_running, shared.clone(), sink.clone());
        let launcher = Launcher::new(LauncherOptions {
            layout: layout.clone(),
            client: client.clone(),
            spawner,
            clock: clock.clone(),
            debug_faults: vars.debug_faults,
            guard: Some(guard),
            own_version: Some(VERSION.to_string()),
            on_event: {
                let sink = sink.clone();
                Box::new(move |kind, detail| sink.launcher(kind, detail))
            },
            hold_for_start: hold,
        });

        let updater = Updater::new(UpdaterOptions {
            current: VERSION.to_string(),
            dev_mode: !root.from_exe,
            http: updater_http,
            runner: runner.clone(),
            clock: clock.clone(),
            layout: layout.clone(),
            releases_url: GITHUB_RELEASES_URL.to_string(),
            test_releases: vars.test_releases.clone(),
            policy: vars.policy,
        });
        let landing = Arc::new(LandingDeps {
            layout: layout.clone(),
            updater: updater.clone(),
            launcher: launcher.clone(),
            client: client.clone(),
            notifier: forwarding.clone(),
            timeline: timeline.clone(),
            clock: clock.clone(),
            runner: runner.clone(),
            kill_tree,
            pid_on_port,
            first_run: {
                let config = shared.clone();
                Arc::new(move || is_first_run(&read(&config)))
            },
            gate: LandingGate::default(),
        });

        let twitch_parts = TwitchParts {
            client: client.clone(),
            notifier: forwarding.clone(),
            http,
            clock: clock.clone(),
            ffmpeg: layout.ffmpeg.to_string_lossy().into_owned(),
            add_event: {
                let timeline = timeline.clone();
                Arc::new(move |m: &str| timeline.add(m))
            },
            restart_clock: RestartClock::new(),
            test_hls_url: vars.test_hls_url.clone(),
        };
        let twitch = twitch_parts.build(&config);
        let confirming = Arc::new(AtomicBool::new(landed.is_some()));
        let control = Arc::new(Control {
            layout: layout.clone(),
            config: shared.clone(),
            clock,
            launcher: launcher.clone(),
            updater: updater.clone(),
            landing: landing.clone(),
            timeline: timeline.clone(),
            sink,
            notifier,
            discord_parts,
            twitch_parts,
            twitch: RwLock::new(twitch),
            port,
            started: AtomicBool::new(false),
            confirming: confirming.clone(),
            landing_running: AtomicBool::new(false),
            tasks: Mutex::new(Vec::new()),
            reload: Mutex::new(()),
            exit: ExitSwitch::new(),
        });

        let state = AppState::new(AppDeps {
            config: shared,
            config_path: layout.config.clone(),
            layout: layout.clone(),
            client,
            launcher: Arc::new(launcher),
            timeline,
            playlists: playlists.clone(),
            updates: Arc::new(RouteUpdates { exe: ExeUpdates { landing }, confirming }),
            notifier: Arc::new(move || forwarding.clone()),
            twitch: {
                let control = control.clone();
                Arc::new(move || control.twitch().status())
            },
            uptime_ms,
            startup_dir,
            test_hooks: vars.test_hooks,
            runner: runner.clone(),
            setup_check,
            protect,
            apply_config: {
                let control = control.clone();
                Arc::new(move |_saved: &Config| control.apply_config())
            },
            // The exit waits for the server to close, and the server lets every reply under way finish.
            trigger_restart: {
                let control = control.clone();
                Arc::new(move || control.request_exit(EXIT_RESTART))
            },
            trigger_shutdown: {
                let control = control.clone();
                Arc::new(move || control.request_exit(EXIT_OK))
            },
        });
        let (close, closed) = oneshot::channel();
        let server = tokio::spawn(serve_api(listener, api::router(Arc::new(state), port), closed));

        control.confirm_then_watch_updates(landed);
        control.start_services();
        // An outdated yt-dlp can stop after the first page of a playlist, so it updates itself at every start.
        if config.auto_update_ytdlp {
            update_ytdlp_in_background(runner, layout.ytdlp.clone(), playlists, "start");
        }
        Ok(App { control, server, close, stop_signal })
    }

    /// The Twitch checks as they run now; a config save replaces them.
    pub fn twitch(&self) -> Arc<TwitchChecker> {
        self.control.twitch()
    }

    /// Runs until Ctrl+C, the console closing, `/api/shutdown` (0), or a landed update, a rollback or the
    /// test hook (75). A landing under way finishes first, so the exit does not leave the stream stopped. Stops the
    /// control process's own timers and closes the server; the stream keeps running.
    pub async fn wait(self) -> i32 {
        let App { control, server, close, mut stop_signal } = self;
        let code = tokio::select! {
            code = control.exit.wait() => code,
            Some(()) = stop_signal.recv() => {
                control.request_exit(EXIT_OK);
                control.exit.wait().await
            }
        };
        control.landing.finish_before_exit(FINISH_BEFORE_EXIT).await;
        control.stop_own();
        let _ = close.send(());
        match tokio::time::timeout(CLOSE_LIMIT, server).await {
            Ok(_) => tracing::info!("Server closed"),
            Err(_) => tracing::warn!("The server did not close within 5 s"),
        }
        control.timeline.flush().await;
        code
    }
}

/// Starts the control process and runs it to its exit code.
pub async fn serve(w: Wiring) -> i32 {
    match App::start(w).await {
        Ok(app) => app.wait().await,
        Err(code) => code,
    }
}

/// `streamloop.exe control [--root DIR]`, or `streamloop.exe [--root DIR]` under START.bat; `args` is every
/// argument after the mode word. The exe has attached to its parent's console before this runs.
pub fn run(args: Vec<String>) -> i32 {
    // The supervisor outlives this process; holding its output file, it would stop a launcher that
    // redirects to that file from starting the control process again.
    streamloop_common::procs::keep_std_handles_from_children();
    let Some(flag) = root_flag(&args) else {
        eprintln!("{USAGE}");
        return EXIT_USAGE;
    };
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    let env_root = std::env::var_os("STREAMLOOP_ROOT").filter(|v| !v.is_empty()).map(PathBuf::from);
    let root = match install_root(flag.as_deref(), &exe_dir, env_root.as_deref()) {
        Ok(root) => root,
        Err(why) => {
            eprintln!("{why}");
            return EXIT_FAILED;
        }
    };
    logging::init(&Layout::new(&root.dir).app_logs);
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            tracing::error!(%err, "Could not start the async runtime");
            return EXIT_FAILED;
        }
    };
    let code = runtime.block_on(async move { serve(Wiring::real(root)).await });
    // Whatever still runs (the Ctrl+C listeners, a landing cut off by Ctrl+C, a Discord post) ends here.
    runtime.shutdown_background();
    code
}

/// `[]` or `["--root", DIR]`, else `None`.
fn root_flag(args: &[String]) -> Option<Option<PathBuf>> {
    match args {
        [] => Some(None),
        [flag, dir] if flag == "--root" && !dir.is_empty() => Some(Some(PathBuf::from(dir))),
        _ => None,
    }
}

async fn bind(port: u64) -> Result<(TcpListener, u16), i32> {
    let Ok(port) = u16::try_from(port) else {
        tracing::error!(port, "The port in config.json is not a port number");
        return Err(EXIT_FAILED);
    };
    match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
        Ok(listener) => Ok((listener, port)),
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            tracing::error!("Port {port} is in use. Is another StreamLoop running? Close it and start this one again.");
            Err(EXIT_PORT_TAKEN)
        }
        Err(err) => {
            tracing::error!(%err, port, "Server error");
            Err(EXIT_FAILED)
        }
    }
}

async fn serve_api(listener: TcpListener, router: axum::Router, closed: oneshot::Receiver<()>) {
    let shutdown = async {
        let _ = closed.await;
    };
    if let Err(err) = axum::serve(listener, router).with_graceful_shutdown(shutdown).await {
        tracing::error!(%err, "Server error");
    }
}

/// START.bat's word for this start; a landed update makes it `update`. Anything else counts as a first start.
fn start_kind(word: Option<&str>, landed: Option<&LandedRecord>, layout: &Layout) -> StartKind {
    if let Some(record) = landed {
        return StartKind::Update { held: record.held };
    }
    match word {
        Some("restart") => StartKind::Restart,
        Some("rollback") => StartKind::Rollback { held: LandedRecord::read(&layout.rolled_back).map(|r| r.held) },
        _ => StartKind::First,
    }
}

/// `_update\landed`, when a landing left one. One that cannot be read is removed, so the progress it left
/// `running` is settled instead of holding every control behind the update gate.
fn landed_record(layout: &Layout) -> Option<LandedRecord> {
    if !layout.landed.exists() {
        return None;
    }
    let record = LandedRecord::read(&layout.landed);
    if record.is_none() {
        tracing::warn!("Removing an _update\\landed that cannot be read");
        if let Err(err) = std::fs::remove_file(&layout.landed) {
            tracing::error!(%err, "Could not remove _update\\landed");
        }
    }
    record
}

fn launch_guard(
    layout: &Layout,
    runner: Arc<dyn Runner>,
    clock: Arc<dyn Clock>,
    obs_running: Arc<dyn Fn() -> bool + Send + Sync>,
    config: Arc<RwLock<Config>>,
    sink: Arc<EventSink>,
) -> LaunchGuard {
    let encoders = config.clone();
    let path = layout.config.clone();
    LaunchGuard::new(LaunchGuardOptions {
        layout: layout.clone(),
        runner,
        obs_running: Box::new(move || obs_running()),
        exists: Box::new(|p: &Path| p.exists()),
        clock,
        get_encoders: Box::new(move || {
            let c = read(&encoders);
            (c.supervisor.encoder, c.supervisor.preferred_encoder)
        }),
        save_encoders: Box::new(move |encoder, preferred| save_encoders(&config, &path, encoder, preferred)),
        on_event: Box::new(move |kind, detail| sink.launcher(kind, detail)),
    })
}

/// The guard's pick, saved through the config the routes hold and under its write lock, so a dashboard save
/// and a fallback never merge into the same old file.
fn save_encoders(config: &RwLock<Config>, path: &Path, encoder: Encoder, preferred: Encoder) -> Result<(), String> {
    let mut current = write(config);
    let patch = json!({ "supervisor": { "encoder": encoder.as_str(), "preferredEncoder": preferred.as_str() } });
    *current = streamloop_common::config::save(&patch, path).map_err(|e| e.to_string())?;
    Ok(())
}

/// yt-dlp's self-update on its own task, so it never holds up the start or the stream; `why` goes to the log.
fn update_ytdlp_in_background(runner: Arc<dyn Runner>, ytdlp: PathBuf, playlists: Arc<PlaylistCache>, why: &'static str) {
    tokio::spawn(async move {
        let result = update_ytdlp(runner.as_ref(), &ytdlp).await;
        if result.ok {
            tracing::info!(version = ?result.version, why, "yt-dlp auto-update complete");
            playlists.clear();
        } else {
            tracing::warn!(error = ?result.error, why, "yt-dlp auto-update skipped");
        }
    });
}

/// Ctrl+C and the console closing, from now until the runtime ends, so neither reaches Windows' default handler
/// (an exit START.bat reads as a crash). After a close, tokio's handler holds the process until it exits 0.
fn console_stop() -> mpsc::UnboundedReceiver<()> {
    let (tx, rx) = mpsc::unbounded_channel();
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        match windows::ctrl_c() {
            Ok(mut ctrl_c) => {
                let tx = tx.clone();
                tokio::spawn(async move {
                    while ctrl_c.recv().await.is_some() {
                        let _ = tx.send(());
                    }
                });
            }
            Err(err) => tracing::warn!(%err, "Ctrl+C will not stop StreamLoop cleanly"),
        }
        match windows::ctrl_close() {
            Ok(mut close) => {
                tokio::spawn(async move {
                    while close.recv().await.is_some() {
                        let _ = tx.send(());
                    }
                });
            }
            Err(err) => tracing::warn!(%err, "Closing the console window will not stop StreamLoop cleanly"),
        }
    }
    #[cfg(not(windows))]
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            let _ = tx.send(());
        }
    });
    rx
}
